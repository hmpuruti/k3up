use crate::{
    activity, charts, connection, detail, feed,
    form::{self, Form},
    format, health, list,
    logs::LogBuffer,
    message::{AgentSetup, Failure, Filter, Message, Page, Snapshot, WatchEvent},
    schedules,
    sidebar::{self, Link},
    theme::{self, Mode, Tone},
    widgets::{self, glyph},
};
use chrono::{DateTime, TimeDelta, Utc};
use iced::{
    Alignment, Element,
    Length::Fill,
    Size, Subscription, Task, Theme,
    widget::{Space, column, container, row, text},
    window,
};
use k3up::{
    autostart::LoginAgent,
    client::Client,
    metrics::{Metrics, Point, ProcessProbe, Usage, WorkloadUsage},
    model::{Event, State, Status, Workload},
    protocol::Command,
};
use std::{
    path::PathBuf,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

const SUCCESS_NOTICE: TimeDelta = TimeDelta::seconds(5);
const START_GRACE: Duration = Duration::from_secs(20);
const MAX_EVENTS: usize = 200;
const HISTORY: TimeDelta = TimeDelta::hours(1);
/// A health warning older than this is stale once metrics stop refreshing.
const WARNING_FRESHNESS: TimeDelta = TimeDelta::minutes(1);
const ALARM_COALESCE: TimeDelta = TimeDelta::seconds(1);
/// A window nobody is looking at gets its relative times refreshed at most this often.
const UNFOCUSED_TICK: TimeDelta = TimeDelta::minutes(1);

struct Notice {
    tone: Tone,
    text: String,
    expires: Option<DateTime<Utc>>,
}

impl Notice {
    fn success(text: String, now: DateTime<Utc>) -> Self {
        Self {
            tone: Tone::Success,
            text,
            expires: Some(now + SUCCESS_NOTICE),
        }
    }

    fn warning(text: String) -> Self {
        Self {
            tone: Tone::Warning,
            text,
            expires: None,
        }
    }

    fn error(text: String) -> Self {
        Self {
            tone: Tone::Danger,
            text,
            expires: None,
        }
    }
}

pub struct App {
    client: Client,
    data_directory: String,
    session: u64,
    show_connection: bool,
    ever_connected: bool,
    offline: Option<String>,
    statuses: Vec<Status>,
    events: Vec<Event>,
    generation: u64,
    stale: bool,
    logs: LogBuffer,
    log_generation: u64,
    selected: Option<String>,
    page: Page,
    filter: Filter,
    search: String,
    activity_search: String,
    mode: Mode,
    loading: bool,
    busy: bool,
    notice: Option<Notice>,
    dismissed_warning: Option<String>,
    form: Option<Form>,
    confirming_remove: bool,
    now: DateTime<Utc>,
    login: Option<LoginAgent>,
    login_enabled: Option<bool>,
    login_checked: bool,
    start_attempted: bool,
    starting_since: Option<Instant>,
    start_error: Option<String>,
    metrics: Option<Box<Metrics>>,
    desktop: Usage,
    probe: Arc<Mutex<ProcessProbe>>,
    charts: charts::Cache,
    health_sort: health::Sort,
    scale: f32,
    window: Size,
    focused: bool,
}

fn window_event(
    event: iced::Event,
    _status: iced::event::Status,
    id: window::Id,
) -> Option<Message> {
    match event {
        iced::Event::Window(window::Event::Opened { size, .. } | window::Event::Resized(size)) => {
            Some(Message::WindowChanged(id, Some(size)))
        }
        iced::Event::Window(window::Event::Moved(_)) => Some(Message::WindowChanged(id, None)),
        iced::Event::Window(window::Event::Focused) => Some(Message::Focus(true)),
        iced::Event::Window(window::Event::Unfocused) => Some(Message::Focus(false)),
        _ => None,
    }
}

impl App {
    pub fn new(data: PathBuf, login: Option<LoginAgent>) -> (Self, Task<Message>) {
        let app = Self {
            data_directory: data.to_string_lossy().into_owned(),
            client: Client::new(data),
            session: 0,
            show_connection: false,
            ever_connected: false,
            offline: None,
            statuses: vec![],
            events: vec![],
            generation: 0,
            stale: false,
            logs: LogBuffer::default(),
            log_generation: 0,
            selected: None,
            page: Page::Workloads,
            filter: Filter::All,
            search: String::new(),
            activity_search: String::new(),
            mode: Mode::Light,
            loading: false,
            busy: false,
            notice: None,
            dismissed_warning: None,
            form: None,
            confirming_remove: false,
            now: Utc::now(),
            login,
            login_enabled: None,
            login_checked: false,
            start_attempted: false,
            starting_since: None,
            start_error: None,
            metrics: None,
            desktop: Usage::default(),
            probe: Arc::new(Mutex::new(ProcessProbe::default())),
            charts: charts::Cache::default(),
            health_sort: health::Sort::default(),
            scale: 1.0,
            window: Size::new(1240.0, 860.0),
            focused: true,
        };
        (app, Task::none())
    }

    pub fn theme(&self) -> Theme {
        theme::iced_theme(self.mode)
    }

    /// The watch loop is the only standing subscription; everything else runs solely while
    /// the window is focused and something on screen needs it.
    pub fn subscription(&self) -> Subscription<Message> {
        let mut subscriptions = vec![
            iced::event::listen_with(window_event),
            feed::watch(self.client.clone(), self.session),
        ];
        if let Some(name) = self.followed_logs() {
            subscriptions.push(feed::logs(
                self.client.clone(),
                self.session,
                self.log_generation,
                name.to_owned(),
                self.logs.offset(),
            ));
        }
        if self.polls_metrics() {
            subscriptions.push(feed::metrics(
                self.client.clone(),
                self.probe.clone(),
                self.session,
                self.newest_point(),
            ));
        }
        if let Some(at) = self.next_alarm() {
            subscriptions.push(feed::alarm(at));
        }
        Subscription::batch(subscriptions)
    }

    fn connected(&self) -> bool {
        self.ever_connected && self.offline.is_none()
    }

    fn starting(&self) -> bool {
        !self.connected()
            && self
                .starting_since
                .is_some_and(|since| since.elapsed() < START_GRACE)
    }

    /// The login agent applies only while connected to the directory it manages.
    fn managed(&self) -> Option<&LoginAgent> {
        self.login
            .as_ref()
            .filter(|login| login.data_dir() == self.client.data_dir)
    }

    fn link(&self) -> Link {
        if self.connected() {
            Link::Connected
        } else if self.starting() {
            Link::Starting
        } else {
            Link::Offline
        }
    }

    /// Refreshes the login item unless the user turned it off, and starts the agent if asked.
    fn setup_agent(&mut self, start: bool) -> Task<Message> {
        let Some(login) = self.managed().cloned() else {
            return Task::none();
        };
        if start {
            self.start_attempted = true;
            self.starting_since = Some(Instant::now());
            self.start_error = None;
        }
        Task::perform(
            async move {
                tokio::task::spawn_blocking(move || {
                    let registered = if login.opted_out() {
                        Ok(false)
                    } else {
                        login
                            .register()
                            .map(|()| true)
                            .map_err(|error| format!("{error:#}"))
                    };
                    let started =
                        start.then(|| login.start().map_err(|error| format!("{error:#}")));
                    AgentSetup {
                        registered,
                        started,
                    }
                })
                .await
                .unwrap_or_else(|error| AgentSetup {
                    registered: Err(error.to_string()),
                    started: start.then(|| Err(error.to_string())),
                })
            },
            Message::AgentSetup,
        )
    }

    fn set_login_item(&mut self, enabled: bool) -> Task<Message> {
        let Some(login) = self.managed().cloned() else {
            return Task::none();
        };
        Task::perform(
            async move {
                tokio::task::spawn_blocking(move || {
                    if enabled {
                        login.register()
                    } else {
                        login.unregister()
                    }
                    .map(|()| enabled)
                    .map_err(|error| format!("{error:#}"))
                })
                .await
                .unwrap_or_else(|error| Err(error.to_string()))
            },
            Message::LoginItemSet,
        )
    }

    fn ready(&self) -> bool {
        self.connected() && !self.busy
    }

    fn selected_status(&self) -> Option<&Status> {
        self.statuses
            .iter()
            .find(|status| Some(&status.workload.name) == self.selected.as_ref())
    }

    fn detail_visible(&self) -> bool {
        self.form.is_none() && self.page == Page::Workloads && self.selected.is_some()
    }

    fn followed_logs(&self) -> Option<&str> {
        (self.focused && self.offline.is_none() && self.detail_visible())
            .then_some(self.selected.as_deref())
            .flatten()
    }

    /// Metrics are only worth the agent's faster sampling while something on screen shows them.
    fn polls_metrics(&self) -> bool {
        self.focused
            && self.offline.is_none()
            && self.form.is_none()
            && match self.page {
                Page::Health => true,
                Page::Workloads => self.selected.is_some(),
                Page::Schedules | Page::Activity => false,
            }
    }

    fn newest_point(&self) -> Option<DateTime<Utc>> {
        self.metrics
            .as_ref()?
            .history
            .iter()
            .map(|point| point.at)
            .max()
    }

    fn selected_usage(&self) -> Option<&WorkloadUsage> {
        let name = self.selected.as_ref()?;
        self.metrics
            .as_ref()?
            .workloads
            .iter()
            .find(|usage| &usage.name == name)
    }

    /// Times on screen that read relative to now, so the clock only ticks when a label would
    /// actually change.
    fn time_anchors(&self) -> Vec<DateTime<Utc>> {
        let mut anchors = vec![];
        if self.form.is_some() {
            return anchors;
        }
        match self.page {
            Page::Workloads => {
                for status in &self.statuses {
                    if !list::matches(status, self.filter, &self.search) {
                        continue;
                    }
                    match status.state {
                        State::Running | State::Starting => anchors.extend(status.started_at),
                        State::Backoff | State::Failed => {}
                        _ => anchors.extend(status.next_run),
                    }
                }
                if let Some(status) = self.selected_status() {
                    anchors.extend(status.started_at);
                    anchors.extend(status.next_run);
                }
            }
            Page::Schedules => anchors.extend(
                self.statuses
                    .iter()
                    .filter(|status| status.workload.schedule.is_some())
                    .filter_map(|status| status.next_run),
            ),
            Page::Activity => {
                let needle = self.activity_search.trim().to_lowercase();
                anchors.extend(
                    self.events
                        .iter()
                        .filter(|event| activity::matches(event, &needle))
                        .map(|event| event.at),
                );
            }
            Page::Health => {}
        }
        anchors
    }

    fn next_alarm(&self) -> Option<DateTime<Utc>> {
        let now = self.now;
        let mut deadlines = vec![];
        deadlines.extend(self.notice.as_ref().and_then(|notice| notice.expires));
        if let Some(since) = self.starting_since
            && self.starting()
        {
            let left = START_GRACE.saturating_sub(since.elapsed());
            deadlines.push(now + TimeDelta::from_std(left).unwrap_or_default());
        }
        if let Some(metrics) = self.metrics.as_deref()
            && health::warnings(metrics) > 0
        {
            deadlines.push(metrics.at + WARNING_FRESHNESS);
        }
        let soonest = if self.focused {
            now
        } else {
            now + UNFOCUSED_TICK
        };
        deadlines.extend(
            self.time_anchors()
                .into_iter()
                .map(|anchor| format::next_change(anchor, now).max(soonest)),
        );
        // Labels due within the same second share one tick.
        let first = deadlines.iter().copied().filter(|at| *at > now).min()?;
        deadlines
            .into_iter()
            .filter(|at| *at > now && *at <= first + ALARM_COALESCE)
            .max()
    }

    fn refresh(&mut self) -> Task<Message> {
        if self.loading || self.busy {
            self.stale = true;
            return Task::none();
        }
        self.stale = false;
        self.loading = true;
        let client = self.client.clone();
        let session = self.session;
        let after = self.events.iter().map(|event| event.id).max();
        Task::perform(
            async move {
                tokio::task::spawn_blocking(move || {
                    let statuses = feed::request(&client, Command::List)?.workloads;
                    let events =
                        feed::request(&client, Command::Events { name: None, after })?.events;
                    Ok(Snapshot { statuses, events })
                })
                .await
                .unwrap_or_else(|error| Err(Failure::Offline(error.to_string())))
            },
            move |result| Message::Loaded { session, result },
        )
    }

    fn action(&mut self, command: Command) -> Task<Message> {
        if !self.ready() {
            return Task::none();
        }
        self.busy = true;
        self.notice = None;
        let client = self.client.clone();
        let session = self.session;
        Task::perform(
            async move {
                tokio::task::spawn_blocking(move || {
                    client.send(command).map_err(|error| format!("{error:#}"))
                })
                .await
                .unwrap_or_else(|error| Err(error.to_string()))
            },
            move |result| Message::ActionDone { session, result },
        )
    }

    fn reset_logs(&mut self) {
        self.logs.clear();
        self.log_generation += 1;
    }

    fn select(&mut self, name: String) {
        if self.selected.as_ref() == Some(&name) {
            self.page = Page::Workloads;
            self.form = None;
            return;
        }
        self.selected = Some(name);
        self.reset_logs();
        self.confirming_remove = false;
        self.page = Page::Workloads;
        self.form = None;
    }

    fn expire_notice(&mut self) {
        if self
            .notice
            .as_ref()
            .and_then(|notice| notice.expires)
            .is_some_and(|at| at <= self.now)
        {
            self.notice = None;
        }
    }

    fn went_offline(&mut self, reason: String) -> Task<Message> {
        self.offline = Some(reason);
        self.metrics = None;
        self.charts.clear();
        if !self.start_attempted {
            self.login_checked = true;
            return self.setup_agent(true);
        }
        Task::none()
    }

    fn merge_snapshot(&mut self, snapshot: Snapshot) {
        self.statuses = snapshot.statuses;
        let mut events = snapshot.events;
        events.append(&mut self.events);
        events.truncate(MAX_EVENTS);
        self.events = events;
        let vanished = self
            .selected
            .as_ref()
            .is_some_and(|name| !self.statuses.iter().any(|s| &s.workload.name == name));
        if vanished {
            self.selected = None;
            self.reset_logs();
            self.confirming_remove = false;
        }
    }

    fn merge_metrics(&mut self, mut fresh: Box<Metrics>) {
        if let Some(previous) = self.metrics.take() {
            let cutoff = fresh.at - HISTORY;
            let mut history: Vec<Point> = previous
                .history
                .into_iter()
                .filter(|point| point.at > cutoff)
                .collect();
            history.append(&mut fresh.history);
            fresh.history = history;
        }
        self.metrics = Some(fresh);
        self.charts.clear();
    }

    pub fn update(&mut self, message: Message) -> Task<Message> {
        self.now = Utc::now();
        match message {
            Message::Tick => self.expire_notice(),
            Message::Watch { session, event } => {
                if session != self.session {
                    return Task::none();
                }
                match event {
                    WatchEvent::Changed(generation) => {
                        self.generation = generation;
                        return self.refresh();
                    }
                    WatchEvent::Failed(Failure::Offline(reason)) => {
                        return self.went_offline(reason);
                    }
                    WatchEvent::Failed(Failure::Request(reason)) => self.warn(reason),
                }
            }
            Message::Loaded { session, result } => {
                if session != self.session {
                    return Task::none();
                }
                self.loading = false;
                match result {
                    Ok(snapshot) => {
                        self.dismissed_warning = None;
                        self.offline = None;
                        self.ever_connected = true;
                        self.start_attempted = false;
                        self.starting_since = None;
                        self.start_error = None;
                        self.merge_snapshot(snapshot);
                        let mut tasks = vec![];
                        if !self.login_checked && self.managed().is_some() {
                            self.login_checked = true;
                            tasks.push(self.setup_agent(false));
                        }
                        if self.stale {
                            tasks.push(self.refresh());
                        }
                        return Task::batch(tasks);
                    }
                    Err(Failure::Offline(reason)) => return self.went_offline(reason),
                    Err(Failure::Request(reason)) => self.warn(reason),
                }
            }
            Message::MetricsLoaded { session, result } => {
                if session != self.session {
                    return Task::none();
                }
                match result {
                    Ok(sample) => {
                        self.desktop = sample.desktop;
                        match sample.metrics {
                            Some(metrics) => self.merge_metrics(metrics),
                            None => self.metrics = None,
                        }
                    }
                    Err(Failure::Offline(_)) => return self.refresh(),
                    Err(Failure::Request(reason)) => self.warn(reason),
                }
            }
            Message::SortHealth(column) => self.health_sort = self.health_sort.toggle(column),
            Message::WindowChanged(id, size) => {
                if let Some(size) = size {
                    self.window = size;
                }
                let current = self.scale;
                return window::get_scale_factor(id).then(move |scale| {
                    if scale == current {
                        Task::none()
                    } else {
                        Task::done(Message::Scale(scale))
                    }
                });
            }
            Message::Scale(scale) => self.scale = scale,
            Message::Focus(focused) => self.focused = focused,
            Message::AgentSetup(setup) => {
                match setup.registered {
                    Ok(registered) => self.login_enabled = Some(registered),
                    Err(error) => {
                        self.login_enabled = Some(false);
                        self.warn(format!("Start at login unavailable: {error}"));
                    }
                }
                if let Some(Err(error)) = setup.started {
                    self.starting_since = None;
                    self.start_error = Some(error);
                }
            }
            Message::LoginItem(enabled) => return self.set_login_item(enabled),
            Message::LoginItemSet(result) => match result {
                Ok(enabled) => self.login_enabled = Some(enabled),
                Err(error) => self.notice = Some(Notice::error(error)),
            },
            Message::LogsLoaded {
                session,
                generation,
                result,
            } => {
                if session != self.session || generation != self.log_generation {
                    return Task::none();
                }
                match result {
                    Ok(chunk) => self.logs.append(&chunk.text, chunk.offset),
                    Err(Failure::Offline(_)) => return self.refresh(),
                    Err(Failure::Request(reason)) => self.warn(reason),
                }
            }
            Message::ActionDone { session, result } => {
                if session != self.session {
                    return Task::none();
                }
                self.busy = false;
                match result {
                    Ok(response) if response.ok => {
                        self.form = None;
                        self.confirming_remove = false;
                        self.notice = Some(Notice::success(response.message, self.now));
                    }
                    Ok(response) => self.notice = Some(Notice::error(response.message)),
                    Err(reason) => self.offline = Some(reason),
                }
                return self.refresh();
            }
            Message::ToggleConnection => self.show_connection = !self.show_connection,
            Message::DataDirectory(value) => self.data_directory = value,
            Message::Connect => {
                self.session += 1;
                self.start_attempted = false;
                self.starting_since = None;
                self.start_error = None;
                self.client = Client::new(PathBuf::from(self.data_directory.trim()));
                self.offline = None;
                self.ever_connected = false;
                self.show_connection = false;
                self.selected = None;
                self.statuses.clear();
                self.events.clear();
                self.generation = 0;
                self.stale = false;
                self.reset_logs();
                self.loading = false;
                self.busy = false;
                self.notice = None;
                self.form = None;
                self.confirming_remove = false;
                self.metrics = None;
                self.charts.clear();
            }
            Message::Navigate(page) => {
                self.page = page;
                self.form = None;
                self.confirming_remove = false;
            }
            Message::Select(name) => self.select(name),
            Message::Deselect => {
                self.selected = None;
                self.reset_logs();
                self.confirming_remove = false;
            }
            Message::Filter(filter) => self.filter = filter,
            Message::Search(search) => self.search = search,
            Message::ActivitySearch(search) => self.activity_search = search,
            Message::Mode(mode) => {
                self.mode = mode;
                self.charts.clear();
            }
            Message::New => {
                self.form = Some(Form::new(Workload::default(), false));
                self.notice = None;
                self.confirming_remove = false;
            }
            Message::Edit => {
                if let Some(status) = self.selected_status() {
                    self.form = Some(Form::new(status.workload.clone(), true));
                    self.notice = None;
                    self.confirming_remove = false;
                }
            }
            Message::CancelForm => self.form = None,
            Message::Form(message) => {
                if let Some(form) = &mut self.form
                    && let Some(error) = form.update(message)
                {
                    self.notice = Some(Notice::error(error));
                }
            }
            Message::Save => return self.save(),
            Message::Start => {
                if let Some(name) = self.selected.clone() {
                    return self.action(Command::Start { name });
                }
            }
            Message::Stop => {
                if let Some(name) = self.selected.clone() {
                    return self.action(Command::Stop { name });
                }
            }
            Message::Restart => {
                if let Some(name) = self.selected.clone() {
                    return self.action(Command::Restart { name });
                }
            }
            Message::Remove => self.confirming_remove = true,
            Message::CancelRemove => self.confirming_remove = false,
            Message::ConfirmRemove => {
                if let Some(name) = self.selected.clone() {
                    return self.action(Command::Remove { name });
                }
            }
            Message::ClearOutput => self.logs.clear_view(),
            Message::Dismiss => {
                if let Some(notice) = self.notice.take() {
                    if notice.tone == Tone::Warning {
                        self.dismissed_warning = Some(notice.text);
                    }
                }
            }
        }
        Task::none()
    }

    /// A failure that repeats on every refresh must not reset the notice or undo Dismiss.
    fn warn(&mut self, reason: String) {
        let shown = self
            .notice
            .as_ref()
            .is_some_and(|notice| notice.text == reason);
        if !shown && self.dismissed_warning.as_ref() != Some(&reason) {
            self.notice = Some(Notice::warning(reason));
        }
    }

    fn save(&mut self) -> Task<Message> {
        let Some(form) = &self.form else {
            return Task::none();
        };
        let outcome = form.workload().and_then(|spec| {
            spec.validate().map_err(|error| error.to_string())?;
            if form.editing && spec.name != form.base.name {
                return Err("Renaming is not supported; create a new workload".into());
            }
            Ok(spec)
        });
        let create_only = !form.editing;
        match outcome {
            Ok(workload) => self.action(Command::Put {
                workload: Box::new(workload),
                create_only,
            }),
            Err(error) => {
                self.notice = Some(Notice::error(error));
                Task::none()
            }
        }
    }

    pub fn view(&self) -> Element<'_, Message> {
        let counts = sidebar::Counts {
            workloads: self.statuses.len(),
            schedules: self
                .statuses
                .iter()
                .filter(|s| s.workload.schedule.is_some())
                .count(),
            attention: self
                .statuses
                .iter()
                .filter(|s| format::needs_attention(s.state))
                .count(),
            // Metrics stop updating off the Health and detail views; an old warning is not news.
            health: self
                .metrics
                .as_deref()
                .filter(|metrics| self.now - metrics.at < WARNING_FRESHNESS)
                .map_or(0, health::warnings),
        };
        let mut content = column![].spacing(theme::SPACE_LG);
        if let Some(notice) = &self.notice {
            content = content.push(notice_view(notice));
        }
        if let Some(reason) = &self.offline
            && self.ever_connected
            && !self.show_connection
            && !self.starting()
        {
            content = content.push(offline_banner(
                self.start_error.as_deref().unwrap_or(reason),
            ));
        }
        if self.show_connection || (!self.ever_connected && !self.starting()) {
            content = content.push(connection::view(connection::Context {
                data_directory: &self.data_directory,
                link: self.link(),
                problem: self.start_error.as_deref().or(self.offline.as_deref()),
                can_connect: !self.busy,
                dismissible: self.ever_connected,
                login_enabled: self.managed().and(self.login_enabled),
            }));
        }
        let body: Element<_> = if let Some(form) = &self.form {
            form::view::view(form::view::Context {
                form,
                names: self
                    .statuses
                    .iter()
                    .map(|s| s.workload.name.clone())
                    .collect(),
                can_save: self.ready(),
            })
        } else {
            match self.page {
                Page::Workloads => self.workloads_page(),
                Page::Schedules => schedules::view(&self.statuses, self.now),
                Page::Activity => activity::view(&self.events, &self.activity_search, self.now),
                Page::Health => health::view(health::Context {
                    metrics: self.metrics.as_deref(),
                    desktop: self.desktop,
                    statuses: &self.statuses,
                    charts: &self.charts,
                    sort: self.health_sort,
                    scale: self.scale,
                    width: self.window.width,
                }),
            }
        };
        content = content.push(body);
        row![
            sidebar::view(
                self.page,
                counts,
                self.link(),
                &self.data_directory,
                self.mode,
            ),
            container(content.height(Fill))
                .padding(theme::SPACE_XL)
                .width(Fill)
                .height(Fill)
                .style(theme::app),
        ]
        .height(Fill)
        .into()
    }

    fn workloads_page(&self) -> Element<'_, Message> {
        let running = self
            .statuses
            .iter()
            .filter(|s| matches!(s.state, State::Running))
            .count();
        let attention = self
            .statuses
            .iter()
            .filter(|s| format::needs_attention(s.state))
            .count();
        let mut summary = row![widgets::pill(
            format!("{running} running"),
            if running > 0 {
                Tone::Success
            } else {
                Tone::Neutral
            }
        )]
        .spacing(theme::SPACE_XS);
        if attention > 0 {
            summary = summary.push(widgets::pill(
                format!("{attention} attention"),
                Tone::Warning,
            ));
        }
        let header = widgets::page_header(
            "Workloads",
            summary.into(),
            Some(widgets::action(
                glyph::PLUS,
                "New",
                self.ready().then_some(Message::New),
                theme::primary,
            )),
        );
        let detail: Element<_> = match self.selected_status() {
            Some(status) => detail::view(detail::Context {
                status,
                usage: self.selected_usage(),
                charts: &self.charts,
                logs: &self.logs,
                now: self.now,
                connected: self.connected(),
                busy: self.busy,
                confirming_remove: self.confirming_remove,
                scale: self.scale,
            }),
            None => list::placeholder(),
        };
        let body = row![
            list::view(
                &self.statuses,
                self.filter,
                &self.search,
                self.selected.as_deref(),
                self.now,
            ),
            detail,
        ]
        .spacing(theme::SPACE_LG)
        .height(Fill);
        column![header, body]
            .spacing(theme::SPACE_XL)
            .height(Fill)
            .into()
    }
}

fn notice_view(notice: &Notice) -> Element<'_, Message> {
    container(
        row![
            widgets::glyph(glyph::DOT, 8).style(theme::text_toned(notice.tone)),
            text(&notice.text).size(theme::TEXT_BODY).width(Fill),
            widgets::icon_button(glyph::CLOSE, "Dismiss", Some(Message::Dismiss)),
        ]
        .spacing(theme::SPACE_MD)
        .align_y(Alignment::Center),
    )
    .padding([theme::SPACE_SM, theme::SPACE_MD])
    .width(Fill)
    .style(theme::banner(notice.tone))
    .into()
}

fn offline_banner(reason: &str) -> Element<'_, Message> {
    container(
        row![
            widgets::glyph(glyph::RING, 9).style(theme::text_toned(Tone::Warning)),
            widgets::toned("Agent offline", theme::TEXT_BODY, Tone::Warning),
            widgets::muted(reason, theme::TEXT_META).width(Fill),
            Space::with_width(theme::SPACE_SM),
            widgets::ghost("Connection", Some(Message::ToggleConnection)),
        ]
        .spacing(theme::SPACE_MD)
        .align_y(Alignment::Center),
    )
    .padding([theme::SPACE_SM, theme::SPACE_MD])
    .width(Fill)
    .style(theme::banner(Tone::Warning))
    .into()
}
