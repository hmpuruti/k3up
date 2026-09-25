use crate::{
    logs::INITIAL_LINES,
    message::{Failure, LogChunk, Message, Sample, WatchEvent},
};
use chrono::{DateTime, Utc};
use iced::{
    Subscription,
    futures::{SinkExt, stream},
    stream::channel,
};
use k3up::{
    client::Client,
    metrics::ProcessProbe,
    protocol::{Command, Response},
};
use std::{
    sync::{Arc, Mutex},
    time::Duration,
};

const WATCH_TIMEOUT: Duration = Duration::from_secs(25);
const WATCH_BACKOFF_MAX: Duration = Duration::from_secs(5);
const LOG_INTERVAL: Duration = Duration::from_secs(1);
const METRICS_INTERVAL: Duration = Duration::from_secs(2);
/// Fires a little after the deadline so the wall clock has certainly crossed it.
const ALARM_MARGIN: Duration = Duration::from_millis(20);

pub fn request(client: &Client, command: Command) -> Result<Response, Failure> {
    let response = client
        .send(command)
        .map_err(|error| Failure::Offline(format!("{error:#}")))?;
    if response.ok {
        Ok(response)
    } else {
        Err(Failure::Request(response.message))
    }
}

async fn send(client: &Client, command: Command) -> Result<Response, Failure> {
    let client = client.clone();
    tokio::task::spawn_blocking(move || request(&client, command))
        .await
        .unwrap_or_else(|error| Err(Failure::Offline(error.to_string())))
}

fn backoff(failures: u32) -> Duration {
    Duration::from_secs(1 << failures.saturating_sub(1).min(3)).min(WATCH_BACKOFF_MAX)
}

/// Long-polls the agent and reports each new generation once. A first probe with no timeout
/// makes a fresh session load right away; after an outage the probe restarts from zero so an
/// agent that came back with a smaller counter is noticed at once.
pub fn watch(client: Client, session: u64) -> Subscription<Message> {
    Subscription::run_with_id(
        ("watch", session),
        channel(8, move |mut out| async move {
            let mut since = 0;
            let mut timeout = Duration::ZERO;
            let mut reported = None;
            let mut failures = 0u32;
            loop {
                let command = Command::Watch {
                    since,
                    timeout_ms: timeout.as_millis() as u64,
                };
                match send(&client, command).await {
                    Ok(response) => {
                        let generation = response.generation.unwrap_or(since);
                        since = generation;
                        timeout = WATCH_TIMEOUT;
                        failures = 0;
                        if reported == Some(generation) {
                            continue;
                        }
                        reported = Some(generation);
                        let event = WatchEvent::Changed(generation);
                        if out.send(Message::Watch { session, event }).await.is_err() {
                            return;
                        }
                    }
                    Err(failure) => {
                        since = 0;
                        timeout = Duration::ZERO;
                        reported = None;
                        failures = failures.saturating_add(1);
                        if failures == 1 {
                            let event = WatchEvent::Failed(failure);
                            if out.send(Message::Watch { session, event }).await.is_err() {
                                return;
                            }
                        }
                        tokio::time::sleep(backoff(failures)).await;
                    }
                }
            }
        }),
    )
}

/// Follows one workload's output from `after`, reporting only chunks that carry new text.
/// The first answer is always reported so the buffer learns its starting offset.
pub fn logs(
    client: Client,
    session: u64,
    generation: u64,
    name: String,
    after: Option<u64>,
) -> Subscription<Message> {
    Subscription::run_with_id(
        ("logs", session, generation, name.clone()),
        channel(8, move |mut out| async move {
            let mut after = after;
            let mut failed = None;
            loop {
                let command = Command::Logs {
                    name: name.clone(),
                    lines: INITIAL_LINES,
                    after,
                };
                let result = match send(&client, command).await {
                    Ok(response) => {
                        let chunk = LogChunk {
                            text: response.text.unwrap_or_default(),
                            offset: response.offset.unwrap_or(0),
                        };
                        let fresh = after.is_none() || !chunk.text.is_empty();
                        after = Some(chunk.offset);
                        failed = None;
                        fresh.then_some(Ok(chunk))
                    }
                    Err(failure) => {
                        let repeat = failed.as_ref() == Some(&failure);
                        failed = Some(failure.clone());
                        (!repeat).then_some(Err(failure))
                    }
                };
                if let Some(result) = result
                    && out
                        .send(Message::LogsLoaded {
                            session,
                            generation,
                            result,
                        })
                        .await
                        .is_err()
                {
                    return;
                }
                tokio::time::sleep(LOG_INTERVAL).await;
            }
        }),
    )
}

/// Polls metrics while something on screen shows them, asking only for history after the
/// newest point already held, and reports a sample only when the agent has taken a new one.
pub fn metrics(
    client: Client,
    probe: Arc<Mutex<ProcessProbe>>,
    session: u64,
    since: Option<DateTime<Utc>>,
) -> Subscription<Message> {
    Subscription::run_with_id(
        ("metrics", session),
        channel(8, move |mut out| async move {
            let mut since = since;
            let mut reported = None;
            let mut failed = None;
            loop {
                let client = client.clone();
                let probe = probe.clone();
                let sampled = tokio::task::spawn_blocking(move || {
                    let metrics = request(&client, Command::Metrics { since })?.metrics;
                    let desktop = probe
                        .lock()
                        .map(|mut probe| probe.sample())
                        .unwrap_or_default();
                    Ok(Sample { metrics, desktop })
                })
                .await
                .unwrap_or_else(|error| Err(Failure::Offline(error.to_string())));
                let result = match sampled {
                    Ok(sample) => {
                        let at = sample.metrics.as_ref().map(|metrics| metrics.at);
                        if let Some(metrics) = &sample.metrics {
                            since = metrics
                                .history
                                .iter()
                                .map(|point| point.at)
                                .chain(since)
                                .max();
                        }
                        failed = None;
                        let repeat = reported == Some(at);
                        reported = Some(at);
                        (!repeat).then_some(Ok(sample))
                    }
                    Err(failure) => {
                        let repeat = failed.as_ref() == Some(&failure);
                        failed = Some(failure.clone());
                        (!repeat).then_some(Err(failure))
                    }
                };
                if let Some(result) = result
                    && out
                        .send(Message::MetricsLoaded { session, result })
                        .await
                        .is_err()
                {
                    return;
                }
                tokio::time::sleep(METRICS_INTERVAL).await;
            }
        }),
    )
}

/// One tick at `at`, replacing any earlier alarm with a different deadline.
pub fn alarm(at: DateTime<Utc>) -> Subscription<Message> {
    Subscription::run_with_id(
        ("alarm", at.timestamp_millis()),
        stream::once(async move {
            let wait = (at - Utc::now()).to_std().unwrap_or_default();
            tokio::time::sleep(wait + ALARM_MARGIN).await;
            Message::Tick
        }),
    )
}
