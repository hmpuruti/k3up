//! Decisions about a single workload's process, shared by the agent's engine and the Windows
//! service host, so each supervision rule is written once.
use crate::{
    model::{Kind, Restart, RestartLimit, State, Workload, retry_delay},
    platform::ManagedProcess,
};
use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use std::{
    fs::OpenOptions,
    io::{Read, Seek, SeekFrom, Write},
    path::Path,
};

pub const LOG_LIMIT: u64 = 5 * 1024 * 1024;
const LOG_WINDOW: u64 = 128 * 1024;
/// A service that has run this long is healthy again, so its retry count starts over.
pub const STABLE_SECS: i64 = 60;

/// How a process ended. `success` is separate from the code because a failed readiness check
/// is a failure whatever code it is reported with.
pub struct Ended {
    pub code: i32,
    pub reason: String,
    pub success: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Exit {
    Retry {
        attempt: u32,
        delay_secs: u64,
        message: String,
    },
    Final {
        state: State,
        message: String,
    },
}

/// What follows an exit, given the retries so far and whether the workload is still wanted.
pub fn on_exit(spec: &Workload, restart_count: u32, wanted: bool, ended: Ended) -> Exit {
    let reason = if ended.success && ended.code != 0 {
        format!("{}, counted as success", ended.reason)
    } else {
        ended.reason
    };
    // Jobs finish once; retrying scheduled jobs requires an explicit future execution.
    let retry = wanted
        && spec.kind == Kind::Service
        && (spec.restart == Restart::Always
            || (spec.restart == Restart::OnFailure && !ended.success));
    if retry && spec.max_restarts.allows(restart_count) {
        let delay_secs = retry_delay(spec.restart_backoff, spec.restart_delay_secs, restart_count);
        let attempt = restart_count.saturating_add(1);
        let message = match spec.max_restarts {
            RestartLimit::Count(limit) => {
                format!("{reason}; retry {attempt}/{limit} in {delay_secs}s")
            }
            RestartLimit::Unlimited => format!("{reason}; retry {attempt} in {delay_secs}s"),
        };
        return Exit::Retry {
            attempt,
            delay_secs,
            message,
        };
    }
    Exit::Final {
        state: if ended.success {
            State::Completed
        } else {
            State::Failed
        },
        message: if retry {
            format!("{reason}; restart limit reached")
        } else {
            reason
        },
    }
}

pub fn timed_out(spec: &Workload, started_at: Option<DateTime<Utc>>, now: DateTime<Utc>) -> bool {
    spec.run_timeout_secs.is_some_and(|limit| {
        started_at.is_some_and(|start| (now - start).num_seconds() >= limit as i64)
    })
}

pub fn stable(started_at: Option<DateTime<Utc>>, now: DateTime<Utc>) -> bool {
    started_at.is_some_and(|start| (now - start).num_seconds() >= STABLE_SECS)
}

/// Starts the workload with its output appended to `log`, after a line marking the start.
pub fn launch(spec: &Workload, log: &Path, now: DateTime<Utc>) -> Result<ManagedProcess> {
    OpenOptions::new()
        .create(true)
        .append(true)
        .open(log)
        .context("Open log file")
        .and_then(|file| launch_into(spec, file, now))
}

/// As `launch`, with the log already open.
pub fn launch_into(
    spec: &Workload,
    mut log: std::fs::File,
    now: DateTime<Utc>,
) -> Result<ManagedProcess> {
    writeln!(log, "\n[{}] Starting {}", now.to_rfc3339(), spec.name)?;
    ManagedProcess::spawn(spec, log)
}

/// Without `after`, the last `lines` lines. With `after`, what was written since that byte
/// offset. Returns the text and the offset to pass next time.
pub fn read_log(path: &Path, lines: usize, after: Option<u64>) -> Result<(String, u64)> {
    let Ok(mut file) = std::fs::File::open(path) else {
        return Ok((String::new(), 0));
    };
    let length = file.metadata()?.len();
    let oldest = length.saturating_sub(LOG_WINDOW);
    // An offset past the end means the log was rotated; its new content starts at zero.
    let requested = after.map(|offset| if offset > length { 0 } else { offset });
    let start = requested.unwrap_or(oldest).max(oldest);
    file.seek(SeekFrom::Start(start))?;
    let mut bytes = Vec::new();
    file.take(length - start).read_to_end(&mut bytes)?;
    // Hold back a trailing, partially written UTF-8 character until the rest arrives.
    let complete = match std::str::from_utf8(&bytes) {
        Err(error) if error.error_len().is_none() => error.valid_up_to(),
        _ => bytes.len(),
    };
    let text = String::from_utf8_lossy(&bytes[..complete]);
    let offset = start + complete as u64;
    let text = match requested {
        Some(requested) if start > requested => {
            format!("[{} bytes skipped]\n{text}", start - requested)
        }
        Some(_) => text.into_owned(),
        None => {
            let text = if start > 0 {
                text.split_once('\n').map_or("", |(_, rest)| rest)
            } else {
                &text
            };
            let mut tail = text
                .lines()
                .rev()
                .take(lines.clamp(1, 2000))
                .collect::<Vec<_>>();
            tail.reverse();
            tail.join("\n")
        }
    };
    Ok((text, offset))
}

pub fn rotate_log(path: &Path) -> Result<()> {
    if !std::fs::metadata(path).is_ok_and(|metadata| metadata.len() > LOG_LIMIT) {
        return Ok(());
    }
    // Copy/truncate keeps existing append handles valid; retention is best effort under heavy output.
    let mut source = std::fs::File::open(path)?;
    let length = source.metadata()?.len();
    source.seek(SeekFrom::Start(length.saturating_sub(LOG_LIMIT)))?;
    let mut archive = std::fs::File::create(path.with_extension("log.1"))?;
    std::io::copy(&mut source.take(LOG_LIMIT), &mut archive)?;
    OpenOptions::new().write(true).open(path)?.set_len(0)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::RestartBackoff;

    fn service() -> Workload {
        Workload {
            name: "web".into(),
            restart_delay_secs: 2,
            max_restarts: RestartLimit::Count(2),
            ..Default::default()
        }
    }

    fn ended(code: i32, spec: &Workload) -> Ended {
        Ended {
            code,
            reason: format!("Process exited with code {code}"),
            success: spec.is_success(code),
        }
    }

    #[test]
    fn failures_retry_with_backoff_until_the_limit() {
        let spec = service();
        assert_eq!(
            on_exit(&spec, 0, true, ended(1, &spec)),
            Exit::Retry {
                attempt: 1,
                delay_secs: 2,
                message: "Process exited with code 1; retry 1/2 in 2s".into(),
            }
        );
        assert_eq!(
            on_exit(&spec, 1, true, ended(1, &spec)),
            Exit::Retry {
                attempt: 2,
                delay_secs: 4,
                message: "Process exited with code 1; retry 2/2 in 4s".into(),
            }
        );
        assert_eq!(
            on_exit(&spec, 2, true, ended(1, &spec)),
            Exit::Final {
                state: State::Failed,
                message: "Process exited with code 1; restart limit reached".into(),
            }
        );
    }

    #[test]
    fn unlimited_restarts_with_a_fixed_delay_never_give_up() {
        let mut spec = service();
        spec.max_restarts = RestartLimit::Unlimited;
        spec.restart_backoff = RestartBackoff::Fixed;
        spec.restart_delay_secs = 5;
        assert_eq!(
            on_exit(&spec, 40, true, ended(1, &spec)),
            Exit::Retry {
                attempt: 41,
                delay_secs: 5,
                message: "Process exited with code 1; retry 41 in 5s".into(),
            }
        );
    }

    #[test]
    fn success_codes_complete_on_failure_services_and_always_restarts_them() {
        let mut spec = service();
        spec.success_exit_codes = vec![3];
        assert_eq!(
            on_exit(&spec, 0, true, ended(3, &spec)),
            Exit::Final {
                state: State::Completed,
                message: "Process exited with code 3, counted as success".into(),
            }
        );
        spec.restart = Restart::Always;
        assert!(matches!(
            on_exit(&spec, 0, true, ended(0, &spec)),
            Exit::Retry { attempt: 1, .. }
        ));
        spec.restart = Restart::Never;
        assert_eq!(
            on_exit(&spec, 0, true, ended(1, &spec)),
            Exit::Final {
                state: State::Failed,
                message: "Process exited with code 1".into(),
            }
        );
    }

    #[test]
    fn jobs_and_unwanted_workloads_finish_once() {
        let mut job = service();
        job.kind = Kind::Job;
        job.success_exit_codes = vec![3];
        assert_eq!(
            on_exit(&job, 0, true, ended(3, &job)),
            Exit::Final {
                state: State::Completed,
                message: "Process exited with code 3, counted as success".into(),
            }
        );
        assert_eq!(
            on_exit(&job, 0, true, ended(1, &job)),
            Exit::Final {
                state: State::Failed,
                message: "Process exited with code 1".into(),
            }
        );
        let spec = service();
        assert!(matches!(
            on_exit(&spec, 0, false, ended(1, &spec)),
            Exit::Final {
                state: State::Failed,
                ..
            }
        ));
    }

    #[test]
    fn timeouts_and_stability_count_from_the_start() {
        let now = Utc::now();
        let mut spec = service();
        let started = Some(now - chrono::Duration::seconds(61));
        assert!(!timed_out(&spec, started, now));
        spec.run_timeout_secs = Some(60);
        assert!(timed_out(&spec, started, now));
        assert!(!timed_out(&spec, None, now));
        assert!(stable(started, now));
        assert!(!stable(Some(now), now));
    }
}
