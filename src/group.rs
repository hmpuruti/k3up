//! Folder paths that organise workloads, such as `watchtower/entra`. Folders are compared
//! without regard to case and never affect dependencies or start order.
use crate::{
    health::needs_attention,
    model::{State, Status},
};
use anyhow::{Result, bail};
use std::{borrow::Borrow, collections::BTreeMap};

pub const MAX_DEPTH: usize = 5;
pub const MAX_SEGMENT: usize = 64;
pub const MAX_LENGTH: usize = 128;

/// An empty path means no folder and is always valid.
pub fn validate(path: &str) -> Result<()> {
    if path.is_empty() {
        return Ok(());
    }
    if path.chars().count() > MAX_LENGTH {
        bail!("Group must be at most {MAX_LENGTH} characters");
    }
    let segments: Vec<&str> = path.split('/').collect();
    if segments.len() > MAX_DEPTH {
        bail!("Group must have at most {MAX_DEPTH} levels, such as watchtower/entra");
    }
    for segment in segments {
        if segment.is_empty() {
            bail!("Group must not have empty segments; write it like watchtower/entra");
        }
        if segment.starts_with(' ') || segment.ends_with(' ') {
            bail!("Group segments must not start or end with a space");
        }
        if segment.chars().count() > MAX_SEGMENT {
            bail!("Each group segment must be at most {MAX_SEGMENT} characters");
        }
        if !segment
            .chars()
            .all(|c| c.is_alphanumeric() || matches!(c, ' ' | '-' | '_' | '.'))
        {
            bail!(
                "Group segments may contain letters, digits, spaces, hyphens, underscores and dots, separated by /"
            );
        }
    }
    Ok(())
}

pub fn segments(path: &str) -> impl DoubleEndedIterator<Item = &str> {
    path.split('/').filter(|segment| !segment.is_empty())
}

/// The form used for comparison and as a map key.
pub fn key(path: &str) -> String {
    path.to_lowercase()
}

fn keys(path: &str) -> Vec<String> {
    segments(path).map(str::to_lowercase).collect()
}

pub fn same(a: &str, b: &str) -> bool {
    keys(a) == keys(b)
}

/// A folder contains itself and every folder below it. `watchtower/en` does not contain
/// `watchtower/entra`; only whole segments match.
pub fn contains(folder: &str, path: &str) -> bool {
    let folder = keys(folder);
    let path = keys(path);
    path.len() >= folder.len() && path[..folder.len()] == folder[..]
}

pub fn name(path: &str) -> &str {
    segments(path).next_back().unwrap_or(path)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Folder {
    /// Spelled as the first workload inside it, in sorted order, spells it.
    pub path: String,
    pub depth: usize,
    /// Positions in the input of the workloads directly inside this folder, sorted by name.
    pub members: Vec<usize>,
    /// Every workload inside, including subfolders.
    pub counts: BTreeMap<State, usize>,
}

impl Folder {
    pub fn name(&self) -> &str {
        name(&self.path)
    }

    pub fn workloads(&self) -> usize {
        self.counts.values().sum()
    }

    pub fn running(&self) -> usize {
        self.counts.get(&State::Running).copied().unwrap_or(0)
    }

    pub fn attention(&self) -> usize {
        self.counts
            .iter()
            .filter(|(state, _)| needs_attention(**state))
            .map(|(_, count)| count)
            .sum()
    }

    /// Non-zero states, such as `12 running, 1 failed`.
    pub fn summary(&self) -> String {
        const ORDER: [State; 8] = [
            State::Running,
            State::Failed,
            State::Backoff,
            State::Blocked,
            State::Starting,
            State::Pending,
            State::Completed,
            State::Stopped,
        ];
        ORDER
            .iter()
            .filter_map(|state| {
                self.counts
                    .get(state)
                    .map(|count| format!("{count} {state}"))
            })
            .collect::<Vec<_>>()
            .join(", ")
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Tree {
    /// Depth first, siblings sorted without regard to case.
    pub folders: Vec<Folder>,
    /// Positions of the workloads without a folder, sorted by name.
    pub ungrouped: Vec<usize>,
}

pub fn tree<S: Borrow<Status>>(statuses: &[S]) -> Tree {
    let status = |index: usize| statuses[index].borrow();
    let mut order: Vec<usize> = (0..statuses.len()).collect();
    order.sort_by_cached_key(|&index| {
        let workload = &status(index).workload;
        (keys(&workload.group), workload.name.clone())
    });
    let mut tree = Tree::default();
    let mut by_key: BTreeMap<Vec<String>, usize> = BTreeMap::new();
    for index in order {
        let status = status(index);
        let segments: Vec<&str> = segments(&status.workload.group).collect();
        if segments.is_empty() {
            tree.ungrouped.push(index);
            continue;
        }
        let mut parent: Option<usize> = None;
        for depth in 0..segments.len() {
            let key = segments[..=depth]
                .iter()
                .map(|segment| segment.to_lowercase())
                .collect();
            let at = *by_key.entry(key).or_insert_with(|| {
                let path = match parent {
                    Some(parent) => format!("{}/{}", tree.folders[parent].path, segments[depth]),
                    None => segments[depth].to_string(),
                };
                tree.folders.push(Folder {
                    path,
                    depth,
                    members: vec![],
                    counts: BTreeMap::new(),
                });
                tree.folders.len() - 1
            });
            *tree.folders[at].counts.entry(status.state).or_default() += 1;
            if depth == segments.len() - 1 {
                tree.folders[at].members.push(index);
            }
            parent = Some(at);
        }
    }
    tree
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Workload;

    fn status(name: &str, group: &str, state: State) -> Status {
        Status {
            workload: Workload {
                name: name.into(),
                group: group.into(),
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

    #[test]
    fn accepts_folder_paths_within_the_limits() {
        for path in [
            "",
            "watchtower",
            "watchtower/entra",
            "Watchtower/Intune Compliance",
            "a/b/c/d/e",
            "ops-1/team_2/v1.2",
            "Überwachung/entra",
            &"x".repeat(64),
        ] {
            validate(path).unwrap_or_else(|error| panic!("{path}: {error}"));
        }
    }

    #[test]
    fn rejects_bad_segments_depth_and_length() {
        let error = |path: &str| validate(path).unwrap_err().to_string();
        assert!(error("a//b").contains("empty segments"));
        assert!(error("/a").contains("empty segments"));
        assert!(error("a/").contains("empty segments"));
        assert!(error(" a").contains("space"));
        assert!(error("a /b").contains("space"));
        assert!(error("a/b:c").contains("letters, digits"));
        assert!(error("a\\b").contains("letters, digits"));
        assert!(error("a/b/c/d/e/f").contains("5 levels"));
        assert!(error(&"x".repeat(65)).contains("64 characters"));
        assert!(error(&format!("{}/{}", "x".repeat(64), "y".repeat(64))).contains("128"));
    }

    #[test]
    fn folders_match_whole_segments_without_regard_to_case() {
        assert!(same("Watchtower/Entra", "watchtower/entra"));
        assert!(!same("watchtower", "watchtower/entra"));
        assert!(contains("watchtower", "watchtower"));
        assert!(contains("watchtower", "Watchtower/Entra"));
        assert!(contains("WATCHTOWER/entra", "watchtower/entra/users"));
        assert!(!contains("watchtower/en", "watchtower/entra"));
        assert!(!contains("watchtower/entra", "watchtower"));
        assert!(!contains("entra", "watchtower/entra"));
        assert!(contains("", "anything/at/all"));
        assert_eq!(name("watchtower/entra"), "entra");
        assert_eq!(name("solo"), "solo");
        assert_eq!(key("Watchtower/Entra"), "watchtower/entra");
    }

    #[test]
    fn tree_nests_folders_and_counts_subfolders() {
        let statuses = [
            status("solo", "", State::Stopped),
            status("users", "watchtower/entra", State::Running),
            status("policies", "Watchtower/Intune", State::Failed),
            status("violations", "watchtower/imperva", State::Running),
            status("devices", "watchtower/entra", State::Backoff),
            status("archive", "Archive", State::Completed),
            status("deep", "archive/2024/q1", State::Stopped),
        ];
        let tree = tree(&statuses);
        let paths: Vec<_> = tree
            .folders
            .iter()
            .map(|folder| (folder.path.as_str(), folder.depth))
            .collect();
        assert_eq!(
            paths,
            [
                ("Archive", 0),
                ("Archive/2024", 1),
                ("Archive/2024/q1", 2),
                ("watchtower", 0),
                ("watchtower/entra", 1),
                ("watchtower/imperva", 1),
                ("watchtower/Intune", 1),
            ]
        );
        let watchtower = &tree.folders[3];
        assert_eq!(watchtower.workloads(), 4);
        assert_eq!(watchtower.running(), 2);
        assert_eq!(watchtower.attention(), 2);
        assert_eq!(watchtower.summary(), "2 running, 1 failed, 1 backoff");
        assert!(watchtower.members.is_empty());
        let entra = &tree.folders[4];
        assert_eq!(entra.name(), "entra");
        assert_eq!(entra.members, [4, 1]);
        assert_eq!(entra.summary(), "1 running, 1 backoff");
        assert_eq!(tree.folders[0].members, [5]);
        assert_eq!(tree.folders[0].workloads(), 2);
        assert_eq!(tree.folders[2].members, [6]);
        assert_eq!(tree.ungrouped, [0]);
    }

    #[test]
    fn tree_spells_a_folder_as_its_first_sorted_workload_does() {
        let statuses = [
            status("b", "watchtower/Entra", State::Running),
            status("a", "Watchtower/entra", State::Running),
            status("c", "watchtower", State::Stopped),
        ];
        let tree = tree(&statuses);
        assert_eq!(tree.folders[0].path, "watchtower");
        assert_eq!(tree.folders[1].path, "watchtower/entra");
        assert_eq!(tree.folders[1].members, [1, 0]);
        assert_eq!(tree.folders.len(), 2);
    }

    #[test]
    fn empty_input_gives_an_empty_tree() {
        let statuses: [Status; 0] = [];
        assert_eq!(tree(&statuses), Tree::default());
        let borrowed: Vec<&Status> = vec![];
        assert_eq!(tree(&borrowed), Tree::default());
    }
}
