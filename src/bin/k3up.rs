use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};
use k3up::{
    client::Client,
    model::*,
    platform,
    protocol::{Command, Response},
    systemd,
};
use std::{
    io::Write,
    path::PathBuf,
    time::{Duration, Instant},
};

#[derive(Parser)]
#[command(version, about = "K3 Up · register, schedule, and manage applications")]
struct Args {
    #[arg(long, global = true)]
    data_dir: Option<PathBuf>,
    #[arg(long, global = true)]
    json: bool,
    #[command(subcommand)]
    command: Action,
}

#[derive(Subcommand)]
enum Action {
    /// Register an executable. Arguments after -- are passed literally.
    Create {
        name: String,
        #[arg(long)]
        exe: PathBuf,
        #[arg(long)]
        cwd: Option<PathBuf>,
        #[arg(long)]
        job: bool,
        #[arg(long)]
        boot: bool,
        #[arg(long)]
        depends_on: Vec<String>,
        #[arg(long)]
        readiness_tcp: Option<String>,
        #[arg(long)]
        never_restart: bool,
        #[arg(last = true)]
        args: Vec<String>,
    },
    List,
    Status {
        name: String,
    },
    Start {
        name: String,
        #[arg(long)]
        wait: bool,
        #[arg(long, default_value_t = 60)]
        timeout: u64,
    },
    Stop {
        name: String,
    },
    Restart {
        name: String,
        #[arg(long)]
        wait: bool,
        #[arg(long, default_value_t = 60)]
        timeout: u64,
    },
    Remove {
        name: String,
    },
    Logs {
        name: String,
        #[arg(long, default_value_t = 100)]
        lines: usize,
        #[arg(long)]
        follow: bool,
    },
    Events {
        name: Option<String>,
    },
    /// Machine health and the resources each running workload uses.
    Stats,
    /// Validate a complete TOML manifest without contacting the agent.
    Validate {
        file: PathBuf,
    },
    /// Create/update definitions. Stop running workloads before editing them.
    Apply {
        file: PathBuf,
        #[arg(long)]
        dry_run: bool,
    },
    Export {
        #[arg(long)]
        output: Option<PathBuf>,
    },
    Schedule {
        name: String,
        #[arg(long, conflicts_with_all = ["cron", "clear"])]
        every: Option<u64>,
        #[arg(long, conflicts_with = "clear")]
        cron: Option<String>,
        #[arg(long, default_value = "UTC")]
        timezone: String,
        #[arg(long)]
        restart: bool,
        #[arg(long)]
        catch_up: bool,
        #[arg(long)]
        clear: bool,
    },
    /// Generate native systemd services/timers; these run without our agent.
    SystemdExport {
        file: PathBuf,
        #[arg(long)]
        output: PathBuf,
    },
    /// Install generated systemd units for the current Linux user.
    SystemdInstall {
        file: PathBuf,
    },
    /// Register the Windows background agent with Service Control Manager.
    InstallAgent,
    /// Remove the Windows agent registration. Stop workloads first.
    UninstallAgent,
}

fn manifest(file: PathBuf) -> Result<Manifest> {
    toml::from_str(
        &std::fs::read_to_string(&file).with_context(|| format!("Read {}", file.display()))?,
    )
    .context("Parse TOML manifest")
}
fn checked(response: Response) -> Result<Response> {
    if !response.ok {
        bail!("{}", response.message);
    }
    Ok(response)
}
fn print(response: Response, json: bool) -> Result<()> {
    let success = response.ok;
    if json {
        println!("{}", serde_json::to_string_pretty(&response)?);
    } else if !success {
        eprintln!("{}", response.message);
    } else if let Some(manifest) = response.manifest {
        println!("{}", toml::to_string_pretty(&manifest)?);
    } else if let Some(text) = response.text {
        println!("{text}");
    } else if !response.workloads.is_empty() {
        println!(
            "{:<24} {:<12} {:<8} {:<8} REASON",
            "NAME", "STATE", "PID", "RETRIES"
        );
        for status in response.workloads {
            println!(
                "{:<24} {:<12} {:<8} {:<8} {}",
                status.workload.name,
                status.state,
                status
                    .pid
                    .map(|id| id.to_string())
                    .unwrap_or_else(|| "-".into()),
                status.restart_count,
                status.reason
            );
        }
    } else if !response.events.is_empty() {
        for event in response.events {
            println!(
                "{}  {:<20} {}",
                event.at.format("%Y-%m-%d %H:%M:%S UTC"),
                event.name,
                event.message
            );
        }
    } else {
        println!("{}", response.message);
    }
    if !success {
        std::process::exit(1);
    }
    Ok(())
}

/// Services are ready once running (after any TCP check); jobs only once they exit successfully.
fn bytes(value: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut size = value as f64;
    let mut unit = 0;
    while size >= 1024.0 && unit < UNITS.len() - 1 {
        size /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{value} B")
    } else {
        format!("{size:.1} {}", UNITS[unit])
    }
}

fn duration(seconds: u64) -> String {
    match seconds {
        0..3600 => format!("{}m", seconds / 60),
        3600..86400 => format!("{}h {}m", seconds / 3600, seconds % 3600 / 60),
        _ => format!("{}d {}h", seconds / 86400, seconds % 86400 / 3600),
    }
}

fn stats(client: &Client) -> Result<Response> {
    // The agent's first sample needs a CPU baseline, so a fresh agent may have none yet.
    // Asking switches the agent to fast sampling, so a stale sample is replaced within seconds.
    let deadline = Instant::now() + Duration::from_secs(5);
    let metrics = loop {
        // The terminal view has no charts, so skip the history.
        let response = checked(client.send(Command::Metrics {
            since: Some(chrono::Utc::now()),
        })?)?;
        match response.metrics {
            Some(metrics)
                if chrono::Utc::now() - metrics.at < chrono::TimeDelta::seconds(1)
                    || Instant::now() >= deadline =>
            {
                break metrics;
            }
            None if Instant::now() >= deadline => {
                bail!("The agent has not collected a sample yet")
            }
            _ => std::thread::sleep(Duration::from_millis(250)),
        }
    };
    let workloads = checked(client.send(Command::List)?)?.workloads;
    let machine = &metrics.machine;
    let mut text = format!(
        "{}  ·  {}  ·  up {}\n",
        machine.host,
        machine.os,
        duration(machine.uptime_secs)
    );
    text += &format!("CPU      {:>5.1}%  of {} cores", machine.cpu, machine.cores);
    if let Some([one, five, fifteen]) = machine.load {
        text += &format!("  ·  load {one:.2} {five:.2} {fifteen:.2}");
    }
    text += &format!(
        "\nMemory   {} of {}  ·  swap {} of {}\n",
        bytes(machine.memory_used),
        bytes(machine.memory_total),
        bytes(machine.swap_used),
        bytes(machine.swap_total)
    );
    text += &format!(
        "Disk     {} free of {}\n",
        bytes(machine.disk_available),
        bytes(machine.disk_total)
    );
    text += &format!(
        "K3 Up    workloads {:.1}% CPU, {}  ·  agent {:.1}% CPU, {}  ·  data {}\n\n",
        metrics.managed.cpu,
        bytes(metrics.managed.memory),
        metrics.agent.cpu,
        bytes(metrics.agent.memory),
        bytes(metrics.data_bytes)
    );
    text += &format!(
        "{:<22} {:>5} {:>7} {:>10} {:>10} {:>10} {:>10}  COMMAND",
        "NAME", "PROCS", "CPU", "MEMORY", "READ/S", "WRITE/S", "LOGS"
    );
    for usage in &metrics.workloads {
        let command = workloads
            .iter()
            .find(|status| status.workload.name == usage.name)
            .map(|status| {
                std::iter::once(status.workload.executable.as_str())
                    .chain(status.workload.args.iter().map(String::as_str))
                    .collect::<Vec<_>>()
                    .join(" ")
            })
            .unwrap_or_default();
        text += &format!(
            "\n{:<22} {:>5} {:>6.1}% {:>10} {:>10} {:>10} {:>10}  {command}",
            usage.name,
            usage.usage.processes,
            usage.usage.cpu,
            bytes(usage.usage.memory),
            bytes(usage.usage.read_rate),
            bytes(usage.usage.write_rate),
            bytes(usage.log_bytes)
        );
    }
    if metrics.workloads.is_empty() {
        text += "\nNo workloads are running.";
    }
    Ok(Response {
        text: Some(text),
        metrics: Some(metrics),
        ..Response::success("Metrics")
    })
}

fn wait_ready(client: &Client, name: &str, seconds: u64) -> Result<Response> {
    let deadline = Instant::now() + Duration::from_secs(seconds);
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

fn run(args: Args) -> Result<()> {
    let client = Client::new(
        args.data_dir
            .clone()
            .unwrap_or_else(platform::default_data_dir),
    );
    let response = match args.command {
        Action::Create {
            name,
            exe,
            cwd,
            job,
            boot,
            depends_on,
            readiness_tcp,
            never_restart,
            args,
        } => {
            // Symlinks are kept as given, so release links and multi-call binaries keep working.
            let executable = std::path::absolute(&exe)?;
            anyhow::ensure!(
                executable.is_file(),
                "Executable {} does not exist",
                executable.display()
            );
            let directory = std::path::absolute(cwd.unwrap_or(std::env::current_dir()?))?;
            anyhow::ensure!(
                directory.is_dir(),
                "Working directory {} does not exist",
                directory.display()
            );
            let workload = Workload {
                name,
                executable: executable.to_string_lossy().into(),
                working_directory: directory.to_string_lossy().into(),
                args,
                kind: if job { Kind::Job } else { Kind::Service },
                start_at_boot: boot,
                depends_on,
                readiness_tcp,
                restart: if never_restart {
                    Restart::Never
                } else {
                    Restart::OnFailure
                },
                ..Default::default()
            };
            workload.validate()?;
            client.send(Command::Put {
                workload: Box::new(workload),
                create_only: true,
            })?
        }
        Action::List => client.send(Command::List)?,
        Action::Status { name } => client.send(Command::Get { name })?,
        Action::Start {
            name,
            wait,
            timeout,
        } => {
            let response = client.send(Command::Start { name: name.clone() })?;
            if response.ok && wait {
                wait_ready(&client, &name, timeout)?
            } else {
                response
            }
        }
        Action::Stop { name } => client.send(Command::Stop { name })?,
        Action::Restart {
            name,
            wait,
            timeout,
        } => {
            let response = client.send(Command::Restart { name: name.clone() })?;
            if response.ok && wait {
                wait_ready(&client, &name, timeout)?
            } else {
                response
            }
        }
        Action::Remove { name } => client.send(Command::Remove { name })?,
        Action::Events { name } => client.send(Command::Events { name, after: None })?,
        Action::Stats => stats(&client)?,
        Action::Logs {
            name,
            lines,
            follow,
        } => {
            if follow {
                let mut after = None;
                loop {
                    let response = checked(client.send(Command::Logs {
                        name: name.clone(),
                        lines,
                        after,
                    })?)?;
                    let text = response.text.unwrap_or_default();
                    if args.json {
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
            client.send(Command::Logs {
                name,
                lines,
                after: None,
            })?
        }
        Action::Validate { file } => {
            let manifest = manifest(file)?;
            let order = manifest.validate()?;
            let now = chrono::Utc::now();
            for workload in &manifest.workloads {
                workload.first_run(now)?;
            }
            Response::success(format!("Valid. Startup order: {}", order.join(" → ")))
        }
        Action::Apply { file, dry_run } => client.send(Command::Apply {
            manifest: manifest(file)?,
            dry_run,
        })?,
        Action::Export { output } => {
            let response = checked(client.send(Command::Export)?)?;
            if let Some(output) = output {
                std::fs::write(
                    &output,
                    toml::to_string_pretty(response.manifest.as_ref().unwrap())?,
                )?;
                Response::success(format!("Saved {}", output.display()))
            } else {
                response
            }
        }
        Action::Schedule {
            name,
            every,
            cron,
            timezone,
            restart,
            catch_up,
            clear,
        } => {
            let response = checked(client.send(Command::Get { name })?)?;
            let mut workload = response.workloads[0].workload.clone();
            workload.schedule = if clear {
                None
            } else {
                Some(Schedule {
                    every_secs: every,
                    cron,
                    timezone,
                    action: if restart {
                        ScheduleAction::Restart
                    } else {
                        ScheduleAction::Start
                    },
                    missed: if catch_up {
                        Missed::RunOnce
                    } else {
                        Missed::Skip
                    },
                })
            };
            workload.validate()?;
            client.send(Command::Put {
                workload: Box::new(workload),
                create_only: false,
            })?
        }
        Action::SystemdExport { file, output } => {
            let count = systemd::export(&manifest(file)?, &output)?;
            Response::success(format!(
                "Exported {count} native units to {}",
                output.display()
            ))
        }
        Action::SystemdInstall { file } => {
            #[cfg(not(target_os = "linux"))]
            {
                let _ = file;
                bail!(
                    "systemd-install requires Linux. Use systemd-export to generate units on this OS"
                );
            }
            #[cfg(target_os = "linux")]
            {
                let manifest = manifest(file)?;
                let directory = PathBuf::from(std::env::var_os("HOME").context("HOME is not set")?)
                    .join(".config/systemd/user");
                let units = systemd::render(&manifest)?;
                // Only install new units, avoiding silent replacement of administrator changes.
                for name in units.keys() {
                    if directory.join(name).exists() {
                        bail!("{name} already exists; export and review changes first");
                    }
                }
                let run = |arguments: &[&str]| -> Result<()> {
                    let status = std::process::Command::new("systemctl")
                        .arg("--user")
                        .args(arguments)
                        .status()?;
                    anyhow::ensure!(status.success(), "systemctl failed: {arguments:?}");
                    Ok(())
                };
                let installed = systemd::export(&manifest, &directory).and_then(|_| {
                    run(&["daemon-reload"])?;
                    for workload in &manifest.workloads {
                        if workload.start_at_boot {
                            run(&["enable", &format!("k3up-{}.service", workload.name)])?;
                        }
                        if workload.schedule.is_some() {
                            run(&["enable", "--now", &format!("k3up-{}.timer", workload.name)])?;
                        }
                    }
                    Ok(())
                });
                if let Err(error) = installed {
                    // Undo this run only; every file removed here was absent before it started.
                    for name in units.keys() {
                        let _ = run(&["disable", "--now", name]);
                        let _ = std::fs::remove_file(directory.join(name));
                    }
                    let _ = run(&["daemon-reload"]);
                    return Err(error.context("Installation rolled back"));
                }
                Response::success(
                    "Installed user units. Use systemctl --user to manage them; login lingering is required for execution without a login",
                )
            }
        }
        Action::InstallAgent => {
            #[cfg(windows)]
            {
                k3up::windows_host::install(&client.data_dir)?;
                Response::success(
                    "Windows agent installed in Program Files with protected data in ProgramData/K3 Up. Start with sc.exe start K3Up; clients need elevation and --data-dir pointing to ProgramData/K3 Up",
                )
            }
            #[cfg(not(windows))]
            {
                bail!(
                    "install-agent currently supports Windows. See README for Linux/systemd and macOS development usage"
                );
            }
        }
        Action::UninstallAgent => {
            #[cfg(windows)]
            {
                k3up::windows_host::uninstall()?;
                Response::success("Windows agent registration removed")
            }
            #[cfg(not(windows))]
            {
                bail!("uninstall-agent currently supports Windows");
            }
        }
    };
    print(response, args.json)
}
fn main() {
    let args = Args::parse();
    let json = args.json;
    if let Err(error) = run(args) {
        if json {
            println!(
                "{}",
                serde_json::to_string(&Response::error(format!("{error:#}"))).unwrap()
            );
        } else {
            eprintln!("Error: {error:#}");
        }
        std::process::exit(1);
    }
}
