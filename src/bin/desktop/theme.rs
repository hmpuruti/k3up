use iced::{
    Background, Border, Color, Shadow, Theme, Vector,
    widget::{
        button, container, overlay::menu, pick_list, rule, scrollable, text, text_editor,
        text_input, toggler,
    },
};
use std::sync::LazyLock;

pub const SPACE_XS: u16 = 4;
pub const SPACE_SM: u16 = 8;
pub const SPACE_MD: u16 = 12;
pub const SPACE_LG: u16 = 16;
pub const SPACE_XL: u16 = 24;
pub const SPACE_2XL: u16 = 32;

pub const TEXT_CAPTION: u16 = 11;
pub const TEXT_META: u16 = 12;
pub const TEXT_BODY: u16 = 13;
pub const TEXT_INPUT: u16 = 14;
pub const TEXT_SUBTITLE: u16 = 16;
pub const TEXT_SECTION: u16 = 20;
pub const TEXT_TITLE: u16 = 26;

pub const RADIUS_SM: f32 = 6.0;
pub const RADIUS_MD: f32 = 10.0;
pub const RADIUS_PILL: f32 = 999.0;

pub const SIDEBAR_WIDTH: f32 = 216.0;
pub const LIST_WIDTH: f32 = 300.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Mode {
    #[default]
    Light,
    Dark,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tone {
    Neutral,
    Accent,
    Success,
    Warning,
    Danger,
    Info,
}

pub struct Tones {
    pub bg: Color,
    pub surface: Color,
    pub surface_hover: Color,
    pub sunken: Color,
    pub line: Color,
    pub line_strong: Color,
    pub ink: Color,
    pub ink_muted: Color,
    pub ink_faint: Color,
    pub accent: Color,
    pub accent_hover: Color,
    pub accent_soft: Color,
    pub on_accent: Color,
    pub success: Color,
    pub success_soft: Color,
    pub warning: Color,
    pub warning_soft: Color,
    pub danger: Color,
    pub danger_soft: Color,
    pub info: Color,
    pub info_soft: Color,
    pub rail_bg: Color,
    pub rail_ink: Color,
    pub rail_muted: Color,
    pub rail_hover: Color,
    pub rail_active: Color,
    pub rail_line: Color,
    pub rail_accent: Color,
    pub console_bg: Color,
    pub console_ink: Color,
    pub console_muted: Color,
    pub console_line: Color,
    pub shadow: Color,
}

const fn rgb(hex: u32) -> Color {
    Color {
        r: ((hex >> 16) & 0xff) as f32 / 255.0,
        g: ((hex >> 8) & 0xff) as f32 / 255.0,
        b: (hex & 0xff) as f32 / 255.0,
        a: 1.0,
    }
}

const fn rgba(hex: u32, alpha: f32) -> Color {
    let mut color = rgb(hex);
    color.a = alpha;
    color
}

pub const LIGHT: Tones = Tones {
    bg: rgb(0xF3F5F7),
    surface: rgb(0xFFFFFF),
    surface_hover: rgb(0xF7F9FA),
    sunken: rgb(0xEFF2F5),
    line: rgb(0xE1E6EB),
    line_strong: rgb(0xC5CFD8),
    ink: rgb(0x172230),
    ink_muted: rgb(0x5A6B7B),
    ink_faint: rgb(0x8C9AA8),
    accent: rgb(0x186F70),
    accent_hover: rgb(0x135B5C),
    accent_soft: rgb(0xDCEEEC),
    on_accent: rgb(0xFFFFFF),
    success: rgb(0x1C8A57),
    success_soft: rgb(0xDFF4E8),
    warning: rgb(0xB0731A),
    warning_soft: rgb(0xFBEFD6),
    danger: rgb(0xC0373F),
    danger_soft: rgb(0xFBE3E5),
    info: rgb(0x2D67C9),
    info_soft: rgb(0xE2EBFA),
    rail_bg: rgb(0x1F2F40),
    rail_ink: rgb(0xEAF0F5),
    rail_muted: rgb(0x93A4B6),
    rail_hover: rgb(0x27394C),
    rail_active: rgb(0x2F4358),
    rail_line: rgb(0x2C3E52),
    rail_accent: rgb(0x39A69A),
    console_bg: rgb(0x0E161E),
    console_ink: rgb(0xD5E1EB),
    console_muted: rgb(0x6C8093),
    console_line: rgb(0x1B2733),
    shadow: rgba(0x101820, 0.07),
};

pub const DARK: Tones = Tones {
    bg: rgb(0x0F1419),
    surface: rgb(0x161D24),
    surface_hover: rgb(0x1B242C),
    sunken: rgb(0x0F161C),
    line: rgb(0x242F38),
    line_strong: rgb(0x36434F),
    ink: rgb(0xE3EAF0),
    ink_muted: rgb(0x94A3B1),
    ink_faint: rgb(0x627180),
    accent: rgb(0x39A69A),
    accent_hover: rgb(0x4FB9AD),
    accent_soft: rgb(0x143634),
    on_accent: rgb(0x06191A),
    success: rgb(0x40C58B),
    success_soft: rgb(0x11332A),
    warning: rgb(0xE2A63D),
    warning_soft: rgb(0x3A2C12),
    danger: rgb(0xE8666F),
    danger_soft: rgb(0x3D1B1F),
    info: rgb(0x6FA5F6),
    info_soft: rgb(0x162A4A),
    rail_bg: rgb(0x0B1118),
    rail_ink: rgb(0xE3EAF0),
    rail_muted: rgb(0x7A8A99),
    rail_hover: rgb(0x131C26),
    rail_active: rgb(0x1B2836),
    rail_line: rgb(0x1B2733),
    rail_accent: rgb(0x39A69A),
    console_bg: rgb(0x0A0F14),
    console_ink: rgb(0xCEDBE6),
    console_muted: rgb(0x5E7082),
    console_line: rgb(0x162029),
    shadow: rgba(0x000000, 0.35),
};

static LIGHT_THEME: LazyLock<Theme> = LazyLock::new(|| build_theme(Mode::Light));
static DARK_THEME: LazyLock<Theme> = LazyLock::new(|| build_theme(Mode::Dark));

/// Asked for on every frame, so the derived palette is generated once per mode.
pub fn iced_theme(mode: Mode) -> Theme {
    match mode {
        Mode::Light => LIGHT_THEME.clone(),
        Mode::Dark => DARK_THEME.clone(),
    }
}

fn build_theme(mode: Mode) -> Theme {
    let tones = tones_for(mode);
    let name = match mode {
        Mode::Light => "K3 Up Light",
        Mode::Dark => "K3 Up Dark",
    };
    Theme::custom(
        name.into(),
        iced::theme::Palette {
            background: tones.bg,
            text: tones.ink,
            primary: tones.accent,
            success: tones.success,
            danger: tones.danger,
        },
    )
}

pub fn tones_for(mode: Mode) -> &'static Tones {
    match mode {
        Mode::Light => &LIGHT,
        Mode::Dark => &DARK,
    }
}

pub fn tones(theme: &Theme) -> &'static Tones {
    if theme.palette().background == DARK.bg {
        &DARK
    } else {
        &LIGHT
    }
}

pub fn tone_colors(t: &Tones, tone: Tone) -> (Color, Color) {
    match tone {
        Tone::Neutral => (t.ink_muted, t.sunken),
        Tone::Accent => (t.accent, t.accent_soft),
        Tone::Success => (t.success, t.success_soft),
        Tone::Warning => (t.warning, t.warning_soft),
        Tone::Danger => (t.danger, t.danger_soft),
        Tone::Info => (t.info, t.info_soft),
    }
}

pub fn tone_color(t: &Tones, tone: Tone) -> Color {
    tone_colors(t, tone).0
}

fn border(color: Color, width: f32, radius: f32) -> Border {
    Border {
        color,
        width,
        radius: radius.into(),
    }
}

fn shadow(color: Color, y: f32, blur: f32) -> Shadow {
    Shadow {
        color,
        offset: Vector::new(0.0, y),
        blur_radius: blur,
    }
}

fn no_shadow() -> Shadow {
    Shadow {
        color: Color::TRANSPARENT,
        offset: Vector::ZERO,
        blur_radius: 0.0,
    }
}

fn dim(color: Color, factor: f32) -> Color {
    color.scale_alpha(factor)
}

pub fn app(theme: &Theme) -> container::Style {
    let t = tones(theme);
    container::Style {
        background: Some(Background::Color(t.bg)),
        text_color: Some(t.ink),
        ..Default::default()
    }
}

pub fn panel(theme: &Theme) -> container::Style {
    let t = tones(theme);
    container::Style {
        background: Some(Background::Color(t.surface)),
        border: border(t.line, 1.0, RADIUS_MD),
        shadow: no_shadow(),
        text_color: Some(t.ink),
    }
}

pub fn panel_flat(theme: &Theme) -> container::Style {
    let t = tones(theme);
    container::Style {
        background: Some(Background::Color(t.surface)),
        border: border(t.line, 1.0, RADIUS_MD),
        shadow: no_shadow(),
        text_color: Some(t.ink),
    }
}

pub fn well(theme: &Theme) -> container::Style {
    let t = tones(theme);
    container::Style {
        background: Some(Background::Color(t.sunken)),
        border: border(Color::TRANSPARENT, 0.0, RADIUS_SM),
        shadow: no_shadow(),
        text_color: Some(t.ink),
    }
}

pub fn rail(theme: &Theme) -> container::Style {
    let t = tones(theme);
    container::Style {
        background: Some(Background::Color(t.rail_bg)),
        text_color: Some(t.rail_ink),
        ..Default::default()
    }
}

pub fn console(theme: &Theme) -> container::Style {
    let t = tones(theme);
    container::Style {
        background: Some(Background::Color(t.console_bg)),
        border: border(t.console_line, 1.0, RADIUS_SM),
        text_color: Some(t.console_ink),
        shadow: no_shadow(),
    }
}

pub fn console_header(theme: &Theme) -> container::Style {
    let t = tones(theme);
    container::Style {
        background: Some(Background::Color(t.console_line)),
        border: Border {
            color: Color::TRANSPARENT,
            width: 0.0,
            radius: iced::border::top(RADIUS_SM),
        },
        text_color: Some(t.console_muted),
        shadow: no_shadow(),
    }
}

pub fn pill(tone: Tone) -> impl Fn(&Theme) -> container::Style {
    move |theme| {
        let (fg, bg) = tone_colors(tones(theme), tone);
        container::Style {
            background: Some(Background::Color(bg)),
            border: border(Color::TRANSPARENT, 0.0, RADIUS_PILL),
            text_color: Some(fg),
            shadow: no_shadow(),
        }
    }
}

pub fn tag(theme: &Theme) -> container::Style {
    let t = tones(theme);
    container::Style {
        background: Some(Background::Color(Color::TRANSPARENT)),
        border: border(t.line_strong, 1.0, RADIUS_SM),
        text_color: Some(t.ink_muted),
        shadow: no_shadow(),
    }
}

pub fn banner(tone: Tone) -> impl Fn(&Theme) -> container::Style {
    move |theme| {
        let t = tones(theme);
        let (fg, bg) = tone_colors(t, tone);
        container::Style {
            background: Some(Background::Color(bg)),
            border: border(dim(fg, 0.35), 1.0, RADIUS_SM),
            text_color: Some(t.ink),
            shadow: no_shadow(),
        }
    }
}

pub fn tooltip(theme: &Theme) -> container::Style {
    let t = tones(theme);
    container::Style {
        background: Some(Background::Color(t.rail_bg)),
        border: border(Color::TRANSPARENT, 0.0, RADIUS_SM),
        text_color: Some(t.rail_ink),
        shadow: shadow(t.shadow, 2.0, 6.0),
    }
}

pub fn primary(theme: &Theme, status: button::Status) -> button::Style {
    let t = tones(theme);
    let background = match status {
        button::Status::Active => t.accent,
        button::Status::Hovered => t.accent_hover,
        button::Status::Pressed => dim(t.accent_hover, 0.9),
        button::Status::Disabled => dim(t.accent, 0.35),
    };
    button::Style {
        background: Some(Background::Color(background)),
        text_color: if status == button::Status::Disabled {
            dim(t.on_accent, 0.7)
        } else {
            t.on_accent
        },
        border: border(Color::TRANSPARENT, 0.0, RADIUS_SM),
        shadow: no_shadow(),
    }
}

pub fn secondary(theme: &Theme, status: button::Status) -> button::Style {
    let t = tones(theme);
    let (background, line, ink) = match status {
        button::Status::Active => (t.surface, t.line_strong, t.ink),
        button::Status::Hovered => (t.surface_hover, t.ink_faint, t.ink),
        button::Status::Pressed => (t.sunken, t.ink_faint, t.ink),
        button::Status::Disabled => (t.surface, t.line, t.ink_faint),
    };
    button::Style {
        background: Some(Background::Color(background)),
        text_color: ink,
        border: border(line, 1.0, RADIUS_SM),
        shadow: no_shadow(),
    }
}

pub fn danger(theme: &Theme, status: button::Status) -> button::Style {
    let t = tones(theme);
    let (background, ink) = match status {
        button::Status::Active => (t.danger, t.on_accent),
        button::Status::Hovered => (dim(t.danger, 0.88), t.on_accent),
        button::Status::Pressed => (dim(t.danger, 0.8), t.on_accent),
        button::Status::Disabled => (dim(t.danger, 0.3), dim(t.on_accent, 0.7)),
    };
    button::Style {
        background: Some(Background::Color(background)),
        text_color: if theme.palette().background == DARK.bg {
            t.ink
        } else {
            ink
        },
        border: border(Color::TRANSPARENT, 0.0, RADIUS_SM),
        shadow: no_shadow(),
    }
}

pub fn danger_outline(theme: &Theme, status: button::Status) -> button::Style {
    let t = tones(theme);
    let (background, line, ink) = match status {
        button::Status::Active => (Color::TRANSPARENT, dim(t.danger, 0.5), t.danger),
        button::Status::Hovered => (t.danger_soft, t.danger, t.danger),
        button::Status::Pressed => (dim(t.danger_soft, 0.8), t.danger, t.danger),
        button::Status::Disabled => (Color::TRANSPARENT, t.line, t.ink_faint),
    };
    button::Style {
        background: Some(Background::Color(background)),
        text_color: ink,
        border: border(line, 1.0, RADIUS_SM),
        shadow: no_shadow(),
    }
}

pub fn ghost(theme: &Theme, status: button::Status) -> button::Style {
    let t = tones(theme);
    let (background, ink) = match status {
        button::Status::Active => (Color::TRANSPARENT, t.ink_muted),
        button::Status::Hovered => (t.sunken, t.ink),
        button::Status::Pressed => (t.line, t.ink),
        button::Status::Disabled => (Color::TRANSPARENT, t.ink_faint),
    };
    button::Style {
        background: Some(Background::Color(background)),
        text_color: ink,
        border: border(Color::TRANSPARENT, 0.0, RADIUS_SM),
        shadow: no_shadow(),
    }
}

pub fn nav(active: bool) -> impl Fn(&Theme, button::Status) -> button::Style {
    move |theme, status| {
        let t = tones(theme);
        let background = if active {
            t.rail_active
        } else if matches!(status, button::Status::Hovered | button::Status::Pressed) {
            t.rail_hover
        } else {
            Color::TRANSPARENT
        };
        button::Style {
            background: Some(Background::Color(background)),
            text_color: if active { t.rail_ink } else { t.rail_muted },
            border: border(Color::TRANSPARENT, 0.0, RADIUS_SM),
            shadow: no_shadow(),
        }
    }
}

pub fn rail_button(theme: &Theme, status: button::Status) -> button::Style {
    let t = tones(theme);
    let background = match status {
        button::Status::Hovered | button::Status::Pressed => t.rail_hover,
        _ => Color::TRANSPARENT,
    };
    button::Style {
        background: Some(Background::Color(background)),
        text_color: t.rail_muted,
        border: border(t.rail_line, 1.0, RADIUS_SM),
        shadow: no_shadow(),
    }
}

pub fn chip(active: bool) -> impl Fn(&Theme, button::Status) -> button::Style {
    move |theme, status| {
        let t = tones(theme);
        let (background, ink, line) = if active {
            (t.accent_soft, t.accent, Color::TRANSPARENT)
        } else {
            match status {
                button::Status::Hovered | button::Status::Pressed => {
                    (t.sunken, t.ink, Color::TRANSPARENT)
                }
                _ => (Color::TRANSPARENT, t.ink_muted, Color::TRANSPARENT),
            }
        };
        button::Style {
            background: Some(Background::Color(background)),
            text_color: ink,
            border: border(line, 0.0, RADIUS_PILL),
            shadow: no_shadow(),
        }
    }
}

pub fn segment(active: bool) -> impl Fn(&Theme, button::Status) -> button::Style {
    move |theme, status| {
        let t = tones(theme);
        let (background, ink) = if active {
            (t.surface, t.ink)
        } else {
            match status {
                button::Status::Hovered | button::Status::Pressed => (dim(t.surface, 0.5), t.ink),
                button::Status::Disabled => (Color::TRANSPARENT, t.ink_faint),
                button::Status::Active => (Color::TRANSPARENT, t.ink_muted),
            }
        };
        button::Style {
            background: Some(Background::Color(background)),
            text_color: ink,
            border: border(
                if active { t.line } else { Color::TRANSPARENT },
                1.0,
                RADIUS_SM,
            ),
            shadow: no_shadow(),
        }
    }
}

pub fn rail_segment(active: bool) -> impl Fn(&Theme, button::Status) -> button::Style {
    move |theme, status| {
        let t = tones(theme);
        let (background, ink) = if active {
            (t.rail_active, t.rail_ink)
        } else {
            match status {
                button::Status::Hovered | button::Status::Pressed => (t.rail_hover, t.rail_ink),
                _ => (Color::TRANSPARENT, t.rail_muted),
            }
        };
        button::Style {
            background: Some(Background::Color(background)),
            text_color: ink,
            border: border(Color::TRANSPARENT, 0.0, RADIUS_SM),
            shadow: no_shadow(),
        }
    }
}

pub fn card(selected: bool) -> impl Fn(&Theme, button::Status) -> button::Style {
    move |theme, status| {
        let t = tones(theme);
        let (background, line, width) = if selected {
            (t.accent_soft, t.accent, 1.0)
        } else {
            match status {
                button::Status::Hovered => (t.surface_hover, t.line_strong, 1.0),
                button::Status::Pressed => (t.sunken, t.line_strong, 1.0),
                _ => (t.surface, t.line, 1.0),
            }
        };
        button::Style {
            background: Some(Background::Color(background)),
            text_color: t.ink,
            border: border(line, width, RADIUS_MD),
            shadow: no_shadow(),
        }
    }
}

pub fn input(theme: &Theme, status: text_input::Status) -> text_input::Style {
    let t = tones(theme);
    let (background, line) = match status {
        text_input::Status::Active => (t.sunken, t.line),
        text_input::Status::Hovered => (t.sunken, t.line_strong),
        text_input::Status::Focused => (t.surface, t.accent),
        text_input::Status::Disabled => (dim(t.sunken, 0.5), t.line),
    };
    text_input::Style {
        background: Background::Color(background),
        border: border(line, 1.0, RADIUS_SM),
        icon: t.ink_faint,
        placeholder: t.ink_faint,
        value: if status == text_input::Status::Disabled {
            t.ink_faint
        } else {
            t.ink
        },
        selection: dim(t.accent, 0.3),
    }
}

pub fn editor(theme: &Theme, status: text_editor::Status) -> text_editor::Style {
    let t = tones(theme);
    let line = match status {
        text_editor::Status::Focused => t.accent,
        text_editor::Status::Hovered => t.line_strong,
        _ => t.line,
    };
    text_editor::Style {
        background: Background::Color(t.sunken),
        border: border(line, 1.0, RADIUS_SM),
        icon: t.ink_faint,
        placeholder: t.ink_faint,
        value: t.ink,
        selection: dim(t.accent, 0.3),
    }
}

pub fn pick(theme: &Theme, status: pick_list::Status) -> pick_list::Style {
    let t = tones(theme);
    let line = match status {
        pick_list::Status::Active => t.line,
        pick_list::Status::Hovered => t.line_strong,
        pick_list::Status::Opened => t.accent,
    };
    pick_list::Style {
        text_color: t.ink,
        placeholder_color: t.ink_faint,
        handle_color: t.ink_muted,
        background: Background::Color(t.sunken),
        border: border(line, 1.0, RADIUS_SM),
    }
}

pub fn menu(theme: &Theme) -> menu::Style {
    let t = tones(theme);
    menu::Style {
        background: Background::Color(t.surface),
        border: border(t.line_strong, 1.0, RADIUS_SM),
        text_color: t.ink,
        selected_text_color: t.accent,
        selected_background: Background::Color(t.accent_soft),
    }
}

pub fn switch(theme: &Theme, status: toggler::Status) -> toggler::Style {
    let t = tones(theme);
    let (on, hovered) = match status {
        toggler::Status::Active { is_toggled } => (is_toggled, false),
        toggler::Status::Hovered { is_toggled } => (is_toggled, true),
        toggler::Status::Disabled => (false, false),
    };
    toggler::Style {
        background: if on {
            if hovered { t.accent_hover } else { t.accent }
        } else if hovered {
            t.line_strong
        } else {
            t.line
        },
        background_border_width: 0.0,
        background_border_color: Color::TRANSPARENT,
        foreground: t.surface,
        foreground_border_width: 0.0,
        foreground_border_color: Color::TRANSPARENT,
    }
}

pub fn divider(theme: &Theme) -> rule::Style {
    let t = tones(theme);
    rule::Style {
        color: t.line,
        width: 1,
        radius: 0.0.into(),
        fill_mode: rule::FillMode::Full,
    }
}

pub fn rail_divider(theme: &Theme) -> rule::Style {
    let t = tones(theme);
    rule::Style {
        color: t.rail_line,
        width: 1,
        radius: 0.0.into(),
        fill_mode: rule::FillMode::Full,
    }
}

fn scroll_style(scroller: Color, status: scrollable::Status) -> scrollable::Style {
    let visible = !matches!(status, scrollable::Status::Active);
    let rail = scrollable::Rail {
        background: None,
        border: border(Color::TRANSPARENT, 0.0, RADIUS_PILL),
        scroller: scrollable::Scroller {
            color: if visible {
                scroller
            } else {
                dim(scroller, 0.4)
            },
            border: border(Color::TRANSPARENT, 0.0, RADIUS_PILL),
        },
    };
    scrollable::Style {
        container: container::Style::default(),
        vertical_rail: rail,
        horizontal_rail: rail,
        gap: None,
    }
}

pub fn scroll(theme: &Theme, status: scrollable::Status) -> scrollable::Style {
    scroll_style(tones(theme).line_strong, status)
}

pub fn console_scroll(theme: &Theme, status: scrollable::Status) -> scrollable::Style {
    scroll_style(tones(theme).console_muted, status)
}

pub fn rail_well(theme: &Theme) -> container::Style {
    let t = tones(theme);
    container::Style {
        background: Some(Background::Color(t.rail_hover)),
        border: border(Color::TRANSPARENT, 0.0, RADIUS_SM),
        text_color: Some(t.rail_ink),
        shadow: no_shadow(),
    }
}

pub fn rail_badge(alert: bool) -> impl Fn(&Theme) -> container::Style {
    move |theme| {
        let t = tones(theme);
        let (fg, bg) = if alert {
            (t.warning, t.warning_soft)
        } else {
            (t.rail_muted, t.rail_active)
        };
        container::Style {
            background: Some(Background::Color(bg)),
            border: border(Color::TRANSPARENT, 0.0, RADIUS_PILL),
            text_color: Some(fg),
            shadow: no_shadow(),
        }
    }
}

fn text_color(color: Color) -> text::Style {
    text::Style { color: Some(color) }
}

pub fn text_muted(theme: &Theme) -> text::Style {
    text_color(tones(theme).ink_muted)
}

pub fn text_faint(theme: &Theme) -> text::Style {
    text_color(tones(theme).ink_faint)
}

pub fn text_rail_ink(theme: &Theme) -> text::Style {
    text_color(tones(theme).rail_ink)
}

pub fn text_rail_muted(theme: &Theme) -> text::Style {
    text_color(tones(theme).rail_muted)
}

pub fn text_rail_accent(theme: &Theme) -> text::Style {
    text_color(tones(theme).rail_accent)
}

pub fn text_console_ink(theme: &Theme) -> text::Style {
    text_color(tones(theme).console_ink)
}

pub fn text_console_muted(theme: &Theme) -> text::Style {
    text_color(tones(theme).console_muted)
}

pub fn text_toned(tone: Tone) -> impl Fn(&Theme) -> text::Style {
    move |theme| text_color(tone_color(tones(theme), tone))
}

pub fn text_connection(connected: bool) -> impl Fn(&Theme) -> text::Style {
    move |theme| {
        let t = tones(theme);
        text_color(if connected { t.success } else { t.warning })
    }
}

pub fn list_row(theme: &Theme) -> container::Style {
    container::Style {
        text_color: Some(tones(theme).ink),
        ..Default::default()
    }
}

pub fn stat_tile(tone: Tone) -> impl Fn(&Theme) -> container::Style {
    move |theme| {
        let t = tones(theme);
        let (line, background) = match tone {
            Tone::Neutral => (t.line, t.surface),
            tone => {
                let (fg, bg) = tone_colors(t, tone);
                (dim(fg, 0.45), bg)
            }
        };
        container::Style {
            background: Some(Background::Color(background)),
            border: border(line, 1.0, RADIUS_MD),
            text_color: Some(t.ink),
            shadow: no_shadow(),
        }
    }
}

pub fn skeleton(theme: &Theme) -> container::Style {
    let t = tones(theme);
    container::Style {
        background: Some(Background::Color(t.sunken)),
        border: border(Color::TRANSPARENT, 0.0, RADIUS_SM),
        text_color: Some(t.ink_faint),
        shadow: no_shadow(),
    }
}

pub fn table_header(active: bool) -> impl Fn(&Theme, button::Status) -> button::Style {
    move |theme, status| {
        let t = tones(theme);
        let ink = if active {
            t.ink
        } else if matches!(status, button::Status::Hovered | button::Status::Pressed) {
            t.ink_muted
        } else {
            t.ink_faint
        };
        button::Style {
            background: Some(Background::Color(Color::TRANSPARENT)),
            text_color: ink,
            border: border(Color::TRANSPARENT, 0.0, RADIUS_SM),
            shadow: no_shadow(),
        }
    }
}

pub fn table_row(theme: &Theme, status: button::Status) -> button::Style {
    let t = tones(theme);
    let background = match status {
        button::Status::Hovered => t.surface_hover,
        button::Status::Pressed => t.sunken,
        _ => Color::TRANSPARENT,
    };
    button::Style {
        background: Some(Background::Color(background)),
        text_color: t.ink,
        border: border(Color::TRANSPARENT, 0.0, RADIUS_SM),
        shadow: no_shadow(),
    }
}

pub struct ChartPalette {
    pub grid: Color,
    pub machine_line: Color,
    pub machine_fill: Color,
    pub managed_line: Color,
    pub managed_fill: Color,
}

pub fn chart(theme: &Theme) -> ChartPalette {
    let t = tones(theme);
    ChartPalette {
        grid: t.line,
        machine_line: t.ink_faint,
        machine_fill: dim(t.ink_faint, 0.16),
        managed_line: t.accent,
        managed_fill: dim(t.accent, 0.35),
    }
}

pub struct SparkPalette {
    pub line: Color,
    pub fill: Color,
}

pub fn spark(tone: Tone) -> impl Fn(&Theme) -> SparkPalette {
    move |theme| {
        let color = tone_color(tones(theme), tone);
        SparkPalette {
            line: color,
            fill: dim(color, 0.22),
        }
    }
}

pub struct GaugePalette {
    pub track: Color,
    pub fill: Color,
}

pub fn gauge(tone: Tone) -> impl Fn(&Theme) -> GaugePalette {
    move |theme| {
        let t = tones(theme);
        GaugePalette {
            track: t.line,
            fill: tone_color(t, tone),
        }
    }
}
