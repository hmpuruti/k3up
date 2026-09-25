use crate::{
    model::*,
    platform::ManagedProcess,
    protocol::{Command, Response},
    store::{Record, Store},
};
use anyhow::{Context, Result, bail};
use chrono::{DateTime, Duration, Utc};
use std::{
    collections::BTreeMap,
    fs::OpenOptions,
    io::{Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
};

const LOG_LIMIT: u64 = 5 * 1024 * 1024;
const LOG_CHECK: Duration = Duration::seconds(10);
/// Polling interval while something is mid-transition, such as a TCP readiness check.
const BUSY: std::time::Duration = std::time::Duration::from_millis(250);
/// Longest sleep when nothing is due. Unix wakes earlier on child exit. Kept short because
/// timers stop while the machine sleeps, so a deadline can be this late after resume.
const IDLE: std::time::Duration = std::time::Duration::from_secs(15);
const LOG_WINDOW: u64 = 128 * 1024;

struct Runtime {
    status: Status,
    process: Option<ManagedProcess>,
    retry_at: Option<DateTime<Utc>>,
}

pub struct Engine {
    store: Store,
    data: PathBuf,
    entries: BTreeMap<String, Runtime>,
    order: Vec<String>,
    last_log_check: DateTime<Utc>,
    last_agent_fault: Option<String>,
    monitor: crate::metrics::Monitor,
    tracked: Vec<(String, u32)>,
    /// Bumped on every observable change; clients wait on it instead of polling.
    generation: tokio::sync::watch::Sender<u64>,
    started_at: DateTime<Utc>,
}

impl Engine {
    pub fn open(data: &Path) -> Result<Self> {
        std::fs::create_dir_all(data.join("logs"))?;
        let store = Store::open(&data.join("k3up.db"))?;
        let now = Utc::now();
        let mut entries = BTreeMap::new();
        let mut notes = vec![];
        for record in store.load()? {
            let desired = record.desired && record.workload.kind == Kind::Service;
            let mut next = record.next_run;
            if let Some(schedule) = &record.workload.schedule
                && (next.is_none() || (next < Some(now) && schedule.missed == Missed::Skip))
            {
                // An expired or unreadable schedule must never keep the agent from starting.
                next = match schedule.next_after(now) {
                    Ok(Some(next)) => Some(next),
                    Ok(None) => {
                        if record.next_run.is_some() {
                            notes.push((
                                record.workload.name.clone(),
                                "Schedule has no further executions".to_string(),
                            ));
                        }
                        None
                    }
                    Err(error) => {
                        notes.push((
                            record.workload.name.clone(),
                            format!("Schedule disabled: {error:#}"),
                        ));
                        None
                    }
                };
            }
            store.runtime(&record.workload.name, desired, next)?;
            let status = Status {
                workload: record.workload,
                state: State::Stopped,
                desired_running: desired,
                pid: None,
                restart_count: 0,
                started_at: None,
                next_run: next,
                last_exit: None,
                reason: if desired {
                    "Waiting for startup reconciliation"
                } else {
                    "Stopped"
                }
                .into(),
            };
            entries.insert(
                status.workload.name.clone(),
                Runtime {
                    status,
                    process: None,
                    retry_at: None,
                },
            );
        }
        let order = Manifest {
            version: 1,
            workloads: entries
                .values()
                .map(|entry| entry.status.workload.clone())
                .collect(),
        }
        .validate()?;
        store.event(
            "agent",
            "Agent started; recovering persisted service state. Interrupted jobs are not replayed",
        )?;
        let mut engine = Self {
            store,
            data: data.into(),
            entries,
            order,
            last_log_check: now,
            last_agent_fault: None,
            monitor: crate::metrics::Monitor::start(data.into()),
            tracked: vec![],
            generation: tokio::sync::watch::Sender::new(0),
            started_at: now,
        };
        for (name, note) in notes {
            engine.event(&name, note)?;
        }
        engine.stop_leftovers();
        // Requesting each recovered service marks its prerequisites wanted too, so boot services
        // that depend on a job run that job first instead of failing on a stopped dependency.
        for name in engine.order.clone() {
            if engine.entries[&name].status.desired_running {
                engine.request_start(&name, "Recovering requested state")?;
            }
        }
        Ok(engine)
    }

    fn status(&self, name: &str) -> Result<Status> {
        Ok(self
            .entries
            .get(name)
            .with_context(|| format!("Unknown workload '{name}'"))?
            .status
            .clone())
    }
    fn event(&mut self, name: &str, message: impl Into<String>) -> Result<()> {
        let message = message.into();
        self.store.event(name, &message)?;
        if let Some(entry) = self.entries.get_mut(name) {
            entry.status.reason = message;
        }
        self.changed();
        Ok(())
    }
    fn changed(&self) {
        self.generation.send_modify(|generation| *generation += 1);
    }
    pub fn subscribe(&self) -> tokio::sync::watch::Receiver<u64> {
        self.generation.subscribe()
    }
    /// Hands the metrics sampler the current set of live processes, when it changed.
    fn publish_roster(&mut self) {
        let roster: Vec<_> = self
            .entries
            .iter()
            .filter_map(|(name, entry)| entry.status.pid.map(|pid| (name.clone(), pid)))
            .collect();
        if roster != self.tracked {
            self.monitor.track(roster.clone());
            self.tracked = roster;
        }
    }

    /// How long supervision may sleep before something is due.
    pub fn next_wake(&self) -> std::time::Duration {
        let now = Utc::now();
        let mut due: Option<DateTime<Utc>> = None;
        let mut at = |time: DateTime<Utc>| due = Some(due.map_or(time, |due| due.min(time)));
        for entry in self.entries.values() {
            let status = &entry.status;
            // Anything reconcile would act on right now, such as a service registered to start.
            let wants_start = status.desired_running
                && entry.process.is_none()
                && status.state != State::Failed
                && entry.retry_at.is_none();
            if wants_start || status.state == State::Starting {
                return BUSY;
            }
            entry.retry_at.into_iter().for_each(&mut at);
            status.next_run.into_iter().for_each(&mut at);
            if entry.process.is_none() {
                continue;
            }
            at(self.last_log_check + LOG_CHECK);
            if let Some(started) = status.started_at {
                if let Some(limit) = status.workload.run_timeout_secs {
                    at(started + Duration::seconds(limit as i64));
                }
                if status.restart_count > 0 {
                    at(started + Duration::seconds(60));
                }
            }
            // Windows has no child-exit signal here, so exits are found by polling.
            if cfg!(not(unix)) {
                at(now + Duration::seconds(1));
            }
        }
        due.map_or(IDLE, |due| {
            (due - now)
                .to_std()
                .unwrap_or_default()
                .min(IDLE)
                // Waking a hair late avoids an extra loop for a deadline not yet reached.
                .saturating_add(std::time::Duration::from_millis(5))
        })
    }
    /// Records an error without interrupting other workloads. Repeats are not re-recorded,
    /// so a persistent fault cannot flood the activity history.
    fn fault(&mut self, name: &str, error: &anyhow::Error) {
        let message = format!("Agent error: {error:#}");
        let repeated = match self.entries.get(name) {
            Some(entry) => entry.status.reason == message,
            None => self.last_agent_fault.as_ref() == Some(&message),
        };
        if repeated {
            return;
        }
        eprintln!("{name}: {message}");
        if !self.entries.contains_key(name) {
            self.last_agent_fault = Some(message.clone());
        }
        if let Err(error) = self.event(name, message) {
            eprintln!("{name}: could not record activity: {error:#}");
        }
    }
    fn persist(&self, name: &str) -> Result<()> {
        let status = &self.entries[name].status;
        self.store
            .runtime(name, status.desired_running, status.next_run)?;
        self.changed();
        Ok(())
    }
    fn log_path(&self, name: &str) -> PathBuf {
        self.data.join("logs").join(format!("{name}.log"))
    }
    fn ticket_path(&self, name: &str) -> PathBuf {
        self.data.join("running").join(name)
    }

    /// A crashed agent cannot stop its workloads. Its successor stops them from the tickets
    /// it left, before recovery starts new copies.
    fn stop_leftovers(&mut self) {
        let Ok(tickets) = std::fs::read_dir(self.data.join("running")) else {
            return;
        };
        for ticket in tickets.flatten() {
            let path = ticket.path();
            let name = ticket.file_name().to_string_lossy().into_owned();
            let timeout = self
                .entries
                .get(&name)
                .map_or(10, |entry| entry.status.workload.stop_timeout_secs);
            if let Ok(text) = std::fs::read_to_string(&path)
                && let Some(pid) =
                    crate::platform::stop_leftover(&text, std::time::Duration::from_secs(timeout))
            {
                let message = format!("Stopped process {pid} left running by a previous agent");
                if let Err(error) = self.event(&name, message) {
                    eprintln!("{name}: {error:#}");
                }
            }
            let _ = std::fs::remove_file(path);
        }
    }

    fn remember(&self, name: &str, pid: u32) {
        let Some(ticket) = crate::platform::process_ticket(pid) else {
            return;
        };
        let path = self.ticket_path(name);
        let written = std::fs::create_dir_all(self.data.join("running"))
            .and_then(|()| std::fs::write(&path, ticket));
        if let Err(error) = written {
            eprintln!("{name}: could not record process {pid}: {error}");
        }
    }

    fn forget(&self, name: &str) {
        let _ = std::fs::remove_file(self.ticket_path(name));
    }

    pub async fn handle(&mut self, command: Command) -> Result<Response> {
        let response = self.execute(command).await;
        self.publish_roster();
        response
    }

    async fn execute(&mut self, command: Command) -> Result<Response> {
        match command {
            Command::List => Ok(Response {
                workloads: self
                    .entries
                    .values()
                    .map(|entry| entry.status.clone())
                    .collect(),
                ..Response::success("Workloads")
            }),
            Command::Get { name } => Ok(Response {
                workloads: vec![self.status(&name)?],
                ..Response::success("Workload")
            }),
            Command::Export => Ok(Response {
                manifest: Some(Manifest {
                    version: 1,
                    workloads: self
                        .entries
                        .values()
                        .map(|entry| entry.status.workload.clone())
                        .collect(),
                }),
                ..Response::success("Configuration exported")
            }),
            Command::Apply { manifest, dry_run } => self.apply(manifest, dry_run),
            Command::Put {
                workload,
                create_only,
            } => {
                if create_only && self.entries.contains_key(&workload.name) {
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
                let status = self.status(&name)?;
                if self
                    .entries
                    .values()
                    .any(|entry| entry.status.workload.depends_on.contains(&name))
                {
                    bail!("Remove dependent workloads first");
                }
                if status.pid.is_some() || status.desired_running {
                    bail!("Stop '{name}' before removing it");
                }
                self.store.remove(&name)?;
                self.entries.remove(&name);
                self.order.retain(|n| n != &name);
                self.event(&name, "Definition removed; log retained")?;
                Ok(Response::success(format!("Removed {name}")))
            }
            Command::Start { name } => {
                self.request_start(&name, "Start requested")?;
                self.reconcile();
                Ok(Response {
                    workloads: vec![self.status(&name)?],
                    ..Response::success(format!("Start requested for {name}"))
                })
            }
            Command::Stop { name } => {
                self.status(&name)?;
                // Persist intent before stopping. Recovery must never undo a successful stop request.
                self.entries.get_mut(&name).unwrap().status.desired_running = false;
                self.persist(&name)?;
                self.stop_process(&name).await?;
                self.event(
                    &name,
                    "Stopped by request; automatic recovery disabled until start or next schedule",
                )?;
                Ok(Response::success(format!("Stopped {name}")))
            }
            Command::Restart { name } => {
                self.status(&name)?;
                self.stop_process(&name).await?;
                self.request_start(&name, "Restart requested")?;
                self.reconcile();
                Ok(Response {
                    workloads: vec![self.status(&name)?],
                    ..Response::success(format!("Restart requested for {name}"))
                })
            }
            Command::Logs { name, lines, after } => {
                self.status(&name)?;
                let (text, offset) = self.read_log(&name, lines, after)?;
                Ok(Response {
                    text: Some(text),
                    offset: Some(offset),
                    ..Response::success("Logs")
                })
            }
            Command::Metrics { since } => Ok(Response {
                metrics: self.monitor.snapshot(since).map(Box::new),
                ..Response::success("Metrics")
            }),
            Command::Info => Ok(Response {
                agent: Some(crate::protocol::AgentInfo {
                    version: env!("CARGO_PKG_VERSION").into(),
                    pid: std::process::id(),
                    started_at: self.started_at,
                    data_dir: self.data.to_string_lossy().into_owned(),
                    executable: std::env::current_exe()
                        .map(|path| path.to_string_lossy().into_owned())
                        .unwrap_or_default(),
                }),
                ..Response::success("Agent")
            }),
            // The server intercepts this to stop its loop; reaching the engine means nothing to do.
            Command::Shutdown => Ok(Response::success("Shutting down")),
            Command::Watch { .. } => Ok(Response {
                generation: Some(*self.generation.borrow()),
                ..Response::success("Generation")
            }),
            Command::Events { name, after } => Ok(Response {
                events: self.store.events(name.as_deref(), after)?,
                ..Response::success("Activity")
            }),
        }
    }

    fn read_log(&self, name: &str, lines: usize, after: Option<u64>) -> Result<(String, u64)> {
        let Ok(mut file) = std::fs::File::open(self.log_path(name)) else {
            return Ok((String::new(), 0));
        };
        let length = file.metadata()?.len();
        let oldest = length.saturating_sub(LOG_WINDOW);
        // An offset past the end means the log was rotated; its new content starts at zero.
        let requested = after.map(|offset| if offset > length { 0 } else { offset });
        let start = requested.unwrap_or(oldest).max(oldest);
        file.seek(SeekFrom::Start(start))?;
        let mut bytes = Vec::new();
        file.take(length - start).read_to_end(&mut bytes)?;
        // Hold back a trailing, partially written UTF-8 character until the rest arrives.
        let complete = match std::str::from_utf8(&bytes) {
            Err(error) if error.error_len().is_none() => error.valid_up_to(),
            _ => bytes.len(),
        };
        let text = String::from_utf8_lossy(&bytes[..complete]);
        let offset = start + complete as u64;
        let text = match requested {
            Some(requested) if start > requested => {
                format!("[{} bytes skipped]\n{text}", start - requested)
            }
            Some(_) => text.into_owned(),
            None => {
                let text = if start > 0 {
                    text.split_once('\n').map_or("", |(_, rest)| rest)
                } else {
                    &text
                };
                let mut tail = text
                    .lines()
                    .rev()
                    .take(lines.clamp(1, 2000))
                    .collect::<Vec<_>>();
                tail.reverse();
                tail.join("\n")
            }
        };
        Ok((text, offset))
    }

    fn apply(&mut self, manifest: Manifest, dry_run: bool) -> Result<Response> {
        if manifest.version != 1 {
            bail!("Unsupported manifest version");
        }
        let mut merged: BTreeMap<_, _> = self
            .entries
            .iter()
            .map(|(name, entry)| (name.clone(), entry.status.workload.clone()))
            .collect();
        let mut seen = std::collections::BTreeSet::new();
        let mut changes = vec![];
        let now = Utc::now();
        for spec in &manifest.workloads {
            if !seen.insert(&spec.name) {
                bail!("Duplicate name '{}'", spec.name);
            }
            let existing = merged.get(&spec.name);
            if existing != Some(spec) {
                if let Some(entry) = self.entries.get(&spec.name)
                    && (entry.process.is_some() || entry.status.desired_running)
                {
                    bail!("Stop '{}' before changing its definition", spec.name);
                }
                changes.push(format!(
                    "{} {}",
                    if existing.is_some() {
                        "Update"
                    } else {
                        "Create"
                    },
                    spec.name
                ));
            }
            merged.insert(spec.name.clone(), spec.clone());
        }
        let order = Manifest {
            version: 1,
            workloads: merged.into_values().collect(),
        }
        .validate()?;
        let mut records = vec![];
        for spec in manifest.workloads {
            if self
                .entries
                .get(&spec.name)
                .is_some_and(|entry| entry.status.workload == spec)
            {
                continue;
            }
            // New definitions can opt into agent startup. Existing stopped definitions remain stopped.
            let desired = !self.entries.contains_key(&spec.name)
                && spec.start_at_boot
                && spec.kind == Kind::Service;
            let next_run = spec.first_run(now)?;
            records.push(Record {
                workload: spec,
                desired,
                next_run,
            });
        }
        let message = if changes.is_empty() {
            "No changes".into()
        } else {
            changes.join("\n")
        };
        if dry_run {
            return Ok(Response::success(format!("Preview only\n{message}")));
        }
        self.store.save_batch(&records)?;
        for record in records {
            let name = record.workload.name.clone();
            let status = Status {
                workload: record.workload,
                state: State::Stopped,
                desired_running: record.desired,
                pid: None,
                restart_count: 0,
                started_at: None,
                next_run: record.next_run,
                last_exit: None,
                reason: "Registered".into(),
            };
            self.entries.insert(
                name.clone(),
                Runtime {
                    status,
                    process: None,
                    retry_at: None,
                },
            );
            self.event(&name, "Definition saved")?;
        }
        self.order = order;
        Ok(Response::success(message))
    }

    fn request_start(&mut self, name: &str, reason: &str) -> Result<()> {
        self.status(name)?;
        if self.entries[name].process.is_some() {
            return Ok(());
        }
        let dependencies = self.entries[name].status.workload.depends_on.clone();
        for dependency in dependencies {
            let status = &self.entries[&dependency].status;
            // A job that already succeeded satisfies its dependents; services must be running.
            if !(status.workload.kind == Kind::Job && status.state == State::Completed) {
                self.request_start(&dependency, "Started as a dependency")?;
            }
        }
        let entry = self.entries.get_mut(name).unwrap();
        entry.status.desired_running = true;
        entry.status.restart_count = 0;
        entry.status.state = State::Pending;
        entry.retry_at = None;
        self.persist(name)?;
        self.event(name, reason)
    }

    async fn stop_process(&mut self, name: &str) -> Result<()> {
        let entry = self.entries.get_mut(name).unwrap();
        if let Some(mut process) = entry.process.take() {
            process.graceful_stop();
            let deadline = tokio::time::Instant::now()
                + std::time::Duration::from_secs(entry.status.workload.stop_timeout_secs);
            loop {
                if process.poll()?.is_some() || tokio::time::Instant::now() >= deadline {
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            }
            process.force_stop();
        }
        entry.status.pid = None;
        entry.status.state = State::Stopped;
        entry.retry_at = None;
        self.forget(name);
        Ok(())
    }

    fn reconcile(&mut self) {
        let now = Utc::now();
        for name in self.order.clone() {
            if let Err(error) = self.reconcile_one(&name, now) {
                self.fault(&name, &error);
            }
        }
    }

    fn reconcile_one(&mut self, name: &str, now: DateTime<Utc>) -> Result<()> {
        let entry = &self.entries[name];
        if !entry.status.desired_running
            || entry.process.is_some()
            || entry.status.state == State::Failed
            || entry.retry_at.is_some_and(|time| time > now)
        {
            return Ok(());
        }
        for dependency in &entry.status.workload.depends_on {
            let prerequisite = &self.entries[dependency];
            let status = &prerequisite.status;
            let ready = match status.workload.kind {
                Kind::Job => status.state == State::Completed,
                Kind::Service => status.state == State::Running,
            };
            if ready {
                continue;
            }
            let dependency = dependency.clone();
            if !status.desired_running && prerequisite.process.is_none() {
                let reason = format!("Not started: dependency {dependency} is {}", status.state);
                let entry = self.entries.get_mut(name).unwrap();
                entry.status.state = State::Failed;
                entry.status.desired_running = false;
                self.persist(name)?;
                return self.event(name, reason);
            }
            let reason = format!("Waiting for {dependency} to become ready");
            if self.entries[name].status.reason != reason {
                self.event(name, reason)?;
            }
            self.entries.get_mut(name).unwrap().status.state = State::Blocked;
            return Ok(());
        }
        let spec = self.entries[name].status.workload.clone();
        let launched = OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.log_path(name))
            .context("Open log file")
            .and_then(|mut log| {
                writeln!(log, "\n[{}] Starting {name}", now.to_rfc3339())?;
                ManagedProcess::spawn(&spec, log)
            });
        match launched {
            Ok(process) => {
                self.remember(name, process.id());
                let waits = spec.readiness_tcp.is_some();
                let entry = self.entries.get_mut(name).unwrap();
                entry.status.pid = Some(process.id());
                entry.status.started_at = Some(now);
                entry.status.state = if waits {
                    State::Starting
                } else {
                    State::Running
                };
                entry.process = Some(process);
                entry.retry_at = None;
                self.event(
                    name,
                    if waits {
                        "Process started; waiting for TCP readiness"
                    } else {
                        "Process started"
                    },
                )
            }
            Err(error) => self.exited(name, 1, format!("Launch failed: {error:#}")),
        }
    }

    fn exited(&mut self, name: &str, code: i32, reason: String) -> Result<()> {
        let entry = self.entries.get_mut(name).unwrap();
        entry.process.take(); // Drop closes the process group, including children left behind by the parent.
        self.forget(name);
        let entry = self.entries.get_mut(name).unwrap();
        entry.status.pid = None;
        entry.status.last_exit = Some(code);
        let spec = &entry.status.workload;
        // Jobs finish once; retrying scheduled jobs requires an explicit future execution.
        let retry = entry.status.desired_running
            && spec.kind == Kind::Service
            && (spec.restart == Restart::Always
                || (spec.restart == Restart::OnFailure && code != 0));

        let message = if retry && entry.status.restart_count < spec.max_restarts {
            let delay = spec
                .restart_delay_secs
                .saturating_mul(2u64.saturating_pow(entry.status.restart_count.min(16)))
                .min(300);
            entry.status.restart_count += 1;
            entry.retry_at = Some(Utc::now() + Duration::seconds(delay as i64));
            entry.status.state = State::Backoff;
            format!(
                "{reason}; retry {}/{} in {delay}s",
                entry.status.restart_count, spec.max_restarts
            )
        } else {
            entry.status.state = if code == 0 {
                State::Completed
            } else {
                State::Failed
            };
            entry.status.desired_running = false;
            if retry {
                format!("{reason}; restart limit reached")
            } else {
                reason
            }
        };
        self.persist(name)?;
        self.event(name, message)
    }

    /// Advances every workload. Errors are recorded against the affected workload and never
    /// stop the agent, so one broken definition cannot interrupt supervision of the others.
    pub async fn tick(&mut self) -> Result<()> {
        let now = Utc::now();
        for name in self.order.clone() {
            if let Err(error) = self.tick_one(&name, now).await {
                self.fault(&name, &error);
            }
        }
        self.reconcile();
        self.publish_roster();
        if now - self.last_log_check >= LOG_CHECK {
            self.last_log_check = now;
            for name in self.entries.keys().cloned().collect::<Vec<_>>() {
                if let Err(error) = self.rotate_log(&name) {
                    self.fault(&name, &error.context("Log rotation failed"));
                }
            }
        }
        Ok(())
    }

    async fn tick_one(&mut self, name: &str, now: DateTime<Utc>) -> Result<()> {
        let mut exit = None;
        {
            let entry = self.entries.get_mut(name).unwrap();
            if let Some(process) = entry.process.as_mut() {
                if let Some(code) = process.poll()? {
                    exit = Some((code, format!("Process exited with code {code}")));
                } else if entry.status.workload.run_timeout_secs.is_some_and(|limit| {
                    entry
                        .status
                        .started_at
                        .is_some_and(|start| (now - start).num_seconds() >= limit as i64)
                }) {
                    exit = Some((124, "Run timeout exceeded".into()));
                }
            }
        }
        if let Some((code, reason)) = exit {
            self.exited(name, code, reason)?;
        }
        if self.entries[name].status.state == State::Starting {
            let address = self.entries[name]
                .status
                .workload
                .readiness_tcp
                .clone()
                .context("Readiness address missing")?;
            if matches!(
                tokio::time::timeout(
                    std::time::Duration::from_millis(100),
                    tokio::net::TcpStream::connect(address)
                )
                .await,
                Ok(Ok(_))
            ) {
                self.entries.get_mut(name).unwrap().status.state = State::Running;
                self.event(name, "TCP readiness passed")?;
            } else if self.entries[name].status.started_at.is_some_and(|start| {
                (now - start).num_seconds()
                    >= self.entries[name].status.workload.startup_timeout_secs as i64
            }) {
                self.exited(name, 124, "TCP readiness timed out".into())?;
            }
        }
        if self.entries[name].status.state == State::Running
            && self.entries[name].status.restart_count > 0
            && self.entries[name]
                .status
                .started_at
                .is_some_and(|start| (now - start).num_seconds() >= 60)
        {
            self.entries.get_mut(name).unwrap().status.restart_count = 0;
            self.changed();
        }
        if self.entries[name]
            .status
            .next_run
            .is_some_and(|next| next <= now)
        {
            let schedule = self.entries[name]
                .status
                .workload
                .schedule
                .clone()
                .context("Next run recorded without a schedule")?;
            // Advance durably before dispatch. A crash in between can miss a run; never claim exactly once.
            let next = schedule.next_after(now);
            self.entries.get_mut(name).unwrap().status.next_run =
                next.as_ref().ok().copied().flatten();
            self.persist(name)?;
            if next.context("Schedule disabled")?.is_none() {
                self.event(name, "Schedule has no further executions")?;
            }
            if self.entries[name].process.is_some() && schedule.action == ScheduleAction::Start {
                self.event(name, "Scheduled start skipped: workload is already running")?;
            } else {
                if schedule.action == ScheduleAction::Restart {
                    self.stop_process(name).await?;
                }
                self.request_start(name, "Scheduled execution requested")?;
            }
        }
        Ok(())
    }

    fn rotate_log(&self, name: &str) -> Result<()> {
        let path = self.log_path(name);
        if !std::fs::metadata(&path).is_ok_and(|metadata| metadata.len() > LOG_LIMIT) {
            return Ok(());
        }
        // Copy/truncate keeps existing append handles valid; retention is best effort under heavy output.
        let mut source = std::fs::File::open(&path)?;
        let length = source.metadata()?.len();
        source.seek(SeekFrom::Start(length.saturating_sub(LOG_LIMIT)))?;
        let mut archive = std::fs::File::create(path.with_extension("log.1"))?;
        std::io::copy(&mut source.take(LOG_LIMIT), &mut archive)?;
        OpenOptions::new().write(true).open(&path)?.set_len(0)?;
        Ok(())
    }

    pub async fn shutdown(&mut self) -> Result<()> {
        for name in self.order.clone().into_iter().rev() {
            if let Err(error) = self.stop_process(&name).await {
                eprintln!("{name}: stop during shutdown failed: {error:#}");
            }
        }
        self.store.event(
            "agent",
            "Agent stopped; owned processes terminated, requested state preserved",
        )
    }
}
