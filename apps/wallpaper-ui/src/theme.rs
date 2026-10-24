//! Colours and widget styles.

use iced::border::{self, Border};
use iced::gradient::Linear;
use iced::widget::{container, text_input};
use iced::{Background, Color, Degrees, Gradient, Theme, color};

pub const BACKGROUND: Color = color!(0x16181d);
pub const TEXT: Color = color!(0xe3e5e8);
pub const MUTED: Color = color!(0x9aa0a8);
pub const PRIMARY: Color = color!(0x2f8cff);
pub const SUCCESS: Color = color!(0x3fb950);
pub const WARNING: Color = color!(0xd29922);
pub const DANGER: Color = color!(0xf85149);
pub const TOP_BAR: Color = color!(0x0f1114);
pub const PANEL: Color = color!(0x1d2027);
pub const RAISED: Color = color!(0x262a33);

pub const RADIUS: f32 = 4.0;

pub fn theme() -> Theme {
    Theme::custom(
        "Wallpaper Engine",
        iced::theme::Palette {
            background: BACKGROUND,
            text: TEXT,
            primary: PRIMARY,
            success: SUCCESS,
            warning: WARNING,
            danger: DANGER,
        },
    )
}

pub fn top_bar(_: &Theme) -> container::Style {
    container::Style::default().background(TOP_BAR)
}

pub fn panel(_: &Theme) -> container::Style {
    container::Style::default().background(PANEL)
}

/// A tile's frame: a blue border when selected.
pub fn tile(selected: bool, hovered: bool) -> container::Style {
    let border = if selected {
        Border {
            color: PRIMARY,
            width: 2.0,
            radius: RADIUS.into(),
        }
    } else if hovered {
        Border {
            color: Color { a: 0.5, ..TEXT },
            width: 1.0,
            radius: RADIUS.into(),
        }
    } else {
        border::rounded(RADIUS)
    };
    container::Style {
        background: Some(RAISED.into()),
        border,
        ..Default::default()
    }
}

/// The shade behind a tile's title, dark at the bottom.
pub fn title_shade(_: &Theme) -> container::Style {
    let gradient = Linear::new(Degrees(180.0))
        .add_stop(0.0, Color::TRANSPARENT)
        .add_stop(1.0, Color::from_rgba8(0, 0, 0, 0.85));
    container::Style {
        background: Some(Background::Gradient(Gradient::Linear(gradient))),
        border: border::rounded(border::bottom(RADIUS)),
        text_color: Some(TEXT),
        ..Default::default()
    }
}

/// A small coloured label (`VIDEO`, `MATURE`, ...).
pub fn badge(color: Color) -> impl Fn(&Theme) -> container::Style {
    move |_| container::Style {
        background: Some(Color { a: 0.85, ..color }.into()),
        border: border::rounded(3),
        text_color: Some(Color::WHITE),
        ..Default::default()
    }
}

pub fn banner(color: Color) -> impl Fn(&Theme) -> container::Style {
    move |_| container::Style {
        background: Some(Color { a: 0.18, ..color }.into()),
        border: Border {
            color,
            width: 1.0,
            radius: RADIUS.into(),
        },
        text_color: Some(TEXT),
        ..Default::default()
    }
}

pub fn search(theme: &Theme, status: text_input::Status) -> text_input::Style {
    let mut style = text_input::default(theme, status);
    style.background = RAISED.into();
    style.border.radius = RADIUS.into();
    if !matches!(status, text_input::Status::Focused { .. }) {
        style.border.color = Color::TRANSPARENT;
    }
    style
}
