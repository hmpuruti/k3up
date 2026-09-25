mod impact;
mod table;
mod trends;
mod vitals;

use crate::{
    charts, format,
    message::Message,
    theme::{self, Tone},
    widgets,
};
use iced::{
    Alignment, Element,
    Length::Fill,
    widget::{Space, column, container, row, scrollable},
};
use k3up::{
    metrics::{Metrics, Usage},
    model::Status,
};

pub use vitals::warnings;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Column {
    Name,
    Processes,
    Cpu,
    Memory,
    Read,
    Write,
    Log,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Sort {
    pub column: Column,
    pub descending: bool,
}

impl Default for Sort {
    fn default() -> Self {
        Self {
            column: Column::Cpu,
            descending: true,
        }
    }
}

impl Sort {
    pub fn toggle(self, column: Column) -> Self {
        if self.column == column {
            Self {
                column,
                descending: !self.descending,
            }
        } else {
            Self {
                column,
                descending: column != Column::Name,
            }
        }
    }

    fn active(self, column: Column) -> Option<bool> {
        (self.column == column).then_some(self.descending)
    }
}

pub struct Context<'a> {
    pub metrics: Option<&'a Metrics>,
    pub desktop: Usage,
    pub statuses: &'a [Status],
    pub charts: &'a charts::Cache,
    pub sort: Sort,
    pub scale: f32,
    pub width: f32,
}

pub fn view(ctx: Context<'_>) -> Element<'_, Message> {
    let warnings = ctx.metrics.map_or(0, warnings);
    let summary = match ctx.metrics {
        None => widgets::pill("Sampling", Tone::Neutral),
        Some(_) if warnings == 0 => widgets::pill("Healthy", Tone::Success),
        Some(_) => widgets::pill(format!("{warnings} warnings"), Tone::Warning),
    };
    let trailing = ctx.metrics.map(|metrics| {
        let machine = &metrics.machine;
        widgets::meta_row(vec![
            machine.host.clone(),
            machine.os.clone(),
            format!("up {}", format::duration_coarse(machine.uptime_secs)),
        ])
    });
    let body = column![
        vitals::view(ctx.metrics, ctx.scale),
        trends::view(ctx.metrics, ctx.charts, ctx.scale),
        impact::view(ctx.metrics, ctx.desktop),
        table::view(
            ctx.statuses,
            ctx.metrics,
            ctx.charts,
            ctx.sort,
            ctx.scale,
            table::command_chars(ctx.width),
        ),
    ]
    .spacing(theme::SPACE_LG);
    column![
        widgets::page_header("Health", summary, trailing),
        scrollable(container(body).padding(iced::Padding {
            right: 10.0,
            bottom: 4.0,
            ..Default::default()
        }))
        .height(Fill)
        .style(theme::scroll),
    ]
    .spacing(theme::SPACE_XL)
    .height(Fill)
    .into()
}

fn panel<'a>(
    heading: Element<'a, Message>,
    content: impl Into<Element<'a, Message>>,
) -> Element<'a, Message> {
    container(
        column![heading, content.into()]
            .spacing(theme::SPACE_MD)
            .width(Fill),
    )
    .padding([theme::SPACE_LG, theme::SPACE_LG + 2])
    .width(Fill)
    .style(theme::panel)
    .into()
}

fn heading<'a>(label: &'a str, trailing: Element<'a, Message>) -> Element<'a, Message> {
    row![widgets::caption(label), Space::with_width(Fill), trailing,]
        .spacing(theme::SPACE_MD)
        .align_y(Alignment::Center)
        .into()
}
