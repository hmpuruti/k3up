use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::{
    collections::{HashMap, VecDeque},
    path::{Path, PathBuf},
    sync::{Arc, Mutex, mpsc},
    thread::JoinHandle,
    time::{Duration, Instant},
};
use sysinfo::{Disks, Pid, ProcessRefreshKind, ProcessesToUpdate, System};

const IDLE_INTERVAL: Duration = Duration::from_secs(10);
const WATCHED_INTERVAL: Duration = Duration::from_secs(2);
/// Someone asked for metrics this recently, so sample at the faster rate.
const WATCH_WINDOW: Duration = Duration::from_secs(30);
const HISTORY: chrono::TimeDelta = chrono::TimeDelta::hours(1);
const SPARK_POINTS: usize = 60;

/// Resource use of one process tree. CPU is a share of the whole machine: 100 means every
/// core is busy, so values add up across workloads.
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
pub struct Usage {
    pub cpu: f32,
    pub memory: u64,
    pub read_rate: u64,
    pub write_rate: u64,
    pub processes: u32,
}

impl Usage {
    fn add(&mut self, other: &Usage) {
        self.cpu += other.cpu;
        self.memory += other.memory;
        self.read_rate += other.read_rate;
        self.write_rate += other.write_rate;
        self.processes += other.processes;
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Machine {
    pub host: String,
    pub os: String,
    pub cores: usize,
    pub cpu: f32,
    pub memory_total: u64,
    pub memory_used: u64,
    pub swap_total: u64,
    pub swap_used: u64,
    /// The volume that holds the agent's data directory.
    pub disk_total: u64,
    pub disk_available: u64,
    /// One, five and fifteen minute load averages; absent on Windows.
    pub load: Option<[f64; 3]>,
    pub uptime_secs: u64,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct WorkloadUsage {
    pub name: String,
    /// The workload's own process first, then its descendants.
    pub pids: Vec<u32>,
    pub usage: Usage,
    pub log_bytes: u64,
    pub cpu_history: Vec<f32>,
    pub memory_history: Vec<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Point {
    pub at: DateTime<Utc>,
    pub cpu: f32,
    pub memory_used: u64,
    pub managed_cpu: f32,
    pub managed_memory: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Metrics {
    pub at: DateTime<Utc>,
    pub machine: Machine,
    /// The agent process alone, without its workloads.
    pub agent: Usage,
    /// Every running workload combined.
    pub managed: Usage,
    pub workloads: Vec<WorkloadUsage>,
    /// Database, logs and other files in the data directory.
    pub data_bytes: u64,
    pub history: Vec<Point>,
}

struct Shared {
    roster: Mutex<Vec<(String, u32)>>,
    latest: Mutex<Option<Metrics>>,
    watched_at: Mutex<Option<Instant>>,
}

/// Samples the machine and every running workload on a background thread, so collecting
/// process lists never blocks supervision.
pub struct Monitor {
    shared: Arc<Shared>,
    /// Sending wakes the sampler early; dropping the sender stops it.
    wake: Option<mpsc::Sender<()>>,
    thread: Option<JoinHandle<()>>,
}

impl Monitor {
    pub fn start(data: PathBuf) -> Self {
        let shared = Arc::new(Shared {
            roster: Mutex::new(vec![]),
            latest: Mutex::new(None),
            watched_at: Mutex::new(None),
        });
        let (wake, woken) = mpsc::channel();
        let worker = shared.clone();
        let thread = std::thread::Builder::new()
            .name("metrics".into())
            .spawn(move || Sampler::new(data).run(&worker, &woken))
            .ok();
        Self {
            shared,
            wake: Some(wake),
            thread,
        }
    }

    /// Workloads with a live process, by name and root PID.
    pub fn track(&self, roster: Vec<(String, u32)>) {
        if let Ok(mut current) = self.shared.roster.lock() {
            *current = roster;
        }
    }

    /// The latest sample, with history limited to points after `since`.
    pub fn snapshot(&self, since: Option<DateTime<Utc>>) -> Option<Metrics> {
        if let Ok(mut watched) = self.shared.watched_at.lock() {
            let idle = watched.is_none_or(|at| at.elapsed() >= WATCH_WINDOW);
            *watched = Some(Instant::now());
            // The sampler may be partway through a long idle wait; switch to the fast rate now.
            if idle && let Some(wake) = &self.wake {
                let _ = wake.send(());
            }
        }
        let latest = self.shared.latest.lock().ok()?;
        let metrics = latest.as_ref()?;
        Some(Metrics {
            at: metrics.at,
            machine: metrics.machine.clone(),
            agent: metrics.agent,
            managed: metrics.managed,
            workloads: metrics.workloads.clone(),
            data_bytes: metrics.data_bytes,
            history: metrics
                .history
                .iter()
                .filter(|point| since.is_none_or(|since| point.at > since))
                .copied()
                .collect(),
        })
    }
}

impl Drop for Monitor {
    fn drop(&mut self) {
        self.wake.take();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

struct Sampler {
    data: PathBuf,
    system: System,
    disks: Disks,
    disks_refreshed: Instant,
    last: Instant,
    history: VecDeque<Point>,
    sparks: HashMap<String, (VecDeque<f32>, VecDeque<u64>)>,
    meter: ProcessMeter,
    /// Processes measured in the last sample. They are refreshed once more so that sysinfo
    /// forgets the ones that exited.
    measured: Vec<Pid>,
    host: String,
    os: String,
}

fn on_volume(path: &Path, mount: &Path) -> bool {
    #[cfg(windows)]
    {
        volume_key(&path.to_string_lossy()).starts_with(&volume_key(&mount.to_string_lossy()))
    }
    #[cfg(not(windows))]
    {
        path.starts_with(mount)
    }
}

/// The data directory is canonical, so on Windows it starts with `\\?\`, while volumes are
/// listed as plain `C:\`. Drive letters and paths also compare case-insensitively.
#[cfg(any(windows, test))]
fn volume_key(path: &str) -> String {
    let mut path = path.replace('/', "\\");
    if let Some(rest) = path.strip_prefix(r"\\?\UNC\") {
        path = format!(r"\\{rest}");
    } else if let Some(rest) = path.strip_prefix(r"\\?\") {
        path = rest.to_string();
    }
    let mut path = path.to_lowercase();
    if !path.ends_with('\\') {
        path.push('\\');
    }
    path
}

impl Sampler {
    fn new(data: PathBuf) -> Self {
        Self {
            data,
            system: System::new(),
            disks: Disks::new_with_refreshed_list(),
            disks_refreshed: Instant::now(),
            last: Instant::now(),
            history: VecDeque::new(),
            sparks: HashMap::new(),
            meter: ProcessMeter::default(),
            measured: vec![],
            host: System::host_name().unwrap_or_default(),
            os: System::long_os_version().unwrap_or_default(),
        }
    }

    fn run(mut self, shared: &Shared, woken: &mpsc::Receiver<()>) {
        // Machine CPU is a difference between refreshes, so this only sets a baseline.
        self.system.refresh_cpu_usage();
        let mut wait = sysinfo::MINIMUM_CPU_UPDATE_INTERVAL.max(Duration::from_millis(500));
        loop {
            if let Err(mpsc::RecvTimeoutError::Disconnected) = woken.recv_timeout(wait) {
                return;
            }
            let roster = shared
                .roster
                .lock()
                .map(|roster| roster.clone())
                .unwrap_or_default();
            let metrics = self.sample(&roster);
            if let Ok(mut latest) = shared.latest.lock() {
                *latest = Some(metrics);
            }
            let watched = shared
                .watched_at
                .lock()
                .ok()
                .and_then(|watched| *watched)
                .is_some_and(|at| at.elapsed() < WATCH_WINDOW);
            wait = if watched {
                WATCHED_INTERVAL
            } else {
                IDLE_INTERVAL
            };
        }
    }

    fn sample(&mut self, roster: &[(String, u32)]) -> Metrics {
        self.system.refresh_cpu_usage();
        self.system.refresh_memory();
        if self.disks_refreshed.elapsed() > Duration::from_secs(30) {
            self.disks.refresh(true);
            self.disks_refreshed = Instant::now();
        }
        let elapsed = self.last.elapsed().as_secs_f64().max(0.001);
        self.last = Instant::now();
        let now = Utc::now();
        let cores = self.system.cpus().len().max(1);
        let own = Pid::from_u32(std::process::id());
        let trees = self.trees(roster);
        let mut measured: Vec<Pid> = trees.iter().flatten().copied().collect();
        measured.push(own);
        measured.sort_unstable();
        measured.dedup();
        let mut refresh = measured.clone();
        refresh.extend(&self.measured);
        refresh.sort_unstable();
        refresh.dedup();
        self.system.refresh_processes_specifics(
            ProcessesToUpdate::Some(&refresh),
            true,
            ProcessRefreshKind::nothing()
                .with_cpu()
                .with_memory()
                .with_disk_usage(),
        );
        self.meter.update(&self.system, &measured, elapsed);
        self.measured = measured;
        let usage_of = |pids: &[Pid]| {
            let mut total = Usage::default();
            for pid in pids {
                if let Some(process) = self.system.process(*pid) {
                    total.add(&self.meter.usage(*pid, cores, process.memory()));
                }
            }
            total
        };
        let mut managed = Usage::default();
        let mut workloads = Vec::with_capacity(roster.len());
        for ((name, _), pids) in roster.iter().zip(&trees) {
            let usage = usage_of(pids);
            managed.add(&usage);
            let (cpu, memory) = self.sparks.entry(name.clone()).or_default();
            push_bounded(cpu, usage.cpu);
            push_bounded(memory, usage.memory);
            workloads.push(WorkloadUsage {
                name: name.clone(),
                pids: pids.iter().map(|pid| pid.as_u32()).collect(),
                usage,
                log_bytes: log_bytes(&self.data, name),
                cpu_history: cpu.iter().copied().collect(),
                memory_history: memory.iter().copied().collect(),
            });
        }
        self.sparks
            .retain(|name, _| roster.iter().any(|(tracked, _)| tracked == name));
        let agent = usage_of(&[own]);
        let machine = self.machine(cores);
        self.history.push_back(Point {
            at: now,
            cpu: machine.cpu,
            memory_used: machine.memory_used,
            managed_cpu: managed.cpu,
            managed_memory: managed.memory,
        });
        while self
            .history
            .front()
            .is_some_and(|point| now - point.at > HISTORY)
        {
            self.history.pop_front();
        }
        Metrics {
            at: now,
            machine,
            agent,
            managed,
            workloads,
            data_bytes: data_bytes(&self.data),
            history: self.history.iter().copied().collect(),
        }
    }

    /// Each workload's process tree, root first. Asks the kernel for children where it can;
    /// otherwise lists every process once to find parent links, which costs far more.
    fn trees(&mut self, roster: &[(String, u32)]) -> Vec<Vec<Pid>> {
        let direct: Option<Vec<Vec<Pid>>> = roster
            .iter()
            .map(|(_, root)| descendants(Pid::from_u32(*root)))
            .collect();
        if let Some(trees) = direct {
            return trees;
        }
        self.system.refresh_processes_specifics(
            ProcessesToUpdate::All,
            true,
            ProcessRefreshKind::nothing().without_tasks(),
        );
        let mut children: HashMap<Pid, Vec<Pid>> = HashMap::new();
        for (pid, process) in self.system.processes() {
            if let Some(parent) = process.parent() {
                children.entry(parent).or_default().push(*pid);
            }
        }
        roster
            .iter()
            .map(|(_, root)| tree(Pid::from_u32(*root), &children))
            .collect()
    }

    fn machine(&self, cores: usize) -> Machine {
        let volume = self
            .disks
            .list()
            .iter()
            .filter(|disk| on_volume(&self.data, disk.mount_point()))
            .max_by_key(|disk| disk.mount_point().as_os_str().len());
        let load = System::load_average();
        Machine {
            host: self.host.clone(),
            os: self.os.clone(),
            cores,
            cpu: self.system.global_cpu_usage(),
            memory_total: self.system.total_memory(),
            memory_used: self.system.used_memory(),
            swap_total: self.system.total_swap(),
            swap_used: self.system.used_swap(),
            disk_total: volume.map_or(0, |disk| disk.total_space()),
            disk_available: volume.map_or(0, |disk| disk.available_space()),
            load: (!cfg!(windows)).then_some([load.one, load.five, load.fifteen]),
            uptime_secs: System::uptime(),
        }
    }
}

/// Cumulative counters for one process, keyed by start time so a reused PID starts fresh.
#[derive(Clone, Copy)]
struct Counters {
    started: u64,
    cpu_ms: u64,
    read: u64,
    written: u64,
}

#[derive(Clone, Copy, Default)]
struct Rates {
    /// Cores' worth of CPU in use.
    cores: f64,
    read: f64,
    write: f64,
}

/// Rates from differences of cumulative counters. sysinfo's own per-refresh CPU keeps its last
/// value on macOS while a process is not scheduled, which would show idle workloads as busy.
#[derive(Default)]
struct ProcessMeter {
    previous: HashMap<Pid, Counters>,
    rates: HashMap<Pid, Rates>,
}

impl ProcessMeter {
    fn update(&mut self, system: &System, pids: &[Pid], elapsed: f64) {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |since| since.as_secs());
        let mut current = HashMap::with_capacity(pids.len());
        self.rates.clear();
        for pid in pids {
            let Some(process) = system.process(*pid) else {
                continue;
            };
            let disk = process.disk_usage();
            let counters = Counters {
                started: process.start_time(),
                cpu_ms: process.accumulated_cpu_time(),
                read: disk.total_read_bytes,
                written: disk.total_written_bytes,
            };
            let rates = match self.previous.get(pid) {
                Some(was) if was.started == counters.started => Rates {
                    cores: counters.cpu_ms.saturating_sub(was.cpu_ms) as f64 / 1000.0 / elapsed,
                    read: counters.read.saturating_sub(was.read) as f64 / elapsed,
                    write: counters.written.saturating_sub(was.written) as f64 / elapsed,
                },
                // First sighting: the average over its life, which is short for anything new.
                _ => {
                    let age = (now.saturating_sub(counters.started) as f64).max(elapsed);
                    Rates {
                        cores: counters.cpu_ms as f64 / 1000.0 / age,
                        read: counters.read as f64 / age,
                        write: counters.written as f64 / age,
                    }
                }
            };
            self.rates.insert(*pid, rates);
            current.insert(*pid, counters);
        }
        self.previous = current;
    }

    fn usage(&self, pid: Pid, cores: usize, memory: u64) -> Usage {
        let rates = self.rates.get(&pid).copied().unwrap_or_default();
        Usage {
            cpu: ((rates.cores / cores as f64 * 100.0) as f32).min(100.0),
            memory,
            read_rate: rates.read as u64,
            write_rate: rates.write as u64,
            processes: 1,
        }
    }
}

/// Descendants via the kernel's child lists, or `None` where the platform has none.
fn descendants(root: Pid) -> Option<Vec<Pid>> {
    let mut found = vec![root];
    let mut next = 0;
    while let Some(&pid) = found.get(next) {
        next += 1;
        for child in crate::platform::children(pid.as_u32())? {
            let child = Pid::from_u32(child);
            if !found.contains(&child) {
                found.push(child);
            }
        }
    }
    Some(found)
}

/// The root and all of its descendants, root first. Unknown roots yield an empty tree.
fn tree(root: Pid, children: &HashMap<Pid, Vec<Pid>>) -> Vec<Pid> {
    let mut found = vec![];
    let mut pending = vec![root];
    while let Some(pid) = pending.pop() {
        if found.contains(&pid) {
            continue;
        }
        found.push(pid);
        if let Some(next) = children.get(&pid) {
            pending.extend(next.iter().rev());
        }
    }
    found
}

fn push_bounded<T>(values: &mut VecDeque<T>, value: T) {
    values.push_back(value);
    while values.len() > SPARK_POINTS {
        values.pop_front();
    }
}

fn file_size(path: &Path) -> u64 {
    std::fs::metadata(path).map_or(0, |metadata| metadata.len())
}

fn log_bytes(data: &Path, name: &str) -> u64 {
    let logs = data.join("logs");
    file_size(&logs.join(format!("{name}.log"))) + file_size(&logs.join(format!("{name}.log.1")))
}

fn data_bytes(data: &Path) -> u64 {
    let size = |directory: &Path| {
        std::fs::read_dir(directory).map_or(0, |entries| {
            entries
                .flatten()
                .filter_map(|entry| entry.metadata().ok())
                .filter(|metadata| metadata.is_file())
                .map(|metadata| metadata.len())
                .sum()
        })
    };
    size(data) + size(&data.join("logs"))
}

/// Samples a single process, such as the desktop app itself.
pub struct ProcessProbe {
    system: System,
    pid: Pid,
    cores: usize,
    meter: ProcessMeter,
    last: Instant,
}

impl Default for ProcessProbe {
    fn default() -> Self {
        let mut system = System::new();
        system.refresh_cpu_list(sysinfo::CpuRefreshKind::nothing());
        let cores = system.cpus().len().max(1);
        let mut probe = Self {
            system,
            pid: Pid::from_u32(std::process::id()),
            cores,
            meter: ProcessMeter::default(),
            last: Instant::now(),
        };
        probe.sample();
        probe
    }
}

impl ProcessProbe {
    pub fn sample(&mut self) -> Usage {
        self.system.refresh_processes_specifics(
            ProcessesToUpdate::Some(&[self.pid]),
            true,
            ProcessRefreshKind::nothing()
                .with_cpu()
                .with_memory()
                .with_disk_usage(),
        );
        let elapsed = self.last.elapsed().as_secs_f64().max(0.001);
        self.last = Instant::now();
        self.meter.update(&self.system, &[self.pid], elapsed);
        self.system
            .process(self.pid)
            .map_or(Usage::default(), |process| {
                self.meter.usage(self.pid, self.cores, process.memory())
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tree_walks_descendants_once() {
        let children = HashMap::from([
            (Pid::from_u32(1), vec![Pid::from_u32(2), Pid::from_u32(3)]),
            (Pid::from_u32(2), vec![Pid::from_u32(4)]),
            (Pid::from_u32(4), vec![Pid::from_u32(2)]),
        ]);
        let pids: Vec<u32> = tree(Pid::from_u32(1), &children)
            .iter()
            .map(|pid| pid.as_u32())
            .collect();
        assert_eq!(pids, vec![1, 2, 4, 3]);
    }

    #[test]
    fn windows_volumes_match_canonical_paths() {
        let data = volume_key(r"\\?\C:\Users\me\AppData\Local\K3 Up");
        assert!(data.starts_with(&volume_key(r"C:\")));
        assert!(data.starts_with(&volume_key("c:/users")));
        assert!(!data.starts_with(&volume_key(r"D:\")));
        assert!(!data.starts_with(&volume_key(r"C:\Use")));
        let share = volume_key(r"\\?\UNC\server\share\k3up");
        assert!(share.starts_with(&volume_key(r"\\server\share\")));
    }

    #[test]
    fn monitor_reports_its_own_workload() {
        let data = tempfile::tempdir().unwrap();
        let monitor = Monitor::start(data.path().to_path_buf());
        monitor.track(vec![("self".into(), std::process::id())]);
        let deadline = Instant::now() + Duration::from_secs(5);
        let metrics = loop {
            if let Some(metrics) = monitor.snapshot(None)
                && !metrics.workloads.is_empty()
            {
                break metrics;
            }
            assert!(Instant::now() < deadline, "no sample within 5s");
            std::thread::sleep(Duration::from_millis(50));
        };
        assert!(metrics.machine.memory_total > 0);
        assert!(metrics.machine.cores > 0);
        let own = &metrics.workloads[0];
        assert_eq!(own.pids[0], std::process::id());
        assert!(own.usage.memory > 0);
        assert!(!metrics.history.is_empty());
    }

    /// Uses dedicated child processes: measuring the test process itself would count
    /// whatever the tests running beside it do.
    #[cfg(unix)]
    #[test]
    fn busy_and_idle_processes_are_told_apart() {
        let mut busy = std::process::Command::new("sh")
            .args(["-c", "while :; do :; done"])
            .spawn()
            .unwrap();
        let mut idle = std::process::Command::new("sleep")
            .arg("30")
            .spawn()
            .unwrap();
        let pids = [Pid::from_u32(busy.id()), Pid::from_u32(idle.id())];
        let mut system = System::new();
        let mut meter = ProcessMeter::default();
        let refresh = |system: &mut System| {
            system.refresh_processes_specifics(
                ProcessesToUpdate::Some(&pids),
                true,
                ProcessRefreshKind::nothing().with_cpu(),
            );
        };
        refresh(&mut system);
        meter.update(&system, &pids, 1.0);
        let started = Instant::now();
        std::thread::sleep(Duration::from_millis(500));
        refresh(&mut system);
        meter.update(&system, &pids, started.elapsed().as_secs_f64());
        let busy_cpu = meter.usage(pids[0], 1, 0).cpu;
        let idle_cpu = meter.usage(pids[1], 1, 0).cpu;
        let _ = busy.kill();
        let _ = idle.kill();
        let _ = busy.wait();
        let _ = idle.wait();
        assert!(busy_cpu > 20.0, "busy child at {busy_cpu}%");
        assert!(idle_cpu < 5.0, "idle child at {idle_cpu}%");
    }
}
