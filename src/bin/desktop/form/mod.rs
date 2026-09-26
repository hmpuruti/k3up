pub mod view;

use iced::widget::text_editor;
use k3up::model::{
    Kind, Missed, Restart, RestartBackoff, RestartLimit, Schedule, ScheduleAction, Workload,
};
use std::collections::BTreeMap;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Field {
    Name,
    Description,
    Executable,
    Directory,
    Readiness,
    MaxRestarts,
    RestartDelay,
    SuccessCodes,
    StopTimeout,
    StartupTimeout,
    RunTimeout,
    Every,
    Cron,
    Timezone,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Cadence {
    #[default]
    Manual,
    Every,
    Cron,
}

#[derive(Clone, Debug)]
pub enum FormMessage {
    Text(Field, String),
    Kind(Kind),
    Boot(bool),
    Restart(Restart),
    Unlimited(bool),
    Backoff(RestartBackoff),
    ArgumentAdd,
    ArgumentChange(usize, String),
    ArgumentRemove(usize),
    VariableAdd,
    VariableKey(usize, String),
    VariableValue(usize, String),
    VariableRemove(usize),
    DependencyAdd(String),
    DependencyRemove(usize),
    Cadence(Cadence),
    Action(ScheduleAction),
    Missed(Missed),
    Raw(bool),
    Editor(text_editor::Action),
}

pub struct Form {
    pub base: Workload,
    pub editing: bool,
    pub raw: bool,
    pub editor: text_editor::Content,
    pub name: String,
    pub description: String,
    pub executable: String,
    pub directory: String,
    pub arguments: Vec<String>,
    pub variables: Vec<(String, String)>,
    pub dependencies: Vec<String>,
    pub kind: Kind,
    pub boot: bool,
    pub restart: Restart,
    pub max_restarts: String,
    pub unlimited: bool,
    pub restart_delay: String,
    pub backoff: RestartBackoff,
    pub success_codes: String,
    pub stop_timeout: String,
    pub startup_timeout: String,
    pub run_timeout: String,
    pub readiness: String,
    pub cadence: Cadence,
    pub every: String,
    pub cron: String,
    pub timezone: String,
    pub action: ScheduleAction,
    pub missed: Missed,
}

impl Form {
    pub fn new(spec: Workload, editing: bool) -> Self {
        let schedule = spec.schedule.clone();
        Self {
            editing,
            raw: false,
            editor: text_editor::Content::with_text(&spec.to_toml().unwrap_or_default()),
            name: spec.name.clone(),
            description: spec.description.clone(),
            executable: spec.executable.clone(),
            directory: spec.working_directory.clone(),
            arguments: spec.args.clone(),
            variables: spec
                .environment
                .iter()
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect(),
            dependencies: spec.depends_on.clone(),
            kind: spec.kind,
            boot: spec.start_at_boot,
            restart: spec.restart,
            max_restarts: match spec.max_restarts {
                RestartLimit::Count(count) => count.to_string(),
                RestartLimit::Unlimited => RestartLimit::default().to_string(),
            },
            unlimited: spec.max_restarts == RestartLimit::Unlimited,
            restart_delay: spec.restart_delay_secs.to_string(),
            backoff: spec.restart_backoff,
            success_codes: spec
                .success_exit_codes
                .iter()
                .map(i32::to_string)
                .collect::<Vec<_>>()
                .join(", "),
            stop_timeout: spec.stop_timeout_secs.to_string(),
            startup_timeout: spec.startup_timeout_secs.to_string(),
            run_timeout: spec
                .run_timeout_secs
                .map(|s| s.to_string())
                .unwrap_or_default(),
            readiness: spec.readiness_tcp.clone().unwrap_or_default(),
            cadence: match &schedule {
                Some(s) if s.every_secs.is_some() => Cadence::Every,
                Some(s) if s.cron.is_some() => Cadence::Cron,
                _ => Cadence::Manual,
            },
            every: schedule
                .as_ref()
                .and_then(|s| s.every_secs)
                .map(|s| s.to_string())
                .unwrap_or_default(),
            cron: schedule
                .as_ref()
                .and_then(|s| s.cron.clone())
                .unwrap_or_default(),
            timezone: schedule
                .as_ref()
                .map(|s| s.timezone.clone())
                .unwrap_or_else(|| "UTC".into()),
            action: schedule.as_ref().map(|s| s.action).unwrap_or_default(),
            missed: schedule.as_ref().map(|s| s.missed).unwrap_or_default(),
            base: spec,
        }
    }

    pub fn is_job(&self) -> bool {
        self.kind == Kind::Job
    }

    /// Returns a notice when the message could not be applied.
    pub fn update(&mut self, message: FormMessage) -> Option<String> {
        match message {
            FormMessage::Text(field, value) => self.set_text(field, value),
            FormMessage::Kind(kind) => {
                self.kind = kind;
                if kind == Kind::Job {
                    self.readiness.clear();
                    self.action = ScheduleAction::Start;
                }
            }
            FormMessage::Boot(value) => self.boot = value,
            FormMessage::Restart(value) => self.restart = value,
            FormMessage::Unlimited(value) => self.unlimited = value,
            FormMessage::Backoff(value) => self.backoff = value,
            FormMessage::ArgumentAdd => self.arguments.push(String::new()),
            FormMessage::ArgumentChange(index, value) => {
                if let Some(slot) = self.arguments.get_mut(index) {
                    *slot = value;
                }
            }
            FormMessage::ArgumentRemove(index) => {
                if index < self.arguments.len() {
                    self.arguments.remove(index);
                }
            }
            FormMessage::VariableAdd => self.variables.push((String::new(), String::new())),
            FormMessage::VariableKey(index, value) => {
                if let Some(slot) = self.variables.get_mut(index) {
                    slot.0 = value;
                }
            }
            FormMessage::VariableValue(index, value) => {
                if let Some(slot) = self.variables.get_mut(index) {
                    slot.1 = value;
                }
            }
            FormMessage::VariableRemove(index) => {
                if index < self.variables.len() {
                    self.variables.remove(index);
                }
            }
            FormMessage::DependencyAdd(name) => {
                if !self.dependencies.contains(&name) {
                    self.dependencies.push(name);
                }
            }
            FormMessage::DependencyRemove(index) => {
                if index < self.dependencies.len() {
                    self.dependencies.remove(index);
                }
            }
            FormMessage::Cadence(cadence) => self.cadence = cadence,
            FormMessage::Action(action) => self.action = action,
            FormMessage::Missed(missed) => self.missed = missed,
            FormMessage::Raw(raw) => {
                if raw == self.raw {
                    return None;
                }
                match self.workload() {
                    Ok(spec) => {
                        // Keep the original definition as the base, so a name changed in TOML
                        // still reads as a rename and Save refuses it.
                        let base = self.base.clone();
                        let editing = self.editing;
                        *self = Self::new(spec, editing);
                        self.base = base;
                        self.raw = raw;
                    }
                    Err(error) => return Some(error),
                }
            }
            FormMessage::Editor(action) => self.editor.perform(action),
        }
        None
    }

    fn set_text(&mut self, field: Field, value: String) {
        let numeric = matches!(
            field,
            Field::MaxRestarts
                | Field::RestartDelay
                | Field::StopTimeout
                | Field::StartupTimeout
                | Field::RunTimeout
                | Field::Every
        );
        if numeric && !value.chars().all(|c| c.is_ascii_digit()) {
            return;
        }
        if field == Field::SuccessCodes
            && !value
                .chars()
                .all(|c| c.is_ascii_digit() || matches!(c, ',' | ' ' | '-'))
        {
            return;
        }
        let slot = match field {
            Field::Name => &mut self.name,
            Field::Description => &mut self.description,
            Field::Executable => &mut self.executable,
            Field::Directory => &mut self.directory,
            Field::Readiness => &mut self.readiness,
            Field::MaxRestarts => &mut self.max_restarts,
            Field::RestartDelay => &mut self.restart_delay,
            Field::SuccessCodes => &mut self.success_codes,
            Field::StopTimeout => &mut self.stop_timeout,
            Field::StartupTimeout => &mut self.startup_timeout,
            Field::RunTimeout => &mut self.run_timeout,
            Field::Every => &mut self.every,
            Field::Cron => &mut self.cron,
            Field::Timezone => &mut self.timezone,
        };
        *slot = value;
    }

    pub fn workload(&self) -> Result<Workload, String> {
        if self.raw {
            return toml::from_str(&self.editor.text()).map_err(|e| format!("Invalid TOML: {e}"));
        }
        let mut spec = self.base.clone();
        spec.name = self.name.trim().into();
        spec.description = self.description.trim().into();
        spec.executable = self.executable.trim().into();
        spec.working_directory = self.directory.trim().into();
        spec.args = self.arguments.clone();
        spec.environment = self
            .variables
            .iter()
            .filter(|(key, _)| !key.trim().is_empty())
            .map(|(key, value)| (key.trim().to_owned(), value.clone()))
            .collect::<BTreeMap<_, _>>();
        spec.depends_on = self.dependencies.clone();
        spec.kind = self.kind;
        spec.start_at_boot = self.boot;
        spec.restart = self.restart;
        spec.max_restarts = if self.unlimited {
            RestartLimit::Unlimited
        } else {
            RestartLimit::Count(number(&self.max_restarts, "Max restarts")?)
        };
        spec.restart_delay_secs = number(&self.restart_delay, "Restart delay")?;
        spec.restart_backoff = self.backoff;
        spec.success_exit_codes = self
            .success_codes
            .split(|c: char| c == ',' || c.is_whitespace())
            .filter(|part| !part.is_empty())
            .map(|part| number(part, "Success exit codes"))
            .collect::<Result<_, _>>()?;
        spec.stop_timeout_secs = number(&self.stop_timeout, "Stop timeout")?;
        spec.startup_timeout_secs = number(&self.startup_timeout, "Startup timeout")?;
        spec.run_timeout_secs = optional_number(&self.run_timeout, "Run timeout")?;
        spec.readiness_tcp = if self.is_job() || self.readiness.trim().is_empty() {
            None
        } else {
            Some(self.readiness.trim().into())
        };
        spec.schedule = match self.cadence {
            Cadence::Manual => None,
            Cadence::Every => Some(Schedule {
                every_secs: Some(number(&self.every, "Interval")?),
                cron: None,
                timezone: self.timezone.trim().into(),
                action: self.effective_action(),
                missed: self.missed,
            }),
            Cadence::Cron => Some(Schedule {
                every_secs: None,
                cron: Some(self.cron.trim().into()).filter(|c: &String| !c.is_empty()),
                timezone: self.timezone.trim().into(),
                action: self.effective_action(),
                missed: self.missed,
            }),
        };
        Ok(spec)
    }

    fn effective_action(&self) -> ScheduleAction {
        if self.is_job() {
            ScheduleAction::Start
        } else {
            self.action
        }
    }
}

fn number<T: std::str::FromStr>(value: &str, label: &str) -> Result<T, String> {
    value
        .trim()
        .parse()
        .map_err(|_| format!("{label} must be a whole number"))
}

fn optional_number<T: std::str::FromStr>(value: &str, label: &str) -> Result<Option<T>, String> {
    if value.trim().is_empty() {
        Ok(None)
    } else {
        number(value, label).map(Some)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Paths must be absolute on the platform running the tests.
    fn filled() -> Form {
        let mut form = Form::new(Workload::default(), false);
        form.name = "worker".into();
        (form.executable, form.directory) = if cfg!(windows) {
            (
                r"C:\Windows\System32\timeout.exe".into(),
                r"C:\Windows\Temp".into(),
            )
        } else {
            ("/bin/sleep".into(), "/tmp".into())
        };
        form
    }

    #[test]
    fn jobs_drop_readiness_and_scheduled_restart() {
        let mut form = filled();
        form.readiness = "127.0.0.1:8080".into();
        form.action = ScheduleAction::Restart;
        form.cadence = Cadence::Every;
        form.every = "60".into();
        form.update(FormMessage::Kind(Kind::Job));
        let spec = form.workload().unwrap();
        assert_eq!(spec.readiness_tcp, None);
        spec.validate().unwrap();
        assert_eq!(spec.schedule.unwrap().action, ScheduleAction::Start);
    }

    #[test]
    fn numeric_fields_reject_non_digits() {
        let mut form = filled();
        form.update(FormMessage::Text(Field::MaxRestarts, "1a".into()));
        assert_eq!(form.max_restarts, "5");
        form.update(FormMessage::Text(Field::MaxRestarts, "12".into()));
        assert_eq!(form.max_restarts, "12");
        form.update(FormMessage::Text(Field::SuccessCodes, "3, 75".into()));
        assert_eq!(form.success_codes, "3, 75");
        form.update(FormMessage::Text(Field::SuccessCodes, "3; 75".into()));
        assert_eq!(form.success_codes, "3, 75");
    }

    #[test]
    fn recovery_and_success_codes_round_trip() {
        let mut original = filled().workload().unwrap();
        original.restart = Restart::Always;
        original.max_restarts = RestartLimit::Unlimited;
        original.restart_delay_secs = 5;
        original.restart_backoff = RestartBackoff::Fixed;
        original.success_exit_codes = vec![3, 75];
        let form = Form::new(original.clone(), true);
        assert!(form.unlimited);
        assert_eq!(form.success_codes, "3, 75");
        assert_eq!(form.workload().unwrap(), original);

        let mut form = Form::new(original.clone(), true);
        form.update(FormMessage::Raw(true));
        form.update(FormMessage::Raw(false));
        assert_eq!(form.workload().unwrap(), original);

        form.update(FormMessage::Unlimited(false));
        form.update(FormMessage::Text(Field::MaxRestarts, "9".into()));
        form.update(FormMessage::Backoff(RestartBackoff::Exponential));
        form.update(FormMessage::Text(Field::SuccessCodes, "".into()));
        let spec = form.workload().unwrap();
        assert_eq!(spec.max_restarts, RestartLimit::Count(9));
        assert_eq!(spec.restart_backoff, RestartBackoff::Exponential);
        assert!(spec.success_exit_codes.is_empty());
        form.update(FormMessage::Text(Field::SuccessCodes, "3,".into()));
        assert_eq!(form.workload().unwrap().success_exit_codes, [3]);
    }

    #[test]
    fn environment_rows_round_trip() {
        let mut form = filled();
        form.update(FormMessage::VariableAdd);
        form.update(FormMessage::VariableKey(0, "MODE".into()));
        form.update(FormMessage::VariableValue(0, "prod".into()));
        form.update(FormMessage::VariableAdd);
        form.update(FormMessage::ArgumentAdd);
        form.update(FormMessage::ArgumentChange(0, "--port".into()));
        let spec = form.workload().unwrap();
        assert_eq!(
            spec.environment.get("MODE").map(String::as_str),
            Some("prod")
        );
        assert_eq!(spec.environment.len(), 1);
        assert_eq!(spec.args, vec!["--port"]);
        form.update(FormMessage::Raw(true));
        assert!(form.raw);
        assert!(form.editor.text().contains("MODE"));
    }
}
