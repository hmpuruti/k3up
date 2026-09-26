use crate::output::{Outcome, checked, duration};
use anyhow::{Context, Result, bail};
use k3up::{
    autostart::{LoginAgent, Registration, bundled_agent},
    client::Client,
    platform,
    protocol::{Command, Response},
};
use std::{
    path::Path,
    time::{Duration, Instant},
};

const ANSWER_TIMEOUT: Duration = Duration::from_secs(20);

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
    let login = login_for_data(client);
    let registration = login.registration();
    let data_dir = client.data_dir.display().to_string();
    let info = match client.send(Command::Info) {
        Ok(response) if response.ok => response.agent,
        Ok(response) => return Ok(unreachable(&response.message, &data_dir, registration)),
        Err(error) => return Ok(unreachable(&format!("{error:#}"), &data_dir, registration)),
    };
    let Some(info) = info else {
        return Ok(unreachable(
            "Agent gave no information",
            &data_dir,
            registration,
        ));
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
        "Agent       running  ·  pid {}  ·  version {}  ·  up {}\nData        {}\nExecutable  {}\nLogin item  {}\nWorkloads   {} defined  ·  {} running  ·  {} need attention",
        info.pid,
        info.version,
        duration(uptime),
        info.data_dir,
        info.executable,
        login_label(registration),
        statuses.len(),
        running,
        attention
    );
    Ok(Outcome::Custom {
        text,
        json: serde_json::json!({
            "reachable": true,
            "version": info.version,
            "pid": info.pid,
            "started_at": info.started_at,
            "uptime_secs": uptime,
            "data_dir": info.data_dir,
            "executable": info.executable,
            "login_item": registration == Registration::ThisDirectory,
            "login_item_elsewhere": registration == Registration::OtherDirectory,
            "workloads": { "total": statuses.len(), "running": running, "attention": attention },
        }),
        ok: true,
    })
}

fn login_label(registration: Registration) -> &'static str {
    match registration {
        Registration::ThisDirectory => "registered",
        Registration::OtherDirectory => "registered for another data directory",
        Registration::None => "not registered",
    }
}

fn unreachable(error: &str, data_dir: &str, registration: Registration) -> Outcome {
    Outcome::Custom {
        text: format!(
            "Agent       not reachable  ·  {error}\nData        {data_dir}\nLogin item  {}",
            login_label(registration)
        ),
        json: serde_json::json!({
            "reachable": false,
            "error": error,
            "data_dir": data_dir,
            "login_item": registration == Registration::ThisDirectory,
            "login_item_elsewhere": registration == Registration::OtherDirectory,
        }),
        ok: false,
    }
}

pub fn install_service(data_dir: &Path) -> Result<Response> {
    #[cfg(windows)]
    {
        k3up::windows_host::install(data_dir)?;
        Ok(Response::success(
            "Windows service installed in Program Files with data in ProgramData\\K3 Up. Start it with `sc.exe start K3Up`; clients need an elevated terminal and --data-dir pointing at ProgramData\\K3 Up",
        ))
    }
    #[cfg(not(windows))]
    {
        let _ = data_dir;
        bail!("The machine service is Windows only. Use `k3up agent install` here")
    }
}

pub fn uninstall_service() -> Result<Response> {
    #[cfg(windows)]
    {
        k3up::windows_host::uninstall()?;
        Ok(Response::success("Windows service removed"))
    }
    #[cfg(not(windows))]
    {
        bail!("The machine service is Windows only")
    }
}
