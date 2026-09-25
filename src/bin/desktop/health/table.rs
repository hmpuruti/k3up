use super::{Column, Sort, panel};
use crate::{
    charts::{self, Sparkline},
    format,
    message::Message,
    theme::{self, Tone},
    widgets::{self, glyph},
};
use iced::{
    Alignment, Element,
    Length::{self, Fill},
    widget::{Space, column, container, row, text, tooltip},
};
use k3up::{
    metrics::{Metrics, WorkloadUsage},
    model::{State, Status},
};

const PROCS: Length = Length::Fixed(48.0);
const RATE: Length = Length::Fixed(68.0);
const LOG: Length = Length::Fixed(60.0);
const SPARK_WIDTH: f32 = 60.0;
const SPARK_HEIGHT: f32 = 20.0;
const SPARK: Length = Length::Fixed(SPARK_WIDTH + 58.0 + 8.0);
const PIDS_SHOWN: usize = 12;
const CPU_FLOOR: f32 = 5.0;
/// Everything in a row except the name column: sidebar, paddings, the other columns and gaps.
const ROW_FIXED_WIDTH: f32 =
    216.0 + 48.0 + 36.0 + 10.0 + 24.0 + 48.0 + 126.0 * 2.0 + 68.0 * 2.0 + 60.0 + 8.0 * 6.0;
const MONO_CHAR_WIDTH: f32 = 7.0;

/// `Wrapping::None` is not applied by iced_graphics 0.13, so the command is cut to the
/// characters that fit the name column at the current window width.
pub fn command_chars(window_width: f32) -> usize {
    ((window_width - ROW_FIXED_WIDTH) / MONO_CHAR_WIDTH).max(12.0) as usize
}

struct Row<'a> {
    name: &'a str,
    state: State,
    command: String,
    usage: Option<&'a WorkloadUsage>,
}

impl Row<'_> {
    fn key(&self, column: Column) -> f64 {
        let Some(usage) = self.usage else {
            return -1.0;
        };
        match column {
            Column::Name => 0.0,
            Column::Processes => f64::from(usage.usage.processes),
            Column::Cpu => f64::from(usage.usage.cpu),
            Column::Memory => usage.usage.memory as f64,
            Column::Read => usage.usage.read_rate as f64,
            Column::Write => usage.usage.write_rate as f64,
            Column::Log => usage.log_bytes as f64,
        }
    }
}

fn rows<'a>(statuses: &'a [Status], metrics: Option<&'a Metrics>, sort: Sort) -> Vec<Row<'a>> {
    let mut rows: Vec<Row<'a>> = statuses
        .iter()
        .map(|status| Row {
            name: &status.workload.name,
            state: status.state,
            command: format::command_line(status),
            usage: metrics.and_then(|metrics| {
                metrics
                    .workloads
                    .iter()
                    .find(|usage| usage.name == status.workload.name)
            }),
        })
        .collect();
    rows.sort_by(|a, b| {
        let order = if sort.column == Column::Name {
            a.name.to_lowercase().cmp(&b.name.to_lowercase())
        } else {
            a.key(sort.column)
                .partial_cmp(&b.key(sort.column))
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| a.name.cmp(b.name))
        };
        if sort.descending {
            order.reverse()
        } else {
            order
        }
    });
    rows
}

pub fn view<'a>(
    statuses: &'a [Status],
    metrics: Option<&'a Metrics>,
    charts: &'a charts::Cache,
    sort: Sort,
    scale: f32,
    command_chars: usize,
) -> Element<'a, Message> {
    let running = metrics.map_or(0, |metrics| metrics.workloads.len());
    let heading = super::heading(
        "WORKLOADS",
        widgets::pill(
            format!("{running} running"),
            if running > 0 {
                Tone::Success
            } else {
                Tone::Neutral
            },
        ),
    );
    if statuses.is_empty() {
        return panel(
            heading,
            container(widgets::empty_state(glyph::RING, "No workloads", None)).height(140),
        );
    }
    let mut body = column![header(sort)].spacing(0);
    for entry in rows(statuses, metrics, sort) {
        body = body.push(widgets::divider());
        body = body.push(line(entry, metrics.is_none(), charts, scale, command_chars));
    }
    panel(heading, body)
}

fn header<'a>(sort: Sort) -> Element<'a, Message> {
    let cell = |label: &'a str, column: Column, width: Length, right: bool| {
        widgets::sort_header(
            label,
            sort.active(column),
            width,
            right,
            Message::SortHealth(column),
        )
    };
    container(
        row![
            cell("NAME", Column::Name, Fill, false),
            cell("PROCS", Column::Processes, PROCS, true),
            cell("CPU", Column::Cpu, SPARK, true),
            cell("MEMORY", Column::Memory, SPARK, true),
            cell("READ/S", Column::Read, RATE, true),
            cell("WRITE/S", Column::Write, RATE, true),
            cell("LOG", Column::Log, LOG, true),
        ]
        .spacing(theme::SPACE_SM)
        .align_y(Alignment::Center),
    )
    .padding([0, theme::SPACE_MD])
    .into()
}

fn line<'a>(
    entry: Row<'a>,
    loading: bool,
    charts: &'a charts::Cache,
    scale: f32,
    command_chars: usize,
) -> Element<'a, Message> {
    let identity = column![
        row![
            widgets::state_dot(entry.state, 9),
            text(entry.name).size(theme::TEXT_BODY),
        ]
        .spacing(theme::SPACE_SM)
        .align_y(Alignment::Center),
        widgets::clip(
            widgets::mono(
                format::elide_end(&entry.command, command_chars),
                theme::TEXT_CAPTION
            )
            .style(theme::text_faint)
        ),
    ]
    .spacing(3)
    .width(Fill);
    let cells: Vec<Element<'a, Message>> = match entry.usage {
        Some(usage) => vec![
            processes(usage),
            metric(
                format::percent(usage.usage.cpu),
                usage.cpu_history.clone(),
                CPU_FLOOR,
                scale,
                charts,
                charts::slot(("table", "cpu", entry.name)),
            ),
            metric(
                format::bytes(usage.usage.memory),
                usage.memory_history.iter().map(|m| *m as f32).collect(),
                usage.memory_history.iter().copied().max().unwrap_or(0) as f32 * 1.1,
                scale,
                charts,
                charts::slot(("table", "memory", entry.name)),
            ),
            widgets::cell(format::rate(usage.usage.read_rate), RATE),
            widgets::cell(format::rate(usage.usage.write_rate), RATE),
            widgets::cell(format::bytes(usage.log_bytes), LOG),
        ],
        None if loading => vec![
            blank(PROCS, 24),
            blank(SPARK, 80),
            blank(SPARK, 80),
            blank(RATE, 44),
            blank(RATE, 44),
            blank(LOG, 40),
        ],
        None => vec![
            widgets::cell_blank(PROCS),
            widgets::cell_blank(SPARK),
            widgets::cell_blank(SPARK),
            widgets::cell_blank(RATE),
            widgets::cell_blank(RATE),
            widgets::cell_blank(LOG),
        ],
    };
    let mut content = row![identity]
        .spacing(theme::SPACE_SM)
        .align_y(Alignment::Center);
    for cell in cells {
        content = content.push(cell);
    }
    widgets::table_row(content, Some(Message::Select(entry.name.to_owned())))
}

fn blank<'a>(width: Length, bar: u16) -> Element<'a, Message> {
    row![Space::with_width(Fill), widgets::skeleton(bar, 12)]
        .width(width)
        .into()
}

fn processes<'a>(usage: &'a WorkloadUsage) -> Element<'a, Message> {
    let mut pids: Vec<String> = usage
        .pids
        .iter()
        .take(PIDS_SHOWN)
        .map(u32::to_string)
        .collect();
    if usage.pids.len() > PIDS_SHOWN {
        pids.push(format!("+{}", usage.pids.len() - PIDS_SHOWN));
    }
    tooltip(
        widgets::cell(usage.usage.processes.to_string(), PROCS),
        container(widgets::mono(pids.join("  "), theme::TEXT_CAPTION)).padding([5, 8]),
        tooltip::Position::Top,
    )
    .style(theme::tooltip)
    .gap(4)
    .into()
}

fn metric<'a>(
    value: String,
    history: Vec<f32>,
    floor: f32,
    scale: f32,
    charts: &'a charts::Cache,
    slot: u64,
) -> Element<'a, Message> {
    row![
        widgets::cell(value, Fill),
        charts::sparkline(
            Sparkline::new(history, floor, Tone::Accent, scale, charts, slot),
            SPARK_WIDTH,
            SPARK_HEIGHT,
        ),
    ]
    .spacing(theme::SPACE_SM)
    .align_y(Alignment::Center)
    .width(SPARK)
    .into()
}
