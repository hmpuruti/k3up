use crate::{
    format,
    message::{Filter, Message},
    theme::{self, Tone},
    tree::{self, Row},
    widgets::{self, glyph},
};
use chrono::{DateTime, Utc};
use iced::{
    Alignment, Element,
    Length::Fill,
    widget::{Space, button, column, container, row, scrollable, text},
};
use k3up::{
    group::{self, Folder},
    model::{State, Status},
};
use std::collections::BTreeSet;

pub struct Context<'a> {
    pub statuses: &'a [Status],
    pub filter: Filter,
    pub search: &'a str,
    pub selected: Option<&'a str>,
    pub collapsed: &'a BTreeSet<String>,
    pub confirming_stop: Option<&'a str>,
    pub ready: bool,
    pub now: DateTime<Utc>,
}

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
            || status.workload.description.to_lowercase().contains(&needle)
            || status.workload.group.to_lowercase().contains(&needle))
}

pub fn view(ctx: Context<'_>) -> Element<'_, Message> {
    let visible: Vec<&Status> = ctx
        .statuses
        .iter()
        .filter(|status| matches(status, ctx.filter, ctx.search))
        .collect();
    let body: Element<_> = if ctx.statuses.is_empty() {
        widgets::empty_state(glyph::RING, "No workloads", None)
    } else if visible.is_empty() {
        widgets::empty_state(glyph::RING, "No matches", None)
    } else {
        let tree = group::tree(&visible);
        let mut items = column![].spacing(theme::SPACE_SM);
        for entry in tree::rows(&tree, ctx.collapsed) {
            match entry {
                Row::Folder { folder, collapsed } => {
                    items = items.push(folder_row(folder, collapsed, ctx.ready));
                    if ctx
                        .confirming_stop
                        .is_some_and(|path| group::same(path, &folder.path))
                    {
                        items = items.push(indented(folder.depth, stop_prompt(folder, ctx.ready)));
                    }
                }
                Row::Workload { index, depth } => {
                    let status = visible[index];
                    let selected = ctx.selected == Some(status.workload.name.as_str());
                    items = items.push(indented(depth, card(status, selected, ctx.now)));
                }
            }
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
        widgets::clip(widgets::input("Search", ctx.search, Message::Search)),
        filters(ctx.statuses, ctx.filter),
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

fn indented<'a>(depth: usize, content: impl Into<Element<'a, Message>>) -> Element<'a, Message> {
    row![
        Space::with_width(depth as f32 * tree::INDENT),
        content.into()
    ]
    .into()
}

/// The folder is read, not borrowed, so the row outlives the tree it came from.
fn folder_row<'a>(folder: &Folder, collapsed: bool, ready: bool) -> Element<'a, Message> {
    let path = folder.path.clone();
    let symbol = if collapsed {
        glyph::FOLDED
    } else {
        glyph::UNFOLDED
    };
    let toggle = button(
        row![
            widgets::glyph(symbol, 14).width(12).center(),
            text(folder.name().to_owned()).size(theme::TEXT_BODY),
            widgets::faint(folder.workloads().to_string(), theme::TEXT_META),
            Space::with_width(Fill),
            widgets::glyph(glyph::DOT, 8).style(theme::text_toned(tree::tone(folder))),
        ]
        .spacing(theme::SPACE_SM)
        .align_y(Alignment::Center),
    )
    .width(Fill)
    .padding([6, 8])
    .style(theme::folder)
    .on_press(Message::ToggleFolder(group::key(&folder.path)));
    let actions = row![
        widgets::icon_button(
            glyph::PLAY,
            "Start all",
            ready.then(|| Message::StartFolder(path.clone())),
        ),
        widgets::icon_button(
            glyph::STOP,
            "Stop all",
            ready.then_some(Message::StopFolder(path)),
        ),
    ];
    indented(
        folder.depth,
        row![toggle, actions]
            .spacing(theme::SPACE_XS)
            .align_y(Alignment::Center),
    )
}

fn stop_prompt<'a>(folder: &Folder, ready: bool) -> Element<'a, Message> {
    let count = folder.workloads();
    let noun = if count == 1 { "workload" } else { "workloads" };
    container(
        column![
            widgets::toned(
                format!("Stop {count} {noun} in {}?", folder.name()),
                theme::TEXT_META,
                Tone::Danger,
            ),
            row![
                Space::with_width(Fill),
                widgets::secondary("Keep", Some(Message::CancelStopFolder)),
                widgets::danger("Stop", ready.then_some(Message::ConfirmStopFolder)),
            ]
            .spacing(theme::SPACE_SM),
        ]
        .spacing(theme::SPACE_SM),
    )
    .padding([theme::SPACE_SM, theme::SPACE_MD])
    .width(Fill)
    .style(theme::banner(Tone::Danger))
    .into()
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
            status.restart_count,
            format::restart_limit(status.workload.max_restarts)
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
