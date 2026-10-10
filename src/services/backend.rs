//! Answers the agent protocol from Windows services, their definitions and the hosts' state
//! files, so the command line works the same in services mode.
use super::{
    files::{self, Layout},
    host, scm,
    status::{self, Definition, Scm, Service},
};
use crate::{
    model::{Manifest, State, Status, Workload},
    protocol::{Command, Response},
};
use anyhow::{Context, Result, anyhow, bail};
use chrono::Utc;
use std::{
    collections::{BTreeMap, BTreeSet},
    io::ErrorKind,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

pub const DEFAULT_PREFIX: &str = "K3Up_";
const ELEVATE: &str = "Access denied. Run this from an elevated terminal";
const STOPPED: &str = "Stopped by request";

/// Where the services' host program is and how their names start.
#[derive(Debug, Clone)]
pub struct Settings {
    pub prefix: String,
    pub host: PathBuf,
}

impl Settings {
    /// K3UP_SERVICE_PREFIX and K3UP_HOST exist for tests, which need their own service names
    /// and the freshly built host. Everyone else gets the defaults.
    pub fn from_env() -> Result<Self> {
        let prefix = std::env::var("K3UP_SERVICE_PREFIX").unwrap_or_else(|_| DEFAULT_PREFIX.into());
        if prefix.is_empty()
            || prefix.len() > 32
            || !prefix
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'_')
        {
            bail!("K3UP_SERVICE_PREFIX must be 1 to 32 ASCII letters, digits or underscores");
        }
        let host = match std::env::var_os("K3UP_HOST") {
            Some(host) => PathBuf::from(host),
            None => super::setup::install_dir()?.join("k3up-host.exe"),
        };
        Ok(Self { prefix, host })
    }
}

pub struct Backend {
    layout: Layout,
    settings: Settings,
}

impl Backend {
    pub fn open(data: &Path) -> Result<Self> {
        Ok(Self::with(data, Settings::from_env()?))
    }

    pub fn with(data: &Path, settings: Settings) -> Self {
        Self {
            layout: Layout::new(data),
            settings,
        }
    }

    pub fn settings(&self) -> &Settings {
        &self.settings
    }

    pub fn handle(&self, command: Command) -> Result<Response> {
        self.execute(command).map_err(explain_denied)
    }

    fn execute(&self, command: Command) -> Result<Response> {
        match command {
            Command::List => Ok(Response {
                workloads: self.statuses()?,
                ..Response::success("Workloads")
            }),
            Command::Get { name } => Ok(Response {
                workloads: vec![self.get(&name)?],
                ..Response::success("Workload")
            }),
            Command::Export => Ok(Response {
                manifest: Some(Manifest {
                    version: 1,
                    workloads: self.definitions()?.into_values().collect(),
                }),
                ..Response::success("Configuration exported")
            }),
            Command::Apply { manifest, dry_run } => {
                let _lock = (!dry_run).then(|| self.lock()).transpose()?;
                self.apply(manifest, dry_run)
            }
            Command::Put {
                workload,
                create_only,
            } => {
                let _lock = self.lock()?;
                if create_only && self.known(&workload.name)? {
                    bail!(
                        "Workload '{}' already exists; use apply to update it",
                        workload.name
                    );
                }
                self.apply(
                    Manifest {
                        version: 1,
                        workloads: vec![*workload],
                    },
                    false,
                )
            }
            Command::Remove { name } => {
                let _lock = self.lock_known(&name)?;
                self.remove(&name)
            }
            Command::Start { name } => {
                let _lock = self.lock_known(&name)?;
                self.start(&name)?;
                Ok(Response {
                    workloads: vec![self.get(&name)?],
                    ..Response::success(format!("Start requested for {name}"))
                })
            }
            Command::Stop { name } => {
                let _lock = self.lock_known(&name)?;
                self.stop(&name)?;
                Ok(Response::success(format!("Stopped {name}")))
            }
            Command::Restart { name } => {
                let _lock = self.lock_known(&name)?;
                self.stop(&name)?;
                self.start(&name)?;
                Ok(Response {
                    workloads: vec![self.get(&name)?],
                    ..Response::success(format!("Restart requested for {name}"))
                })
            }
            Command::Logs { name, lines, after } => {
                self.get(&name)?;
                let path = self.layout.log(&name);
                if let Err(error) = std::fs::File::open(&path)
                    && error.kind() == ErrorKind::PermissionDenied
                {
                    return Err(error).with_context(|| format!("Read {}", path.display()));
                }
                let (text, offset) = crate::supervisor::read_log(&path, lines, after)?;
                Ok(Response {
                    text: Some(text),
                    offset: Some(offset),
                    ..Response::success("Logs")
                })
            }
            Command::Metrics { .. } => Ok(Response {
                metrics: Some(Box::new(self.metrics()?)),
                ..Response::success("Metrics")
            }),
            Command::Events { name, after } => {
                if let Some(name) = &name {
                    check_name(name)?;
                }
                Ok(Response {
                    events: files::read_events(&self.layout, name.as_deref(), after)?,
                    ..Response::success("Activity")
                })
            }
            Command::Info => Ok(Response::success(
                "Services mode: each workload runs as its own Windows service, without an agent",
            )),
            Command::Shutdown => bail!(
                "Services mode has no agent to stop. Stop workloads with `k3up stop NAME` or `k3up stop --group PATH`"
            ),
            Command::Watch { since, timeout_ms } => self.watch(since, timeout_ms),
        }
    }

    /// Serialises every command that changes definitions or services, across processes.
    fn lock(&self) -> Result<files::Lock> {
        lock(self.layout.root())
    }

    /// Names an unknown workload as such before asking for the lock, which users without
    /// administrator rights cannot take.
    fn lock_known(&self, name: &str) -> Result<files::Lock> {
        check_name(name)?;
        let exists = scm::exists(&self.service_name(name))?;
        if !status::known(&self.definition(name)?, exists) {
            bail!("Unknown workload '{name}'");
        }
        self.lock()
    }

    fn service_name(&self, name: &str) -> String {
        format!("{}{name}", self.settings.prefix)
    }

    fn registration<'a>(&'a self, service: &'a str) -> scm::Registration<'a> {
        scm::Registration {
            service,
            host: &self.settings.host,
            data: self.layout.root(),
        }
    }

    fn event(&self, name: &str, message: &str) {
        // History is a convenience; failing to record it must not fail the change itself.
        let _ = files::append_event(&self.layout.events(name), name, message);
    }

    fn services(&self) -> Result<BTreeMap<String, Service>> {
        let prefix = self.settings.prefix.len();
        Ok(scm::list(&self.settings.prefix)?
            .into_iter()
            .filter_map(|service| {
                let name = service.name.get(prefix..)?.to_string();
                Some((name, service))
            })
            .collect())
    }

    /// Every definition. Fails for users who may not read them.
    fn definitions(&self) -> Result<BTreeMap<String, Workload>> {
        let mut found = BTreeMap::new();
        for name in files::names(&self.layout.workloads(), "toml")? {
            if let Some(workload) = files::read_definition(&self.layout.definition(&name))? {
                found.insert(name, workload);
            }
        }
        Ok(found)
    }

    fn definition(&self, name: &str) -> Result<Definition> {
        match files::read_definition(&self.layout.definition(name)) {
            Ok(Some(workload)) => Ok(Definition::Present(Box::new(workload))),
            Ok(None) => Ok(Definition::Missing),
            Err(error) if denied(&error) => Ok(Definition::Hidden),
            Err(error) => Err(error),
        }
    }

    fn statuses(&self) -> Result<Vec<Status>> {
        let services = self.services()?;
        let definitions = match self.definitions() {
            Ok(definitions) => Some(definitions),
            Err(error) if denied(&error) => None,
            Err(error) => return Err(error),
        };
        let mut names: BTreeSet<&String> = services.keys().collect();
        names.extend(definitions.iter().flat_map(|found| found.keys()));
        Ok(names
            .into_iter()
            .map(|name| {
                let definition = match &definitions {
                    None => Definition::Hidden,
                    Some(found) => found.get(name).map_or(Definition::Missing, |workload| {
                        Definition::Present(Box::new(workload.clone()))
                    }),
                };
                status::status(
                    name,
                    definition,
                    services.get(name),
                    files::read_state(&self.layout.state(name)),
                )
            })
            .collect())
    }

    fn get(&self, name: &str) -> Result<Status> {
        check_name(name)?;
        let service = scm::query(&self.service_name(name))?;
        let definition = self.definition(name)?;
        if !status::known(&definition, service.is_some()) {
            bail!("Unknown workload '{name}'");
        }
        Ok(status::status(
            name,
            definition,
            service.as_ref(),
            files::read_state(&self.layout.state(name)),
        ))
    }

    fn known(&self, name: &str) -> Result<bool> {
        Ok(self.layout.definition(name).exists() || scm::exists(&self.service_name(name))?)
    }

    fn apply(&self, manifest: Manifest, dry_run: bool) -> Result<Response> {
        if manifest.version != 1 {
            bail!("Unsupported manifest version");
        }
        let existing = self.definitions()?;
        let services = self.services()?;
        let mut seen = BTreeSet::new();
        for spec in &manifest.workloads {
            if !seen.insert(&spec.name) {
                bail!("Duplicate name '{}'", spec.name);
            }
            status::ensure_supported(spec)
                .with_context(|| format!("Invalid workload '{}'", spec.name))?;
        }
        let mut merged = existing.clone();
        for spec in &manifest.workloads {
            merged.insert(spec.name.clone(), spec.clone());
        }
        Manifest {
            version: 1,
            workloads: merged.into_values().collect(),
        }
        .validate()?;
        let mut changes = vec![];
        let mut plan = vec![];
        for spec in manifest.workloads {
            let current = existing.get(&spec.name);
            let service = services.get(&spec.name);
            if current == Some(&spec) && service.is_some() {
                continue;
            }
            let relabel = current.is_some_and(|current| current.only_labels_differ(&spec));
            let running = service.is_some_and(|service| service.scm != Scm::Stopped);
            if running && !relabel && current != Some(&spec) {
                bail!("Stop '{}' before changing its definition", spec.name);
            }
            changes.push(format!(
                "{} {}",
                if current.is_some() {
                    "Update"
                } else {
                    "Create"
                },
                spec.name
            ));
            plan.push(Change {
                previous: current.cloned(),
                registered: service.is_some(),
                relabel,
                workload: spec,
            });
        }
        let message = if changes.is_empty() {
            "No changes".into()
        } else {
            changes.join("\n")
        };
        if dry_run || plan.is_empty() {
            let preview = if dry_run { "Preview only\n" } else { "" };
            return Ok(Response::success(format!("{preview}{message}")));
        }
        let mut steps = self.preflight(plan)?;
        self.commit(&mut steps)?;
        for step in &steps {
            self.event(
                &step.change.workload.name,
                if step.change.relabel {
                    "Definition saved; group or description changed"
                } else {
                    "Definition saved"
                },
            );
        }
        self.start_new(&steps)?;
        Ok(Response::success(message))
    }

    /// Checks what can be checked before the first change, and records what each change
    /// replaces so that `commit` can undo it.
    fn preflight(&self, plan: Vec<Change>) -> Result<Vec<Step>> {
        refuse_unprotected(&self.layout.workloads())?;
        if plan.iter().any(|change| !change.registered) {
            scm::check_create_access()?;
        }
        plan.into_iter()
            .map(|change| {
                let name = &change.workload.name;
                let path = self.layout.definition(name);
                let file = match std::fs::read(&path) {
                    Ok(bytes) => Some(bytes),
                    Err(error) if error.kind() == ErrorKind::NotFound => None,
                    Err(error) => {
                        return Err(error).with_context(|| format!("Read {}", path.display()));
                    }
                };
                let service = if change.registered {
                    Some(scm::snapshot(&self.service_name(name))?)
                } else {
                    None
                };
                Ok(Step {
                    change,
                    file,
                    service,
                    written: false,
                    created: false,
                })
            })
            .collect()
    }

    /// Saves every change, or none: after a failure, the changes made so far are undone.
    fn commit(&self, steps: &mut [Step]) -> Result<()> {
        for index in 0..steps.len() {
            if let Err(error) = self.save(&mut steps[index]) {
                return Err(self.roll_back(&steps[..=index], error));
            }
        }
        Ok(())
    }

    fn save(&self, step: &mut Step) -> Result<()> {
        let workload = &step.change.workload;
        write_definition(&self.layout.definition(&workload.name), workload)?;
        step.written = true;
        let service = self.service_name(&workload.name);
        let registration = self.registration(&service);
        if step.service.is_some() {
            return scm::reconfigure(workload, &registration);
        }
        scm::create(workload, &registration)?;
        step.created = true;
        Ok(())
    }

    fn roll_back(&self, steps: &[Step], error: anyhow::Error) -> anyhow::Error {
        let failures: Vec<String> = steps
            .iter()
            .rev()
            .filter(|step| step.written)
            .filter_map(|step| {
                let undone = self.undo(step).err()?;
                Some(format!("{}: {undone:#}", step.change.workload.name))
            })
            .collect();
        if failures.is_empty() {
            error.context("Saving failed and every change was undone")
        } else {
            error.context(format!(
                "Saving failed and undoing it did not finish ({})",
                failures.join("; ")
            ))
        }
    }

    fn undo(&self, step: &Step) -> Result<()> {
        let name = &step.change.workload.name;
        let service = self.service_name(name);
        match &step.service {
            Some(snapshot) => scm::restore(&service, snapshot)?,
            None if step.created => scm::delete(&service)?,
            None => {}
        }
        let path = self.layout.definition(name);
        match &step.file {
            Some(bytes) => {
                files::write_atomic(&path, bytes, crate::win32::set_owner_to_administrators)
            }
            None => match std::fs::remove_file(&path) {
                Err(error) if error.kind() != ErrorKind::NotFound => {
                    Err(error).with_context(|| format!("Delete {}", path.display()))
                }
                _ => Ok(()),
            },
        }
    }

    /// New definitions can opt into starting right away, as they do with the agent.
    fn start_new(&self, steps: &[Step]) -> Result<()> {
        let failures: Vec<String> = steps
            .iter()
            .map(|step| &step.change)
            .filter(|change| change.previous.is_none() && status::starts_at_boot(&change.workload))
            .filter_map(|change| {
                let name = &change.workload.name;
                let error = scm::start(&self.service_name(name)).err()?;
                Some(format!("Created {name}, but it did not start: {error:#}"))
            })
            .collect();
        if failures.is_empty() {
            Ok(())
        } else {
            Err(anyhow!(failures.join("\n")))
        }
    }

    fn remove(&self, name: &str) -> Result<Response> {
        check_name(name)?;
        let service = self.service_name(name);
        let found = scm::query(&service)?;
        if !status::known(&self.definition(name)?, found.is_some()) {
            bail!("Unknown workload '{name}'");
        }
        let path = self.layout.definition(name);
        if found
            .as_ref()
            .is_some_and(|found| found.scm != Scm::Stopped)
        {
            bail!("Stop '{name}' before removing it");
        }
        if found.is_some() {
            scm::delete(&service)?;
        }
        for file in [path, self.layout.state(name)] {
            match std::fs::remove_file(&file) {
                Err(error) if error.kind() != ErrorKind::NotFound => {
                    return Err(error).with_context(|| format!("Delete {}", file.display()));
                }
                _ => {}
            }
        }
        self.event(name, "Definition removed; log retained");
        Ok(Response::success(format!("Removed {name}")))
    }

    fn start(&self, name: &str) -> Result<()> {
        check_name(name)?;
        let service = self.service_name(name);
        let exists = scm::exists(&service)?;
        let definition = self.definition(name)?;
        if !status::known(&definition, exists) {
            bail!("Unknown workload '{name}'");
        }
        let workload = match definition {
            Definition::Present(workload) => workload,
            Definition::Missing => {
                bail!("Definition missing; `k3up remove {name}` deletes its service")
            }
            Definition::Hidden => bail!(ELEVATE),
        };
        status::ensure_supported(&workload)?;
        if !exists {
            scm::create(&workload, &self.registration(&service))?;
            self.event(name, "Windows service created again from the definition");
        }
        let requested = Utc::now();
        scm::start(&service)?;
        let state = scm::wait(&service, scm::START_TIMEOUT, |state| {
            matches!(state, Scm::Running | Scm::Stopped)
        })?;
        match state {
            Some(Scm::Running) => Ok(()),
            Some(Scm::Stopped) => {
                let report = files::read_state(&self.layout.state(name))
                    .filter(|report| report.updated_at >= requested);
                match report {
                    // A job that finished this quickly succeeded.
                    Some(report) if report.state == State::Completed => Ok(()),
                    Some(report) => bail!("{name} stopped right after starting: {}", report.reason),
                    None => bail!(
                        "{name} stopped right after starting: {}",
                        host::explain_exit(scm::specific_exit_code(&service)?)
                    ),
                }
            }
            _ => bail!(
                "{name} did not start within {} seconds",
                scm::START_TIMEOUT.as_secs()
            ),
        }
    }

    fn stop(&self, name: &str) -> Result<()> {
        check_name(name)?;
        let service = self.service_name(name);
        let definition = self.definition(name)?;
        if !status::known(&definition, scm::exists(&service)?) {
            bail!("Unknown workload '{name}'");
        }
        let timeout = match definition {
            Definition::Present(workload) => workload.stop_timeout_secs,
            _ => 30,
        };
        scm::stop(&service, Duration::from_secs(timeout + 30))?;
        self.record_stop(name)
    }

    /// A host that already ended, after a failure or a stop outside K3 Up, left its last
    /// state behind; the request to stop replaces it.
    fn record_stop(&self, name: &str) -> Result<()> {
        let path = self.layout.state(name);
        let mut state = files::read_state(&path).unwrap_or_default();
        if state.state == State::Stopped {
            return Ok(());
        }
        state.state = State::Stopped;
        state.pid = None;
        state.reason = STOPPED.into();
        state.updated_at = Utc::now();
        files::write_state(&path, &state)?;
        self.event(name, STOPPED);
        Ok(())
    }

    fn metrics(&self) -> Result<crate::metrics::Metrics> {
        let roster: Vec<(String, u32)> = self
            .statuses()?
            .into_iter()
            .filter_map(|status| Some((status.workload.name, status.pid?)))
            .collect();
        Ok(crate::metrics::sample_once(
            self.layout.root().to_path_buf(),
            &roster,
        ))
    }

    fn watch(&self, since: u64, timeout_ms: u64) -> Result<Response> {
        let deadline = Instant::now() + Duration::from_millis(timeout_ms.min(60_000));
        loop {
            let current = status::generation(&self.statuses()?);
            let now = Instant::now();
            if current != since || now >= deadline {
                return Ok(Response {
                    generation: Some(current),
                    ..Response::success("Generation")
                });
            }
            std::thread::sleep((deadline - now).min(Duration::from_secs(1)));
        }
    }
}

struct Change {
    workload: Workload,
    previous: Option<Workload>,
    registered: bool,
    relabel: bool,
}

/// A change with what it replaces: the definition file's bytes, and the service's settings
/// when the service already existed.
struct Step {
    change: Change,
    file: Option<Vec<u8>>,
    service: Option<scm::Snapshot>,
    written: bool,
    created: bool,
}

/// The lock every command that changes services in the data directory `data` holds. It lives
/// in the folder only administrators may write.
pub fn lock(data: &Path) -> Result<files::Lock> {
    let folder = Layout::new(data).workloads();
    refuse_unprotected(&folder)?;
    files::lock(&folder.join(".lock"), Duration::from_secs(60))
}

fn refuse_unprotected(folder: &Path) -> Result<()> {
    // Users without administrator rights may not read the folder's permissions; this says
    // so, rather than calling the folder unprotected.
    crate::win32::owned_by_system_or_administrators(folder)?;
    if !crate::win32::is_protected(folder) {
        bail!(
            "{} is not protected; services mode refuses to write definitions there. Run `k3up services enable` from an elevated terminal",
            folder.display()
        );
    }
    Ok(())
}

/// Owned by Administrators, which the host requires before it trusts a definition.
fn write_definition(path: &Path, workload: &Workload) -> Result<()> {
    let text = files::definition_text(workload)?;
    files::write_atomic(
        path,
        text.as_bytes(),
        crate::win32::set_owner_to_administrators,
    )
}

/// Names become file and service names, so anything else is refused before it reaches either.
fn check_name(name: &str) -> Result<()> {
    if name.is_empty()
        || name.len() > 64
        || !name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
    {
        bail!("Unknown workload '{name}'");
    }
    Ok(())
}

fn denied(error: &anyhow::Error) -> bool {
    error.chain().any(|cause| {
        cause
            .downcast_ref::<std::io::Error>()
            .is_some_and(|error| error.kind() == ErrorKind::PermissionDenied)
            || cause
                .downcast_ref::<windows_service::Error>()
                .is_some_and(|error| {
                    matches!(error, windows_service::Error::Winapi(error)
                        if error.kind() == ErrorKind::PermissionDenied)
                })
    })
}

fn explain_denied(error: anyhow::Error) -> anyhow::Error {
    if denied(&error) {
        return anyhow!("{ELEVATE} ({error:#})");
    }
    error
}
