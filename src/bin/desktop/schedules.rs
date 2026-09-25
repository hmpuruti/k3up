use crate::{
    format,
    message::Message,
    theme::{self, Tone},
    widgets::{self, glyph},
};
use chrono::{DateTime, Utc};
use iced::{
    Alignment, Element,
    Length::Fill,
    widget::{Space, button, column, container, row, scrollable, text},
};
use k3up::model::{ScheduleAction, Status};

pub fn view<'a>(statuses: &'a [Status], now: DateTime<Utc>) -> Element<'a, Message> {
    let scheduled: Vec<&Status> = statuses
        .iter()
        .filter(|status| status.workload.schedule.is_some())
        .collect();
    let summary = widgets::pill(
        format!("{} scheduled", scheduled.len()),
        if scheduled.is_empty() {
            Tone::Neutral
        } else {
            Tone::Accent
        },
    );
    let body: Element<_> = if scheduled.is_empty() {
        widgets::empty_state(glyph::RING, "No schedules", None)
    } else {
        let mut items = column![].spacing(theme::SPACE_SM);
        for status in scheduled {
            items = items.push(card(status, now));
        }
        scrollable(container(items).padding(iced::Padding {
            right: 10.0,
            bottom: 4.0,
            ..Default::default()
        }))
        .height(Fill)
        .style(theme::scroll)
        .into()
    };
    column![widgets::page_header("Schedules", summary, None), body]
        .spacing(theme::SPACE_XL)
        .height(Fill)
        .into()
}

fn card<'a>(status: &'a Status, now: DateTime<Utc>) -> Element<'a, Message> {
    let schedule = status.workload.schedule.as_ref().unwrap();
    let cadence = format::cadence(schedule);
    let next = status
        .next_run
        .map(|at| format!("{} · {}", format::timestamp(at), format::relative(at, now)))
        .unwrap_or_else(|| "—".into());
    let action_tone = match schedule.action {
        ScheduleAction::Start => Tone::Accent,
        ScheduleAction::Restart => Tone::Warning,
    };
    let heading = row![
        widgets::state_dot(status.state, 10),
        text(&status.workload.name).size(theme::TEXT_SUBTITLE),
        widgets::kind_tag(status.workload.kind),
        Space::with_width(Fill),
        widgets::status_pill(status.state),
    ]
    .spacing(theme::SPACE_MD)
    .align_y(Alignment::Center);
    let cadence_row: Element<_> = if schedule.every_secs.is_some() {
        text(cadence).size(theme::TEXT_INPUT).into()
    } else {
        row![
            widgets::mono(cadence, theme::TEXT_INPUT),
            widgets::pill(&schedule.timezone, Tone::Neutral),
        ]
        .spacing(theme::SPACE_SM)
        .align_y(Alignment::Center)
        .into()
    };
    let facts = row![
        widgets::fact_text("NEXT RUN", next),
        widgets::fact(
            "ACTION",
            widgets::pill(format::action_label(schedule.action), action_tone)
        ),
        widgets::fact_text(
            "IF RUNNING",
            format::overlap_label(schedule.action).to_owned()
        ),
        widgets::fact_text(
            "IF MISSED",
            format::missed_label(schedule.missed).to_owned()
        ),
    ]
    .spacing(theme::SPACE_MD);
    button(
        column![heading, cadence_row, facts]
            .spacing(theme::SPACE_MD)
            .width(Fill),
    )
    .padding([theme::SPACE_LG, theme::SPACE_XL])
    .width(Fill)
    .style(theme::card(false))
    .on_press(Message::Select(status.workload.name.clone()))
    .into()
}
