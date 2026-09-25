use crate::{
    message::Message,
    sidebar::Link,
    theme::{self, Tone},
    widgets::{self, glyph::CLOSE},
};
use iced::{
    Alignment, Element,
    Length::Fill,
    widget::{Space, column, container, row, text, text_input, toggler},
};

pub struct Context<'a> {
    pub data_directory: &'a str,
    pub link: Link,
    pub problem: Option<&'a str>,
    pub can_connect: bool,
    pub dismissible: bool,
    /// `None` when this directory's agent is not managed by the app.
    pub login_enabled: Option<bool>,
}

pub fn view(context: Context<'_>) -> Element<'_, Message> {
    let mut header = row![
        widgets::caption("AGENT"),
        Space::with_width(Fill),
        match context.link {
            Link::Connected => widgets::pill("Connected", Tone::Success),
            Link::Starting => widgets::pill("Starting", Tone::Neutral),
            Link::Offline => widgets::pill("Offline", Tone::Warning),
        },
    ]
    .spacing(theme::SPACE_SM)
    .align_y(Alignment::Center);
    if context.dismissible {
        header = header.push(widgets::icon_button(
            CLOSE,
            "Close",
            Some(Message::ToggleConnection),
        ));
    }
    let input = text_input("Agent data directory", context.data_directory)
        .on_input(Message::DataDirectory)
        .on_submit(Message::Connect)
        .padding([9, 11])
        .size(theme::TEXT_INPUT)
        .font(iced::Font::MONOSPACE)
        .style(theme::input);
    let mut body = column![
        header,
        row![
            container(widgets::clip(input)).width(Fill),
            widgets::primary("Connect", context.can_connect.then_some(Message::Connect)),
        ]
        .spacing(theme::SPACE_SM)
        .align_y(Alignment::Center),
    ]
    .spacing(theme::SPACE_MD);
    if let Some(enabled) = context.login_enabled {
        body = body.push(
            row![
                text("Start at login").size(theme::TEXT_BODY),
                Space::with_width(Fill),
                toggler(enabled)
                    .on_toggle(Message::LoginItem)
                    .size(22)
                    .style(theme::switch),
            ]
            .align_y(Alignment::Center),
        );
    }
    if let Some(problem) = context.problem
        && context.link == Link::Offline
    {
        body = body.push(
            text(problem)
                .size(theme::TEXT_META)
                .style(theme::text_toned(Tone::Warning)),
        );
    }
    container(body)
        .padding(theme::SPACE_XL)
        .width(Fill)
        .style(theme::panel)
        .into()
}
