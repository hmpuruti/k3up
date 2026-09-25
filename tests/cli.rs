//! The k3up command line against a real agent.
#![cfg(unix)]

use k3up::{
    model::{Kind, Manifest, Missed, Restart, State, Status},
    protocol::Response,
};
use std::time::{Duration, Instant};

struct Agent {
    dir: tempfile::TempDir,
    process: std::process::Child,
}

impl Agent {
    fn start() -> Self {
        // Socket paths are length-limited, so avoid the long default temp directory on macOS.
        let dir = tempfile::Builder::new()
            .prefix("k3cli-")
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

    fn run(&self, arguments: &[&str]) -> (bool, String) {
        let output = std::process::Command::new(env!("CARGO_BIN_EXE_k3up"))
            .arg("--data-dir")
            .arg(self.dir.path())
            .args(arguments)
            .output()
            .unwrap();
        (
            output.status.success(),
            String::from_utf8(output.stdout).unwrap(),
        )
    }

    fn call(&self, arguments: &[&str]) -> (bool, Response) {
        let (success, stdout) = self.run(&[&["--json"], arguments].concat());
        (success, serde_json::from_str(&stdout).unwrap())
    }

    fn json(&self, arguments: &[&str]) -> (bool, serde_json::Value) {
        let (success, stdout) = self.run(&[&["--json"], arguments].concat());
        (success, serde_json::from_str(&stdout).unwrap())
    }

    fn show(&self, name: &str) -> Status {
        let (success, value) = self.json(&["show", name]);
        assert!(success, "{value}");
        serde_json::from_value(value).unwrap()
    }
}

impl Drop for Agent {
    fn drop(&mut self) {
        let _ = self.process.kill();
        let _ = self.process.wait();
    }
}

#[test]
fn create_round_trips_through_show() {
    let agent = Agent::start();
    let (success, response) = agent.call(&[
        "create",
        "backup",
        "--exe",
        "sleep",
        "--job",
        "--description",
        "Nightly backup",
        "--env",
        "A=1",
        "--env",
        "B=two",
        "--restart",
        "never",
        "--cron",
        "0 0 2 * * *",
        "--timezone",
        "Europe/Berlin",
        "--catch-up",
        "--",
        "300",
    ]);
    assert!(success, "{}", response.message);
    let status = agent.show("backup");
    let workload = &status.workload;
    assert_eq!(workload.kind, Kind::Job);
    assert_eq!(workload.description, "Nightly backup");
    assert!(
        workload.executable.ends_with("/sleep"),
        "{}",
        workload.executable
    );
    assert_eq!(workload.args, ["300"]);
    assert_eq!(workload.environment["A"], "1");
    assert_eq!(workload.environment["B"], "two");
    assert_eq!(workload.restart, Restart::Never);
    let schedule = workload.schedule.as_ref().unwrap();
    assert_eq!(schedule.cron.as_deref(), Some("0 0 2 * * *"));
    assert_eq!(schedule.timezone, "Europe/Berlin");
    assert_eq!(schedule.missed, Missed::RunOnce);
    assert!(status.next_run.is_some());

    let (success, text) = agent.run(&["show", "backup"]);
    assert!(success);
    let manifest: Manifest = toml::from_str(&text).unwrap();
    assert_eq!(manifest.workloads, vec![workload.clone()]);
}

#[test]
fn edit_changes_only_what_was_given() {
    let agent = Agent::start();
    let (success, _) = agent.call(&[
        "create",
        "backup",
        "--exe",
        "sleep",
        "--job",
        "--env",
        "A=1",
        "--env",
        "B=two",
        "--cron",
        "0 0 2 * * *",
        "--",
        "300",
    ]);
    assert!(success);
    let (success, response) = agent.call(&[
        "edit",
        "backup",
        "--unset-env",
        "A",
        "--env",
        "C=3",
        "--max-restarts",
        "3",
        "--job=false",
        "--stop-timeout",
        "7",
        "--",
        "200",
    ]);
    assert!(success, "{}", response.message);
    let workload = agent.show("backup").workload;
    assert_eq!(workload.kind, Kind::Service);
    assert_eq!(workload.args, ["200"]);
    assert_eq!(workload.max_restarts, 3);
    assert_eq!(workload.stop_timeout_secs, 7);
    assert_eq!(workload.environment.keys().collect::<Vec<_>>(), ["B", "C"]);
    assert_eq!(
        workload.schedule.as_ref().unwrap().cron.as_deref(),
        Some("0 0 2 * * *")
    );
    let (success, response) = agent.call(&["edit", "backup", "--clear-schedule", "--clear-env"]);
    assert!(success, "{}", response.message);
    let workload = agent.show("backup").workload;
    assert!(workload.schedule.is_none() && workload.environment.is_empty());
    let (success, response) = agent.call(&["edit", "backup", "--clear-env"]);
    assert!(success && response.message == "No changes");
}

#[test]
fn edit_restart_running_stops_applies_and_starts_again() {
    let agent = Agent::start();
    let (success, _) = agent.call(&["create", "runner", "--exe", "sleep", "--", "300"]);
    assert!(success);
    let (success, response) = agent.call(&["start", "runner", "--wait", "--timeout", "20"]);
    assert!(success, "{}", response.message);
    let first = response.workloads[0].pid.unwrap();

    let (success, response) = agent.call(&["edit", "runner", "--env", "X=1"]);
    assert!(!success);
    assert!(
        response.message.contains("--restart-running"),
        "{}",
        response.message
    );
    assert_eq!(agent.show("runner").pid, Some(first));

    let (success, response) = agent.call(&["edit", "runner", "--env", "X=1", "--restart-running"]);
    assert!(success, "{}", response.message);
    let status = agent.show("runner");
    assert_eq!(status.workload.environment["X"], "1");
    assert!(status.desired_running);
    assert_ne!(status.pid, Some(first));

    let (success, response) = agent.call(&["events", "runner", "--limit", "2"]);
    assert!(success && response.events.len() == 2);
    let (success, response) = agent.call(&["remove", "runner", "--stop"]);
    assert!(success, "{}", response.message);
    let (_, response) = agent.call(&["list"]);
    assert!(response.workloads.is_empty());
}

#[test]
fn create_start_wait_reports_a_completed_job() {
    let agent = Agent::start();
    let (success, response) = agent.call(&[
        "create",
        "quick",
        "--exe",
        "true",
        "--job",
        "--start",
        "--wait",
        "--timeout",
        "20",
    ]);
    assert!(success, "{}", response.message);
    assert_eq!(response.workloads[0].state, State::Completed);
    assert_eq!(response.workloads[0].last_exit, Some(0));
}

#[test]
fn health_json_has_the_documented_shape() {
    let agent = Agent::start();
    let (success, health) = agent.json(&["health"]);
    assert!(success, "{health}");
    assert!(matches!(health["status"].as_str(), Some("ok" | "warning")));
    assert!(health["warnings"].is_array() && health["attention"].is_array());
    assert!(health["machine"]["cores"].as_u64().unwrap() > 0);
    assert_eq!(health["k3up"]["workload_count"], 0);
    assert!(health["k3up"]["agent"]["memory"].as_u64().unwrap() > 0);
    for warning in health["warnings"].as_array().unwrap() {
        assert!(warning["kind"].is_string() && warning["message"].is_string());
    }
}

#[test]
fn stats_for_one_workload_reports_its_usage() {
    let agent = Agent::start();
    let (success, _) = agent.call(&["create", "runner", "--exe", "sleep", "--", "300"]);
    assert!(success);
    let (success, _) = agent.call(&["start", "runner", "--wait", "--timeout", "20"]);
    assert!(success);
    let deadline = Instant::now() + Duration::from_secs(15);
    let response = loop {
        let (success, response) = agent.call(&["stats", "runner"]);
        assert!(success, "{}", response.message);
        let sampled = response
            .metrics
            .as_ref()
            .is_some_and(|metrics| !metrics.workloads.is_empty());
        if sampled || Instant::now() >= deadline {
            break response;
        }
        std::thread::sleep(Duration::from_millis(250));
    };
    assert_eq!(response.workloads[0].state, State::Running);
    let usage = &response.metrics.unwrap().workloads[0];
    assert_eq!(usage.name, "runner");
    assert!(usage.usage.processes >= 1);
    assert!(usage.pids.contains(&response.workloads[0].pid.unwrap()));
    assert!(response.text.unwrap().contains("Processes"));
}

#[test]
fn agent_status_reports_the_running_agent() {
    let agent = Agent::start();
    let (success, _) = agent.call(&["create", "runner", "--exe", "sleep", "--", "300"]);
    assert!(success);
    let (success, status) = agent.json(&["agent", "status"]);
    assert!(success, "{status}");
    assert_eq!(status["reachable"], true);
    assert_eq!(status["pid"], agent.process.id());
    assert_eq!(status["version"], env!("CARGO_PKG_VERSION"));
    assert_eq!(status["workloads"]["total"], 1);
    assert_eq!(status["workloads"]["running"], 0);
    assert!(
        status["executable"]
            .as_str()
            .unwrap()
            .contains("k3up-agent")
    );
    assert_eq!(status["login_item"], false);
}

#[test]
fn agent_stop_makes_the_agent_exit() {
    let mut agent = Agent::start();
    let (success, _) = agent.call(&["create", "runner", "--exe", "sleep", "--", "300"]);
    assert!(success);
    let (success, _) = agent.call(&["start", "runner", "--wait", "--timeout", "20"]);
    assert!(success);
    let (success, response) = agent.call(&["agent", "stop", "--timeout", "30"]);
    assert!(success, "{}", response.message);
    let deadline = Instant::now() + Duration::from_secs(10);
    let exit = loop {
        if let Some(exit) = agent.process.try_wait().unwrap() {
            break exit;
        }
        assert!(Instant::now() < deadline, "agent still running");
        std::thread::sleep(Duration::from_millis(50));
    };
    assert!(exit.success(), "{exit}");
    let (success, status) = agent.json(&["agent", "status"]);
    assert!(!success);
    assert_eq!(status["reachable"], false);
    let (success, response) = agent.call(&["agent", "stop"]);
    assert!(success && response.message.contains("not running"));
}

#[test]
fn template_output_applies() {
    let agent = Agent::start();
    let (success, template) = agent.run(&["template"]);
    assert!(success);
    let file = agent.dir.path().join("template.toml");
    std::fs::write(&file, template).unwrap();
    let (success, response) = agent.call(&["validate", file.to_str().unwrap()]);
    assert!(success, "{}", response.message);
    let (success, response) = agent.call(&["apply", file.to_str().unwrap()]);
    assert!(success, "{}", response.message);
    assert!(response.message.contains("Create web") && response.message.contains("Create migrate"));
    let (success, response) = agent.call(&["apply", file.to_str().unwrap()]);
    assert!(success && response.message == "No changes");
    let (_, response) = agent.call(&["export"]);
    let names: Vec<_> = response
        .manifest
        .unwrap()
        .workloads
        .into_iter()
        .map(|workload| workload.name)
        .collect();
    assert_eq!(names, ["migrate", "web"]);
}

#[test]
fn usage_errors_exit_with_two_and_bad_programs_fail_locally() {
    let agent = Agent::start();
    let status = std::process::Command::new(env!("CARGO_BIN_EXE_k3up"))
        .args(["create", "web"])
        .output()
        .unwrap()
        .status;
    assert_eq!(status.code(), Some(2));
    let (success, response) = agent.call(&["create", "web", "--exe", "no-such-program-k3up"]);
    assert!(!success);
    assert!(
        response.message.contains("not found on PATH"),
        "{}",
        response.message
    );
    let (success, response) = agent.call(&["create", "web", "--exe", "sleep", "--every", "0"]);
    assert!(
        !success && response.message.contains("Schedule"),
        "{}",
        response.message
    );
}
