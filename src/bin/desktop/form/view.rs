use super::{Cadence, Field, Form, FormMessage};
use crate::{
    format,
    message::Message,
    theme::{self, Tone},
    widgets::{self, glyph},
};
use iced::{
    Alignment, Element, Font,
    Length::Fill,
    widget::{
        Space, column, combo_box, container, pick_list, row, scrollable, text, text_editor, toggler,
    },
};
use k3up::model::{Kind, Missed, Restart, RestartBackoff, ScheduleAction};

const KINDS: [(&str, Kind); 2] = [("Service", Kind::Service), ("Job", Kind::Job)];
const RESTARTS: [(&str, Restart); 3] = [
    ("Never", Restart::Never),
    ("On failure", Restart::OnFailure),
    ("Always", Restart::Always),
];
const LIMITS: [(&str, bool); 2] = [("Up to", false), ("Unlimited", true)];
const BACKOFFS: [(&str, RestartBackoff); 2] = [
    ("Doubling", RestartBackoff::Exponential),
    ("Fixed", RestartBackoff::Fixed),
];
const CADENCES: [(&str, Cadence); 3] = [
    ("Manual", Cadence::Manual),
    ("Interval", Cadence::Every),
    ("Cron", Cadence::Cron),
];
const ACTIONS: [(&str, ScheduleAction); 2] = [
    ("Start", ScheduleAction::Start),
    ("Restart", ScheduleAction::Restart),
];
const MISSED: [(&str, Missed); 2] = [("Skip", Missed::Skip), ("Run once", Missed::RunOnce)];

pub struct Context<'a> {
    pub form: &'a Form,
    pub names: Vec<String>,
    pub can_save: bool,
}

pub fn view(ctx: Context<'_>) -> Element<'_, Message> {
    let form = ctx.form;
    let body: Element<_> = if form.raw {
        raw(form)
    } else {
        fields(form, ctx.names)
    };
    column![header(form, ctx.can_save), body]
        .spacing(theme::SPACE_XL)
        .height(Fill)
        .into()
}

fn header(form: &Form, can_save: bool) -> Element<'_, Message> {
    let heading = if form.editing {
        format!("Edit {}", form.base.name)
    } else {
        "New workload".to_owned()
    };
    row![
        widgets::icon_button(glyph::BACK, "Back", Some(Message::CancelForm)),
        text(heading).size(theme::TEXT_TITLE),
        Space::with_width(Fill),
        widgets::segmented(&[("Form", false), ("TOML", true)], form.raw, |raw| {
            Message::Form(FormMessage::Raw(raw))
        }),
        widgets::secondary("Cancel", Some(Message::CancelForm)),
        widgets::primary("Save", can_save.then_some(Message::Save)),
    ]
    .spacing(theme::SPACE_MD)
    .align_y(Alignment::Center)
    .into()
}

fn raw(form: &Form) -> Element<'_, Message> {
    container(
        text_editor(&form.editor)
            .on_action(|action| Message::Form(FormMessage::Editor(action)))
            .font(Font::MONOSPACE)
            .size(theme::TEXT_BODY)
            .padding(theme::SPACE_LG)
            .height(Fill)
            .style(theme::editor),
    )
    .height(Fill)
    .into()
}

fn fields(form: &Form, names: Vec<String>) -> Element<'_, Message> {
    let left = column![program(form), environment(form)].spacing(theme::SPACE_LG);
    let mut right = column![lifecycle(form, names)].spacing(theme::SPACE_LG);
    if !form.is_job() {
        right = right.push(recovery(form));
    }
    right = right.push(limits(form));
    right = right.push(schedule(form));
    scrollable(
        container(
            row![left.width(Fill), right.width(Fill)]
                .spacing(theme::SPACE_LG)
                .align_y(Alignment::Start),
        )
        .padding(iced::Padding {
            right: 12.0,
            bottom: 8.0,
            ..Default::default()
        }),
    )
    .height(Fill)
    .style(theme::scroll)
    .into()
}

fn text_field<'a>(
    label: &'a str,
    placeholder: &'a str,
    value: &'a str,
    field: Field,
) -> Element<'a, Message> {
    widgets::field(
        label,
        widgets::clip(widgets::input(placeholder, value, move |value| {
            Message::Form(FormMessage::Text(field, value))
        })),
    )
}

fn mono_field<'a>(
    label: &'a str,
    placeholder: &'a str,
    value: &'a str,
    field: Field,
) -> Element<'a, Message> {
    widgets::field(
        label,
        widgets::clip(
            widgets::input(placeholder, value, move |value| {
                Message::Form(FormMessage::Text(field, value))
            })
            .font(Font::MONOSPACE),
        ),
    )
}

fn number_field<'a>(
    label: &'a str,
    placeholder: &'a str,
    value: &'a str,
    field: Field,
    unit: &'a str,
) -> Element<'a, Message> {
    widgets::field(
        label,
        row![
            widgets::input(placeholder, value, move |value| {
                Message::Form(FormMessage::Text(field, value))
            })
            .width(Fill),
            widgets::faint(unit, theme::TEXT_META),
        ]
        .spacing(theme::SPACE_SM)
        .align_y(Alignment::Center),
    )
}

fn program(form: &Form) -> Element<'_, Message> {
    let (exe_hint, dir_hint) = if cfg!(windows) {
        ("C:\\apps\\worker.exe", "C:\\apps")
    } else {
        ("/usr/local/bin/worker", "/opt/app")
    };
    let name: Element<_> = if form.editing {
        widgets::clip(widgets::readonly(&form.name))
    } else {
        widgets::clip(widgets::input("api-worker", &form.name, |value| {
            Message::Form(FormMessage::Text(Field::Name, value))
        }))
    };
    widgets::section(
        "Program",
        column![
            widgets::field("NAME", name),
            text_field("DESCRIPTION", "", &form.description, Field::Description),
            widgets::field("GROUP", widgets::clip(folder_picker(form))),
            mono_field("EXECUTABLE", exe_hint, &form.executable, Field::Executable),
            mono_field(
                "WORKING DIRECTORY",
                dir_hint,
                &form.directory,
                Field::Directory
            ),
            arguments(form),
        ]
        .spacing(theme::SPACE_LG),
    )
}

/// Existing folders are offered as you type; any valid path is accepted.
fn folder_picker(form: &Form) -> Element<'_, Message> {
    let set = |value| Message::Form(FormMessage::Text(Field::Group, value));
    combo_box(&form.folders, "folder/subfolder", Some(&form.group), set)
        .on_input(set)
        .padding([9, 11])
        .size(f32::from(theme::TEXT_INPUT))
        .input_style(theme::input)
        .menu_style(theme::menu)
        .into()
}

fn arguments(form: &Form) -> Element<'_, Message> {
    let mut rows = column![].spacing(theme::SPACE_SM);
    for (index, argument) in form.arguments.iter().enumerate() {
        rows = rows.push(
            row![
                container(widgets::clip(
                    widgets::input("--flag", argument, move |value| {
                        Message::Form(FormMessage::ArgumentChange(index, value))
                    })
                    .font(Font::MONOSPACE)
                ))
                .width(Fill),
                widgets::icon_button(
                    glyph::CLOSE,
                    "Remove",
                    Some(Message::Form(FormMessage::ArgumentRemove(index))),
                ),
            ]
            .spacing(theme::SPACE_XS)
            .align_y(Alignment::Center),
        );
    }
    rows = rows.push(add_button(
        "Argument",
        Message::Form(FormMessage::ArgumentAdd),
    ));
    widgets::field("ARGUMENTS", rows)
}

fn environment(form: &Form) -> Element<'_, Message> {
    let mut rows = column![].spacing(theme::SPACE_SM);
    for (index, (key, value)) in form.variables.iter().enumerate() {
        rows = rows.push(
            row![
                container(widgets::clip(
                    widgets::input("KEY", key, move |value| {
                        Message::Form(FormMessage::VariableKey(index, value))
                    })
                    .font(Font::MONOSPACE)
                ))
                .width(iced::Length::FillPortion(2)),
                container(widgets::clip(
                    widgets::input("value", value, move |value| {
                        Message::Form(FormMessage::VariableValue(index, value))
                    })
                    .font(Font::MONOSPACE)
                ))
                .width(iced::Length::FillPortion(3)),
                widgets::icon_button(
                    glyph::CLOSE,
                    "Remove",
                    Some(Message::Form(FormMessage::VariableRemove(index))),
                ),
            ]
            .spacing(theme::SPACE_XS)
            .align_y(Alignment::Center),
        );
    }
    rows = rows.push(add_button(
        "Variable",
        Message::Form(FormMessage::VariableAdd),
    ));
    widgets::section("Environment", rows)
}

fn lifecycle(form: &Form, names: Vec<String>) -> Element<'_, Message> {
    let mut content = column![
        row![
            widgets::field(
                "KIND",
                widgets::segmented(&KINDS, form.kind, |kind| Message::Form(FormMessage::Kind(
                    kind
                )))
            ),
            Space::with_width(Fill),
            widgets::field(
                "START ON REGISTER",
                toggler(form.boot)
                    .on_toggle(|value| Message::Form(FormMessage::Boot(value)))
                    .size(22)
                    .style(theme::switch)
            ),
        ]
        .spacing(theme::SPACE_LG)
        .align_y(Alignment::End),
        dependencies(form, names),
        mono_field(
            "SUCCESS EXIT CODES",
            "—",
            &form.success_codes,
            Field::SuccessCodes
        ),
    ]
    .spacing(theme::SPACE_LG);
    if !form.is_job() {
        content = content.push(
            row![
                mono_field(
                    "TCP READINESS",
                    "127.0.0.1:8080",
                    &form.readiness,
                    Field::Readiness
                ),
                number_field(
                    "STARTUP TIMEOUT",
                    "30",
                    &form.startup_timeout,
                    Field::StartupTimeout,
                    "s"
                ),
            ]
            .spacing(theme::SPACE_MD),
        );
    }
    widgets::section("Lifecycle", content)
}

fn dependencies(form: &Form, names: Vec<String>) -> Element<'_, Message> {
    let available: Vec<String> = names
        .into_iter()
        .filter(|name| name != &form.name && !form.dependencies.contains(name))
        .collect();
    let mut chips = row![].spacing(theme::SPACE_XS);
    for (index, name) in form.dependencies.iter().enumerate() {
        chips = chips.push(chip(
            name,
            Message::Form(FormMessage::DependencyRemove(index)),
        ));
    }
    let mut content = column![].spacing(theme::SPACE_SM);
    if !form.dependencies.is_empty() {
        content = content.push(chips.wrap());
    }
    if !available.is_empty() {
        content = content.push(
            pick_list(available, None::<String>, |name| {
                Message::Form(FormMessage::DependencyAdd(name))
            })
            .placeholder("Add dependency")
            .padding([8, 11])
            .text_size(theme::TEXT_BODY)
            .style(theme::pick)
            .menu_style(theme::menu),
        );
    } else if form.dependencies.is_empty() {
        content = content.push(widgets::faint("—", theme::TEXT_BODY));
    }
    widgets::field("DEPENDS ON", content)
}

fn recovery(form: &Form) -> Element<'_, Message> {
    let mut content = column![widgets::field(
        "RESTART",
        widgets::segmented(&RESTARTS, form.restart, |value| Message::Form(
            FormMessage::Restart(value)
        ))
    )]
    .spacing(theme::SPACE_LG);
    if form.restart != Restart::Never {
        content = content.push(
            row![
                max_restarts(form),
                number_field(
                    "RESTART DELAY",
                    "2",
                    &form.restart_delay,
                    Field::RestartDelay,
                    "s"
                ),
            ]
            .spacing(theme::SPACE_MD)
            .align_y(Alignment::End),
        );
        content = content.push(widgets::field(
            "BACKOFF",
            widgets::segmented(&BACKOFFS, form.backoff, |value| {
                Message::Form(FormMessage::Backoff(value))
            }),
        ));
    }
    widgets::section("Recovery", content)
}

fn max_restarts(form: &Form) -> Element<'_, Message> {
    let mut control = row![widgets::segmented(&LIMITS, form.unlimited, |value| {
        Message::Form(FormMessage::Unlimited(value))
    })]
    .spacing(theme::SPACE_SM)
    .align_y(Alignment::Center);
    if !form.unlimited {
        control = control.push(
            widgets::input("5", &form.max_restarts, |value| {
                Message::Form(FormMessage::Text(Field::MaxRestarts, value))
            })
            .width(Fill),
        );
    }
    widgets::field("MAX RESTARTS", control)
}

fn limits(form: &Form) -> Element<'_, Message> {
    widgets::section(
        "Limits",
        row![
            number_field(
                "STOP TIMEOUT",
                "5",
                &form.stop_timeout,
                Field::StopTimeout,
                "s"
            ),
            number_field(
                "RUN TIMEOUT",
                "—",
                &form.run_timeout,
                Field::RunTimeout,
                "s"
            ),
        ]
        .spacing(theme::SPACE_MD),
    )
}

fn schedule(form: &Form) -> Element<'_, Message> {
    let mut content = column![widgets::field(
        "CADENCE",
        widgets::segmented(&CADENCES, form.cadence, |value| Message::Form(
            FormMessage::Cadence(value)
        ))
    )]
    .spacing(theme::SPACE_LG);
    match form.cadence {
        Cadence::Manual => {}
        Cadence::Every => {
            content = content.push(number_field(
                "EVERY",
                "3600",
                &form.every,
                Field::Every,
                "s",
            ));
        }
        Cadence::Cron => {
            content = content.push(
                row![
                    mono_field("CRON", "0 0 2 * * *", &form.cron, Field::Cron),
                    text_field("TIMEZONE", "UTC", &form.timezone, Field::Timezone),
                ]
                .spacing(theme::SPACE_MD),
            );
        }
    }
    if form.cadence != Cadence::Manual {
        let mut policies = row![].spacing(theme::SPACE_XL);
        if form.is_job() {
            policies = policies.push(widgets::field(
                "ACTION",
                widgets::pill(format::action_label(ScheduleAction::Start), Tone::Accent),
            ));
        } else {
            policies = policies.push(widgets::field(
                "ACTION",
                widgets::segmented(&ACTIONS, form.action, |value| {
                    Message::Form(FormMessage::Action(value))
                }),
            ));
        }
        policies = policies.push(widgets::field(
            "IF MISSED",
            widgets::segmented(&MISSED, form.missed, |value| {
                Message::Form(FormMessage::Missed(value))
            }),
        ));
        content = content.push(policies);
    }
    widgets::section("Schedule", content)
}

fn add_button<'a>(label: &'a str, message: Message) -> Element<'a, Message> {
    widgets::action(glyph::PLUS, label, Some(message), theme::ghost)
}

fn chip<'a>(label: &'a str, remove: Message) -> Element<'a, Message> {
    container(
        row![
            text(label).size(theme::TEXT_META),
            widgets::icon_button(glyph::CLOSE, "Remove", Some(remove)),
        ]
        .spacing(2)
        .align_y(Alignment::Center),
    )
    .padding(iced::Padding {
        top: 2.0,
        right: 4.0,
        bottom: 2.0,
        left: 10.0,
    })
    .style(theme::pill(Tone::Neutral))
    .into()
}
