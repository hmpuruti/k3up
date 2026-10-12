//! Real Windows services created through the services back end and the k3up command line.
//! Creating services needs administrator rights; without them each test reports that it was
//! skipped, unless K3UP_REQUIRE_ELEVATION is set, as in CI, where skipping is a failure.
#![cfg(windows)]

use k3up::{
    model::{
        Kind, Manifest, Restart, RestartBackoff, RestartLimit, Schedule, State, Status, Workload,
    },
    protocol::{Command, Response},
    services::{Backend, Settings, exposure, files, integrity, scm, setup},
};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
    ptr,
    sync::atomic::{AtomicU32, Ordering},
    time::{Duration, Instant},
};

const HOST: &str = env!("CARGO_BIN_EXE_k3up-host");
const CLI: &str = env!("CARGO_BIN_EXE_k3up");
const WAIT: Duration = Duration::from_secs(60);

// The test executable doubles as the workload, so the tests need no shell.
#[test]
#[ignore = "child process fixture; run by the services tests"]
fn fixture() {
    use std::io::Write;
    match std::env::var("K3UP_FIXTURE").unwrap_or_default().as_str() {
        "exit" => std::process::exit(
            std::env::var("K3UP_EXIT_CODE")
                .ok()
                .and_then(|code| code.parse().ok())
                .unwrap_or(1),
        ),
        "pulse" => {
            println!("pid {}", std::process::id());
            loop {
                println!("tick");
                std::io::stdout().flush().unwrap();
                std::thread::sleep(Duration::from_millis(200));
            }
        }
        _ => panic!("Fixture requires K3UP_FIXTURE"),
    }
}

/// A temporary folder whose ancestors only administrators may move, as services mode requires:
/// inside ProgramData rather than a user profile. Users may read it, as they may read
/// ProgramData, so that they can check those ancestors too.
fn protected_temp_dir() -> tempfile::TempDir {
    let base = k3up::win32::program_data().unwrap().join("K3UpTests");
    if let Err(error) = k3up::win32::create_dir_with(&base, k3up::win32::SHARED_DIR_SDDL) {
        assert!(base.is_dir(), "{error:#}");
    }
    let dir = tempfile::tempdir_in(&base).unwrap();
    k3up::win32::set_owner_to_administrators(dir.path()).unwrap();
    dir
}

/// A data directory in services mode with its own service name prefix. Dropping it deletes
/// every service with that prefix, even after a failed assertion.
struct Machine {
    _dir: tempfile::TempDir,
    data: PathBuf,
    prefix: String,
    backend: Backend,
}

impl Machine {
    fn start() -> Option<Self> {
        if !k3up::win32::is_elevated() {
            assert!(
                std::env::var_os("K3UP_REQUIRE_ELEVATION").is_none(),
                "K3UP_REQUIRE_ELEVATION is set, but the tests are not elevated"
            );
            eprintln!("skipped: not elevated");
            return None;
        }
        static NEXT: AtomicU32 = AtomicU32::new(0);
        let dir = protected_temp_dir();
        let data = dir.path().join("data");
        setup::prepare(&data).unwrap();
        let prefix = format!(
            "K3UpTest{}n{}_",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::SeqCst)
        );
        integrity::ensure_key(&prefix, &data.join("workloads")).unwrap();
        let backend = Backend::with(
            &data,
            Settings {
                prefix: prefix.clone(),
                host: HOST.into(),
            },
        );
        Some(Self {
            _dir: dir,
            data,
            prefix,
            backend,
        })
    }

    fn service(&self, name: &str) -> String {
        format!("{}{name}", self.prefix)
    }

    /// Signs a definition a test wrote itself, as K3 Up would have.
    fn sign(&self, name: &str) {
        let workloads = self.data.join("workloads");
        let definition = std::fs::read(workloads.join(format!("{name}.toml"))).unwrap();
        let key = integrity::load_key(&self.prefix).unwrap();
        let signature = integrity::sign(&key, name, &definition).unwrap();
        files::write_atomic(
            &workloads.join(format!("{name}.sig")),
            signature.as_bytes(),
            k3up::win32::set_owner_to_administrators,
        )
        .unwrap();
    }

    fn workload(&self, name: &str, mode: &str) -> Workload {
        Workload {
            name: name.into(),
            executable: std::env::current_exe().unwrap().to_string_lossy().into(),
            working_directory: self.data.to_string_lossy().into(),
            args: ["--exact", "fixture", "--ignored", "--nocapture"]
                .map(String::from)
                .to_vec(),
            environment: BTreeMap::from([
                ("K3UP_FIXTURE".into(), mode.into()),
                ("PATH".into(), system_path()),
            ]),
            stop_timeout_secs: 1,
            restart_delay_secs: 1,
            ..Default::default()
        }
    }

    fn send(&self, command: Command) -> Response {
        self.backend
            .handle(command)
            .unwrap_or_else(|error| Response::error(format!("{error:#}")))
    }

    fn put(&self, workload: Workload) -> Response {
        self.send(Command::Put {
            workload: Box::new(workload),
            create_only: false,
        })
    }

    fn ok(&self, command: Command) -> Response {
        let response = self.send(command);
        assert!(response.ok, "{}", response.message);
        response
    }

    fn get(&self, name: &str) -> Status {
        self.ok(Command::Get { name: name.into() })
            .workloads
            .remove(0)
    }

    fn until(&self, name: &str, wanted: impl Fn(&Status) -> bool) -> Status {
        let deadline = Instant::now() + WAIT;
        loop {
            let status = self.get(name);
            if wanted(&status) {
                return status;
            }
            assert!(
                Instant::now() < deadline,
                "{name} never reached the wanted state: {status:?}"
            );
            std::thread::sleep(Duration::from_millis(200));
        }
    }

    fn log(&self, name: &str) -> String {
        std::fs::read_to_string(self.data.join("logs").join(format!("{name}.log")))
            .unwrap_or_default()
    }

    fn until_logged(&self, name: &str, text: &str) {
        let deadline = Instant::now() + WAIT;
        while !self.log(name).contains(text) {
            assert!(
                Instant::now() < deadline,
                "no '{text}' in the log of {name}"
            );
            std::thread::sleep(Duration::from_millis(200));
        }
    }

    fn sc(&self, verb: &str, name: &str) -> String {
        let output = std::process::Command::new("sc.exe")
            .args([verb, &self.service(name)])
            .output()
            .unwrap();
        String::from_utf8_lossy(&output.stdout).into_owned()
    }

    fn sc_config(&self, name: &str, settings: &[&str]) {
        let output = std::process::Command::new("sc.exe")
            .args(["config", &self.service(name)])
            .args(settings)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stdout)
        );
    }

    fn apply(&self, workloads: &[Workload], dry_run: bool) -> Response {
        self.send(Command::Apply {
            manifest: Manifest {
                version: 1,
                workloads: workloads.to_vec(),
            },
            dry_run,
        })
    }

    fn cli_command(&self, arguments: &[&str]) -> std::process::Command {
        let mut command = std::process::Command::new(CLI);
        command
            .arg("--data-dir")
            .arg(&self.data)
            .arg("--json")
            .args(arguments)
            .env("K3UP_SERVICE_PREFIX", &self.prefix)
            .env("K3UP_HOST", HOST);
        command
    }

    fn cli(&self, arguments: &[&str]) -> (bool, String) {
        let output = self.cli_command(arguments).output().unwrap();
        (
            output.status.success(),
            String::from_utf8_lossy(&output.stdout).into_owned(),
        )
    }

    fn cli_response(&self, arguments: &[&str]) -> Response {
        let (success, stdout) = self.cli(arguments);
        let response: Response = serde_json::from_str(&stdout).unwrap();
        assert!(
            success && response.ok,
            "{arguments:?}: {}",
            response.message
        );
        response
    }
}

impl Drop for Machine {
    fn drop(&mut self) {
        for service in scm::list(&self.prefix).unwrap_or_default() {
            let _ = scm::stop(&service.name, Duration::from_secs(60));
            let _ = scm::delete(&service.name);
        }
        let _ = std::process::Command::new("reg.exe")
            .args([
                "delete",
                &format!(r"HKLM\{}", integrity::key_path(&self.prefix)),
                "/f",
            ])
            .output();
    }
}

#[test]
fn a_service_runs_with_a_process_and_writes_its_log() {
    let Some(machine) = Machine::start() else {
        return;
    };
    let created = machine.put(machine.workload("pulse", "pulse"));
    assert!(created.ok, "{}", created.message);
    assert_eq!(created.message, "Create pulse");
    assert!(machine.sc("query", "pulse").contains("STOPPED"));

    let started = machine.ok(Command::Start {
        name: "pulse".into(),
    });
    assert_eq!(started.message, "Start requested for pulse");
    let status = machine.until("pulse", |status| status.state == State::Running);
    assert!(status.pid.is_some_and(|pid| pid > 0), "{status:?}");
    assert!(status.desired_running);
    assert!(machine.sc("query", "pulse").contains("RUNNING"));
    machine.until_logged("pulse", "tick");

    let listed = machine.ok(Command::List).workloads;
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].pid, status.pid);
    let logs = machine.ok(Command::Logs {
        name: "pulse".into(),
        lines: 20,
        after: None,
    });
    assert!(logs.text.unwrap().contains("tick"));
    let events = machine.ok(Command::Events {
        name: Some("pulse".into()),
        after: None,
    });
    assert!(
        events
            .events
            .iter()
            .any(|event| event.message == "Process started"),
        "{:?}",
        events.events
    );
}

#[test]
fn a_service_stops_starts_again_and_is_removed_keeping_its_log() {
    let Some(machine) = Machine::start() else {
        return;
    };
    assert!(machine.put(machine.workload("cycle", "pulse")).ok);
    machine.ok(Command::Start {
        name: "cycle".into(),
    });
    let first = machine.until("cycle", |status| status.state == State::Running);
    machine.until_logged("cycle", "tick");

    let removing = machine.send(Command::Remove {
        name: "cycle".into(),
    });
    assert!(!removing.ok);
    assert_eq!(removing.message, "Stop 'cycle' before removing it");

    machine.ok(Command::Stop {
        name: "cycle".into(),
    });
    let stopped = machine.get("cycle");
    assert_eq!(stopped.state, State::Stopped, "{stopped:?}");
    assert_eq!(stopped.pid, None);
    assert_eq!(stopped.reason, "Stopped by request");
    assert!(machine.sc("query", "cycle").contains("STOPPED"));

    machine.ok(Command::Start {
        name: "cycle".into(),
    });
    let second = machine.until("cycle", |status| status.state == State::Running);
    assert_ne!(second.pid, first.pid);
    machine.ok(Command::Stop {
        name: "cycle".into(),
    });

    let removed = machine.ok(Command::Remove {
        name: "cycle".into(),
    });
    assert_eq!(removed.message, "Removed cycle");
    assert!(!scm::exists(&machine.service("cycle")).unwrap());
    assert!(machine.log("cycle").contains("tick"));
    assert!(!machine.data.join("workloads").join("cycle.toml").exists());
    let gone = machine.send(Command::Get {
        name: "cycle".into(),
    });
    assert_eq!(gone.message, "Unknown workload 'cycle'");
}

#[test]
fn jobs_complete_with_listed_codes_and_fail_with_others() {
    let Some(machine) = Machine::start() else {
        return;
    };
    let mut passing = machine.workload("passing", "exit");
    passing.kind = Kind::Job;
    passing.success_exit_codes = vec![3];
    passing
        .environment
        .insert("K3UP_EXIT_CODE".into(), "3".into());
    assert!(machine.put(passing).ok);
    machine.ok(Command::Start {
        name: "passing".into(),
    });
    let done = machine.until("passing", |status| status.state == State::Completed);
    assert_eq!(done.last_exit, Some(3));
    assert_eq!(
        done.reason,
        "Process exited with code 3, counted as success"
    );
    assert_eq!(
        scm::specific_exit_code(&machine.service("passing")).unwrap(),
        None
    );

    let mut failing = machine.workload("failing", "exit");
    failing.kind = Kind::Job;
    assert!(machine.put(failing).ok);
    let started = machine.send(Command::Start {
        name: "failing".into(),
    });
    if !started.ok {
        assert_eq!(
            started.message,
            "failing stopped right after starting: Process exited with code 1"
        );
    }
    let failed = machine.until("failing", |status| status.state == State::Failed);
    assert_eq!(failed.last_exit, Some(1));
    assert_eq!(failed.reason, "Process exited with code 1");
    assert!(!failed.desired_running);
    assert_eq!(
        scm::specific_exit_code(&machine.service("failing")).unwrap(),
        Some(k3up::services::host::EXIT_FAILED)
    );
}

#[test]
fn the_host_restarts_a_failing_service_until_the_limit() {
    let Some(machine) = Machine::start() else {
        return;
    };
    let mut crashing = machine.workload("crashing", "exit");
    crashing.restart = Restart::OnFailure;
    crashing.max_restarts = RestartLimit::Count(2);
    crashing.restart_backoff = RestartBackoff::Fixed;
    assert!(machine.put(crashing).ok);
    machine.ok(Command::Start {
        name: "crashing".into(),
    });
    let failed = machine.until("crashing", |status| status.state == State::Failed);
    assert_eq!(failed.restart_count, 2);
    assert_eq!(
        failed.reason,
        "Process exited with code 1; restart limit reached"
    );
    let history: Vec<String> = machine
        .ok(Command::Events {
            name: Some("crashing".into()),
            after: None,
        })
        .events
        .into_iter()
        .map(|event| event.message)
        .collect();
    assert!(
        history.contains(&"Process exited with code 1; retry 1/2 in 1s".to_string()),
        "{history:?}"
    );
    assert!(
        history.contains(&"Process exited with code 1; retry 2/2 in 1s".to_string()),
        "{history:?}"
    );
    // A workload that gave up must stay stopped: the service manager's failure actions are
    // only for a crashed host, and the first would act after five seconds.
    std::thread::sleep(Duration::from_secs(8));
    assert!(machine.sc("query", "crashing").contains("STOPPED"));
    assert_eq!(machine.get("crashing").state, State::Failed);
}

#[test]
fn labels_change_live_and_other_changes_wait_for_a_stop() {
    let Some(machine) = Machine::start() else {
        return;
    };
    let mut web = machine.workload("web", "pulse");
    web.group = "tools".into();
    assert!(machine.put(web.clone()).ok);
    assert_eq!(
        scm::query(&machine.service("web"))
            .unwrap()
            .unwrap()
            .display_name,
        "K3 Up: tools/web"
    );
    machine.ok(Command::Start { name: "web".into() });
    let before = machine.until("web", |status| status.state == State::Running);

    web.group = "tools/front".into();
    web.description = "Front end".into();
    let relabelled = machine.put(web.clone());
    assert!(relabelled.ok, "{}", relabelled.message);
    let after = machine.get("web");
    assert_eq!(after.pid, before.pid);
    assert_eq!(after.state, State::Running);
    assert_eq!(after.workload.group, "tools/front");
    assert_eq!(
        scm::query(&machine.service("web"))
            .unwrap()
            .unwrap()
            .display_name,
        "K3 Up: tools/front/web"
    );

    web.args.push("--quiet".into());
    let refused = machine.put(web);
    assert!(!refused.ok);
    assert_eq!(refused.message, "Stop 'web' before changing its definition");
    assert_eq!(machine.get("web").pid, before.pid);
}

#[test]
fn start_at_boot_means_automatic_delayed_start() {
    let Some(machine) = Machine::start() else {
        return;
    };
    let mut boot = machine.workload("boot", "pulse");
    boot.start_at_boot = true;
    assert!(machine.put(boot).ok);
    let config = machine.sc("qc", "boot");
    assert!(config.contains("AUTO_START"), "{config}");
    assert!(config.contains("DELAYED"), "{config}");
    machine.until("boot", |status| status.state == State::Running);

    let manual = machine.workload("manual", "pulse");
    assert!(machine.put(manual).ok);
    assert!(machine.sc("qc", "manual").contains("DEMAND_START"));

    let mut job = machine.workload("bootjob", "exit");
    job.kind = Kind::Job;
    job.start_at_boot = true;
    assert!(machine.put(job).ok);
    let config = machine.sc("qc", "bootjob");
    assert!(config.contains("DEMAND_START"), "{config}");
    assert_eq!(machine.get("bootjob").state, State::Stopped);
}

#[test]
fn schedules_are_refused_clearly() {
    let Some(machine) = Machine::start() else {
        return;
    };
    let mut nightly = machine.workload("nightly", "exit");
    nightly.kind = Kind::Job;
    nightly.schedule = Some(Schedule {
        every_secs: Some(3600),
        cron: None,
        timezone: "UTC".into(),
        action: Default::default(),
        missed: Default::default(),
    });
    let refused = machine.put(nightly);
    assert!(!refused.ok);
    assert!(
        refused
            .message
            .contains("Schedules are not supported for Windows services yet"),
        "{}",
        refused.message
    );
    assert!(!scm::exists(&machine.service("nightly")).unwrap());
    assert!(!machine.data.join("workloads").join("nightly.toml").exists());
}

#[test]
fn the_command_line_talks_to_services_in_process() {
    let Some(machine) = Machine::start() else {
        return;
    };
    let exe = std::env::current_exe().unwrap();
    let data = machine.data.to_string_lossy().into_owned();
    machine.cli_response(&[
        "create",
        "cli",
        "--exe",
        &exe.to_string_lossy(),
        "--cwd",
        &data,
        "--env",
        "K3UP_FIXTURE=pulse",
        "--group",
        "tools/cli",
        "--stop-timeout",
        "1",
        "--start",
        "--wait",
        "--",
        "--exact",
        "fixture",
        "--ignored",
        "--nocapture",
    ]);

    let listed = machine.cli_response(&["list"]).workloads;
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].workload.name, "cli");
    assert_eq!(listed[0].state, State::Running);

    let status = machine.cli_response(&["status", "cli"]).workloads.remove(0);
    assert_eq!(status.state, State::Running);
    assert!(status.pid.is_some());
    assert_eq!(status.workload.group, "tools/cli");

    machine.until_logged("cli", "tick");
    let logs = machine.cli_response(&["logs", "cli", "--lines", "50"]);
    assert!(logs.text.unwrap().contains("tick"));

    let (success, groups) = machine.cli(&["groups"]);
    assert!(success, "{groups}");
    let groups: serde_json::Value = serde_json::from_str(&groups).unwrap();
    let paths: Vec<&str> = groups
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|folder| folder["path"].as_str())
        .collect();
    assert_eq!(paths, ["tools", "tools/cli"]);

    let (success, summary) = machine.cli(&["services", "status"]);
    assert!(success, "{summary}");
    let summary: serde_json::Value = serde_json::from_str(&summary).unwrap();
    assert_eq!(summary["enabled"], true);
    assert_eq!(summary["workloads"]["running"], 1);

    let (success, agent) = machine.cli(&["agent", "start"]);
    assert!(!success);
    assert!(agent.contains("services mode"), "{agent}");

    machine.cli_response(&["stop", "cli"]);
    machine.cli_response(&["remove", "cli"]);
    assert!(machine.cli_response(&["list"]).workloads.is_empty());
}

/// A PATH of folders only administrators can change, so tests see only the warnings they set
/// up, whatever the runner's machine PATH holds.
fn system_path() -> String {
    system_cmd()
        .parent()
        .unwrap()
        .to_string_lossy()
        .into_owned()
}

fn system_cmd() -> PathBuf {
    PathBuf::from(std::env::var_os("SystemRoot").unwrap())
        .join("System32")
        .join("cmd.exe")
}

/// A folder beside the data directory in which users may change everything.
fn open_folder(machine: &Machine) -> PathBuf {
    let open = machine.data.parent().unwrap().join("open");
    std::fs::create_dir(&open).unwrap();
    icacls(&open, &["/grant", "*S-1-5-32-545:(OI)(CI)M"]);
    open
}

fn warnings(message: &str) -> Vec<&str> {
    message
        .lines()
        .filter(|line| line.starts_with("Warning: "))
        .collect()
}

#[test]
fn a_program_users_may_change_is_created_with_a_warning() {
    let Some(machine) = Machine::start() else {
        return;
    };
    let program = open_folder(&machine).join("cmd.exe");
    std::fs::copy(system_cmd(), &program).unwrap();
    let data = machine.data.to_string_lossy().into_owned();
    let created = machine.cli_response(&[
        "create",
        "open",
        "--exe",
        &program.to_string_lossy(),
        "--cwd",
        &data,
        "--env",
        &format!("PATH={}", system_path()),
        "--",
        "/c",
        "exit",
        "0",
    ]);
    let folder = program.parent().unwrap();
    assert_eq!(
        warnings(&created.message),
        [
            exposure::warning_line("open", &program),
            exposure::warning_line("open", folder),
        ]
    );

    let (success, health) = machine.cli(&["health"]);
    assert!(success, "{health}");
    let health: serde_json::Value = serde_json::from_str(&health).unwrap();
    assert_eq!(health["status"], "warning");
    let expected = format!(
        "open runs as SYSTEM using {}, {}, which users who aren't administrators can change",
        program.display(),
        folder.display()
    );
    assert!(
        health["warnings"]
            .as_array()
            .unwrap()
            .iter()
            .any(|warning| warning["kind"] == "program" && warning["message"] == expected.as_str()),
        "{health}"
    );
}

#[test]
fn a_start_that_fails_after_the_create_still_warns() {
    let Some(machine) = Machine::start() else {
        return;
    };
    let program = open_folder(&machine).join("cmd.exe");
    std::fs::copy(system_cmd(), &program).unwrap();
    let data = machine.data.to_string_lossy().into_owned();
    // The service is created for a host that isn't there, so the start always fails.
    let output = machine
        .cli_command(&[
            "create",
            "failing",
            "--exe",
            &program.to_string_lossy(),
            "--cwd",
            &data,
            "--env",
            &format!("PATH={}", system_path()),
            "--start",
            "--",
            "/c",
            "exit",
            "0",
        ])
        .env("K3UP_HOST", machine.data.join("missing-host.exe"))
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert_eq!(output.status.code(), Some(1), "{stdout}");
    let response: Response = serde_json::from_str(&stdout).unwrap();
    assert!(!response.ok);
    assert_eq!(
        warnings(&response.message),
        [
            exposure::warning_line("failing", &program),
            exposure::warning_line("failing", program.parent().unwrap()),
        ],
        "{}",
        response.message
    );
    assert!(
        !response.message.starts_with("Warning: "),
        "{}",
        response.message
    );
    assert_eq!(
        machine.get("failing").workload.executable,
        program.to_string_lossy()
    );
}

#[test]
fn a_missing_program_is_judged_by_the_folder_that_would_hold_it() {
    let Some(machine) = Machine::start() else {
        return;
    };
    let open = open_folder(&machine);
    let mut workload = machine.workload("later", "run");
    let missing = open.join("not-yet").join("tool.exe");
    workload.executable = missing.to_string_lossy().into();
    let exposed: Vec<PathBuf> = exposure::exposed_paths(&workload)
        .into_iter()
        .map(|exposure| exposure.path)
        .collect();
    assert_eq!(
        exposed,
        [missing.clone(), missing.parent().unwrap().to_path_buf()]
    );

    workload.executable = machine
        .data
        .join("not-yet")
        .join("tool.exe")
        .to_string_lossy()
        .into();
    assert!(exposure::exposed_paths(&workload).is_empty());

    // A link in a protected folder that leads to a missing folder users can create.
    let link = machine.data.join("later-link");
    std::os::windows::fs::symlink_dir(open.join("gone"), &link).unwrap();
    let through_link = link.join("tool.exe");
    workload.executable = through_link.to_string_lossy().into();
    let exposed: Vec<PathBuf> = exposure::exposed_paths(&workload)
        .into_iter()
        .map(|exposure| exposure.path)
        .collect();
    assert_eq!(exposed, [through_link, link.clone()]);
    std::fs::remove_dir(&link).unwrap();

    // A link higher up leads into a folder users control, though the last folder is protected.
    let real = open.join("deep");
    std::fs::create_dir_all(real.join("inner")).unwrap();
    k3up::win32::secure_dir(&real.join("inner"), k3up::win32::PRIVATE_DIR_SDDL).unwrap();
    let hop = machine.data.join("hop");
    std::os::windows::fs::symlink_dir(&real, &hop).unwrap();
    let nested = hop.join("inner").join("tool.exe");
    workload.executable = nested.to_string_lossy().into();
    let exposed: Vec<PathBuf> = exposure::exposed_paths(&workload)
        .into_iter()
        .map(|exposure| exposure.path)
        .collect();
    assert_eq!(
        exposed,
        [nested.clone(), nested.parent().unwrap().to_path_buf()]
    );
    std::fs::remove_dir(&hop).unwrap();

    // A protected link leads through a second link that sits in a folder users control.
    let settled = machine.data.join("settled");
    std::fs::create_dir(&settled).unwrap();
    let middle = open.join("middle");
    std::os::windows::fs::symlink_dir(&settled, &middle).unwrap();
    let front = machine.data.join("front");
    std::os::windows::fs::symlink_dir(&middle, &front).unwrap();
    let chained = front.join("tool.exe");
    workload.executable = chained.to_string_lossy().into();
    let exposed: Vec<PathBuf> = exposure::exposed_paths(&workload)
        .into_iter()
        .map(|exposure| exposure.path)
        .collect();
    assert_eq!(exposed, [chained, front.clone()]);
    std::fs::remove_dir(&front).unwrap();
    std::fs::remove_dir(&middle).unwrap();

    // A bare command is searched for on the workload's PATH, here through a folder users control.
    workload.executable = system_cmd().to_string_lossy().into();
    workload.args = vec!["/c".into(), "helper.exe".into()];
    let searched = open.join("bin");
    workload.environment.insert(
        "PATH".into(),
        format!("{};{}", searched.display(), system_path()),
    );
    assert_eq!(
        exposure::exposed_paths(&workload),
        [exposure::Exposure {
            path: searched,
            what: exposure::Use::SearchPath,
        }]
    );
}

#[test]
fn only_scripts_users_may_change_are_warned_about() {
    let Some(machine) = Machine::start() else {
        return;
    };
    let protected = machine.data.parent().unwrap().join("protected");
    k3up::win32::create_dir_with(&protected, k3up::win32::PRIVATE_DIR_SDDL).unwrap();
    let kept = protected.join("job.cmd");
    std::fs::write(&kept, "exit 0").unwrap();
    k3up::win32::set_owner_to_administrators(&kept).unwrap();
    let open = open_folder(&machine).join("job.cmd");
    std::fs::write(&open, "exit 0").unwrap();

    let cmd = system_cmd().to_string_lossy().into_owned();
    let data = machine.data.to_string_lossy().into_owned();
    let create = |name: &str, script: &Path| {
        machine
            .cli_response(&[
                "create",
                name,
                "--exe",
                &cmd,
                "--cwd",
                &data,
                "--env",
                &format!("PATH={}", system_path()),
                "--",
                "/c",
                &script.to_string_lossy(),
            ])
            .message
    };
    let quiet = create("quiet", &kept);
    assert!(warnings(&quiet).is_empty(), "{quiet}");
    let open_folder = open.parent().unwrap();
    let warned = create("warned", &open);
    assert_eq!(
        warnings(&warned),
        [
            exposure::warning_line("warned", &open),
            exposure::warning_line("warned", open_folder),
        ]
    );
    let relative = create("relative", Path::new(r"..\open\job.cmd"));
    assert_eq!(
        warnings(&relative),
        [
            exposure::warning_line("relative", &open),
            exposure::warning_line("relative", open_folder),
        ]
    );
    // The whole command string is one argument; cmd runs the script inside it.
    let called = create("called", Path::new(r"call ..\open\job.cmd"));
    assert_eq!(
        warnings(&called),
        [
            exposure::warning_line("called", &open),
            exposure::warning_line("called", open_folder),
            exposure::unresolved_line("called", Path::new(r"call ..\open\job.cmd")),
        ]
    );

    let option = Workload {
        name: "option".into(),
        executable: cmd.clone(),
        working_directory: data.clone(),
        args: vec![format!("--script={}", open.display())],
        environment: BTreeMap::from([("PATH".into(), system_path())]),
        ..Default::default()
    };
    assert_eq!(
        exposure::exposed_paths(&option),
        [
            exposure::Exposure {
                path: open.clone(),
                what: exposure::Use::Argument,
            },
            exposure::Exposure {
                path: open_folder.to_path_buf(),
                what: exposure::Use::ScriptFolder,
            },
        ]
    );

    // A bare command is run from the PATH folder that holds it; the file itself is checked.
    let shelf = machine.data.parent().unwrap().join("shelf");
    k3up::win32::create_dir_with(&shelf, k3up::win32::PRIVATE_DIR_SDDL).unwrap();
    let helper = shelf.join("helper.cmd");
    std::fs::write(&helper, "exit 0").unwrap();
    k3up::win32::set_owner_to_administrators(&helper).unwrap();
    icacls(&helper, &["/grant", "*S-1-5-32-545:M"]);
    let bare = Workload {
        name: "bare".into(),
        executable: cmd.clone(),
        working_directory: data.clone(),
        args: vec!["/c".into(), "helper".into()],
        environment: BTreeMap::from([(
            "PATH".into(),
            format!("{};{}", shelf.display(), system_path()),
        )]),
        ..Default::default()
    };
    let exposed: Vec<PathBuf> = exposure::exposed_paths(&bare)
        .into_iter()
        .map(|exposure| exposure.path)
        .collect();
    assert_eq!(exposed, [helper]);
}

const UNSIGNED: &str =
    "Refusing to run: the definition was not written by K3 Up; re-apply it from a trusted manifest";

impl Machine {
    /// Starts `name`, expecting its host to refuse the definition as unsigned.
    fn refused(&self, name: &str) {
        let response = self.send(Command::Start { name: name.into() });
        assert!(!response.ok);
        assert_eq!(
            response.message,
            format!("{name} stopped right after starting: {UNSIGNED}")
        );
        assert_eq!(
            scm::specific_exit_code(&self.service(name)).unwrap(),
            Some(k3up::services::host::EXIT_UNSIGNED)
        );
        let status = self.get(name);
        assert_eq!(status.state, State::Failed);
        assert_eq!(status.reason, UNSIGNED);
        for shared in [
            self.data.join("state").join(format!("{name}.json")),
            self.data.join("events").join(format!("{name}.jsonl")),
        ] {
            let text = std::fs::read_to_string(&shared).unwrap();
            assert!(!text.contains("K3UP_FIXTURE"), "{text}");
            assert!(!text.contains("stop_timeout_secs"), "{text}");
        }
    }
}

#[test]
fn the_host_runs_only_definitions_k3up_signed() {
    let Some(machine) = Machine::start() else {
        return;
    };
    let workloads = machine.data.join("workloads");
    let definition = workloads.join("signed.toml");
    let signature = workloads.join("signed.sig");
    assert!(machine.put(machine.workload("signed", "pulse")).ok);
    assert!(signature.is_file());
    machine.ok(Command::Start {
        name: "signed".into(),
    });
    machine.until("signed", |status| status.state == State::Running);
    machine.ok(Command::Stop {
        name: "signed".into(),
    });

    let original = std::fs::read_to_string(&definition).unwrap();
    let changed = original.replacen("stop_timeout_secs = 1", "stop_timeout_secs = 2", 1);
    assert_ne!(changed, original);
    std::fs::write(&definition, &changed).unwrap();
    machine.refused("signed");

    std::fs::write(&definition, &original).unwrap();
    std::fs::remove_file(&signature).unwrap();
    machine.refused("signed");

    // Applying the same definition again signs it again.
    let applied = machine.put(machine.workload("signed", "pulse"));
    assert_eq!(applied.message, "Update signed");
    machine.ok(Command::Start {
        name: "signed".into(),
    });
    machine.until("signed", |status| status.state == State::Running);
    machine.ok(Command::Stop {
        name: "signed".into(),
    });
}

#[test]
fn a_lost_key_is_not_replaced_while_definitions_are_signed_with_it() {
    let Some(machine) = Machine::start() else {
        return;
    };
    assert!(machine.put(machine.workload("kept", "pulse")).ok);
    let workloads = machine.data.join("workloads");
    let key = integrity::key_path(&machine.prefix);
    let stored = || k3up::win32::registry_binary(k3up::win32::Hive::LocalMachine, &key, "Key");
    let refused = format!(
        r"The signing key in HKLM\{key} is missing or damaged, but 1 workload definition is signed with it. Restore the key from a backup, or remove those workloads with `k3up remove --stop NAME` and re-apply them from a trusted manifest after `k3up services enable`."
    );

    k3up::win32::delete_registry_value(k3up::win32::Hive::LocalMachine, &key, "Key").unwrap();
    let error = integrity::ensure_key(&machine.prefix, &workloads).unwrap_err();
    assert_eq!(format!("{error:#}"), refused);
    assert_eq!(stored(), None);

    let output = std::process::Command::new("reg.exe")
        .args([
            "add",
            &format!(r"HKLM\{key}"),
            "/v",
            "Key",
            "/t",
            "REG_BINARY",
        ])
        .args(["/d", "0102", "/f"])
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    let error = integrity::ensure_key(&machine.prefix, &workloads).unwrap_err();
    assert_eq!(format!("{error:#}"), refused);
    assert_eq!(stored(), Some(vec![1, 2]));

    machine.ok(Command::Remove {
        name: "kept".into(),
    });
    integrity::ensure_key(&machine.prefix, &workloads).unwrap();
    assert_eq!(
        integrity::load_key(&machine.prefix).unwrap().len(),
        integrity::KEY_LENGTH
    );
}

#[test]
fn a_signature_is_bound_to_its_workload_name() {
    let Some(machine) = Machine::start() else {
        return;
    };
    let workloads = machine.data.join("workloads");
    assert!(machine.put(machine.workload("a", "pulse")).ok);
    assert!(machine.put(machine.workload("b", "pulse")).ok);
    for extension in ["toml", "sig"] {
        let bytes = std::fs::read(workloads.join(format!("a.{extension}"))).unwrap();
        files::write_atomic(
            &workloads.join(format!("b.{extension}")),
            &bytes,
            k3up::win32::set_owner_to_administrators,
        )
        .unwrap();
    }
    machine.refused("b");
}

#[test]
fn repair_refuses_a_file_another_program_holds_open() {
    use std::os::windows::fs::OpenOptionsExt;
    let Some(machine) = Machine::start() else {
        return;
    };
    let log = machine.data.join("logs").join("held.log");
    std::fs::write(&log, b"written before").unwrap();
    k3up::win32::set_owner_to_administrators(&log).unwrap();
    // FILE_SHARE_READ | FILE_SHARE_WRITE: a writer that lets others write too.
    let held = std::fs::OpenOptions::new()
        .write(true)
        .share_mode(0x1 | 0x2)
        .open(&log)
        .unwrap();
    let error = format!("{:#}", setup::prepare(&machine.data).unwrap_err());
    assert_eq!(
        error,
        format!(
            "{} is open in another program; close it and try again",
            log.display()
        )
    );
    drop(held);
    setup::prepare(&machine.data).unwrap();
}

fn elevated() -> bool {
    Machine::start().is_some()
}

fn junction(link: &std::path::Path, target: &std::path::Path) {
    let status = std::process::Command::new("cmd.exe")
        .args(["/c", "mklink", "/J"])
        .arg(link)
        .arg(target)
        .stdout(std::process::Stdio::null())
        .status()
        .unwrap();
    assert!(status.success());
}

#[test]
fn a_marker_in_a_folder_users_may_change_is_refused() {
    if !elevated() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let open = dir.path().join("open");
    std::fs::create_dir(&open).unwrap();
    let granted = std::process::Command::new("icacls.exe")
        .arg(&open)
        .args(["/grant", "*S-1-5-32-545:(OI)(CI)M"])
        .stdout(std::process::Stdio::null())
        .status()
        .unwrap();
    assert!(granted.success());
    std::fs::write(open.join(files::MARKER), b"").unwrap();

    let error = k3up::services::check(&open).unwrap_err().to_string();
    assert!(
        error.ends_with(
            "is not protected; services mode refuses it. Remove it and run `k3up services enable` from an elevated terminal"
        ),
        "{error}"
    );
    assert!(!k3up::services::active(&open));
    let output = std::process::Command::new(CLI)
        .arg("--data-dir")
        .arg(&open)
        .args(["--json", "list"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    let response: Response = serde_json::from_slice(&output.stdout).unwrap();
    assert!(
        response.message.contains("services mode refuses it"),
        "{}",
        response.message
    );
}

#[test]
fn junctions_in_the_data_directory_are_refused() {
    let Some(machine) = Machine::start() else {
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("target");
    setup::prepare(&target).unwrap();
    let link = dir.path().join("link");
    junction(&link, &target);
    let error = format!("{:#}", setup::prepare(&link).unwrap_err());
    assert!(error.contains("is a link to another location"), "{error}");
    assert!(k3up::services::check(&link).is_err());

    let workloads = machine.data.join("workloads");
    std::fs::remove_dir(&workloads).unwrap();
    let elsewhere = dir.path().join("elsewhere");
    k3up::win32::create_dir_with(&elsewhere, k3up::win32::PRIVATE_DIR_SDDL).unwrap();
    junction(&workloads, &elsewhere);
    let refused = machine.put(machine.workload("redirected", "pulse"));
    assert!(!refused.ok);
    assert!(
        refused
            .message
            .contains("services mode refuses to write definitions there"),
        "{}",
        refused.message
    );
    assert_eq!(std::fs::read_dir(&elsewhere).unwrap().count(), 0);
    assert!(!scm::exists(&machine.service("redirected")).unwrap());
    let error = format!("{:#}", setup::prepare(&machine.data).unwrap_err());
    assert!(error.contains("is a link to another location"), "{error}");
}

#[test]
fn a_broken_definition_never_leaks_into_files_every_user_reads() {
    let Some(machine) = Machine::start() else {
        return;
    };
    assert!(machine.put(machine.workload("leaky", "pulse")).ok);
    let broken = "name = \"leaky\"\n[environment]\nK3UP_SECRET = \"hunter2-do-not-leak\" oops\n";
    files::write_atomic(
        &machine.data.join("workloads").join("leaky.toml"),
        broken.as_bytes(),
        k3up::win32::set_owner_to_administrators,
    )
    .unwrap();
    machine.sign("leaky");
    scm::start(&machine.service("leaky")).unwrap();
    let state = scm::wait(&machine.service("leaky"), WAIT, |state| {
        state == k3up::services::status::Scm::Stopped
    })
    .unwrap();
    assert_eq!(state, Some(k3up::services::status::Scm::Stopped));
    assert_eq!(
        scm::specific_exit_code(&machine.service("leaky")).unwrap(),
        Some(k3up::services::host::EXIT_DEFINITION)
    );
    let state = std::fs::read_to_string(machine.data.join("state").join("leaky.json")).unwrap();
    let events = std::fs::read_to_string(machine.data.join("events").join("leaky.jsonl")).unwrap();
    assert!(
        state.contains("Refusing to run: the definition could not be read or is not valid"),
        "{state}"
    );
    assert!(!state.contains("hunter2"), "{state}");
    assert!(!events.contains("hunter2"), "{events}");
    assert!(machine.log("leaky").contains("k3up-host: Refusing to run"));
}

const USERS_READ: u32 = 0x1200a9;

/// A file's owner and access as SDDL, and the rights its DACL gives the Users group.
fn security(path: &Path) -> (String, u32) {
    use windows_sys::Win32::{
        Foundation::{ERROR_SUCCESS, LocalFree},
        Security::{
            ACL,
            Authorization::{
                ConvertSecurityDescriptorToStringSecurityDescriptorW, GetEffectiveRightsFromAclW,
                GetNamedSecurityInfoW, NO_MULTIPLE_TRUSTEE, SDDL_REVISION_1, SE_FILE_OBJECT,
                TRUSTEE_IS_SID, TRUSTEE_IS_WELL_KNOWN_GROUP, TRUSTEE_W,
            },
            CreateWellKnownSid, DACL_SECURITY_INFORMATION, OWNER_SECURITY_INFORMATION,
            PSECURITY_DESCRIPTOR, SECURITY_MAX_SID_SIZE, WinBuiltinUsersSid,
        },
    };
    let path = k3up::win32::wide(path.as_os_str());
    let wanted = OWNER_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION;
    // SAFETY: every out-pointer is valid for its call; the DACL points into the descriptor,
    // and the descriptor and the string are freed once read.
    unsafe {
        let mut dacl: *mut ACL = ptr::null_mut();
        let mut descriptor: PSECURITY_DESCRIPTOR = ptr::null_mut();
        let status = GetNamedSecurityInfoW(
            path.as_ptr(),
            SE_FILE_OBJECT,
            wanted,
            ptr::null_mut(),
            ptr::null_mut(),
            &mut dacl,
            ptr::null_mut(),
            &mut descriptor,
        );
        assert_eq!(status, ERROR_SUCCESS);
        let mut text = ptr::null_mut();
        let mut length = 0;
        assert_ne!(
            ConvertSecurityDescriptorToStringSecurityDescriptorW(
                descriptor,
                SDDL_REVISION_1,
                wanted,
                &mut text,
                &mut length,
            ),
            0
        );
        let sddl = String::from_utf16_lossy(std::slice::from_raw_parts(text, length as usize))
            .trim_end_matches('\0')
            .to_string();
        let mut sid = [0u8; SECURITY_MAX_SID_SIZE as usize];
        let mut size = SECURITY_MAX_SID_SIZE;
        assert_ne!(
            CreateWellKnownSid(
                WinBuiltinUsersSid,
                ptr::null_mut(),
                sid.as_mut_ptr().cast(),
                &mut size,
            ),
            0
        );
        let users = TRUSTEE_W {
            pMultipleTrustee: ptr::null_mut(),
            MultipleTrusteeOperation: NO_MULTIPLE_TRUSTEE,
            TrusteeForm: TRUSTEE_IS_SID,
            TrusteeType: TRUSTEE_IS_WELL_KNOWN_GROUP,
            ptstrName: sid.as_mut_ptr().cast(),
        };
        let mut rights = 0;
        assert_eq!(
            GetEffectiveRightsFromAclW(dacl, &users, &mut rights),
            ERROR_SUCCESS
        );
        LocalFree(text.cast());
        LocalFree(descriptor);
        (sddl, rights)
    }
}

#[test]
fn every_user_may_read_the_marker_and_only_administrators_change_it() {
    let Some(machine) = Machine::start() else {
        return;
    };
    let (sddl, users) = security(&machine.data.join(files::MARKER));
    let (owner, dacl) = sddl.split_once("D:").unwrap();
    assert_eq!(owner, "O:BA", "{sddl}");
    let (flags, aces) = dacl.split_once('(').unwrap();
    assert!(flags.contains('P'), "{sddl}");
    let aces: BTreeSet<&str> = aces
        .split(')')
        .map(|ace| ace.trim_start_matches('('))
        .filter(|ace| !ace.is_empty())
        .collect();
    assert_eq!(
        aces,
        BTreeSet::from(["A;;FA;;;SY", "A;;FA;;;BA", "A;;0x1200a9;;;BU"]),
        "{sddl}"
    );
    assert_eq!(users, USERS_READ);
    assert!(!k3up::services::grants_change(users));

    // Markers written before Users could read them inherited only the administrators' access.
    files::write_atomic(
        &machine.data.join(files::MARKER),
        b"",
        k3up::win32::set_owner_to_administrators,
    )
    .unwrap();
    let marker = machine.data.join(files::MARKER);
    assert_eq!(security(&marker).1, 0);
    setup::prepare(&machine.data).unwrap();
    assert_eq!(security(&marker).1, USERS_READ);
}

fn icacls(path: &Path, arguments: &[&str]) {
    let output = std::process::Command::new("icacls.exe")
        .arg(path)
        .args(arguments)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stdout)
    );
}

#[test]
fn files_left_in_the_folders_lose_their_own_permissions() {
    let Some(machine) = Machine::start() else {
        return;
    };
    let log = machine.data.join("logs").join("planted.log");
    let history = machine.data.join("events").join("planted.jsonl");
    for file in [&log, &history] {
        std::fs::write(file, b"planted\n").unwrap();
        icacls(file, &["/setowner", "*S-1-5-32-544"]);
        icacls(file, &["/inheritance:r", "/grant", "*S-1-5-32-545:F"]);
        assert_eq!(security(file).1 & 0x1f01ff, 0x1f01ff);
    }
    setup::prepare(&machine.data).unwrap();
    let (sddl, users) = security(&log);
    assert_eq!(users, 0, "{sddl}");
    assert!(sddl.starts_with("O:BA"), "{sddl}");
    let (sddl, users) = security(&history);
    assert_eq!(users, USERS_READ, "{sddl}");

    // Setting another account as owner needs the restore privilege, which icacls turns on.
    icacls(&history, &["/setowner", "*S-1-5-32-545"]);
    let error = format!("{:#}", setup::prepare(&machine.data).unwrap_err());
    assert!(
        error.contains("planted.jsonl belongs to another account"),
        "{error}"
    );
    let appended = files::append_event(&files::Layout::new(&machine.data), "planted", "refused");
    assert!(
        format!("{:#}", appended.unwrap_err()).contains("belongs to another account"),
        "the host must not append to a file another account owns"
    );
}

#[test]
fn files_others_may_read_or_change_are_refused_even_when_administrators_own_them() {
    let Some(machine) = Machine::start() else {
        return;
    };
    let log = machine.data.join("logs").join("readable.log");
    std::fs::write(&log, b"").unwrap();
    icacls(&log, &["/setowner", "*S-1-5-32-544"]);
    files::open_append(&log, files::Access::Private).unwrap();
    icacls(&log, &["/grant", "*S-1-5-32-545:R"]);
    let error = format!(
        "{:#}",
        files::open_append(&log, files::Access::Private).unwrap_err()
    );
    assert!(
        error.ends_with("can be read by other accounts; services mode refuses to write to it"),
        "{error}"
    );

    let state = machine.data.join("state").join("forged.json");
    files::write_state(
        &state,
        &files::HostState {
            state: State::Running,
            reason: "Process started".into(),
            ..Default::default()
        },
    )
    .unwrap();
    assert!(files::read_state(&state).is_some());
    icacls(&state, &["/grant", "*S-1-5-32-545:W"]);
    assert_eq!(files::read_state(&state), None);
    let error = format!(
        "{:#}",
        files::refuse_redirected(&state, files::Access::Shared).unwrap_err()
    );
    assert!(
        error.ends_with("can be changed by other accounts; services mode refuses it"),
        "{error}"
    );
}

#[test]
fn stopping_a_failed_workload_records_the_stop() {
    let Some(machine) = Machine::start() else {
        return;
    };
    let mut broken = machine.workload("broken", "exit");
    broken.restart = Restart::Never;
    assert!(machine.put(broken).ok);
    let _ = machine.send(Command::Start {
        name: "broken".into(),
    });
    let failed = machine.until("broken", |status| status.state == State::Failed);
    assert!(!failed.desired_running);
    machine.ok(Command::Stop {
        name: "broken".into(),
    });
    let stopped = machine.get("broken");
    assert_eq!(stopped.state, State::Stopped, "{stopped:?}");
    assert_eq!(stopped.reason, "Stopped by request");
    assert!(!stopped.desired_running);
    assert_eq!(stopped.pid, None);
    let listed = machine.ok(Command::List).workloads;
    assert_eq!(listed[0].state, State::Stopped);
}

/// Runs `read` on this thread as this account with its administrator rights turned off, as a
/// standard user would. Impersonating keeps the window station of a CI service session out of
/// the way, which a separate process could not open with the Administrators group disabled.
fn as_standard_user<T>(read: impl FnOnce() -> T) -> T {
    use windows_sys::Win32::{
        Foundation::{CloseHandle, HANDLE},
        Security::{
            CreateRestrictedToken, CreateWellKnownSid, DISABLE_MAX_PRIVILEGE,
            ImpersonateLoggedOnUser, RevertToSelf, SECURITY_MAX_SID_SIZE, SID_AND_ATTRIBUTES,
            TOKEN_DUPLICATE, TOKEN_QUERY, WinBuiltinAdministratorsSid,
        },
        System::Threading::{GetCurrentProcess, OpenProcessToken},
    };
    let mut token: HANDLE = ptr::null_mut();
    let mut restricted: HANDLE = ptr::null_mut();
    // SAFETY: every pointer refers to a live, initialized buffer for its call.
    unsafe {
        assert_ne!(
            OpenProcessToken(
                GetCurrentProcess(),
                TOKEN_DUPLICATE | TOKEN_QUERY,
                &mut token
            ),
            0
        );
        let mut sid = [0u8; SECURITY_MAX_SID_SIZE as usize];
        let mut size = SECURITY_MAX_SID_SIZE;
        assert_ne!(
            CreateWellKnownSid(
                WinBuiltinAdministratorsSid,
                ptr::null_mut(),
                sid.as_mut_ptr().cast(),
                &mut size,
            ),
            0
        );
        let administrators = SID_AND_ATTRIBUTES {
            Sid: sid.as_mut_ptr().cast(),
            Attributes: 0,
        };
        assert_ne!(
            CreateRestrictedToken(
                token,
                DISABLE_MAX_PRIVILEGE,
                1,
                &administrators,
                0,
                ptr::null(),
                0,
                ptr::null(),
                &mut restricted,
            ),
            0,
            "{}",
            std::io::Error::last_os_error()
        );
        assert_ne!(
            ImpersonateLoggedOnUser(restricted),
            0,
            "{}",
            std::io::Error::last_os_error()
        );
    }
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(read));
    // SAFETY: ends the impersonation started above and closes the handles opened above.
    unsafe {
        RevertToSelf();
        CloseHandle(restricted);
        CloseHandle(token);
    }
    result.unwrap_or_else(|panic| std::panic::resume_unwind(panic))
}

#[test]
fn a_standard_user_sees_states_but_not_definitions_or_logs() {
    let Some(machine) = Machine::start() else {
        return;
    };
    let mut reader = machine.workload("reader", "pulse");
    reader.group = "shared".into();
    assert!(machine.put(reader).ok);
    machine.ok(Command::Start {
        name: "reader".into(),
    });
    machine.until("reader", |status| status.state == State::Running);
    machine.until_logged("reader", "tick");

    let (detected, summary, listed, status, events, export, logs) = as_standard_user(|| {
        (
            k3up::services::check(&machine.data).map_err(|error| format!("{error:#}")),
            setup::summary(&machine.data).map(|summary| summary.enabled),
            machine.send(Command::List),
            machine.send(Command::Get {
                name: "reader".into(),
            }),
            machine.send(Command::Events {
                name: Some("reader".into()),
                after: None,
            }),
            machine.send(Command::Export),
            machine.send(Command::Logs {
                name: "reader".into(),
                lines: 20,
                after: None,
            }),
        )
    });
    assert_eq!(detected, Ok(true));
    assert!(summary.unwrap());

    assert!(listed.ok, "{}", listed.message);
    assert_eq!(listed.workloads.len(), 1, "{:?}", listed.workloads);
    assert_eq!(listed.workloads[0].state, State::Running);
    assert_eq!(listed.workloads[0].workload.group, "shared");
    assert!(listed.workloads[0].workload.executable.is_empty());

    assert!(status.ok, "{}", status.message);
    assert_eq!(status.workloads[0].state, State::Running);
    assert!(status.workloads[0].pid.is_some());
    assert!(status.workloads[0].workload.executable.is_empty());

    assert!(events.ok, "{}", events.message);
    assert!(!events.events.is_empty());

    for refused in [export, logs] {
        assert!(!refused.ok);
        assert!(
            refused.message.starts_with("Access denied"),
            "{}",
            refused.message
        );
    }

    let unknown = as_standard_user(|| unknown_name_requests(&machine));
    let elevated = unknown_name_requests(&machine);
    for response in unknown.into_iter().chain(elevated) {
        assert!(!response.ok);
        assert_eq!(response.message, "Unknown workload 'nobody'");
    }
}

/// Every request that names one workload, for a name that has no definition or service.
fn unknown_name_requests(machine: &Machine) -> Vec<Response> {
    let name = || "nobody".to_string();
    [
        Command::Get { name: name() },
        Command::Logs {
            name: name(),
            lines: 20,
            after: None,
        },
        Command::Start { name: name() },
        Command::Stop { name: name() },
        Command::Restart { name: name() },
        Command::Remove { name: name() },
    ]
    .into_iter()
    .map(|command| machine.send(command))
    .collect()
}

/// A kernel driver service, which the service manager lists apart from programs, so services
/// mode does not see it until it tries to create a service with the same name.
struct Driver(String);

impl Drop for Driver {
    fn drop(&mut self) {
        let _ = scm::delete(&self.0);
    }
}

#[test]
fn a_failed_apply_leaves_every_workload_as_it_was() {
    let Some(machine) = Machine::start() else {
        return;
    };
    let mut old = machine.workload("tx-old", "pulse");
    old.group = "before".into();
    old.description = "Before".into();
    assert!(machine.put(old.clone()).ok);
    let definition = machine.data.join("workloads").join("tx-old.toml");
    let saved = std::fs::read(&definition).unwrap();
    let config = machine.sc("qc", "tx-old");
    let description = machine.sc("qdescription", "tx-old");

    let clash = Driver(machine.service("tx-clash"));
    let created = std::process::Command::new("sc.exe")
        .args(["create", &clash.0, "type=", "kernel"])
        .args(["binPath=", r"C:\Windows\System32\drivers\k3up-test.sys"])
        .output()
        .unwrap();
    assert!(
        created.status.success(),
        "{}",
        String::from_utf8_lossy(&created.stdout)
    );
    let driver = machine.sc("qc", "tx-clash");

    old.group = "after".into();
    old.description = "After".into();
    old.start_at_boot = true;
    old.args.push("--quiet".into());
    let failed = machine.send(Command::Apply {
        manifest: Manifest {
            version: 1,
            workloads: vec![
                old,
                machine.workload("tx-new", "pulse"),
                machine.workload("tx-clash", "pulse"),
            ],
        },
        dry_run: false,
    });
    assert!(!failed.ok, "{}", failed.message);
    assert!(
        failed.message.starts_with(&format!(
            "Saving failed and every change was undone: Create service {}",
            clash.0
        )),
        "{}",
        failed.message
    );

    assert_eq!(std::fs::read(&definition).unwrap(), saved);
    assert_eq!(machine.sc("qc", "tx-old"), config);
    assert_eq!(machine.sc("qdescription", "tx-old"), description);
    assert!(!scm::exists(&machine.service("tx-new")).unwrap());
    assert_eq!(machine.sc("qc", "tx-clash"), driver);
    for name in ["tx-new", "tx-clash"] {
        let file = machine.data.join("workloads").join(format!("{name}.toml"));
        assert!(!file.exists(), "{name}");
    }
}

#[test]
fn concurrent_applies_leave_one_consistent_workload() {
    let Some(machine) = Machine::start() else {
        return;
    };
    let shared = |group: &str| {
        let mut workload = machine.workload("shared", "pulse");
        workload.group = group.into();
        workload.description = group.into();
        workload
    };
    std::thread::scope(|scope| {
        for group in ["one", "two", "three"] {
            let workload = shared(group);
            let machine = &machine;
            scope.spawn(move || {
                for _ in 0..4 {
                    let response = machine.send(Command::Apply {
                        manifest: Manifest {
                            version: 1,
                            workloads: vec![workload.clone()],
                        },
                        dry_run: false,
                    });
                    assert!(response.ok, "{}", response.message);
                }
            });
        }
    });
    let saved = files::read_definition(&machine.data.join("workloads").join("shared.toml"))
        .unwrap()
        .unwrap();
    assert_eq!(saved, shared(&saved.group));
    let service = scm::query(&machine.service("shared")).unwrap().unwrap();
    assert_eq!(
        service.display_name,
        format!("K3 Up: {}/shared", saved.group)
    );
    let description = machine.sc("qdescription", "shared");
    assert!(
        description.lines().any(|line| {
            line.trim_start().starts_with("DESCRIPTION") && line.trim_end().ends_with(&saved.group)
        }),
        "{description}"
    );
}

#[test]
fn readers_holding_shared_files_do_not_stall_the_host() {
    use std::os::windows::fs::OpenOptionsExt;
    let Some(machine) = Machine::start() else {
        return;
    };
    let mut looping = machine.workload("looping", "exit");
    looping.restart = Restart::Always;
    looping.max_restarts = RestartLimit::Unlimited;
    looping.restart_backoff = RestartBackoff::Fixed;
    assert!(machine.put(looping).ok);
    let _ = machine.send(Command::Start {
        name: "looping".into(),
    });
    let starts = || machine.log("looping").matches("Starting looping").count();
    let deadline = Instant::now() + WAIT;
    while starts() < 2 {
        assert!(Instant::now() < deadline, "{}", machine.log("looping"));
        std::thread::sleep(Duration::from_millis(200));
    }
    // A user may open these files, and with no sharing nobody else can write them.
    let hold = |path: PathBuf| {
        std::fs::OpenOptions::new()
            .read(true)
            .share_mode(0)
            .open(path)
            .unwrap()
    };
    let held = [
        hold(machine.data.join("state").join("looping.json")),
        hold(machine.data.join("events").join("looping.jsonl")),
    ];
    let before = starts();
    std::thread::sleep(Duration::from_secs(10));
    assert!(starts() >= before + 2, "{}", machine.log("looping"));
    assert!(machine.sc("query", "looping").contains("RUNNING"));

    let requested = Instant::now();
    let stopped = machine.send(Command::Stop {
        name: "looping".into(),
    });
    assert!(requested.elapsed() < Duration::from_secs(30));
    assert!(!stopped.ok);
    assert_eq!(stopped.message, state_file_in_use("looping"));
    assert!(machine.sc("query", "looping").contains("STOPPED"));
    drop(held);
    assert!(
        machine
            .log("looping")
            .contains("Activity history not updated yet")
    );
    let status = machine.get("looping");
    assert_eq!(status.state, State::Stopped, "{status:?}");
    assert!(!status.desired_running);
}

fn state_file_in_use(name: &str) -> String {
    format!(
        "Stopped {name}, but its state file is in use by another program; `k3up status` may show the old state until it is released"
    )
}

#[test]
fn a_stop_that_cannot_record_itself_fails_and_status_catches_up() {
    use std::os::windows::fs::OpenOptionsExt;
    let Some(machine) = Machine::start() else {
        return;
    };
    assert!(machine.put(machine.workload("held", "pulse")).ok);
    machine.ok(Command::Start {
        name: "held".into(),
    });
    machine.until("held", |status| status.state == State::Running);
    let state = machine.data.join("state").join("held.json");
    let held = std::fs::OpenOptions::new()
        .read(true)
        .share_mode(0)
        .open(&state)
        .unwrap();
    let output = std::process::Command::new(CLI)
        .arg("--data-dir")
        .arg(&machine.data)
        .args(["--json", "stop", "held"])
        .env("K3UP_SERVICE_PREFIX", &machine.prefix)
        .env("K3UP_HOST", HOST)
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    let response: Response = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(response.message, state_file_in_use("held"));
    assert!(machine.sc("query", "held").contains("STOPPED"));
    drop(held);

    let status = machine.get("held");
    assert_eq!(status.state, State::Stopped, "{status:?}");
    assert_eq!(status.reason, "Stopped (state file was out of date)");
    assert!(!status.desired_running);
    assert_eq!(status.pid, None);

    machine.ok(Command::Stop {
        name: "held".into(),
    });
    assert_eq!(machine.get("held").reason, "Stopped by request");
}

#[test]
fn a_host_that_dies_shows_as_ended_unexpectedly_until_it_is_restarted() {
    let Some(machine) = Machine::start() else {
        return;
    };
    assert!(machine.put(machine.workload("killed", "pulse")).ok);
    machine.ok(Command::Start {
        name: "killed".into(),
    });
    machine.until("killed", |status| status.state == State::Running);
    let host = scm::query(&machine.service("killed"))
        .unwrap()
        .unwrap()
        .host_pid
        .unwrap();
    let killed = std::process::Command::new("taskkill.exe")
        .args(["/F", "/PID", &host.to_string()])
        .output()
        .unwrap();
    assert!(
        killed.status.success(),
        "{}",
        String::from_utf8_lossy(&killed.stdout)
    );
    let state = scm::wait(&machine.service("killed"), WAIT, |state| {
        state == k3up::services::status::Scm::Stopped
    })
    .unwrap();
    assert_eq!(state, Some(k3up::services::status::Scm::Stopped));
    let crashed = machine.get("killed");
    assert_eq!(crashed.state, State::Backoff, "{crashed:?}");
    assert!(crashed.desired_running);
    assert_eq!(
        crashed.reason,
        "The K3 Up host ended unexpectedly; the service manager restarts it"
    );
    let restarted = machine.until("killed", |status| status.state == State::Running);
    assert_ne!(
        scm::query(&machine.service("killed"))
            .unwrap()
            .unwrap()
            .host_pid,
        Some(host),
        "{restarted:?}"
    );
}

#[test]
fn an_account_set_outside_k3up_is_put_back_when_a_change_fails() {
    let Some(machine) = Machine::start() else {
        return;
    };
    let web = machine.workload("account", "pulse");
    assert!(machine.put(web.clone()).ok);
    let local_system = machine.sc("qc", "account");
    machine.sc_config(
        "account",
        &["obj=", r"NT AUTHORITY\LocalService", "password=", ""],
    );
    let drifted = machine.sc("qc", "account");
    assert!(drifted.contains(r"NT AUTHORITY\LocalService"), "{drifted}");

    let service = machine.service("account");
    scm::FAIL_AFTER_ACCOUNT
        .lock()
        .unwrap()
        .push(service.clone());
    let failed = machine.apply(std::slice::from_ref(&web), false);
    scm::FAIL_AFTER_ACCOUNT
        .lock()
        .unwrap()
        .retain(|name| *name != service);
    assert!(!failed.ok);
    assert!(
        failed
            .message
            .starts_with("Saving failed and every change was undone: "),
        "{}",
        failed.message
    );
    assert!(
        failed.message.ends_with("failed on purpose"),
        "{}",
        failed.message
    );
    assert_eq!(machine.sc("qc", "account"), drifted);

    let applied = machine.apply(std::slice::from_ref(&web), false);
    assert_eq!(
        applied.message,
        "Update account: service settings differ (account)"
    );
    assert_eq!(machine.sc("qc", "account"), local_system);
}

#[test]
fn stopping_reports_whether_it_stopped_a_running_host() {
    let Some(machine) = Machine::start() else {
        return;
    };
    assert!(machine.put(machine.workload("steady", "pulse")).ok);
    machine.ok(Command::Start {
        name: "steady".into(),
    });
    machine.until("steady", |status| status.state == State::Running);
    let service = machine.service("steady");
    assert!(scm::stop(&service, WAIT).unwrap());
    assert!(!scm::stop(&service, WAIT).unwrap());

    let mut job = machine.workload("finished", "exit");
    job.kind = Kind::Job;
    job.environment.insert("K3UP_EXIT_CODE".into(), "0".into());
    assert!(machine.put(job).ok);
    machine.ok(Command::Start {
        name: "finished".into(),
    });
    machine.until("finished", |status| status.state == State::Completed);
    assert!(!scm::stop(&machine.service("finished"), WAIT).unwrap());
    assert!(!scm::stop(&machine.service("missing"), WAIT).unwrap());
}

#[test]
fn a_running_agent_in_the_per_user_folder_is_refused() {
    use fs2::FileExt;
    let dir = tempfile::tempdir().unwrap();
    let idle = setup::refuse_user_agent_in(dir.path());
    if let Err(error) = &idle {
        assert!(!error.to_string().contains("running"), "{error}");
    }
    let lock = std::fs::File::create(dir.path().join("agent.lock")).unwrap();
    lock.lock_exclusive().unwrap();
    let error = setup::refuse_user_agent_in(dir.path())
        .unwrap_err()
        .to_string();
    assert!(error.starts_with("Your own K3 Up agent "), "{error}");
    assert!(error.contains("is running"), "{error}");
}

#[test]
fn reserved_names_and_long_descriptions_change_nothing() {
    let Some(machine) = Machine::start() else {
        return;
    };
    let refused = machine.put(machine.workload("Com1", "pulse"));
    assert!(!refused.ok);
    assert!(
        refused.message.ends_with(
            "Com1 is reserved by Windows and can't be used as a workload name in services mode"
        ),
        "{}",
        refused.message
    );
    assert!(!scm::exists(&machine.service("Com1")).unwrap());

    let mut wordy = machine.workload("wordy", "pulse");
    wordy.description = "d".repeat(9000);
    let refused = machine.put(wordy.clone());
    assert!(!refused.ok);
    assert_eq!(
        refused.message,
        "The description for wordy is 9000 characters; Windows allows at most 8191"
    );
    assert!(!scm::exists(&machine.service("wordy")).unwrap());
    assert!(!machine.data.join("workloads").join("wordy.toml").exists());

    wordy.description = "d".repeat(8191);
    assert!(machine.put(wordy).ok);
}

#[test]
fn programs_are_installed_only_from_protected_folders() {
    if !elevated() {
        return;
    }
    let target = setup::install_dir().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let open = dir.path().join("open");
    std::fs::create_dir(&open).unwrap();
    std::fs::write(open.join("k3up-host.exe"), b"host").unwrap();
    icacls(&open, &["/grant", "*S-1-5-32-545:(OI)(CI)M"]);
    let error = setup::open_sources(&open, &target)
        .err()
        .unwrap()
        .to_string();
    assert_eq!(
        error,
        format!(
            "{} can be changed by users who aren't administrators, so services mode won't install programs from it. Extract the K3 Up release zip into {} and run \"{}\" services enable from an elevated terminal.",
            open.display(),
            target.display(),
            target.join("k3up.exe").display()
        )
    );

    let protected = dir.path().join("protected");
    k3up::win32::create_dir_with(&protected, k3up::win32::PRIVATE_DIR_SDDL).unwrap();
    for (program, bytes) in [("k3up-host.exe", "host"), ("k3up.exe", "cli")] {
        std::fs::write(protected.join(program), bytes).unwrap();
        icacls(&protected.join(program), &["/setowner", "*S-1-5-32-544"]);
    }
    let sources = setup::open_sources(&protected, &target).unwrap();
    let names: Vec<&str> = sources.iter().map(|(name, _)| *name).collect();
    assert_eq!(names, ["k3up-host.exe", "k3up.exe"]);
    // Held open, a source cannot be replaced before it is copied.
    assert!(std::fs::write(protected.join("k3up-host.exe"), b"swapped").is_err());
    drop(sources);

    // A program that users may change is refused even in a protected folder.
    icacls(&protected.join("k3up.exe"), &["/grant", "*S-1-5-32-545:M"]);
    let error = setup::open_sources(&protected, &target)
        .err()
        .unwrap()
        .to_string();
    assert!(error.starts_with(&format!(
        "{} can be changed",
        protected.join("k3up.exe").display()
    )));

    // Program Files belongs to TrustedInstaller and grants CREATOR OWNER on new children.
    assert!(k3up::win32::is_protected(
        &k3up::win32::program_files().unwrap()
    ));
}

#[test]
fn a_data_directory_below_a_folder_users_control_is_refused() {
    let Some(machine) = Machine::start() else {
        return;
    };
    assert!(k3up::services::check(&machine.data).unwrap());
    let default = k3up::win32::program_data().unwrap().join("K3 Up");
    assert!(k3up::win32::ancestors_protected(&default));

    let dir = protected_temp_dir();
    let open = dir.path().join("open");
    std::fs::create_dir(&open).unwrap();
    icacls(&open, &["/grant", "*S-1-5-32-545:(OI)(CI)F"]);
    let data = open.join("data");
    setup::prepare(&data).unwrap();
    assert!(k3up::win32::is_protected(&data));
    assert!(!k3up::win32::ancestors_protected(&data));
    let error = k3up::services::check(&data).unwrap_err().to_string();
    assert!(
        error.ends_with("is not protected; services mode refuses it. Remove it and run `k3up services enable` from an elevated terminal"),
        "{error}"
    );
}

#[test]
fn machine_settings_round_trip_through_the_registry() {
    use k3up::win32::{Hive, delete_registry_value, registry_string, set_registry_string};
    let Some(machine) = Machine::start() else {
        return;
    };
    // Its own key, so the record `services enable` keeps is never touched.
    let key = format!(r"SOFTWARE\K3 Up Test\{}", machine.prefix);
    let value = k3up::services::REGISTRY_VALUE;
    assert_eq!(registry_string(Hive::LocalMachine, &key, value), None);
    let data = machine.data.to_string_lossy().into_owned();
    set_registry_string(Hive::LocalMachine, &key, value, &data).unwrap();
    assert_eq!(registry_string(Hive::LocalMachine, &key, value), Some(data));
    delete_registry_value(Hive::LocalMachine, &key, value).unwrap();
    delete_registry_value(Hive::LocalMachine, &key, value).unwrap();
    assert_eq!(registry_string(Hive::LocalMachine, &key, value), None);
    let _ = std::process::Command::new("reg.exe")
        .args(["delete", &format!(r"HKLM\{key}"), "/f"])
        .output();
}

#[test]
fn a_new_service_that_fails_to_start_is_still_applied() {
    let Some(machine) = Machine::start() else {
        return;
    };
    let broken = Backend::with(
        &machine.data,
        Settings {
            prefix: machine.prefix.clone(),
            host: machine.data.join("missing-host.exe"),
        },
    );
    let mut boot = machine.workload("boothost", "pulse");
    boot.start_at_boot = true;
    let manifest = Manifest {
        version: 1,
        workloads: vec![boot],
    };
    let apply = || {
        broken
            .handle(Command::Apply {
                manifest: manifest.clone(),
                dry_run: false,
            })
            .unwrap()
    };
    let applied = apply();
    assert!(applied.ok, "{}", applied.message);
    assert!(
        applied
            .message
            .starts_with("Create boothost\nboothost failed to start: "),
        "{}",
        applied.message
    );
    let status = machine.get("boothost");
    assert_eq!(status.state, State::Failed, "{status:?}");
    assert!(status.reason.starts_with("Failed to start: "), "{status:?}");
    assert_eq!(apply().message, "No changes");
}

#[test]
fn a_busy_state_file_keeps_the_workload_until_remove_can_finish() {
    use std::os::windows::fs::OpenOptionsExt;
    let Some(machine) = Machine::start() else {
        return;
    };
    let mut busy = machine.workload("busy", "exit");
    busy.restart = Restart::Never;
    assert!(machine.put(busy).ok);
    let _ = machine.send(Command::Start {
        name: "busy".into(),
    });
    machine.until("busy", |status| status.state == State::Failed);
    let state = machine.data.join("state").join("busy.json");
    let held = std::fs::OpenOptions::new()
        .read(true)
        .share_mode(0)
        .open(&state)
        .unwrap();
    let refused = machine.send(Command::Remove {
        name: "busy".into(),
    });
    assert!(!refused.ok);
    assert!(
        refused
            .message
            .starts_with(&format!("{} is in use; try again", state.display())),
        "{}",
        refused.message
    );
    assert!(scm::exists(&machine.service("busy")).unwrap());
    assert!(machine.data.join("workloads").join("busy.toml").exists());

    drop(held);
    machine.ok(Command::Remove {
        name: "busy".into(),
    });
    assert!(!scm::exists(&machine.service("busy")).unwrap());
    assert!(!state.exists());

    // A new workload of the same name never shows a report left by the old one.
    std::fs::write(
        &state,
        serde_json::to_vec(&files::HostState {
            state: State::Failed,
            reason: "left behind".into(),
            ..Default::default()
        })
        .unwrap(),
    )
    .unwrap();
    assert!(machine.put(machine.workload("busy", "pulse")).ok);
    let fresh = machine.get("busy");
    assert_eq!(fresh.state, State::Stopped, "{fresh:?}");
    assert_ne!(fresh.reason, "left behind");
}

#[test]
fn an_install_folder_users_may_change_is_refused() {
    if !elevated() {
        return;
    }
    let dir = protected_temp_dir();
    let target = dir.path().join("K3 Up");
    k3up::win32::create_dir_with(&target, k3up::win32::PRIVATE_DIR_SDDL).unwrap();
    let host = target.join("k3up-host.exe");
    std::fs::write(&host, b"host").unwrap();
    icacls(&host, &["/setowner", "*S-1-5-32-544"]);
    setup::check_target(&target).unwrap();

    icacls(&target, &["/grant", "*S-1-5-32-545:(OI)(CI)M"]);
    let error = setup::check_target(&target).unwrap_err().to_string();
    assert!(
        error.contains(&format!("\n  {}\n", target.display())),
        "{error}"
    );
    assert!(error.ends_with(&format!(
        "Delete {} and extract the K3 Up release zip into it again.",
        target.display()
    )));
}

#[test]
fn definitions_users_could_edit_are_not_repaired_into_trust() {
    let Some(machine) = Machine::start() else {
        return;
    };
    assert!(machine.put(machine.workload("edited", "pulse")).ok);
    assert!(machine.put(machine.workload("kept", "pulse")).ok);
    setup::prepare(&machine.data).unwrap();

    let edited = machine.data.join("workloads").join("edited.toml");
    icacls(&edited, &["/grant", "*S-1-5-32-545:M"]);
    let error = format!("{:#}", setup::prepare(&machine.data).unwrap_err());
    assert!(
        error.contains(&format!("  {}", edited.display())),
        "{error}"
    );
    assert!(!error.contains("kept.toml"), "{error}");
    assert!(
        error.ends_with("apply the workloads again from a trusted manifest"),
        "{error}"
    );
    assert!(k3up::win32::writable_by_others(&edited).unwrap());
}

#[test]
fn enabling_and_disabling_take_turns_across_the_machine() {
    let Some(machine) = Machine::start() else {
        return;
    };
    let prefix = machine.prefix.clone();
    let short = Duration::from_millis(300);
    let other_thread = |prefix: &str| {
        std::thread::scope(|scope| {
            scope
                .spawn(|| setup::setup_lock(prefix, short).map(drop))
                .join()
                .unwrap()
        })
    };
    let held = setup::setup_lock(&prefix, short).unwrap();
    let error = other_thread(&prefix).unwrap_err();
    assert_eq!(
        error.to_string(),
        "Another k3up command is enabling or disabling services mode; try again"
    );
    let denied = as_standard_user(|| setup::setup_lock(&prefix, short).map(drop)).unwrap_err();
    assert!(format!("{denied:#}").contains("os error 5"), "{denied:#}");
    drop(held);
    other_thread(&prefix).unwrap();

    // A holder that ends without releasing the lock leaves it to the next.
    std::thread::scope(|scope| {
        scope.spawn(|| std::mem::forget(setup::setup_lock(&prefix, short).unwrap()));
    });
    drop(setup::setup_lock(&prefix, short).unwrap());
}

#[test]
fn a_setup_lock_another_account_created_is_refused() {
    let Some(machine) = Machine::start() else {
        return;
    };
    let output = std::process::Command::new("whoami.exe")
        .args(["/user", "/fo", "csv", "/nh"])
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&output.stdout);
    let user = text.trim().rsplit(',').next().unwrap().trim_matches('"');
    assert!(user.starts_with("S-1-5-21-"), "{text}");
    let name = format!(r"Global\K3UpTestSquat_{}", machine.prefix);
    let squatted = k3up::win32::lock_mutex(
        &name,
        &format!("O:{user}D:(A;;GA;;;WD)"),
        Duration::from_secs(1),
    )
    .unwrap()
    .unwrap();
    let error = std::thread::scope(|scope| {
        scope
            .spawn(|| k3up::win32::lock_mutex(&name, "D:P(A;;GA;;;SY)(A;;GA;;;BA)", WAIT).map(drop))
            .join()
            .unwrap()
    })
    .unwrap_err();
    assert_eq!(
        error.to_string(),
        format!(
            "{name} belongs to an account other than SYSTEM or Administrators. Restart Windows and try again"
        )
    );
    drop(squatted);
}

#[test]
fn names_that_differ_only_in_case_change_nothing() {
    let Some(machine) = Machine::start() else {
        return;
    };
    let workloads = machine.data.join("workloads");
    let both = machine.apply(
        &[
            machine.workload("Web", "pulse"),
            machine.workload("web", "pulse"),
        ],
        false,
    );
    assert!(!both.ok);
    assert_eq!(
        both.message,
        "The manifest names both 'Web' and 'web'; Windows treats names that differ only in case as the same"
    );
    assert!(!scm::exists(&machine.service("web")).unwrap());
    assert!(files::names(&workloads, "toml").unwrap().is_empty());

    assert!(machine.put(machine.workload("web", "pulse")).ok);
    let config = machine.sc("qc", "web");
    let saved = std::fs::read(workloads.join("web.toml")).unwrap();
    let mut renamed = machine.workload("Web", "pulse");
    renamed.description = "Renamed".into();
    let expected = "A workload named 'web' already exists; Windows treats names that differ only in case as the same";
    for dry_run in [true, false] {
        let refused = machine.apply(std::slice::from_ref(&renamed), dry_run);
        assert!(!refused.ok);
        assert_eq!(refused.message, expected);
    }
    let created = machine.send(Command::Put {
        workload: Box::new(renamed),
        create_only: true,
    });
    assert_eq!(created.message, expected);
    assert_eq!(machine.sc("qc", "web"), config);
    assert_eq!(std::fs::read(workloads.join("web.toml")).unwrap(), saved);
    assert_eq!(files::names(&workloads, "toml").unwrap(), ["web"]);

    let mut web = machine.workload("web", "pulse");
    web.description = "Front".into();
    let updated = machine.put(web);
    assert_eq!(updated.message, "Update web");
}

#[test]
fn a_start_type_changed_outside_k3up_is_restored_while_running() {
    let Some(machine) = Machine::start() else {
        return;
    };
    let web = machine.workload("drift", "pulse");
    assert!(machine.put(web.clone()).ok);
    let config = machine.sc("qc", "drift");
    assert!(config.contains("DEMAND_START"), "{config}");
    machine.ok(Command::Start {
        name: "drift".into(),
    });
    let before = machine.until("drift", |status| status.state == State::Running);
    machine.sc_config("drift", &["start=", "auto"]);
    assert!(machine.sc("qc", "drift").contains("AUTO_START"));

    let preview = machine.apply(std::slice::from_ref(&web), true);
    assert_eq!(
        preview.message,
        "Preview only\nUpdate drift: service settings differ (start type)"
    );
    assert!(machine.sc("qc", "drift").contains("AUTO_START"));
    let applied = machine.apply(std::slice::from_ref(&web), false);
    assert_eq!(
        applied.message,
        "Update drift: service settings differ (start type)"
    );
    assert_eq!(machine.sc("qc", "drift"), config);
    let after = machine.get("drift");
    assert_eq!(after.state, State::Running);
    assert_eq!(after.pid, before.pid);
    assert_eq!(
        machine.apply(std::slice::from_ref(&web), false).message,
        "No changes"
    );
}

#[test]
fn a_command_line_or_account_changed_outside_k3up_is_restored() {
    let Some(machine) = Machine::start() else {
        return;
    };
    let web = machine.workload("binpath", "pulse");
    assert!(machine.put(web.clone()).ok);
    let config = machine.sc("qc", "binpath");
    let other = r"C:\Windows\System32\cmd.exe /c exit 0";
    machine.sc_config("binpath", &["binPath=", other]);
    assert!(machine.sc("qc", "binpath").contains(other));
    let applied = machine.apply(std::slice::from_ref(&web), false);
    assert_eq!(
        applied.message,
        "Update binpath: service settings differ (command line)"
    );
    assert_eq!(machine.sc("qc", "binpath"), config);

    machine.sc_config(
        "binpath",
        &["obj=", r"NT AUTHORITY\LocalService", "password=", ""],
    );
    let applied = machine.apply(std::slice::from_ref(&web), false);
    assert_eq!(
        applied.message,
        "Update binpath: service settings differ (account)"
    );
    assert_eq!(machine.sc("qc", "binpath"), config);

    machine.ok(Command::Start {
        name: "binpath".into(),
    });
    machine.until("binpath", |status| status.state == State::Running);
    machine.sc_config("binpath", &["binPath=", other]);
    let refused = machine.apply(std::slice::from_ref(&web), false);
    assert!(!refused.ok);
    assert_eq!(
        refused.message,
        "Stop 'binpath' before changing its definition: its service settings differ (command line)"
    );
    assert!(machine.sc("qc", "binpath").contains(other));
    machine.ok(Command::Stop {
        name: "binpath".into(),
    });
    assert!(machine.apply(std::slice::from_ref(&web), false).ok);
    assert_eq!(machine.sc("qc", "binpath"), config);
    machine.ok(Command::Start {
        name: "binpath".into(),
    });
    machine.until("binpath", |status| status.state == State::Running);
}

#[test]
fn a_state_file_held_while_the_workload_starts_is_written_once_released() {
    use std::os::windows::fs::OpenOptionsExt;
    let Some(machine) = Machine::start() else {
        return;
    };
    assert!(machine.put(machine.workload("late", "pulse")).ok);
    let state = machine.data.join("state").join("late.json");
    let held = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .share_mode(0)
        .open(&state)
        .unwrap();
    machine.ok(Command::Start {
        name: "late".into(),
    });
    machine.until_logged("late", "pid ");
    let pid = machine
        .log("late")
        .lines()
        .filter_map(|line| line.strip_prefix("pid ")?.trim().parse::<u32>().ok())
        .next_back();
    std::thread::sleep(Duration::from_secs(3));
    let waiting = machine.get("late");
    assert_eq!(waiting.state, State::Pending, "{waiting:?}");
    assert_eq!(waiting.pid, None);
    assert!(machine.log("late").contains("State not saved yet"));

    drop(held);
    let released = Instant::now();
    let running = machine.until("late", |status| status.state == State::Running);
    assert!(
        released.elapsed() < Duration::from_secs(5),
        "{:?}",
        released.elapsed()
    );
    assert_eq!(running.pid, pid);
}

#[test]
fn a_running_workload_whose_definition_cannot_be_read_is_neither_job_nor_service() {
    use std::os::windows::fs::OpenOptionsExt;
    let Some(machine) = Machine::start() else {
        return;
    };
    let mut job = machine.workload("job", "pulse");
    job.kind = Kind::Job;
    for workload in [job, machine.workload("web", "pulse")] {
        let name = workload.name.clone();
        assert!(machine.put(workload).ok);
        machine.ok(Command::Start { name: name.clone() });
        machine.until(&name, |status| status.state == State::Running);
    }
    let split = || setup::running_workloads(&machine.data, &machine.prefix);
    assert_eq!(
        split().unwrap(),
        (vec!["job".to_string()], vec!["web".to_string()])
    );

    let definition = machine.data.join("workloads").join("job.toml");
    let held = std::fs::OpenOptions::new()
        .read(true)
        .share_mode(0)
        .open(&definition)
        .unwrap();
    assert_eq!(
        split().unwrap_err().to_string(),
        "Can't read the definition of job to tell whether it is a job; try again when it is readable"
    );
    drop(held);
    assert_eq!(split().unwrap().0, ["job"]);
}

#[test]
fn a_final_state_held_back_by_a_reader_is_saved_before_the_host_exits() {
    use std::os::windows::fs::OpenOptionsExt;
    let Some(machine) = Machine::start() else {
        return;
    };
    let mut job = machine.workload("final", "exit");
    job.kind = Kind::Job;
    job.success_exit_codes = vec![3];
    job.environment.insert("K3UP_EXIT_CODE".into(), "3".into());
    assert!(machine.put(job).ok);
    let state = machine.data.join("state").join("final.json");
    let held = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .share_mode(0)
        .open(&state)
        .unwrap();
    machine.ok(Command::Start {
        name: "final".into(),
    });
    machine.until_logged("final", "State not saved yet");
    std::thread::sleep(Duration::from_secs(1));
    assert!(machine.sc("query", "final").contains("RUNNING"));
    drop(held);
    let done = machine.until("final", |status| status.state == State::Completed);
    assert_eq!(done.last_exit, Some(3));
    assert_eq!(
        done.reason,
        "Process exited with code 3, counted as success"
    );
    assert!(!machine.log("final").contains("Exited without saving"));
}

#[test]
fn an_event_held_back_by_a_reader_is_recorded_once_released() {
    use std::os::windows::fs::OpenOptionsExt;
    let Some(machine) = Machine::start() else {
        return;
    };
    assert!(machine.put(machine.workload("quiet", "pulse")).ok);
    let events = machine.data.join("events").join("quiet.jsonl");
    let held = std::fs::OpenOptions::new()
        .read(true)
        .share_mode(windows_sys::Win32::Storage::FileSystem::FILE_SHARE_READ)
        .open(&events)
        .unwrap();
    machine.ok(Command::Start {
        name: "quiet".into(),
    });
    machine.until("quiet", |status| status.state == State::Running);
    machine.until_logged("quiet", "Activity history not updated yet");
    let started = || {
        machine
            .ok(Command::Events {
                name: Some("quiet".into()),
                after: None,
            })
            .events
            .iter()
            .any(|event| event.message == "Process started")
    };
    std::thread::sleep(Duration::from_secs(2));
    assert!(!started());
    drop(held);
    let released = Instant::now();
    while !started() {
        assert!(
            released.elapsed() < Duration::from_secs(5),
            "{}",
            machine.log("quiet")
        );
        std::thread::sleep(Duration::from_millis(200));
    }
}

#[test]
fn a_definition_with_an_upper_case_extension_is_checked_too() {
    let Some(machine) = Machine::start() else {
        return;
    };
    let workloads = machine.data.join("workloads");
    let upper = workloads.join("X.TOML");
    let text = files::definition_text(&machine.workload("X", "pulse"), None).unwrap();
    std::fs::write(&upper, text).unwrap();
    icacls(&upper, &["/setowner", "*S-1-5-32-544"]);
    icacls(&upper, &["/grant", "*S-1-5-32-545:M"]);
    let error = format!("{:#}", setup::prepare(&machine.data).unwrap_err());
    assert!(error.contains(&format!("  {}", upper.display())), "{error}");
    assert!(k3up::win32::writable_by_others(&upper).unwrap());
    assert_eq!(files::names(&workloads, "toml").unwrap(), ["X"]);
    let listed = machine.ok(Command::List).workloads;
    let names: Vec<&str> = listed
        .iter()
        .map(|status| status.workload.name.as_str())
        .collect();
    assert_eq!(names, ["X"]);
}

#[test]
fn recovery_commands_and_dependencies_set_outside_k3up_survive_a_failed_change() {
    let Some(machine) = Machine::start() else {
        return;
    };
    let web = machine.workload("recovery", "pulse");
    assert!(machine.put(web.clone()).ok);
    let service = machine.service("recovery");
    let output = std::process::Command::new("sc.exe")
        .args(["failure", &service, "reset=", "3600"])
        .args(["actions=", "run/5000/restart/10000"])
        .args(["command=", "cmd /c echo x"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stdout)
    );
    machine.sc_config("recovery", &["depend=", "Tcpip"]);
    let failure = machine.sc("qfailure", "recovery");
    assert!(failure.contains("cmd /c echo x"), "{failure}");
    let config = machine.sc("qc", "recovery");
    assert!(config.contains("Tcpip"), "{config}");

    scm::FAIL_AFTER_ACCOUNT
        .lock()
        .unwrap()
        .push(service.clone());
    let failed = machine.apply(std::slice::from_ref(&web), false);
    scm::FAIL_AFTER_ACCOUNT
        .lock()
        .unwrap()
        .retain(|name| *name != service);
    assert!(!failed.ok);
    assert!(
        failed
            .message
            .starts_with("Saving failed and every change was undone: "),
        "{}",
        failed.message
    );
    assert_eq!(machine.sc("qfailure", "recovery"), failure);
    assert_eq!(machine.sc("qc", "recovery"), config);
}

#[test]
fn a_dependency_added_outside_k3up_is_removed_while_running() {
    let Some(machine) = Machine::start() else {
        return;
    };
    let web = machine.workload("depends", "pulse");
    assert!(machine.put(web.clone()).ok);
    let config = machine.sc("qc", "depends");
    machine.ok(Command::Start {
        name: "depends".into(),
    });
    let before = machine.until("depends", |status| status.state == State::Running);
    machine.sc_config("depends", &["depend=", "Tcpip"]);
    assert!(machine.sc("qc", "depends").contains("Tcpip"));

    let applied = machine.apply(std::slice::from_ref(&web), false);
    assert_eq!(
        applied.message,
        "Update depends: service settings differ (dependencies)"
    );
    assert_eq!(machine.sc("qc", "depends"), config);
    let after = machine.get("depends");
    assert_eq!(after.state, State::Running);
    assert_eq!(after.pid, before.pid);
    assert_eq!(
        machine.apply(std::slice::from_ref(&web), false).message,
        "No changes"
    );
}
