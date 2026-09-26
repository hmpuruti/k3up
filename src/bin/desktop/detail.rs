use crate::{
    charts::{self, Sparkline},
    format,
    logs::LogBuffer,
    message::Message,
    output,
    theme::{self, Tone},
    widgets::{self, glyph},
};
use chrono::{DateTime, Utc};
use iced::{
    Alignment, Element,
    Length::Fill,
    widget::{Space, column, container, row, text},
};
use k3up::{
    metrics::WorkloadUsage,
    model::{Kind, Status},
};

const SPARK_WIDTH: f32 = 56.0;
const SPARK_HEIGHT: f32 = 18.0;

pub struct Context<'a> {
    pub status: &'a Status,
    pub usage: Option<&'a WorkloadUsage>,
    pub charts: &'a charts::Cache,
    pub logs: &'a LogBuffer,
    pub now: DateTime<Utc>,
    pub connected: bool,
    pub busy: bool,
    pub confirming_remove: bool,
    pub scale: f32,
}

pub fn view(ctx: Context<'_>) -> Element<'_, Message> {
    let status = ctx.status;
    let live = status.pid.is_some();
    let mut body = column![
        header(status),
        actions(&ctx),
        facts(status, ctx.usage, ctx.charts, ctx.now, ctx.scale),
        command(status),
    ]
    .spacing(theme::SPACE_LG);
    if ctx.confirming_remove {
        body = body.push(remove_prompt(&ctx));
    }
    body = body.push(output::view(ctx.logs, live));
    container(body.height(Fill))
        .padding(theme::SPACE_XL)
        .width(Fill)
        .height(Fill)
        .style(theme::panel)
        .into()
}

fn header(status: &Status) -> Element<'_, Message> {
    let mut heading = row![
        text(&status.workload.name).size(theme::TEXT_SECTION + 2),
        widgets::kind_tag(status.workload.kind),
    ]
    .spacing(theme::SPACE_MD)
    .align_y(Alignment::Center);
    if !status.workload.group.is_empty() {
        heading = heading.push(widgets::pill(&status.workload.group, Tone::Neutral));
    }
    heading = heading.push(Space::with_width(Fill));
    heading = heading.push(widgets::status_pill(status.state));
    heading = heading.push(widgets::icon_button(
        glyph::CLOSE,
        "Close",
        Some(Message::Deselect),
    ));
    let mut header = column![heading].spacing(theme::SPACE_XS + 2);
    if !status.workload.description.is_empty() {
        header = header.push(widgets::muted(
            &status.workload.description,
            theme::TEXT_BODY,
        ));
    }
    if !status.reason.is_empty() {
        header = header.push(widgets::toned(
            &status.reason,
            theme::TEXT_META,
            format::state_tone(status.state),
        ));
    }
    header.into()
}

fn actions<'a>(ctx: &Context<'a>) -> Element<'a, Message> {
    let status = ctx.status;
    let ready = ctx.connected && !ctx.busy;
    let active = status.pid.is_some() || status.desired_running;
    let editable = ready && !active;
    row![
        widgets::action(
            glyph::PLAY,
            "Start",
            (ready && status.pid.is_none()).then_some(Message::Start),
            theme::primary,
        ),
        widgets::action(
            glyph::STOP,
            "Stop",
            (ready && active).then_some(Message::Stop),
            theme::secondary,
        ),
        widgets::action(
            glyph::RESTART,
            "Restart",
            ready.then_some(Message::Restart),
            theme::secondary,
        ),
        Space::with_width(Fill),
        widgets::secondary("Edit", editable.then_some(Message::Edit)),
        widgets::danger_outline(
            "Remove",
            (editable && !ctx.confirming_remove).then_some(Message::Remove)
        ),
    ]
    .spacing(theme::SPACE_SM)
    .align_y(Alignment::Center)
    .wrap()
    .into()
}

fn facts<'a>(
    status: &'a Status,
    usage: Option<&'a WorkloadUsage>,
    charts: &'a charts::Cache,
    now: DateTime<Utc>,
    scale: f32,
) -> Element<'a, Message> {
    let uptime = status
        .started_at
        .filter(|_| status.pid.is_some())
        .map(|at| format::duration_coarse((now - at).num_seconds().max(0) as u64))
        .unwrap_or_else(|| "—".into());
    let started = status
        .started_at
        .map(|at| format!("{} · {}", format::clock(at), format::relative(at, now)))
        .unwrap_or_else(|| "—".into());
    let next = status
        .next_run
        .map(|at| format!("{} · {}", format::clock(at), format::relative(at, now)))
        .unwrap_or_else(|| "—".into());
    let recovery = if status.workload.kind == Kind::Job {
        "—".to_owned()
    } else {
        format!(
            "{} / {} · {}",
            status.restart_count,
            format::restart_limit(status.workload.max_restarts),
            format::restart_label(status.workload.restart)
        )
    };
    let dependencies: Element<_> = if status.workload.depends_on.is_empty() {
        text("—").size(theme::TEXT_BODY).into()
    } else {
        let mut chips = row![].spacing(theme::SPACE_XS);
        for name in &status.workload.depends_on {
            chips = chips.push(widgets::pill(name, theme::Tone::Neutral));
        }
        chips.wrap().into()
    };
    let top = row![
        widgets::fact_text(
            "PID",
            status
                .pid
                .map(|pid| pid.to_string())
                .unwrap_or_else(|| "—".into())
        ),
        widgets::fact_text("UPTIME", uptime),
        widgets::fact_text("RESTARTS", recovery),
        widgets::fact_text("LAST EXIT", format::exit_label(status.last_exit)),
    ]
    .spacing(theme::SPACE_MD);
    let bottom = row![
        widgets::fact_text("STARTED", started),
        widgets::fact_text("NEXT RUN", next),
        widgets::fact("DEPENDS ON", dependencies),
    ]
    .spacing(theme::SPACE_MD);
    container(column![top, bottom, resources(usage, charts, scale)].spacing(theme::SPACE_LG))
        .padding([theme::SPACE_MD, theme::SPACE_LG])
        .width(Fill)
        .style(theme::well)
        .into()
}

fn resources<'a>(
    usage: Option<&'a WorkloadUsage>,
    charts: &'a charts::Cache,
    scale: f32,
) -> Element<'a, Message> {
    let Some(usage) = usage else {
        return row![
            widgets::fact_text("CPU", "—".into()),
            widgets::fact_text("MEMORY", "—".into()),
            widgets::fact_text("PROCESSES", "—".into()),
            widgets::fact_text("LOG", "—".into()),
        ]
        .spacing(theme::SPACE_MD)
        .into();
    };
    let memory_peak = usage.memory_history.iter().copied().max().unwrap_or(0) as f32 * 1.1;
    row![
        widgets::fact(
            "CPU",
            live(
                format::percent(usage.usage.cpu),
                usage.cpu_history.clone(),
                5.0,
                scale,
                charts,
                charts::slot(("detail", "cpu", &usage.name)),
            )
        ),
        widgets::fact(
            "MEMORY",
            live(
                format::bytes(usage.usage.memory),
                usage.memory_history.iter().map(|m| *m as f32).collect(),
                memory_peak,
                scale,
                charts,
                charts::slot(("detail", "memory", &usage.name)),
            )
        ),
        widgets::fact_text("PROCESSES", usage.usage.processes.to_string()),
        widgets::fact_text("LOG", format::bytes(usage.log_bytes)),
    ]
    .spacing(theme::SPACE_MD)
    .into()
}

fn live<'a>(
    value: String,
    history: Vec<f32>,
    floor: f32,
    scale: f32,
    charts: &'a charts::Cache,
    slot: u64,
) -> Element<'a, Message> {
    row![
        text(value).size(theme::TEXT_BODY),
        charts::sparkline(
            Sparkline::new(history, floor, Tone::Accent, scale, charts, slot),
            SPARK_WIDTH,
            SPARK_HEIGHT,
        ),
    ]
    .spacing(theme::SPACE_SM)
    .align_y(Alignment::Center)
    .into()
}

fn command(status: &Status) -> Element<'_, Message> {
    column![
        row![
            widgets::caption("COMMAND"),
            widgets::mono(format::command_line(status), theme::TEXT_META).width(Fill),
        ]
        .spacing(theme::SPACE_MD)
        .align_y(Alignment::Start),
        row![
            widgets::caption("CWD"),
            widgets::mono(&status.workload.working_directory, theme::TEXT_META).width(Fill),
        ]
        .spacing(theme::SPACE_MD)
        .align_y(Alignment::Start),
    ]
    .spacing(theme::SPACE_XS + 2)
    .into()
}

fn remove_prompt<'a>(ctx: &Context<'a>) -> Element<'a, Message> {
    let name = &ctx.status.workload.name;
    container(
        row![
            widgets::toned(
                format!("Remove {name}?"),
                theme::TEXT_BODY,
                theme::Tone::Danger
            ),
            Space::with_width(Fill),
            widgets::secondary("Keep", Some(Message::CancelRemove)),
            widgets::danger(
                "Remove",
                (ctx.connected && !ctx.busy).then_some(Message::ConfirmRemove)
            ),
        ]
        .spacing(theme::SPACE_SM)
        .align_y(Alignment::Center),
    )
    .padding([theme::SPACE_SM, theme::SPACE_MD])
    .width(Fill)
    .style(theme::banner(theme::Tone::Danger))
    .into()
}
