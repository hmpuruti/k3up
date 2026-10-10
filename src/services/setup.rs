//! Turning services mode on and off for a data directory.
use super::{
    backend::{Backend, Settings},
    files::{self, Layout},
    host,
    pause::{Control, install_then_repair},
    scm,
    status::Scm,
};
use crate::autostart::LoginAgent;
use crate::win32::{
    MARKER_SDDL, PRIVATE_DIR_SDDL, ROOT_DIR_SDDL, SHARED_DIR_SDDL, reset_contents, secure_dir,
    set_security,
};
use anyhow::{Context, Result, anyhow, bail};
use std::{
    collections::BTreeMap,
    fs::File,
    io::Read,
    path::{Path, PathBuf},
    time::Duration,
};

/// Copied to Program Files from the folder of the running k3up.exe. The host is required.
const PROGRAMS: [&str; 3] = ["k3up-host.exe", "k3up.exe", "k3up-desktop.exe"];
const CENTRAL_AGENT: &str = "K3Up";

pub fn install_dir() -> Result<PathBuf> {
    Ok(crate::win32::program_files()?.join("K3 Up"))
}

/// Creates or secures the data directory and its folders and what they already hold, then
/// writes the marker. Safe to repeat, and repeating it replaces a marker written with older
/// permissions.
pub fn prepare(data: &Path) -> Result<()> {
    let layout = Layout::new(data);
    refuse_tampered(&layout)?;
    secure_dir(data, ROOT_DIR_SDDL)?;
    for (folder, sddl) in [
        (layout.workloads(), PRIVATE_DIR_SDDL),
        (layout.logs(), PRIVATE_DIR_SDDL),
        (layout.states(), SHARED_DIR_SDDL),
        (layout.events_dir(), SHARED_DIR_SDDL),
    ] {
        secure_dir(&folder, sddl)?;
        reset_contents(&folder)?;
    }
    files::write_atomic(&layout.marker(), b"", |path| {
        set_security(path, MARKER_SDDL)
    })
}

/// Refuses definitions that users who aren't administrators could have edited, or that sit
/// in a folder they could change: the host would run what they wrote as LocalSystem, and
/// repairing the permissions must not make them trusted. Logs, state and history are never
/// run, so their permissions are simply repaired.
fn refuse_tampered(layout: &Layout) -> Result<()> {
    let folder = layout.workloads();
    if !folder.exists() {
        return Ok(());
    }
    let mut suspect = vec![];
    if crate::win32::writable_by_others(&folder)? {
        suspect.push(folder.clone());
    }
    for name in files::names(&folder, "toml")? {
        let definition = layout.definition(&name);
        if crate::win32::writable_by_others(&definition)? {
            suspect.push(definition);
        }
    }
    if suspect.is_empty() {
        return Ok(());
    }
    let listed: Vec<String> = suspect
        .iter()
        .map(|path| format!("  {}", path.display()))
        .collect();
    bail!(
        "Users who aren't administrators could have changed these definitions:\n{}\nReview them, delete them, and apply the workloads again from a trusted manifest",
        listed.join("\n")
    )
}

pub fn enable(data: &Path) -> Result<Vec<String>> {
    let settings = Settings::from_env()?;
    if scm::exists(CENTRAL_AGENT)? {
        bail!(
            "The K3 Up agent service is installed. Run `k3up agent uninstall-service` first, then enable services mode"
        );
    }
    let own = LoginAgent::new(PathBuf::new(), crate::platform::login_data_dir());
    super::refuse_user_agent(own.registration(), own.is_running())?;
    if !crate::win32::ancestors_protected(data) {
        bail!(
            "A folder above {} can be moved or changed by users who aren't administrators, so services mode refuses it",
            data.display()
        );
    }
    super::check_registration(super::registered().as_deref(), data)?;
    let _lock = lock_if_prepared(data)?;
    let target = install_dir()?;
    if target.exists() {
        check_target(&target)?;
    }
    let sources = program_sources(&target)?;
    let prefix = settings.prefix.as_str();
    let running: Vec<String> = scm::list(prefix)?
        .into_iter()
        .filter(|service| matches!(service.scm, Scm::Running | Scm::Starting))
        .filter_map(|service| Some(service.name.get(prefix.len()..)?.to_string()))
        .collect();
    install_then_repair(
        &mut Services { prefix },
        &running,
        || match sources {
            Some(sources) => copy_programs(sources, &target),
            None => Ok(vec![format!(
                "Programs are already in {}",
                target.display()
            )]),
        },
        || {
            prepare(data)?;
            register(data)?;
            Ok(vec![format!("Services mode is on for {}", data.display())])
        },
    )
    .context("Turning services mode on did not finish")
}

/// Records `data` for the whole machine as the one services mode directory. Only `enable`
/// and `disable` touch this record; the tests, which use their own service name prefix and
/// data directories, call `prepare` instead and leave it alone.
fn register(data: &Path) -> Result<()> {
    let data = std::path::absolute(data)?;
    crate::win32::set_registry_string(
        crate::win32::Hive::LocalMachine,
        super::REGISTRY_KEY,
        super::REGISTRY_VALUE,
        &data.to_string_lossy(),
    )
    .context("Record the services mode data directory in the registry")
}

/// The lock that commands changing services hold, once a protected folder exists to hold it.
/// Before that, no command can change services in `data`.
fn lock_if_prepared(data: &Path) -> Result<Option<files::Lock>> {
    if !crate::win32::is_protected(&Layout::new(data).workloads()) {
        return Ok(None);
    }
    super::backend::lock(data).map(Some)
}

/// The programs beside the running k3up.exe to copy into `target`, or `None` when they
/// already run from there. Running services lock the host, so the copy happens while they
/// are stopped.
fn program_sources(target: &Path) -> Result<Option<Vec<(&'static str, File)>>> {
    let source = std::env::current_exe()?
        .parent()
        .context("Locate the folder of k3up.exe")?
        .to_path_buf();
    if !source.join(PROGRAMS[0]).is_file() {
        bail!("Place k3up-host.exe beside k3up.exe");
    }
    if same_dir(&source, target) {
        return Ok(None);
    }
    open_sources(&source, target).map(Some)
}

/// Refuses an install folder that users who aren't administrators could change, or that
/// holds a program they could change: every service runs the host from there as LocalSystem.
/// It is not repaired, since its programs may already have been replaced.
pub fn check_target(target: &Path) -> Result<()> {
    let mut problems = vec![];
    if !crate::win32::is_protected(target) {
        problems.push(format!("  {}", target.display()));
    }
    if !crate::win32::ancestors_protected(target) {
        problems.push(format!("  a folder above {}", target.display()));
    }
    for program in PROGRAMS {
        let path = target.join(program);
        if path.exists() && crate::win32::open_protected(&path)?.is_none() {
            problems.push(format!("  {}", path.display()));
        }
    }
    if problems.is_empty() {
        return Ok(());
    }
    bail!(
        "Users who aren't administrators can change these, so services mode won't run programs from them:\n{}\nDelete {} and extract the K3 Up release zip into it again.",
        problems.join("\n"),
        target.display()
    )
}

/// The programs in `source` to install in `target`, held open so nobody can change them before
/// they are copied. The host runs as LocalSystem for every workload, so a folder or program
/// that users who aren't administrators may change is refused.
pub fn open_sources(source: &Path, target: &Path) -> Result<Vec<(&'static str, File)>> {
    let refuse = |path: &Path| {
        anyhow!(
            "{} can be changed by users who aren't administrators, so services mode won't install programs from it. Extract the K3 Up release zip into {} and run \"{}\" services enable from an elevated terminal.",
            path.display(),
            target.display(),
            target.join("k3up.exe").display()
        )
    };
    if !crate::win32::is_protected(source) {
        return Err(refuse(source));
    }
    PROGRAMS
        .into_iter()
        .filter(|program| source.join(program).is_file())
        .map(|program| {
            let path = source.join(program);
            let file = crate::win32::open_protected(&path)?.ok_or_else(|| refuse(&path))?;
            Ok((program, file))
        })
        .collect()
}

/// Workload services, by workload name.
struct Services<'a> {
    prefix: &'a str,
}

impl Control for Services<'_> {
    fn stop(&mut self, name: &str) -> Result<()> {
        scm::stop(&format!("{}{name}", self.prefix), Duration::from_secs(90))
    }

    /// Waits for the host to run, since a host that cannot run stops again at once.
    fn start(&mut self, name: &str) -> Result<()> {
        let service = format!("{}{name}", self.prefix);
        scm::start(&service)?;
        let state = scm::wait(&service, scm::START_TIMEOUT, |state| {
            matches!(state, Scm::Running | Scm::Stopped)
        })?;
        match state {
            Some(Scm::Running) => Ok(()),
            // A job that already finished reports no error code.
            Some(Scm::Stopped) => match scm::specific_exit_code(&service)? {
                None => Ok(()),
                code => bail!("it stopped at once: {}", host::explain_exit(code)),
            },
            None => bail!("its service no longer exists"),
            Some(_) => bail!(
                "it did not start within {} seconds",
                scm::START_TIMEOUT.as_secs()
            ),
        }
    }
}

fn copy_programs(sources: Vec<(&str, File)>, target: &Path) -> Result<Vec<String>> {
    if !target.exists() {
        crate::win32::create_dir_with(target, crate::win32::INSTALL_DIR_SDDL)?;
    }
    let mut lines = vec![];
    for (program, mut file) in sources {
        let to = target.join(program);
        let mut bytes = vec![];
        // A partly written host would break every workload, so the copy replaces the
        // installed program in one step or not at all.
        file.read_to_end(&mut bytes)
            .map_err(anyhow::Error::from)
            .and_then(|_| {
                files::write_atomic(&to, &bytes, |path| {
                    set_security(path, crate::win32::PROGRAM_SDDL)
                })
            })
            .with_context(|| format!("Copy {program} to {}", to.display()))?;
        lines.push(format!("Copied {program} to {}", target.display()));
    }
    Ok(lines)
}

fn same_dir(a: &Path, b: &Path) -> bool {
    match (std::fs::canonicalize(a), std::fs::canonicalize(b)) {
        (Ok(a), Ok(b)) => a == b,
        _ => false,
    }
}

pub fn disable(data: &Path) -> Result<String> {
    let settings = Settings::from_env()?;
    if let Some(registered) = super::registered() {
        super::check_registration(Some(data), &registered).map_err(|_| {
            anyhow!(
                "Services mode uses {}. Turn it off there with --data-dir \"{}\"",
                registered.display(),
                registered.display()
            )
        })?;
    }
    let _lock = lock_if_prepared(data)?;
    let services = scm::list(&settings.prefix)?;
    if !services.is_empty() {
        let names: Vec<String> = services
            .iter()
            .map(|service| format!("  {}", service.name))
            .collect();
        bail!(
            "Remove these workloads first, with `k3up remove NAME --stop`:\n{}",
            names.join("\n")
        );
    }
    let marker = Layout::new(data).marker();
    match std::fs::remove_file(&marker) {
        Err(error) if error.kind() != std::io::ErrorKind::NotFound => {
            return Err(error).with_context(|| format!("Delete {}", marker.display()));
        }
        _ => {}
    }
    crate::win32::delete_registry_value(
        crate::win32::Hive::LocalMachine,
        super::REGISTRY_KEY,
        super::REGISTRY_VALUE,
    )
    .context("Remove the services mode record from the registry")?;
    Ok(format!(
        "Services mode is off. Definitions, logs and history are kept in {}",
        data.display()
    ))
}

pub struct Summary {
    pub enabled: bool,
    pub data: PathBuf,
    pub host: PathBuf,
    pub host_present: bool,
    pub counts: BTreeMap<String, usize>,
}

pub fn summary(data: &Path) -> Result<Summary> {
    let enabled = super::check(data)?;
    let settings = Settings::from_env()?;
    let mut counts = BTreeMap::new();
    if enabled {
        let response =
            Backend::with(data, settings.clone()).handle(crate::protocol::Command::List)?;
        for status in response.workloads {
            *counts.entry(status.state.to_string()).or_default() += 1;
        }
    }
    Ok(Summary {
        enabled,
        data: data.to_path_buf(),
        host_present: settings.host.is_file(),
        host: settings.host,
        counts,
    })
}
