use clap::{Args as ClapArgs, Parser, Subcommand, ValueEnum};
use clap_complete::Shell;
use k3up::model::{Restart, ScheduleAction};
use std::path::PathBuf;

const ABOUT: &str = "Keep applications running as background services and scheduled jobs";

const LONG_ABOUT: &str = "\
Keep applications running as background services and scheduled jobs.

The agent
  k3up talks to a local agent, k3up-agent, which starts your programs, restarts them when
  they crash, runs them on a schedule and brings them back after a reboot. The agent keeps
  running when you close k3up or the desktop app. Only `k3up agent stop`, `k3up agent
  uninstall` or a shutdown of the machine stops it, and the login item starts it again at
  the next login.

Services and jobs
  A service is kept running. When it exits, its restart policy decides whether it comes
  back. A job runs to completion each time it is started or scheduled, and is never
  restarted. Both can have a schedule, dependencies, environment variables and timeouts.

Data directory
  The agent keeps its database, logs and socket in one directory:
    macOS and Linux   ~/.local/share/k3up
    Windows           %LOCALAPPDATA%\\K3 Up
  Set K3UP_DATA_DIR, or pass --data-dir to every command, to use another one. The login
  item always manages the default directory.

Command groups
  Workloads   create, edit, show, list, status, start, stop, restart, remove, logs,
              events, schedule
  Manifests   validate, apply, export, template
  Health      stats, health
  Agent       agent install, uninstall, start, stop, status, install-service,
              uninstall-service
  Other       completions, systemd-export, systemd-install

Output and exit codes
  --json prints the result as one JSON document, including errors, for scripts and AI
  agents. The exit code is 0 on success, 1 when the request failed and 2 when the command
  line was invalid. `k3up <command> --help` explains each command and shows examples.";

const START_HERE: &str = "\
Start here:
  k3up agent install
  k3up create web --exe node --cwd /srv/web --env PORT=8080 \\
      --readiness-tcp 127.0.0.1:8080 --start-at-boot -- server.js
  k3up start web --wait
  k3up health
  k3up logs web --follow";

#[derive(Parser)]
#[command(
    name = "k3up",
    version,
    about = ABOUT,
    long_about = LONG_ABOUT,
    after_long_help = START_HERE,
    arg_required_else_help = true,
    subcommand_required = true,
    disable_help_subcommand = true
)]
pub struct Args {
    /// Agent data directory. Overrides K3UP_DATA_DIR and the platform default
    #[arg(long, global = true, value_name = "DIR")]
    pub data_dir: Option<PathBuf>,
    /// Print the result as JSON, including errors
    #[arg(long, global = true)]
    pub json: bool,
    #[command(subcommand)]
    pub command: Action,
}

#[derive(Subcommand)]
pub enum Action {
    /// Register a program as a service or job
    #[command(
        display_order = 1,
        long_about = "\
Register a program as a service or job. The definition is stored by the agent; nothing runs
until you start it, pass --start, give it a schedule, or set --start-at-boot.

--exe takes an absolute or relative path, or a bare program name looked up on PATH (and
PATHEXT on Windows). The absolute path is stored. Arguments after -- are passed to the
program exactly as written; K3 Up never runs commands through a shell. To use shell
features, register the shell itself, such as `--exe sh -- -c 'cmd1 && cmd2'`.

Boolean flags take an optional value, so `--start-at-boot=false` is explicit. The
definition is validated locally before it is sent to the agent.",
        after_long_help = "\
Examples:
  k3up create web --exe node --cwd /srv/web --env PORT=8080 -- server.js
  k3up create web --exe /usr/local/bin/node --readiness-tcp 127.0.0.1:8080 \\
      --start-at-boot --start --wait -- server.js
  k3up create backup --exe /usr/local/bin/backup.sh --job --cron '0 0 2 * * *' \\
      --timezone Europe/Berlin --catch-up
  k3up create poller --exe python3 --job --every 300 --run-timeout 60 -- poll.py
  k3up create api --exe ./bin/api --depends-on database --restart always --max-restarts 10"
    )]
    Create {
        /// Name: 1 to 64 letters, digits, hyphens or underscores
        name: String,
        /// Program to run: a path, or a name found on PATH
        #[arg(long, value_name = "PROGRAM")]
        exe: String,
        #[command(flatten)]
        flags: WorkloadFlags,
        /// Start the workload after creating it
        #[arg(long, help_heading = "Start")]
        start: bool,
        /// With --start, wait until a service is running or a job has completed
        #[arg(long, requires = "start", help_heading = "Start")]
        wait: bool,
        /// Seconds to wait with --wait
        #[arg(
            long,
            default_value_t = 60,
            value_name = "SECS",
            help_heading = "Start"
        )]
        timeout: u64,
        /// Arguments passed to the program exactly as written
        #[arg(last = true, value_name = "ARGS")]
        args: Vec<String>,
    },
    /// Change parts of a definition
    #[command(
        display_order = 2,
        long_about = "\
Change parts of a definition. Only the flags you give change; everything else is kept.
The result is validated locally and then saved by the agent.

--env adds or replaces one variable; --unset-env removes one; --clear-env removes all.
--depends-on replaces the whole dependency list. Arguments after -- replace the whole
argument list. Boolean flags take an optional value, so `--start-at-boot=false` turns a
setting off. The name cannot be changed: create a new workload instead.

The agent refuses to change a workload that is running or wanted running. Stop it first,
or pass --restart-running to stop it, apply the change and start it again.",
        after_long_help = "\
Examples:
  k3up edit web --env PORT=9090 --restart-running
  k3up edit web --description 'Public API' --max-restarts 10 --stop-timeout 10
  k3up edit web --unset-env DEBUG --clear-readiness
  k3up edit backup --cron '0 30 3 * * *' --timezone UTC
  k3up edit backup --clear-schedule
  k3up edit web --exe /opt/node/bin/node -- server.js --port 9090
  k3up edit web --start-at-boot=false --job=false"
    )]
    Edit {
        /// Workload to change
        name: String,
        /// Program to run: a path, or a name found on PATH
        #[arg(long, value_name = "PROGRAM", help_heading = "Definition")]
        exe: Option<String>,
        #[command(flatten)]
        flags: WorkloadFlags,
        /// Remove every environment variable
        #[arg(long, help_heading = "Clearing")]
        clear_env: bool,
        /// Remove one environment variable, repeatable
        #[arg(long, value_name = "KEY", help_heading = "Clearing")]
        unset_env: Vec<String>,
        /// Remove every dependency
        #[arg(long, help_heading = "Clearing")]
        clear_depends_on: bool,
        /// Remove the schedule
        #[arg(long, help_heading = "Clearing")]
        clear_schedule: bool,
        /// Remove the TCP readiness check
        #[arg(long, help_heading = "Clearing")]
        clear_readiness: bool,
        /// Remove the run timeout
        #[arg(long, help_heading = "Clearing")]
        clear_run_timeout: bool,
        /// Remove every program argument
        #[arg(long, help_heading = "Clearing")]
        clear_args: bool,
        /// If the workload is running, stop it, apply the change and start it again
        #[arg(long)]
        restart_running: bool,
        /// New arguments, replacing the existing ones
        #[arg(last = true, value_name = "ARGS")]
        args: Vec<String>,
    },
    /// Print a definition as TOML
    #[command(
        display_order = 3,
        long_about = "\
Print a definition as a one-workload TOML manifest, ready to save and apply. With --json,
print the workload's status object, which includes the definition under \"workload\".",
        after_long_help = "\
Examples:
  k3up show web
  k3up show web > web.toml
  k3up show web --json | jq .workload.environment"
    )]
    Show {
        /// Workload to print
        name: String,
    },
    /// List every workload with its state
    #[command(
        display_order = 4,
        after_long_help = "\
Examples:
  k3up list
  k3up list --json | jq -r '.workloads[] | select(.state == \"failed\") | .workload.name'"
    )]
    List,
    /// Show one workload's state, process and last reason
    #[command(
        display_order = 5,
        long_about = "\
Show one workload's state, process, restart count, next scheduled run, last exit code and
the reason for its current state. States: stopped, pending, blocked, starting, running,
backoff, completed, failed.",
        after_long_help = "\
Examples:
  k3up status web
  k3up status web --json | jq .workloads[0].state"
    )]
    Status {
        /// Workload to inspect
        name: String,
    },
    /// Start a workload, and its dependencies first
    #[command(
        display_order = 6,
        long_about = "\
Start a workload. Dependencies that are not ready are started first, and the workload waits
for them. Starting a running workload does nothing. With --wait, return once a service is
running (after its TCP check, if it has one) or a job has exited with code 0, and exit with
1 if it fails or stops before then.",
        after_long_help = "\
Examples:
  k3up start web
  k3up start web --wait --timeout 120
  k3up start migrate --wait"
    )]
    Start {
        /// Workload to start
        name: String,
        /// Wait until a service is running or a job has completed
        #[arg(long)]
        wait: bool,
        /// Seconds to wait with --wait
        #[arg(long, default_value_t = 60, value_name = "SECS")]
        timeout: u64,
    },
    /// Stop a workload and keep it stopped
    #[command(
        display_order = 7,
        long_about = "\
Stop a workload. The agent sends a termination request, waits for the stop timeout, then
kills the process. The stop is remembered: the workload stays stopped after an agent restart
until you start it again or its schedule runs. Workloads that depend on it keep running.",
        after_long_help = "\
Examples:
  k3up stop web"
    )]
    Stop {
        /// Workload to stop
        name: String,
    },
    /// Stop and start a workload
    #[command(
        display_order = 8,
        after_long_help = "\
Examples:
  k3up restart web
  k3up restart web --wait"
    )]
    Restart {
        /// Workload to restart
        name: String,
        /// Wait until a service is running or a job has completed
        #[arg(long)]
        wait: bool,
        /// Seconds to wait with --wait
        #[arg(long, default_value_t = 60, value_name = "SECS")]
        timeout: u64,
    },
    /// Delete a definition; its log file is kept
    #[command(
        display_order = 9,
        long_about = "\
Delete a definition. The workload must be stopped, unless --stop is given, and nothing may
depend on it. Its log file and activity history are kept.",
        after_long_help = "\
Examples:
  k3up remove web --stop"
    )]
    Remove {
        /// Workload to delete
        name: String,
        /// Stop the workload first
        #[arg(long)]
        stop: bool,
    },
    /// Print a workload's output
    #[command(
        display_order = 10,
        long_about = "\
Print the last lines of a workload's log, which holds its standard output and error. Logs
rotate at 5 MB. With --follow, keep printing new output until Ctrl-C; with --json as well,
each batch is printed as one JSON object per line.",
        after_long_help = "\
Examples:
  k3up logs web
  k3up logs web --lines 500
  k3up logs web --follow"
    )]
    Logs {
        /// Workload whose log to read
        name: String,
        /// Lines to print, up to 2000
        #[arg(long, default_value_t = 100, value_name = "N")]
        lines: usize,
        /// Keep printing new output
        #[arg(long)]
        follow: bool,
    },
    /// Show the activity history, newest first
    #[command(
        display_order = 11,
        long_about = "\
Show the activity history: every start, stop, exit, restart decision and schedule action,
with its reason. Without a name, the history of every workload and of the agent itself.",
        after_long_help = "\
Examples:
  k3up events
  k3up events web --limit 20
  k3up events web --json | jq -r '.events[] | \"\\(.at) \\(.message)\"'"
    )]
    Events {
        /// Workload to show; omit for all
        name: Option<String>,
        /// Newest events to show, up to 200
        #[arg(long, default_value_t = 200, value_name = "N")]
        limit: usize,
    },
    /// Set or clear a workload's schedule
    #[command(
        display_order = 12,
        long_about = "\
Replace a workload's schedule, or remove it with --clear. Use --every for an interval or
--cron for a cron expression. Cron expressions have 6 or 7 fields starting with seconds:
`0 0 2 * * *` is 02:00 every day, and an optional seventh field is the year.

A scheduled start is skipped while the workload is running; a scheduled restart stops it
first. Jobs support --schedule-action start only. Stop the workload before changing its
schedule, or use `k3up edit --restart-running`.",
        after_long_help = "\
Examples:
  k3up schedule backup --cron '0 0 2 * * *' --timezone Europe/Berlin
  k3up schedule backup --every 3600 --catch-up
  k3up schedule web --cron '0 0 4 * * 1' --schedule-action restart
  k3up schedule backup --clear"
    )]
    Schedule {
        /// Workload to schedule
        name: String,
        #[command(flatten)]
        schedule: ScheduleFlags,
        /// Remove the schedule
        #[arg(long, conflicts_with_all = ["every", "cron"])]
        clear: bool,
        #[arg(long, hide = true, conflicts_with = "schedule_action")]
        restart: bool,
    },
    /// Check a TOML manifest without contacting the agent
    #[command(
        display_order = 20,
        long_about = "\
Check a TOML manifest: field values, dependency order and that every schedule has a future
run. Prints the order in which the workloads would start.",
        after_long_help = "\
Examples:
  k3up validate workloads.toml"
    )]
    Validate {
        /// Manifest to check
        file: PathBuf,
    },
    /// Create or update the workloads in a TOML manifest
    #[command(
        display_order = 21,
        long_about = "\
Create or update the workloads in a manifest as one transaction: either every change is
saved or none is. Workloads not in the file are left alone; apply never deletes. A workload
that is running or wanted running must be stopped before its definition can change.
Unchanged workloads are skipped, so applying the same file twice is safe.",
        after_long_help = "\
Examples:
  k3up apply workloads.toml --dry-run
  k3up apply workloads.toml"
    )]
    Apply {
        /// Manifest to apply
        file: PathBuf,
        /// Show what would change without saving
        #[arg(long)]
        dry_run: bool,
    },
    /// Save every definition as a TOML manifest
    #[command(
        display_order = 22,
        after_long_help = "\
Examples:
  k3up export
  k3up export --output workloads.toml"
    )]
    Export {
        /// File to write; prints to standard output when omitted
        #[arg(long, value_name = "FILE")]
        output: Option<PathBuf>,
    },
    /// Print a commented manifest showing every field
    #[command(
        display_order = 23,
        long_about = "\
Print a complete TOML manifest with a service and a scheduled job, showing every field with
its allowed values and defaults. It is valid as printed, so copy it, edit the paths and
apply it.",
        after_long_help = "\
Examples:
  k3up template > workloads.toml
  k3up template | grep -v '^#'"
    )]
    Template,
    /// Resource use of the machine and of each running workload
    #[command(
        display_order = 30,
        long_about = "\
Show machine CPU, memory, swap, disk and load, what K3 Up costs, and the processes, CPU,
memory, disk I/O and log size of each running workload. With a name, show that workload in
detail, including the lowest, average and highest CPU and memory over the last samples.

CPU is a share of the whole machine, so 100% means every core is busy. Each workload's
figures include its child processes. The agent samples every 10 seconds, or every 2 seconds
while someone is watching. With --watch, redraw every N seconds until Ctrl-C; with --json
as well, print one JSON document per line.",
        after_long_help = "\
Examples:
  k3up stats
  k3up stats web
  k3up stats --watch 2
  k3up stats web --json | jq '.metrics.workloads[0].usage'"
    )]
    Stats {
        /// Workload to show in detail; omit for the overview
        name: Option<String>,
        /// Redraw every N seconds until Ctrl-C
        #[arg(long, value_name = "SECS")]
        watch: Option<u64>,
    },
    /// Overall verdict: machine vitals, warnings and workloads needing attention
    #[command(
        display_order = 31,
        long_about = "\
Report machine vitals, warnings and the workloads that need attention.

Warnings use the same thresholds as the desktop app: memory above 85%, swap more than half
used, less than 10% of the data volume free, or CPU at 90% or more for the whole of the last
60 seconds. Workloads in the failed, backoff or blocked state need attention, and are listed
with their reason and last exit code.

The status is \"ok\" when there are no warnings and nothing needs attention, and \"warning\"
otherwise. With --strict, exit with 1 unless the status is ok, for monitoring scripts.",
        after_long_help = "\
Examples:
  k3up health
  k3up health --strict
  k3up health --json | jq '{status, warnings, attention}'"
    )]
    Health {
        /// Exit with 1 unless the status is ok
        #[arg(long)]
        strict: bool,
    },
    /// Install, start, stop and inspect the agent
    #[command(display_order = 40, subcommand)]
    Agent(AgentCommand),
    /// Print a shell completion script
    #[command(
        display_order = 50,
        long_about = "\
Print a completion script for a shell. Save it where the shell loads completions from, or
source it from the shell's startup file.",
        after_long_help = "\
Examples:
  k3up completions bash > /etc/bash_completion.d/k3up
  k3up completions zsh > \"${fpath[1]}/_k3up\"
  k3up completions fish > ~/.config/fish/completions/k3up.fish
  k3up completions powershell >> $PROFILE"
    )]
    Completions {
        /// Shell to generate for
        shell: Shell,
    },
    /// Write native systemd units for a manifest
    #[command(
        display_order = 51,
        long_about = "\
Write native systemd services and timers for the workloads in a manifest. The units run
under systemd without the K3 Up agent. Interval schedules with action start and missed skip
are supported; cron schedules, scheduled restarts, catch-up and TCP checks are rejected
rather than silently dropped.",
        after_long_help = "\
Examples:
  k3up systemd-export workloads.toml --output ./units"
    )]
    SystemdExport {
        /// Manifest to convert
        file: PathBuf,
        /// Directory to write the units to
        #[arg(long, value_name = "DIR")]
        output: PathBuf,
    },
    /// Install native systemd user units for a manifest (Linux)
    #[command(
        display_order = 52,
        long_about = "\
Install native systemd user units for a manifest into ~/.config/systemd/user, enable the
services marked start_at_boot and start the timers. Existing unit files are never
overwritten. If systemctl fails partway, the units written by this run are removed. Running
user units without a login session needs `loginctl enable-linger`.",
        after_long_help = "\
Examples:
  k3up systemd-install workloads.toml
  systemctl --user status k3up-web.service"
    )]
    SystemdInstall {
        /// Manifest to install
        file: PathBuf,
    },
    #[command(hide = true)]
    InstallAgent,
    #[command(hide = true)]
    UninstallAgent,
    #[command(hide = true, subcommand)]
    Path(PathCommand),
}

#[derive(Subcommand)]
pub enum AgentCommand {
    /// Register the agent to start at login, and start it now
    #[command(
        long_about = "\
Register the agent found beside k3up as a login item, start it, and wait until it answers.
The login item is a launchd agent on macOS, a systemd user service on Linux and a Run entry
on Windows. Running it again is safe: it refreshes the login item and leaves a running agent
alone. An earlier opt-out made in the desktop app is cleared, since you are asking for the
login item explicitly. The login item always manages the default data directory.",
        after_long_help = "\
Examples:
  k3up agent install
  k3up agent install && k3up agent status"
    )]
    Install,
    /// Remove the login item and stop the agent; data is kept
    #[command(
        long_about = "\
Remove the login item, ask the agent to stop, and wait until it has exited. Stopping the
agent stops every workload. The data directory, with its definitions, logs and history, is
kept. The desktop app will not register the login item again until you ask it to.",
        after_long_help = "\
Examples:
  k3up agent uninstall"
    )]
    Uninstall {
        /// Seconds to wait for the agent to exit
        #[arg(long, default_value_t = 120, value_name = "SECS")]
        timeout: u64,
    },
    /// Start the agent now without registering it
    #[command(
        long_about = "\
Start the agent found beside k3up and wait until it answers. With the default data
directory, the registered login item is used when there is one, so the agent is supervised.
With another --data-dir, the agent is launched directly. Does nothing if an agent is already
running for the directory.",
        after_long_help = "\
Examples:
  k3up agent start
  k3up agent start --data-dir /srv/k3up"
    )]
    Start,
    /// Stop the agent and every workload
    #[command(
        long_about = "\
Ask the agent to stop and wait until it has exited. It stops every workload in reverse
dependency order first. A stopped agent is not restarted by launchd or systemd; start it
again with `k3up agent start`, or it starts at the next login.",
        after_long_help = "\
Examples:
  k3up agent stop
  k3up agent stop --timeout 300"
    )]
    Stop {
        /// Seconds to wait for the agent to exit
        #[arg(long, default_value_t = 120, value_name = "SECS")]
        timeout: u64,
    },
    /// Whether the agent is reachable, and what it is running
    #[command(
        long_about = "\
Show whether the agent answers, its version, process, uptime, data directory and executable,
whether the login item is registered, and how many workloads are defined, running and in
need of attention. Exits with 1 when the agent is not reachable.",
        after_long_help = "\
Examples:
  k3up agent status
  k3up agent status --json | jq .reachable"
    )]
    Status,
    /// Install the agent as a Windows machine service
    #[command(
        long_about = "\
Install the agent as a Windows service running as LocalSystem, for every user on the
machine. Copies k3up-agent.exe from beside k3up.exe into Program Files and keeps its data in
%ProgramData%\\K3 Up. Run from an elevated terminal, then `sc.exe start K3Up`. Clients need
an elevated terminal and --data-dir pointing at that data directory.",
        after_long_help = "\
Examples:
  k3up agent install-service
  sc.exe start K3Up
  k3up --data-dir \"$env:ProgramData\\K3 Up\" list"
    )]
    InstallService,
    /// Stop and remove the Windows machine service
    #[command(
        long_about = "\
Stop the Windows service and remove its registration. The Program Files and ProgramData
folders are left in place for review.",
        after_long_help = "\
Examples:
  k3up agent uninstall-service"
    )]
    UninstallService,
}

#[derive(Subcommand)]
pub enum PathCommand {
    /// Add a folder to the user's PATH (Windows)
    Add { dir: PathBuf },
    /// Remove a folder from the user's PATH (Windows)
    Remove { dir: PathBuf },
}

#[derive(ClapArgs, Clone, Default)]
pub struct WorkloadFlags {
    /// Working directory. create uses the current directory when omitted
    #[arg(long, value_name = "DIR", help_heading = "Definition")]
    pub cwd: Option<PathBuf>,
    /// Short description
    #[arg(long, value_name = "TEXT", help_heading = "Definition")]
    pub description: Option<String>,
    /// Run as a job, which exits when done, instead of a service
    #[arg(
        long,
        num_args = 0..=1,
        default_missing_value = "true",
        value_name = "BOOL",
        help_heading = "Definition"
    )]
    pub job: Option<bool>,
    /// Start when the agent starts, including after a reboot
    #[arg(
        long,
        alias = "boot",
        num_args = 0..=1,
        default_missing_value = "true",
        value_name = "BOOL",
        help_heading = "Definition"
    )]
    pub start_at_boot: Option<bool>,
    /// Environment variable, repeatable
    #[arg(long, value_name = "KEY=VALUE", value_parser = parse_env, help_heading = "Definition")]
    pub env: Vec<(String, String)>,
    /// Workload that must be ready first, repeatable
    #[arg(long, value_name = "NAME", help_heading = "Definition")]
    pub depends_on: Vec<String>,
    /// IP:port that must accept connections before a service counts as running
    #[arg(long, value_name = "HOST:PORT", help_heading = "Definition")]
    pub readiness_tcp: Option<String>,
    /// When to restart a service that exits [default: on-failure]
    #[arg(long, value_enum, value_name = "POLICY", help_heading = "Recovery")]
    pub restart: Option<RestartArg>,
    #[arg(long, hide = true, conflicts_with = "restart")]
    pub never_restart: bool,
    /// Restarts allowed before giving up, 0 to 100 [default: 5]
    #[arg(long, value_name = "N", help_heading = "Recovery")]
    pub max_restarts: Option<u32>,
    /// Delay before the first restart, doubling each time, 1 to 300 [default: 2]
    #[arg(long, value_name = "SECS", help_heading = "Recovery")]
    pub restart_delay: Option<u64>,
    /// Time allowed for a graceful stop before the process is killed, 1 to 30 [default: 5]
    #[arg(long, value_name = "SECS", help_heading = "Recovery")]
    pub stop_timeout: Option<u64>,
    /// Longest run allowed; the process is stopped with exit code 124 after it
    #[arg(long, value_name = "SECS", help_heading = "Recovery")]
    pub run_timeout: Option<u64>,
    /// Time allowed for the TCP check to pass, 1 to 300 [default: 30]
    #[arg(long, value_name = "SECS", help_heading = "Recovery")]
    pub startup_timeout: Option<u64>,
    #[command(flatten)]
    pub schedule: ScheduleFlags,
}

#[derive(ClapArgs, Clone, Default)]
pub struct ScheduleFlags {
    /// Run every N seconds, 1 to 31536000
    #[arg(
        long,
        value_name = "SECS",
        conflicts_with = "cron",
        help_heading = "Schedule"
    )]
    pub every: Option<u64>,
    /// Cron expression with 6 or 7 fields, starting with seconds
    #[arg(long, value_name = "EXPR", help_heading = "Schedule")]
    pub cron: Option<String>,
    /// IANA timezone for the cron expression [default: UTC]
    #[arg(long, value_name = "TZ", help_heading = "Schedule")]
    pub timezone: Option<String>,
    /// What a scheduled run does; jobs support start only [default: start]
    #[arg(long, value_enum, value_name = "ACTION", help_heading = "Schedule")]
    pub schedule_action: Option<ActionArg>,
    /// Run once to catch up a run missed while the agent was down
    #[arg(
        long,
        num_args = 0..=1,
        default_missing_value = "true",
        value_name = "BOOL",
        help_heading = "Schedule"
    )]
    pub catch_up: Option<bool>,
}

impl ScheduleFlags {
    pub fn is_empty(&self) -> bool {
        self.every.is_none()
            && self.cron.is_none()
            && self.timezone.is_none()
            && self.schedule_action.is_none()
            && self.catch_up.is_none()
    }
}

#[derive(ValueEnum, Clone, Copy, Debug, PartialEq, Eq)]
pub enum RestartArg {
    Never,
    OnFailure,
    Always,
}

impl From<RestartArg> for Restart {
    fn from(value: RestartArg) -> Self {
        match value {
            RestartArg::Never => Restart::Never,
            RestartArg::OnFailure => Restart::OnFailure,
            RestartArg::Always => Restart::Always,
        }
    }
}

#[derive(ValueEnum, Clone, Copy, Debug, PartialEq, Eq)]
pub enum ActionArg {
    Start,
    Restart,
}

impl From<ActionArg> for ScheduleAction {
    fn from(value: ActionArg) -> Self {
        match value {
            ActionArg::Start => ScheduleAction::Start,
            ActionArg::Restart => ScheduleAction::Restart,
        }
    }
}

fn parse_env(value: &str) -> Result<(String, String), String> {
    match value.split_once('=') {
        Some((key, value)) if !key.is_empty() => Ok((key.to_string(), value.to_string())),
        _ => Err("expected KEY=VALUE".to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn command_tree_is_consistent() {
        use clap::CommandFactory;
        Args::command().debug_assert();
    }

    #[test]
    fn boolean_flags_take_optional_values() {
        let args = Args::try_parse_from(["k3up", "edit", "web", "--start-at-boot=false", "--job"])
            .unwrap();
        let Action::Edit { flags, .. } = args.command else {
            panic!("expected edit");
        };
        assert_eq!(flags.start_at_boot, Some(false));
        assert_eq!(flags.job, Some(true));
    }

    #[test]
    fn hidden_aliases_still_parse() {
        let args = Args::try_parse_from([
            "k3up",
            "create",
            "web",
            "--exe",
            "node",
            "--boot",
            "--never-restart",
            "--",
            "server.js",
        ])
        .unwrap();
        let Action::Create { flags, args, .. } = args.command else {
            panic!("expected create");
        };
        assert_eq!(flags.start_at_boot, Some(true));
        assert!(flags.never_restart);
        assert_eq!(args, ["server.js"]);
        assert!(Args::try_parse_from(["k3up", "install-agent"]).is_ok());
    }

    #[test]
    fn env_values_must_have_a_key() {
        assert!(parse_env("=x").is_err());
        assert_eq!(
            parse_env("A=b=c").unwrap(),
            ("A".to_string(), "b=c".to_string())
        );
    }
}
