use crate::model::{Event, Manifest, Status, Workload};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

pub const VERSION: u32 = 1;
pub const MAX_FRAME: usize = 1024 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Request {
    pub version: u32,
    #[serde(flatten)]
    pub command: Command,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "command", content = "params", rename_all = "snake_case")]
pub enum Command {
    List,
    Get {
        name: String,
    },
    Apply {
        manifest: Manifest,
        dry_run: bool,
    },
    Put {
        workload: Box<Workload>,
        create_only: bool,
    },
    Remove {
        name: String,
    },
    Start {
        name: String,
    },
    Stop {
        name: String,
    },
    Restart {
        name: String,
    },
    /// Without `after`, returns the last `lines` lines. With `after`, returns output written
    /// since that byte offset; pass back the returned `offset` to follow a log.
    Logs {
        name: String,
        lines: usize,
        #[serde(default)]
        after: Option<u64>,
    },
    /// Machine health and resource use of the agent and each running workload. History is
    /// limited to points after `since`, so a client that keeps its own copy fetches only
    /// what is new.
    Metrics {
        #[serde(default)]
        since: Option<DateTime<Utc>>,
    },
    /// Newest first. With `after`, only events with a larger id.
    Events {
        name: Option<String>,
        #[serde(default)]
        after: Option<i64>,
    },
    Export,
    /// The agent's version, process, start time and data directory.
    Info,
    /// Stops every workload in reverse dependency order, then exits the agent.
    Shutdown,
    /// Waits until the agent's state generation exceeds `since`, or `timeout_ms` passes, and
    /// returns the current generation. Lets clients refresh on change instead of polling.
    Watch {
        since: u64,
        timeout_ms: u64,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AgentInfo {
    pub version: String,
    pub pid: u32,
    pub started_at: DateTime<Utc>,
    pub data_dir: String,
    pub executable: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Response {
    pub ok: bool,
    pub message: String,
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub workloads: Vec<Status>,
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub events: Vec<Event>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub manifest: Option<Manifest>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub offset: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub metrics: Option<Box<crate::metrics::Metrics>>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub generation: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub agent: Option<AgentInfo>,
}
impl Response {
    pub fn success(message: impl Into<String>) -> Self {
        Self {
            ok: true,
            message: message.into(),
            ..Default::default()
        }
    }
    pub fn error(error: impl std::fmt::Display) -> Self {
        Self {
            message: error.to_string(),
            ..Default::default()
        }
    }
}
