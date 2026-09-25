use crate::{
    format,
    message::{Message, Page},
    theme::{self, Mode},
    widgets::{glyph, glyph::CHEVRON, glyph::DOT, glyph::RING},
};
use iced::{
    Alignment, Element, Font,
    Length::Fill,
    font::Weight,
    widget::{Space, button, column, container, horizontal_rule, image, row, text, tooltip},
};
use std::sync::LazyLock;

static MARK: LazyLock<image::Handle> = LazyLock::new(|| {
    image::Handle::from_bytes(
        include_bytes!("../../../assets/branding/k3-up-mark-on-dark.png").as_slice(),
    )
});

const WORDMARK: Font = Font {
    weight: Weight::Semibold,
    ..Font::DEFAULT
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Link {
    Connected,
    Starting,
    Offline,
}

pub struct Counts {
    pub workloads: usize,
    pub schedules: usize,
    pub attention: usize,
    pub health: usize,
}

pub fn view<'a>(
    page: Page,
    counts: Counts,
    link: Link,
    data_directory: &'a str,
    mode: Mode,
) -> Element<'a, Message> {
    container(
        column![
            brand(),
            Space::with_height(theme::SPACE_2XL),
            nav_item(
                page,
                Page::Workloads,
                glyph::GRID,
                "Workloads",
                if counts.attention > 0 {
                    badge(counts.attention, true)
                } else {
                    badge(counts.workloads, false)
                }
            ),
            nav_item(
                page,
                Page::Schedules,
                glyph::CLOCK,
                "Schedules",
                badge(counts.schedules, false)
            ),
            nav_item(page, Page::Activity, glyph::LINES, "Activity", None),
            nav_item(
                page,
                Page::Health,
                glyph::PULSE,
                "Health",
                badge(counts.health, true)
            ),
            Space::with_height(Fill),
            horizontal_rule(1).style(theme::rail_divider),
            connection(link, data_directory),
            mode_switch(mode),
        ]
        .spacing(theme::SPACE_XS)
        .height(Fill),
    )
    .padding([theme::SPACE_XL, theme::SPACE_LG])
    .width(theme::SIDEBAR_WIDTH)
    .height(Fill)
    .style(theme::rail)
    .into()
}

fn brand<'a>() -> Element<'a, Message> {
    row![
        image(MARK.clone()).width(30).height(30),
        column![
            row![
                text("K3")
                    .size(16)
                    .font(WORDMARK)
                    .style(theme::text_rail_ink),
                text("Up")
                    .size(16)
                    .font(WORDMARK)
                    .style(theme::text_rail_accent),
            ]
            .spacing(theme::SPACE_XS),
            text("CONTROL CONSOLE")
                .size(9)
                .style(theme::text_rail_muted),
        ]
        .spacing(2),
    ]
    .spacing(theme::SPACE_MD)
    .align_y(Alignment::Center)
    .into()
}

fn nav_item<'a>(
    current: Page,
    page: Page,
    symbol: &'a str,
    label: &'a str,
    badge: Option<Element<'a, Message>>,
) -> Element<'a, Message> {
    let active = current == page;
    let mut content = row![
        crate::widgets::glyph(symbol, 13).width(16).center(),
        text(label).size(theme::TEXT_BODY + 1),
    ]
    .spacing(theme::SPACE_SM + 2)
    .align_y(Alignment::Center);
    content = content.push(Space::with_width(Fill));
    if let Some(badge) = badge {
        content = content.push(badge);
    }
    button(content)
        .width(Fill)
        .padding([9, 12])
        .style(theme::nav(active))
        .on_press(Message::Navigate(page))
        .into()
}

fn badge<'a>(count: usize, alert: bool) -> Option<Element<'a, Message>> {
    if count == 0 {
        return None;
    }
    Some(
        container(text(format::count(count)).size(10).line_height(1.0))
            .padding([3, 7])
            .style(theme::rail_badge(alert))
            .into(),
    )
}

fn connection<'a>(link: Link, data_directory: &'a str) -> Element<'a, Message> {
    let (symbol, label) = match link {
        Link::Connected => (DOT, "Connected"),
        Link::Starting => (RING, "Starting"),
        Link::Offline => (RING, "Offline"),
    };
    let symbol = crate::widgets::glyph(symbol, 9);
    let symbol = if link == Link::Starting {
        symbol.style(theme::text_rail_muted)
    } else {
        symbol.style(theme::text_connection(link == Link::Connected))
    };
    let status = row![
        symbol,
        text(label).size(theme::TEXT_META),
        Space::with_width(Fill),
        crate::widgets::glyph(CHEVRON, 14),
    ]
    .spacing(theme::SPACE_SM)
    .align_y(Alignment::Center);
    let target = button(
        column![
            status,
            text(format::elide_start(data_directory, 26))
                .size(theme::TEXT_CAPTION)
                .font(Font::MONOSPACE)
                .style(theme::text_rail_muted),
        ]
        .spacing(theme::SPACE_XS + 2),
    )
    .width(Fill)
    .padding([theme::SPACE_SM + 2, theme::SPACE_MD])
    .style(theme::rail_button)
    .on_press(Message::ToggleConnection);
    container(
        tooltip(
            target,
            container(
                text(data_directory)
                    .size(theme::TEXT_CAPTION)
                    .font(Font::MONOSPACE),
            )
            .padding([6, 8]),
            tooltip::Position::Top,
        )
        .style(theme::tooltip)
        .gap(6),
    )
    .padding([theme::SPACE_MD, 0])
    .width(Fill)
    .into()
}

fn mode_switch<'a>(mode: Mode) -> Element<'a, Message> {
    let segment = |label: &'a str, value: Mode| {
        button(text(label).size(theme::TEXT_META).center())
            .width(Fill)
            .padding([6, 0])
            .style(theme::rail_segment(mode == value))
            .on_press(Message::Mode(value))
    };
    container(
        row![segment("Light", Mode::Light), segment("Dark", Mode::Dark)]
            .spacing(2)
            .width(Fill),
    )
    .padding(3)
    .width(Fill)
    .style(theme::rail_well)
    .into()
}
