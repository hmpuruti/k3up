# AGENTS.md

K3 Up keeps programs running as background services and scheduled jobs. The `k3up` command line talks to a background agent, `k3up-agent`, which starts the programs, restarts them after a crash and runs them on schedules. The desktop app is optional.

## Using K3 Up

- Read the README section "Automation and AI agents" first. It has the JSON output shapes, exit codes, which commands are safe to repeat, and recipes. `k3up <command> --help` shows every flag with examples.
- Add `--json` to any command for machine-readable output. Exit code 0 is success, 1 is a failed request and 2 is an invalid command line.
- Four commands change the system outside the data directory: `agent install` and `agent uninstall` (a login item) and `agent install-service` and `agent uninstall-service` (a Windows service). Don't run them unless the user asked for that. Every other command only touches the data directory.
- To experiment without affecting a real installation, set `K3UP_DATA_DIR` to a scratch directory, then use `k3up agent start`. Afterwards, run `k3up agent stop` and delete the directory.
- A schedule is active as soon as it's set, without `--start`. Between runs the workload shows as `stopped`. `k3up list` shows the next run.
- To find out why a workload failed, run `k3up status NAME`, `k3up events NAME --limit 20` and `k3up logs NAME --lines 200`.

## Working on the code

- The README's "Project layout" table maps the source files.
- Before committing, all of these must pass:

  ```sh
  cargo fmt --all -- --check
  cargo clippy --locked --all-targets --features desktop -- -D warnings
  cargo test --locked --features desktop
  ```

- CI runs these on macOS, Windows and Linux, and checks that the code builds on Rust 1.88, the minimum in `Cargo.toml`. Code must build on all three platforms, so gate platform code with `cfg`, and use paths that are absolute on each platform in tests.
- `main` is protected. Every change goes through a pull request.
- Keep comments rare. Write them only to explain a non-obvious reason.
- Keep help text and docs plain and specific, with no marketing language and no em dashes.
