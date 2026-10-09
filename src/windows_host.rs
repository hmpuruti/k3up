use anyhow::{Context, Result};
use std::{
    ffi::OsString,
    path::{Path, PathBuf},
    sync::OnceLock,
    time::{Duration, Instant},
};
use windows_service::{
    define_windows_service,
    service::*,
    service_control_handler::{self, ServiceControlHandlerResult, ServiceStatusHandle},
    service_dispatcher,
    service_manager::*,
};

const NAME: &str = "K3Up";
/// Installed into Program Files from the folder of the running k3up.exe. The agent is
/// required; the others are copied when present.
const PROGRAMS: [&str; 3] = ["k3up-agent.exe", "k3up.exe", "k3up-desktop.exe"];
static DATA: OnceLock<PathBuf> = OnceLock::new();
define_windows_service!(ffi_main, service_main);

pub fn run(data: PathBuf) -> Result<()> {
    log_to(&data.join("agent.log"));
    DATA.set(data)
        .map_err(|_| anyhow::anyhow!("Service already initialized"))?;
    service_dispatcher::start(NAME, ffi_main)?;
    Ok(())
}

/// A service has no console, so the agent's own messages go to a log beside its data, as
/// they do for an agent started by a login item.
fn log_to(path: &Path) {
    use std::os::windows::io::IntoRawHandle;
    use windows_sys::Win32::System::Console::{STD_ERROR_HANDLE, STD_OUTPUT_HANDLE, SetStdHandle};
    let Ok(file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
    else {
        return;
    };
    // Kept open for the life of the process. Rust opens it non-inheritable, so workloads
    // do not receive it.
    let handle = file.into_raw_handle();
    // SAFETY: the handle is open and owned by this process from here on.
    unsafe {
        SetStdHandle(STD_OUTPUT_HANDLE, handle);
        SetStdHandle(STD_ERROR_HANDLE, handle);
    }
}

fn report(handle: ServiceStatusHandle, state: ServiceState, checkpoint: u32, exit_code: u32) {
    let _ = handle.set_service_status(ServiceStatus {
        service_type: ServiceType::OWN_PROCESS,
        current_state: state,
        controls_accepted: if state == ServiceState::Running {
            ServiceControlAccept::STOP | ServiceControlAccept::SHUTDOWN
        } else {
            ServiceControlAccept::empty()
        },
        exit_code: ServiceExitCode::Win32(exit_code),
        checkpoint,
        wait_hint: if state == ServiceState::StopPending {
            Duration::from_secs(15)
        } else {
            Duration::default()
        },
        process_id: None,
    });
}

fn service_main(_: Vec<OsString>) {
    let (tx, mut rx) = tokio::sync::watch::channel(false);
    let Ok(handle) = service_control_handler::register(NAME, move |control| match control {
        ServiceControl::Stop | ServiceControl::Shutdown => {
            let _ = tx.send(true);
            ServiceControlHandlerResult::NoError
        }
        ServiceControl::Interrogate => ServiceControlHandlerResult::NoError,
        _ => ServiceControlHandlerResult::NotImplemented,
    }) else {
        return;
    };
    report(handle, ServiceState::Running, 0, 0);
    let result = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(anyhow::Error::from)
        .and_then(|runtime| {
            runtime.block_on(crate::server::run_with(
                DATA.get().unwrap().clone(),
                async move {
                    let _ = rx.changed().await;
                    // Stopping workloads can take each one's full stop timeout. Advancing the
                    // checkpoint tells the Service Control Manager the agent is still progressing.
                    tokio::spawn(async move {
                        let mut checkpoint = 0u32;
                        loop {
                            checkpoint = checkpoint.wrapping_add(1);
                            report(handle, ServiceState::StopPending, checkpoint, 0);
                            tokio::time::sleep(Duration::from_secs(5)).await;
                        }
                    });
                },
                crate::server::Access::Interactive,
            ))
        });
    if let Err(error) = &result {
        eprintln!("{error:#}");
    }
    report(
        handle,
        ServiceState::Stopped,
        0,
        if result.is_ok() { 0 } else { 1 },
    );
}

pub fn install_dir() -> Result<PathBuf> {
    Ok(
        PathBuf::from(std::env::var_os("ProgramFiles").context("ProgramFiles is not set")?)
            .join("K3 Up"),
    )
}

/// Installs the service, or updates an existing installation in place, and starts it. The
/// programs go to Program Files and the data to ProgramData, both shared by every user.
pub fn install(timeout: Duration) -> Result<()> {
    let data = crate::platform::machine_data_dir();
    let install_dir = install_dir()?;
    let source = std::env::current_exe()?
        .parent()
        .context("Locate the folder of k3up.exe")?
        .to_path_buf();
    anyhow::ensure!(
        source.join(PROGRAMS[0]).is_file(),
        "Place k3up-agent.exe beside k3up.exe"
    );
    let manager = ServiceManager::local_computer(
        None::<&str>,
        ServiceManagerAccess::CONNECT | ServiceManagerAccess::CREATE_SERVICE,
    )
    .context("Run installation from an elevated terminal")?;
    let existing = match manager.open_service(NAME, configure()) {
        Ok(service) => Some(service),
        Err(error) if missing(&error) => None,
        Err(error) => return Err(error).context("Open the K3 Up service"),
    };
    let mut created = vec![];
    let mut service = None;
    let result = install_into(
        &manager,
        existing.as_ref(),
        Locations {
            source: &source,
            data: &data,
            install_dir: &install_dir,
        },
        timeout,
        &mut created,
        &mut service,
    );
    if let Err(error) = result {
        // Only what this attempt created is removed, so a retry starts clean and an earlier
        // installation keeps its data.
        if let Some(service) = service {
            let _ = service.delete();
        }
        for directory in created.iter().rev() {
            let _ = std::fs::remove_dir_all(directory);
        }
        // An update may have stopped the earlier installation to replace its programs.
        if let Some(existing) = &existing {
            let _ = start_and_wait(existing);
        }
        return Err(error.context("Installation failed"));
    }
    Ok(())
}

fn configure() -> ServiceAccess {
    ServiceAccess::QUERY_STATUS
        | ServiceAccess::START
        | ServiceAccess::STOP
        | ServiceAccess::CHANGE_CONFIG
        | ServiceAccess::WRITE_DAC
        | ServiceAccess::DELETE
}

struct Locations<'a> {
    source: &'a Path,
    data: &'a Path,
    install_dir: &'a Path,
}

fn install_into(
    manager: &ServiceManager,
    existing: Option<&Service>,
    at: Locations,
    timeout: Duration,
    created: &mut Vec<PathBuf>,
    service: &mut Option<Service>,
) -> Result<()> {
    if at.data.exists() {
        crate::win32::protect_existing_dir(at.data)?;
    } else {
        crate::win32::create_protected_dir(at.data)?;
        created.push(at.data.to_path_buf());
    }
    if !same_dir(at.source, at.install_dir) {
        // A running agent locks its executable.
        if let Some(existing) = existing {
            wait_for_stop(existing, timeout)?;
        }
        if !at.install_dir.exists() {
            // Inherits the Program Files ACL, which lets only administrators write.
            std::fs::create_dir(at.install_dir)
                .with_context(|| format!("Create {}", at.install_dir.display()))?;
            created.push(at.install_dir.to_path_buf());
        }
        for program in PROGRAMS {
            let from = at.source.join(program);
            if from.is_file() {
                let to = at.install_dir.join(program);
                std::fs::copy(&from, &to)
                    .with_context(|| format!("Copy {program} to {}", to.display()))?;
            }
        }
    }
    let info = ServiceInfo {
        name: NAME.into(),
        display_name: "K3 Up agent".into(),
        service_type: ServiceType::OWN_PROCESS,
        start_type: ServiceStartType::AutoStart,
        error_control: ServiceErrorControl::Normal,
        executable_path: at.install_dir.join(PROGRAMS[0]),
        launch_arguments: vec![
            "--windows-service".into(),
            "--data-dir".into(),
            at.data.as_os_str().to_owned(),
        ],
        dependencies: vec![],
        account_name: None,
        account_password: None,
    };
    // Only a service created by this attempt goes in `service`, which a failure deletes.
    let registered: &Service = match existing {
        Some(existing) => {
            existing.change_config(&info)?;
            existing
        }
        None => service.insert(manager.create_service(&info, configure())?),
    };
    registered.set_description("Runs K3 Up workloads for every user, from boot until shutdown")?;
    // The last action repeats for every further failure, so the agent always comes back.
    registered.update_failure_actions(ServiceFailureActions {
        reset_period: ServiceFailureResetPeriod::After(Duration::from_secs(86400)),
        reboot_msg: None,
        command: None,
        actions: Some(
            [5, 5, 30]
                .into_iter()
                .map(|secs| ServiceAction {
                    action_type: ServiceActionType::Restart,
                    delay: Duration::from_secs(secs),
                })
                .collect(),
        ),
    })?;
    // Also restart after the agent exits with an error, not only after a crash.
    registered.set_failure_actions_on_non_crash_failures(true)?;
    crate::win32::set_service_security(registered.raw_handle())?;
    start_and_wait(registered)
}

fn same_dir(a: &Path, b: &Path) -> bool {
    match (std::fs::canonicalize(a), std::fs::canonicalize(b)) {
        (Ok(a), Ok(b)) => a == b,
        _ => false,
    }
}

fn missing(error: &windows_service::Error) -> bool {
    use windows_sys::Win32::Foundation::ERROR_SERVICE_DOES_NOT_EXIST;
    matches!(error, windows_service::Error::Winapi(error)
        if error.raw_os_error() == Some(ERROR_SERVICE_DOES_NOT_EXIST as i32))
}

/// The service's state, or `None` when it is not installed. Any user may ask.
pub fn state() -> Result<Option<ServiceState>> {
    let manager = ServiceManager::local_computer(None::<&str>, ServiceManagerAccess::CONNECT)?;
    match manager.open_service(NAME, ServiceAccess::QUERY_STATUS) {
        Ok(service) => Ok(Some(service.query_status()?.current_state)),
        Err(error) if missing(&error) => Ok(None),
        Err(error) => Err(error).context("Open the K3 Up service"),
    }
}

fn open(access: ServiceAccess) -> Result<Service> {
    let manager = ServiceManager::local_computer(None::<&str>, ServiceManagerAccess::CONNECT)?;
    match manager.open_service(NAME, access) {
        Ok(service) => Ok(service),
        Err(error) if missing(&error) => anyhow::bail!(
            "The K3 Up service is not installed. Run `k3up agent install` from an elevated terminal"
        ),
        Err(error) => Err(error).context("Open the K3 Up service"),
    }
}

/// Starts the service and waits until it runs. Any interactive user may do this.
pub fn start() -> Result<()> {
    start_and_wait(&open(ServiceAccess::START | ServiceAccess::QUERY_STATUS)?)
}

/// Stops the service, which stops its workloads first, and waits until it has exited. Any
/// interactive user may do this. It starts again at the next boot.
pub fn stop(timeout: Duration) -> Result<()> {
    wait_for_stop(
        &open(ServiceAccess::STOP | ServiceAccess::QUERY_STATUS)?,
        timeout,
    )
}

fn start_and_wait(service: &Service) -> Result<()> {
    let deadline = Instant::now() + Duration::from_secs(30);
    let mut start_sent = false;
    loop {
        match service.query_status()?.current_state {
            ServiceState::Running => return Ok(()),
            ServiceState::Stopped if !start_sent => {
                service.start::<&str>(&[])?;
                start_sent = true;
            }
            ServiceState::Stopped => anyhow::bail!(
                "The K3 Up service stopped right after starting. See the System event log"
            ),
            _ => {}
        }
        anyhow::ensure!(
            Instant::now() < deadline,
            "Timed out waiting for the K3 Up service to start"
        );
        std::thread::sleep(Duration::from_millis(200));
    }
}

fn wait_for_stop(service: &Service, timeout: Duration) -> Result<()> {
    let deadline = Instant::now() + timeout;
    let mut stop_sent = false;
    loop {
        match service.query_status()?.current_state {
            ServiceState::Stopped => return Ok(()),
            // A starting service cannot accept a stop request until it reports Running.
            ServiceState::Running if !stop_sent => {
                service.stop()?;
                stop_sent = true;
            }
            _ => {}
        }
        anyhow::ensure!(
            Instant::now() < deadline,
            "The K3 Up service is still stopping its workloads after {} seconds",
            timeout.as_secs()
        );
        std::thread::sleep(Duration::from_millis(200));
    }
}

/// Stops and removes the service. Program Files and ProgramData are left in place.
pub fn uninstall(timeout: Duration) -> Result<()> {
    let manager = ServiceManager::local_computer(None::<&str>, ServiceManagerAccess::CONNECT)?;
    let service = match manager.open_service(
        NAME,
        ServiceAccess::DELETE | ServiceAccess::QUERY_STATUS | ServiceAccess::STOP,
    ) {
        Ok(service) => service,
        Err(error) if missing(&error) => return Ok(()),
        Err(error) => {
            return Err(error).context("Run uninstallation from an elevated terminal");
        }
    };
    wait_for_stop(&service, timeout)?;
    service.delete()?;
    Ok(())
}
