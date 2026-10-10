//! Real Windows services created through the services back end and the k3up command line.
//! Creating services needs administrator rights; without them each test reports that it was
//! skipped, unless K3UP_REQUIRE_ELEVATION is set, as in CI, where skipping is a failure.
#![cfg(windows)]

use k3up::{
    model::{
        Kind, Manifest, Restart, RestartBackoff, RestartLimit, Schedule, State, Status, Workload,
    },
    protocol::{Command, Response},
    services::{Backend, Settings, files, scm, setup},
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
        "pulse" => loop {
            println!("tick");
            std::io::stdout().flush().unwrap();
            std::thread::sleep(Duration::from_millis(200));
        },
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
    machine.ok(Command::Stop {
        name: "looping".into(),
    });
    assert!(requested.elapsed() < Duration::from_secs(30));
    assert!(machine.sc("query", "looping").contains("STOPPED"));
    drop(held);
    assert!(
        machine
            .log("looping")
            .contains("Activity history not updated yet")
    );
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
