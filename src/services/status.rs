//! Turns what the service manager, the definition and the host's state file say about a
//! workload into the `Status` every client already understands.
use super::files::HostState;
use crate::model::{Kind, State, Status, Workload};
use anyhow::{Result, bail};

const DISPLAY_PREFIX: &str = "K3 Up: ";

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
pub fn generation(statuses: &[Status]) -> u64 {
    let text = serde_json::to_string(statuses).unwrap_or_default();
    text.bytes().fold(0xcbf29ce484222325u64, |hash, byte| {
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
        }
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
    fn generation_changes_with_any_state() {
        let one = status("web", Definition::Present(Box::new(web())), None, None);
        let mut two = one.clone();
        assert_eq!(
            generation(std::slice::from_ref(&one)),
            generation(std::slice::from_ref(&two))
        );
        two.state = State::Running;
        assert_ne!(generation(&[one]), generation(&[two]));
    }
}
