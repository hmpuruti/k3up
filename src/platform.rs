use anyhow::{Context, Result};
use std::path::{Path, PathBuf};

pub fn default_data_dir() -> PathBuf {
    if let Some(path) = std::env::var_os("K3UP_DATA_DIR") {
        return path.into();
    }
    #[cfg(windows)]
    {
        PathBuf::from(std::env::var_os("LOCALAPPDATA").unwrap_or_else(|| ".".into())).join("K3 Up")
    }
    #[cfg(not(windows))]
    {
        PathBuf::from(std::env::var_os("HOME").unwrap_or_else(|| ".".into()))
            .join(".local/share/k3up")
    }
}

pub fn prepare_dir(path: &Path) -> Result<PathBuf> {
    std::fs::create_dir_all(path).context("Create data directory")?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        let metadata = std::fs::metadata(path)?;
        // SAFETY: geteuid has no preconditions.
        anyhow::ensure!(
            metadata.uid() == unsafe { libc::geteuid() },
            "Data directory must belong to the current user"
        );
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))?;
    }
    std::fs::canonicalize(path).context("Resolve data directory")
}

pub fn endpoint(path: &Path) -> Result<String> {
    #[cfg(unix)]
    {
        // sockaddr_un.sun_path capacity, including the terminating NUL.
        const CAPACITY: usize = if cfg!(target_os = "linux") { 108 } else { 104 };
        let socket = path.join("agent.sock").to_string_lossy().into_owned();
        anyhow::ensure!(
            socket.len() < CAPACITY,
            "Data directory path is too long for a local socket ({} of {} bytes allowed). Use a shorter --data-dir",
            socket.len(),
            CAPACITY - 1
        );
        Ok(socket)
    }
    #[cfg(windows)]
    {
        // FNV-1a, stable between processes and releases; access is checked separately.
        let text = path.to_string_lossy().to_lowercase();
        let hash = text.bytes().fold(0xcbf29ce484222325u64, |h, b| {
            (h ^ b as u64).wrapping_mul(0x100000001b3)
        });
        Ok(format!(r"\\.\pipe\k3up-{hash:016x}"))
    }
}

#[cfg(unix)]
mod process {
    use super::*;
    use crate::model::Workload;
    use std::{
        fs::File,
        os::unix::process::CommandExt,
        process::{Child, Command, Stdio},
    };

    pub struct ManagedProcess {
        child: Child,
        pgid: i32,
    }
    impl ManagedProcess {
        pub fn spawn(spec: &Workload, log: File) -> Result<Self> {
            let mut command = Command::new(&spec.executable);
            command
                .args(&spec.args)
                .current_dir(&spec.working_directory)
                .envs(&spec.environment)
                .stdin(Stdio::null())
                .stdout(log.try_clone()?)
                .stderr(log)
                .process_group(0);
            let child = command
                .spawn()
                .with_context(|| format!("Launch {}", spec.executable))?;
            let pgid = child.id() as i32;
            Ok(Self { child, pgid })
        }
        pub fn id(&self) -> u32 {
            self.child.id()
        }
        pub fn poll(&mut self) -> Result<Option<i32>> {
            use std::os::unix::process::ExitStatusExt;
            Ok(self.child.try_wait()?.map(|status| {
                status
                    .code()
                    .unwrap_or_else(|| 128 + status.signal().unwrap_or(0))
            }))
        }
        pub fn graceful_stop(&mut self) {
            // SAFETY: pgid is the positive ID of the isolated process group we created.
            unsafe {
                libc::kill(-self.pgid, libc::SIGTERM);
            }
        }
        pub fn force_stop(&mut self) {
            // SAFETY: this targets only the workload's isolated process group.
            unsafe {
                libc::kill(-self.pgid, libc::SIGKILL);
            }
            let _ = self.child.wait();
        }
    }
    impl Drop for ManagedProcess {
        fn drop(&mut self) {
            self.force_stop();
        }
    }
}

#[cfg(windows)]
mod process {
    use super::*;
    use crate::model::Workload;
    use std::{
        collections::BTreeMap,
        ffi::OsStr,
        fs::File,
        os::windows::{ffi::OsStrExt, io::AsRawHandle},
        ptr,
    };
    use windows_sys::Win32::{
        Foundation::*,
        System::{JobObjects::*, Threading::*},
    };

    pub struct ManagedProcess {
        process: HANDLE,
        job: HANDLE,
        pid: u32,
    }
    // SAFETY: ownership of both handles is exclusive; the engine uses them on one task.
    unsafe impl Send for ManagedProcess {}
    fn wide(value: &OsStr) -> Vec<u16> {
        value.encode_wide().chain(Some(0)).collect()
    }
    fn quote(value: &str) -> String {
        let mut output = String::from("\"");
        let mut slashes = 0;
        for c in value.chars() {
            if c == '\\' {
                slashes += 1;
                continue;
            }
            if c == '"' {
                output.push_str(&"\\".repeat(slashes * 2 + 1));
            } else {
                output.push_str(&"\\".repeat(slashes));
            }
            output.push(c);
            slashes = 0;
        }
        output.push_str(&"\\".repeat(slashes * 2));
        output.push('"');
        output
    }
    impl ManagedProcess {
        pub fn spawn(spec: &Workload, log: File) -> Result<Self> {
            let executable = wide(OsStr::new(&spec.executable));
            let cwd = wide(OsStr::new(&spec.working_directory));
            let command = std::iter::once(spec.executable.as_str())
                .chain(spec.args.iter().map(String::as_str))
                .map(quote)
                .collect::<Vec<_>>()
                .join(" ");
            let mut command = wide(OsStr::new(&command));
            // Windows requires the block sorted case-insensitively; uppercase keys also dedupe.
            let mut environment = BTreeMap::new();
            for (key, value) in std::env::vars_os() {
                environment.insert(key.to_string_lossy().to_uppercase(), value);
            }
            for (key, value) in &spec.environment {
                environment.insert(key.to_uppercase(), value.into());
            }
            let mut environment: Vec<u16> = environment
                .iter()
                .flat_map(|(key, value)| {
                    let mut entry = std::ffi::OsString::from(format!("{key}="));
                    entry.push(value);
                    wide(&entry)
                })
                .collect();
            environment.push(0);
            let input = File::open("NUL")?;
            // SAFETY: pointers reference live, terminated UTF-16 buffers and initialized Win32 structures.
            unsafe {
                let job = CreateJobObjectW(ptr::null(), ptr::null());
                if job.is_null() {
                    return Err(std::io::Error::last_os_error().into());
                }
                let mut limits: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = std::mem::zeroed();
                limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
                if SetInformationJobObject(
                    job,
                    JobObjectExtendedLimitInformation,
                    &limits as *const _ as _,
                    std::mem::size_of_val(&limits) as u32,
                ) == 0
                {
                    let error = std::io::Error::last_os_error();
                    CloseHandle(job);
                    return Err(error.into());
                }
                let stdout = log.as_raw_handle() as HANDLE;
                let stdin = input.as_raw_handle() as HANDLE;
                if SetHandleInformation(stdout, HANDLE_FLAG_INHERIT, HANDLE_FLAG_INHERIT) == 0
                    || SetHandleInformation(stdin, HANDLE_FLAG_INHERIT, HANDLE_FLAG_INHERIT) == 0
                {
                    let error = std::io::Error::last_os_error();
                    CloseHandle(job);
                    return Err(error.into());
                }
                let mut startup: STARTUPINFOW = std::mem::zeroed();
                startup.cb = std::mem::size_of_val(&startup) as u32;
                startup.dwFlags = STARTF_USESTDHANDLES;
                startup.hStdInput = stdin;
                startup.hStdOutput = stdout;
                startup.hStdError = stdout;
                let mut info: PROCESS_INFORMATION = std::mem::zeroed();
                let created = CreateProcessW(
                    executable.as_ptr(),
                    command.as_mut_ptr(),
                    ptr::null(),
                    ptr::null(),
                    1,
                    CREATE_SUSPENDED | CREATE_NO_WINDOW | CREATE_UNICODE_ENVIRONMENT,
                    environment.as_ptr() as _,
                    cwd.as_ptr(),
                    &startup,
                    &mut info,
                );
                SetHandleInformation(stdout, HANDLE_FLAG_INHERIT, 0);
                SetHandleInformation(stdin, HANDLE_FLAG_INHERIT, 0);
                if created == 0 {
                    let error = std::io::Error::last_os_error();
                    CloseHandle(job);
                    return Err(error.into());
                }
                // Assign before execution, so a fast child cannot escape before job membership.
                if AssignProcessToJobObject(job, info.hProcess) == 0 {
                    let error = std::io::Error::last_os_error();
                    TerminateProcess(info.hProcess, 1);
                    CloseHandle(info.hThread);
                    CloseHandle(info.hProcess);
                    CloseHandle(job);
                    return Err(error.into());
                }
                if ResumeThread(info.hThread) == u32::MAX {
                    let error = std::io::Error::last_os_error();
                    TerminateJobObject(job, 1);
                    CloseHandle(info.hThread);
                    CloseHandle(info.hProcess);
                    CloseHandle(job);
                    return Err(error.into());
                }
                CloseHandle(info.hThread);
                Ok(Self {
                    process: info.hProcess,
                    job,
                    pid: info.dwProcessId,
                })
            }
        }
        pub fn id(&self) -> u32 {
            self.pid
        }
        pub fn poll(&mut self) -> Result<Option<i32>> {
            // SAFETY: process is a live handle owned by this instance.
            unsafe {
                if WaitForSingleObject(self.process, 0) == WAIT_TIMEOUT {
                    return Ok(None);
                }
                let mut code = 0;
                if GetExitCodeProcess(self.process, &mut code) == 0 {
                    return Err(std::io::Error::last_os_error().into());
                }
                Ok(Some(code as i32))
            }
        }
        pub fn graceful_stop(&mut self) { /* Headless arbitrary Windows processes have no universal graceful-stop protocol. */
        }
        pub fn force_stop(&mut self) {
            unsafe {
                TerminateJobObject(self.job, 1);
                WaitForSingleObject(self.process, 5000);
            }
        }
    }
    impl Drop for ManagedProcess {
        fn drop(&mut self) {
            self.force_stop();
            unsafe {
                CloseHandle(self.process);
                CloseHandle(self.job);
            }
        }
    }
}

pub use process::ManagedProcess;

/// Identifies a spawned workload well enough for a later agent to find it after a crash,
/// without trusting a PID that may have been reused. `None` where that is not possible or
/// not needed: Windows Job Objects already end workloads when the agent dies.
pub fn process_ticket(pid: u32) -> Option<String> {
    start_time(pid).map(|start| format!("{pid} {start}"))
}

/// Stops the process group a crashed agent left behind, if the ticket still names the same
/// process. Returns its PID when something was stopped.
pub fn stop_leftover(ticket: &str, timeout: std::time::Duration) -> Option<u32> {
    let (pid, start) = ticket.trim().split_once(' ')?;
    let pid: u32 = pid.parse().ok()?;
    if pid <= 1 || start_time(pid)?.to_string() != start {
        return None;
    }
    #[cfg(unix)]
    {
        let group = -(pid as i32);
        // SAFETY: the start time proves this group is the workload recorded in the ticket.
        unsafe {
            libc::kill(group, libc::SIGTERM);
        }
        let deadline = std::time::Instant::now() + timeout;
        // SAFETY: signal 0 only checks whether the group still exists.
        while unsafe { libc::kill(group, 0) } == 0 && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        // SAFETY: same verified group as above.
        unsafe {
            libc::kill(group, libc::SIGKILL);
        }
        Some(pid)
    }
    #[cfg(not(unix))]
    {
        let _ = timeout;
        None
    }
}

/// Direct children of a process, from the kernel. `None` where there is no cheap way to ask,
/// so callers fall back to listing every process.
#[cfg(target_os = "macos")]
pub fn children(pid: u32) -> Option<Vec<u32>> {
    let mut capacity = 64;
    loop {
        let mut buffer = vec![0 as libc::pid_t; capacity];
        // SAFETY: the buffer holds `capacity` pids and its size in bytes is passed alongside.
        let count = unsafe {
            libc::proc_listchildpids(
                pid as libc::pid_t,
                buffer.as_mut_ptr().cast(),
                (capacity * size_of::<libc::pid_t>()) as libc::c_int,
            )
        };
        if count < 0 {
            return Some(vec![]);
        }
        let count = count as usize;
        // A full buffer may have been truncated; ask again with more room.
        if count < capacity {
            buffer.truncate(count);
            return Some(buffer.into_iter().map(|child| child as u32).collect());
        }
        capacity *= 4;
    }
}

#[cfg(target_os = "linux")]
pub fn children(pid: u32) -> Option<Vec<u32>> {
    // The children files need CONFIG_PROC_CHILDREN; without it there is nothing to read.
    static SUPPORTED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    let supported = *SUPPORTED.get_or_init(|| {
        Path::new(&format!("/proc/self/task/{}/children", std::process::id())).exists()
    });
    if !supported {
        return None;
    }
    let Ok(tasks) = std::fs::read_dir(format!("/proc/{pid}/task")) else {
        return Some(vec![]);
    };
    let mut found = vec![];
    for task in tasks.flatten() {
        if let Ok(listed) = std::fs::read_to_string(task.path().join("children")) {
            found.extend(
                listed
                    .split_whitespace()
                    .filter_map(|child| child.parse::<u32>().ok()),
            );
        }
    }
    Some(found)
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
pub fn children(_: u32) -> Option<Vec<u32>> {
    None
}

#[cfg(target_os = "macos")]
fn start_time(pid: u32) -> Option<u64> {
    // SAFETY: proc_bsdinfo is plain data, and the buffer size passed matches it.
    unsafe {
        let mut info: libc::proc_bsdinfo = std::mem::zeroed();
        let size = size_of::<libc::proc_bsdinfo>() as libc::c_int;
        let read = libc::proc_pidinfo(
            pid as libc::c_int,
            libc::PROC_PIDTBSDINFO,
            0,
            (&mut info as *mut libc::proc_bsdinfo).cast(),
            size,
        );
        (read == size).then(|| info.pbi_start_tvsec * 1_000_000 + info.pbi_start_tvusec)
    }
}

#[cfg(target_os = "linux")]
fn start_time(pid: u32) -> Option<u64> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    // The command name may contain spaces and parentheses; fields resume after the last ')'.
    let fields = stat.rsplit_once(')')?.1;
    fields.split_whitespace().nth(19)?.parse().ok()
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn start_time(_: u32) -> Option<u64> {
    None
}

#[cfg(all(test, any(target_os = "macos", target_os = "linux")))]
mod tests {
    use super::*;

    #[test]
    fn children_lists_a_spawned_child() {
        let mut child = std::process::Command::new("sleep")
            .arg("5")
            .spawn()
            .unwrap();
        let found = children(std::process::id()).unwrap();
        let _ = child.kill();
        let _ = child.wait();
        assert!(found.contains(&child.id()), "{found:?}");
    }

    #[test]
    fn tickets_match_only_the_same_process() {
        let ticket = process_ticket(std::process::id()).unwrap();
        assert_eq!(process_ticket(std::process::id()).unwrap(), ticket);
        let (pid, _) = ticket.split_once(' ').unwrap();
        assert_eq!(
            stop_leftover(&format!("{pid} 1"), std::time::Duration::ZERO),
            None
        );
    }
}
