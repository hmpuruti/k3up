use anyhow::{Context, Result, bail};
use chrono::{DateTime, Duration, Utc};
use chrono_tz::Tz;
use serde::{Deserialize, Deserializer, Serialize, Serializer, de, ser::SerializeStruct};
use std::{
    collections::{BTreeMap, BTreeSet},
    fmt,
    path::Path,
    str::FromStr,
};

pub const MAX_RESTART_COUNT: u32 = 100;
pub const MAX_SUCCESS_EXIT_CODES: usize = 32;
const MAX_RETRY_DELAY_SECS: u64 = 300;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct Workload {
    pub name: String,
    pub description: String,
    /// Folder path such as `watchtower/entra`; empty means no folder.
    pub group: String,
    pub executable: String,
    pub args: Vec<String>,
    pub working_directory: String,
    pub environment: BTreeMap<String, String>,
    pub kind: Kind,
    pub start_at_boot: bool,
    pub depends_on: Vec<String>,
    pub restart: Restart,
    pub max_restarts: RestartLimit,
    pub restart_delay_secs: u64,
    pub restart_backoff: RestartBackoff,
    pub success_exit_codes: Vec<i32>,
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
            group: String::new(),
            executable: String::new(),
            args: vec![],
            working_directory: String::new(),
            environment: BTreeMap::new(),
            kind: Kind::Service,
            start_at_boot: false,
            depends_on: vec![],
            restart: Restart::OnFailure,
            max_restarts: RestartLimit::default(),
            restart_delay_secs: 2,
            restart_backoff: RestartBackoff::Exponential,
            success_exit_codes: vec![],
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RestartLimit {
    Count(u32),
    Unlimited,
}

impl RestartLimit {
    pub fn allows(self, retries: u32) -> bool {
        match self {
            Self::Count(limit) => retries < limit,
            Self::Unlimited => true,
        }
    }
}

impl Default for RestartLimit {
    fn default() -> Self {
        Self::Count(5)
    }
}

impl fmt::Display for RestartLimit {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Count(limit) => write!(f, "{limit}"),
            Self::Unlimited => f.write_str("unlimited"),
        }
    }
}

impl FromStr for RestartLimit {
    type Err = String;
    fn from_str(text: &str) -> Result<Self, String> {
        if text.eq_ignore_ascii_case("unlimited") {
            return Ok(Self::Unlimited);
        }
        text.parse()
            .map(Self::Count)
            .map_err(|_| "expected a whole number or \"unlimited\"".to_string())
    }
}

impl Serialize for RestartLimit {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            Self::Count(limit) => serializer.serialize_u32(*limit),
            Self::Unlimited => serializer.serialize_str("unlimited"),
        }
    }
}

impl<'de> Deserialize<'de> for RestartLimit {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct Visitor;
        impl de::Visitor<'_> for Visitor {
            type Value = RestartLimit;
            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("a whole number or \"unlimited\"")
            }
            fn visit_u64<E: de::Error>(self, value: u64) -> Result<Self::Value, E> {
                u32::try_from(value)
                    .map(RestartLimit::Count)
                    .map_err(|_| E::custom("max_restarts is too large"))
            }
            fn visit_i64<E: de::Error>(self, value: i64) -> Result<Self::Value, E> {
                u32::try_from(value)
                    .map(RestartLimit::Count)
                    .map_err(|_| E::custom("max_restarts must be 0 or more"))
            }
            fn visit_str<E: de::Error>(self, value: &str) -> Result<Self::Value, E> {
                value.parse().map_err(|_| {
                    E::custom(format!(
                        "max_restarts must be a whole number or \"unlimited\", not \"{value}\""
                    ))
                })
            }
        }
        deserializer.deserialize_any(Visitor)
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum RestartBackoff {
    #[default]
    Exponential,
    Fixed,
}

/// Seconds to wait before a retry, given how many retries came before it.
pub fn retry_delay(backoff: RestartBackoff, base_secs: u64, retries: u32) -> u64 {
    match backoff {
        RestartBackoff::Fixed => base_secs,
        RestartBackoff::Exponential => base_secs
            .saturating_mul(2u64.saturating_pow(retries.min(16)))
            .min(MAX_RETRY_DELAY_SECS),
    }
}

/// A definition as people read it: restart settings are left out of jobs, which never restart.
struct Printable<'a>(&'a Workload);

impl Serialize for Printable<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let workload = self.0;
        let mut state = serializer.serialize_struct("Workload", 21)?;
        state.serialize_field("name", &workload.name)?;
        state.serialize_field("description", &workload.description)?;
        if !workload.group.is_empty() {
            state.serialize_field("group", &workload.group)?;
        }
        state.serialize_field("executable", &workload.executable)?;
        state.serialize_field("args", &workload.args)?;
        state.serialize_field("working_directory", &workload.working_directory)?;
        state.serialize_field("environment", &workload.environment)?;
        state.serialize_field("kind", &workload.kind)?;
        state.serialize_field("start_at_boot", &workload.start_at_boot)?;
        state.serialize_field("depends_on", &workload.depends_on)?;
        if workload.kind == Kind::Service {
            state.serialize_field("restart", &workload.restart)?;
            state.serialize_field("max_restarts", &workload.max_restarts)?;
            state.serialize_field("restart_delay_secs", &workload.restart_delay_secs)?;
            state.serialize_field("restart_backoff", &workload.restart_backoff)?;
        }
        state.serialize_field("success_exit_codes", &workload.success_exit_codes)?;
        state.serialize_field("stop_timeout_secs", &workload.stop_timeout_secs)?;
        state.serialize_field("run_timeout_secs", &workload.run_timeout_secs)?;
        state.serialize_field("readiness_tcp", &workload.readiness_tcp)?;
        state.serialize_field("startup_timeout_secs", &workload.startup_timeout_secs)?;
        state.serialize_field("schedule", &workload.schedule)?;
        state.end()
    }
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
    pub fn is_success(&self, code: i32) -> bool {
        code == 0 || self.success_exit_codes.contains(&code)
    }

    pub fn to_toml(&self) -> Result<String, toml::ser::Error> {
        toml::to_string_pretty(&Printable(self))
    }

    /// Group and description are labels: changing them never affects a running process.
    pub fn only_labels_differ(&self, other: &Workload) -> bool {
        let relabelled = Workload {
            group: other.group.clone(),
            description: other.description.clone(),
            ..self.clone()
        };
        self != other && relabelled == *other
    }

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
        crate::group::validate(&self.group)?;
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
        if matches!(self.max_restarts, RestartLimit::Count(limit) if limit > MAX_RESTART_COUNT)
            || !(1..=300).contains(&self.restart_delay_secs)
            || !(1..=30).contains(&self.stop_timeout_secs)
            || !(1..=300).contains(&self.startup_timeout_secs)
        {
            bail!(
                "Recovery limits: max_restarts <= 100 or \"unlimited\", restart delay/startup timeout 1–300s, stop timeout 1–30s"
            );
        }
        if self.success_exit_codes.len() > MAX_SUCCESS_EXIT_CODES {
            bail!("At most {MAX_SUCCESS_EXIT_CODES} success exit codes");
        }
        if self.success_exit_codes.contains(&0) {
            bail!("Exit code 0 always counts as success; leave it out of success_exit_codes");
        }
        if self
            .success_exit_codes
            .iter()
            .collect::<BTreeSet<_>>()
            .len()
            != self.success_exit_codes.len()
        {
            bail!("Duplicate success exit codes");
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
    pub fn to_toml(&self) -> Result<String, toml::ser::Error> {
        #[derive(Serialize)]
        struct View<'a> {
            version: u32,
            workloads: Vec<Printable<'a>>,
        }
        toml::to_string_pretty(&View {
            version: self.version,
            workloads: self.workloads.iter().map(Printable).collect(),
        })
    }

    pub fn validate(&self) -> Result<Vec<String>> {
        self.validate_for(PathStyle::Host)
    }

    pub fn validate_for(&self, paths: PathStyle) -> Result<Vec<String>> {
        if self.version != 1 {
            bail!("Unsupported manifest version {}; expected 1", self.version);
        }
        for workload in &self.workloads {
            workload
                .validate_for(paths)
                .with_context(|| format!("Invalid workload '{}'", workload.name))?;
        }
        self.order()
    }

    /// Startup order: dependencies before dependents, otherwise by name. Groups play no part.
    pub fn order(&self) -> Result<Vec<String>> {
        let mut by_name = BTreeMap::new();
        for workload in &self.workloads {
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

#[derive(
    Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash, Default,
)]
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
    /// Paths must be absolute on the platform running the tests.
    fn service(name: &str) -> Workload {
        let (executable, working_directory) = if cfg!(windows) {
            (r"C:\Windows\System32\timeout.exe", r"C:\Windows\Temp")
        } else {
            ("/bin/sleep", "/tmp")
        };
        Workload {
            name: name.into(),
            executable: executable.into(),
            working_directory: working_directory.into(),
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
    #[test]
    fn restart_limit_reads_and_writes_a_number_or_unlimited() {
        let mut counted = service("counted");
        counted.max_restarts = RestartLimit::Count(7);
        let toml_text = toml::to_string(&counted).unwrap();
        assert!(toml_text.contains("max_restarts = 7\n"));
        assert_eq!(toml::from_str::<Workload>(&toml_text).unwrap(), counted);
        let json_text = serde_json::to_string(&counted).unwrap();
        assert!(json_text.contains("\"max_restarts\":7"));
        assert_eq!(
            serde_json::from_str::<Workload>(&json_text).unwrap(),
            counted
        );

        let mut unlimited = service("unlimited");
        unlimited.max_restarts = RestartLimit::Unlimited;
        let toml_text = toml::to_string(&unlimited).unwrap();
        assert!(toml_text.contains("max_restarts = \"unlimited\"\n"));
        assert_eq!(toml::from_str::<Workload>(&toml_text).unwrap(), unlimited);
        let json_text = serde_json::to_string(&unlimited).unwrap();
        assert!(json_text.contains("\"max_restarts\":\"unlimited\""));
        assert_eq!(
            serde_json::from_str::<Workload>(&json_text).unwrap(),
            unlimited
        );
        assert!(unlimited.validate().is_ok());

        let error = toml::from_str::<Workload>("max_restarts = \"forever\"")
            .unwrap_err()
            .to_string();
        assert!(error.contains("whole number or \"unlimited\""), "{error}");
        assert!(toml::from_str::<Workload>("max_restarts = -1").is_err());
        assert_eq!("unlimited".parse(), Ok(RestartLimit::Unlimited));
        assert_eq!("12".parse(), Ok(RestartLimit::Count(12)));
        assert!("many".parse::<RestartLimit>().is_err());
    }
    #[test]
    fn definitions_without_the_newer_fields_still_load() {
        let stored = r#"{"name":"old","description":"","executable":"/bin/sleep","args":[],
            "working_directory":"/tmp","environment":{},"kind":"service","start_at_boot":false,
            "depends_on":[],"restart":"on_failure","max_restarts":5,"restart_delay_secs":2,
            "stop_timeout_secs":5,"run_timeout_secs":null,"readiness_tcp":null,
            "startup_timeout_secs":30,"schedule":null}"#;
        let workload: Workload = serde_json::from_str(stored).unwrap();
        assert_eq!(workload.max_restarts, RestartLimit::Count(5));
        assert_eq!(workload.restart_backoff, RestartBackoff::Exponential);
        assert!(workload.success_exit_codes.is_empty());
        assert_eq!(workload.group, "");
        let manifest: Manifest = toml::from_str(
            "version = 1\n[[workloads]]\nname = \"old\"\nexecutable = \"/bin/sleep\"\nworking_directory = \"/tmp\"\n",
        )
        .unwrap();
        assert_eq!(manifest.workloads[0].group, "");
    }
    #[test]
    fn group_is_validated_and_printed_only_when_set() {
        let mut grouped = service("users");
        grouped.group = "Watchtower/Entra".into();
        grouped.validate().unwrap();
        let text = grouped.to_toml().unwrap();
        assert!(text.contains("group = \"Watchtower/Entra\"\n"), "{text}");
        assert_eq!(toml::from_str::<Workload>(&text).unwrap(), grouped);
        let json = serde_json::to_value(&grouped).unwrap();
        assert_eq!(json["group"], "Watchtower/Entra");
        let plain = service("plain");
        assert!(!plain.to_toml().unwrap().contains("group"));
        assert_eq!(serde_json::to_value(&plain).unwrap()["group"], "");
        let mut invalid = service("invalid");
        invalid.group = "a//b".into();
        assert!(
            invalid
                .validate()
                .unwrap_err()
                .to_string()
                .contains("empty segments")
        );
        invalid.group = "a/b/c/d/e/f".into();
        assert!(invalid.validate().is_err());
    }
    #[test]
    fn label_changes_are_told_apart_from_real_changes() {
        let base = service("web");
        let mut relabelled = base.clone();
        relabelled.group = "watchtower".into();
        relabelled.description = "Users".into();
        assert!(base.only_labels_differ(&relabelled));
        assert!(relabelled.only_labels_differ(&base));
        assert!(!base.only_labels_differ(&base));
        let mut changed = relabelled.clone();
        changed.args.push("--x".into());
        assert!(!base.only_labels_differ(&changed));
    }
    #[test]
    fn order_ignores_groups() {
        let mut api = service("api");
        api.group = "a".into();
        api.depends_on.push("db".into());
        let mut db = service("db");
        db.group = "z".into();
        let manifest = Manifest {
            version: 1,
            workloads: vec![api, db],
        };
        assert_eq!(manifest.order().unwrap(), vec!["db", "api"]);
        assert_eq!(manifest.validate().unwrap(), vec!["db", "api"]);
    }
    #[test]
    fn retry_delay_doubles_with_a_cap_or_stays_fixed() {
        let exponential = |retries| retry_delay(RestartBackoff::Exponential, 2, retries);
        assert_eq!(exponential(0), 2);
        assert_eq!(exponential(1), 4);
        assert_eq!(exponential(3), 16);
        assert_eq!(exponential(8), 300);
        assert_eq!(exponential(u32::MAX), 300);
        assert_eq!(retry_delay(RestartBackoff::Exponential, 300, 0), 300);
        let fixed = |retries| retry_delay(RestartBackoff::Fixed, 5, retries);
        assert_eq!(fixed(0), 5);
        assert_eq!(fixed(9), 5);
        assert!(RestartLimit::Count(2).allows(1));
        assert!(!RestartLimit::Count(2).allows(2));
        assert!(RestartLimit::Unlimited.allows(u32::MAX));
    }
    #[test]
    fn success_exit_codes_are_bounded_and_unique() {
        let mut job = service("loader");
        job.kind = Kind::Job;
        job.success_exit_codes = vec![3, 75];
        job.validate().unwrap();
        assert!(job.is_success(0) && job.is_success(3) && !job.is_success(1));
        job.success_exit_codes = vec![3, 3];
        assert!(
            job.validate()
                .unwrap_err()
                .to_string()
                .contains("Duplicate")
        );
        job.success_exit_codes = vec![0];
        assert!(job.validate().unwrap_err().to_string().contains("0"));
        job.success_exit_codes = (1..=33).collect();
        assert!(job.validate().unwrap_err().to_string().contains("32"));
    }
    #[test]
    fn printed_toml_omits_restart_settings_for_jobs_only() {
        let mut web = service("web");
        web.description = "Web".into();
        web.args = vec!["--port".into(), "80".into()];
        web.environment.insert("MODE".into(), "prod".into());
        web.start_at_boot = true;
        web.depends_on = vec!["db".into()];
        web.restart = Restart::Always;
        web.max_restarts = RestartLimit::Unlimited;
        web.restart_delay_secs = 5;
        web.restart_backoff = RestartBackoff::Fixed;
        web.success_exit_codes = vec![3];
        web.stop_timeout_secs = 9;
        web.run_timeout_secs = Some(60);
        web.readiness_tcp = Some("127.0.0.1:80".into());
        web.startup_timeout_secs = 12;
        web.schedule = Some(Schedule {
            every_secs: Some(60),
            cron: None,
            timezone: "UTC".into(),
            action: ScheduleAction::Restart,
            missed: Missed::RunOnce,
        });
        let text = web.to_toml().unwrap();
        assert_eq!(toml::from_str::<Workload>(&text).unwrap(), web);

        let mut job = service("job");
        job.kind = Kind::Job;
        job.restart = Restart::Never;
        job.success_exit_codes = vec![3];
        let manifest = Manifest {
            version: 1,
            workloads: vec![web, job.clone()],
        };
        let text = manifest.to_toml().unwrap();
        let parsed: Manifest = toml::from_str(&text).unwrap();
        assert_eq!(parsed.workloads[0], manifest.workloads[0]);
        let printed_job = &toml::from_str::<toml::Value>(&text).unwrap()["workloads"][1];
        for field in [
            "restart",
            "max_restarts",
            "restart_delay_secs",
            "restart_backoff",
        ] {
            assert!(printed_job.get(field).is_none(), "{field}");
        }
        assert_eq!(parsed.workloads[1].success_exit_codes, [3]);
        assert_eq!(parsed.workloads[1].restart, Restart::OnFailure);
    }
}
