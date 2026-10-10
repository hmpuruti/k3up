use crate::model::{Event, State, Workload};
use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::{
    io::{ErrorKind, Write},
    path::{Path, PathBuf},
};

pub const MARKER: &str = "services-mode";
const EVENTS_LIMIT: usize = 2000;
const EVENTS_KEPT: usize = 1000;
const EVENTS_PAGE: usize = 200;

/// Where services mode keeps each kind of file inside a data directory.
#[derive(Debug, Clone)]
pub struct Layout {
    root: PathBuf,
}

impl Layout {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }
    pub fn root(&self) -> &Path {
        &self.root
    }
    pub fn marker(&self) -> PathBuf {
        self.root.join(MARKER)
    }
    pub fn workloads(&self) -> PathBuf {
        self.root.join("workloads")
    }
    pub fn logs(&self) -> PathBuf {
        self.root.join("logs")
    }
    pub fn states(&self) -> PathBuf {
        self.root.join("state")
    }
    pub fn events_dir(&self) -> PathBuf {
        self.root.join("events")
    }
    pub fn definition(&self, name: &str) -> PathBuf {
        self.workloads().join(format!("{name}.toml"))
    }
    pub fn log(&self, name: &str) -> PathBuf {
        self.logs().join(format!("{name}.log"))
    }
    pub fn state(&self, name: &str) -> PathBuf {
        self.states().join(format!("{name}.json"))
    }
    pub fn events(&self, name: &str) -> PathBuf {
        self.events_dir().join(format!("{name}.jsonl"))
    }
}

/// What a workload's host last reported. `host_pid` tells a report from the running host apart
/// from one left by an earlier run.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct HostState {
    pub state: State,
    pub pid: Option<u32>,
    pub host_pid: u32,
    pub restart_count: u32,
    pub started_at: Option<DateTime<Utc>>,
    pub last_exit: Option<i32>,
    pub reason: String,
    pub updated_at: DateTime<Utc>,
}

/// Replaces `path` in one step, so readers never see a partly written file. `prepare` runs on
/// the finished temporary file before it takes the final name.
pub fn write_atomic(
    path: &Path,
    bytes: &[u8],
    prepare: impl FnOnce(&Path) -> Result<()>,
) -> Result<()> {
    refuse_redirected_parent(path)?;
    let temporary = path.with_extension(format!("{}.tmp", std::process::id()));
    // A fresh file, so a link planted under the temporary name cannot redirect the write.
    let _ = std::fs::remove_file(&temporary);
    let written = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temporary)
        .and_then(|mut file| {
            file.write_all(bytes)?;
            file.sync_all()
        })
        .with_context(|| format!("Write {}", temporary.display()))
        .and_then(|()| prepare(&temporary))
        .and_then(|()| rename(&temporary, path));
    if written.is_err() {
        let _ = std::fs::remove_file(&temporary);
    }
    written
}

/// Refuses a file that a write could be redirected through: a link, or on Windows a file with
/// another hard link. Run before appending to a file as an administrator or SYSTEM.
pub fn refuse_redirected(path: &Path) -> Result<()> {
    refuse_redirected_parent(path)?;
    #[cfg(windows)]
    {
        crate::win32::refuse_redirected_file(path)
    }
    #[cfg(not(windows))]
    {
        match std::fs::symlink_metadata(path) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                anyhow::bail!("{} is a link to another location", path.display())
            }
            _ => Ok(()),
        }
    }
}

fn refuse_redirected_parent(path: &Path) -> Result<()> {
    #[cfg(windows)]
    if let Some(parent) = path.parent() {
        crate::win32::refuse_reparse_point(parent)?;
    }
    let _ = path;
    Ok(())
}

/// A reader or a virus scanner can hold the target open for a moment on Windows.
fn rename(from: &Path, to: &Path) -> Result<()> {
    let mut attempts = 0;
    loop {
        match std::fs::rename(from, to) {
            Ok(()) => return Ok(()),
            Err(error) if attempts < 20 && error.kind() == ErrorKind::PermissionDenied => {
                attempts += 1;
                std::thread::sleep(std::time::Duration::from_millis(25));
            }
            Err(error) => {
                return Err(error).with_context(|| format!("Replace {}", to.display()));
            }
        }
    }
}

pub fn definition_text(workload: &Workload) -> Result<String> {
    Ok(toml::to_string_pretty(workload)?)
}

/// `None` when there is no definition. Unreadable definitions are errors, so callers can tell
/// a missing definition from one they may not read.
pub fn read_definition(path: &Path) -> Result<Option<Workload>> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error).with_context(|| format!("Read {}", path.display())),
    };
    let workload: Workload =
        toml::from_str(&text).with_context(|| format!("Invalid definition {}", path.display()))?;
    Ok(Some(workload))
}

/// The names of the files in `dir` with the given extension, sorted. A missing folder is empty.
pub fn names(dir: &Path, extension: &str) -> Result<Vec<String>> {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(vec![]),
        Err(error) => return Err(error).with_context(|| format!("Read {}", dir.display())),
    };
    let mut names: Vec<String> = entries
        .flatten()
        .filter_map(|entry| {
            let path = entry.path();
            (path.extension()? == extension)
                .then(|| path.file_stem()?.to_str().map(str::to_string))
                .flatten()
        })
        .collect();
    names.sort();
    Ok(names)
}

pub fn read_state(path: &Path) -> Option<HostState> {
    let text = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&text).ok()
}

pub fn write_state(path: &Path, state: &HostState) -> Result<()> {
    write_atomic(path, &serde_json::to_vec_pretty(state)?, |_| Ok(()))
}

/// Appends an event. Ids are microsecond timestamps, so events from separate files sort into
/// one history. The file is cut back to its newest entries once it grows past the limit.
pub fn append_event(path: &Path, name: &str, message: &str) -> Result<()> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) if error.kind() == ErrorKind::NotFound => String::new(),
        Err(error) => return Err(error).with_context(|| format!("Read {}", path.display())),
    };
    let last = text
        .lines()
        .next_back()
        .and_then(|line| serde_json::from_str::<Event>(line).ok())
        .map_or(0, |event| event.id);
    let at = Utc::now();
    let event = Event {
        id: at.timestamp_micros().max(last + 1),
        at,
        name: name.into(),
        message: message.into(),
    };
    let line = serde_json::to_string(&event)?;
    let lines: Vec<&str> = text.lines().collect();
    if lines.len() >= EVENTS_LIMIT {
        let mut kept = lines[lines.len() + 1 - EVENTS_KEPT..].join("\n");
        kept.push('\n');
        kept.push_str(&line);
        kept.push('\n');
        return write_atomic(path, kept.as_bytes(), |_| Ok(()));
    }
    refuse_redirected(path)?;
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .with_context(|| format!("Open {}", path.display()))?;
    file.write_all(format!("{line}\n").as_bytes())?;
    Ok(())
}

/// Newest first, at most one page, only with ids above `after`.
pub fn read_events(layout: &Layout, name: Option<&str>, after: Option<i64>) -> Result<Vec<Event>> {
    let files = match name {
        Some(name) => vec![name.to_string()],
        None => names(&layout.events_dir(), "jsonl")?,
    };
    let after = after.unwrap_or(0);
    let mut events = vec![];
    for file in files {
        let Ok(text) = std::fs::read_to_string(layout.events(&file)) else {
            continue;
        };
        events.extend(
            text.lines()
                .filter_map(|line| serde_json::from_str::<Event>(line).ok())
                .filter(|event| event.id > after),
        );
    }
    events.sort_by_key(|event| std::cmp::Reverse(event.id));
    events.truncate(EVENTS_PAGE);
    Ok(events)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn events_merge_newest_first_and_stay_bounded() {
        let dir = tempfile::tempdir().unwrap();
        let layout = Layout::new(dir.path());
        std::fs::create_dir_all(layout.events_dir()).unwrap();
        append_event(&layout.events("a"), "a", "first").unwrap();
        append_event(&layout.events("b"), "b", "second").unwrap();
        append_event(&layout.events("a"), "a", "third").unwrap();
        let all = read_events(&layout, None, None).unwrap();
        let messages: Vec<_> = all.iter().map(|event| event.message.as_str()).collect();
        assert_eq!(messages, ["third", "second", "first"]);
        let newer = read_events(&layout, None, Some(all[1].id)).unwrap();
        assert_eq!(newer.len(), 1);
        assert_eq!(read_events(&layout, Some("b"), None).unwrap().len(), 1);
        assert!(
            read_events(&layout, Some("missing"), None)
                .unwrap()
                .is_empty()
        );

        for index in 0..EVENTS_LIMIT + 5 {
            append_event(&layout.events("c"), "c", &index.to_string()).unwrap();
        }
        let text = std::fs::read_to_string(layout.events("c")).unwrap();
        let count = text.lines().count();
        assert!((EVENTS_KEPT..=EVENTS_LIMIT).contains(&count), "{count}");
        let ids: Vec<i64> = text
            .lines()
            .map(|line| serde_json::from_str::<Event>(line).unwrap().id)
            .collect();
        assert!(ids.windows(2).all(|pair| pair[0] < pair[1]));
        assert!(text.ends_with(&format!("\"{}\"}}\n", EVENTS_LIMIT + 4)));
    }

    #[test]
    fn definitions_and_states_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let layout = Layout::new(dir.path());
        std::fs::create_dir_all(layout.workloads()).unwrap();
        std::fs::create_dir_all(layout.states()).unwrap();
        let workload = Workload {
            name: "web".into(),
            group: "a/b".into(),
            executable: "/bin/sleep".into(),
            working_directory: "/tmp".into(),
            args: vec!["5".into()],
            ..Default::default()
        };
        assert_eq!(read_definition(&layout.definition("web")).unwrap(), None);
        let text = definition_text(&workload).unwrap();
        write_atomic(&layout.definition("web"), text.as_bytes(), |_| Ok(())).unwrap();
        assert_eq!(
            read_definition(&layout.definition("web")).unwrap(),
            Some(workload)
        );
        assert_eq!(names(&layout.workloads(), "toml").unwrap(), ["web"]);
        assert!(names(&layout.logs(), "log").unwrap().is_empty());

        assert_eq!(read_state(&layout.state("web")), None);
        let state = HostState {
            state: State::Running,
            pid: Some(7),
            host_pid: 6,
            reason: "Process started".into(),
            ..Default::default()
        };
        write_state(&layout.state("web"), &state).unwrap();
        write_state(&layout.state("web"), &state).unwrap();
        assert_eq!(read_state(&layout.state("web")), Some(state));
        assert_eq!(names(&layout.states(), "json").unwrap(), ["web"]);
    }
}
