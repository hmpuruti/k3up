//! The k3up command line against a real agent.
#![cfg(unix)]

use k3up::{
    model::{Kind, Manifest, Missed, RestartBackoff, RestartLimit, State, Status},
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
        "--success-exit-code",
        "3",
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
    assert_eq!(workload.success_exit_codes, [3]);
    let schedule = workload.schedule.as_ref().unwrap();
    assert_eq!(schedule.cron.as_deref(), Some("0 0 2 * * *"));
    assert_eq!(schedule.timezone, "Europe/Berlin");
    assert_eq!(schedule.missed, Missed::RunOnce);
    assert!(status.next_run.is_some());

    let (success, text) = agent.run(&["show", "backup"]);
    assert!(success);
    let manifest: Manifest = toml::from_str(&text).unwrap();
    assert_eq!(manifest.workloads, vec![workload.clone()]);
    let printed: toml::Value = toml::from_str(&text).unwrap();
    for field in [
        "restart",
        "max_restarts",
        "restart_delay_secs",
        "restart_backoff",
    ] {
        assert!(printed["workloads"][0].get(field).is_none(), "{field}");
    }
    assert!(text.contains("success_exit_codes = [3]"), "{text}");
}

#[test]
fn restart_settings_are_refused_for_jobs() {
    let agent = Agent::start();
    for flags in [
        vec!["--restart", "always"],
        vec!["--max-restarts", "unlimited"],
        vec!["--restart-delay", "5"],
        vec!["--restart-backoff", "fixed"],
    ] {
        let arguments = [&["create", "loader", "--exe", "sleep", "--job"], &flags[..]].concat();
        let (success, response) = agent.call(&arguments);
        assert!(!success, "{flags:?}");
        assert_eq!(response.message, "Restart settings apply to services only");
    }
    let (success, _) = agent.call(&["create", "loader", "--exe", "sleep", "--job"]);
    assert!(success);
    let (success, response) = agent.call(&["edit", "loader", "--max-restarts", "3"]);
    assert!(!success);
    assert_eq!(response.message, "Restart settings apply to services only");
    let (success, response) = agent.call(&["create", "web", "--exe", "sleep", "--", "300"]);
    assert!(success, "{}", response.message);
    let (success, response) = agent.call(&["edit", "web", "--job=true", "--restart", "always"]);
    assert!(!success);
    assert_eq!(response.message, "Restart settings apply to services only");
    let (success, response) = agent.call(&["edit", "web", "--job=false", "--restart", "always"]);
    assert!(success, "{}", response.message);
}

#[test]
fn unlimited_restarts_and_success_codes_round_trip() {
    let agent = Agent::start();
    let (success, response) = agent.call(&[
        "create",
        "sentinel",
        "--exe",
        "sleep",
        "--restart",
        "always",
        "--max-restarts",
        "unlimited",
        "--restart-backoff",
        "fixed",
        "--restart-delay",
        "5",
        "--success-exit-code",
        "3",
        "--success-exit-code",
        "75",
        "--",
        "300",
    ]);
    assert!(success, "{}", response.message);
    let workload = agent.show("sentinel").workload;
    assert_eq!(workload.max_restarts, RestartLimit::Unlimited);
    assert_eq!(workload.restart_backoff, RestartBackoff::Fixed);
    assert_eq!(workload.restart_delay_secs, 5);
    assert_eq!(workload.success_exit_codes, [3, 75]);
    let (success, text) = agent.run(&["show", "sentinel"]);
    assert!(success);
    assert!(text.contains("max_restarts = \"unlimited\""), "{text}");
    assert!(text.contains("restart_backoff = \"fixed\""), "{text}");
    assert_eq!(
        toml::from_str::<Manifest>(&text).unwrap().workloads,
        vec![workload]
    );

    let (success, response) = agent.call(&["edit", "sentinel", "--max-restarts", "10"]);
    assert!(success, "{}", response.message);
    assert_eq!(
        agent.show("sentinel").workload.max_restarts,
        RestartLimit::Count(10)
    );
    let (success, response) = agent.call(&["edit", "sentinel", "--clear-success-exit-codes"]);
    assert!(success, "{}", response.message);
    assert!(
        agent
            .show("sentinel")
            .workload
            .success_exit_codes
            .is_empty()
    );
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_k3up"))
        .arg("--data-dir")
        .arg(agent.dir.path())
        .args(["edit", "sentinel", "--max-restarts", "many"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&output.stderr).contains("unlimited"));
    let (success, response) = agent.call(&["edit", "sentinel", "--success-exit-code", "0"]);
    assert!(!success);
    assert!(
        response.message.contains("Exit code 0"),
        "{}",
        response.message
    );
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
    assert_eq!(workload.max_restarts, RestartLimit::Count(3));
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

fn create_grouped(agent: &Agent, name: &str, group: &str, extra: &[&str]) {
    let mut arguments = vec!["create", name, "--exe", "sleep"];
    if !group.is_empty() {
        arguments.extend(["--group", group]);
    }
    arguments.extend(extra);
    arguments.extend(["--", "300"]);
    let (success, response) = agent.call(&arguments);
    assert!(success, "{name}: {}", response.message);
}

#[test]
fn list_prints_folders_as_headers_and_filters_by_folder() {
    let agent = Agent::start();
    create_grouped(&agent, "entra-users", "watchtower/entra", &[]);
    create_grouped(&agent, "intune-policies", "Watchtower/Intune", &[]);
    create_grouped(&agent, "imperva", "watchtower/imperva", &[]);
    create_grouped(&agent, "solo", "", &[]);
    create_grouped(&agent, "entra-devices", "watchtower/entra", &[]);
    let (success, text) = agent.run(&["list"]);
    assert!(success);
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines[1], "watchtower/entra");
    assert!(lines[2].starts_with("  entra-devices "), "{}", lines[2]);
    assert!(lines[3].starts_with("  entra-users "), "{}", lines[3]);
    assert_eq!(lines[4], "watchtower/imperva");
    assert!(lines[5].starts_with("  imperva "), "{}", lines[5]);
    assert_eq!(lines[6], "watchtower/Intune");
    assert!(lines[7].starts_with("  intune-policies "), "{}", lines[7]);
    assert!(lines[8].starts_with("solo "), "{}", lines[8]);
    assert_eq!(lines.len(), 9);
    let state_column = lines[0].find("STATE").unwrap();
    for line in [lines[2], lines[8]] {
        assert_eq!(line.find("stopped"), Some(state_column), "{line}");
    }

    let (success, text) = agent.run(&["list", "--group", "WATCHTOWER/entra"]);
    assert!(success);
    assert!(
        text.contains("entra-users") && !text.contains("imperva"),
        "{text}"
    );
    let (success, response) = agent.call(&["list", "--group", "watchtower"]);
    assert!(success);
    assert_eq!(response.workloads.len(), 4);
    let (success, response) = agent.call(&["list", "--group", "watchtower/en"]);
    assert!(!success);
    assert!(
        response.message.contains("'watchtower/en'"),
        "{}",
        response.message
    );
    let (success, response) = agent.call(&["list", "--group", "a//b"]);
    assert!(!success && response.message.contains("empty segments"));

    let (success, value) = agent.json(&["list"]);
    assert!(success);
    let workloads = value["workloads"].as_array().unwrap();
    assert_eq!(workloads[0]["workload"]["name"], "entra-devices");
    assert_eq!(workloads[0]["workload"]["group"], "watchtower/entra");
    assert_eq!(workloads[4]["workload"]["name"], "solo");
    assert_eq!(workloads[4]["workload"]["group"], "");
    let (success, text) = agent.run(&["show", "entra-users"]);
    assert!(success);
    assert!(text.contains("group = \"watchtower/entra\""), "{text}");
    let (success, text) = agent.run(&["show", "solo"]);
    assert!(success && !text.contains("group"), "{text}");
}

#[test]
fn groups_reports_the_folder_tree_with_counts() {
    let agent = Agent::start();
    create_grouped(&agent, "entra-users", "watchtower/entra", &[]);
    create_grouped(&agent, "solo", "", &[]);
    let (success, response) = agent.call(&[
        "create",
        "crash",
        "--exe",
        "false",
        "--group",
        "watchtower/intune",
        "--job",
    ]);
    assert!(success, "{}", response.message);
    let (success, _) = agent.call(&["start", "entra-users", "--wait", "--timeout", "20"]);
    assert!(success);
    let (success, _) = agent.call(&["start", "crash", "--wait", "--timeout", "20"]);
    assert!(!success);
    let (success, value) = agent.json(&["groups"]);
    assert!(success, "{value}");
    assert_eq!(
        value,
        serde_json::json!([
            { "path": "watchtower", "workloads": 2, "running": 1, "attention": 1 },
            { "path": "watchtower/entra", "workloads": 1, "running": 1, "attention": 0 },
            { "path": "watchtower/intune", "workloads": 1, "running": 0, "attention": 1 },
        ])
    );
    let (success, text) = agent.run(&["groups"]);
    assert!(success);
    let lines: Vec<&str> = text.lines().collect();
    assert!(lines[0].starts_with("FOLDER"), "{text}");
    assert!(
        lines[1].starts_with("watchtower ") && lines[1].ends_with("1 running, 1 failed"),
        "{text}"
    );
    assert!(
        lines[2].starts_with("  entra ") && lines[2].ends_with("1 running"),
        "{text}"
    );
    assert!(
        lines[3].starts_with("  intune ") && lines[3].ends_with("1 failed"),
        "{text}"
    );
    assert!(
        lines[4].starts_with("(no folder)") && lines[4].ends_with("1 stopped"),
        "{text}"
    );
    let (success, _) = agent.call(&["stop", "entra-users"]);
    assert!(success);
    let empty = Agent::start();
    let (success, value) = empty.json(&["groups"]);
    assert!(success);
    assert_eq!(value, serde_json::json!([]));
    let (_, text) = empty.run(&["groups"]);
    assert_eq!(text.trim(), "No workloads");
}

#[test]
fn group_start_and_stop_follow_dependency_order() {
    let agent = Agent::start();
    create_grouped(&agent, "db", "stack/data", &[]);
    create_grouped(&agent, "api", "stack/app", &["--depends-on", "db"]);
    create_grouped(&agent, "web", "stack/app", &["--depends-on", "api"]);
    create_grouped(&agent, "other", "elsewhere", &[]);
    let (success, text) = agent.run(&["start", "--group", "Stack", "--wait", "--timeout", "20"]);
    assert!(success, "{text}");
    assert_eq!(
        text.lines().collect::<Vec<_>>(),
        ["Started db", "Started api", "Started web", "3 started"]
    );
    for name in ["db", "api", "web"] {
        assert_eq!(agent.show(name).state, State::Running, "{name}");
    }
    assert_eq!(agent.show("other").state, State::Stopped);

    let (success, response) = agent.call(&["restart", "--group", "stack/app", "--wait"]);
    assert!(success, "{}", response.message);
    assert_eq!(
        response.message,
        "Restarted api\nRestarted web\n2 restarted"
    );
    assert_eq!(response.workloads.len(), 2);

    let (success, text) = agent.run(&["stop", "--group", "stack"]);
    assert!(success, "{text}");
    assert_eq!(
        text.lines().collect::<Vec<_>>(),
        ["Stopped web", "Stopped api", "Stopped db", "3 stopped"]
    );
    for name in ["db", "api", "web"] {
        let status = agent.show(name);
        assert!(status.pid.is_none() && !status.desired_running, "{name}");
    }

    let (success, response) = agent.call(&["start", "--group", "nope"]);
    assert!(!success);
    assert_eq!(response.message, "No workloads in folder 'nope'");
    let (success, response) = agent.call(&["stop", "--group", "stack/ap"]);
    assert!(!success);
    assert!(response.message.contains("'stack/ap'"));
}

#[test]
fn group_start_attempts_every_workload_and_exits_one_on_failure() {
    let agent = Agent::start();
    let (success, _) = agent.call(&["create", "bad", "--exe", "false", "--group", "g", "--job"]);
    assert!(success);
    create_grouped(&agent, "good", "g", &[]);
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_k3up"))
        .arg("--data-dir")
        .arg(agent.dir.path())
        .args(["start", "--group", "g", "--wait", "--timeout", "20"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    let text = String::from_utf8(output.stdout).unwrap();
    let lines: Vec<&str> = text.lines().collect();
    assert!(lines[0].starts_with("Failed bad: bad failed"), "{text}");
    assert_eq!(lines[1], "Started good");
    assert_eq!(lines[2], "1 started, 1 failed");
    assert_eq!(agent.show("good").state, State::Running);
    let (success, response) = agent.call(&["start", "--group", "g", "--wait", "--timeout", "20"]);
    assert!(!success);
    assert!(!response.ok);
    assert_eq!(response.workloads.len(), 2);
    assert!(response.message.ends_with("1 started, 1 failed"));
}

#[test]
fn export_group_keeps_only_that_folder() {
    let agent = Agent::start();
    create_grouped(&agent, "a", "x/y", &[]);
    create_grouped(&agent, "b", "x", &[]);
    create_grouped(&agent, "c", "z", &[]);
    let (success, response) = agent.call(&["export", "--group", "x"]);
    assert!(success, "{}", response.message);
    let names: Vec<_> = response
        .manifest
        .unwrap()
        .workloads
        .iter()
        .map(|workload| workload.name.clone())
        .collect();
    assert_eq!(names, ["a", "b"]);
    let file = agent.dir.path().join("z.toml");
    let (success, _) = agent.call(&["export", "--group", "Z", "--output", file.to_str().unwrap()]);
    assert!(success);
    let manifest: Manifest = toml::from_str(&std::fs::read_to_string(&file).unwrap()).unwrap();
    assert_eq!(manifest.workloads.len(), 1);
    assert_eq!(manifest.workloads[0].name, "c");
    assert_eq!(manifest.workloads[0].group, "z");
    let (success, response) = agent.call(&["export", "--group", "nope"]);
    assert!(!success);
    assert_eq!(response.message, "No workloads in folder 'nope'");
}

#[test]
fn editing_only_labels_keeps_a_running_workload_running() {
    let agent = Agent::start();
    let (success, _) = agent.call(&["create", "runner", "--exe", "sleep", "--", "300"]);
    assert!(success);
    let (success, response) = agent.call(&["start", "runner", "--wait", "--timeout", "20"]);
    assert!(success, "{}", response.message);
    let pid = response.workloads[0].pid.unwrap();

    let (success, response) = agent.call(&["edit", "runner", "--group", "Watchtower/Entra"]);
    assert!(success, "{}", response.message);
    let status = agent.show("runner");
    assert_eq!(status.workload.group, "Watchtower/Entra");
    assert_eq!(status.pid, Some(pid));
    assert_eq!(status.state, State::Running);
    let (success, _) = agent.call(&["edit", "runner", "--description", "Entra users"]);
    assert!(success);
    let (success, _) = agent.call(&["edit", "runner", "--clear-group"]);
    assert!(success);
    let status = agent.show("runner");
    assert_eq!(status.workload.group, "");
    assert_eq!(status.workload.description, "Entra users");
    assert_eq!(status.pid, Some(pid));
    let (success, response) = agent.call(&["edit", "runner", "--group", "a b"]);
    assert!(
        success && response.message == "Update runner",
        "{}",
        response.message
    );
    let (success, response) = agent.call(&["edit", "runner", "--group", "a b"]);
    assert!(success && response.message == "No changes");

    let (success, response) = agent.call(&["edit", "runner", "--env", "X=1"]);
    assert!(!success);
    assert!(
        response.message.contains("--restart-running"),
        "{}",
        response.message
    );
    assert_eq!(agent.show("runner").pid, Some(pid));
    let (success, response) =
        agent.call(&["edit", "runner", "--group", "ops", "--restart-running"]);
    assert!(success, "{}", response.message);
    let status = agent.show("runner");
    assert_eq!(status.workload.group, "ops");
    assert_ne!(status.pid, Some(pid));

    let (success, response) = agent.call(&["edit", "runner", "--group", "a/b/c/d/e/f"]);
    assert!(
        !success && response.message.contains("5 levels"),
        "{}",
        response.message
    );
    let (success, response) = agent.call(&["events", "runner", "--limit", "20"]);
    assert!(success);
    assert!(
        response
            .events
            .iter()
            .any(|event| event.message.contains("group or description changed"))
    );
}
