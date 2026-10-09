use crate::output::{Outcome, checked, duration};
use anyhow::{Context, Result, bail};
use k3up::{
    autostart::{LoginAgent, Registration, bundled_agent},
    client::Client,
    platform,
    protocol::{Command, Response},
};
use std::time::{Duration, Instant};

const ANSWER_TIMEOUT: Duration = Duration::from_secs(20);

/// Whether the client's data directory belongs to the Windows service.
#[cfg(windows)]
fn service(client: &Client) -> bool {
    platform::is_machine_data_dir(&client.data_dir)
}

/// Whether the client's data directory is the one the login item manages.
fn managed(client: &Client) -> bool {
    let default = platform::default_data_dir();
    std::path::absolute(&client.data_dir).ok() == std::path::absolute(&default).ok()
}

fn login(client: &Client) -> Result<LoginAgent> {
    let agent = bundled_agent().with_context(|| {
        format!(
            "No agent found beside {}. Install k3up-agent next to k3up",
            std::env::current_exe()
                .map(|path| path.display().to_string())
                .unwrap_or_else(|_| "k3up".into())
        )
    })?;
    Ok(LoginAgent::new(agent, client.data_dir.clone()))
}

/// For status and stop, which only need the data directory.
fn login_for_data(client: &Client) -> LoginAgent {
    LoginAgent::new(bundled_agent().unwrap_or_default(), client.data_dir.clone())
}

fn wait_until_answering(client: &Client) -> Result<Response> {
    let deadline = Instant::now() + ANSWER_TIMEOUT;
    loop {
        if let Ok(response) = client.send(Command::Info)
            && response.ok
        {
            return Ok(response);
        }
        if Instant::now() >= deadline {
            bail!(
                "The agent did not answer within {} seconds. See {}",
                ANSWER_TIMEOUT.as_secs(),
                client.data_dir.join("agent.log").display()
            );
        }
        std::thread::sleep(Duration::from_millis(200));
    }
}

fn wait_until_stopped(login: &LoginAgent, timeout: u64, request: Result<()>) -> Result<()> {
    let deadline = Instant::now() + Duration::from_secs(timeout);
    while login.is_running() {
        if Instant::now() >= deadline {
            return match request {
                Ok(()) => {
                    bail!("The agent is still stopping its workloads after {timeout} seconds")
                }
                Err(error) => Err(error.context("The agent did not stop")),
            };
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    Ok(())
}

pub fn install(client: &Client) -> Result<Response> {
    if cfg!(windows) {
        return install_service(client, 120);
    }
    if !managed(client) {
        bail!(
            "The login item always manages the default data directory. Use `k3up agent start --data-dir {}` to run an agent for another one",
            client.data_dir.display()
        );
    }
    let login = login(client)?;
    login.register()?;
    login.start()?;
    let info = wait_until_answering(client)?;
    Ok(Response {
        message: format!(
            "Agent registered to start at login and running (pid {})",
            info.agent.as_ref().map_or(0, |agent| agent.pid)
        ),
        ..info
    })
}

pub fn uninstall(client: &Client, timeout: u64) -> Result<Response> {
    if cfg!(windows) {
        return uninstall_service(client, timeout);
    }
    if !managed(client) {
        bail!(
            "The login item always manages the default data directory. Use `k3up agent stop` for another one"
        );
    }
    let login = login(client)?;
    login.unregister()?;
    let stopped = shutdown(client, &login, timeout)?;
    Ok(Response::success(format!(
        "Login item removed. {stopped} Data kept in {}",
        client.data_dir.display()
    )))
}

pub fn start(client: &Client) -> Result<Response> {
    #[cfg(windows)]
    if service(client) {
        k3up::windows_host::start()?;
        let info = wait_until_answering(client)?;
        return Ok(Response {
            message: format!(
                "K3 Up service running (pid {})",
                info.agent.as_ref().map_or(0, |agent| agent.pid)
            ),
            ..info
        });
    }
    let login = login(client)?;
    if login.is_running() {
        let info = wait_until_answering(client)?;
        return Ok(Response {
            message: format!(
                "Agent already running (pid {})",
                info.agent.as_ref().map_or(0, |agent| agent.pid)
            ),
            ..info
        });
    }
    if managed(client) {
        login.start()?;
    } else {
        login.launch()?;
    }
    let info = wait_until_answering(client)?;
    Ok(Response {
        message: format!(
            "Agent running (pid {})",
            info.agent.as_ref().map_or(0, |agent| agent.pid)
        ),
        ..info
    })
}

pub fn stop(client: &Client, timeout: u64) -> Result<Response> {
    #[cfg(windows)]
    if service(client) {
        k3up::windows_host::stop(Duration::from_secs(timeout))?;
        return Ok(Response::success(
            "K3 Up service stopped. It starts again at the next boot, or with `k3up agent start`",
        ));
    }
    let login = login_for_data(client);
    Ok(Response::success(shutdown(client, &login, timeout)?))
}

fn shutdown(client: &Client, login: &LoginAgent, timeout: u64) -> Result<String> {
    if !login.is_running() {
        return Ok("Agent is not running.".into());
    }
    // An agent with nothing to stop can exit before its reply reaches us; what matters is
    // that it exits, so a lost reply only counts once the wait runs out.
    let request = client.send(Command::Shutdown).and_then(checked).map(|_| ());
    wait_until_stopped(login, timeout, request)?;
    Ok("Agent stopped.".into())
}

pub fn status(client: &Client) -> Result<Outcome> {
    let starter = Starter::of(client);
    let data_dir = client.data_dir.display().to_string();
    let info = match client.send(Command::Info) {
        Ok(response) if response.ok => response.agent,
        Ok(response) => return Ok(unreachable(&response.message, &data_dir, &starter)),
        Err(error) => return Ok(unreachable(&format!("{error:#}"), &data_dir, &starter)),
    };
    let Some(info) = info else {
        return Ok(unreachable("Agent gave no information", &data_dir, &starter));
    };
    let statuses = checked(client.send(Command::List)?)?.workloads;
    let running = statuses
        .iter()
        .filter(|status| status.state == k3up::model::State::Running)
        .count();
    let attention = statuses
        .iter()
        .filter(|status| k3up::health::needs_attention(status.state))
        .count();
    let uptime = (chrono::Utc::now() - info.started_at).num_seconds().max(0) as u64;
    let text = format!(
        "Agent       running  ·  pid {}  ·  version {}  ·  up {}\nData        {}\nExecutable  {}\n{}\nWorkloads   {} defined  ·  {} running  ·  {} need attention",
        info.pid,
        info.version,
        duration(uptime),
        info.data_dir,
        info.executable,
        starter.line(),
        statuses.len(),
        running,
        attention
    );
    let mut json = serde_json::json!({
        "reachable": true,
        "version": info.version,
        "pid": info.pid,
        "started_at": info.started_at,
        "uptime_secs": uptime,
        "data_dir": info.data_dir,
        "executable": info.executable,
        "workloads": { "total": statuses.len(), "running": running, "attention": attention },
    });
    starter.add_to(&mut json);
    Ok(Outcome::Custom {
        text,
        json,
        ok: true,
    })
}

/// What starts the agent for a data directory: a login item, or on Windows the service.
enum Starter {
    Login(Registration),
    /// The service's state, `None` when it is not installed.
    #[cfg_attr(not(windows), allow(dead_code))]
    Service(Option<&'static str>),
}

impl Starter {
    fn of(client: &Client) -> Self {
        #[cfg(windows)]
        if service(client) {
            return Self::Service(service_state());
        }
        Self::Login(login_for_data(client).registration())
    }

    fn line(&self) -> String {
        match self {
            Self::Login(registration) => format!(
                "Login item  {}",
                match registration {
                    Registration::ThisDirectory => "registered",
                    Registration::OtherDirectory => "registered for another data directory",
                    Registration::None => "not registered",
                }
            ),
            Self::Service(Some(state)) => format!("Service     {state}  ·  starts at boot"),
            Self::Service(None) => "Service     not installed".into(),
        }
    }

    fn add_to(&self, json: &mut serde_json::Value) {
        let registration = match self {
            Self::Login(registration) => *registration,
            Self::Service(_) => Registration::None,
        };
        json["login_item"] = (registration == Registration::ThisDirectory).into();
        json["login_item_elsewhere"] = (registration == Registration::OtherDirectory).into();
        if let Self::Service(state) = self {
            json["service"] = state.unwrap_or("not_installed").into();
        }
    }
}

#[cfg(windows)]
fn service_state() -> Option<&'static str> {
    use windows_service::service::ServiceState;
    match k3up::windows_host::state() {
        Ok(Some(ServiceState::Running)) => Some("running"),
        Ok(Some(ServiceState::Stopped)) => Some("stopped"),
        Ok(Some(ServiceState::StartPending)) => Some("starting"),
        Ok(Some(ServiceState::StopPending)) => Some("stopping"),
        Ok(Some(_)) => Some("paused"),
        Ok(None) | Err(_) => None,
    }
}

fn unreachable(error: &str, data_dir: &str, starter: &Starter) -> Outcome {
    let mut json = serde_json::json!({
        "reachable": false,
        "error": error,
        "data_dir": data_dir,
    });
    starter.add_to(&mut json);
    Outcome::Custom {
        text: format!(
            "Agent       not reachable  ·  {error}\nData        {data_dir}\n{}",
            starter.line()
        ),
        json,
        ok: false,
    }
}

pub fn install_service(client: &Client, timeout: u64) -> Result<Response> {
    #[cfg(windows)]
    {
        if !service(client) {
            bail!(
                "The K3 Up service always uses {}. Use `k3up agent start --data-dir {}` to run an agent for another directory",
                platform::machine_data_dir().display(),
                client.data_dir.display()
            );
        }
        k3up::windows_host::install(Duration::from_secs(timeout))?;
        let install_dir = k3up::windows_host::install_dir()?;
        let path = k3up::user_path::add(&install_dir, k3up::user_path::Scope::System);
        let info = wait_until_answering(client)?;
        Ok(Response {
            message: format!(
                "K3 Up service installed in {} and running as SYSTEM (pid {}). It starts at boot, and every logged-in user can manage it{}",
                install_dir.display(),
                info.agent.as_ref().map_or(0, |agent| agent.pid),
                match path {
                    Ok(_) => String::new(),
                    Err(error) => format!(". The system PATH was not updated: {error:#}"),
                }
            ),
            ..info
        })
    }
    #[cfg(not(windows))]
    {
        let _ = (client, timeout);
        bail!("The machine service is Windows only. Use `k3up agent install` here")
    }
}

pub fn uninstall_service(client: &Client, timeout: u64) -> Result<Response> {
    #[cfg(windows)]
    {
        if !service(client) {
            bail!(
                "The K3 Up service always uses {}. Use `k3up agent stop` for another directory",
                platform::machine_data_dir().display()
            );
        }
        k3up::windows_host::uninstall(Duration::from_secs(timeout))?;
        Ok(Response::success(format!(
            "K3 Up service removed. Data kept in {}",
            platform::machine_data_dir().display()
        )))
    }
    #[cfg(not(windows))]
    {
        let _ = (client, timeout);
        bail!("The machine service is Windows only")
    }
}
