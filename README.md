<p align="center">
  <img src="assets/branding/k3-up-logo.png" alt="K3 Up logo" width="420">
</p>

# K3 Up

**Keep any application running as a background service on macOS, Linux and Windows.**

K3 Up is an open-source service manager and process supervisor. Register a program once, and a small agent starts it, restarts it when it crashes, runs it on a schedule, and brings it back after a reboot. A desktop app and a command-line tool manage the same agent, and closing either one leaves your applications running.

K3 Up is written in Rust. It has no browser runtime, no .NET, no listening network port and no telemetry.

## Use cases

- Run a Node.js, Python, Java, Go or .NET app as a Windows service, without writing a service wrapper.
- Keep a script or server running after you log out or reboot, and restart it automatically when it crashes.
- Run commands on a schedule, like cron or Task Scheduler, with a desktop app and a history of every run.
- Start a stack of services in order, waiting for a database port before starting the app that needs it.
- See how much CPU, memory and disk each background process uses, and how your machine is holding up.
- Manage the same workloads on macOS, Linux and Windows with one tool and one configuration format.

## How it compares

| Tool | Platforms | What it is |
|---|---|---|
| systemd | Linux | The Linux service manager, configured with unit files. K3 Up can export native systemd units. |
| launchd | macOS | The macOS service manager, configured with property lists. |
| NSSM | Windows | A wrapper that runs any program as a Windows service. |
| supervisord | Linux, macOS | A Python process supervisor configured with INI files. |
| PM2 | macOS, Linux, Windows | A process manager built on Node.js. |
| **K3 Up** | macOS, Linux, Windows | A single native binary with a desktop app, CLI, schedules, dependencies and resource monitoring. |

## Features

- Run long-lived **services** and one-off or scheduled **jobs**, with literal arguments, a working directory and environment variables.
- Restart on failure or always, with exponential backoff, a restart budget, run timeouts and stop timeouts.
- Interval and cron schedules in any IANA timezone, with a choice of skipping or catching up missed runs.
- Start workloads in dependency order, optionally waiting until a TCP port accepts connections.
- Remember whether you stopped a workload on purpose, so recovery never undoes a stop.
- Per-workload logs with rotation, and an activity history of every lifecycle decision and its reason.
- A health view of machine CPU, memory, swap and disk, and of the CPU, memory, disk I/O and log size of every workload and of K3 Up itself.
- Define workloads in the app, with CLI flags, or in a versioned TOML manifest you can validate, preview and apply as one transaction.
- Export workloads as native systemd services and timers on Linux.
- Run the agent as a Windows service, with each workload's processes contained in a Job Object.

## Platforms

| | Agent | Desktop app | Start at login |
|---|---|---|---|
| macOS | Yes | Yes | launchd agent |
| Linux | Yes | Yes (X11 or Wayland) | systemd user service |
| Windows | Yes, per user or as a machine service | Yes | Run entry, or the Windows service |

macOS is the most tested platform. The Linux and Windows versions are newer and have seen less real-world use, so please report what you find.

## Download

Prebuilt downloads are attached to each release on the GitHub Releases page:

| System | Download | Contents |
|---|---|---|
| Windows (x64) | `k3up-setup-x86_64.exe` | Installer |
| Windows (x64) | `k3up-windows-x86_64.zip` | `k3up.exe`, `k3up-agent.exe`, `k3up-desktop.exe`, for portable use |
| macOS (Apple silicon) | `k3up-macos-arm64.zip` | `K3 Up.app`, with the agent and CLI inside |
| Linux (x86-64) | `k3up-linux-x86_64.tar.gz` | `k3up`, `k3up-agent`, `k3up-desktop` |

The builds are not code-signed yet, so each system asks you to confirm the first launch:
- **Windows:** run `k3up-setup-x86_64.exe` and choose **More info → Run anyway** when SmartScreen appears. Setup installs for your account only, so it needs no administrator rights. It adds K3 Up to the Start menu, starts the agent, and sets it to start at login. Remove K3 Up from **Settings → Apps**; it asks whether to keep your workloads and logs.
- **macOS:** move `K3 Up.app` to Applications first, then right-click it and choose **Open**. The app refuses to set up its login item when run from the download location.
- **Linux:** extract the archive somewhere permanent and run `./k3up-desktop`.

With Rust installed, you can also install from [crates.io](https://crates.io/crates/k3up):

```sh
cargo install k3up --features desktop
```

Leave out `--features desktop` on a server to install only the agent and the command-line tool.

## Install from source

You need Rust 1.88 or newer and a C compiler, which builds the bundled SQLite.

```sh
cargo build --locked --release --features desktop
```

This produces three programs in `target/release`:

| Program | Purpose |
|---|---|
| `k3up-desktop` | The desktop app |
| `k3up-agent` | The background agent that runs your workloads |
| `k3up` | The command-line tool |

For a headless machine, leave out the desktop app and its GUI dependencies:

```sh
cargo build --locked --release
```

On Linux, the desktop app needs your distribution's X11, Wayland and xkbcommon development packages. `.github/workflows/ci.yml` lists the Ubuntu package names.

To build a macOS app bundle at `target/K3 Up.app`, with the agent and CLI inside:

```sh
packaging/macos/bundle.sh
```

To build the Windows installer, install [NSIS](https://nsis.sourceforge.io) and run this after a Windows release build. On macOS and Linux, `makensis` needs a UTF-8 locale such as `LANG=en_US.UTF-8`.

```sh
makensis -DVERSION=0.1.0 -DBINARIES=../../target/release packaging/windows/installer.nsi
```

## Getting started

Open the desktop app:

```sh
./target/release/k3up-desktop
```

On first launch the app finds the agent next to its own executable, registers it to start at login, and starts it. You don't run the agent yourself.

| System | Login item | If the agent crashes |
|---|---|---|
| macOS | `~/Library/LaunchAgents/com.k3.up.agent.plist` | launchd restarts it |
| Linux | systemd user service `k3up-agent.service` | systemd restarts it |
| Windows | `K3 Up` value in `HKCU\Software\Microsoft\Windows\CurrentVersion\Run` | the app starts it next time it opens |

macOS shows a "Background Items Added" notice the first time. The **Start at login** switch in the connection panel removes or restores the login item without stopping the running agent. To open the panel, click the connection status at the bottom of the sidebar. You can also use it to connect to a different agent.

Choose **New** to register a workload. The form covers every setting; **TOML** in its header switches to the raw definition.

### Data directory

The agent keeps its database, logs and socket in one directory:

| System | Default |
|---|---|
| macOS and Linux | `~/.local/share/k3up` |
| Windows | `%LOCALAPPDATA%\K3 Up` |

Set `K3UP_DATA_DIR`, or pass `--data-dir` to every program, to use another directory. Automatic agent setup applies only to the default directory. With a custom directory, start the agent yourself:

```sh
k3up-agent --data-dir ~/k3up-test
k3up-desktop --data-dir ~/k3up-test
```

On macOS and Linux the socket path must fit the operating system's limit of about 100 bytes, so keep custom data directories short. The agent reports a clear error when a path is too long.

### Headless install

On a server, or anywhere without the desktop app, the command line installs the agent. `cargo install k3up` puts `k3up` and `k3up-agent` side by side, which is what `agent install` needs.

```sh
k3up agent install
k3up agent status
```

- **Linux:** the login item is a systemd user service, which normally runs only while you are logged in. To keep the agent running without a login session, an administrator enables lingering once: `sudo loginctl enable-linger $USER`.
- **macOS:** the login item is a launchd agent in `~/Library/LaunchAgents`, which runs while you are logged in. The first `agent install` shows a "Background Items Added" notice.
- **Windows:** `agent install` writes a Run entry for your account and starts the agent. For a service that runs for every user without a login, use `agent install-service` from an elevated terminal instead, see [Windows](#windows).

`agent uninstall` reverses the install and keeps your data.

## Command line

`k3up` manages the same agent as the desktop app, and covers everything the app does: installing the agent, defining workloads, starting and stopping them, and reading health. Every command has `--help` with an explanation and examples, and `k3up --help` covers the concepts.

```sh
k3up agent install
k3up create worker --exe /usr/local/bin/worker --cwd /srv/worker -- --port 8080
k3up start worker --wait --timeout 30
k3up list
k3up status worker --json
k3up logs worker --follow
k3up health
k3up stop worker
k3up remove worker
```

Arguments after `--` are passed to the program exactly as written. K3 Up never runs commands through a shell. To use shell features, register the shell itself, such as `--exe sh -- -c "cmd1 && cmd2"` or `--exe cmd.exe -- /c ...`.

### Command-line reference

Global flags: `--data-dir DIR` selects the agent's data directory and `--json` prints the result as JSON. Boolean workload flags take an optional value, so `--start-at-boot=false` turns a setting off in `edit`.

**Workloads**

| Command | What it does |
|---|---|
| `create NAME --exe PROGRAM [flags] [-- ARGS...]` | Register a program. `--exe` is a path or a bare name found on PATH (and PATHEXT on Windows); the absolute path is stored. `--start` starts it, `--wait` waits like `start --wait`. |
| `edit NAME [flags] [-- ARGS...]` | Change only the flags given. `--env` adds or replaces a variable, `--unset-env KEY` removes one, `--depends-on` and `-- ARGS` replace their lists. `--clear-env`, `--clear-depends-on`, `--clear-schedule`, `--clear-readiness`, `--clear-run-timeout` and `--clear-args` remove settings. A running workload needs `--restart-running`, which stops it, applies the change and starts it again. |
| `show NAME` | The definition as a one-workload TOML manifest. With `--json`, the status object. |
| `list` | Every workload with its state, process and reason. |
| `status NAME` | One workload's state, process, restarts, next run, last exit and reason. |
| `start NAME [--wait] [--timeout SECS]` | Start a workload and its dependencies. `--wait` returns once a service is running or a job has exited with 0. |
| `stop NAME` | Stop a workload and remember the stop across agent restarts. |
| `restart NAME [--wait] [--timeout SECS]` | Stop and start. |
| `remove NAME [--stop]` | Delete the definition; the log file is kept. |
| `logs NAME [--lines N] [--follow]` | The last lines of the log, or a live tail. |
| `events [NAME] [--limit N]` | The activity history, newest first, up to 200. |
| `schedule NAME --every SECS or --cron EXPR [--timezone TZ] [--schedule-action start or restart] [--catch-up]` | Replace the schedule. `--clear` removes it. |

Workload flags, shared by `create` and `edit`: `--cwd DIR`, `--description TEXT`, `--job`, `--start-at-boot`, `--env KEY=VALUE`, `--depends-on NAME`, `--readiness-tcp HOST:PORT`, `--restart never|on-failure|always`, `--max-restarts N`, `--restart-delay SECS`, `--stop-timeout SECS`, `--run-timeout SECS`, `--startup-timeout SECS`, `--every SECS`, `--cron EXPR`, `--timezone TZ`, `--schedule-action start|restart`, `--catch-up`. `k3up create --help` lists the ranges and defaults.

**Manifests**

| Command | What it does |
|---|---|
| `validate FILE` | Check a manifest without contacting the agent, and print the startup order. |
| `apply FILE [--dry-run]` | Create or update the workloads in the file as one transaction. Never deletes. Running workloads must be stopped before their definition changes. |
| `export [--output FILE]` | Every definition as a manifest. |
| `template` | A complete, commented manifest showing every field, valid as printed. |

**Health**

| Command | What it does |
|---|---|
| `stats [NAME] [--watch SECS]` | Machine vitals and the resources of each running workload. With a name, one workload in detail with min, average and max CPU and memory. `--watch` redraws every N seconds. |
| `health [--strict]` | The overall verdict: vitals, warnings and workloads needing attention. `--strict` exits with 1 unless the status is ok. |

**Agent**

| Command | What it does |
|---|---|
| `agent install` | Register the agent found beside `k3up` as a login item, start it and wait until it answers. Safe to repeat. Clears an opt-out made in the app. |
| `agent uninstall` | Remove the login item, stop the agent and wait for it to exit. Data is kept. |
| `agent start` | Start the agent now without registering it. |
| `agent stop` | Stop the agent and every workload, and wait for it to exit. |
| `agent status` | Reachability, version, process, uptime, data directory, executable, login item and workload counts. Exits with 1 when unreachable. |
| `agent install-service`, `agent uninstall-service` | The Windows machine service, see [Windows](#windows). |

**Other**

| Command | What it does |
|---|---|
| `completions SHELL` | A completion script for bash, zsh, fish, PowerShell or elvish. |
| `systemd-export FILE --output DIR`, `systemd-install FILE` | Native systemd units, see [Linux with systemd](#linux-with-systemd). |

## Automation and AI agents

`k3up` is built to be driven by scripts and by AI agents: every command is documented in `--help`, every result is available as JSON, and repeating a command never makes things worse.

### The JSON contract

With `--json`, most commands print the agent's response object, on success and on failure:

| Field | Meaning |
|---|---|
| `ok` | `true` on success. The exit code is 1 when it is `false`. |
| `message` | What happened, or the error. |
| `workloads` | Status objects, for `list`, `status`, `start`, `restart`, `stats NAME` and `create --start`. Each has `workload` (the definition), `state`, `desired_running`, `pid`, `restart_count`, `started_at`, `next_run`, `last_exit` and `reason`. |
| `events` | For `events`: `id`, `at`, `name` and `message`, newest first. |
| `text` | Log output, or the text view of `stats` and `template`. |
| `offset` | For `logs`: the byte offset to continue from. |
| `manifest` | For `export`: `version` and `workloads`. |
| `metrics` | For `stats`: `at`, `machine`, `agent`, `managed`, `workloads` and `data_bytes`. |
| `agent` | For `agent install` and `agent start`: `version`, `pid`, `started_at`, `data_dir` and `executable`. |

States are `stopped`, `pending`, `blocked`, `starting`, `running`, `backoff`, `completed` and `failed`. `failed`, `backoff` and `blocked` need attention.

Three commands print their own shape. `show --json` prints the status object itself. `health --json` prints:

```json
{
  "status": "ok",
  "warnings": [{ "kind": "memory", "message": "Memory 90% used, above 85%" }],
  "attention": [{ "name": "api", "state": "failed", "reason": "Process exited with code 1; restart limit reached", "last_exit": 1 }],
  "machine": { "host": "...", "os": "...", "cores": 8, "cpu": 12.5, "memory_total": 0, "memory_used": 0, "swap_total": 0, "swap_used": 0, "disk_total": 0, "disk_available": 0, "load": [1.0, 1.0, 1.0], "uptime_secs": 0 },
  "k3up": { "agent": { "cpu": 0.0, "memory": 0, "read_rate": 0, "write_rate": 0, "processes": 1 }, "workloads": { "cpu": 0.0, "memory": 0, "read_rate": 0, "write_rate": 0, "processes": 0 }, "data_bytes": 0, "workload_count": 0, "running": 0 }
}
```

`status` is `warning` when there is any warning or any workload needing attention. Warning kinds are `cpu`, `memory`, `swap` and `disk`.

`agent status --json` prints:

```json
{
  "reachable": true,
  "version": "0.1.0",
  "pid": 4242,
  "started_at": "2026-09-25T12:00:00Z",
  "uptime_secs": 3600,
  "data_dir": "/home/me/.local/share/k3up",
  "executable": "/usr/local/bin/k3up-agent",
  "login_item": true,
  "workloads": { "total": 3, "running": 2, "attention": 0 }
}
```

When the agent is not reachable it prints `{ "reachable": false, "error": "...", "data_dir": "...", "login_item": false }` and exits with 1.

### Exit codes

| Code | Meaning |
|---|---|
| 0 | Success. For `health --strict`, the status is ok. |
| 1 | The request failed, the workload failed or stopped while waiting, the agent is unreachable, or `health --strict` found a problem. |
| 2 | The command line was invalid. |

### What is safe to repeat

- `agent install`, `agent start`: leave a running agent alone and refresh the login item.
- `agent stop`, `agent uninstall`: report "not running" when there is nothing to stop.
- `apply`: skips unchanged workloads and reports "No changes".
- `start`: does nothing to a running workload. `stop`: does nothing to a stopped one.
- `edit` with values already in place: reports "No changes" without stopping anything.
- `create`: fails when the name exists. Use `apply` or `edit` to update.

### Recipes

A web service with environment variables and a TCP check, started now and at every login:

```sh
k3up create web --exe node --cwd /srv/web --env NODE_ENV=production --env PORT=8080 \
    --readiness-tcp 127.0.0.1:8080 --start-at-boot --start --wait -- server.js
```

A nightly job at 02:00 Berlin time, catching up one run if the machine was off:

```sh
k3up create backup --exe /usr/local/bin/backup.sh --cwd /srv --job \
    --cron '0 0 2 * * *' --timezone Europe/Berlin --catch-up --run-timeout 3600
```

Change a running workload:

```sh
k3up edit web --env PORT=9090 --readiness-tcp 127.0.0.1:9090 --restart-running
```

Find out why a workload failed:

```sh
k3up status web --json | jq '{state: .workloads[0].state, reason: .workloads[0].reason, exit: .workloads[0].last_exit}'
k3up events web --limit 20
k3up logs web --lines 200
```

A machine health check for monitoring:

```sh
k3up health --strict --json > /var/log/k3up-health.json || alert "K3 Up needs attention"
```

Only the agent and the command line on a server, without the desktop app:

```sh
cargo install k3up
k3up agent install
k3up agent status
```

## Configuration

```toml
version = 1

[[workloads]]
name = "worker"
description = "Background application worker"
executable = "/opt/myapp/worker"
args = ["--port", "8080"]
working_directory = "/opt/myapp"
kind = "service"
start_at_boot = true
depends_on = ["database"]
restart = "on_failure"
max_restarts = 5
restart_delay_secs = 2
stop_timeout_secs = 5
readiness_tcp = "127.0.0.1:8080"
startup_timeout_secs = 30

[workloads.environment]
APP_MODE = "production"
```

`kind` is `service` or `job`:
- Services keep running. `restart` can be `never`, `on_failure` or `always`, and restarts back off exponentially until `max_restarts` is reached.
- Jobs run to completion each time they are started or scheduled, and are never restarted.
- `run_timeout_secs` limits how long either kind may run.

`start_at_boot = true` starts a service when it is first registered. After that, the agent remembers whether you last started or stopped it, and restores that state whenever the agent starts.

### Dependencies

Workloads start in dependency order, and dependency cycles are rejected.
- A service dependency is ready when its process has started, or when its TCP check passes.
- A job dependency is ready when it has exited with code 0.
- If a dependency fails or is stopped before it is ready, the waiting workload is marked failed, with the reason.

Stopping a dependency does not stop workloads that are already running. The TCP check applies at startup only; it is not an ongoing health check.

### Schedules

```toml
[workloads.schedule]
cron = "0 0 2 * * *"
timezone = "Europe/Berlin"
action = "start"
missed = "skip"
```

Use either `every_secs` or `cron`. Cron expressions have 6 or 7 fields, starting with seconds; the optional seventh field is the year.

- `action = "start"` starts the workload unless it is already running. Jobs support only this action.
- `action = "restart"` stops and starts a service.
- `missed = "skip"` ignores runs missed while the agent was down.
- `missed = "run_once"` runs once to catch up after downtime.

A scheduled start overrides an earlier manual stop, so clear the schedule if you want a workload to stay stopped. The agent records the next run before starting the current one. A crash at the wrong moment can therefore skip a run, but never repeats one.

## Health and resource use

The desktop app's **Health** page, `k3up stats` and `k3up health` show:
- machine CPU, memory, swap, free space on the data volume, load average and uptime
- the share of the machine used by K3 Up: its workloads, the agent and the desktop app
- for each workload: process count, CPU, memory, disk read and write rates, log size and command, with CPU and memory sparklines in the app

CPU is a share of the whole machine, so 100% means every core is busy, and figures add up across workloads. Each workload's figures include all of its child processes. The app shows the last hour as charts, and turns a tile amber when memory is above 85%, swap is more than half used, free disk is below 10%, or CPU stays above 90% for a minute.

The agent samples every 10 seconds, or every 2 seconds while a client is viewing the data. Children that live for less than a second, such as commands launched in a tight shell loop, can fall between samples, and their activity is then undercounted.

## Performance

K3 Up is designed to cost almost nothing while nothing is happening.

The agent:
- sleeps until its next deadline, such as a schedule, a retry or a timeout, instead of polling on a timer
- wakes immediately when a workload exits, on macOS and Linux
- runs on one thread, plus one for resource sampling
- finds each workload's processes through the kernel's child lists instead of scanning every process on the machine

The desktop app:
- waits for the agent to report a change instead of polling it
- repaints only when something visible changes
- stops fetching logs and metrics while its window is in the background

Measured on an Apple silicon Mac with release builds, as a share of one CPU core:

| | CPU | Memory |
|---|---|---|
| Agent, idle, running 3 workloads | 0.0% | 4.5 MB |
| Agent, while the Health page is open | 0.2% | about 6 MB |
| Desktop app, idle | 0.0% | about 50 MB |
| Desktop app, window in the background | 0.0% | about 50 MB |
| Desktop app, Health page open | 2.4–3.8% | about 50 MB |

The desktop app uses a CPU renderer to keep its memory use low, so each repaint while you watch live data costs a few milliseconds of CPU.

## Platform notes

### macOS and Linux

Each workload runs in its own process group. To stop a workload, the agent sends SIGTERM to the group, waits for the stop timeout, then sends SIGKILL. When the agent shuts down, it stops workloads in reverse dependency order.

If the agent itself is killed, its workloads keep running. The agent records each workload's process ID and start time. The next agent stops those leftover processes before starting new copies, and the start-time check prevents it from ever signalling an unrelated process that reused the ID.

### Linux with systemd

To run workloads as native systemd units, independent of the K3 Up agent:

```sh
k3up systemd-export examples/workloads.toml --output ./units
k3up systemd-install examples/workloads.toml
systemctl --user status k3up-example-worker.service
```

`systemd-install` installs user units and refuses to overwrite existing files. If `systemctl` fails partway, it removes the units it wrote. Export supports interval schedules with `action = "start"` and `missed = "skip"`, and rejects cron schedules, scheduled restarts, catch-up and TCP checks rather than silently dropping them. Generated services require systemd with `Type=exec` support, such as RHEL 9 or newer. To run user units without an active login session, an administrator must enable lingering for the account.

### Windows

The installer and the desktop app run the agent under your own account. To run it instead as a machine-wide service, place `k3up.exe` and `k3up-agent.exe` in the same folder and run this from an elevated terminal:

```powershell
.\k3up.exe agent install-service
sc.exe start K3Up
.\k3up.exe --data-dir "$env:ProgramData\K3 Up" list
```

Installation copies the agent to `%ProgramFiles%\K3 Up` and keeps data in `%ProgramData%\K3 Up`. Both folders are created with access limited to SYSTEM and Administrators. If any step fails, the installer removes what it created. `.\k3up.exe agent uninstall-service` stops and removes the service; the folders are left in place for you to review.

The machine service runs as LocalSystem, and so do its workloads. Clients need an elevated terminal to reach it.

Each workload runs in a Job Object, so its processes end if the agent stops unexpectedly. Windows has no universal way to ask an arbitrary program to shut down gracefully. A stop therefore waits for the stop timeout and then ends the Job Object.

## Security

- The agent listens only on a local Unix socket or named pipe, never on a network port.
- On macOS and Linux, the socket and data directory are readable by their owner only.
- On Windows, the named pipe admits only SYSTEM, Administrators and the account that started the agent. Clients refuse a pipe created by any other account.

Anyone who can reach the agent can run programs under its account, so treat access to it accordingly.

Workload definitions, including environment variables, are stored in plain text in the agent's database. Don't put secrets in definitions or command-line arguments.

## Limitations

- Requests are handled one at a time, so a slow stop delays other requests until it finishes.
- There is no secret storage, per-workload user accounts, resource limits or ongoing health checks.
- There are no signed installers or in-place upgrades yet.
- The desktop app manages agents on the local machine only.

## Project layout

| Path | Contents |
|---|---|
| `src/model.rs` | Workload definitions, schedules and dependency validation |
| `src/engine.rs` | Lifecycle state, recovery, scheduling and supervision |
| `src/server.rs`, `src/client.rs`, `src/protocol.rs` | The local request protocol |
| `src/store.rs` | SQLite storage for definitions and activity |
| `src/metrics.rs` | Machine and workload resource sampling |
| `src/platform.rs` | Process containment and platform endpoints |
| `src/autostart.rs` | Starting the agent at login |
| `src/systemd.rs` | Native systemd unit generation |
| `src/windows_host.rs`, `src/win32.rs` | Windows service hosting and security |
| `src/health.rs` | Health thresholds shared by the command line and the app |
| `src/bin/agent.rs` | The `k3up-agent` program |
| `src/bin/k3up/` | The `k3up` command line, one module per command group |
| `src/bin/desktop/` | The desktop app |

## Development

```sh
cargo fmt --all -- --check
cargo clippy --locked --all-targets --features desktop -- -D warnings
cargo test --locked --features desktop
```

The tests launch real child processes to exercise starts, stops, restarts, dependencies, schedules, crash recovery and the CLI against a running agent.

`examples/demo_worker.rs` is a harmless program for trying things out:

```sh
cargo build --locked --release --example demo_worker
```

Register it with the argument `heartbeat` (prints a line every 5 seconds), `job` (exits successfully) or `fail` (exits with code 7).

Debug builds of the desktop app redraw slowly; use a release build for everyday use.

## Contributing

Bug reports, platform test results and pull requests are welcome. Before sending a change, run the checks under [Development](#development).

## License

K3 Up is released under the [MIT License](LICENSE).
