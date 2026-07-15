use crate::Capabilities;
use crate::daemon::{self, DaemonError};
use crate::theme;
use crate::widgets::{section, setting_row};
use iced::widget::{button, column, container, pick_list, row, slider, text};
use iced::{Color, Element, Fill, Task};
use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EffectChoice {
    Static,
    StaticGradient,
    WaveGradient,
    Breathing,
}

pub const EFFECTS: [EffectChoice; 4] = [
    EffectChoice::Static,
    EffectChoice::StaticGradient,
    EffectChoice::WaveGradient,
    EffectChoice::Breathing,
];

impl EffectChoice {
    fn from_index(index: u8) -> Option<EffectChoice> {
        EFFECTS.get(usize::from(index)).copied()
    }

    pub fn uses_second_color(self) -> bool {
        matches!(self, Self::StaticGradient | Self::WaveGradient)
    }
}

impl fmt::Display for EffectChoice {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Static => "Static",
            Self::StaticGradient => "Static Gradient",
            Self::WaveGradient => "Wave Gradient",
            Self::Breathing => "Breathing",
        })
    }
}

pub fn effect_params(
    effect: EffectChoice,
    color1: [u8; 3],
    color2: [u8; 3],
) -> (&'static str, Vec<u8>) {
    let [r, g, b] = color1;
    let [r2, g2, b2] = color2;
    match effect {
        EffectChoice::Static => ("static", vec![r, g, b]),
        EffectChoice::StaticGradient => ("static_gradient", vec![r, g, b, r2, g2, b2]),
        EffectChoice::WaveGradient => ("wave_gradient", vec![r, g, b, r2, g2, b2]),
        // The trailing byte is the breathing speed the GTK app always sent.
        EffectChoice::Breathing => ("breathing_single", vec![r, g, b, 10]),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogoMode {
    Off,
    On,
    Breathing,
}

pub const LOGO_MODES: [LogoMode; 3] = [LogoMode::Off, LogoMode::On, LogoMode::Breathing];

impl LogoMode {
    fn wire(self) -> u8 {
        match self {
            Self::Off => 0,
            Self::On => 1,
            Self::Breathing => 2,
        }
    }

    fn from_wire(wire: u8) -> Option<LogoMode> {
        LOGO_MODES.get(usize::from(wire)).copied()
    }
}

impl fmt::Display for LogoMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Off => "Off",
            Self::On => "On",
            Self::Breathing => "Breathing",
        })
    }
}

pub struct State {
    ac: bool,
    loaded: bool,
    slider_brightness: u8,
    logo: Option<LogoMode>,
    effect: EffectChoice,
    color1: [u8; 3],
    color2: [u8; 3],
}

impl State {
    pub fn new() -> State {
        State {
            ac: true,
            loaded: false,
            slider_brightness: 100,
            logo: None,
            effect: EffectChoice::Static,
            color1: [0, 229, 255],
            color2: [0, 119, 255],
        }
    }
}

#[derive(Debug, Clone)]
pub struct Loaded {
    pub brightness: u8,
    pub logo: Option<u8>,
    pub effect: u8,
    pub params: Vec<u8>,
}

#[derive(Debug, Clone)]
pub enum Message {
    SourceSelected(bool),
    Loaded(bool, Result<Loaded, DaemonError>),
    BrightnessMoved(u8),
    BrightnessReleased,
    LogoSelected(LogoMode),
    EffectSelected(EffectChoice),
    ColorChanged {
        second: bool,
        channel: usize,
        value: u8,
    },
    ApplyEffect,
    Applied(Result<(), DaemonError>),
}

pub fn load(ac: bool) -> Task<Message> {
    Task::perform(
        daemon::blocking(move || {
            let brightness = daemon::brightness(ac)?;
            // Logo state is only meaningful on devices that have one; the
            // caller hides the row via Capabilities, so a failure here is real.
            let logo = daemon::logo(ac).ok();
            let (effect, params) = daemon::standard_effect()?;
            Ok(Loaded {
                brightness,
                logo,
                effect,
                params,
            })
        }),
        move |result| Message::Loaded(ac, result),
    )
}

pub fn update(state: &mut State, message: Message) -> Task<Message> {
    match message {
        Message::SourceSelected(ac) => {
            state.ac = ac;
            state.loaded = false;
            load(ac)
        }
        Message::Loaded(ac, Ok(loaded)) => {
            state.ac = ac;
            state.loaded = true;
            state.slider_brightness = loaded.brightness;
            state.logo = loaded.logo.and_then(LogoMode::from_wire);
            if let Some(effect) = EffectChoice::from_index(loaded.effect) {
                state.effect = effect;
            }
            if loaded.params.len() >= 3 {
                state.color1 = [loaded.params[0], loaded.params[1], loaded.params[2]];
            }
            if loaded.params.len() >= 6 {
                state.color2 = [loaded.params[3], loaded.params[4], loaded.params[5]];
            }
            Task::none()
        }
        Message::Loaded(_, Err(error)) => Task::done(Message::Applied(Err(error))),
        Message::BrightnessMoved(value) => {
            state.slider_brightness = value;
            Task::none()
        }
        // Reloads chain after the write; a batch would race the reload
        // against the setter and could repaint pre-write state.
        Message::BrightnessReleased => {
            let (ac, value) = (state.ac, state.slider_brightness);
            Task::perform(
                daemon::blocking(move || daemon::set_brightness(ac, value)),
                Message::Applied,
            )
            .chain(load(ac))
        }
        Message::LogoSelected(mode) => {
            let ac = state.ac;
            Task::perform(
                daemon::blocking(move || daemon::set_logo(ac, mode.wire())),
                Message::Applied,
            )
            .chain(load(ac))
        }
        Message::EffectSelected(effect) => {
            state.effect = effect;
            Task::none()
        }
        Message::ColorChanged {
            second,
            channel,
            value,
        } => {
            let target = if second {
                &mut state.color2
            } else {
                &mut state.color1
            };
            target[channel] = value;
            Task::none()
        }
        Message::ApplyEffect => {
            let (name, params) = effect_params(state.effect, state.color1, state.color2);
            Task::perform(
                daemon::blocking(move || daemon::set_effect(name, params)),
                Message::Applied,
            )
        }
        Message::Applied(_) => Task::none(),
    }
}

fn color_editor(second: bool, rgb: [u8; 3]) -> Element<'static, Message> {
    let swatch = container(text(" "))
        .style(move |_: &iced::Theme| iced::widget::container::Style {
            background: Some(iced::Background::Color(Color::from_rgb8(
                rgb[0], rgb[1], rgb[2],
            ))),
            border: iced::Border {
                radius: 6.0.into(),
                width: 1.0,
                color: theme::BORDER,
            },
            ..iced::widget::container::Style::default()
        })
        .width(34)
        .height(34);
    let channel_slider = |channel: usize, label: &'static str| {
        row![
            text(label).size(11).color(theme::MUTED).width(14),
            slider(0..=255u8, rgb[channel], move |value| {
                Message::ColorChanged {
                    second,
                    channel,
                    value,
                }
            }),
            text(format!("{}", rgb[channel]))
                .size(11)
                .color(theme::MUTED)
                .width(30),
        ]
        .spacing(8)
        .align_y(iced::Center)
    };
    row![
        swatch,
        column![
            channel_slider(0, "R"),
            channel_slider(1, "G"),
            channel_slider(2, "B")
        ]
        .spacing(2)
        .width(240),
    ]
    .spacing(12)
    .align_y(iced::Center)
    .into()
}

pub fn view<'a>(state: &'a State, capabilities: &'a Capabilities) -> Element<'a, Message> {
    if !state.loaded {
        return text("Loading lighting...").color(theme::MUTED).into();
    }
    let source_button = |label: &'static str, ac: bool| {
        button(text(label).size(13))
            .style(theme::sidebar_item(state.ac == ac))
            .on_press(Message::SourceSelected(ac))
    };
    let toggle = row![source_button("AC", true), source_button("Battery", false)].spacing(6);

    let brightness = section(
        Some("Keyboard Brightness"),
        vec![setting_row(
            "Brightness Level",
            "Committed when the slider is released",
            column![
                slider(0..=100u8, state.slider_brightness, Message::BrightnessMoved)
                    .on_release(Message::BrightnessReleased),
                text(format!("{}%", state.slider_brightness))
                    .size(11)
                    .color(theme::MUTED),
            ]
            .spacing(4)
            .width(220)
            .into(),
        )],
    );

    let mut page = column![toggle, brightness].spacing(14).width(Fill);

    if capabilities.has_logo {
        page = page.push(section(
            Some("Logo"),
            vec![setting_row(
                "Logo Mode",
                "Control Razer logo lighting",
                pick_list(LOGO_MODES, state.logo, Message::LogoSelected).into(),
            )],
        ));
    }

    let mut effect_rows: Vec<Element<'_, Message>> = vec![
        setting_row(
            "Effect Type",
            "Applies to all keys; not tied to the power source",
            pick_list(EFFECTS, Some(state.effect), Message::EffectSelected).into(),
        ),
        setting_row("Primary Color", "", color_editor(false, state.color1)),
    ];
    if state.effect.uses_second_color() {
        effect_rows.push(setting_row(
            "Secondary Color",
            "For gradient effects",
            color_editor(true, state.color2),
        ));
    }
    effect_rows.push(
        container(button(text("Apply Effect")).on_press(Message::ApplyEffect))
            .align_x(iced::alignment::Horizontal::Right)
            .width(Fill)
            .into(),
    );
    page = page.push(section(Some("Keyboard Effects"), effect_rows));
    page.into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn effect_params_match_the_daemon_wire_format() {
        let c1 = [255, 0, 64];
        let c2 = [0, 128, 255];
        assert_eq!(
            effect_params(EffectChoice::Static, c1, c2),
            ("static", vec![255, 0, 64])
        );
        assert_eq!(
            effect_params(EffectChoice::StaticGradient, c1, c2),
            ("static_gradient", vec![255, 0, 64, 0, 128, 255]),
        );
        assert_eq!(
            effect_params(EffectChoice::WaveGradient, c1, c2),
            ("wave_gradient", vec![255, 0, 64, 0, 128, 255]),
        );
        // breathing carries a fixed trailing speed byte of 10, as on main
        assert_eq!(
            effect_params(EffectChoice::Breathing, c1, c2),
            ("breathing_single", vec![255, 0, 64, 10]),
        );
    }

    #[test]
    fn gradient_effects_expose_the_secondary_color() {
        assert!(!EffectChoice::Static.uses_second_color());
        assert!(EffectChoice::StaticGradient.uses_second_color());
        assert!(EffectChoice::WaveGradient.uses_second_color());
        assert!(!EffectChoice::Breathing.uses_second_color());
    }
}
