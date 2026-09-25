use crate::{
    format,
    message::{Filter, Message},
    theme,
    widgets::{self, glyph},
};
use chrono::{DateTime, Utc};
use iced::{
    Alignment, Element,
    Length::Fill,
    widget::{button, column, container, row, scrollable, text},
};
use k3up::model::{State, Status};

pub fn matches(status: &Status, filter: Filter, search: &str) -> bool {
    let by_filter = match filter {
        Filter::All => true,
        Filter::Running => matches!(status.state, State::Running | State::Starting),
        Filter::Attention => format::needs_attention(status.state),
        Filter::Stopped => matches!(status.state, State::Stopped | State::Completed),
    };
    let needle = search.trim().to_lowercase();
    by_filter
        && (needle.is_empty()
            || status.workload.name.to_lowercase().contains(&needle)
            || status.workload.description.to_lowercase().contains(&needle))
}

pub fn view<'a>(
    statuses: &'a [Status],
    filter: Filter,
    search: &'a str,
    selected: Option<&'a str>,
    now: DateTime<Utc>,
) -> Element<'a, Message> {
    let visible: Vec<&Status> = statuses
        .iter()
        .filter(|status| matches(status, filter, search))
        .collect();
    let body: Element<_> = if statuses.is_empty() {
        widgets::empty_state(glyph::RING, "No workloads", None)
    } else if visible.is_empty() {
        widgets::empty_state(glyph::RING, "No matches", None)
    } else {
        let mut items = column![].spacing(theme::SPACE_SM);
        for status in visible {
            items = items.push(card(
                status,
                selected == Some(status.workload.name.as_str()),
                now,
            ));
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
    column![
        widgets::clip(widgets::input("Search", search, Message::Search)),
        filters(statuses, filter),
        body,
    ]
    .spacing(theme::SPACE_MD)
    .width(theme::LIST_WIDTH)
    .height(Fill)
    .into()
}

fn filters<'a>(statuses: &'a [Status], active: Filter) -> Element<'a, Message> {
    let counts = |filter: Filter| statuses.iter().filter(|s| matches(s, filter, "")).count();
    let mut chips = row![].spacing(theme::SPACE_XS);
    for (label, filter) in [
        ("All", Filter::All),
        ("Running", Filter::Running),
        ("Attention", Filter::Attention),
        ("Stopped", Filter::Stopped),
    ] {
        let count = counts(filter);
        let content = row![text(label).size(theme::TEXT_META)]
            .spacing(theme::SPACE_XS + 1)
            .align_y(Alignment::Center)
            .push_maybe(
                (count > 0 && filter != Filter::All)
                    .then(|| text(count.to_string()).size(theme::TEXT_CAPTION)),
            );
        chips = chips.push(
            button(content)
                .padding([5, 10])
                .style(theme::chip(filter == active))
                .on_press(Message::Filter(filter)),
        );
    }
    chips.into()
}

fn card<'a>(status: &'a Status, selected: bool, now: DateTime<Utc>) -> Element<'a, Message> {
    let name = &status.workload.name;
    let meta = card_meta(status, now);
    let content = row![
        widgets::state_dot(status.state, 10),
        column![
            row![
                text(name).size(theme::TEXT_INPUT).width(Fill),
                widgets::kind_tag(status.workload.kind),
            ]
            .spacing(theme::SPACE_SM)
            .align_y(Alignment::Center),
            row![
                widgets::toned(
                    format::state_label(status.state),
                    theme::TEXT_META,
                    format::state_tone(status.state)
                ),
                widgets::faint(meta, theme::TEXT_META),
            ]
            .spacing(theme::SPACE_MD)
            .align_y(Alignment::Center),
        ]
        .spacing(theme::SPACE_XS + 1)
        .width(Fill),
    ]
    .spacing(theme::SPACE_MD)
    .align_y(Alignment::Center);
    button(content)
        .padding([theme::SPACE_MD, theme::SPACE_LG - 2])
        .width(Fill)
        .style(theme::card(selected))
        .on_press(Message::Select(name.clone()))
        .into()
}

fn card_meta(status: &Status, now: DateTime<Utc>) -> String {
    match status.state {
        State::Running | State::Starting => status
            .started_at
            .map(|at| {
                format!(
                    "up {}",
                    format::duration_coarse((now - at).num_seconds().max(0) as u64)
                )
            })
            .unwrap_or_default(),
        State::Backoff => format!(
            "retry {}/{}",
            status.restart_count, status.workload.max_restarts
        ),
        State::Failed => status
            .last_exit
            .map(|code| format!("exit {code}"))
            .unwrap_or_default(),
        _ => status
            .next_run
            .map(|next| format!("next {}", format::relative(next, now)))
            .unwrap_or_default(),
    }
}

pub fn placeholder<'a>() -> Element<'a, Message> {
    container(widgets::empty_state(
        glyph::CHEVRON,
        "Select a workload",
        None,
    ))
    .width(Fill)
    .height(Fill)
    .style(theme::panel_flat)
    .into()
}
