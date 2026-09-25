use crate::{
    format,
    logs::{LogBuffer, MAX_LINES},
    message::Message,
    theme,
    widgets::{self, glyph},
};
use iced::{
    Alignment, Element, Font,
    Length::Fill,
    widget::{Space, column, container, row, scrollable, text},
};

pub fn view<'a>(logs: &'a LogBuffer, live: bool) -> Element<'a, Message> {
    let mut header = row![
        text("OUTPUT").size(theme::TEXT_CAPTION - 1),
        Space::with_width(Fill),
    ]
    .spacing(theme::SPACE_SM)
    .align_y(Alignment::Center);
    if live {
        header = header.push(
            row![
                widgets::glyph(glyph::DOT, 8).style(theme::text_toned(theme::Tone::Success)),
                text("LIVE").size(theme::TEXT_CAPTION - 1),
            ]
            .spacing(theme::SPACE_XS)
            .align_y(Alignment::Center),
        );
    }
    header = header.push(text(lines_label(logs.len())).size(theme::TEXT_CAPTION - 1));
    header = header.push(widgets::icon_button(
        glyph::CLOSE,
        "Clear",
        (!logs.is_empty()).then_some(Message::ClearOutput),
    ));
    let body: Element<_> = if logs.is_empty() {
        container(
            text("No output")
                .size(theme::TEXT_META)
                .style(theme::text_console_muted),
        )
        .padding(theme::SPACE_LG)
        .width(Fill)
        .height(Fill)
        .center(Fill)
        .into()
    } else {
        scrollable(
            container(
                text(logs.text())
                    .font(Font::MONOSPACE)
                    .size(theme::TEXT_META)
                    .line_height(1.5)
                    .wrapping(text::Wrapping::WordOrGlyph)
                    .style(theme::text_console_ink),
            )
            .padding([theme::SPACE_SM, theme::SPACE_MD])
            .width(Fill),
        )
        .anchor_bottom()
        .height(Fill)
        .width(Fill)
        .style(theme::console_scroll)
        .into()
    };
    container(
        column![
            container(header)
                .padding([theme::SPACE_XS + 2, theme::SPACE_MD])
                .width(Fill)
                .style(theme::console_header),
            body,
        ]
        .height(Fill),
    )
    .width(Fill)
    .height(Fill)
    .style(theme::console)
    .into()
}

fn lines_label(count: usize) -> String {
    if count >= MAX_LINES {
        format!("LAST {}", format::count(MAX_LINES))
    } else {
        format!("{} LINES", format::count(count))
    }
}
