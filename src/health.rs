//! Machine and workload health verdicts, shared by the command line and the desktop app.
use crate::{
    metrics::{Machine, Metrics, Usage},
    model::{State, Status},
};
use chrono::TimeDelta;
use serde::{Deserialize, Serialize};

pub const MEMORY_WARNING: f32 = 0.85;
pub const SWAP_WARNING: f32 = 0.5;
pub const DISK_FREE_WARNING: f32 = 0.10;
pub const CPU_WARNING: f32 = 90.0;
pub const CPU_WINDOW: TimeDelta = TimeDelta::seconds(60);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Verdict {
    Ok,
    Warning,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WarningKind {
    Cpu,
    Memory,
    Swap,
    Disk,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Warning {
    pub kind: WarningKind,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Attention {
    pub name: String,
    pub state: State,
    pub reason: String,
    pub last_exit: Option<i32>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Footprint {
    pub agent: Usage,
    pub workloads: Usage,
    pub data_bytes: u64,
    pub workload_count: usize,
    pub running: usize,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Report {
    pub status: Verdict,
    pub warnings: Vec<Warning>,
    pub attention: Vec<Attention>,
    pub machine: Machine,
    pub k3up: Footprint,
}

pub fn needs_attention(state: State) -> bool {
    matches!(state, State::Failed | State::Backoff | State::Blocked)
}

pub fn ratio(used: u64, total: u64) -> f32 {
    if total == 0 {
        0.0
    } else {
        (used as f32 / total as f32).clamp(0.0, 1.0)
    }
}

/// High CPU only counts once the samples actually span most of the window; a fresh agent's
/// two samples a few seconds apart are not "sustained".
pub fn cpu_sustained(metrics: &Metrics) -> bool {
    let recent: Vec<_> = metrics
        .history
        .iter()
        .filter(|point| metrics.at - point.at <= CPU_WINDOW)
        .collect();
    let spanned = recent
        .first()
        .is_some_and(|first| metrics.at - first.at >= CPU_WINDOW * 3 / 4);
    spanned && recent.iter().all(|point| point.cpu >= CPU_WARNING)
}

pub fn warnings(metrics: &Metrics) -> Vec<Warning> {
    let machine = &metrics.machine;
    let memory = ratio(machine.memory_used, machine.memory_total);
    let swap = ratio(machine.swap_used, machine.swap_total);
    let disk_free = ratio(machine.disk_available, machine.disk_total);
    let mut found = vec![];
    if cpu_sustained(metrics) {
        found.push(Warning {
            kind: WarningKind::Cpu,
            message: format!(
                "CPU at {:.0}% or more for the last {} seconds",
                CPU_WARNING,
                CPU_WINDOW.num_seconds()
            ),
        });
    }
    if memory > MEMORY_WARNING {
        found.push(Warning {
            kind: WarningKind::Memory,
            message: format!(
                "Memory {:.0}% used, above {:.0}%",
                memory * 100.0,
                MEMORY_WARNING * 100.0
            ),
        });
    }
    if machine.swap_total > 0 && swap > SWAP_WARNING {
        found.push(Warning {
            kind: WarningKind::Swap,
            message: format!(
                "Swap {:.0}% used, above {:.0}%",
                swap * 100.0,
                SWAP_WARNING * 100.0
            ),
        });
    }
    if machine.disk_total > 0 && disk_free < DISK_FREE_WARNING {
        found.push(Warning {
            kind: WarningKind::Disk,
            message: format!(
                "Disk {:.1}% free on the data volume, below {:.0}%",
                disk_free * 100.0,
                DISK_FREE_WARNING * 100.0
            ),
        });
    }
    found
}

pub fn attention(statuses: &[Status]) -> Vec<Attention> {
    statuses
        .iter()
        .filter(|status| needs_attention(status.state))
        .map(|status| Attention {
            name: status.workload.name.clone(),
            state: status.state,
            reason: status.reason.clone(),
            last_exit: status.last_exit,
        })
        .collect()
}

pub fn assess(metrics: &Metrics, statuses: &[Status]) -> Report {
    let warnings = warnings(metrics);
    let attention = attention(statuses);
    let status = if warnings.is_empty() && attention.is_empty() {
        Verdict::Ok
    } else {
        Verdict::Warning
    };
    Report {
        status,
        warnings,
        attention,
        machine: metrics.machine.clone(),
        k3up: Footprint {
            agent: metrics.agent,
            workloads: metrics.managed,
            data_bytes: metrics.data_bytes,
            workload_count: statuses.len(),
            running: statuses
                .iter()
                .filter(|status| status.state == State::Running)
                .count(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{metrics::Point, model::Workload};
    use chrono::Utc;

    fn metrics() -> Metrics {
        Metrics {
            at: Utc::now(),
            machine: Machine {
                cores: 4,
                cpu: 10.0,
                memory_total: 1000,
                memory_used: 500,
                swap_total: 1000,
                swap_used: 100,
                disk_total: 1000,
                disk_available: 500,
                ..Default::default()
            },
            agent: Usage::default(),
            managed: Usage::default(),
            workloads: vec![],
            data_bytes: 0,
            history: vec![],
        }
    }

    fn status(name: &str, state: State) -> Status {
        Status {
            workload: Workload {
                name: name.into(),
                ..Default::default()
            },
            state,
            desired_running: false,
            pid: None,
            restart_count: 0,
            started_at: None,
            next_run: None,
            last_exit: Some(7),
            reason: "Process exited with code 7".into(),
        }
    }

    fn kinds(metrics: &Metrics) -> Vec<WarningKind> {
        warnings(metrics).into_iter().map(|w| w.kind).collect()
    }

    #[test]
    fn healthy_machine_has_no_warnings() {
        assert!(warnings(&metrics()).is_empty());
        assert_eq!(assess(&metrics(), &[]).status, Verdict::Ok);
    }

    #[test]
    fn memory_swap_and_disk_thresholds() {
        let mut high = metrics();
        high.machine.memory_used = 860;
        assert_eq!(kinds(&high), vec![WarningKind::Memory]);
        let mut swapping = metrics();
        swapping.machine.swap_used = 501;
        assert_eq!(kinds(&swapping), vec![WarningKind::Swap]);
        let mut no_swap = metrics();
        no_swap.machine.swap_total = 0;
        no_swap.machine.swap_used = 0;
        assert!(kinds(&no_swap).is_empty());
        let mut full = metrics();
        full.machine.disk_available = 99;
        assert_eq!(kinds(&full), vec![WarningKind::Disk]);
    }

    #[test]
    fn cpu_warning_needs_a_sustained_window() {
        let mut brief = metrics();
        brief.history = vec![Point {
            at: brief.at - TimeDelta::seconds(5),
            cpu: 100.0,
            memory_used: 0,
            managed_cpu: 0.0,
            managed_memory: 0,
        }];
        assert!(!cpu_sustained(&brief));
        let mut sustained = metrics();
        sustained.history = [60, 50, 40, 30, 20, 10, 0]
            .into_iter()
            .map(|ago: i64| Point {
                at: sustained.at - TimeDelta::seconds(ago),
                cpu: 95.0,
                memory_used: 0,
                managed_cpu: 0.0,
                managed_memory: 0,
            })
            .collect();
        assert!(cpu_sustained(&sustained));
        assert_eq!(kinds(&sustained), vec![WarningKind::Cpu]);
        sustained.history[3].cpu = 50.0;
        assert!(!cpu_sustained(&sustained));
    }

    #[test]
    fn failed_backoff_and_blocked_workloads_need_attention() {
        let statuses = [
            status("ok", State::Running),
            status("crashed", State::Failed),
            status("retrying", State::Backoff),
            status("waiting", State::Blocked),
            status("done", State::Completed),
        ];
        let report = assess(&metrics(), &statuses);
        assert_eq!(report.status, Verdict::Warning);
        let names: Vec<_> = report.attention.iter().map(|a| a.name.as_str()).collect();
        assert_eq!(names, ["crashed", "retrying", "waiting"]);
        assert_eq!(report.attention[0].last_exit, Some(7));
        assert_eq!(report.k3up.workload_count, 5);
        assert_eq!(report.k3up.running, 1);
        let json = serde_json::to_value(&report).unwrap();
        assert_eq!(json["status"], "warning");
        assert_eq!(json["attention"][0]["state"], "failed");
    }
}
