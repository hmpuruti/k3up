use crate::{
    cli::{ScheduleFlags, WorkloadFlags},
    output::{Outcome, checked},
    resolve,
};
use anyhow::{Result, bail};
use k3up::{
    client::Client,
    model::{Kind, Manifest, Missed, Schedule, State, Status, Workload},
    protocol::{Command, Response},
};
use std::{
    io::Write,
    time::{Duration, Instant},
};

pub struct Create {
    pub name: String,
    pub exe: String,
    pub flags: WorkloadFlags,
    pub start: bool,
    pub wait: bool,
    pub timeout: u64,
    pub args: Vec<String>,
}

pub fn create(client: &Client, request: Create) -> Result<Response> {
    let cwd = match &request.flags.cwd {
        Some(cwd) => resolve::directory(cwd)?,
        None => std::env::current_dir()?,
    };
    let mut workload = Workload {
        name: request.name.clone(),
        executable: resolve::executable(&request.exe)?.to_string_lossy().into(),
        working_directory: cwd.to_string_lossy().into(),
        args: request.args,
        ..Default::default()
    };
    apply_flags(&mut workload, &request.flags)?;
    workload.validate()?;
    let response = client.send(Command::Put {
        workload: Box::new(workload),
        create_only: true,
    })?;
    if !response.ok || !request.start {
        return Ok(response);
    }
    start(client, &request.name, request.wait, request.timeout)
}

pub struct Edit {
    pub name: String,
    pub exe: Option<String>,
    pub flags: WorkloadFlags,
    pub clear_env: bool,
    pub unset_env: Vec<String>,
    pub clear_depends_on: bool,
    pub clear_schedule: bool,
    pub clear_readiness: bool,
    pub clear_run_timeout: bool,
    pub clear_args: bool,
    pub clear_success_exit_codes: bool,
    pub clear_group: bool,
    pub restart_running: bool,
    pub args: Vec<String>,
}

pub fn edit(client: &Client, request: Edit) -> Result<Response> {
    let name = request.name.clone();
    let current = get(client, &name)?;
    let live = current.pid.is_some() || current.desired_running;
    let mut workload = current.workload.clone();
    if request.clear_env {
        workload.environment.clear();
    }
    for key in &request.unset_env {
        workload.environment.remove(key);
    }
    if request.clear_depends_on {
        workload.depends_on.clear();
    }
    if request.clear_schedule {
        workload.schedule = None;
    }
    if request.clear_readiness {
        workload.readiness_tcp = None;
    }
    if request.clear_run_timeout {
        workload.run_timeout_secs = None;
    }
    if request.clear_args {
        workload.args.clear();
    }
    if request.clear_success_exit_codes {
        workload.success_exit_codes.clear();
    }
    if request.clear_group {
        workload.group.clear();
    }
    if let Some(exe) = &request.exe {
        workload.executable = resolve::executable(exe)?.to_string_lossy().into();
    }
    apply_flags(&mut workload, &request.flags)?;
    if !request.args.is_empty() {
        workload.args = request.args;
    }
    workload.validate()?;
    if workload == current.workload {
        return Ok(Response::success("No changes"));
    }
    // Labels apply to a running workload at once, unless a restart was asked for anyway.
    let relabel = current.workload.only_labels_differ(&workload);
    if !live || (relabel && !request.restart_running) {
        return put(client, workload);
    }
    if !request.restart_running {
        bail!(
            "'{name}' is {}. Stop it first, or pass --restart-running to stop it, apply the change and start it again",
            current.state
        );
    }
    checked(client.send(Command::Stop { name: name.clone() })?)?;
    let saved = put(client, workload);
    let started = client.send(Command::Start { name: name.clone() });
    let saved = saved?;
    if !saved.ok {
        return Ok(saved);
    }
    let started = checked(started?)?;
    Ok(Response {
        message: format!("{}\nStarted {name} again", saved.message),
        ..started
    })
}

fn put(client: &Client, workload: Workload) -> Result<Response> {
    let response = client.send(Command::Put {
        workload: Box::new(workload),
        create_only: false,
    })?;
    if !response.ok && response.message.contains("before changing") {
        return Ok(Response::error(format!(
            "{}, or pass --restart-running",
            response.message
        )));
    }
    Ok(response)
}

/// Applies the flags that were given, leaving the rest of the definition as it is.
fn apply_flags(workload: &mut Workload, flags: &WorkloadFlags) -> Result<()> {
    if let Some(cwd) = &flags.cwd {
        workload.working_directory = resolve::directory(cwd)?.to_string_lossy().into();
    }
    if let Some(description) = &flags.description {
        workload.description = description.clone();
    }
    if let Some(group) = &flags.group {
        workload.group = group.clone();
    }
    if let Some(job) = flags.job {
        workload.kind = if job { Kind::Job } else { Kind::Service };
    }
    if let Some(boot) = flags.start_at_boot {
        workload.start_at_boot = boot;
    }
    for (key, value) in &flags.env {
        workload.environment.insert(key.clone(), value.clone());
    }
    if !flags.depends_on.is_empty() {
        workload.depends_on = flags.depends_on.clone();
    }
    if let Some(address) = &flags.readiness_tcp {
        workload.readiness_tcp = Some(address.clone());
    }
    let restart_given = flags.never_restart
        || flags.restart.is_some()
        || flags.max_restarts.is_some()
        || flags.restart_delay.is_some()
        || flags.restart_backoff.is_some();
    if restart_given && workload.kind == Kind::Job {
        bail!("Restart settings apply to services only");
    }
    if flags.never_restart {
        workload.restart = k3up::model::Restart::Never;
    } else if let Some(restart) = flags.restart {
        workload.restart = restart.into();
    }
    if let Some(limit) = flags.max_restarts {
        workload.max_restarts = limit;
    }
    if let Some(delay) = flags.restart_delay {
        workload.restart_delay_secs = delay;
    }
    if let Some(backoff) = flags.restart_backoff {
        workload.restart_backoff = backoff.into();
    }
    if !flags.success_exit_code.is_empty() {
        workload.success_exit_codes = flags.success_exit_code.clone();
    }
    if let Some(timeout) = flags.stop_timeout {
        workload.stop_timeout_secs = timeout;
    }
    if let Some(timeout) = flags.run_timeout {
        workload.run_timeout_secs = Some(timeout);
    }
    if let Some(timeout) = flags.startup_timeout {
        workload.startup_timeout_secs = timeout;
    }
    if !flags.schedule.is_empty() {
        let mut schedule = workload.schedule.take().unwrap_or_else(empty_schedule);
        merge_schedule(&mut schedule, &flags.schedule);
        if schedule.every_secs.is_none() && schedule.cron.is_none() {
            bail!("A schedule needs --every or --cron");
        }
        workload.schedule = Some(schedule);
    }
    Ok(())
}

fn empty_schedule() -> Schedule {
    Schedule {
        every_secs: None,
        cron: None,
        timezone: "UTC".into(),
        action: Default::default(),
        missed: Default::default(),
    }
}

fn merge_schedule(schedule: &mut Schedule, flags: &ScheduleFlags) {
    if let Some(seconds) = flags.every {
        schedule.every_secs = Some(seconds);
        schedule.cron = None;
    }
    if let Some(cron) = &flags.cron {
        schedule.cron = Some(cron.clone());
        schedule.every_secs = None;
    }
    if let Some(timezone) = &flags.timezone {
        schedule.timezone = timezone.clone();
    }
    if let Some(action) = flags.schedule_action {
        schedule.action = action.into();
    }
    if let Some(catch_up) = flags.catch_up {
        schedule.missed = if catch_up {
            Missed::RunOnce
        } else {
            Missed::Skip
        };
    }
}

pub fn schedule(
    client: &Client,
    name: String,
    flags: ScheduleFlags,
    clear: bool,
    restart: bool,
) -> Result<Response> {
    let mut workload = get(client, &name)?.workload;
    workload.schedule = if clear {
        None
    } else {
        let mut schedule = empty_schedule();
        merge_schedule(&mut schedule, &flags);
        if restart {
            schedule.action = k3up::model::ScheduleAction::Restart;
        }
        if schedule.every_secs.is_none() && schedule.cron.is_none() {
            bail!("Give --every, --cron or --clear");
        }
        Some(schedule)
    };
    workload.validate()?;
    put(client, workload)
}

pub fn get(client: &Client, name: &str) -> Result<Status> {
    let mut response = checked(client.send(Command::Get { name: name.into() })?)?;
    Ok(response.workloads.remove(0))
}

pub fn show(client: &Client, name: &str) -> Result<Outcome> {
    let status = get(client, name)?;
    let manifest = Manifest {
        version: 1,
        workloads: vec![status.workload.clone()],
    };
    Ok(Outcome::Custom {
        text: manifest.to_toml()?.trim_end().to_string(),
        json: serde_json::to_value(&status)?,
        ok: true,
    })
}

pub fn deadline(wait: bool, timeout: u64) -> Option<Instant> {
    wait.then(|| Instant::now() + Duration::from_secs(timeout))
}

pub fn start(client: &Client, name: &str, wait: bool, timeout: u64) -> Result<Response> {
    start_by(client, name, deadline(wait, timeout))
}

/// With a deadline, waits for the workload to be ready, sharing the deadline across a group.
pub fn start_by(client: &Client, name: &str, deadline: Option<Instant>) -> Result<Response> {
    let response = client.send(Command::Start { name: name.into() })?;
    match deadline {
        Some(deadline) if response.ok => wait_ready(client, name, deadline),
        _ => Ok(response),
    }
}

pub fn restart(client: &Client, name: &str, wait: bool, timeout: u64) -> Result<Response> {
    restart_by(client, name, deadline(wait, timeout))
}

pub fn restart_by(client: &Client, name: &str, deadline: Option<Instant>) -> Result<Response> {
    let response = client.send(Command::Restart { name: name.into() })?;
    match deadline {
        Some(deadline) if response.ok => wait_ready(client, name, deadline),
        _ => Ok(response),
    }
}

pub fn remove(client: &Client, name: String, stop: bool) -> Result<Response> {
    if stop {
        checked(client.send(Command::Stop { name: name.clone() })?)?;
    }
    client.send(Command::Remove { name })
}

/// Services are ready once running (after any TCP check); jobs only once they exit successfully.
fn wait_ready(client: &Client, name: &str, deadline: Instant) -> Result<Response> {
    loop {
        let response = checked(client.send(Command::Get { name: name.into() })?)?;
        let status = &response.workloads[0];
        let ready = match status.workload.kind {
            Kind::Job => status.state == State::Completed,
            Kind::Service => matches!(status.state, State::Running | State::Completed),
        };
        if ready {
            return Ok(response);
        }
        if status.state == State::Failed {
            bail!("{name} failed: {}", status.reason);
        }
        if status.state == State::Stopped && !status.desired_running {
            bail!("{name} stopped: {}", status.reason);
        }
        if Instant::now() >= deadline {
            bail!("Timed out waiting for {name}: {}", status.reason);
        }
        std::thread::sleep(Duration::from_millis(250));
    }
}

pub fn events(client: &Client, name: Option<String>, limit: usize) -> Result<Response> {
    let mut response = client.send(Command::Events { name, after: None })?;
    response.events.truncate(limit);
    Ok(response)
}

pub fn logs(
    client: &Client,
    name: String,
    lines: usize,
    follow: bool,
    json: bool,
) -> Result<Response> {
    if !follow {
        return client.send(Command::Logs {
            name,
            lines,
            after: None,
        });
    }
    let mut after = None;
    loop {
        let response = checked(client.send(Command::Logs {
            name: name.clone(),
            lines,
            after,
        })?)?;
        let text = response.text.unwrap_or_default();
        if json {
            if !text.is_empty() {
                println!(
                    "{}",
                    serde_json::json!({"name": name, "text": text, "offset": response.offset})
                );
            }
        } else if after.is_none() {
            if !text.is_empty() {
                println!("{text}");
            }
        } else {
            print!("{text}");
            std::io::stdout().flush()?;
        }
        after = Some(response.offset.unwrap_or_default());
        std::thread::sleep(Duration::from_millis(500));
    }
}
