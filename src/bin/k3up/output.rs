use anyhow::{Result, bail};
use k3up::protocol::Response;

/// What a command hands back to be printed. Most commands return the agent's response;
/// `health` and `agent status` have their own JSON shape.
pub enum Outcome {
    Response(Box<Response>),
    Custom {
        text: String,
        json: serde_json::Value,
        ok: bool,
    },
}

impl From<Response> for Outcome {
    fn from(response: Response) -> Self {
        Self::Response(Box::new(response))
    }
}

pub fn checked(response: Response) -> Result<Response> {
    if !response.ok {
        bail!("{}", response.message);
    }
    Ok(response)
}

/// Prints the outcome and returns whether it counts as a success.
pub fn print(outcome: Outcome, json: bool) -> Result<bool> {
    match outcome {
        Outcome::Custom {
            text,
            json: value,
            ok,
        } => {
            if json {
                println!("{}", serde_json::to_string_pretty(&value)?);
            } else {
                println!("{text}");
            }
            Ok(ok)
        }
        Outcome::Response(response) => print_response(*response, json),
    }
}

fn print_response(response: Response, json: bool) -> Result<bool> {
    let success = response.ok;
    if json {
        println!("{}", serde_json::to_string_pretty(&response)?);
    } else if !success {
        eprintln!("{}", response.message);
    } else if let Some(manifest) = response.manifest {
        println!("{}", manifest.to_toml()?);
    } else if let Some(text) = response.text {
        println!("{text}");
    } else if !response.workloads.is_empty() {
        print_workloads(&response.workloads);
    } else if !response.events.is_empty() {
        for event in response.events {
            println!(
                "{}  {:<20} {}",
                event.at.format("%Y-%m-%d %H:%M:%S UTC"),
                event.name,
                event.message
            );
        }
    } else {
        println!("{}", response.message);
    }
    Ok(success)
}

fn print_workloads(workloads: &[k3up::model::Status]) {
    println!(
        "{:<24} {:<12} {:<8} {:<8} {:<6} {:<12} REASON",
        "NAME", "STATE", "PID", "RETRIES", "EXIT", "NEXT RUN"
    );
    let now = chrono::Utc::now();
    for status in workloads {
        let dash = || "-".to_string();
        println!(
            "{:<24} {:<12} {:<8} {:<8} {:<6} {:<12} {}",
            status.workload.name,
            status.state,
            status.pid.map(|id| id.to_string()).unwrap_or_else(dash),
            status.restart_count,
            status
                .last_exit
                .map(|code| code.to_string())
                .unwrap_or_else(dash),
            status
                .next_run
                .map(|at| next_run(at, now))
                .unwrap_or_else(dash),
            status.reason
        );
    }
}

fn next_run(at: chrono::DateTime<chrono::Utc>, now: chrono::DateTime<chrono::Utc>) -> String {
    match (at - now).num_seconds() {
        ..=0 => "now".into(),
        seconds => format!("in {}", duration(seconds as u64)),
    }
}

pub fn bytes(value: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut size = value as f64;
    let mut unit = 0;
    while size >= 1024.0 && unit < UNITS.len() - 1 {
        size /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{value} B")
    } else {
        format!("{size:.1} {}", UNITS[unit])
    }
}

pub fn rate(value: u64) -> String {
    format!("{}/s", bytes(value))
}

pub fn duration(seconds: u64) -> String {
    match seconds {
        0..60 => format!("{seconds}s"),
        60..3600 => format!("{}m {}s", seconds / 60, seconds % 60),
        3600..86400 => format!("{}h {}m", seconds / 3600, seconds % 3600 / 60),
        _ => format!("{}d {}h", seconds / 86400, seconds % 86400 / 3600),
    }
}

pub fn command_line(workload: &k3up::model::Workload) -> String {
    std::iter::once(workload.executable.as_str())
        .chain(workload.args.iter().map(String::as_str))
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_sizes_and_durations() {
        assert_eq!(bytes(512), "512 B");
        assert_eq!(bytes(1536), "1.5 KB");
        assert_eq!(rate(2048), "2.0 KB/s");
        assert_eq!(duration(45), "45s");
        assert_eq!(duration(125), "2m 5s");
        assert_eq!(duration(3_660), "1h 1m");
        assert_eq!(duration(90_000), "1d 1h");
    }

    #[test]
    fn next_run_is_relative_to_now() {
        let now = chrono::Utc::now();
        assert_eq!(
            next_run(now + chrono::TimeDelta::seconds(300), now),
            "in 5m 0s"
        );
        assert_eq!(next_run(now - chrono::TimeDelta::seconds(1), now), "now");
    }
}
