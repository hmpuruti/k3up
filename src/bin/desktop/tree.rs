use crate::theme::Tone;
use k3up::{
    group::{self, Folder, Tree},
    model::{Manifest, State, Status},
};
use std::collections::BTreeSet;

pub const INDENT: f32 = 14.0;

pub enum Row<'a> {
    Folder { folder: &'a Folder, collapsed: bool },
    Workload { index: usize, depth: usize },
}

/// The list in display order, leaving out everything under a collapsed folder.
pub fn rows<'a>(tree: &'a Tree, collapsed: &BTreeSet<String>) -> Vec<Row<'a>> {
    let mut rows = vec![];
    let mut hidden_below = None;
    for folder in &tree.folders {
        if let Some(depth) = hidden_below {
            if folder.depth > depth {
                continue;
            }
            hidden_below = None;
        }
        let closed = collapsed.contains(&group::key(&folder.path));
        rows.push(Row::Folder {
            folder,
            collapsed: closed,
        });
        if closed {
            hidden_below = Some(folder.depth);
            continue;
        }
        for &index in &folder.members {
            rows.push(Row::Workload {
                index,
                depth: folder.depth + 1,
            });
        }
    }
    for &index in &tree.ungrouped {
        rows.push(Row::Workload { index, depth: 0 });
    }
    rows
}

/// Red when anything inside needs attention, green when everything is running or
/// completed, grey otherwise.
pub fn tone(folder: &Folder) -> Tone {
    if folder.attention() > 0 {
        return Tone::Danger;
    }
    let settled = folder.running() + folder.counts.get(&State::Completed).copied().unwrap_or(0);
    if settled > 0 && settled == folder.workloads() {
        Tone::Success
    } else {
        Tone::Neutral
    }
}

/// Names inside a folder in startup order, reversed for a stop.
pub fn members(statuses: &[Status], folder: &str, stop: bool) -> Vec<String> {
    let order = Manifest {
        version: 1,
        workloads: statuses
            .iter()
            .map(|status| status.workload.clone())
            .collect(),
    }
    .order()
    .unwrap_or_else(|_| {
        statuses
            .iter()
            .map(|status| status.workload.name.clone())
            .collect()
    });
    let mut names: Vec<String> = order
        .into_iter()
        .filter(|name| {
            statuses.iter().any(|status| {
                status.workload.name == *name && group::contains(folder, &status.workload.group)
            })
        })
        .collect();
    if stop {
        names.reverse();
    }
    names
}

#[cfg(test)]
mod tests {
    use super::*;
    use k3up::model::Workload;

    fn status(name: &str, group: &str, state: State, depends_on: &[&str]) -> Status {
        Status {
            workload: Workload {
                name: name.into(),
                group: group.into(),
                depends_on: depends_on.iter().map(|s| s.to_string()).collect(),
                ..Default::default()
            },
            state,
            desired_running: false,
            pid: None,
            restart_count: 0,
            started_at: None,
            next_run: None,
            last_exit: None,
            reason: String::new(),
        }
    }

    fn fleet() -> Vec<Status> {
        vec![
            status("solo", "", State::Stopped, &[]),
            status("users", "watchtower/entra", State::Running, &[]),
            status("policies", "watchtower/intune", State::Failed, &[]),
            status("devices", "watchtower/entra", State::Running, &["users"]),
            status("done", "batch", State::Completed, &[]),
        ]
    }

    fn describe(rows: &[Row<'_>], statuses: &[Status]) -> Vec<String> {
        rows.iter()
            .map(|row| match row {
                Row::Folder { folder, collapsed } => {
                    format!("{}{}", folder.path, if *collapsed { " +" } else { "" })
                }
                Row::Workload { index, depth } => {
                    format!("{}{}", "  ".repeat(*depth), statuses[*index].workload.name)
                }
            })
            .collect()
    }

    #[test]
    fn rows_nest_workloads_under_folders_and_ungrouped_last() {
        let statuses = fleet();
        let tree = group::tree(&statuses);
        let rows = rows(&tree, &BTreeSet::new());
        assert_eq!(
            describe(&rows, &statuses),
            [
                "batch",
                "  done",
                "watchtower",
                "watchtower/entra",
                "    devices",
                "    users",
                "watchtower/intune",
                "    policies",
                "solo",
            ]
        );
    }

    #[test]
    fn collapsing_hides_everything_below_a_folder() {
        let statuses = fleet();
        let tree = group::tree(&statuses);
        let collapsed = BTreeSet::from(["watchtower".to_string()]);
        let shown = rows(&tree, &collapsed);
        assert_eq!(
            describe(&shown, &statuses),
            ["batch", "  done", "watchtower +", "solo"]
        );
        let collapsed = BTreeSet::from(["watchtower/entra".to_string()]);
        let shown = rows(&tree, &collapsed);
        assert_eq!(
            describe(&shown, &statuses),
            [
                "batch",
                "  done",
                "watchtower",
                "watchtower/entra +",
                "watchtower/intune",
                "    policies",
                "solo",
            ]
        );
    }

    #[test]
    fn folder_tone_follows_the_worst_state_inside() {
        let statuses = fleet();
        let tree = group::tree(&statuses);
        let by_path = |path: &str| {
            tree.folders
                .iter()
                .find(|folder| folder.path == path)
                .unwrap()
        };
        assert_eq!(tone(by_path("batch")), Tone::Success);
        assert_eq!(tone(by_path("watchtower")), Tone::Danger);
        assert_eq!(tone(by_path("watchtower/entra")), Tone::Success);
        assert_eq!(tone(by_path("watchtower/intune")), Tone::Danger);
        assert_eq!(by_path("watchtower").workloads(), 3);
        let mixed = [
            status("a", "mixed", State::Running, &[]),
            status("b", "mixed", State::Stopped, &[]),
        ];
        assert_eq!(tone(&group::tree(&mixed).folders[0]), Tone::Neutral);
    }

    #[test]
    fn folder_members_follow_dependency_order() {
        let statuses = fleet();
        assert_eq!(
            members(&statuses, "Watchtower", false),
            ["users", "devices", "policies"]
        );
        assert_eq!(
            members(&statuses, "watchtower", true),
            ["policies", "devices", "users"]
        );
        assert_eq!(
            members(&statuses, "watchtower/en", false),
            Vec::<String>::new()
        );
    }
}
