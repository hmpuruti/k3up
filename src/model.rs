use anyhow::{Context, Result, bail};
use chrono::{DateTime, Duration, Utc};
use chrono_tz::Tz;
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::Path,
    str::FromStr,
};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct Workload {
    pub name: String,
    pub description: String,
    pub executable: String,
    pub args: Vec<String>,
    pub working_directory: String,
    pub environment: BTreeMap<String, String>,
    pub kind: Kind,
    pub start_at_boot: bool,
    pub depends_on: Vec<String>,
    pub restart: Restart,
    pub max_restarts: u32,
    pub restart_delay_secs: u64,
    pub stop_timeout_secs: u64,
    pub run_timeout_secs: Option<u64>,
    pub readiness_tcp: Option<String>,
    pub startup_timeout_secs: u64,
    pub schedule: Option<Schedule>,
}

impl Default for Workload {
    fn default() -> Self {
        Self {
            name: String::new(),
            description: String::new(),
            executable: String::new(),
            args: vec![],
            working_directory: String::new(),
            environment: BTreeMap::new(),
            kind: Kind::Service,
            start_at_boot: false,
            depends_on: vec![],
            restart: Restart::OnFailure,
            max_restarts: 5,
            restart_delay_secs: 2,
            stop_timeout_secs: 5,
            run_timeout_secs: None,
            readiness_tcp: None,
            startup_timeout_secs: 30,
            schedule: None,
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    #[default]
    Service,
    Job,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum Restart {
    Never,
    #[default]
    OnFailure,
    Always,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Schedule {
    pub every_secs: Option<u64>,
    /// Six or seven fields, including seconds, as accepted by the cron crate.
    pub cron: Option<String>,
    #[serde(default = "utc")]
    pub timezone: String,
    #[serde(default)]
    pub action: ScheduleAction,
    #[serde(default)]
    pub missed: Missed,
}
fn utc() -> String {
    "UTC".into()
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum ScheduleAction {
    #[default]
    Start,
    Restart,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum Missed {
    #[default]
    Skip,
    RunOnce,
}

impl Schedule {
    /// `None` means a bounded cron expression (one with a year field) has no executions left.
    pub fn next_after(&self, now: DateTime<Utc>) -> Result<Option<DateTime<Utc>>> {
        match (self.every_secs, &self.cron) {
            (Some(seconds), None) if (1..=31_536_000).contains(&seconds) => {
                Ok(Some(now + Duration::seconds(seconds as i64)))
            }
            (None, Some(expression)) => {
                let zone = Tz::from_str(&self.timezone).context("Unknown IANA timezone")?;
                Ok(cron::Schedule::from_str(expression)
                    .context("Cron requires 6 or 7 fields, including seconds")?
                    .after(&now.with_timezone(&zone))
                    .next()
                    .map(|next| next.with_timezone(&Utc)))
            }
            _ => bail!("Schedule needs either every_secs (1..31536000) or cron, exclusively"),
        }
    }
}

/// Which platform's path rules apply. Native systemd export checks Linux paths on any host.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PathStyle {
    Host,
    Unix,
}

impl PathStyle {
    fn is_absolute(self, path: &str) -> bool {
        match self {
            Self::Host => Path::new(path).is_absolute(),
            Self::Unix => path.starts_with('/'),
        }
    }
}

impl Workload {
    /// First execution for a new or changed definition. Registering an exhausted schedule is refused.
    pub fn first_run(&self, now: DateTime<Utc>) -> Result<Option<DateTime<Utc>>> {
        let Some(schedule) = &self.schedule else {
            return Ok(None);
        };
        schedule
            .next_after(now)?
            .map(Some)
            .with_context(|| format!("Schedule for '{}' has no future execution", self.name))
    }

    pub fn validate(&self) -> Result<()> {
        self.validate_for(PathStyle::Host)
    }

    pub fn validate_for(&self, paths: PathStyle) -> Result<()> {
        if self.name.is_empty()
            || self.name.len() > 64
            || !self
                .name
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
        {
            bail!("Name must be 1–64 ASCII letters, digits, underscores, or hyphens");
        }
        if !paths.is_absolute(&self.executable) || self.executable.contains(['\0', '\n', '\r']) {
            bail!("Executable must be an absolute path without control characters");
        }
        if !paths.is_absolute(&self.working_directory)
            || self.working_directory.contains(['\0', '\n', '\r'])
        {
            bail!("Working directory must be an absolute path");
        }
        if self.args.iter().any(|s| s.contains('\0'))
            || self
                .environment
                .iter()
                .any(|(k, v)| k.is_empty() || k.contains(['=', '\0']) || v.contains('\0'))
        {
            bail!("Invalid argument or environment variable");
        }
        if self.max_restarts > 100
            || !(1..=300).contains(&self.restart_delay_secs)
            || !(1..=30).contains(&self.stop_timeout_secs)
            || !(1..=300).contains(&self.startup_timeout_secs)
        {
            bail!(
                "Recovery limits: max_restarts <= 100, restart delay/startup timeout 1–300s, stop timeout 1–30s"
            );
        }
        if self.run_timeout_secs == Some(0) {
            bail!("Run timeout must be positive");
        }
        if let Some(address) = &self.readiness_tcp {
            address
                .parse::<std::net::SocketAddr>()
                .context("Readiness must be an IP:port, for example 127.0.0.1:8080")?;
            if self.kind == Kind::Job {
                bail!("Readiness checks apply to services only");
            }
        }
        if self.depends_on.contains(&self.name) {
            bail!("A workload cannot depend on itself");
        }
        if self.depends_on.iter().collect::<BTreeSet<_>>().len() != self.depends_on.len() {
            bail!("Duplicate dependencies");
        }
        if let Some(schedule) = &self.schedule {
            Tz::from_str(&schedule.timezone).context("Unknown IANA timezone")?;
            schedule.next_after(Utc::now())?;
            if self.kind == Kind::Job && schedule.action == ScheduleAction::Restart {
                bail!("Jobs use schedule action 'start'; overlapping runs are skipped");
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    pub version: u32,
    pub workloads: Vec<Workload>,
}

impl Manifest {
    pub fn validate(&self) -> Result<Vec<String>> {
        self.validate_for(PathStyle::Host)
    }

    pub fn validate_for(&self, paths: PathStyle) -> Result<Vec<String>> {
        if self.version != 1 {
            bail!("Unsupported manifest version {}; expected 1", self.version);
        }
        let mut by_name = BTreeMap::new();
        for workload in &self.workloads {
            workload
                .validate_for(paths)
                .with_context(|| format!("Invalid workload '{}'", workload.name))?;
            if by_name.insert(workload.name.clone(), workload).is_some() {
                bail!("Duplicate name '{}'", workload.name);
            }
        }
        let mut ordered = Vec::new();
        let mut visiting = BTreeSet::new();
        let mut visited = BTreeSet::new();
        fn visit(
            name: &str,
            all: &BTreeMap<String, &Workload>,
            visiting: &mut BTreeSet<String>,
            visited: &mut BTreeSet<String>,
            ordered: &mut Vec<String>,
        ) -> Result<()> {
            if visited.contains(name) {
                return Ok(());
            }
            if !visiting.insert(name.into()) {
                bail!("Dependency cycle involving '{name}'");
            }
            let workload = all
                .get(name)
                .with_context(|| format!("Missing dependency '{name}'"))?;
            for dependency in &workload.depends_on {
                visit(dependency, all, visiting, visited, ordered)?;
            }
            visiting.remove(name);
            visited.insert(name.into());
            ordered.push(name.into());
            Ok(())
        }
        for name in by_name.keys() {
            visit(name, &by_name, &mut visiting, &mut visited, &mut ordered)?;
        }
        Ok(ordered)
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum State {
    #[default]
    Stopped,
    Pending,
    Blocked,
    Starting,
    Running,
    Backoff,
    Completed,
    Failed,
}

impl State {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Stopped => "stopped",
            Self::Pending => "pending",
            Self::Blocked => "blocked",
            Self::Starting => "starting",
            Self::Running => "running",
            Self::Backoff => "backoff",
            Self::Completed => "completed",
            Self::Failed => "failed",
        }
    }
}

impl std::fmt::Display for State {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.pad(self.as_str())
    }
}

impl PartialEq<&str> for State {
    fn eq(&self, other: &&str) -> bool {
        self.as_str() == *other
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Status {
    pub workload: Workload,
    pub state: State,
    pub desired_running: bool,
    pub pid: Option<u32>,
    pub restart_count: u32,
    pub started_at: Option<DateTime<Utc>>,
    pub next_run: Option<DateTime<Utc>>,
    pub last_exit: Option<i32>,
    pub reason: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Event {
    pub id: i64,
    pub at: DateTime<Utc>,
    pub name: String,
    pub message: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    fn service(name: &str) -> Workload {
        Workload {
            name: name.into(),
            executable: "/bin/sleep".into(),
            working_directory: "/tmp".into(),
            ..Default::default()
        }
    }
    #[test]
    fn orders_dependencies_and_rejects_cycles() {
        let mut api = service("api");
        api.depends_on.push("db".into());
        let mut db = service("db");
        let mut manifest = Manifest {
            version: 1,
            workloads: vec![api.clone(), db.clone()],
        };
        assert_eq!(manifest.validate().unwrap(), vec!["db", "api"]);
        db.depends_on.push("api".into());
        manifest.workloads = vec![api, db];
        assert!(
            manifest
                .validate()
                .unwrap_err()
                .to_string()
                .contains("cycle")
        );
    }
    #[test]
    fn rejects_missing_dependencies_and_unsafe_names() {
        let mut api = service("api");
        api.depends_on.push("missing".into());
        assert!(
            Manifest {
                version: 1,
                workloads: vec![api]
            }
            .validate()
            .is_err()
        );
        assert!(service("../../escape").validate().is_err());
    }
    #[test]
    fn schedule_respects_timezone() {
        let schedule = Schedule {
            every_secs: None,
            cron: Some("0 0 2 * * *".into()),
            timezone: "Africa/Dar_es_Salaam".into(),
            action: ScheduleAction::Start,
            missed: Missed::Skip,
        };
        let now = "2026-09-07T12:00:00Z".parse().unwrap();
        assert_eq!(
            schedule.next_after(now).unwrap().unwrap().to_rfc3339(),
            "2026-09-07T23:00:00+00:00"
        );
    }
    #[test]
    fn exhausted_cron_is_valid_but_cannot_be_registered() {
        let mut job = service("yearly");
        job.schedule = Some(Schedule {
            every_secs: None,
            cron: Some("0 0 2 1 1 * 2020".into()),
            timezone: "UTC".into(),
            action: ScheduleAction::Start,
            missed: Missed::Skip,
        });
        assert!(job.validate().is_ok());
        assert_eq!(
            job.schedule
                .as_ref()
                .unwrap()
                .next_after(Utc::now())
                .unwrap(),
            None
        );
        assert!(
            job.first_run(Utc::now())
                .unwrap_err()
                .to_string()
                .contains("no future execution")
        );
    }
    #[test]
    fn rejects_ambiguous_schedules() {
        let schedule = Schedule {
            every_secs: Some(5),
            cron: Some("0 * * * * *".into()),
            timezone: "UTC".into(),
            action: ScheduleAction::Start,
            missed: Missed::Skip,
        };
        assert!(schedule.next_after(Utc::now()).is_err());
    }
}
