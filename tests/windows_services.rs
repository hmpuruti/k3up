//! Real Windows services created through the services back end and the k3up command line.
//! Creating services needs administrator rights; without them each test reports that it was
//! skipped, unless K3UP_REQUIRE_ELEVATION is set, as in CI, where skipping is a failure.
#![cfg(windows)]

use k3up::{
    model::{Kind, Restart, RestartBackoff, RestartLimit, Schedule, State, Status, Workload},
    protocol::{Command, Response},
    services::{Backend, Settings, scm, setup},
};
use std::{
    collections::BTreeMap,
    path::PathBuf,
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
        "pulse" => loop {
            println!("tick");
            std::io::stdout().flush().unwrap();
            std::thread::sleep(Duration::from_millis(200));
        },
        _ => panic!("Fixture requires K3UP_FIXTURE"),
    }
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
        let dir = tempfile::tempdir().unwrap();
        let data = dir.path().join("data");
        setup::prepare(&data).unwrap();
        let prefix = format!(
            "K3UpTest{}n{}_",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::SeqCst)
        );
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

    fn workload(&self, name: &str, mode: &str) -> Workload {
        Workload {
            name: name.into(),
            executable: std::env::current_exe().unwrap().to_string_lossy().into(),
            working_directory: self.data.to_string_lossy().into(),
            args: ["--exact", "fixture", "--ignored", "--nocapture"]
                .map(String::from)
                .to_vec(),
            environment: BTreeMap::from([("K3UP_FIXTURE".into(), mode.into())]),
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

    fn cli(&self, arguments: &[&str]) -> (bool, String) {
        let output = std::process::Command::new(CLI)
            .arg("--data-dir")
            .arg(&self.data)
            .arg("--json")
            .args(arguments)
            .env("K3UP_SERVICE_PREFIX", &self.prefix)
            .env("K3UP_HOST", HOST)
            .output()
            .unwrap();
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
