use iced::widget::{button, container};
use iced::{Background, Border, Color, Theme, theme};

pub const BACKGROUND: Color = Color::from_rgb8(0x0a, 0x0e, 0x14);
pub const SIDEBAR: Color = Color::from_rgb8(0x0d, 0x14, 0x20);
pub const CARD: Color = Color::from_rgb8(0x11, 0x18, 0x23);
pub const BORDER: Color = Color::from_rgb8(0x1a, 0x25, 0x32);
pub const TEXT: Color = Color::from_rgb8(0xc8, 0xd6, 0xe5);
pub const TEXT_BRIGHT: Color = Color::from_rgb8(0xe8, 0xf4, 0xff);
pub const MUTED: Color = Color::from_rgb8(0x5a, 0x6a, 0x7a);
pub const ACCENT: Color = Color::from_rgb8(0x00, 0xe5, 0xff);
// The spec's gauge arc gradient (accent to ACCENT_DEEP) shipped as solid
// accent plus a translucent glow understroke instead, so rendering never
// reads this constant. It stays as the documented gradient stop and as
// lighting's default secondary color value (see pages/lighting.rs).
#[allow(dead_code)]
pub const ACCENT_DEEP: Color = Color::from_rgb8(0x00, 0x77, 0xff);
pub const WARM: Color = Color::from_rgb8(0xff, 0x9f, 0x43);
pub const DANGER: Color = Color::from_rgb8(0xff, 0x6b, 0x6b);

pub fn app_theme() -> Theme {
    Theme::custom(
        "Razer Neon".to_string(),
        theme::Palette {
            background: BACKGROUND,
            text: TEXT,
            primary: ACCENT,
            success: ACCENT,
            warning: WARM,
            danger: DANGER,
        },
    )
}

pub fn card(_theme: &Theme) -> container::Style {
    container::Style {
        background: Some(Background::Color(CARD)),
        border: Border {
            color: BORDER,
            width: 1.0,
            radius: 10.0.into(),
        },
        text_color: Some(TEXT),
        ..container::Style::default()
    }
}

pub fn sidebar(_theme: &Theme) -> container::Style {
    container::Style {
        background: Some(Background::Color(SIDEBAR)),
        ..container::Style::default()
    }
}

/// Active item: accent-tinted background only; deliberately no border accent.
pub fn sidebar_item(selected: bool) -> impl Fn(&Theme, button::Status) -> button::Style {
    move |_theme, status| {
        let tint = Color { a: 0.09, ..ACCENT };
        let hover = Color { a: 0.05, ..ACCENT };
        let background = if selected {
            Some(Background::Color(tint))
        } else if matches!(status, button::Status::Hovered) {
            Some(Background::Color(hover))
        } else {
            None
        };
        button::Style {
            background,
            text_color: if selected { ACCENT } else { MUTED },
            border: Border {
                radius: 6.0.into(),
                ..Border::default()
            },
            ..button::Style::default()
        }
    }
}
