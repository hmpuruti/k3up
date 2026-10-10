//! Turning services mode on and off for a data directory.
use super::{
    backend::{Backend, Settings},
    files::{self, Layout},
    host,
    pause::{Control, while_stopped},
    scm,
    status::Scm,
};
use crate::autostart::LoginAgent;
use crate::win32::{
    MARKER_SDDL, PRIVATE_DIR_SDDL, ROOT_DIR_SDDL, SHARED_DIR_SDDL, reset_contents, secure_dir,
    set_security,
};
use anyhow::{Context, Result, bail};
use std::{
    collections::BTreeMap,
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

pub fn enable(data: &Path) -> Result<Vec<String>> {
    let settings = Settings::from_env()?;
    if scm::exists(CENTRAL_AGENT)? {
        bail!(
            "The K3 Up agent service is installed. Run `k3up agent uninstall-service` first, then enable services mode"
        );
    }
    let own = LoginAgent::new(PathBuf::new(), crate::platform::login_data_dir());
    super::refuse_user_agent(own.registration(), own.is_running())?;
    let _lock = lock_if_prepared(data)?;
    let mut lines = install_programs(&settings.prefix)?;
    prepare(data)?;
    lines.push(format!("Services mode is on for {}", data.display()));
    Ok(lines)
}

/// The lock that commands changing services hold, once a protected folder exists to hold it.
/// Before that, no command can change services in `data`.
fn lock_if_prepared(data: &Path) -> Result<Option<files::Lock>> {
    if !crate::win32::is_protected(&Layout::new(data).workloads()) {
        return Ok(None);
    }
    super::backend::lock(data).map(Some)
}

/// Copies the programs into Program Files. Running services lock the host, so they are
/// stopped for the copy and started again afterwards.
fn install_programs(prefix: &str) -> Result<Vec<String>> {
    let source = std::env::current_exe()?
        .parent()
        .context("Locate the folder of k3up.exe")?
        .to_path_buf();
    let target = install_dir()?;
    if !source.join(PROGRAMS[0]).is_file() {
        bail!("Place k3up-host.exe beside k3up.exe");
    }
    if same_dir(&source, &target) {
        return Ok(vec![format!(
            "Programs are already in {}",
            target.display()
        )]);
    }
    let running: Vec<String> = scm::list(prefix)?
        .into_iter()
        .filter(|service| matches!(service.scm, Scm::Running | Scm::Starting))
        .filter_map(|service| Some(service.name.get(prefix.len()..)?.to_string()))
        .collect();
    while_stopped(&mut Services { prefix }, &running, || {
        copy_programs(&source, &target)
    })
    .context("Updating the programs in Program Files failed")
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

fn copy_programs(source: &Path, target: &Path) -> Result<Vec<String>> {
    if !target.exists() {
        // Inherits the Program Files ACL, which lets only administrators write.
        std::fs::create_dir(target).with_context(|| format!("Create {}", target.display()))?;
    }
    let mut lines = vec![];
    for program in PROGRAMS {
        let from = source.join(program);
        if from.is_file() {
            let to = target.join(program);
            // A partly written host would break every workload, so the copy replaces the
            // installed program in one step or not at all.
            std::fs::read(&from)
                .with_context(|| format!("Read {}", from.display()))
                .and_then(|bytes| files::write_atomic(&to, &bytes, |_| Ok(())))
                .with_context(|| format!("Copy {program} to {}", to.display()))?;
            lines.push(format!("Copied {program} to {}", target.display()));
        }
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
