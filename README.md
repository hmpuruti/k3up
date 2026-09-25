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

## Command line

```sh
k3up create worker --exe /usr/local/bin/worker --cwd /srv/worker -- --port 8080
k3up start worker --wait --timeout 30
k3up list
k3up status worker --json
k3up logs worker --follow
k3up restart worker --wait
k3up stop worker
k3up remove worker
```

Arguments after `--` are passed to the program exactly as written. K3 Up never runs commands through a shell. To use shell features, register the shell itself, such as `/bin/sh -c "..."` or `cmd.exe /c ...`.

Manage workloads as a manifest:

```sh
k3up validate examples/workloads.toml
k3up apply examples/workloads.toml --dry-run
k3up apply examples/workloads.toml
k3up export --output saved-workloads.toml
```

`apply` creates or updates only the workloads in the file and never deletes the others. Stop a workload before changing its definition.

Manage schedules and history:

```sh
k3up schedule backup --cron '0 0 2 * * *' --timezone Europe/Berlin
k3up schedule backup --every 3600
k3up schedule backup --clear
k3up events backup
```

Every command accepts `--json` for structured output, including errors. Exit code 0 means success, 1 means the request failed, and 2 means the command line was invalid.

`start --wait` and `restart --wait` return once a service is running (and has passed its TCP check, if it has one), or once a job has exited with code 0. They exit with 1 if the workload fails or stops.

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

The desktop app's **Health** page and `k3up stats` show:
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
.\k3up.exe install-agent
sc.exe start K3Up
.\k3up.exe --data-dir "$env:ProgramData\K3 Up" list
```

Installation copies the agent to `%ProgramFiles%\K3 Up` and keeps data in `%ProgramData%\K3 Up`. Both folders are created with access limited to SYSTEM and Administrators. If any step fails, the installer removes what it created. `.\k3up.exe uninstall-agent` stops and removes the service; the folders are left in place for you to review.

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
| `src/bin/` | The `k3up` and `k3up-agent` programs |
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
