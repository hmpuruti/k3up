use crate::{
    output::{Outcome, bytes, checked, command_line, duration, rate},
    workloads,
};
use anyhow::{Result, bail};
use k3up::{
    client::Client,
    health::{Report, Verdict},
    metrics::{Metrics, WorkloadUsage},
    model::Status,
    protocol::{Command, Response},
};
use std::time::{Duration, Instant};

/// The agent's first sample needs a CPU baseline, so a fresh agent may have none yet.
/// Asking switches the agent to fast sampling, so a stale sample is replaced within seconds.
fn fresh_metrics(client: &Client, history: bool) -> Result<Metrics> {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let since = (!history).then(chrono::Utc::now);
        let response = checked(client.send(Command::Metrics { since })?)?;
        match response.metrics {
            Some(metrics)
                if chrono::Utc::now() - metrics.at < chrono::TimeDelta::seconds(1)
                    || Instant::now() >= deadline =>
            {
                return Ok(*metrics);
            }
            None if Instant::now() >= deadline => {
                bail!("The agent has not collected a sample yet")
            }
            _ => std::thread::sleep(Duration::from_millis(250)),
        }
    }
}

pub fn stats(
    client: &Client,
    name: Option<String>,
    watch: Option<u64>,
    json: bool,
) -> Result<Outcome> {
    let Some(seconds) = watch else {
        return Ok(sample(client, name.as_deref())?.into());
    };
    loop {
        let response = sample(client, name.as_deref())?;
        if json {
            println!("{}", serde_json::to_string(&response)?);
        } else {
            println!(
                "\x1b[2J\x1b[H{}",
                response.text.as_deref().unwrap_or_default()
            );
        }
        std::thread::sleep(Duration::from_secs(seconds.max(1)));
    }
}

fn sample(client: &Client, name: Option<&str>) -> Result<Response> {
    match name {
        None => overview(client),
        Some(name) => detail(client, name),
    }
}

fn overview(client: &Client) -> Result<Response> {
    let metrics = fresh_metrics(client, false)?;
    let workloads = checked(client.send(Command::List)?)?.workloads;
    let mut text = machine_lines(&metrics);
    text += &format!(
        "K3 Up    workloads {:.1}% CPU, {}  ·  agent {:.1}% CPU, {}  ·  data {}\n\n",
        metrics.managed.cpu,
        bytes(metrics.managed.memory),
        metrics.agent.cpu,
        bytes(metrics.agent.memory),
        bytes(metrics.data_bytes)
    );
    text += &format!(
        "{:<22} {:>5} {:>7} {:>10} {:>10} {:>10} {:>10}  COMMAND",
        "NAME", "PROCS", "CPU", "MEMORY", "READ/S", "WRITE/S", "LOGS"
    );
    for usage in &metrics.workloads {
        let command = workloads
            .iter()
            .find(|status| status.workload.name == usage.name)
            .map(|status| command_line(&status.workload))
            .unwrap_or_default();
        text += &format!(
            "\n{:<22} {:>5} {:>6.1}% {:>10} {:>10} {:>10} {:>10}  {command}",
            usage.name,
            usage.usage.processes,
            usage.usage.cpu,
            bytes(usage.usage.memory),
            bytes(usage.usage.read_rate),
            bytes(usage.usage.write_rate),
            bytes(usage.log_bytes)
        );
    }
    if metrics.workloads.is_empty() {
        text += "\nNo workloads are running.";
    }
    Ok(Response {
        text: Some(text),
        metrics: Some(Box::new(metrics)),
        ..Response::success("Metrics")
    })
}

fn machine_lines(metrics: &Metrics) -> String {
    let machine = &metrics.machine;
    let mut text = format!(
        "{}  ·  {}  ·  up {}\n",
        machine.host,
        machine.os,
        duration(machine.uptime_secs)
    );
    text += &format!("CPU      {:>5.1}%  of {} cores", machine.cpu, machine.cores);
    if let Some([one, five, fifteen]) = machine.load {
        text += &format!("  ·  load {one:.2} {five:.2} {fifteen:.2}");
    }
    text += &format!(
        "\nMemory   {} of {}  ·  swap {} of {}\n",
        bytes(machine.memory_used),
        bytes(machine.memory_total),
        bytes(machine.swap_used),
        bytes(machine.swap_total)
    );
    text += &format!(
        "Disk     {} free of {}\n",
        bytes(machine.disk_available),
        bytes(machine.disk_total)
    );
    text
}

fn detail(client: &Client, name: &str) -> Result<Response> {
    let status = workloads::get(client, name)?;
    let mut metrics = fresh_metrics(client, false)?;
    metrics.workloads.retain(|usage| usage.name == name);
    let text = detail_lines(&status, metrics.workloads.first(), metrics.at);
    Ok(Response {
        text: Some(text),
        workloads: vec![status],
        metrics: Some(Box::new(metrics)),
        ..Response::success("Metrics")
    })
}

fn detail_lines(
    status: &Status,
    usage: Option<&WorkloadUsage>,
    now: chrono::DateTime<chrono::Utc>,
) -> String {
    let mut text = format!("{}  ·  {}", status.workload.name, status.state);
    if let Some(pid) = status.pid {
        text += &format!("  ·  pid {pid}");
    }
    if let Some(started) = status.started_at.filter(|_| status.pid.is_some()) {
        text += &format!(
            "  ·  up {}",
            duration((now - started).num_seconds().max(0) as u64)
        );
    }
    text += &format!("\nCommand    {}\n", command_line(&status.workload));
    text += &format!("Reason     {}\n", status.reason);
    let Some(usage) = usage else {
        text += "No resource sample: the workload is not running.";
        return text;
    };
    let pids = usage
        .pids
        .iter()
        .map(|pid| pid.to_string())
        .collect::<Vec<_>>()
        .join(" ");
    text += &format!("Processes  {}  ({pids})\n", usage.usage.processes);
    let (cpu_min, cpu_avg, cpu_max) = spread(usage.cpu_history.iter().map(|value| *value as f64));
    text += &format!(
        "CPU        {:.1}%  ·  min {cpu_min:.1}%  avg {cpu_avg:.1}%  max {cpu_max:.1}%  over {} samples\n",
        usage.usage.cpu,
        usage.cpu_history.len()
    );
    let (mem_min, mem_avg, mem_max) =
        spread(usage.memory_history.iter().map(|value| *value as f64));
    text += &format!(
        "Memory     {}  ·  min {}  avg {}  max {}\n",
        bytes(usage.usage.memory),
        bytes(mem_min as u64),
        bytes(mem_avg as u64),
        bytes(mem_max as u64)
    );
    text += &format!(
        "Disk       read {}  ·  write {}\n",
        rate(usage.usage.read_rate),
        rate(usage.usage.write_rate)
    );
    text += &format!("Logs       {}", bytes(usage.log_bytes));
    text
}

fn spread(values: impl Iterator<Item = f64>) -> (f64, f64, f64) {
    let (mut min, mut max, mut sum, mut count) = (f64::MAX, f64::MIN, 0.0, 0usize);
    for value in values {
        min = min.min(value);
        max = max.max(value);
        sum += value;
        count += 1;
    }
    if count == 0 {
        (0.0, 0.0, 0.0)
    } else {
        (min, sum / count as f64, max)
    }
}

pub fn health(client: &Client, strict: bool) -> Result<Outcome> {
    let metrics = fresh_metrics(client, true)?;
    let statuses = checked(client.send(Command::List)?)?.workloads;
    let report = k3up::health::assess(&metrics, &statuses);
    let ok = !strict || report.status == Verdict::Ok;
    Ok(Outcome::Custom {
        text: health_lines(&report, &metrics),
        json: serde_json::to_value(&report)?,
        ok,
    })
}

fn health_lines(report: &Report, metrics: &Metrics) -> String {
    let verdict = match report.status {
        Verdict::Ok => "ok",
        Verdict::Warning => "warning",
    };
    let mut text = format!("Status   {verdict}\n");
    text += &machine_lines(metrics);
    text += &format!(
        "K3 Up    {} {}, {} running  ·  workloads {:.1}% CPU, {}  ·  agent {:.1}% CPU, {}  ·  data {}\n",
        report.k3up.workload_count,
        if report.k3up.workload_count == 1 {
            "workload"
        } else {
            "workloads"
        },
        report.k3up.running,
        report.k3up.workloads.cpu,
        bytes(report.k3up.workloads.memory),
        report.k3up.agent.cpu,
        bytes(report.k3up.agent.memory),
        bytes(report.k3up.data_bytes)
    );
    text += "\nWarnings\n";
    if report.warnings.is_empty() {
        text += "  none\n";
    }
    for warning in &report.warnings {
        text += &format!(
            "  {:<8} {}\n",
            format!("{:?}", warning.kind).to_lowercase(),
            warning.message
        );
    }
    text += "\nAttention\n";
    if report.attention.is_empty() {
        text += "  none";
    } else {
        text += &format!("  {:<24} {:<10} {:>5}  REASON", "NAME", "STATE", "EXIT");
        for item in &report.attention {
            text += &format!(
                "\n  {:<24} {:<10} {:>5}  {}",
                item.name,
                item.state,
                item.last_exit
                    .map(|code| code.to_string())
                    .unwrap_or_else(|| "-".into()),
                item.reason
            );
        }
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spread_handles_empty_and_filled_histories() {
        assert_eq!(spread(std::iter::empty()), (0.0, 0.0, 0.0));
        assert_eq!(spread([1.0, 2.0, 6.0].into_iter()), (1.0, 3.0, 6.0));
    }
}
