use crate::{
    format,
    message::Message,
    theme::{self, Tone},
};
use iced::{
    Alignment, Element, Font, Length,
    Length::Fill,
    alignment::Horizontal,
    widget::{Space, button, column, container, horizontal_rule, row, text, text_input, tooltip},
};
use k3up::model::{Kind, State};

pub mod glyph {
    pub const DOT: &str = "●";
    pub const RING: &str = "○";
    pub const PLAY: &str = "▶";
    pub const STOP: &str = "■";
    pub const RESTART: &str = "↻";
    pub const PLUS: &str = "+";
    pub const CLOSE: &str = "×";
    pub const CHEVRON: &str = "›";
    pub const BACK: &str = "‹";
    pub const ASC: &str = "↑";
    pub const DESC: &str = "↓";
    pub const GRID: &str = "▣";
    pub const CLOCK: &str = "◷";
    pub const LINES: &str = "≡";
    pub const PULSE: &str = "∿";
}

pub fn glyph<'a>(symbol: &'a str, size: u16) -> iced::widget::Text<'a> {
    text(symbol)
        .size(size)
        .shaping(text::Shaping::Advanced)
        .line_height(1.0)
}

pub fn caption<'a>(label: impl text::IntoFragment<'a>) -> Element<'a, Message> {
    text(label)
        .size(theme::TEXT_CAPTION)
        .style(theme::text_faint)
        .into()
}

pub fn muted<'a>(label: impl text::IntoFragment<'a>, size: u16) -> iced::widget::Text<'a> {
    text(label).size(size).style(theme::text_muted)
}

pub fn faint<'a>(label: impl text::IntoFragment<'a>, size: u16) -> iced::widget::Text<'a> {
    text(label).size(size).style(theme::text_faint)
}

pub fn toned<'a>(
    label: impl text::IntoFragment<'a>,
    size: u16,
    tone: Tone,
) -> iced::widget::Text<'a> {
    text(label).size(size).style(theme::text_toned(tone))
}

pub fn mono<'a>(label: impl text::IntoFragment<'a>, size: u16) -> iced::widget::Text<'a> {
    text(label).size(size).font(Font::MONOSPACE)
}

pub fn title<'a>(label: impl text::IntoFragment<'a>) -> iced::widget::Text<'a> {
    text(label).size(theme::TEXT_TITLE)
}

pub fn pill<'a>(label: impl text::IntoFragment<'a>, tone: Tone) -> Element<'a, Message> {
    container(text(label).size(theme::TEXT_CAPTION).line_height(1.0))
        .padding([4, 9])
        .style(theme::pill(tone))
        .into()
}

pub fn status_pill<'a>(state: State) -> Element<'a, Message> {
    let tone = format::state_tone(state);
    container(
        row![
            glyph(glyph::DOT, 8),
            text(format::state_label(state))
                .size(theme::TEXT_CAPTION)
                .line_height(1.0),
        ]
        .spacing(theme::SPACE_XS + 1)
        .align_y(Alignment::Center),
    )
    .padding([4, 9])
    .style(theme::pill(tone))
    .into()
}

pub fn kind_tag<'a>(kind: Kind) -> Element<'a, Message> {
    container(
        text(format::kind_label(kind).to_uppercase())
            .size(theme::TEXT_CAPTION - 1)
            .line_height(1.0),
    )
    .padding([3, 6])
    .style(theme::tag)
    .into()
}

pub fn state_dot<'a>(state: State, size: u16) -> Element<'a, Message> {
    glyph(glyph::DOT, size)
        .style(theme::text_toned(format::state_tone(state)))
        .into()
}

pub fn primary<'a>(label: &'a str, message: Option<Message>) -> Element<'a, Message> {
    button(text(label).size(theme::TEXT_BODY))
        .padding([9, 14])
        .style(theme::primary)
        .on_press_maybe(message)
        .into()
}

pub fn secondary<'a>(label: &'a str, message: Option<Message>) -> Element<'a, Message> {
    button(text(label).size(theme::TEXT_BODY))
        .padding([8, 13])
        .style(theme::secondary)
        .on_press_maybe(message)
        .into()
}

pub fn danger<'a>(label: &'a str, message: Option<Message>) -> Element<'a, Message> {
    button(text(label).size(theme::TEXT_BODY))
        .padding([9, 14])
        .style(theme::danger)
        .on_press_maybe(message)
        .into()
}

pub fn danger_outline<'a>(label: &'a str, message: Option<Message>) -> Element<'a, Message> {
    button(text(label).size(theme::TEXT_BODY))
        .padding([8, 13])
        .style(theme::danger_outline)
        .on_press_maybe(message)
        .into()
}

pub fn ghost<'a>(label: &'a str, message: Option<Message>) -> Element<'a, Message> {
    button(text(label).size(theme::TEXT_META))
        .padding([6, 10])
        .style(theme::ghost)
        .on_press_maybe(message)
        .into()
}

pub fn action<'a>(
    symbol: &'a str,
    label: &'a str,
    message: Option<Message>,
    style: fn(&iced::Theme, button::Status) -> button::Style,
) -> Element<'a, Message> {
    button(
        row![glyph(symbol, 10), text(label).size(theme::TEXT_BODY)]
            .spacing(theme::SPACE_SM)
            .align_y(Alignment::Center),
    )
    .padding([8, 13])
    .style(style)
    .on_press_maybe(message)
    .into()
}

pub fn icon_button<'a>(
    symbol: &'a str,
    tip: &'a str,
    message: Option<Message>,
) -> Element<'a, Message> {
    let control = button(glyph(symbol, 14).center().width(18))
        .padding(5)
        .style(theme::ghost)
        .on_press_maybe(message);
    tooltip(
        control,
        container(text(tip).size(theme::TEXT_CAPTION)).padding([4, 8]),
        tooltip::Position::Bottom,
    )
    .style(theme::tooltip)
    .gap(4)
    .into()
}

pub fn segmented<'a, T: Copy + PartialEq + 'a>(
    options: &'a [(&'a str, T)],
    selected: T,
    on_select: impl Fn(T) -> Message + 'a,
) -> Element<'a, Message> {
    let mut items = row![].spacing(2);
    for (label, value) in options {
        let active = *value == selected;
        items = items.push(
            button(text(*label).size(theme::TEXT_META).center())
                .padding([6, 12])
                .style(theme::segment(active))
                .on_press(on_select(*value)),
        );
    }
    container(items).padding(3).style(theme::well).into()
}

pub fn field<'a>(label: &'a str, control: impl Into<Element<'a, Message>>) -> Element<'a, Message> {
    column![caption(label), control.into()]
        .spacing(theme::SPACE_XS + 2)
        .into()
}

pub fn input<'a>(
    placeholder: &'a str,
    value: &'a str,
    on_input: impl Fn(String) -> Message + 'a,
) -> iced::widget::TextInput<'a, Message> {
    text_input(placeholder, value)
        .on_input(on_input)
        .padding([9, 11])
        .size(theme::TEXT_INPUT)
        .style(theme::input)
}

pub fn clip<'a>(control: impl Into<Element<'a, Message>>) -> Element<'a, Message> {
    crate::clip::clip(control).into()
}

pub fn readonly<'a>(value: &'a str) -> iced::widget::TextInput<'a, Message> {
    text_input("", value)
        .padding([9, 11])
        .size(theme::TEXT_INPUT)
        .style(theme::input)
}

pub fn fact<'a>(label: &'a str, value: impl Into<Element<'a, Message>>) -> Element<'a, Message> {
    column![caption(label), value.into()]
        .spacing(theme::SPACE_XS)
        .width(Fill)
        .into()
}

pub fn fact_text<'a>(label: &'a str, value: String) -> Element<'a, Message> {
    fact(label, text(value).size(theme::TEXT_BODY))
}

pub fn divider<'a>() -> Element<'a, Message> {
    horizontal_rule(1).style(theme::divider).into()
}

pub fn section<'a>(
    heading: &'a str,
    content: impl Into<Element<'a, Message>>,
) -> Element<'a, Message> {
    container(
        column![
            text(heading).size(theme::TEXT_SUBTITLE),
            divider(),
            content.into()
        ]
        .spacing(theme::SPACE_LG),
    )
    .padding(theme::SPACE_XL)
    .width(Fill)
    .style(theme::panel)
    .into()
}

pub fn empty_state<'a>(
    symbol: &'a str,
    heading: &'a str,
    extra: Option<Element<'a, Message>>,
) -> Element<'a, Message> {
    let mut content = column![
        glyph(symbol, 30).style(theme::text_faint),
        muted(heading, theme::TEXT_SUBTITLE),
    ]
    .spacing(theme::SPACE_MD)
    .align_x(Alignment::Center);
    if let Some(extra) = extra {
        content = content.push(Space::with_height(theme::SPACE_XS));
        content = content.push(extra);
    }
    container(content)
        .width(Fill)
        .height(Fill)
        .center(Fill)
        .into()
}

pub fn page_header<'a>(
    heading: &'a str,
    summary: Element<'a, Message>,
    trailing: Option<Element<'a, Message>>,
) -> Element<'a, Message> {
    let mut bar = row![title(heading), summary]
        .spacing(theme::SPACE_MD)
        .align_y(Alignment::Center);
    bar = bar.push(Space::with_width(Fill));
    if let Some(trailing) = trailing {
        bar = bar.push(trailing);
    }
    bar.into()
}

pub fn value<'a>(label: impl text::IntoFragment<'a>, tone: Tone) -> iced::widget::Text<'a> {
    let value = text(label).size(theme::TEXT_SECTION + 2).line_height(1.1);
    if tone == Tone::Neutral {
        value
    } else {
        value.style(theme::text_toned(tone))
    }
}

pub fn stat_tile<'a>(
    label: &'a str,
    value: Element<'a, Message>,
    gauge: Option<Element<'a, Message>>,
    meta: Element<'a, Message>,
    tone: Tone,
) -> Element<'a, Message> {
    let mut heading = row![caption(label)].align_y(Alignment::Center);
    if tone != Tone::Neutral {
        heading = heading.push(Space::with_width(Fill));
        heading = heading.push(glyph(glyph::DOT, 8).style(theme::text_toned(tone)));
    }
    let mut content = column![heading, value].spacing(theme::SPACE_SM);
    if let Some(gauge) = gauge {
        content = content.push(gauge);
    }
    content = content.push(meta);
    container(content.width(Fill))
        .padding([theme::SPACE_LG, theme::SPACE_LG + 2])
        .width(Fill)
        .style(theme::stat_tile(tone))
        .into()
}

pub fn meta_row<'a>(parts: Vec<String>) -> Element<'a, Message> {
    let mut items = row![].spacing(theme::SPACE_SM).align_y(Alignment::Center);
    for (index, part) in parts.into_iter().enumerate() {
        if index > 0 {
            items = items.push(faint("·", theme::TEXT_META));
        }
        items = items.push(muted(part, theme::TEXT_META));
    }
    items.wrap().into()
}

pub fn meta_lines<'a>(lines: Vec<String>) -> Element<'a, Message> {
    let mut items = column![].spacing(theme::SPACE_XS);
    for line in lines {
        items = items.push(muted(line, theme::TEXT_META));
    }
    items.into()
}

pub fn skeleton<'a>(width: impl Into<Length>, height: impl Into<Length>) -> Element<'a, Message> {
    container(Space::new(width, height))
        .style(theme::skeleton)
        .into()
}

pub fn legend<'a>(label: &'a str, value: String, tone: Tone) -> Element<'a, Message> {
    row![
        glyph(glyph::DOT, 8).style(theme::text_toned(tone)),
        muted(label, theme::TEXT_META),
        text(value).size(theme::TEXT_META),
    ]
    .spacing(theme::SPACE_XS + 1)
    .align_y(Alignment::Center)
    .into()
}

pub fn sort_header<'a>(
    label: &'a str,
    active: Option<bool>,
    width: impl Into<Length>,
    right: bool,
    on_press: Message,
) -> Element<'a, Message> {
    let mut content = row![].spacing(theme::SPACE_XS).align_y(Alignment::Center);
    if right {
        content = content.push(Space::with_width(Fill));
    }
    content = content.push(text(label).size(theme::TEXT_CAPTION - 1).line_height(1.0));
    if let Some(descending) = active {
        content = content.push(glyph(if descending { glyph::DESC } else { glyph::ASC }, 9));
    }
    if !right {
        content = content.push(Space::with_width(Fill));
    }
    button(content)
        .width(width)
        .padding([theme::SPACE_XS + 1, 0])
        .style(theme::table_header(active.is_some()))
        .on_press(on_press)
        .into()
}

pub fn cell<'a>(
    value: impl text::IntoFragment<'a>,
    width: impl Into<Length>,
) -> Element<'a, Message> {
    text(value)
        .size(theme::TEXT_META)
        .width(width)
        .align_x(Horizontal::Right)
        .into()
}

pub fn cell_blank<'a>(width: impl Into<Length>) -> Element<'a, Message> {
    faint("—", theme::TEXT_META)
        .width(width)
        .align_x(Horizontal::Right)
        .into()
}

pub fn table_row<'a>(
    content: impl Into<Element<'a, Message>>,
    on_press: Option<Message>,
) -> Element<'a, Message> {
    button(content)
        .width(Fill)
        .padding([theme::SPACE_SM + 2, theme::SPACE_MD])
        .style(theme::table_row)
        .on_press_maybe(on_press)
        .into()
}
