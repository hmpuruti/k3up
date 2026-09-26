use crate::{form::FormMessage, health::Column, theme::Mode};
use k3up::{
    metrics::{Metrics, Usage},
    model::{Event, Status},
    protocol::Response,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Page {
    Workloads,
    Schedules,
    Activity,
    Health,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Filter {
    #[default]
    All,
    Running,
    Attention,
    Stopped,
}

/// `events` holds only those newer than the ones already shown.
#[derive(Clone, Debug)]
pub struct Snapshot {
    pub statuses: Vec<Status>,
    pub events: Vec<Event>,
}

/// `metrics` is `None` until the agent has taken its first sample.
#[derive(Clone, Debug)]
pub struct Sample {
    pub metrics: Option<Box<Metrics>>,
    pub desktop: Usage,
}

#[derive(Clone, Debug)]
pub struct LogChunk {
    pub text: String,
    pub offset: u64,
}

/// `registered` is whether the login item exists afterwards; `started` is set only when this
/// run also had to start the agent.
#[derive(Clone, Debug)]
pub struct AgentSetup {
    pub registered: Result<bool, String>,
    pub started: Option<Result<(), String>>,
}

/// `Offline` is a transport failure (no agent). `Request` is an answer from a live agent
/// that refused the command, which must not be reported as a lost connection.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Failure {
    Offline(String),
    Request(String),
}

#[derive(Clone, Debug)]
pub enum WatchEvent {
    Changed(u64),
    Failed(Failure),
}

#[derive(Clone, Debug)]
pub enum Message {
    Tick,
    Watch {
        session: u64,
        event: WatchEvent,
    },
    Loaded {
        session: u64,
        result: Result<Snapshot, Failure>,
    },
    LogsLoaded {
        session: u64,
        generation: u64,
        result: Result<LogChunk, Failure>,
    },
    ActionDone {
        session: u64,
        result: Result<Box<Response>, String>,
    },
    MetricsLoaded {
        session: u64,
        result: Result<Sample, Failure>,
    },
    SortHealth(Column),
    WindowChanged(iced::window::Id, Option<iced::Size>),
    Scale(f32),
    Focus(bool),
    ToggleConnection,
    DataDirectory(String),
    Connect,
    AgentSetup(AgentSetup),
    LoginItem(bool),
    LoginItemSet(Result<bool, String>),
    Navigate(Page),
    Select(String),
    Deselect,
    Filter(Filter),
    Search(String),
    ActivitySearch(String),
    Mode(Mode),
    New,
    Edit,
    CancelForm,
    Form(FormMessage),
    Save,
    Start,
    Stop,
    Restart,
    ToggleFolder(String),
    StartFolder(String),
    StopFolder(String),
    ConfirmStopFolder,
    CancelStopFolder,
    Remove,
    ConfirmRemove,
    CancelRemove,
    ClearOutput,
    Dismiss,
}
