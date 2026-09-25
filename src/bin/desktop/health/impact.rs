use super::{heading, panel};
use crate::{
    format,
    message::Message,
    theme::{self, Tone},
    widgets,
};
use iced::{
    Alignment, Element,
    Length::{self, Fill},
    widget::{Space, column, row},
};
use k3up::metrics::{Metrics, Usage};

const PART_WIDTH: Length = Length::Fixed(110.0);

fn total(metrics: &Metrics, desktop: Usage) -> Usage {
    Usage {
        cpu: metrics.managed.cpu + metrics.agent.cpu + desktop.cpu,
        memory: metrics.managed.memory + metrics.agent.memory + desktop.memory,
        read_rate: metrics.managed.read_rate + metrics.agent.read_rate + desktop.read_rate,
        write_rate: metrics.managed.write_rate + metrics.agent.write_rate + desktop.write_rate,
        processes: metrics.managed.processes + metrics.agent.processes + desktop.processes,
    }
}

pub fn view(metrics: Option<&Metrics>, desktop: Usage) -> Element<'_, Message> {
    let Some(metrics) = metrics else {
        return skeleton();
    };
    let sum = total(metrics, desktop);
    let logs: u64 = metrics.workloads.iter().map(|w| w.log_bytes).sum();
    let totals = row![
        widgets::fact(
            "CPU",
            widgets::value(format::percent(sum.cpu), Tone::Neutral)
        ),
        widgets::fact(
            "MEMORY",
            widgets::value(format::bytes(sum.memory), Tone::Neutral)
        ),
        widgets::fact(
            "READ",
            widgets::value(format::rate(sum.read_rate), Tone::Neutral)
        ),
        widgets::fact(
            "WRITE",
            widgets::value(format::rate(sum.write_rate), Tone::Neutral)
        ),
        widgets::fact(
            "DATA",
            column![
                widgets::value(format::bytes(metrics.data_bytes), Tone::Neutral),
                widgets::faint(format!("logs {}", format::bytes(logs)), theme::TEXT_META),
            ]
            .spacing(2)
        ),
    ]
    .spacing(theme::SPACE_MD);
    let breakdown = column![
        columns(),
        widgets::divider(),
        part("Workloads", metrics.managed, Tone::Accent),
        part("Agent", metrics.agent, Tone::Info),
        part("Desktop", desktop, Tone::Neutral),
    ]
    .spacing(theme::SPACE_SM);
    panel(
        heading(
            "K3 UP",
            widgets::pill(format!("{} processes", sum.processes), Tone::Neutral),
        ),
        column![totals, widgets::divider(), breakdown].spacing(theme::SPACE_LG),
    )
}

fn columns<'a>() -> Element<'a, Message> {
    let label = |text: &'a str| widgets::caption(text);
    row![
        Space::with_width(PART_WIDTH),
        right(label("PROCS")),
        right(label("CPU")),
        right(label("MEMORY")),
        right(label("READ/S")),
        right(label("WRITE/S")),
    ]
    .spacing(theme::SPACE_MD)
    .into()
}

fn right<'a>(content: Element<'a, Message>) -> Element<'a, Message> {
    row![Space::with_width(Fill), content].width(Fill).into()
}

fn part<'a>(label: &'a str, usage: Usage, tone: Tone) -> Element<'a, Message> {
    row![
        row![
            widgets::glyph(widgets::glyph::DOT, 8).style(theme::text_toned(tone)),
            iced::widget::text(label).size(theme::TEXT_BODY),
        ]
        .spacing(theme::SPACE_SM)
        .align_y(Alignment::Center)
        .width(PART_WIDTH),
        widgets::cell(usage.processes.to_string(), Fill),
        widgets::cell(format::percent(usage.cpu), Fill),
        widgets::cell(format::bytes(usage.memory), Fill),
        widgets::cell(format::rate(usage.read_rate), Fill),
        widgets::cell(format::rate(usage.write_rate), Fill),
    ]
    .spacing(theme::SPACE_MD)
    .align_y(Alignment::Center)
    .into()
}

fn skeleton<'a>() -> Element<'a, Message> {
    let mut totals = row![].spacing(theme::SPACE_MD);
    for label in ["CPU", "MEMORY", "READ", "WRITE", "DATA"] {
        totals = totals.push(widgets::fact(label, widgets::skeleton(72, 24)));
    }
    let mut parts = column![columns(), widgets::divider()].spacing(theme::SPACE_SM);
    for label in ["Workloads", "Agent", "Desktop"] {
        parts = parts.push(
            row![
                iced::widget::text(label)
                    .size(theme::TEXT_BODY)
                    .width(PART_WIDTH),
                widgets::skeleton(Fill, 12),
            ]
            .spacing(theme::SPACE_MD)
            .align_y(Alignment::Center),
        );
    }
    panel(
        heading("K3 UP", widgets::skeleton(80, 12)),
        column![totals, widgets::divider(), parts].spacing(theme::SPACE_LG),
    )
}
