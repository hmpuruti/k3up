use super::{heading, panel};
use crate::{
    charts::{self, Area, Sample},
    format,
    message::Message,
    theme::{self, Tone},
    widgets,
};
use chrono::TimeDelta;
use iced::{
    Alignment, Element,
    Length::Fill,
    widget::{Space, column, row},
};
use k3up::metrics::{Metrics, Point};

const WINDOW: TimeDelta = TimeDelta::hours(1);
const CHART_HEIGHT: f32 = 150.0;
const AXIS_WIDTH: f32 = 44.0;

pub fn view<'a>(
    metrics: Option<&'a Metrics>,
    charts: &'a charts::Cache,
    scale: f32,
) -> Element<'a, Message> {
    let (cpu, memory) = match metrics {
        Some(metrics) => (cpu(metrics, charts, scale), memory(metrics, charts, scale)),
        None => (skeleton("CPU"), skeleton("MEMORY")),
    };
    row![cpu, memory].spacing(theme::SPACE_LG).into()
}

fn samples(
    metrics: &Metrics,
    total: impl Fn(&Point) -> f32,
    managed: impl Fn(&Point) -> f32,
) -> Vec<Sample> {
    let span = WINDOW.num_milliseconds() as f32;
    metrics
        .history
        .iter()
        .map(|point| Sample {
            x: 1.0 - (metrics.at - point.at).num_milliseconds() as f32 / span,
            total: total(point),
            managed: managed(point),
        })
        .collect()
}

fn cpu<'a>(metrics: &'a Metrics, charts: &'a charts::Cache, scale: f32) -> Element<'a, Message> {
    let chart = Area::new(
        samples(metrics, |p| p.cpu, |p| p.managed_cpu),
        100.0,
        scale,
        charts,
        charts::slot(("trend", "cpu")),
    );
    trend(
        "CPU",
        format::percent(metrics.machine.cpu),
        format::percent(metrics.managed.cpu),
        chart,
        ["100%".into(), "50%".into()],
    )
}

fn memory<'a>(metrics: &'a Metrics, charts: &'a charts::Cache, scale: f32) -> Element<'a, Message> {
    let total = metrics.machine.memory_total;
    let chart = Area::new(
        samples(
            metrics,
            |p| p.memory_used as f32,
            |p| p.managed_memory as f32,
        ),
        total as f32,
        scale,
        charts,
        charts::slot(("trend", "memory")),
    );
    trend(
        "MEMORY",
        format::bytes(metrics.machine.memory_used),
        format::bytes(metrics.managed.memory),
        chart,
        [format::bytes(total), format::bytes(total / 2)],
    )
}

fn trend<'a>(
    label: &'a str,
    machine: String,
    managed: String,
    chart: Area<'a>,
    ticks: [String; 2],
) -> Element<'a, Message> {
    let legend = row![
        widgets::legend("Machine", machine, Tone::Neutral),
        widgets::legend("K3 Up", managed, Tone::Accent),
    ]
    .spacing(theme::SPACE_MD);
    let [top, middle] = ticks;
    panel(
        heading(label, legend.into()),
        column![
            row![charts::area(chart, CHART_HEIGHT), axis_y(top, middle)].spacing(theme::SPACE_XS),
            axis_x(),
        ]
        .spacing(theme::SPACE_XS),
    )
}

fn axis_y<'a>(top: String, middle: String) -> Element<'a, Message> {
    column![
        widgets::caption(top),
        Space::with_height(Fill),
        widgets::caption(middle),
        Space::with_height(Fill),
        widgets::caption("0"),
    ]
    .align_x(Alignment::Start)
    .width(AXIS_WIDTH)
    .height(CHART_HEIGHT)
    .padding(iced::Padding {
        top: 1.0,
        bottom: 1.0,
        ..Default::default()
    })
    .into()
}

fn axis_x<'a>() -> Element<'a, Message> {
    row![
        widgets::caption("60 MIN"),
        Space::with_width(Fill),
        widgets::caption("NOW"),
    ]
    .padding(iced::Padding {
        left: 6.0,
        right: AXIS_WIDTH + 6.0,
        ..Default::default()
    })
    .into()
}

fn skeleton<'a>(label: &'a str) -> Element<'a, Message> {
    panel(
        heading(label, widgets::skeleton(120, 12)),
        column![
            row![
                widgets::skeleton(Fill, CHART_HEIGHT),
                Space::with_width(AXIS_WIDTH),
            ]
            .spacing(theme::SPACE_XS),
            axis_x(),
        ]
        .spacing(theme::SPACE_XS),
    )
}
