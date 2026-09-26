mod agent;
mod cli;
mod groups;
mod health;
mod manifests;
mod output;
mod path;
mod resolve;
mod systemd;
mod template;
mod workloads;

use anyhow::{Result, bail};
use clap::{CommandFactory, Parser};
use cli::{Action, AgentCommand, Args, PathCommand};
use k3up::{client::Client, platform, protocol::Response};
use output::Outcome;

enum Target {
    Name(String),
    Folder(String),
}

/// clap already insists on exactly one of the two; this keeps the match total.
fn target(name: Option<String>, group: Option<String>) -> Result<Target> {
    match (name, group) {
        (Some(name), None) => Ok(Target::Name(name)),
        (None, Some(folder)) => Ok(Target::Folder(folder)),
        _ => bail!("Give a workload name or --group PATH, not both"),
    }
}

fn run(args: Args) -> Result<Outcome> {
    let client = Client::new(
        args.data_dir
            .clone()
            .unwrap_or_else(platform::default_data_dir),
    );
    let outcome = match args.command {
        Action::Create {
            name,
            exe,
            flags,
            start,
            wait,
            timeout,
            args,
        } => workloads::create(
            &client,
            workloads::Create {
                name,
                exe,
                flags,
                start,
                wait,
                timeout,
                args,
            },
        )?
        .into(),
        Action::Edit {
            name,
            exe,
            flags,
            clear_env,
            unset_env,
            clear_depends_on,
            clear_schedule,
            clear_readiness,
            clear_run_timeout,
            clear_args,
            clear_success_exit_codes,
            clear_group,
            restart_running,
            args,
        } => workloads::edit(
            &client,
            workloads::Edit {
                name,
                exe,
                flags,
                clear_env,
                unset_env,
                clear_depends_on,
                clear_schedule,
                clear_readiness,
                clear_run_timeout,
                clear_args,
                clear_success_exit_codes,
                clear_group,
                restart_running,
                args,
            },
        )?
        .into(),
        Action::Show { name } => workloads::show(&client, &name)?,
        Action::List { group } => groups::list(&client, group.as_deref())?.into(),
        Action::Status { name } => client.send(k3up::protocol::Command::Get { name })?.into(),
        Action::Start {
            name,
            group,
            wait,
            timeout,
        } => match target(name, group)? {
            Target::Name(name) => workloads::start(&client, &name, wait, timeout)?.into(),
            Target::Folder(folder) => {
                groups::act(&client, &folder, groups::Verb::Start, wait, timeout)?
            }
        },
        Action::Stop { name, group } => match target(name, group)? {
            Target::Name(name) => client.send(k3up::protocol::Command::Stop { name })?.into(),
            Target::Folder(folder) => groups::act(&client, &folder, groups::Verb::Stop, false, 0)?,
        },
        Action::Restart {
            name,
            group,
            wait,
            timeout,
        } => match target(name, group)? {
            Target::Name(name) => workloads::restart(&client, &name, wait, timeout)?.into(),
            Target::Folder(folder) => {
                groups::act(&client, &folder, groups::Verb::Restart, wait, timeout)?
            }
        },
        Action::Groups => groups::groups(&client)?,
        Action::Remove { name, stop } => workloads::remove(&client, name, stop)?.into(),
        Action::Logs {
            name,
            lines,
            follow,
        } => workloads::logs(&client, name, lines, follow, args.json)?.into(),
        Action::Events { name, limit } => workloads::events(&client, name, limit)?.into(),
        Action::Schedule {
            name,
            schedule,
            clear,
            restart,
        } => workloads::schedule(&client, name, schedule, clear, restart)?.into(),
        Action::Validate { file } => manifests::validate(&file)?.into(),
        Action::Apply { file, dry_run } => manifests::apply(&client, &file, dry_run)?.into(),
        Action::Export { output, group } => {
            manifests::export(&client, output, group.as_deref())?.into()
        }
        Action::Template => manifests::template().into(),
        Action::Stats { name, watch } => health::stats(&client, name, watch, args.json)?,
        Action::Health { strict } => health::health(&client, strict)?,
        Action::Agent(command) => match command {
            AgentCommand::Install => agent::install(&client)?.into(),
            AgentCommand::Uninstall { timeout } => agent::uninstall(&client, timeout)?.into(),
            AgentCommand::Start => agent::start(&client)?.into(),
            AgentCommand::Stop { timeout } => agent::stop(&client, timeout)?.into(),
            AgentCommand::Status => agent::status(&client)?,
            AgentCommand::InstallService => agent::install_service(&client.data_dir)?.into(),
            AgentCommand::UninstallService => agent::uninstall_service()?.into(),
        },
        Action::InstallAgent => agent::install_service(&client.data_dir)?.into(),
        Action::UninstallAgent => agent::uninstall_service()?.into(),
        Action::Path(command) => match command {
            PathCommand::Add { dir } => path::add(dir)?.into(),
            PathCommand::Remove { dir } => path::remove(dir)?.into(),
        },
        Action::Completions { shell } => {
            clap_complete::generate(shell, &mut Args::command(), "k3up", &mut std::io::stdout());
            return Ok(Response::success("").into());
        }
        Action::SystemdExport { file, output } => systemd::export(&file, &output)?.into(),
        Action::SystemdInstall { file } => systemd::install(&file)?.into(),
    };
    Ok(outcome)
}

fn main() {
    let args = Args::parse();
    let json = args.json;
    let completions = matches!(args.command, Action::Completions { .. });
    let code = match run(args) {
        Ok(_) if completions => 0,
        Ok(outcome) => match output::print(outcome, json) {
            Ok(true) => 0,
            Ok(false) => 1,
            Err(error) => report(&error, json),
        },
        Err(error) => report(&error, json),
    };
    std::process::exit(code);
}

fn report(error: &anyhow::Error, json: bool) -> i32 {
    if json {
        println!(
            "{}",
            serde_json::to_string(&Response::error(format!("{error:#}"))).unwrap()
        );
    } else {
        eprintln!("Error: {error:#}");
    }
    1
}
