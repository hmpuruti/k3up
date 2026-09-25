use crate::{
    charts, format,
    message::Message,
    theme::{self, Tone},
    widgets,
};
use chrono::TimeDelta;
use iced::{
    Element,
    Length::Fill,
    widget::{column, row},
};
use k3up::metrics::Metrics;

const MEMORY_WARNING: f32 = 0.85;
const SWAP_WARNING: f32 = 0.5;
const DISK_FREE_WARNING: f32 = 0.10;
const CPU_WARNING: f32 = 90.0;
const CPU_WINDOW: TimeDelta = TimeDelta::seconds(60);

struct Vital {
    label: &'static str,
    value: String,
    fraction: f32,
    meta: Vec<String>,
    warning: bool,
}

impl Vital {
    fn tone(&self) -> Tone {
        if self.warning {
            Tone::Warning
        } else {
            Tone::Neutral
        }
    }
}

/// High CPU only counts once the samples actually span most of the window; a fresh agent's
/// two samples a few seconds apart are not "sustained".
fn cpu_sustained(metrics: &Metrics) -> bool {
    let recent: Vec<_> = metrics
        .history
        .iter()
        .filter(|point| metrics.at - point.at <= CPU_WINDOW)
        .collect();
    let spanned = recent
        .first()
        .is_some_and(|first| metrics.at - first.at >= CPU_WINDOW * 3 / 4);
    spanned && recent.iter().all(|point| point.cpu >= CPU_WARNING)
}

fn assess(metrics: &Metrics) -> [Vital; 3] {
    let machine = &metrics.machine;
    let memory = format::ratio(machine.memory_used, machine.memory_total);
    let swap = format::ratio(machine.swap_used, machine.swap_total);
    let disk_free = format::ratio(machine.disk_available, machine.disk_total);
    let cpu_meta = vec![
        format!("{} cores", machine.cores),
        machine
            .load
            .map(|load| format!("load {}", format::load(load)))
            .unwrap_or_else(|| format!("up {}", format::duration_coarse(machine.uptime_secs))),
    ];
    let memory_meta = vec![
        format!(
            "{} of {}",
            format::bytes(machine.memory_used),
            format::bytes(machine.memory_total)
        ),
        if machine.swap_total > 0 {
            format!(
                "swap {} of {}",
                format::bytes(machine.swap_used),
                format::bytes(machine.swap_total)
            )
        } else {
            "no swap".into()
        },
    ];
    let disk_meta = vec![
        format!(
            "{} of {}",
            format::percent(disk_free * 100.0),
            format::bytes(machine.disk_total)
        ),
        format!(
            "{} used",
            format::bytes(machine.disk_total.saturating_sub(machine.disk_available))
        ),
    ];
    [
        Vital {
            label: "CPU",
            value: format::percent(machine.cpu),
            fraction: machine.cpu / 100.0,
            meta: cpu_meta,
            warning: cpu_sustained(metrics),
        },
        Vital {
            label: "MEMORY",
            value: format::percent(memory * 100.0),
            fraction: memory,
            meta: memory_meta,
            warning: memory > MEMORY_WARNING || (machine.swap_total > 0 && swap > SWAP_WARNING),
        },
        Vital {
            label: "DISK FREE",
            value: format::bytes(machine.disk_available),
            fraction: 1.0 - disk_free,
            meta: disk_meta,
            warning: machine.disk_total > 0 && disk_free < DISK_FREE_WARNING,
        },
    ]
}

pub fn warnings(metrics: &Metrics) -> usize {
    assess(metrics).iter().filter(|vital| vital.warning).count()
}

pub fn view<'a>(metrics: Option<&'a Metrics>, scale: f32) -> Element<'a, Message> {
    let mut tiles = row![].spacing(theme::SPACE_LG).width(Fill);
    match metrics {
        Some(metrics) => {
            for vital in assess(metrics) {
                tiles = tiles.push(tile(vital, scale));
            }
        }
        None => {
            for label in ["CPU", "MEMORY", "DISK FREE"] {
                tiles = tiles.push(skeleton(label));
            }
        }
    }
    tiles.into()
}

fn tile<'a>(vital: Vital, scale: f32) -> Element<'a, Message> {
    let tone = vital.tone();
    widgets::stat_tile(
        vital.label,
        widgets::value(vital.value, tone).into(),
        Some(charts::gauge(charts::Gauge::new(
            vital.fraction,
            if vital.warning {
                Tone::Warning
            } else {
                Tone::Accent
            },
            scale,
        ))),
        widgets::meta_lines(vital.meta),
        tone,
    )
}

fn skeleton<'a>(label: &'a str) -> Element<'a, Message> {
    widgets::stat_tile(
        label,
        widgets::skeleton(88, 24),
        Some(widgets::skeleton(Fill, 5)),
        column![widgets::skeleton(140, 12), widgets::skeleton(100, 12)]
            .spacing(theme::SPACE_XS + 2)
            .into(),
        Tone::Neutral,
    )
}
