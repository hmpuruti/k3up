use crate::model::{Event, State, Workload};
use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use std::{
    fs::{File, OpenOptions},
    io::{ErrorKind, Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

pub const MARKER: &str = "services-mode";
/// The most of a state, definition or event file that is read.
const READ_LIMIT: u64 = 1024 * 1024;
/// How long recording an event waits for another writer before giving up.
const EVENT_WAIT: Duration = Duration::from_secs(2);
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
    /// The last event id handed out. In the administrators' folder, so no other account can
    /// hold its lock and stall the hosts.
    pub fn sequence(&self) -> PathBuf {
        self.workloads().join(".sequence")
    }
    /// Held by every command that changes services.
    pub fn changes_lock(&self) -> PathBuf {
        self.workloads().join(".lock")
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
                std::thread::sleep(Duration::from_millis(25));
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

/// Reads a whole file, refusing one larger than `READ_LIMIT`, so a huge or corrupt file
/// cannot exhaust memory.
fn read_bounded(path: &Path) -> std::io::Result<String> {
    let file = File::open(path)?;
    if file.metadata()?.len() > READ_LIMIT {
        return Err(std::io::Error::new(
            ErrorKind::InvalidData,
            "larger than 1 MB",
        ));
    }
    let mut text = String::new();
    file.take(READ_LIMIT).read_to_string(&mut text)?;
    Ok(text)
}

/// The last `READ_LIMIT` bytes of a file, from the first whole line, and whether anything
/// before them was left out.
fn read_tail(path: &Path) -> std::io::Result<(String, bool)> {
    let mut file = File::open(path)?;
    let length = file.metadata()?.len();
    let cut = length > READ_LIMIT;
    if cut {
        file.seek(SeekFrom::Start(length - READ_LIMIT))?;
    }
    let mut bytes = vec![];
    file.take(READ_LIMIT).read_to_end(&mut bytes)?;
    let text = String::from_utf8_lossy(&bytes);
    let text = match cut {
        true => text.split_once('\n').map_or("", |(_, rest)| rest),
        false => &text,
    };
    Ok((text.to_string(), cut))
}

/// `None` when there is no definition. Unreadable definitions are errors, so callers can tell
/// a missing definition from one they may not read.
pub fn read_definition(path: &Path) -> Result<Option<Workload>> {
    let text = match read_bounded(path) {
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
    serde_json::from_str(&read_bounded(path).ok()?).ok()
}

pub fn write_state(path: &Path, state: &HostState) -> Result<()> {
    write_atomic(path, &serde_json::to_vec_pretty(state)?, |_| Ok(()))
}

/// Appends an event. Ids come from one sequence shared by every workload, kept where only
/// administrators can open it, so they grow across all workloads whatever the clocks say.
/// The sequence stays locked until the event is written, so a reader that has seen an id
/// never misses a smaller one written later. Waits at most `EVENT_WAIT` for the sequence,
/// so the host's supervision never stalls on it. The file is cut back to its newest entries
/// once it grows past the limit.
pub fn append_event(layout: &Layout, name: &str, message: &str) -> Result<()> {
    let mut sequence = lock(&layout.sequence(), EVENT_WAIT)?
        .context("The event history is busy; the event was not recorded")?;
    let id = next_event_id(&mut sequence.0, &layout.events_dir())?;
    let event = Event {
        id,
        at: Utc::now(),
        name: name.into(),
        message: message.into(),
    };
    write_event(&layout.events(name), &serde_json::to_string(&event)?)
}

/// Holds an exclusive lock on a file until dropped.
pub struct Lock(File);

impl Drop for Lock {
    fn drop(&mut self) {
        let _ = FileExt::unlock(&self.0);
    }
}

/// Takes an exclusive lock on `path`, waiting up to `wait` for another holder to finish.
/// `None` when the wait runs out.
pub fn lock(path: &Path, wait: Duration) -> Result<Option<Lock>> {
    let file = open_lock_file(path)?;
    let deadline = Instant::now() + wait;
    loop {
        match file.try_lock_exclusive() {
            Ok(()) => return Ok(Some(Lock(file))),
            Err(error) if error.raw_os_error() != fs2::lock_contended_error().raw_os_error() => {
                return Err(error).with_context(|| format!("Lock {}", path.display()));
            }
            Err(_) if Instant::now() >= deadline => return Ok(None),
            Err(_) => std::thread::sleep(Duration::from_millis(50)),
        }
    }
}

/// Opens a file used only for locking, creating it owned by Administrators when missing.
fn open_lock_file(path: &Path) -> Result<File> {
    match OpenOptions::new().write(true).create_new(true).open(path) {
        Ok(_) => own(path),
        Err(error) if error.kind() == ErrorKind::AlreadyExists => {}
        Err(error) => return Err(error).with_context(|| format!("Create {}", path.display())),
    }
    refuse_redirected(path)?;
    OpenOptions::new()
        .read(true)
        .write(true)
        .open(path)
        .with_context(|| format!("Open {}", path.display()))
}

/// Takes the next id and saves it before the event is written, so a failed write skips an
/// id rather than reusing one. A missing or unreadable sequence continues after the largest
/// id already written.
fn next_event_id(sequence: &mut File, events: &Path) -> Result<i64> {
    let mut text = String::new();
    sequence.seek(SeekFrom::Start(0))?;
    sequence.take(64).read_to_string(&mut text)?;
    let last = match text.trim().parse::<i64>() {
        Ok(last) => last,
        Err(_) => largest_event_id(events)?,
    };
    let id = last + 1;
    sequence.set_len(0)?;
    sequence.seek(SeekFrom::Start(0))?;
    sequence.write_all(id.to_string().as_bytes())?;
    sequence.sync_all()?;
    Ok(id)
}

fn largest_event_id(events: &Path) -> Result<i64> {
    let mut largest = 0;
    for name in names(events, "jsonl")? {
        let Ok((text, _)) = read_tail(&events.join(format!("{name}.jsonl"))) else {
            continue;
        };
        let ids = text
            .lines()
            .filter_map(|line| serde_json::from_str::<Event>(line).ok())
            .map(|event| event.id);
        largest = ids.fold(largest, i64::max);
    }
    Ok(largest)
}

fn write_event(path: &Path, line: &str) -> Result<()> {
    let (text, cut) = match read_tail(path) {
        Ok(read) => read,
        Err(error) if error.kind() == ErrorKind::NotFound => {
            return write_atomic(path, format!("{line}\n").as_bytes(), |path| {
                own(path);
                Ok(())
            });
        }
        Err(error) => return Err(error).with_context(|| format!("Read {}", path.display())),
    };
    let lines: Vec<&str> = text.lines().collect();
    if cut || lines.len() >= EVENTS_LIMIT {
        let mut kept = lines[lines.len().saturating_sub(EVENTS_KEPT - 1)..].join("\n");
        kept.push('\n');
        kept.push_str(line);
        kept.push('\n');
        return write_atomic(path, kept.as_bytes(), |path| {
            own(path);
            Ok(())
        });
    }
    let mut file = open_append(path)?;
    file.write_all(format!("{line}\n").as_bytes())?;
    Ok(())
}

/// Opens a file to append to, creating it when missing, and refuses one a write could be
/// redirected through or that another account owns. On Windows the checks read the handle
/// that is written to.
pub fn open_append(path: &Path) -> Result<File> {
    refuse_redirected_parent(path)?;
    #[cfg(windows)]
    {
        crate::win32::open_append(path)
    }
    #[cfg(not(windows))]
    {
        refuse_redirected(path)?;
        OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .with_context(|| format!("Open {}", path.display()))
    }
}

/// Makes Administrators the owner of a shared file this process created, which the host
/// requires before it appends to the file. Best effort: if it fails, the file keeps this
/// process's account as owner, and the host refuses it unless that is SYSTEM or
/// Administrators.
fn own(path: &Path) {
    #[cfg(windows)]
    {
        let _ = crate::win32::set_owner_to_administrators(path);
    }
    let _ = path;
}

/// Newest first, at most one page, only with ids above `after`. Only the end of a large file
/// is read.
pub fn read_events(layout: &Layout, name: Option<&str>, after: Option<i64>) -> Result<Vec<Event>> {
    let files = match name {
        Some(name) => vec![name.to_string()],
        None => names(&layout.events_dir(), "jsonl")?,
    };
    let after = after.unwrap_or(0);
    let mut events = vec![];
    for file in files {
        let Ok((text, _)) = read_tail(&layout.events(&file)) else {
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

    fn layout(dir: &tempfile::TempDir) -> Layout {
        let layout = Layout::new(dir.path());
        std::fs::create_dir_all(layout.events_dir()).unwrap();
        std::fs::create_dir_all(layout.workloads()).unwrap();
        layout
    }

    #[test]
    fn events_merge_newest_first_and_stay_bounded() {
        let dir = tempfile::tempdir().unwrap();
        let layout = layout(&dir);
        append_event(&layout, "a", "first").unwrap();
        append_event(&layout, "b", "second").unwrap();
        append_event(&layout, "a", "third").unwrap();
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
            append_event(&layout, "c", &index.to_string()).unwrap();
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

    fn ids(layout: &Layout, name: &str) -> Vec<i64> {
        std::fs::read_to_string(layout.events(name))
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str::<Event>(line).unwrap().id)
            .collect()
    }

    #[test]
    fn concurrent_writers_share_one_increasing_sequence() {
        let dir = tempfile::tempdir().unwrap();
        let layout = layout(&dir);
        let writers: Vec<_> = (0..4)
            .map(|writer| {
                let layout = layout.clone();
                std::thread::spawn(move || {
                    for index in 0..50 {
                        append_event(&layout, &format!("w{writer}"), &index.to_string()).unwrap();
                    }
                })
            })
            .collect();
        for writer in writers {
            writer.join().unwrap();
        }
        let mut all = vec![];
        for writer in 0..4 {
            let ids = ids(&layout, &format!("w{writer}"));
            assert!(ids.windows(2).all(|pair| pair[0] < pair[1]), "{ids:?}");
            all.extend(ids);
        }
        all.sort();
        assert_eq!(all, (1..=200).collect::<Vec<i64>>());
    }

    #[test]
    fn a_lost_or_broken_sequence_continues_after_the_largest_id() {
        let dir = tempfile::tempdir().unwrap();
        let layout = layout(&dir);
        let old = Event {
            id: 1_700_000_000_000_000,
            at: Utc::now(),
            name: "a".into(),
            message: "Written with a clock id".into(),
        };
        let line = format!("{}\n", serde_json::to_string(&old).unwrap());
        std::fs::write(layout.events("a"), line).unwrap();
        append_event(&layout, "b", "after the upgrade").unwrap();
        assert_eq!(ids(&layout, "b"), [old.id + 1]);

        std::fs::write(layout.sequence(), b"not a number").unwrap();
        append_event(&layout, "a", "after corruption").unwrap();
        std::fs::remove_file(layout.sequence()).unwrap();
        append_event(&layout, "b", "after deletion").unwrap();
        assert_eq!(ids(&layout, "a"), [old.id, old.id + 2]);
        assert_eq!(ids(&layout, "b"), [old.id + 1, old.id + 3]);
        let merged: Vec<i64> = read_events(&layout, None, Some(old.id))
            .unwrap()
            .iter()
            .map(|event| event.id)
            .collect();
        assert_eq!(merged, [old.id + 3, old.id + 2, old.id + 1]);
    }

    #[test]
    fn a_held_sequence_makes_recording_give_up_instead_of_waiting() {
        let dir = tempfile::tempdir().unwrap();
        let layout = layout(&dir);
        let held = lock(&layout.sequence(), Duration::ZERO).unwrap().unwrap();
        let started = Instant::now();
        let refused = append_event(&layout, "a", "blocked").unwrap_err();
        let waited = started.elapsed();
        assert!(
            waited >= EVENT_WAIT && waited < EVENT_WAIT * 3,
            "{waited:?}"
        );
        assert!(refused.to_string().starts_with("The event history is busy"));
        assert!(!layout.events("a").exists());
        drop(held);
        append_event(&layout, "a", "recorded").unwrap();
    }

    #[test]
    fn a_second_holder_waits_and_then_gives_up() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(".lock");
        let held = lock(&path, Duration::ZERO).unwrap().unwrap();
        let started = Instant::now();
        assert!(lock(&path, Duration::from_millis(300)).unwrap().is_none());
        assert!(started.elapsed() >= Duration::from_millis(300));
        drop(held);
        assert!(lock(&path, Duration::ZERO).unwrap().is_some());
    }

    #[test]
    fn huge_files_are_read_only_in_part() {
        let dir = tempfile::tempdir().unwrap();
        let layout = layout(&dir);
        std::fs::create_dir_all(layout.states()).unwrap();
        let huge = vec![b'x'; READ_LIMIT as usize + 1];
        std::fs::write(layout.state("web"), &huge).unwrap();
        std::fs::write(layout.definition("web"), &huge).unwrap();
        assert_eq!(read_state(&layout.state("web")), None);
        let error = format!(
            "{:#}",
            read_definition(&layout.definition("web")).unwrap_err()
        );
        assert!(error.ends_with("larger than 1 MB"), "{error}");

        let event = Event {
            id: 7,
            at: Utc::now(),
            name: "web".into(),
            message: "last".into(),
        };
        let mut events = huge;
        events.push(b'\n');
        events.extend(serde_json::to_vec(&event).unwrap());
        events.push(b'\n');
        std::fs::write(layout.events("web"), &events).unwrap();
        let read = read_events(&layout, Some("web"), None).unwrap();
        assert_eq!(read.len(), 1);
        assert_eq!(read[0].message, "last");
        append_event(&layout, "web", "after").unwrap();
        assert!(std::fs::metadata(layout.events("web")).unwrap().len() < READ_LIMIT);
        assert_eq!(ids(&layout, "web"), [7, 8]);
    }

    #[test]
    fn a_failed_replacement_keeps_the_old_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("k3up-host.exe");
        std::fs::write(&path, b"old program").unwrap();
        let failed = write_atomic(&path, b"new program", |_| anyhow::bail!("disk full"));
        assert_eq!(failed.unwrap_err().to_string(), "disk full");
        assert_eq!(std::fs::read(&path).unwrap(), b"old program");
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);

        write_atomic(&path, b"new program", |_| Ok(())).unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"new program");
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
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
