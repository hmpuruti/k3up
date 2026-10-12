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
    platform::same_dir(&client.data_dir, &platform::login_data_dir())
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

/// Refuses a new login item while services mode runs the workloads instead: the agent would
/// run its own workloads beside the services.
fn check_install(managed: bool, services_mode: bool, data: &Path) -> Result<()> {
    if !managed {
        bail!(
            "The login item always manages the default data directory. Use `k3up agent start --data-dir {}` to run an agent for another one",
            data.display()
        );
    }
    if services_mode {
        bail!(
            "Services mode is on, so workloads run as Windows services. Run them there with `k3up apply`, not under your own agent"
        );
    }
    Ok(())
}

fn services_mode() -> bool {
    k3up::services::machine_mode_on()
}

pub fn install(client: &Client) -> Result<Response> {
    check_install(managed(client), services_mode(), &client.data_dir)?;
    let login = login(client)?;
    login.register()?;
    login.start()?;
    let info = wait_until_answering(client)?;
    back_out_if_services_mode(client, &login, true)?;
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
    if managed(client) {
        check_install(true, services_mode(), &client.data_dir)?;
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
    if managed(client) {
        back_out_if_services_mode(client, &login, false)?;
    }
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

/// `services enable` may have published services mode while this agent was starting. The
/// agent then stops again, so its workloads never run beside the services.
fn back_out_if_services_mode(client: &Client, login: &LoginAgent, registered: bool) -> Result<()> {
    if !services_mode() {
        return Ok(());
    }
    back_out(
        || shutdown(client, login, 60).map(drop),
        registered.then_some(|| login.unregister()),
    )?;
    bail!(
        "Services mode was turned on while the agent was starting, so the agent was stopped{}. Run workloads as Windows services with `k3up apply`",
        if registered {
            " and its login item removed"
        } else {
            ""
        }
    )
}

/// Stops the agent, then removes the login item, attempting both whatever fails, so the
/// agent never keeps running beside the services.
fn back_out(
    stop: impl FnOnce() -> Result<()>,
    unregister: Option<impl FnOnce() -> Result<()>>,
) -> Result<()> {
    let mut problems = vec![];
    if let Err(error) = stop() {
        problems.push(format!("The agent did not stop: {error:#}"));
    }
    if let Some(unregister) = unregister
        && let Err(error) = unregister()
    {
        problems.push(format!("The login item was not removed: {error:#}"));
    }
    if problems.is_empty() {
        return Ok(());
    }
    bail!(
        "Services mode was turned on while the agent was starting, and backing out failed:\n{}",
        problems.join("\n")
    )
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_login_item_stays_with_the_users_own_directory_in_services_mode() {
        let own = std::env::temp_dir().join("k3up-own");
        let machine = std::env::temp_dir().join("k3up-machine");

        check_install(true, false, &own).unwrap();
        let other = check_install(false, false, &machine).unwrap_err();
        assert!(
            other
                .to_string()
                .starts_with("The login item always manages")
        );
        let beside = check_install(true, true, &own).unwrap_err();
        assert!(beside.to_string().starts_with("Services mode is on"));
    }

    #[test]
    fn the_default_directory_is_managed_however_it_is_spelled() {
        let default = platform::login_data_dir();
        let text = default.to_string_lossy().into_owned();
        let trailing = format!("{text}{}", std::path::MAIN_SEPARATOR);
        assert!(managed(&Client::new(&default)));
        assert!(managed(&Client::new(&trailing)));
        assert!(!managed(&Client::new(default.join("other"))));
        let upper = text.to_uppercase();
        assert_ne!(upper, text);
        assert_eq!(managed(&Client::new(&upper)), cfg!(windows));
        #[cfg(windows)]
        assert!(managed(&Client::new(format!(
            r"\\?\{}",
            text.to_lowercase()
        ))));
    }

    fn backed_out(
        stop_fails: bool,
        unregister: Option<bool>,
    ) -> (Vec<&'static str>, Option<String>) {
        let calls = std::cell::RefCell::new(vec![]);
        let step = &|name: &'static str, fails: bool| -> Result<()> {
            calls.borrow_mut().push(name);
            if fails {
                bail!("{name} failed");
            }
            Ok(())
        };
        let result = back_out(
            || step("stop", stop_fails),
            unregister.map(|fails| move || step("unregister", fails)),
        );
        (
            calls.into_inner(),
            result.err().map(|error| error.to_string()),
        )
    }

    #[test]
    fn backing_out_stops_the_agent_first_and_attempts_every_step() {
        assert_eq!(
            backed_out(false, Some(false)),
            (vec!["stop", "unregister"], None)
        );
        assert_eq!(backed_out(false, None), (vec!["stop"], None));
        let heading =
            "Services mode was turned on while the agent was starting, and backing out failed:";
        assert_eq!(
            backed_out(true, Some(false)),
            (
                vec!["stop", "unregister"],
                Some(format!("{heading}\nThe agent did not stop: stop failed"))
            )
        );
        assert_eq!(
            backed_out(false, Some(true)),
            (
                vec!["stop", "unregister"],
                Some(format!(
                    "{heading}\nThe login item was not removed: unregister failed"
                ))
            )
        );
        assert_eq!(
            backed_out(true, Some(true)),
            (
                vec!["stop", "unregister"],
                Some(format!(
                    "{heading}\nThe agent did not stop: stop failed\nThe login item was not removed: unregister failed"
                ))
            )
        );
    }
}
