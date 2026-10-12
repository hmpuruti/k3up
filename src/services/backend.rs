//! Answers the agent protocol from Windows services, their definitions and the hosts' state
//! files, so the command line works the same in services mode.
use super::{
    files::{self, Layout},
    host, integrity, scm,
    status::{self, Definition, Drift, Scm, Service},
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

pub const DEFAULT_PREFIX: &str = super::DEFAULT_SERVICE_PREFIX;
const ELEVATE: &str = "Access denied. Run this from an elevated terminal";
const STOPPED: &str = "Stopped by request";
/// How long a stop keeps trying to record itself while another program holds the state file.
const STATE_WAIT: Duration = Duration::from_secs(2);

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
                if create_only {
                    let existing = self.definitions()?;
                    let services = self.services()?;
                    status::refuse_case_clashes(
                        &[workload.name.as_str()],
                        &known_names(&existing, &services),
                    )?;
                }
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
                self.record_stop(&name)?;
                Ok(Response::success(format!("Stopped {name}")))
            }
            Command::Restart { name } => {
                let _lock = self.lock_known(&name)?;
                self.stop(&name)?;
                // The host started next writes its own report.
                let _ = self.record_stop(&name);
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

    fn registration<'a>(
        &'a self,
        service: &'a str,
        instance: Option<&'a str>,
    ) -> scm::Registration<'a> {
        scm::Registration {
            service,
            host: &self.settings.host,
            data: self.layout.root(),
            instance,
        }
    }

    fn event(&self, name: &str, message: &str) {
        // History is a convenience; failing to record it must not fail the change itself.
        let _ = files::append_event(&self.layout, name, message);
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
        let mut names: Vec<&String> = services.keys().collect();
        for name in definitions.iter().flat_map(|found| found.keys()) {
            if status::same_name(&services, name).is_none() {
                names.push(name);
            }
        }
        names.sort();
        Ok(names
            .into_iter()
            .map(|name| {
                let definition = match &definitions {
                    None => Definition::Hidden,
                    Some(found) => status::same_name(found, name)
                        .map_or(Definition::Missing, |workload| {
                            Definition::Present(Box::new(workload.clone()))
                        }),
                };
                status::status(
                    name,
                    definition,
                    status::same_name(&services, name),
                    files::read_state(&self.layout.state(name)),
                    crate::win32::process_alive,
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
            crate::win32::process_alive,
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
        let names: Vec<&str> = manifest
            .workloads
            .iter()
            .map(|spec| spec.name.as_str())
            .collect();
        status::refuse_case_clashes(&names, &known_names(&existing, &services))?;
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
            let drift = match (current, service) {
                (Some(current), Some(_)) => self.drift(current)?,
                _ => Drift::default(),
            };
            // A definition changed outside K3 Up is written again from the manifest.
            let unchanged = current == Some(&spec) && self.signed(&spec.name);
            if unchanged && service.is_some() && drift.is_empty() {
                continue;
            }
            let relabel = current.is_some_and(|current| current.only_labels_differ(&spec));
            let running = service.is_some_and(|service| service.scm != Scm::Stopped);
            if running && !relabel && !unchanged {
                bail!("Stop '{}' before changing its definition", spec.name);
            }
            if running && drift.needs_stop() {
                bail!(
                    "Stop '{}' before changing its definition: its service settings differ ({drift})",
                    spec.name
                );
            }
            changes.push(match (current, drift.is_empty()) {
                (None, _) => format!("Create {}", spec.name),
                (Some(_), true) => format!("Update {}", spec.name),
                (Some(_), false) => {
                    format!("Update {}: service settings differ ({drift})", spec.name)
                }
            });
            plan.push(Change {
                previous: current.cloned(),
                registered: service.is_some(),
                relabel,
                restored: unchanged,
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
                if step.change.restored {
                    "Windows service settings restored from the definition"
                } else if step.change.relabel {
                    "Definition saved; group or description changed"
                } else {
                    "Definition saved"
                },
            );
        }
        let failed = self.start_new(&steps);
        Ok(Response::success(
            std::iter::once(message)
                .chain(failed)
                .collect::<Vec<_>>()
                .join("\n"),
        ))
    }

    /// Whether the definition still has the signature K3 Up gave it.
    fn signed(&self, name: &str) -> bool {
        let Ok(key) = integrity::load_key(&self.settings.prefix) else {
            return false;
        };
        let definition = std::fs::read(self.layout.definition(name));
        let signature = std::fs::read_to_string(self.layout.signature(name));
        let (Ok(definition), Ok(signature)) = (definition, signature) else {
            return false;
        };
        integrity::verify(&key, name, &definition, &signature).unwrap_or(false)
    }

    /// How the service of an existing workload differs from what its definition asks for, for
    /// example after a change in the Services console.
    fn drift(&self, workload: &Workload) -> Result<Drift> {
        let instance =
            files::read_definition_and_instance(&self.layout.definition(&workload.name))?
                .and_then(|(_, instance)| instance);
        let service = self.service_name(&workload.name);
        let wanted = scm::wanted(workload, &self.registration(&service, instance.as_deref()));
        Ok(status::drift(&wanted, &scm::settings(&service)?))
    }

    /// Checks what can be checked before the first change, and records what each change
    /// replaces so that `commit` can undo it.
    fn preflight(&self, plan: Vec<Change>) -> Result<Vec<Step>> {
        refuse_unprotected(&self.layout.workloads())?;
        integrity::load_key(&self.settings.prefix)?;
        if plan.iter().any(|change| !change.registered) {
            scm::check_create_access()?;
        }
        plan.into_iter()
            .map(|change| {
                let name = &change.workload.name;
                status::check_service_limits(&change.workload)?;
                let file = read_if_present(&self.layout.definition(name))?;
                let signature = read_if_present(&self.layout.signature(name))?;
                let service = if change.registered {
                    Some(scm::snapshot(&self.service_name(name))?)
                } else {
                    None
                };
                // A workload keeps its instance; one that is new gets its own.
                let instance = match &file {
                    Some(bytes) => files::instance_in(bytes),
                    None => Some(files::new_instance()),
                };
                files::check_definition_size(
                    name,
                    &files::definition_text(&change.workload, instance.as_deref())?,
                )?;
                if let Some(account) = service.as_ref().and_then(scm::Snapshot::account) {
                    status::refuse_unrestorable_account(name, account)?;
                }
                Ok(Step {
                    change,
                    file,
                    signature,
                    service,
                    instance,
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
        let instance = step.instance.as_deref();
        let key = integrity::load_key(&self.settings.prefix)?;
        step.written = true;
        write_definition(&self.layout, &key, workload, instance)?;
        let service = self.service_name(&workload.name);
        let registration = self.registration(&service, instance);
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
        // The previous signature comes back as it was, so an undo never signs anything.
        restore(&self.layout.definition(name), step.file.as_deref())?;
        restore(&self.layout.signature(name), step.signature.as_deref())
    }

    /// New definitions can opt into starting right away, as they do with the agent. As there,
    /// the definitions are saved by then, so a start that fails is the workload's state, not a
    /// failed apply; `k3up start NAME` tries again. Returns a line for each that failed.
    fn start_new(&self, steps: &[Step]) -> Vec<String> {
        steps
            .iter()
            .filter(|step| {
                step.change.previous.is_none() && status::starts_at_boot(&step.change.workload)
            })
            .filter_map(|step| {
                let name = &step.change.workload.name;
                let error = scm::start(&self.service_name(name)).err()?;
                let reason = format!("Failed to start: {error:#}");
                let failed = files::HostState {
                    state: State::Failed,
                    reason: reason.clone(),
                    updated_at: Utc::now(),
                    instance: step.instance.clone(),
                    ..Default::default()
                };
                let _ = files::write_state(&self.layout.state(name), &failed);
                self.event(name, &reason);
                Some(format!("{name} failed to start: {error:#}"))
            })
            .collect()
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
        // Any user may hold the state file open, so it goes first: if it cannot, nothing has
        // changed and the removal can be tried again.
        let state = self.layout.state(name);
        if let Err(error) = remove_if_present(&state) {
            return Err(error.context(format!("{} is in use; try again", state.display())));
        }
        if found.is_some() {
            scm::delete(&service)?;
        }
        remove_if_present(&path)?;
        remove_if_present(&self.layout.signature(name))?;
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
            let instance = files::read_definition_and_instance(&self.layout.definition(name))?
                .and_then(|(_, instance)| instance);
            scm::create(&workload, &self.registration(&service, instance.as_deref()))?;
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
        Ok(())
    }

    /// A host that already ended, after a failure or a stop outside K3 Up, left its last
    /// state behind; the request to stop replaces it. Any user may hold the state file open,
    /// which keeps the old report in place.
    fn record_stop(&self, name: &str) -> Result<()> {
        let path = self.layout.state(name);
        let instance = scm::query(&self.service_name(name))?.and_then(|found| found.instance);
        let read = files::read_state(&path);
        // A file held open without sharing cannot be read, and may still report a live host.
        let unread = read.is_none() && path.exists();
        let mut state = read
            .filter(|state| state.instance == instance)
            .unwrap_or_default();
        if state.state == State::Stopped && !unread {
            return Ok(());
        }
        state.instance = instance;
        state.state = State::Stopped;
        state.pid = None;
        state.reason = STOPPED.into();
        state.updated_at = Utc::now();
        let written = write_state_patiently(&path, &state);
        self.event(name, STOPPED);
        written.map_err(|_| {
            anyhow!(
                "Stopped {name}, but its state file is in use by another program; `k3up status` may show the old state until it is released"
            )
        })
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
            let current = status::generation(&self.statuses()?, files::events_mark(&self.layout));
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
    /// The definition is unchanged; only its service's settings are put back.
    restored: bool,
}

/// A change with what it replaces: the definition file's bytes, and the service's settings
/// when the service already existed.
struct Step {
    change: Change,
    file: Option<Vec<u8>>,
    signature: Option<Vec<u8>>,
    service: Option<scm::Snapshot>,
    instance: Option<String>,
    written: bool,
    created: bool,
}

/// The lock every command that changes services in the data directory `data` holds. It lives
/// in the folder only administrators may write.
pub fn lock(data: &Path) -> Result<files::Lock> {
    let layout = Layout::new(data);
    refuse_unprotected(&layout.workloads())?;
    files::lock(&layout.changes_lock(), Duration::from_secs(60))?
        .context("Another k3up command is changing services; try again")
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

fn known_names<'a>(
    definitions: &'a BTreeMap<String, Workload>,
    services: &'a BTreeMap<String, Service>,
) -> Vec<&'a str> {
    definitions
        .keys()
        .chain(services.keys())
        .map(String::as_str)
        .collect()
}

fn remove_if_present(path: &Path) -> Result<()> {
    match std::fs::remove_file(path) {
        Err(error) if error.kind() != ErrorKind::NotFound => {
            Err(error).with_context(|| format!("Delete {}", path.display()))
        }
        _ => Ok(()),
    }
}

fn write_state_patiently(path: &Path, state: &files::HostState) -> Result<()> {
    let deadline = Instant::now() + STATE_WAIT;
    loop {
        match files::write_state(path, state) {
            Err(_) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(100)),
            written => return written,
        }
    }
}

/// Owned by Administrators, which the host requires before it trusts a definition, and
/// signed, which it requires before it runs one. The signature follows the definition, so a
/// failure in between leaves a definition the host refuses rather than runs.
fn write_definition(
    layout: &Layout,
    key: &[u8],
    workload: &Workload,
    instance: Option<&str>,
) -> Result<()> {
    let name = &workload.name;
    let text = files::definition_text(workload, instance)?;
    let signature = integrity::sign(key, name, text.as_bytes())?;
    files::write_atomic(
        &layout.definition(name),
        text.as_bytes(),
        crate::win32::set_owner_to_administrators,
    )?;
    files::write_atomic(
        &layout.signature(name),
        signature.as_bytes(),
        crate::win32::set_owner_to_administrators,
    )
}

fn read_if_present(path: &Path) -> Result<Option<Vec<u8>>> {
    match std::fs::read(path) {
        Ok(bytes) => Ok(Some(bytes)),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error).with_context(|| format!("Read {}", path.display())),
    }
}

fn restore(path: &Path, bytes: Option<&[u8]>) -> Result<()> {
    match bytes {
        Some(bytes) => files::write_atomic(path, bytes, crate::win32::set_owner_to_administrators),
        None => remove_if_present(path),
    }
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
