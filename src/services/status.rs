//! Turns what the service manager, the definition and the host's state file say about a
//! workload into the `Status` every client already understands.
use super::files::HostState;
use crate::model::{Kind, State, Status, Workload};
use anyhow::{Result, bail};
use std::time::Duration;

const DISPLAY_PREFIX: &str = "K3 Up: ";
const SAME_NAME: &str = "Windows treats names that differ only in case as the same";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scm {
    Stopped,
    Starting,
    Stopping,
    Running,
}

/// A workload's Windows service as the service manager reports it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Service {
    pub name: String,
    pub display_name: String,
    pub scm: Scm,
    pub host_pid: Option<u32>,
    /// The definition's instance, from the host's command line, which any user may read.
    pub instance: Option<String>,
}

/// The value after `--instance` in a service's command line.
pub fn instance_of(command_line: &str) -> Option<String> {
    let mut words = split_command_line(command_line).into_iter();
    words.find(|word| word == "--instance")?;
    words.next()
}

/// Splits a command line the way Windows programs read their arguments, so a quoted data
/// directory that contains ` --instance ` is not mistaken for the option.
pub fn split_command_line(line: &str) -> Vec<String> {
    let mut chars = line.trim_start().chars().peekable();
    let mut words = vec![];
    // The program path ends at the closing quote or at whitespace, without escapes.
    let mut program = String::new();
    if chars.peek() == Some(&'"') {
        chars.next();
        program.extend(chars.by_ref().take_while(|c| *c != '"'));
    } else {
        while let Some(c) = chars.next_if(|c| !c.is_whitespace()) {
            program.push(c);
        }
    }
    words.push(program);
    loop {
        while chars.next_if(|c| *c == ' ' || *c == '\t').is_some() {}
        if chars.peek().is_none() {
            return words;
        }
        words.push(next_argument(&mut chars));
    }
}

fn next_argument(chars: &mut std::iter::Peekable<std::str::Chars>) -> String {
    let mut word = String::new();
    let mut quoted = false;
    while let Some(&c) = chars.peek() {
        match c {
            ' ' | '\t' if !quoted => break,
            '\\' => {
                let mut slashes = 0;
                while chars.next_if(|c| *c == '\\').is_some() {
                    slashes += 1;
                }
                if chars.peek() == Some(&'"') {
                    word.extend(std::iter::repeat_n('\\', slashes / 2));
                    if slashes % 2 == 1 {
                        word.push('"');
                        chars.next();
                    }
                } else {
                    word.extend(std::iter::repeat_n('\\', slashes));
                }
            }
            '"' => {
                chars.next();
                if quoted && chars.peek() == Some(&'"') {
                    word.push('"');
                    chars.next();
                } else {
                    quoted = !quoted;
                }
            }
            _ => {
                word.push(c);
                chars.next();
            }
        }
    }
    word
}

#[derive(Debug, Clone, PartialEq)]
pub enum Definition {
    Present(Box<Workload>),
    Missing,
    /// Only administrators may read definitions, since environment variables can hold secrets.
    Hidden,
}

pub fn display_name(workload: &Workload) -> String {
    if workload.group.is_empty() {
        format!("{DISPLAY_PREFIX}{}", workload.name)
    } else {
        format!("{DISPLAY_PREFIX}{}/{}", workload.group, workload.name)
    }
}

pub fn description(workload: &Workload) -> &str {
    if workload.description.is_empty() {
        "K3 Up workload"
    } else {
        &workload.description
    }
}

/// The folder recorded in a display name, for callers that may not read the definition.
pub fn group_from_display(display_name: &str, name: &str) -> String {
    display_name
        .strip_prefix(DISPLAY_PREFIX)
        .and_then(|path| path.strip_suffix(name))
        .and_then(|path| path.strip_suffix('/'))
        .unwrap_or_default()
        .to_string()
}

/// Whether the service starts with Windows. As with the agent, `start_at_boot` applies to
/// services only; a job runs only when started.
pub fn starts_at_boot(workload: &Workload) -> bool {
    workload.start_at_boot && workload.kind == Kind::Service
}

/// Whether a name refers to a workload. A definition the caller may not read counts only when
/// its service exists, since users without administrator rights cannot tell a hidden
/// definition from a missing one.
pub fn known(definition: &Definition, service_exists: bool) -> bool {
    service_exists || matches!(definition, Definition::Present(_))
}

/// Refuses a name in `manifest` that differs only in case from another in it, or from one in
/// `existing`: Windows service and file names ignore case, so both would share one service
/// and one definition. The same name in the same case is an update.
pub fn refuse_case_clashes(manifest: &[&str], existing: &[&str]) -> Result<()> {
    let clash = |names: &[&str], name: &str| {
        names
            .iter()
            .find(|other| other.eq_ignore_ascii_case(name) && **other != name)
            .map(|other| other.to_string())
    };
    for (index, name) in manifest.iter().enumerate() {
        if let Some(other) = clash(&manifest[..index], name) {
            bail!("The manifest names both '{other}' and '{name}'; {SAME_NAME}");
        }
        if let Some(other) = clash(existing, name) {
            bail!("A workload named '{other}' already exists; {SAME_NAME}");
        }
    }
    Ok(())
}

/// What a workload's Windows service is set to: what K3 Up asks for, or what the service
/// manager holds.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ServiceSettings {
    /// The program and each argument, as Windows splits the command line.
    pub command: Vec<String>,
    pub start_type: u32,
    pub delayed: bool,
    pub account: String,
    pub display_name: String,
    pub description: String,
    pub recovery: Recovery,
}

/// What the service manager does when the host crashes.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Recovery {
    /// `None` when the failure count is never reset.
    pub reset: Option<Duration>,
    /// Each action's type, as Windows numbers them, and delay.
    pub actions: Vec<(i32, Duration)>,
    pub on_non_crash_failures: bool,
}

/// The settings of a service that differ from what K3 Up asks for, by name.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Drift(pub Vec<&'static str>);

impl Drift {
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// A running host keeps its command line and account until it stops.
    pub fn needs_stop(&self) -> bool {
        self.0
            .iter()
            .any(|setting| matches!(*setting, "command line" | "account"))
    }
}

impl std::fmt::Display for Drift {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        f.write_str(&self.0.join(", "))
    }
}

pub fn drift(wanted: &ServiceSettings, found: &ServiceSettings) -> Drift {
    let account = |name: &str| {
        let name = name.strip_prefix(".\\").unwrap_or(name);
        name.to_ascii_lowercase()
    };
    Drift(
        [
            ("command line", wanted.command == found.command),
            (
                "start type",
                (wanted.start_type, wanted.delayed) == (found.start_type, found.delayed),
            ),
            (
                "account",
                account(&wanted.account) == account(&found.account),
            ),
            ("display name", wanted.display_name == found.display_name),
            ("description", wanted.description == found.description),
            ("recovery actions", wanted.recovery == found.recovery),
        ]
        .into_iter()
        .filter(|(_, same)| !same)
        .map(|(setting, _)| setting)
        .collect(),
    )
}

/// The one place that lists what services mode cannot run yet.
pub fn ensure_supported(workload: &Workload) -> Result<()> {
    if workload.schedule.is_some() {
        bail!("Schedules are not supported for Windows services yet");
    }
    if !workload.depends_on.is_empty() {
        bail!("Dependencies are not supported for Windows services yet");
    }
    if workload.readiness_tcp.is_some() {
        bail!("TCP readiness checks are not supported for Windows services yet");
    }
    Ok(())
}

pub fn status(
    name: &str,
    definition: Definition,
    service: Option<&Service>,
    host: Option<HostState>,
) -> Status {
    // A report from an earlier workload of the same name says nothing about this one.
    let host = host.filter(|host| service.is_none_or(|service| service.instance == host.instance));
    let workload = match &definition {
        Definition::Present(workload) => (**workload).clone(),
        _ => Workload {
            name: name.into(),
            group: service
                .map(|service| group_from_display(&service.display_name, name))
                .unwrap_or_default(),
            ..Default::default()
        },
    };
    let mut status = Status {
        workload,
        state: State::Stopped,
        desired_running: false,
        pid: None,
        restart_count: 0,
        started_at: None,
        next_run: None,
        last_exit: None,
        reason: "Stopped".into(),
    };
    let Some(service) = service else {
        status.state = State::Failed;
        status.reason =
            format!("Its Windows service is missing; `k3up start {name}` creates it again");
        return status;
    };
    if let Some(host) = &host {
        status.restart_count = host.restart_count;
        status.started_at = host.started_at;
        status.last_exit = host.last_exit;
    }
    // A report from an earlier run of the host says nothing about the running one.
    let current = host
        .clone()
        .filter(|host| service.host_pid == Some(host.host_pid));
    match service.scm {
        Scm::Running => match current {
            Some(host) => {
                status.state = host.state;
                status.pid = host.pid;
                status.reason = host.reason;
                status.desired_running = true;
            }
            None => starting(&mut status),
        },
        Scm::Starting => starting(&mut status),
        Scm::Stopping => status.reason = "Stopping".into(),
        Scm::Stopped => match host {
            // The host records every way it ends, so a live state means it died.
            Some(host)
                if matches!(
                    host.state,
                    State::Running | State::Starting | State::Pending | State::Backoff
                ) =>
            {
                status.state = State::Backoff;
                status.reason =
                    "The K3 Up host ended unexpectedly; the service manager restarts it".into();
                status.desired_running = true;
            }
            Some(host) => {
                status.state = host.state;
                status.reason = host.reason;
            }
            None => {}
        },
    }
    if definition == Definition::Missing {
        status.reason = format!("Definition missing; `k3up remove {name}` deletes its service");
    }
    status
}

fn starting(status: &mut Status) {
    status.state = State::Pending;
    status.reason = "Starting".into();
    status.desired_running = true;
}

/// FNV-1a over every status, stable between processes and releases.
/// `events` is `files::events_mark`, so an event that changes no status still wakes watchers.
pub fn generation(statuses: &[Status], events: u64) -> u64 {
    let text = serde_json::to_string(statuses).unwrap_or_default();
    text.bytes()
        .chain(events.to_le_bytes())
        .fold(0xcbf29ce484222325u64, |hash, byte| {
            (hash ^ byte as u64).wrapping_mul(0x100000001b3)
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn web() -> Workload {
        Workload {
            name: "web".into(),
            group: "shop/front".into(),
            executable: "/bin/sleep".into(),
            working_directory: "/tmp".into(),
            ..Default::default()
        }
    }

    fn service(scm: Scm, host_pid: Option<u32>) -> Service {
        Service {
            name: "K3Up_web".into(),
            display_name: display_name(&web()),
            scm,
            host_pid,
            instance: None,
        }
    }

    #[test]
    fn a_report_from_another_instance_is_ignored() {
        let present = || Definition::Present(Box::new(web()));
        let mut current = service(Scm::Stopped, None);
        current.instance = Some("b2".into());
        let mut stale = report(State::Failed, 9);
        stale.instance = Some("a1".into());
        let fresh = status("web", present(), Some(&current), Some(stale.clone()));
        assert_eq!(fresh.state, State::Stopped);
        assert_eq!(fresh.reason, "Stopped");
        assert_eq!(fresh.restart_count, 0);

        stale.instance = Some("b2".into());
        let own = status("web", present(), Some(&current), Some(stale.clone()));
        assert_eq!(own.state, State::Failed);

        // Without instances on either side, as before they existed, the report applies.
        current.instance = None;
        stale.instance = None;
        assert_eq!(
            status("web", present(), Some(&current), Some(stale.clone())).state,
            State::Failed
        );
        // A service without an instance never takes a report that has one.
        stale.instance = Some("b2".into());
        assert_eq!(
            status("web", present(), Some(&current), Some(stale)).state,
            State::Stopped
        );
    }

    #[test]
    fn a_quoted_path_that_mentions_the_option_is_not_the_option() {
        assert_eq!(
            instance_of(
                r#""C:\k3up-host.exe" --data-dir "C:\svc --instance stale" --workload web --instance 0a1b"#
            ),
            Some("0a1b".into())
        );
    }

    #[test]
    fn command_lines_follow_windows_quoting() {
        assert_eq!(
            split_command_line(r#""C:\Program Files\h.exe" a\\"b c" "d\"e" f\g "h""i" "C:\dir\\""#),
            [
                r"C:\Program Files\h.exe",
                r"a\b c",
                r#"d"e"#,
                r"f\g",
                r#"h"i"#,
                r"C:\dir\",
            ]
        );
    }

    #[test]
    fn the_instance_comes_from_the_command_line() {
        assert_eq!(
            instance_of(
                r#""C:\Program Files\K3 Up\k3up-host.exe" --data-dir "C:\ProgramData\K3 Up" --workload web --instance 0a1b2c"#
            ),
            Some("0a1b2c".into())
        );
        assert_eq!(
            instance_of(r#""C:\k3up-host.exe" --data-dir C:\data --workload web"#),
            None
        );
    }

    fn report(state: State, host_pid: u32) -> HostState {
        HostState {
            state,
            pid: Some(42),
            host_pid,
            restart_count: 2,
            last_exit: Some(1),
            reason: "Process started".into(),
            ..Default::default()
        }
    }

    #[test]
    fn names_and_folders_survive_the_display_name() {
        let mut workload = web();
        assert_eq!(display_name(&workload), "K3 Up: shop/front/web");
        assert_eq!(
            group_from_display("K3 Up: shop/front/web", "web"),
            "shop/front"
        );
        assert_eq!(description(&workload), "K3 Up workload");
        workload.group.clear();
        workload.description = "Shop".into();
        assert_eq!(display_name(&workload), "K3 Up: web");
        assert_eq!(group_from_display("K3 Up: web", "web"), "");
        assert_eq!(description(&workload), "Shop");
    }

    #[test]
    fn only_services_start_at_boot() {
        let mut workload = web();
        assert!(!starts_at_boot(&workload));
        workload.start_at_boot = true;
        assert!(starts_at_boot(&workload));
        workload.kind = Kind::Job;
        assert!(!starts_at_boot(&workload));
    }

    #[test]
    fn a_hidden_definition_without_a_service_is_unknown() {
        let present = Definition::Present(Box::new(web()));
        assert!(known(&present, false));
        assert!(known(&present, true));
        assert!(known(&Definition::Hidden, true));
        assert!(known(&Definition::Missing, true));
        assert!(!known(&Definition::Hidden, false));
        assert!(!known(&Definition::Missing, false));
    }

    #[test]
    fn unsupported_features_are_named() {
        let mut workload = web();
        ensure_supported(&workload).unwrap();
        workload.readiness_tcp = Some("127.0.0.1:80".into());
        assert!(
            ensure_supported(&workload)
                .unwrap_err()
                .to_string()
                .contains("TCP readiness")
        );
        workload.depends_on = vec!["db".into()];
        assert!(
            ensure_supported(&workload)
                .unwrap_err()
                .to_string()
                .contains("Dependencies")
        );
        workload.schedule = Some(crate::model::Schedule {
            every_secs: Some(60),
            cron: None,
            timezone: "UTC".into(),
            action: Default::default(),
            missed: Default::default(),
        });
        assert_eq!(
            ensure_supported(&workload).unwrap_err().to_string(),
            "Schedules are not supported for Windows services yet"
        );
    }

    #[test]
    fn running_hosts_report_their_process() {
        let present = || Definition::Present(Box::new(web()));
        let status = status(
            "web",
            present(),
            Some(&service(Scm::Running, Some(9))),
            Some(report(State::Running, 9)),
        );
        assert_eq!(status.state, State::Running);
        assert_eq!(status.pid, Some(42));
        assert!(status.desired_running);
        assert_eq!(status.restart_count, 2);

        let stale = super::status(
            "web",
            present(),
            Some(&service(Scm::Running, Some(10))),
            Some(report(State::Failed, 9)),
        );
        assert_eq!(stale.state, State::Pending);
        assert_eq!(stale.pid, None);

        let crashed = super::status(
            "web",
            present(),
            Some(&service(Scm::Stopped, None)),
            Some(report(State::Running, 9)),
        );
        assert_eq!(crashed.state, State::Backoff);
        assert!(crashed.reason.contains("ended unexpectedly"));

        let mut finished = report(State::Failed, 9);
        finished.reason = "Process exited with code 1; restart limit reached".into();
        let failed = super::status(
            "web",
            present(),
            Some(&service(Scm::Stopped, None)),
            Some(finished),
        );
        assert_eq!(failed.state, State::Failed);
        assert_eq!(failed.pid, None);
        assert!(!failed.desired_running);
        assert!(failed.reason.ends_with("restart limit reached"));
    }

    #[test]
    fn mismatches_are_explained_and_hidden_definitions_keep_the_folder() {
        let orphan = status(
            "web",
            Definition::Missing,
            Some(&service(Scm::Stopped, None)),
            None,
        );
        assert!(orphan.reason.starts_with("Definition missing"));
        assert_eq!(orphan.workload.group, "shop/front");

        let unregistered = status("web", Definition::Present(Box::new(web())), None, None);
        assert_eq!(unregistered.state, State::Failed);
        assert!(unregistered.reason.contains("service is missing"));

        let hidden = status(
            "web",
            Definition::Hidden,
            Some(&service(Scm::Running, Some(9))),
            Some(report(State::Running, 9)),
        );
        assert_eq!(hidden.workload.group, "shop/front");
        assert!(hidden.workload.executable.is_empty());
        assert_eq!(hidden.state, State::Running);
    }

    #[test]
    fn names_that_differ_only_in_case_are_refused() {
        refuse_case_clashes(&["web", "api"], &["web", "db"]).unwrap();
        assert_eq!(
            refuse_case_clashes(&["Web"], &["web", "db"])
                .unwrap_err()
                .to_string(),
            "A workload named 'web' already exists; Windows treats names that differ only in case as the same"
        );
        assert_eq!(
            refuse_case_clashes(&["api", "Web", "web"], &[])
                .unwrap_err()
                .to_string(),
            "The manifest names both 'Web' and 'web'; Windows treats names that differ only in case as the same"
        );
        refuse_case_clashes(&["web-1", "WEB_1"], &["web_2"]).unwrap();
    }

    fn settings() -> ServiceSettings {
        ServiceSettings {
            command: [r"C:\k3up-host.exe", "--workload", "web"]
                .map(String::from)
                .to_vec(),
            start_type: 3,
            delayed: false,
            account: "LocalSystem".into(),
            display_name: "K3 Up: web".into(),
            description: "K3 Up workload".into(),
            recovery: Recovery {
                reset: Some(Duration::from_secs(86_400)),
                actions: vec![(1, Duration::from_secs(5))],
                on_non_crash_failures: false,
            },
        }
    }

    type Change = fn(&mut ServiceSettings);

    #[test]
    fn every_service_setting_counts_as_drift() {
        let wanted = settings();
        assert!(drift(&wanted, &wanted).is_empty());
        let mut found = settings();
        found.account = r".\localsystem".into();
        assert!(drift(&wanted, &found).is_empty());

        let changes: [(&str, Change); 8] = [
            ("command line", |found| {
                found.command[0] = r"C:\other.exe".into()
            }),
            ("command line", |found| found.command.push("--extra".into())),
            ("start type", |found| found.start_type = 2),
            ("start type", |found| found.delayed = true),
            ("account", |found| found.account = r".\someone".into()),
            ("display name", |found| found.display_name = "Other".into()),
            ("description", |found| found.description.clear()),
            ("recovery actions", |found| {
                found.recovery.on_non_crash_failures = true
            }),
        ];
        for (setting, change) in changes {
            let mut found = settings();
            change(&mut found);
            let drift = drift(&wanted, &found);
            assert_eq!(drift.0, [setting]);
            assert_eq!(
                drift.needs_stop(),
                matches!(setting, "command line" | "account"),
                "{setting}"
            );
        }
        let mut found = settings();
        found.recovery.actions.clear();
        found.display_name.clear();
        assert_eq!(
            drift(&wanted, &found).to_string(),
            "display name, recovery actions"
        );
    }

    #[test]
    fn generation_changes_with_any_state() {
        let one = status("web", Definition::Present(Box::new(web())), None, None);
        let mut two = one.clone();
        assert_eq!(
            generation(std::slice::from_ref(&one), 5),
            generation(std::slice::from_ref(&two), 5)
        );
        assert_ne!(
            generation(std::slice::from_ref(&one), 5),
            generation(std::slice::from_ref(&one), 6)
        );
        two.state = State::Running;
        assert_ne!(generation(&[one], 5), generation(&[two], 5));
    }
}
