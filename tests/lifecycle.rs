use k3up::{engine::Engine, model::*, protocol::Command};
use std::{collections::BTreeMap, time::Duration};

// The test executable doubles as a portable child-process fixture. No shell dependency.
#[test]
#[ignore = "child process fixture; invoked by lifecycle tests"]
fn fixture() {
    use std::io::Write;
    match std::env::var("K3UP_FIXTURE").unwrap_or_default().as_str() {
        "exit" => std::process::exit(
            std::env::var("K3UP_EXIT_CODE")
                .ok()
                .and_then(|code| code.parse().ok())
                .unwrap_or(7),
        ),
        "job" => {
            println!("job completed");
        }
        "tcp" => {
            let listener =
                std::net::TcpListener::bind(std::env::var("K3UP_ADDRESS").unwrap()).unwrap();
            println!("listening");
            std::io::stdout().flush().unwrap();
            for connection in listener.incoming() {
                drop(connection.unwrap());
            }
        }
        "tree" => {
            let mut child = std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", "fixture", "--ignored", "--nocapture"])
                .env("K3UP_FIXTURE", "pulse")
                .spawn()
                .unwrap();
            std::fs::write(
                std::env::var("K3UP_CHILD_PID").unwrap(),
                child.id().to_string(),
            )
            .unwrap();
            let _ = child.wait();
        }
        "pulse" => loop {
            println!("pulse");
            std::io::stdout().flush().unwrap();
            std::thread::sleep(Duration::from_millis(100));
        },
        _ => panic!("Fixture requires K3UP_FIXTURE"),
    }
}

fn spec(name: &str, mode: &str, directory: &std::path::Path) -> Workload {
    Workload {
        name: name.into(),
        executable: std::env::current_exe().unwrap().to_string_lossy().into(),
        working_directory: directory.to_string_lossy().into(),
        args: vec![
            "--exact".into(),
            "fixture".into(),
            "--ignored".into(),
            "--nocapture".into(),
        ],
        environment: BTreeMap::from([("K3UP_FIXTURE".into(), mode.into())]),
        stop_timeout_secs: 1,
        restart_delay_secs: 1,
        ..Default::default()
    }
}
async fn put(engine: &mut Engine, spec: Workload) {
    engine
        .handle(Command::Put {
            workload: Box::new(spec),
            create_only: true,
        })
        .await
        .unwrap();
}
async fn status(engine: &mut Engine, name: &str) -> Status {
    engine
        .handle(Command::Get { name: name.into() })
        .await
        .unwrap()
        .workloads
        .remove(0)
}
async fn until(engine: &mut Engine, name: &str, wanted: &str) -> Status {
    let deadline = std::time::Instant::now() + Duration::from_secs(8);
    loop {
        engine.tick().await.unwrap();
        let state = status(engine, name).await;
        if state.state == wanted {
            return state;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "Wanted {wanted}, got {} ({})",
            state.state,
            state.reason
        );
        tokio::time::sleep(Duration::from_millis(60)).await;
    }
}

#[tokio::test]
async fn duplicate_start_is_idempotent_and_manual_stop_survives_reopen() {
    let dir = tempfile::tempdir().unwrap();
    let mut engine = Engine::open(dir.path()).unwrap();
    put(&mut engine, spec("worker", "pulse", dir.path())).await;
    engine
        .handle(Command::Start {
            name: "worker".into(),
        })
        .await
        .unwrap();
    let first = status(&mut engine, "worker").await.pid;
    engine
        .handle(Command::Start {
            name: "worker".into(),
        })
        .await
        .unwrap();
    assert_eq!(status(&mut engine, "worker").await.pid, first);
    tokio::time::sleep(Duration::from_millis(250)).await;
    let output = engine
        .handle(Command::Logs {
            name: "worker".into(),
            lines: 100,
            after: None,
        })
        .await
        .unwrap()
        .text
        .unwrap();
    assert!(output.contains("pulse"));
    engine
        .handle(Command::Stop {
            name: "worker".into(),
        })
        .await
        .unwrap();
    drop(engine);
    let mut engine = Engine::open(dir.path()).unwrap();
    engine.tick().await.unwrap();
    let stopped = status(&mut engine, "worker").await;
    assert!(!stopped.desired_running);
    assert!(stopped.pid.is_none());
}

#[tokio::test]
async fn restart_budget_ends_a_crash_loop() {
    let dir = tempfile::tempdir().unwrap();
    let mut engine = Engine::open(dir.path()).unwrap();
    let mut workload = spec("crasher", "exit", dir.path());
    workload.max_restarts = RestartLimit::Count(1);
    put(&mut engine, workload).await;
    engine
        .handle(Command::Start {
            name: "crasher".into(),
        })
        .await
        .unwrap();
    let failed = until(&mut engine, "crasher", "failed").await;
    assert_eq!(failed.restart_count, 1);
    assert_eq!(failed.last_exit, Some(7));
    assert!(failed.reason.contains("restart limit"));
    assert!(!failed.desired_running);
}

#[tokio::test]
async fn unlimited_restarts_keep_retrying_with_a_fixed_delay() {
    let dir = tempfile::tempdir().unwrap();
    let mut engine = Engine::open(dir.path()).unwrap();
    let mut workload = spec("sentinel", "exit", dir.path());
    workload.restart = Restart::Always;
    workload.max_restarts = RestartLimit::Unlimited;
    workload.restart_backoff = RestartBackoff::Fixed;
    put(&mut engine, workload).await;
    engine
        .handle(Command::Start {
            name: "sentinel".into(),
        })
        .await
        .unwrap();
    let deadline = std::time::Instant::now() + Duration::from_secs(30);
    let retrying = loop {
        engine.tick().await.unwrap();
        let current = status(&mut engine, "sentinel").await;
        assert_ne!(current.state, "failed", "{}", current.reason);
        if current.restart_count >= 6 {
            break current;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "only {} retries: {}",
            current.restart_count,
            current.reason
        );
        if current.state == "backoff" {
            assert!(engine.next_wake() <= Duration::from_millis(1005));
        }
        tokio::time::sleep(Duration::from_millis(60)).await;
    };
    assert!(retrying.desired_running);
    assert!(
        retrying.reason.contains("retry 6 in 1s"),
        "{}",
        retrying.reason
    );
    engine
        .handle(Command::Stop {
            name: "sentinel".into(),
        })
        .await
        .unwrap();
}

#[tokio::test]
async fn listed_exit_codes_complete_a_job_and_release_its_dependents() {
    let dir = tempfile::tempdir().unwrap();
    let mut engine = Engine::open(dir.path()).unwrap();
    let mut loader = spec("loader", "exit", dir.path());
    loader.kind = Kind::Job;
    loader.success_exit_codes = vec![3];
    loader
        .environment
        .insert("K3UP_EXIT_CODE".into(), "3".into());
    put(&mut engine, loader).await;
    let mut worker = spec("worker", "pulse", dir.path());
    worker.depends_on.push("loader".into());
    put(&mut engine, worker).await;
    engine
        .handle(Command::Start {
            name: "worker".into(),
        })
        .await
        .unwrap();
    until(&mut engine, "worker", "running").await;
    let completed = status(&mut engine, "loader").await;
    assert_eq!(completed.state, "completed");
    assert_eq!(completed.last_exit, Some(3));
    assert!(
        completed
            .reason
            .contains("exited with code 3, counted as success"),
        "{}",
        completed.reason
    );
    engine.shutdown().await.unwrap();
}

#[tokio::test]
async fn on_failure_services_are_not_restarted_after_a_listed_exit_code() {
    let dir = tempfile::tempdir().unwrap();
    let mut engine = Engine::open(dir.path()).unwrap();
    let mut service = spec("service", "exit", dir.path());
    service.success_exit_codes = vec![3];
    service
        .environment
        .insert("K3UP_EXIT_CODE".into(), "3".into());
    put(&mut engine, service).await;
    engine
        .handle(Command::Start {
            name: "service".into(),
        })
        .await
        .unwrap();
    let completed = until(&mut engine, "service", "completed").await;
    assert_eq!(completed.restart_count, 0);
    assert_eq!(completed.last_exit, Some(3));
    assert!(!completed.desired_running);
}

#[tokio::test]
async fn jobs_complete_and_schedules_persist() {
    let dir = tempfile::tempdir().unwrap();
    let mut engine = Engine::open(dir.path()).unwrap();
    let mut workload = spec("backup", "job", dir.path());
    workload.kind = Kind::Job;
    workload.schedule = Some(Schedule {
        every_secs: Some(1),
        cron: None,
        timezone: "UTC".into(),
        action: ScheduleAction::Start,
        missed: Missed::Skip,
    });
    put(&mut engine, workload).await;
    until(&mut engine, "backup", "completed").await;
    let saved = status(&mut engine, "backup").await;
    assert_eq!(saved.last_exit, Some(0));
    assert_eq!(saved.restart_count, 0);
    drop(engine);
    let mut engine = Engine::open(dir.path()).unwrap();
    assert!(status(&mut engine, "backup").await.next_run.is_some());
}

#[tokio::test]
async fn dependencies_wait_for_actual_tcp_readiness() {
    let dir = tempfile::tempdir().unwrap();
    let mut engine = Engine::open(dir.path()).unwrap();
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap().to_string();
    drop(listener);
    let mut database = spec("database", "tcp", dir.path());
    database.readiness_tcp = Some(address.clone());
    database.environment.insert("K3UP_ADDRESS".into(), address);
    put(&mut engine, database).await;
    let mut worker = spec("worker", "pulse", dir.path());
    worker.depends_on.push("database".into());
    put(&mut engine, worker).await;
    engine
        .handle(Command::Start {
            name: "worker".into(),
        })
        .await
        .unwrap();
    assert_eq!(status(&mut engine, "worker").await.state, "blocked");
    until(&mut engine, "worker", "running").await;
    assert_eq!(status(&mut engine, "database").await.state, "running");
    engine.shutdown().await.unwrap();
}

#[tokio::test]
async fn rejected_batch_has_no_partial_writes_and_dry_run_is_read_only() {
    let dir = tempfile::tempdir().unwrap();
    let mut engine = Engine::open(dir.path()).unwrap();
    let first = spec("first", "pulse", dir.path());
    let mut second = spec("second", "pulse", dir.path());
    second.depends_on.push("missing".into());
    let invalid = Manifest {
        version: 1,
        workloads: vec![first.clone(), second],
    };
    assert!(
        engine
            .handle(Command::Apply {
                manifest: invalid,
                dry_run: false
            })
            .await
            .is_err()
    );
    assert!(
        engine
            .handle(Command::List)
            .await
            .unwrap()
            .workloads
            .is_empty()
    );
    engine
        .handle(Command::Apply {
            manifest: Manifest {
                version: 1,
                workloads: vec![first],
            },
            dry_run: true,
        })
        .await
        .unwrap();
    assert!(
        engine
            .handle(Command::List)
            .await
            .unwrap()
            .workloads
            .is_empty()
    );
}

#[tokio::test]
async fn refuses_live_configuration_changes_and_reaps_timed_out_jobs() {
    let dir = tempfile::tempdir().unwrap();
    let mut engine = Engine::open(dir.path()).unwrap();
    let mut workload = spec("limited", "pulse", dir.path());
    workload.kind = Kind::Job;
    workload.run_timeout_secs = Some(1);
    put(&mut engine, workload.clone()).await;
    engine
        .handle(Command::Start {
            name: "limited".into(),
        })
        .await
        .unwrap();
    workload.args.push("--changed".into());
    assert!(
        engine
            .handle(Command::Put {
                workload: Box::new(workload),
                create_only: false
            })
            .await
            .is_err()
    );
    let failed = until(&mut engine, "limited", "failed").await;
    assert_eq!(failed.last_exit, Some(124));
    assert!(failed.pid.is_none());
}

#[tokio::test]
async fn relabelling_a_running_workload_keeps_its_process_and_persists() {
    let dir = tempfile::tempdir().unwrap();
    let mut engine = Engine::open(dir.path()).unwrap();
    let mut workload = spec("worker", "pulse", dir.path());
    put(&mut engine, workload.clone()).await;
    engine
        .handle(Command::Start {
            name: "worker".into(),
        })
        .await
        .unwrap();
    let running = until(&mut engine, "worker", "running").await;
    workload.group = "Watchtower/Entra".into();
    workload.description = "Users".into();
    let response = engine
        .handle(Command::Put {
            workload: Box::new(workload.clone()),
            create_only: false,
        })
        .await
        .unwrap();
    assert_eq!(response.message, "Update worker");
    let relabelled = status(&mut engine, "worker").await;
    assert_eq!(relabelled.workload, workload);
    assert_eq!(relabelled.pid, running.pid);
    assert_eq!(relabelled.state, "running");
    assert_eq!(relabelled.reason, running.reason);
    assert!(relabelled.desired_running);
    workload.group = "a//b".into();
    assert!(
        engine
            .handle(Command::Put {
                workload: Box::new(workload.clone()),
                create_only: false,
            })
            .await
            .is_err()
    );
    engine.shutdown().await.unwrap();
    drop(engine);
    let mut engine = Engine::open(dir.path()).unwrap();
    let reopened = status(&mut engine, "worker").await;
    assert_eq!(reopened.workload.group, "Watchtower/Entra");
    assert_eq!(reopened.workload.description, "Users");
    assert!(reopened.desired_running);
    engine.shutdown().await.unwrap();
}

#[cfg(unix)]
#[tokio::test]
async fn stopping_parent_terminates_child_process_group() {
    let dir = tempfile::tempdir().unwrap();
    let mut engine = Engine::open(dir.path()).unwrap();
    let pid_file = dir.path().join("child.pid");
    let mut workload = spec("tree", "tree", dir.path());
    workload
        .environment
        .insert("K3UP_CHILD_PID".into(), pid_file.to_string_lossy().into());
    put(&mut engine, workload).await;
    engine
        .handle(Command::Start {
            name: "tree".into(),
        })
        .await
        .unwrap();
    for _ in 0..100 {
        if pid_file.exists() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let pid: i32 = std::fs::read_to_string(pid_file).unwrap().parse().unwrap();
    engine
        .handle(Command::Stop {
            name: "tree".into(),
        })
        .await
        .unwrap();
    let mut alive = true;
    for _ in 0..100 {
        // SAFETY: signal 0 only tests whether the process exists.
        alive = unsafe { libc::kill(pid, 0) } == 0;
        if !alive {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(!alive, "Child process {pid} survived stop");
}

#[cfg(unix)]
struct Agent {
    dir: tempfile::TempDir,
    process: std::process::Child,
}

#[cfg(unix)]
impl Agent {
    fn start() -> Self {
        // Socket paths are length-limited, so avoid the long default temp directory on macOS.
        let dir = tempfile::Builder::new()
            .prefix("bs-")
            .tempdir_in("/tmp")
            .unwrap();
        let process = std::process::Command::new(env!("CARGO_BIN_EXE_k3up-agent"))
            .arg("--data-dir")
            .arg(dir.path())
            .stdout(std::process::Stdio::null())
            .spawn()
            .unwrap();
        for _ in 0..100 {
            if dir.path().join("agent.sock").exists() {
                break;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        Self { dir, process }
    }

    fn call(&self, arguments: &[&str]) -> (bool, k3up::protocol::Response) {
        let output = std::process::Command::new(env!("CARGO_BIN_EXE_k3up"))
            .arg("--data-dir")
            .arg(self.dir.path())
            .arg("--json")
            .args(arguments)
            .output()
            .unwrap();
        (
            output.status.success(),
            serde_json::from_slice(&output.stdout).unwrap(),
        )
    }
}

#[cfg(unix)]
impl Drop for Agent {
    fn drop(&mut self) {
        let _ = self.process.kill();
        let _ = self.process.wait();
    }
}

#[cfg(unix)]
#[test]
fn cli_and_daemon_share_real_socket_and_json_errors() {
    let agent = Agent::start();
    let (success, response) = agent.call(&["list"]);
    assert!(success && response.ok);
    let (success, response) = agent.call(&["start", "missing"]);
    assert!(!success && !response.ok);
    let duplicate = std::process::Command::new(env!("CARGO_BIN_EXE_k3up-agent"))
        .arg("--data-dir")
        .arg(agent.dir.path())
        .output()
        .unwrap();
    assert!(!duplicate.status.success());
}

#[cfg(unix)]
#[test]
fn start_wait_reports_job_outcome() {
    let agent = Agent::start();
    let mut succeeding = spec("succeeds", "job", agent.dir.path());
    succeeding.kind = Kind::Job;
    let mut failing = spec("fails", "exit", agent.dir.path());
    failing.kind = Kind::Job;
    let manifest = agent.dir.path().join("jobs.toml");
    std::fs::write(
        &manifest,
        toml::to_string(&Manifest {
            version: 1,
            workloads: vec![succeeding, failing],
        })
        .unwrap(),
    )
    .unwrap();
    let (success, _) = agent.call(&["apply", manifest.to_str().unwrap()]);
    assert!(success);
    let (success, response) = agent.call(&["start", "succeeds", "--wait", "--timeout", "20"]);
    assert!(success, "{}", response.message);
    assert_eq!(response.workloads[0].state, "completed");
    let (success, response) = agent.call(&["start", "fails", "--wait", "--timeout", "20"]);
    assert!(!success);
    assert!(response.message.contains("code 7"), "{}", response.message);
}

#[cfg(unix)]
#[test]
fn overlong_data_directory_is_reported_clearly() {
    let dir = tempfile::tempdir().unwrap();
    let deep = dir.path().join("x".repeat(120));
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_k3up-agent"))
        .arg("--data-dir")
        .arg(&deep)
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("too long for a local socket"));
}

#[tokio::test]
async fn exhausted_schedule_does_not_block_agent_start() {
    let dir = tempfile::tempdir().unwrap();
    let mut engine = Engine::open(dir.path()).unwrap();
    let mut yearly = spec("yearly", "job", dir.path());
    yearly.kind = Kind::Job;
    yearly.schedule = Some(Schedule {
        every_secs: None,
        cron: Some("0 0 2 1 1 * 2099".into()),
        timezone: "UTC".into(),
        action: ScheduleAction::Start,
        missed: Missed::Skip,
    });
    put(&mut engine, yearly).await;
    drop(engine);
    // Simulate the final run passing while the agent was down.
    rusqlite::Connection::open(dir.path().join("k3up.db"))
        .unwrap()
        .execute(
            "UPDATE workloads SET spec = replace(spec, '2099', '2020'), next_run = '2020-01-01T02:00:00+00:00'",
            [],
        )
        .unwrap();
    let mut engine = Engine::open(dir.path()).unwrap();
    let saved = status(&mut engine, "yearly").await;
    assert_eq!(saved.next_run, None);
    assert!(saved.reason.contains("no further executions"));
    engine.tick().await.unwrap();
    engine
        .handle(Command::Remove {
            name: "yearly".into(),
        })
        .await
        .unwrap();
}

#[tokio::test]
async fn failed_dependency_fails_its_dependent() {
    let dir = tempfile::tempdir().unwrap();
    let mut engine = Engine::open(dir.path()).unwrap();
    let mut migrate = spec("migrate", "exit", dir.path());
    migrate.kind = Kind::Job;
    put(&mut engine, migrate).await;
    let mut worker = spec("worker", "pulse", dir.path());
    worker.depends_on.push("migrate".into());
    put(&mut engine, worker).await;
    engine
        .handle(Command::Start {
            name: "worker".into(),
        })
        .await
        .unwrap();
    let failed = until(&mut engine, "worker", "failed").await;
    assert!(
        failed.reason.contains("dependency migrate"),
        "{}",
        failed.reason
    );
    assert!(!failed.desired_running);
    assert_eq!(status(&mut engine, "worker").await.pid, None);
}

#[tokio::test]
async fn logs_follow_from_an_offset() {
    let dir = tempfile::tempdir().unwrap();
    let mut engine = Engine::open(dir.path()).unwrap();
    put(&mut engine, spec("worker", "pulse", dir.path())).await;
    engine
        .handle(Command::Start {
            name: "worker".into(),
        })
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;
    let logs = |after| Command::Logs {
        name: "worker".into(),
        lines: 100,
        after,
    };
    let first = engine.handle(logs(None)).await.unwrap();
    let offset = first.offset.unwrap();
    assert!(first.text.unwrap().contains("Starting worker"));
    tokio::time::sleep(Duration::from_millis(300)).await;
    let next = engine.handle(logs(Some(offset))).await.unwrap();
    let text = next.text.unwrap();
    assert!(text.contains("pulse") && !text.contains("Starting worker"));
    assert_eq!(next.offset.unwrap(), offset + text.len() as u64);
    let rotated = engine.handle(logs(Some(u64::MAX))).await.unwrap();
    assert!(rotated.text.unwrap().contains("Starting worker"));
    engine.shutdown().await.unwrap();
}

#[tokio::test]
async fn recovered_service_reruns_its_job_dependency() {
    let dir = tempfile::tempdir().unwrap();
    let mut engine = Engine::open(dir.path()).unwrap();
    let mut migrate = spec("migrate", "job", dir.path());
    migrate.kind = Kind::Job;
    put(&mut engine, migrate).await;
    let mut worker = spec("worker", "pulse", dir.path());
    worker.depends_on.push("migrate".into());
    put(&mut engine, worker).await;
    engine
        .handle(Command::Start {
            name: "worker".into(),
        })
        .await
        .unwrap();
    until(&mut engine, "worker", "running").await;
    drop(engine);
    let mut engine = Engine::open(dir.path()).unwrap();
    until(&mut engine, "worker", "running").await;
    assert_eq!(status(&mut engine, "migrate").await.state, "completed");
    engine.shutdown().await.unwrap();
}

#[cfg(unix)]
#[test]
fn restarted_agent_stops_workloads_left_by_a_crash() {
    let mut agent = Agent::start();
    let manifest = agent.dir.path().join("pulse.toml");
    std::fs::write(
        &manifest,
        toml::to_string(&Manifest {
            version: 1,
            workloads: vec![spec("worker", "pulse", agent.dir.path())],
        })
        .unwrap(),
    )
    .unwrap();
    assert!(agent.call(&["apply", manifest.to_str().unwrap()]).0);
    let (_, response) = agent.call(&["start", "worker", "--wait", "--timeout", "20"]);
    let orphan = response.workloads[0].pid.unwrap() as i32;
    agent.process.kill().unwrap();
    agent.process.wait().unwrap();
    // SAFETY: signal 0 only checks that the process exists.
    assert_eq!(
        unsafe { libc::kill(orphan, 0) },
        0,
        "crash should orphan the workload"
    );

    let _ = std::fs::remove_file(agent.dir.path().join("agent.sock"));
    agent.process = std::process::Command::new(env!("CARGO_BIN_EXE_k3up-agent"))
        .arg("--data-dir")
        .arg(agent.dir.path())
        .stdout(std::process::Stdio::null())
        .spawn()
        .unwrap();
    for _ in 0..100 {
        if agent.dir.path().join("agent.sock").exists() {
            break;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    let (_, response) = agent.call(&["start", "worker", "--wait", "--timeout", "20"]);
    let replacement = response.workloads[0].pid.unwrap() as i32;
    assert_ne!(replacement, orphan);
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    // SAFETY: as above; the orphan is not our child, so it disappears without a wait.
    while unsafe { libc::kill(orphan, 0) } == 0 {
        assert!(
            std::time::Instant::now() < deadline,
            "orphan {orphan} survived restart"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
    let (_, events) = agent.call(&["events", "worker"]);
    assert!(
        events
            .events
            .iter()
            .any(|event| event.message.contains("left running by a previous agent"))
    );
}

#[tokio::test]
async fn idle_agent_sleeps_until_something_is_due() {
    let dir = tempfile::tempdir().unwrap();
    let mut engine = Engine::open(dir.path()).unwrap();
    assert_eq!(engine.next_wake(), Duration::from_secs(15));
    let mut boot = spec("boot", "pulse", dir.path());
    boot.start_at_boot = true;
    put(&mut engine, boot).await;
    assert!(engine.next_wake() <= Duration::from_millis(250));
    until(&mut engine, "boot", "running").await;
    put(&mut engine, spec("worker", "pulse", dir.path())).await;
    engine
        .handle(Command::Start {
            name: "worker".into(),
        })
        .await
        .unwrap();
    let running = engine.next_wake();
    // Unix wakes on SIGCHLD, so it can sleep; Windows polls running processes every second.
    if cfg!(unix) {
        assert!(running > Duration::from_secs(5), "{running:?}");
    } else {
        assert!(running <= Duration::from_millis(1005), "{running:?}");
    }
    engine.shutdown().await.unwrap();
}

#[tokio::test]
async fn events_and_metrics_are_incremental() {
    let dir = tempfile::tempdir().unwrap();
    let mut engine = Engine::open(dir.path()).unwrap();
    put(&mut engine, spec("worker", "pulse", dir.path())).await;
    let events = |after| Command::Events { name: None, after };
    let all = engine.handle(events(None)).await.unwrap().events;
    let newest = all[0].id;
    assert!(
        engine
            .handle(events(Some(newest)))
            .await
            .unwrap()
            .events
            .is_empty()
    );
    engine
        .handle(Command::Start {
            name: "worker".into(),
        })
        .await
        .unwrap();
    let fresh = engine.handle(events(Some(newest))).await.unwrap().events;
    assert!(!fresh.is_empty() && fresh.iter().all(|event| event.id > newest));

    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    let metrics = loop {
        engine.tick().await.unwrap();
        let response = engine
            .handle(Command::Metrics { since: None })
            .await
            .unwrap();
        if let Some(metrics) = response.metrics {
            break metrics;
        }
        assert!(std::time::Instant::now() < deadline, "no metrics");
        tokio::time::sleep(Duration::from_millis(50)).await;
    };
    let later = engine
        .handle(Command::Metrics {
            since: Some(metrics.at),
        })
        .await
        .unwrap()
        .metrics
        .unwrap();
    assert!(later.history.iter().all(|point| point.at > metrics.at));
    engine.shutdown().await.unwrap();
}

#[cfg(unix)]
#[test]
fn watch_returns_on_change_and_waits_otherwise() {
    let agent = Agent::start();
    let client = k3up::client::Client::new(agent.dir.path());
    let generation = |since, timeout_ms| {
        client
            .send(Command::Watch { since, timeout_ms })
            .unwrap()
            .generation
            .unwrap()
    };
    let current = generation(0, 0);
    let started = std::time::Instant::now();
    assert_eq!(generation(current, 300), current);
    assert!(started.elapsed() >= Duration::from_millis(250));

    let directory = agent.dir.path().to_path_buf();
    let change = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(200));
        std::process::Command::new(env!("CARGO_BIN_EXE_k3up"))
            .arg("--data-dir")
            .arg(directory)
            .args(["create", "sleeper", "--exe", "/bin/sleep", "--", "30"])
            .output()
            .unwrap();
    });
    let started = std::time::Instant::now();
    assert!(generation(current, 10_000) > current);
    assert!(started.elapsed() < Duration::from_secs(5));
    change.join().unwrap();
}
