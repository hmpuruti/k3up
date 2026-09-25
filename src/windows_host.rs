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
static DATA: OnceLock<PathBuf> = OnceLock::new();
define_windows_service!(ffi_main, service_main);

pub fn run(data: PathBuf) -> Result<()> {
    DATA.set(data)
        .map_err(|_| anyhow::anyhow!("Service already initialized"))?;
    service_dispatcher::start(NAME, ffi_main)?;
    Ok(())
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
            runtime.block_on(crate::server::run(
                DATA.get().unwrap().clone(),
                async move {
                    let _ = rx.changed().await;
                    // Stopping workloads can take each one's full stop timeout. Advancing the
                    // checkpoint tells the Service Control Manager the agent is still progressing.
                    tokio::spawn(async move {
                        for checkpoint in 1.. {
                            report(handle, ServiceState::StopPending, checkpoint, 0);
                            tokio::time::sleep(Duration::from_secs(5)).await;
                        }
                    });
                },
            ))
        });
    report(
        handle,
        ServiceState::Stopped,
        0,
        if result.is_ok() { 0 } else { 1 },
    );
}

pub fn install(_data: &Path) -> Result<()> {
    // A machine service must never execute binaries or configuration from a user-writable folder.
    let data = PathBuf::from(std::env::var_os("ProgramData").context("ProgramData is not set")?)
        .join("K3 Up");
    let install_dir =
        PathBuf::from(std::env::var_os("ProgramFiles").context("ProgramFiles is not set")?)
            .join("K3 Up");
    let source = std::env::current_exe()?.with_file_name("k3up-agent.exe");
    anyhow::ensure!(source.exists(), "Place k3up-agent.exe beside k3up.exe");
    anyhow::ensure!(
        !data.exists() && !install_dir.exists(),
        "Fresh installation requires unused ProgramData/K3 Up and ProgramFiles/K3 Up directories. Existing installations require a reviewed upgrade"
    );
    let manager = ServiceManager::local_computer(
        None::<&str>,
        ServiceManagerAccess::CONNECT | ServiceManagerAccess::CREATE_SERVICE,
    )
    .context("Run installation from an elevated terminal")?;
    let mut created = vec![];
    let mut service = None;
    let result = install_into(
        &manager,
        &source,
        &data,
        &install_dir,
        &mut created,
        &mut service,
    );
    if let Err(error) = result {
        // Every path removed here was created by this attempt, so a retry starts clean.
        if let Some(service) = service {
            let _ = service.delete();
        }
        for directory in created.iter().rev() {
            let _ = std::fs::remove_dir_all(directory);
        }
        return Err(error.context("Installation rolled back"));
    }
    Ok(())
}

fn install_into(
    manager: &ServiceManager,
    source: &Path,
    data: &Path,
    install_dir: &Path,
    created: &mut Vec<PathBuf>,
    service: &mut Option<Service>,
) -> Result<()> {
    for directory in [data, install_dir] {
        crate::win32::create_protected_dir(directory)?;
        created.push(directory.to_path_buf());
    }
    let executable = install_dir.join("k3up-agent.exe");
    std::fs::copy(source, &executable)?;
    let registered = service.insert(manager.create_service(
        &ServiceInfo {
            name: NAME.into(),
            display_name: "K3 Up agent".into(),
            service_type: ServiceType::OWN_PROCESS,
            start_type: ServiceStartType::AutoStart,
            error_control: ServiceErrorControl::Normal,
            executable_path: executable,
            launch_arguments: vec![
                "--windows-service".into(),
                "--data-dir".into(),
                data.as_os_str().to_owned(),
            ],
            dependencies: vec![],
            account_name: None,
            account_password: None,
        },
        // Restart failure actions require SERVICE_START in addition to SERVICE_CHANGE_CONFIG.
        ServiceAccess::CHANGE_CONFIG | ServiceAccess::START | ServiceAccess::DELETE,
    )?);
    registered.set_description("Runs K3 Up workloads independently of the desktop app")?;
    registered.update_failure_actions(ServiceFailureActions {
        reset_period: ServiceFailureResetPeriod::After(Duration::from_secs(3600)),
        reboot_msg: None,
        command: None,
        actions: Some(vec![ServiceAction {
            action_type: ServiceActionType::Restart,
            delay: Duration::from_secs(5),
        }]),
    })?;
    Ok(())
}

pub fn uninstall() -> Result<()> {
    let manager = ServiceManager::local_computer(None::<&str>, ServiceManagerAccess::CONNECT)
        .context("Run uninstallation from an elevated terminal")?;
    let service = manager.open_service(
        NAME,
        ServiceAccess::DELETE | ServiceAccess::QUERY_STATUS | ServiceAccess::STOP,
    )?;
    let deadline = Instant::now() + Duration::from_secs(600);
    let mut stop_sent = false;
    loop {
        match service.query_status()?.current_state {
            ServiceState::Stopped => break,
            // A starting service cannot accept a stop request until it reports Running.
            ServiceState::Running if !stop_sent => {
                service.stop()?;
                stop_sent = true;
            }
            _ => {}
        }
        anyhow::ensure!(
            Instant::now() < deadline,
            "Timed out waiting for the Windows agent to stop"
        );
        std::thread::sleep(Duration::from_millis(500));
    }
    service.delete()?;
    Ok(())
}
