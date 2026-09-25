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
    widget::{column, container, row, scrollable, text},
};
use k3up::model::Event;

/// `needle` is already trimmed and lowercased.
pub fn matches(event: &Event, needle: &str) -> bool {
    needle.is_empty()
        || event.name.to_lowercase().contains(needle)
        || event.message.to_lowercase().contains(needle)
}

pub fn view<'a>(events: &'a [Event], search: &'a str, now: DateTime<Utc>) -> Element<'a, Message> {
    let needle = search.trim().to_lowercase();
    let visible: Vec<&Event> = events
        .iter()
        .filter(|event| matches(event, &needle))
        .collect();
    let summary = widgets::pill(
        format!("{} events", format::count(visible.len())),
        Tone::Neutral,
    );
    let search_box = container(widgets::clip(widgets::input(
        "Filter",
        search,
        Message::ActivitySearch,
    )))
    .width(260);
    let body: Element<_> = if events.is_empty() {
        widgets::empty_state(glyph::RING, "No activity", None)
    } else if visible.is_empty() {
        widgets::empty_state(glyph::RING, "No matches", None)
    } else {
        let mut rows = column![];
        let mut previous_day = None;
        for event in visible {
            let day = event.at.date_naive();
            if previous_day != Some(day) {
                rows = rows.push(day_header(event.at));
                previous_day = Some(day);
            }
            rows = rows.push(entry(event, now));
            rows = rows.push(widgets::divider());
        }
        container(
            scrollable(container(rows).padding(iced::Padding {
                right: 10.0,
                ..Default::default()
            }))
            .height(Fill)
            .style(theme::scroll),
        )
        .padding([theme::SPACE_XS, 0])
        .width(Fill)
        .height(Fill)
        .style(theme::panel)
        .into()
    };
    column![
        widgets::page_header("Activity", summary, Some(search_box.into())),
        body,
    ]
    .spacing(theme::SPACE_XL)
    .height(Fill)
    .into()
}

fn day_header<'a>(at: DateTime<Utc>) -> Element<'a, Message> {
    container(widgets::caption(at.format("%A, %d %B %Y").to_string()))
        .padding(iced::Padding {
            top: f32::from(theme::SPACE_MD),
            right: f32::from(theme::SPACE_XL),
            bottom: f32::from(theme::SPACE_XS),
            left: f32::from(theme::SPACE_XL),
        })
        .width(Fill)
        .into()
}

fn entry<'a>(event: &'a Event, now: DateTime<Utc>) -> Element<'a, Message> {
    let tone = message_tone(&event.message);
    let content = row![
        column![
            widgets::mono(format::clock(event.at), theme::TEXT_META),
            widgets::faint(format::relative(event.at, now), theme::TEXT_CAPTION),
        ]
        .spacing(2)
        .width(96),
        container(widgets::toned(&event.name, theme::TEXT_META, Tone::Accent)).width(150),
        row![
            widgets::glyph(glyph::DOT, 7).style(theme::text_toned(tone)),
            text(&event.message).size(theme::TEXT_BODY).width(Fill),
        ]
        .spacing(theme::SPACE_SM)
        .align_y(Alignment::Center)
        .width(Fill),
    ]
    .spacing(theme::SPACE_LG)
    .align_y(Alignment::Center);
    container(content)
        .padding([theme::SPACE_SM + 2, theme::SPACE_XL])
        .width(Fill)
        .style(theme::list_row)
        .into()
}

fn message_tone(message: &str) -> Tone {
    let lower = message.to_lowercase();
    if lower.contains("exited with code 0") {
        Tone::Success
    } else if lower.contains("fail")
        || lower.contains("error")
        || lower.contains("exited with code")
    {
        Tone::Danger
    } else if lower.contains("retry")
        || lower.contains("timed out")
        || lower.contains("skipped")
        || lower.contains("limit")
    {
        Tone::Warning
    } else if lower.contains("started") || lower.contains("ready") || lower.contains("completed") {
        Tone::Success
    } else {
        Tone::Neutral
    }
}
