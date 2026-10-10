//! k3up-host: runs one workload as a Windows service, using the same supervision rules as the
//! agent.
use super::{
    files::{self, HostState, Layout},
    status,
};
use crate::{
    model::{State, Workload},
    platform::ManagedProcess,
    supervisor::{self, Ended, Exit},
};
use anyhow::{Context, Result, anyhow, bail};
use chrono::Utc;
use std::{
    ffi::OsString,
    path::{Path, PathBuf},
    sync::{
        OnceLock,
        mpsc::{Receiver, RecvTimeoutError},
    },
    time::{Duration, Instant},
};
use windows_service::{
    define_windows_service,
    service::{
        ServiceControl, ServiceControlAccept, ServiceExitCode, ServiceState, ServiceStatus,
        ServiceType,
    },
    service_control_handler::{self, ServiceControlHandlerResult, ServiceStatusHandle},
    service_dispatcher,
};

/// Service-specific exit codes, shown by `sc query` and explained by `explain_exit`.
pub const EXIT_FAILED: u32 = 1;
pub const EXIT_DEFINITION: u32 = 2;
pub const EXIT_UNTRUSTED: u32 = 3;

const POLL: Duration = Duration::from_millis(250);
const ROTATE_EVERY: Duration = Duration::from_secs(10);
const PENDING_LIMIT: usize = 100;

static TARGET: OnceLock<(PathBuf, String, Option<String>)> = OnceLock::new();
define_windows_service!(ffi_main, service_main);

/// `instance` is the definition's instance, recorded in every report so that one left by an
/// earlier workload of the same name is told apart.
pub fn run(data: PathBuf, workload: String, instance: Option<String>) -> Result<()> {
    TARGET
        .set((data, workload, instance))
        .map_err(|_| anyhow!("Host already initialized"))?;
    service_dispatcher::start("K3Up", ffi_main)
        .context("k3up-host runs only when the Windows service manager starts it")
}

pub fn explain_exit(code: Option<u32>) -> String {
    match code {
        Some(EXIT_FAILED) => "the workload failed".into(),
        Some(EXIT_DEFINITION) => "its definition could not be loaded".into(),
        Some(EXIT_UNTRUSTED) => {
            "its data folders are not protected, so the host refused to run".into()
        }
        Some(code) => format!("the host exited with code {code}"),
        None => "the host stopped without giving a reason".into(),
    }
}

fn service_main(arguments: Vec<OsString>) {
    let Some((data, workload, instance)) = TARGET.get() else {
        return;
    };
    // The service manager passes the service's own name first.
    let service = arguments
        .first()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    let (stop, stopped) = std::sync::mpsc::channel();
    let Ok(handle) = service_control_handler::register(&service, move |control| match control {
        ServiceControl::Stop | ServiceControl::Shutdown => {
            let _ = stop.send(());
            ServiceControlHandlerResult::NoError
        }
        ServiceControl::Interrogate => ServiceControlHandlerResult::NoError,
        _ => ServiceControlHandlerResult::NotImplemented,
    }) else {
        return;
    };
    let mut host = Host {
        layout: Layout::new(data),
        name: workload.clone(),
        handle,
        state: HostState {
            instance: instance.clone(),
            ..HostState::default()
        },
        checkpoint: 0,
        pending: vec![],
    };
    let exit = host.run(&stopped);
    host.report(ServiceState::Stopped, exit, Duration::ZERO);
}

enum Ending {
    Requested,
    Final(State),
}

struct Host {
    layout: Layout,
    name: String,
    handle: ServiceStatusHandle,
    state: HostState,
    checkpoint: u32,
    /// Events not written yet, oldest first.
    pending: Vec<String>,
}

impl Host {
    fn run(&mut self, stop: &Receiver<()>) -> ServiceExitCode {
        let spec = match self.load() {
            Ok(spec) => spec,
            Err(refusal) => {
                self.log_line(&format!("Refusing to run: {:#}", refusal.detail));
                // Writing into folders another account controls could overwrite any file.
                if is_protected(&self.layout.states()) && is_protected(&self.layout.events_dir()) {
                    self.record(
                        State::Failed,
                        format!("Refusing to run: {}", refusal.reason),
                    );
                }
                return ServiceExitCode::ServiceSpecific(refusal.code);
            }
        };
        self.report(
            ServiceState::Running,
            ServiceExitCode::NO_ERROR,
            Duration::ZERO,
        );
        match self.supervise(&spec, stop) {
            Ending::Requested | Ending::Final(State::Completed) => ServiceExitCode::NO_ERROR,
            Ending::Final(_) => ServiceExitCode::ServiceSpecific(EXIT_FAILED),
        }
    }

    /// Reads the definition, but only from folders that no other account could have written.
    /// The reason is shown to every user, so it never quotes the definition.
    fn load(&self) -> Result<Workload, Refusal> {
        let definition = self.layout.definition(&self.name);
        if !definition.is_file() {
            return Err(Refusal::new(
                EXIT_DEFINITION,
                "the definition is missing",
                anyhow!("{} does not exist", definition.display()),
            ));
        }
        let paths = [
            self.layout.root().to_path_buf(),
            self.layout.workloads(),
            self.layout.logs(),
            self.layout.states(),
            self.layout.events_dir(),
            definition.clone(),
        ];
        if let Some(path) = paths.iter().find(|path| !is_protected(path)) {
            return Err(Refusal::new(
                EXIT_UNTRUSTED,
                format!("{} is not protected", path.display()),
                anyhow!(
                    "{} must belong to SYSTEM or Administrators, must not be a link, and only they may change it",
                    path.display()
                ),
            ));
        }
        if !crate::win32::ancestors_protected(self.layout.root()) {
            return Err(Refusal::new(
                EXIT_UNTRUSTED,
                "a folder above the data directory is not protected",
                anyhow!(
                    "Every folder above {} must not be a link, and only SYSTEM, Administrators and TrustedInstaller may move or change it",
                    self.layout.root().display()
                ),
            ));
        }
        crate::win32::open_inside(&definition)
            .and_then(|file| files::definition_from(file, &definition))
            .and_then(|spec| check(spec, &self.name))
            .map_err(|error| {
                Refusal::new(
                    EXIT_DEFINITION,
                    "the definition could not be read or is not valid",
                    error,
                )
            })
    }

    fn supervise(&mut self, spec: &Workload, stop: &Receiver<()>) -> Ending {
        loop {
            let now = Utc::now();
            let log = self.layout.log(&self.name);
            let launched =
                files::open_append(&log).and_then(|file| supervisor::launch_into(spec, file, now));
            let ended = match launched {
                Ok(process) => {
                    self.state.started_at = Some(now);
                    self.state.pid = Some(process.id());
                    self.record(State::Running, "Process started");
                    match self.watch(spec, process, stop) {
                        Some(ended) => ended,
                        None => return Ending::Requested,
                    }
                }
                Err(error) => {
                    // The error can quote the command line, which only administrators may read.
                    self.log_line(&format!("Launch failed: {error:#}"));
                    Ended {
                        code: 1,
                        reason: "Launch failed; the log has the reason".into(),
                        success: false,
                    }
                }
            };
            self.state.pid = None;
            self.state.last_exit = Some(ended.code);
            match supervisor::on_exit(spec, self.state.restart_count, true, ended) {
                Exit::Retry {
                    attempt,
                    delay_secs,
                    message,
                } => {
                    self.state.restart_count = attempt;
                    self.record(State::Backoff, message);
                    if self.pause(Duration::from_secs(delay_secs), stop) {
                        self.record(State::Stopped, "Stopped by request");
                        return Ending::Requested;
                    }
                }
                Exit::Final { state, message } => {
                    self.record(state, message);
                    return Ending::Final(state);
                }
            }
        }
    }

    /// Follows the process until it ends, returning how, or `None` once a stop request has
    /// ended it.
    fn watch(
        &mut self,
        spec: &Workload,
        mut process: ManagedProcess,
        stop: &Receiver<()>,
    ) -> Option<Ended> {
        let log = self.layout.log(&self.name);
        let mut rotated = Instant::now();
        loop {
            if !matches!(stop.recv_timeout(POLL), Err(RecvTimeoutError::Timeout)) {
                self.stop_process(spec, process);
                self.record(State::Stopped, "Stopped by request");
                return None;
            }
            let now = Utc::now();
            match process.poll() {
                Ok(Some(code)) => {
                    return Some(Ended {
                        code,
                        reason: format!("Process exited with code {code}"),
                        success: spec.is_success(code),
                    });
                }
                Ok(None) => {}
                Err(error) => {
                    self.log_line(&format!("Lost track of the process: {error:#}"));
                    return Some(Ended {
                        code: 1,
                        reason: "Lost track of the process; the log has the reason".into(),
                        success: false,
                    });
                }
            }
            if supervisor::timed_out(spec, self.state.started_at, now) {
                return Some(Ended {
                    code: 124,
                    reason: "Run timeout exceeded".into(),
                    success: spec.is_success(124),
                });
            }
            if self.state.restart_count > 0 && supervisor::stable(self.state.started_at, now) {
                self.state.restart_count = 0;
                self.save();
            }
            if rotated.elapsed() >= ROTATE_EVERY {
                rotated = Instant::now();
                let rotated_log = files::refuse_redirected(&log)
                    .and_then(|()| files::refuse_redirected(&log.with_extension("log.1")))
                    .and_then(|()| supervisor::rotate_log(&log));
                if let Err(error) = rotated_log {
                    self.log_line(&format!("Log rotation failed: {error:#}"));
                    self.note("Log rotation failed");
                }
            }
        }
    }

    /// Waits up to the stop timeout for the process to end, then ends its whole Job Object.
    fn stop_process(&mut self, spec: &Workload, mut process: ManagedProcess) {
        let wait = Duration::from_secs(spec.stop_timeout_secs);
        let hint = wait + Duration::from_secs(5);
        self.report(ServiceState::StopPending, ServiceExitCode::NO_ERROR, hint);
        process.graceful_stop();
        let deadline = Instant::now() + wait;
        let mut reported = Instant::now();
        while matches!(process.poll(), Ok(None)) && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(50));
            if reported.elapsed() >= Duration::from_secs(1) {
                reported = Instant::now();
                self.report(ServiceState::StopPending, ServiceExitCode::NO_ERROR, hint);
            }
        }
        process.force_stop();
        drop(process);
    }

    /// Sleeps for the retry delay. Returns true when a stop request ended the wait.
    fn pause(&self, delay: Duration, stop: &Receiver<()>) -> bool {
        !matches!(stop.recv_timeout(delay), Err(RecvTimeoutError::Timeout))
    }

    fn record(&mut self, state: State, reason: impl Into<String>) {
        let reason = reason.into();
        self.state.state = state;
        self.state.reason = reason.clone();
        if !matches!(state, State::Running | State::Starting) {
            self.state.pid = None;
        }
        self.save();
        self.note(&reason);
    }

    fn save(&mut self) {
        self.state.host_pid = std::process::id();
        self.state.updated_at = Utc::now();
        let _ = files::write_state(&self.layout.state(&self.name), &self.state);
    }

    /// Details for administrators only, in the workload's log.
    fn log_line(&self, text: &str) {
        use std::io::Write;
        let log = self.layout.log(&self.name);
        if !is_protected(&self.layout.logs()) {
            return;
        }
        if let Ok(mut file) = files::open_append(&log) {
            let _ = writeln!(file, "[{}] k3up-host: {text}", Utc::now().to_rfc3339());
        }
    }

    /// Records an event in the history. One that cannot be written yet, because another
    /// writer holds the history or a reader blocks the file, is kept and written with the
    /// next one, so supervision never waits on it.
    fn note(&mut self, message: &str) {
        if self.pending.len() == PENDING_LIMIT {
            self.pending.remove(0);
        }
        self.pending.push(message.to_string());
        while let Some(next) = self.pending.first() {
            match files::append_event(&self.layout, &self.name, next) {
                Ok(()) => {
                    self.pending.remove(0);
                }
                Err(error) => {
                    self.log_line(&format!("Activity history not updated yet: {error:#}"));
                    break;
                }
            }
        }
    }

    fn report(&mut self, state: ServiceState, exit_code: ServiceExitCode, wait_hint: Duration) {
        let pending = matches!(
            state,
            ServiceState::StartPending | ServiceState::StopPending
        );
        self.checkpoint = if pending { self.checkpoint + 1 } else { 0 };
        let _ = self.handle.set_service_status(ServiceStatus {
            service_type: ServiceType::OWN_PROCESS,
            current_state: state,
            controls_accepted: if state == ServiceState::Running {
                ServiceControlAccept::STOP | ServiceControlAccept::SHUTDOWN
            } else {
                ServiceControlAccept::empty()
            },
            exit_code,
            checkpoint: self.checkpoint,
            wait_hint,
            process_id: None,
        });
    }
}

fn check(spec: Workload, name: &str) -> Result<Workload> {
    if spec.name != name {
        bail!("The definition names '{}', not '{name}'", spec.name);
    }
    spec.validate()?;
    status::ensure_supported(&spec)?;
    Ok(spec)
}

fn is_protected(path: &Path) -> bool {
    crate::win32::is_protected(path)
}

struct Refusal {
    code: u32,
    reason: String,
    detail: anyhow::Error,
}

impl Refusal {
    fn new(code: u32, reason: impl Into<String>, detail: anyhow::Error) -> Self {
        Self {
            code,
            reason: reason.into(),
            detail,
        }
    }
}
