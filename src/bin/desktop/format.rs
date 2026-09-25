use crate::theme::Tone;
use chrono::{DateTime, TimeDelta, Utc};
use k3up::model::{Kind, Missed, Restart, Schedule, ScheduleAction, State, Status};

pub fn state_tone(state: State) -> Tone {
    match state {
        State::Running => Tone::Success,
        State::Completed => Tone::Accent,
        State::Failed => Tone::Danger,
        State::Backoff | State::Blocked => Tone::Warning,
        State::Starting | State::Pending => Tone::Info,
        State::Stopped => Tone::Neutral,
    }
}

pub fn state_label(state: State) -> &'static str {
    match state {
        State::Stopped => "Stopped",
        State::Pending => "Pending",
        State::Blocked => "Blocked",
        State::Starting => "Starting",
        State::Running => "Running",
        State::Backoff => "Backoff",
        State::Completed => "Completed",
        State::Failed => "Failed",
    }
}

pub fn needs_attention(state: State) -> bool {
    matches!(state, State::Failed | State::Backoff | State::Blocked)
}

pub fn kind_label(kind: Kind) -> &'static str {
    match kind {
        Kind::Service => "Service",
        Kind::Job => "Job",
    }
}

pub fn restart_label(restart: Restart) -> &'static str {
    match restart {
        Restart::Never => "Never",
        Restart::OnFailure => "On failure",
        Restart::Always => "Always",
    }
}

pub fn action_label(action: ScheduleAction) -> &'static str {
    match action {
        ScheduleAction::Start => "Start",
        ScheduleAction::Restart => "Restart",
    }
}

pub fn overlap_label(action: ScheduleAction) -> &'static str {
    match action {
        ScheduleAction::Start => "Skip",
        ScheduleAction::Restart => "Stop, then start",
    }
}

pub fn missed_label(missed: Missed) -> &'static str {
    match missed {
        Missed::Skip => "Skip",
        Missed::RunOnce => "Run once",
    }
}

pub fn duration(seconds: u64) -> String {
    let (days, hours, minutes, secs) = (
        seconds / 86_400,
        (seconds % 86_400) / 3_600,
        (seconds % 3_600) / 60,
        seconds % 60,
    );
    let mut parts = Vec::new();
    if days > 0 {
        parts.push(format!("{days}d"));
    }
    if hours > 0 {
        parts.push(format!("{hours}h"));
    }
    if minutes > 0 {
        parts.push(format!("{minutes}m"));
    }
    if secs > 0 || parts.is_empty() {
        parts.push(format!("{secs}s"));
    }
    parts.join(" ")
}

/// Seconds only under a minute, then the two most significant units, so live labels settle
/// down after their first minute.
pub fn duration_coarse(seconds: u64) -> String {
    if seconds < 60 {
        return format!("{seconds}s");
    }
    let full = duration(seconds - seconds % 60);
    let mut parts = full.split(' ');
    match (parts.next(), parts.next()) {
        (Some(first), Some(second)) => format!("{first} {second}"),
        (Some(first), None) => first.to_owned(),
        _ => full,
    }
}

/// How long a coarse label for this many seconds stays the same.
fn label_step(seconds: i64) -> i64 {
    if seconds < 60 {
        1
    } else if seconds < 86_400 {
        60
    } else {
        3_600
    }
}

/// The first instant after `now` at which `relative(time, _)` or the coarse duration since
/// `time` reads differently.
pub fn next_change(time: DateTime<Utc>, now: DateTime<Utc>) -> DateTime<Utc> {
    let delta = time - now;
    let elapsed = delta.abs();
    let step = label_step(elapsed.num_seconds()) * 1_000;
    let phase = elapsed.num_milliseconds() % step;
    // A countdown exactly on a boundary changes right after `now`, never at it.
    let wait = if delta < TimeDelta::zero() {
        step - phase
    } else {
        phase.max(1)
    };
    now + TimeDelta::milliseconds(wait)
}

pub fn relative(time: DateTime<Utc>, now: DateTime<Utc>) -> String {
    let delta = (time - now).num_seconds();
    if delta.abs() < 2 {
        return "now".into();
    }
    let span = duration_coarse(delta.unsigned_abs());
    if delta < 0 {
        format!("{span} ago")
    } else {
        format!("in {span}")
    }
}

pub fn clock(time: DateTime<Utc>) -> String {
    time.format("%H:%M:%S").to_string()
}

pub fn timestamp(time: DateTime<Utc>) -> String {
    time.format("%Y-%m-%d %H:%M:%S UTC").to_string()
}

pub fn cadence(schedule: &Schedule) -> String {
    match (schedule.every_secs, &schedule.cron) {
        (Some(seconds), _) => format!("Every {}", duration(seconds)),
        (None, Some(cron)) => cron.clone(),
        (None, None) => "—".into(),
    }
}

pub fn command_line(status: &Status) -> String {
    let mut parts = vec![status.workload.executable.clone()];
    parts.extend(status.workload.args.iter().map(|arg| {
        if arg.contains(' ') {
            format!("\"{arg}\"")
        } else {
            arg.clone()
        }
    }));
    parts.join(" ")
}

pub fn exit_label(code: Option<i32>) -> String {
    match code {
        None => "—".into(),
        Some(0) => "0".into(),
        Some(124) => "124 · timeout".into(),
        Some(code) => code.to_string(),
    }
}

pub fn count(value: usize) -> String {
    let digits = value.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (index, ch) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index).is_multiple_of(3) {
            out.push(',');
        }
        out.push(ch);
    }
    out
}

pub fn elide_start(text: &str, max: usize) -> String {
    let chars = text.chars().count();
    if chars <= max {
        return text.to_owned();
    }
    let tail: String = text.chars().skip(chars - max + 1).collect();
    format!("…{tail}")
}

pub fn elide_end(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_owned();
    }
    let head: String = text.chars().take(max.saturating_sub(1)).collect();
    format!("{head}…")
}

const BYTE_UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];

pub fn bytes(value: u64) -> String {
    let mut size = value as f64;
    let mut unit = 0;
    while size >= 1000.0 && unit < BYTE_UNITS.len() - 1 {
        size /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{value} B")
    } else if size >= 100.0 {
        format!("{size:.0} {}", BYTE_UNITS[unit])
    } else {
        format!("{size:.1} {}", BYTE_UNITS[unit])
    }
}

pub fn rate(value: u64) -> String {
    format!("{}/s", bytes(value))
}

pub fn percent(value: f32) -> String {
    format!("{:.1}%", value.max(0.0))
}

pub fn load(load: [f64; 3]) -> String {
    format!("{:.2} · {:.2} · {:.2}", load[0], load[1], load[2])
}

pub fn ratio(used: u64, total: u64) -> f32 {
    if total == 0 {
        0.0
    } else {
        (used as f32 / total as f32).clamp(0.0, 1.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_durations_compactly() {
        assert_eq!(duration(0), "0s");
        assert_eq!(duration(59), "59s");
        assert_eq!(duration(3_600), "1h");
        assert_eq!(duration(90_061), "1d 1h 1m 1s");
    }

    #[test]
    fn coarse_durations_keep_two_units() {
        assert_eq!(duration_coarse(90_061), "1d 1h");
        assert_eq!(duration_coarse(3_661), "1h 1m");
        assert_eq!(duration_coarse(3_600), "1h");
        assert_eq!(duration_coarse(90), "1m");
        assert_eq!(duration_coarse(59), "59s");
    }

    #[test]
    fn next_change_lands_on_label_boundaries() {
        let now = DateTime::parse_from_rfc3339("2026-01-01T12:00:00.500Z")
            .unwrap()
            .to_utc();
        let after = |ms: i64| now + TimeDelta::milliseconds(ms);
        assert_eq!(next_change(now - TimeDelta::seconds(30), now), after(1_000));
        assert_eq!(
            next_change(now - TimeDelta::milliseconds(30_200), now),
            after(800)
        );
        assert_eq!(
            next_change(now - TimeDelta::seconds(90), now),
            after(30_000)
        );
        assert_eq!(
            next_change(now - TimeDelta::seconds(90_000), now),
            after(3_600_000)
        );
        assert_eq!(
            next_change(now + TimeDelta::milliseconds(90_500), now),
            after(30_500)
        );
        assert_eq!(next_change(now + TimeDelta::seconds(60), now), after(1));
        assert_eq!(
            next_change(now + TimeDelta::milliseconds(2_400), now),
            after(400)
        );
    }

    #[test]
    fn formats_bytes_and_rates() {
        assert_eq!(bytes(0), "0 B");
        assert_eq!(bytes(999), "999 B");
        assert_eq!(bytes(1_536), "1.5 KB");
        assert_eq!(bytes(150 * 1024 * 1024), "150 MB");
        assert_eq!(bytes(3 * 1024 * 1024 * 1024 + 512 * 1024 * 1024), "3.5 GB");
        assert_eq!(rate(2_048), "2.0 KB/s");
        assert_eq!(percent(12.345), "12.3%");
        assert_eq!(percent(-0.2), "0.0%");
    }

    #[test]
    fn elides_from_the_end() {
        assert_eq!(elide_end("short", 10), "short");
        assert_eq!(elide_end("/bin/sh -c yes", 8), "/bin/sh…");
    }

    #[test]
    fn groups_thousands() {
        assert_eq!(count(999), "999");
        assert_eq!(count(1_000), "1,000");
        assert_eq!(count(1_234_567), "1,234,567");
    }
}
