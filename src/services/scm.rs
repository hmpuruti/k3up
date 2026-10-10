//! The Windows service manager calls that services mode needs.
use super::status::{Scm, Service, description, display_name, starts_at_boot};
use crate::model::Workload;
use anyhow::{Context, Result, bail};
use std::{
    ffi::OsString,
    path::{Path, PathBuf},
    ptr,
    time::{Duration, Instant},
};
use windows_service::{
    service::{
        ServiceAccess, ServiceAction, ServiceActionType, ServiceErrorControl,
        ServiceFailureActions, ServiceFailureResetPeriod, ServiceInfo, ServiceStartType,
        ServiceState, ServiceType,
    },
    service_manager::{ServiceManager, ServiceManagerAccess},
};
use windows_sys::Win32::{
    Foundation::{
        ERROR_INSUFFICIENT_BUFFER, ERROR_MORE_DATA, ERROR_SERVICE_ALREADY_RUNNING,
        ERROR_SERVICE_CANNOT_ACCEPT_CTRL, ERROR_SERVICE_DOES_NOT_EXIST,
        ERROR_SERVICE_MARKED_FOR_DELETE, ERROR_SERVICE_NOT_ACTIVE,
    },
    System::Services::{
        ChangeServiceConfigW, CloseServiceHandle, ENUM_SERVICE_STATUS_PROCESSW,
        EnumServicesStatusExW, OpenSCManagerW, QueryServiceConfig2W, SC_ENUM_PROCESS_INFO,
        SC_HANDLE, SC_MANAGER_CONNECT, SC_MANAGER_ENUMERATE_SERVICE, SERVICE_CONFIG,
        SERVICE_CONFIG_DELAYED_AUTO_START_INFO, SERVICE_CONFIG_DESCRIPTION,
        SERVICE_DELAYED_AUTO_START_INFO, SERVICE_DESCRIPTIONW, SERVICE_NO_CHANGE, SERVICE_RUNNING,
        SERVICE_START_PENDING, SERVICE_STATE_ALL, SERVICE_STOP_PENDING, SERVICE_STOPPED,
        SERVICE_WIN32,
    },
};

pub const START_TIMEOUT: Duration = Duration::from_secs(30);
const DELETE_TIMEOUT: Duration = Duration::from_secs(30);

/// How the service manager starts a workload's host.
pub struct Registration<'a> {
    pub service: &'a str,
    pub host: &'a Path,
    pub data: &'a Path,
}

fn os_error(error: &windows_service::Error) -> Option<u32> {
    match error {
        windows_service::Error::Winapi(error) => error.raw_os_error().map(|code| code as u32),
        _ => None,
    }
}

pub fn is_missing(error: &windows_service::Error) -> bool {
    os_error(error) == Some(ERROR_SERVICE_DOES_NOT_EXIST)
}

fn manager(access: ServiceManagerAccess) -> Result<ServiceManager> {
    ServiceManager::local_computer(None::<&str>, access).context("Open the service manager")
}

fn open(service: &str, access: ServiceAccess) -> Result<Option<windows_service::service::Service>> {
    match manager(ServiceManagerAccess::CONNECT)?.open_service(service, access) {
        Ok(handle) => Ok(Some(handle)),
        Err(error) if is_missing(&error) => Ok(None),
        Err(error) => Err(error).with_context(|| format!("Open service {service}")),
    }
}

fn require(service: &str, access: ServiceAccess) -> Result<windows_service::service::Service> {
    open(service, access)?.with_context(|| format!("Service {service} does not exist"))
}

pub fn exists(service: &str) -> Result<bool> {
    Ok(open(service, ServiceAccess::QUERY_STATUS)?.is_some())
}

fn scm_state(state: ServiceState) -> Scm {
    match state {
        ServiceState::Stopped => Scm::Stopped,
        ServiceState::StartPending => Scm::Starting,
        ServiceState::StopPending => Scm::Stopping,
        _ => Scm::Running,
    }
}

/// One service by name, or `None` when it does not exist. Any user may ask.
pub fn query(service: &str) -> Result<Option<Service>> {
    let Some(handle) = open(
        service,
        ServiceAccess::QUERY_STATUS | ServiceAccess::QUERY_CONFIG,
    )?
    else {
        return Ok(None);
    };
    let status = handle.query_status()?;
    let config = handle.query_config()?;
    Ok(Some(Service {
        name: service.into(),
        display_name: config.display_name.to_string_lossy().into_owned(),
        scm: scm_state(status.current_state),
        host_pid: status.process_id.filter(|pid| *pid != 0),
    }))
}

/// The exit code a stopped service reported, when it reported a service-specific one.
pub fn specific_exit_code(service: &str) -> Result<Option<u32>> {
    let Some(handle) = open(service, ServiceAccess::QUERY_STATUS)? else {
        return Ok(None);
    };
    Ok(match handle.query_status()?.exit_code {
        windows_service::service::ServiceExitCode::ServiceSpecific(code) => Some(code),
        windows_service::service::ServiceExitCode::Win32(_) => None,
    })
}

struct ManagerHandle(SC_HANDLE);

impl Drop for ManagerHandle {
    fn drop(&mut self) {
        // SAFETY: the handle came from OpenSCManagerW and is closed only here.
        unsafe {
            CloseServiceHandle(self.0);
        }
    }
}

/// Every service whose name starts with `prefix`, case-insensitively. Any user may ask.
pub fn list(prefix: &str) -> Result<Vec<Service>> {
    // SAFETY: OpenSCManagerW takes optional NUL-terminated strings; null means the local
    // database.
    let handle = unsafe {
        OpenSCManagerW(
            ptr::null(),
            ptr::null(),
            SC_MANAGER_CONNECT | SC_MANAGER_ENUMERATE_SERVICE,
        )
    };
    if handle.is_null() {
        return Err(std::io::Error::last_os_error()).context("Open the service manager");
    }
    let handle = ManagerHandle(handle);
    let wanted = prefix.to_lowercase();
    // u64 elements keep the returned structures aligned.
    let mut buffer = vec![0u64; 8 * 1024];
    let mut resume = 0u32;
    let mut found = vec![];
    loop {
        let mut needed = 0u32;
        let mut count = 0u32;
        // SAFETY: the buffer is writable for the size passed; the out-pointers are valid.
        let done = unsafe {
            EnumServicesStatusExW(
                handle.0,
                SC_ENUM_PROCESS_INFO,
                SERVICE_WIN32,
                SERVICE_STATE_ALL,
                buffer.as_mut_ptr().cast(),
                (buffer.len() * size_of::<u64>()) as u32,
                &mut needed,
                &mut count,
                &mut resume,
                ptr::null(),
            )
        } != 0;
        let error = std::io::Error::last_os_error();
        // SAFETY: the call filled `count` entries at the start of the buffer, and their
        // strings point into the same buffer.
        let entries = unsafe {
            std::slice::from_raw_parts(
                buffer.as_ptr().cast::<ENUM_SERVICE_STATUS_PROCESSW>(),
                count as usize,
            )
        };
        for entry in entries {
            // SAFETY: both names are NUL-terminated strings inside the buffer.
            let (name, display) = unsafe {
                (
                    wide_string(entry.lpServiceName),
                    wide_string(entry.lpDisplayName),
                )
            };
            if !name.to_lowercase().starts_with(&wanted) {
                continue;
            }
            let process = entry.ServiceStatusProcess;
            found.push(Service {
                name,
                display_name: display,
                scm: match process.dwCurrentState {
                    SERVICE_STOPPED => Scm::Stopped,
                    SERVICE_START_PENDING => Scm::Starting,
                    SERVICE_STOP_PENDING => Scm::Stopping,
                    SERVICE_RUNNING => Scm::Running,
                    _ => Scm::Running,
                },
                host_pid: Some(process.dwProcessId).filter(|pid| *pid != 0),
            });
        }
        if done {
            return Ok(found);
        }
        if error.raw_os_error() != Some(ERROR_MORE_DATA as i32) {
            return Err(error).context("List services");
        }
        if count == 0 {
            buffer = vec![0u64; (needed as usize).div_ceil(size_of::<u64>()) + 1];
        }
    }
}

/// # Safety
/// `text` must be null or point to a NUL-terminated UTF-16 string.
unsafe fn wide_string(text: *const u16) -> String {
    if text.is_null() {
        return String::new();
    }
    unsafe {
        let mut length = 0;
        while *text.add(length) != 0 {
            length += 1;
        }
        String::from_utf16_lossy(std::slice::from_raw_parts(text, length))
    }
}

/// The data directory as the host should receive it: without the `\\?\` prefix that
/// canonical paths carry, which some programs do not accept.
pub fn plain(path: &Path) -> PathBuf {
    let text = path.to_string_lossy();
    match text.strip_prefix(r"\\?\") {
        Some(rest) if rest.as_bytes().get(1) == Some(&b':') => PathBuf::from(rest),
        Some(rest) if rest.starts_with("UNC\\") => PathBuf::from(format!(r"\\{}", &rest[4..])),
        _ => path.to_path_buf(),
    }
}

fn info(workload: &Workload, at: &Registration) -> ServiceInfo {
    ServiceInfo {
        name: at.service.into(),
        display_name: display_name(workload).into(),
        service_type: ServiceType::OWN_PROCESS,
        start_type: if starts_at_boot(workload) {
            ServiceStartType::AutoStart
        } else {
            ServiceStartType::OnDemand
        },
        error_control: ServiceErrorControl::Normal,
        executable_path: at.host.to_path_buf(),
        launch_arguments: vec![
            "--data-dir".into(),
            plain(at.data).into_os_string(),
            "--workload".into(),
            OsString::from(&workload.name),
        ],
        dependencies: vec![],
        account_name: None,
        account_password: None,
    }
}

fn configure(handle: &windows_service::service::Service, workload: &Workload) -> Result<()> {
    handle.set_description(description(workload))?;
    handle.set_delayed_auto_start(starts_at_boot(workload))?;
    // These only act when the host itself crashes. A workload that gave up for good reports
    // a clean stop with an error code, which must not bring it back.
    handle.update_failure_actions(ServiceFailureActions {
        reset_period: ServiceFailureResetPeriod::After(Duration::from_secs(86_400)),
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
    handle.set_failure_actions_on_non_crash_failures(false)?;
    Ok(())
}

fn change_access() -> ServiceAccess {
    // Restart failure actions need START as well as CHANGE_CONFIG.
    ServiceAccess::QUERY_STATUS
        | ServiceAccess::QUERY_CONFIG
        | ServiceAccess::CHANGE_CONFIG
        | ServiceAccess::START
}

pub fn create(workload: &Workload, at: &Registration) -> Result<()> {
    let manager = manager(ServiceManagerAccess::CONNECT | ServiceManagerAccess::CREATE_SERVICE)?;
    let handle = manager
        .create_service(&info(workload, at), change_access() | ServiceAccess::DELETE)
        .with_context(|| format!("Create service {}", at.service))?;
    if let Err(error) = configure(&handle, workload) {
        let _ = handle.delete();
        return Err(error.context(format!("Configure service {}", at.service)));
    }
    Ok(())
}

/// Applies the whole definition. Also used for a running service whose labels changed: the
/// service manager accepts a new configuration while the service runs.
pub fn reconfigure(workload: &Workload, at: &Registration) -> Result<()> {
    let handle = require(at.service, change_access())?;
    handle
        .change_config(&info(workload, at))
        .with_context(|| format!("Change service {}", at.service))?;
    configure(&handle, workload).with_context(|| format!("Configure service {}", at.service))
}

/// Fails unless this process may create services.
pub fn check_create_access() -> Result<()> {
    manager(ServiceManagerAccess::CONNECT | ServiceManagerAccess::CREATE_SERVICE).map(drop)
}

/// What `create` and `reconfigure` set on a service, as the service manager holds it, so a
/// failed change can be undone. Failure actions are left out: K3 Up always sets the same ones.
pub struct Snapshot {
    start_type: u32,
    command_line: Vec<u16>,
    display_name: Vec<u16>,
    description: String,
    delayed: bool,
}

/// Opens the service with the rights a change needs, so a caller without them fails here,
/// before anything has changed.
pub fn snapshot(service: &str) -> Result<Snapshot> {
    let handle = require(service, change_access())?;
    let read = || -> Result<Snapshot> {
        let config = handle.query_config()?;
        let description = query_config2(&handle, SERVICE_CONFIG_DESCRIPTION, |buffer| {
            // SAFETY: for this level the buffer starts with a SERVICE_DESCRIPTIONW whose
            // string is null or points into the same buffer.
            unsafe { wide_string((*buffer.cast::<SERVICE_DESCRIPTIONW>()).lpDescription) }
        })?;
        let delayed = query_config2(&handle, SERVICE_CONFIG_DELAYED_AUTO_START_INFO, |buffer| {
            // SAFETY: for this level the buffer starts with a SERVICE_DELAYED_AUTO_START_INFO.
            unsafe { (*buffer.cast::<SERVICE_DELAYED_AUTO_START_INFO>()).fDelayedAutostart != 0 }
        })?;
        Ok(Snapshot {
            start_type: config.start_type.to_raw(),
            command_line: crate::win32::wide(config.executable_path.as_os_str()),
            display_name: crate::win32::wide(&config.display_name),
            description,
            delayed,
        })
    };
    read().with_context(|| format!("Read service {service}"))
}

/// Calls QueryServiceConfig2W for `level` and hands the filled buffer to `parse`.
fn query_config2<T>(
    handle: &windows_service::service::Service,
    level: SERVICE_CONFIG,
    parse: impl FnOnce(*const u8) -> T,
) -> Result<T> {
    // u64 elements keep the returned structure aligned.
    let mut buffer = vec![0u64; 1024];
    loop {
        let mut needed = 0u32;
        // SAFETY: the buffer is writable for the size passed; the out-pointer is valid.
        let read = unsafe {
            QueryServiceConfig2W(
                handle.raw_handle(),
                level,
                buffer.as_mut_ptr().cast(),
                (buffer.len() * size_of::<u64>()) as u32,
                &mut needed,
            )
        } != 0;
        if read {
            return Ok(parse(buffer.as_ptr().cast()));
        }
        let error = std::io::Error::last_os_error();
        if error.raw_os_error() != Some(ERROR_INSUFFICIENT_BUFFER as i32) {
            return Err(error.into());
        }
        buffer = vec![0u64; (needed as usize).div_ceil(size_of::<u64>())];
    }
}

/// Puts back the settings `snapshot` recorded.
pub fn restore(service: &str, snapshot: &Snapshot) -> Result<()> {
    let handle = require(service, change_access())?;
    // SAFETY: the strings are NUL-terminated and outlive the call; null pointers and
    // SERVICE_NO_CHANGE leave those settings as they are.
    let changed = unsafe {
        ChangeServiceConfigW(
            handle.raw_handle(),
            SERVICE_NO_CHANGE,
            snapshot.start_type,
            SERVICE_NO_CHANGE,
            snapshot.command_line.as_ptr(),
            ptr::null(),
            ptr::null_mut(),
            ptr::null(),
            ptr::null(),
            ptr::null(),
            snapshot.display_name.as_ptr(),
        )
    } != 0;
    let restored = if changed {
        handle
            .set_description(&snapshot.description)
            .and_then(|()| handle.set_delayed_auto_start(snapshot.delayed))
            .map_err(Into::into)
    } else {
        Err(anyhow::Error::from(std::io::Error::last_os_error()))
    };
    restored.with_context(|| format!("Restore service {service}"))
}

pub fn start(service: &str) -> Result<()> {
    let handle = require(service, ServiceAccess::START | ServiceAccess::QUERY_STATUS)?;
    match handle.start::<&str>(&[]) {
        Ok(()) => Ok(()),
        Err(error) if os_error(&error) == Some(ERROR_SERVICE_ALREADY_RUNNING) => Ok(()),
        Err(error) => Err(error).with_context(|| format!("Start service {service}")),
    }
}

/// Polls until the service's state satisfies `done`, returning that state.
pub fn wait(
    service: &str,
    timeout: Duration,
    mut done: impl FnMut(Scm) -> bool,
) -> Result<Option<Scm>> {
    let deadline = Instant::now() + timeout;
    loop {
        let state = query(service)?.map(|found| found.scm);
        if state.is_none_or(&mut done) {
            return Ok(state);
        }
        if Instant::now() >= deadline {
            return Ok(state);
        }
        std::thread::sleep(Duration::from_millis(200));
    }
}

/// Stops the service and waits until it has. A starting service is stopped once it runs.
pub fn stop(service: &str, timeout: Duration) -> Result<()> {
    let Some(handle) = open(service, ServiceAccess::STOP | ServiceAccess::QUERY_STATUS)? else {
        return Ok(());
    };
    let deadline = Instant::now() + timeout;
    let mut sent = false;
    loop {
        let state = handle.query_status()?.current_state;
        if state == ServiceState::Stopped {
            return Ok(());
        }
        if !sent && state == ServiceState::Running {
            match handle.stop() {
                Ok(_) => sent = true,
                Err(error)
                    if matches!(
                        os_error(&error),
                        Some(ERROR_SERVICE_NOT_ACTIVE | ERROR_SERVICE_CANNOT_ACCEPT_CTRL)
                    ) => {}
                Err(error) => {
                    return Err(error).with_context(|| format!("Stop service {service}"));
                }
            }
        }
        if Instant::now() >= deadline {
            bail!(
                "Service {service} did not stop within {} seconds",
                timeout.as_secs()
            );
        }
        std::thread::sleep(Duration::from_millis(200));
    }
}

/// Deletes a stopped service and waits until the name is free again.
pub fn delete(service: &str) -> Result<()> {
    let Some(handle) = open(service, ServiceAccess::DELETE | ServiceAccess::QUERY_STATUS)? else {
        return Ok(());
    };
    match handle.delete() {
        Ok(()) => {}
        Err(error) if os_error(&error) == Some(ERROR_SERVICE_MARKED_FOR_DELETE) => {}
        Err(error) => return Err(error).with_context(|| format!("Delete service {service}")),
    }
    drop(handle);
    let deadline = Instant::now() + DELETE_TIMEOUT;
    while exists(service)? {
        if Instant::now() >= deadline {
            bail!(
                "Service {service} is marked for deletion but still exists. Close the Services window and try again"
            );
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    Ok(())
}
