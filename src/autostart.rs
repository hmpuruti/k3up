use crate::platform;
use anyhow::{Context, Result};
use std::{
    fs::OpenOptions,
    path::PathBuf,
    process::{Command, Stdio},
};

#[cfg(target_os = "macos")]
const LABEL: &str = "com.k3.up.agent";
#[cfg(target_os = "linux")]
const UNIT: &str = "k3up-agent.service";
#[cfg(windows)]
const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
#[cfg(windows)]
const RUN_VALUE: &str = "K3 Up";
const OPT_OUT: &str = "login-item-disabled";

/// The agent executable shipped beside the running program, if there is one. Inside the macOS
/// app it is named "K3 Up Agent", because macOS shows that file name for login items.
pub fn bundled_agent() -> Option<PathBuf> {
    let names: &[&str] = if cfg!(windows) {
        &["k3up-agent.exe"]
    } else if cfg!(target_os = "macos") {
        &["K3 Up Agent", "k3up-agent"]
    } else {
        &["k3up-agent"]
    };
    let program = std::env::current_exe().ok()?;
    names
        .iter()
        .map(|name| program.with_file_name(name))
        .find(|path| path.is_file())
}

/// Starts the agent for the current user at login and on demand.
///
/// macOS uses a launchd LaunchAgent and Linux a systemd user service; both restart the agent
/// after a crash. Windows has no per-user supervisor, so its Run entry launches the calling
/// program with `--start-agent`, which must start the agent and exit.
#[derive(Clone, Debug)]
pub struct LoginAgent {
    agent: PathBuf,
    data: PathBuf,
}

impl LoginAgent {
    pub fn new(agent: PathBuf, data: PathBuf) -> Self {
        Self { agent, data }
    }

    pub fn data_dir(&self) -> &std::path::Path {
        &self.data
    }

    /// True once the user has turned the login item off; automatic setup must respect that.
    pub fn opted_out(&self) -> bool {
        self.data.join(OPT_OUT).exists()
    }

    /// Creates or refreshes the login item. It takes effect at the next login; use `start`
    /// to run the agent now.
    pub fn register(&self) -> Result<()> {
        platform::prepare_dir(&self.data)?;
        match std::fs::remove_file(self.data.join(OPT_OUT)) {
            Err(error) if error.kind() != std::io::ErrorKind::NotFound => {
                return Err(error).context("Clear login item preference");
            }
            _ => {}
        }
        self.install()
    }

    /// Removes the login item without stopping a running agent, so workloads keep running.
    pub fn unregister(&self) -> Result<()> {
        platform::prepare_dir(&self.data)?;
        std::fs::write(self.data.join(OPT_OUT), b"").context("Save login item preference")?;
        self.uninstall()
    }

    /// Whether an agent holds this data directory's lock. A busy agent can time out a client
    /// and look offline, so this is the check that decides whether to start another.
    pub fn is_running(&self) -> bool {
        use fs2::FileExt;
        let Ok(lock) = OpenOptions::new()
            .read(true)
            .write(true)
            .open(self.data.join("agent.lock"))
        else {
            return false;
        };
        let held = lock.try_lock_exclusive().is_err();
        if !held {
            let _ = FileExt::unlock(&lock);
        }
        held
    }

    /// Starts the agent now, through the login item's supervisor when one is registered.
    /// Does nothing if an agent is already running.
    pub fn start(&self) -> Result<()> {
        if self.is_running() {
            return Ok(());
        }
        if self.is_registered() && self.start_supervised().is_ok() {
            return Ok(());
        }
        self.spawn()
    }

    fn spawn(&self) -> Result<()> {
        let data = platform::prepare_dir(&self.data)?;
        let log = OpenOptions::new()
            .create(true)
            .append(true)
            .open(data.join("agent.log"))
            .context("Open agent log")?;
        let mut command = Command::new(&self.agent);
        command
            .arg("--data-dir")
            .arg(&data)
            .stdin(Stdio::null())
            .stdout(log.try_clone()?)
            .stderr(log);
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            // A new session keeps the agent alive when the launching terminal or app exits.
            // SAFETY: setsid is async-signal-safe and touches no parent state.
            unsafe {
                command.pre_exec(|| {
                    if libc::setsid() == -1 {
                        return Err(std::io::Error::last_os_error());
                    }
                    Ok(())
                });
            }
        }
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            use windows_sys::Win32::System::Threading::{
                CREATE_NEW_PROCESS_GROUP, CREATE_NO_WINDOW,
            };
            command.creation_flags(CREATE_NO_WINDOW | CREATE_NEW_PROCESS_GROUP);
        }
        let mut child = command
            .spawn()
            .with_context(|| format!("Start {}", self.agent.display()))?;
        std::thread::spawn(move || child.wait());
        Ok(())
    }
}

#[cfg(target_os = "macos")]
impl LoginAgent {
    fn plist_path() -> Result<PathBuf> {
        Ok(home()?
            .join("Library/LaunchAgents")
            .join(format!("{LABEL}.plist")))
    }

    pub fn is_registered(&self) -> bool {
        Self::plist_path().is_ok_and(|path| path.exists())
    }

    fn install(&self) -> Result<()> {
        // Gatekeeper runs a quarantined app from a temporary copy that disappears later.
        anyhow::ensure!(
            !self.agent.to_string_lossy().contains("/AppTranslocation/"),
            "Move K3 Up to the Applications folder first"
        );
        let path = Self::plist_path()?;
        let data = platform::prepare_dir(&self.data)?;
        let plist = launchd_plist(&self.agent, &data, &data.join("agent.log"));
        if std::fs::read_to_string(&path).ok().as_deref() != Some(plist.as_str()) {
            std::fs::create_dir_all(path.parent().context("LaunchAgents directory")?)?;
            std::fs::write(&path, plist).context("Write launchd agent")?;
        }
        Ok(())
    }

    fn uninstall(&self) -> Result<()> {
        remove_if_present(&Self::plist_path()?)
    }

    fn start_supervised(&self) -> Result<()> {
        // SAFETY: getuid has no preconditions.
        let domain = format!("gui/{}", unsafe { libc::getuid() });
        // Only called while the agent is unreachable, so reloading cannot interrupt workloads,
        // and it picks up a plist rewritten since login, such as after the app moved.
        let _ = run("launchctl", &["bootout", &format!("{domain}/{LABEL}")]);
        let path = Self::plist_path()?;
        run(
            "launchctl",
            &["bootstrap", &domain, &path.to_string_lossy()],
        )
    }
}

#[cfg(target_os = "linux")]
impl LoginAgent {
    fn unit_path() -> Result<PathBuf> {
        let config = match std::env::var_os("XDG_CONFIG_HOME") {
            Some(path) if !path.is_empty() => PathBuf::from(path),
            _ => home()?.join(".config"),
        };
        Ok(config.join("systemd/user").join(UNIT))
    }

    pub fn is_registered(&self) -> bool {
        Self::unit_path().is_ok_and(|path| path.exists())
    }

    fn install(&self) -> Result<()> {
        let path = Self::unit_path()?;
        let data = platform::prepare_dir(&self.data)?;
        let unit = systemd_unit(&self.agent, &data);
        if std::fs::read_to_string(&path).ok().as_deref() != Some(unit.as_str()) {
            std::fs::create_dir_all(path.parent().context("systemd user directory")?)?;
            std::fs::write(&path, unit).context("Write systemd user service")?;
        }
        run("systemctl", &["--user", "daemon-reload"])?;
        run("systemctl", &["--user", "enable", UNIT])
    }

    fn uninstall(&self) -> Result<()> {
        let _ = run("systemctl", &["--user", "disable", UNIT]);
        remove_if_present(&Self::unit_path()?)?;
        let _ = run("systemctl", &["--user", "daemon-reload"]);
        Ok(())
    }

    fn start_supervised(&self) -> Result<()> {
        run("systemctl", &["--user", "start", UNIT])
    }
}

#[cfg(windows)]
impl LoginAgent {
    fn run_entry(&self) -> Result<String> {
        let launcher = std::env::current_exe()?;
        // A trailing backslash would escape the closing quote.
        let data = self.data.to_string_lossy();
        Ok(format!(
            "\"{}\" --start-agent --data-dir \"{}\"",
            launcher.display(),
            data.trim_end_matches('\\')
        ))
    }

    pub fn is_registered(&self) -> bool {
        use windows_sys::Win32::{
            Foundation::ERROR_SUCCESS,
            System::Registry::{HKEY_CURRENT_USER, RRF_RT_REG_SZ, RegGetValueW},
        };
        let key = crate::win32::wide(RUN_KEY.as_ref());
        let value = crate::win32::wide(RUN_VALUE.as_ref());
        // SAFETY: NUL-terminated names; a null data pointer only queries existence.
        unsafe {
            RegGetValueW(
                HKEY_CURRENT_USER,
                key.as_ptr(),
                value.as_ptr(),
                RRF_RT_REG_SZ,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            ) == ERROR_SUCCESS
        }
    }

    fn install(&self) -> Result<()> {
        use windows_sys::Win32::{
            Foundation::ERROR_SUCCESS,
            System::Registry::{HKEY_CURRENT_USER, REG_SZ, RegSetKeyValueW},
        };
        let key = crate::win32::wide(RUN_KEY.as_ref());
        let value = crate::win32::wide(RUN_VALUE.as_ref());
        let data = crate::win32::wide(self.run_entry()?.as_ref());
        // SAFETY: all buffers are NUL-terminated and outlive the call.
        let status = unsafe {
            RegSetKeyValueW(
                HKEY_CURRENT_USER,
                key.as_ptr(),
                value.as_ptr(),
                REG_SZ,
                data.as_ptr().cast(),
                (data.len() * 2) as u32,
            )
        };
        if status != ERROR_SUCCESS {
            return Err(std::io::Error::from_raw_os_error(status as i32))
                .context("Write login entry");
        }
        Ok(())
    }

    fn uninstall(&self) -> Result<()> {
        use windows_sys::Win32::{
            Foundation::{ERROR_FILE_NOT_FOUND, ERROR_SUCCESS},
            System::Registry::{HKEY_CURRENT_USER, RegDeleteKeyValueW},
        };
        let key = crate::win32::wide(RUN_KEY.as_ref());
        let value = crate::win32::wide(RUN_VALUE.as_ref());
        // SAFETY: NUL-terminated names that outlive the call.
        let status = unsafe { RegDeleteKeyValueW(HKEY_CURRENT_USER, key.as_ptr(), value.as_ptr()) };
        if status != ERROR_SUCCESS && status != ERROR_FILE_NOT_FOUND {
            return Err(std::io::Error::from_raw_os_error(status as i32))
                .context("Remove login entry");
        }
        Ok(())
    }

    fn start_supervised(&self) -> Result<()> {
        anyhow::bail!("Windows has no per-user agent supervisor")
    }
}

#[cfg(not(any(target_os = "macos", target_os = "linux", windows)))]
impl LoginAgent {
    pub fn is_registered(&self) -> bool {
        false
    }

    fn install(&self) -> Result<()> {
        anyhow::bail!("Starting the agent at login is not supported on this system")
    }

    fn uninstall(&self) -> Result<()> {
        Ok(())
    }

    fn start_supervised(&self) -> Result<()> {
        anyhow::bail!("No agent supervisor on this system")
    }
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn home() -> Result<PathBuf> {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .context("HOME is not set")
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn remove_if_present(path: &std::path::Path) -> Result<()> {
    match std::fs::remove_file(path) {
        Err(error) if error.kind() != std::io::ErrorKind::NotFound => {
            Err(error).with_context(|| format!("Remove {}", path.display()))
        }
        _ => Ok(()),
    }
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn run(program: &str, arguments: &[&str]) -> Result<()> {
    let output = Command::new(program)
        .args(arguments)
        .stdin(Stdio::null())
        .output()
        .with_context(|| format!("Run {program}"))?;
    anyhow::ensure!(
        output.status.success(),
        "{program} {} failed: {}",
        arguments.join(" "),
        String::from_utf8_lossy(&output.stderr).trim()
    );
    Ok(())
}

#[cfg(any(target_os = "macos", test))]
fn launchd_plist(agent: &std::path::Path, data: &std::path::Path, log: &std::path::Path) -> String {
    let escape = |path: &std::path::Path| {
        path.to_string_lossy()
            .replace('&', "&amp;")
            .replace('<', "&lt;")
            .replace('>', "&gt;")
    };
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key>
    <string>com.k3.up.agent</string>
    <key>AssociatedBundleIdentifiers</key>
    <array>
        <string>com.k3.up</string>
    </array>
    <key>ProgramArguments</key>
    <array>
        <string>{agent}</string>
        <string>--data-dir</string>
        <string>{data}</string>
    </array>
    <key>RunAtLoad</key>
    <true/>
    <key>KeepAlive</key>
    <dict>
        <key>SuccessfulExit</key>
        <false/>
    </dict>
    <key>StandardOutPath</key>
    <string>{log}</string>
    <key>StandardErrorPath</key>
    <string>{log}</string>
</dict>
</plist>
"#,
        agent = escape(agent),
        data = escape(data),
        log = escape(log),
    )
}

#[cfg(any(target_os = "linux", test))]
fn systemd_unit(agent: &std::path::Path, data: &std::path::Path) -> String {
    let quote = |path: &std::path::Path| crate::systemd::quoted(&path.to_string_lossy());
    // KillMode=mixed lets the agent stop its workloads itself before systemd cleans up.
    format!(
        "[Unit]\nDescription=K3 Up agent\n\n[Service]\nExecStart={} --data-dir {}\nRestart=on-failure\nRestartSec=5\nKillMode=mixed\nTimeoutStopSec=600\n\n[Install]\nWantedBy=default.target\n",
        quote(agent),
        quote(data)
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn launchd_plist_escapes_paths() {
        let plist = launchd_plist(
            Path::new("/Apps/R&D <x>/k3up-agent"),
            Path::new("/data"),
            Path::new("/data/agent.log"),
        );
        assert!(plist.contains("<string>/Apps/R&amp;D &lt;x&gt;/k3up-agent</string>"));
        assert!(plist.contains("<key>SuccessfulExit</key>\n        <false/>"));
    }

    #[test]
    fn systemd_unit_quotes_paths() {
        let unit = systemd_unit(
            Path::new("/opt/my apps/k3up-agent"),
            Path::new("/data/100%"),
        );
        assert!(unit.contains("ExecStart=\"/opt/my apps/k3up-agent\" --data-dir \"/data/100%%\""));
    }
}
