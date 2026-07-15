use crate::Capabilities;
use crate::daemon::{self, DaemonError};
use crate::pages::overview::{ModeChoice, mode_choices};
use crate::telemetry::Snapshot;
use crate::theme;
use crate::widgets::{section, setting_row};
use iced::widget::{button, column, pick_list, row, slider, text};
use iced::{Element, Fill, Task};
use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BoostChoice {
    pub label: &'static str,
    pub value: u8,
}

impl fmt::Display for BoostChoice {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.label)
    }
}

const fn boost(label: &'static str, value: u8) -> BoostChoice {
    BoostChoice { label, value }
}

const BOOST_2025: [BoostChoice; 4] = [
    boost("Low", 0),
    boost("Medium", 1),
    boost("High", 2),
    boost("Extreme", 3),
];
const BOOST_PLAIN: [BoostChoice; 3] = [boost("Low", 0), boost("Medium", 1), boost("High", 2)];
const BOOST_LEGACY: [BoostChoice; 4] = [
    boost("Low", 0),
    boost("Medium", 1),
    boost("High", 2),
    boost("Boost", 3),
];
const GPU_BOOST: [BoostChoice; 4] = [
    boost("Low", 0),
    boost("Medium", 1),
    boost("High", 2),
    boost("Extreme", 3),
];

pub fn boost_choices(is_2025: bool, can_boost: bool) -> &'static [BoostChoice] {
    if is_2025 {
        &BOOST_2025
    } else if can_boost {
        &BOOST_LEGACY
    } else {
        &BOOST_PLAIN
    }
}

pub fn fan_bounds(is_2025: bool, wire: u8, device_range: (u16, u16)) -> (u16, u16) {
    if is_2025 {
        razer_core::provisional_rpm_range(wire)
    } else {
        device_range
    }
}

/// A short description for a performance mode, keyed on the wire value
/// (carried over verbatim from the GTK app for parity).
fn mode_description(is_2025: bool, wire: u8) -> &'static str {
    if is_2025 {
        match wire {
            0 | 6 => "Good mix of performance and battery life",
            2 => "Maximum performance, higher power draw",
            3 => "Extends battery life, reduced performance",
            4 => "Manually tune CPU and GPU levels",
            5 => "Minimal noise, reduced performance",
            7 => "Highest wattage; Razer pairs this with the cooling pad, runs hot without it",
            _ => "",
        }
    } else {
        match wire {
            0 => "Good mix of performance and battery life",
            1 => "Maximum performance, higher power draw",
            2 => "Balanced clocks for sustained workloads",
            3 => "Minimal noise, reduced performance",
            4 => "Manually tune CPU and GPU levels",
            _ => "",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FanMode {
    Automatic,
    Manual,
}

impl fmt::Display for FanMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Automatic => "Automatic",
            Self::Manual => "Fixed Manual",
        })
    }
}

pub struct State {
    ac: bool,
    loaded: bool,
    wire: u8,
    cpu: u8,
    gpu: u8,
    /// Daemon-confirmed manual rpm; 0 means automatic.
    fan_rpm: i32,
    /// Local slider position; committed on release only.
    slider_rpm: u16,
}

impl State {
    pub fn new() -> State {
        State {
            ac: true,
            loaded: false,
            wire: 0,
            cpu: 0,
            gpu: 0,
            fan_rpm: 0,
            slider_rpm: 0,
        }
    }
}

#[derive(Debug, Clone)]
pub enum Message {
    SourceSelected(bool),
    Loaded(bool, Result<(u8, u8, u8, i32), DaemonError>),
    ProfileSelected(ModeChoice),
    CpuBoostSelected(BoostChoice),
    GpuBoostSelected(BoostChoice),
    FanModeSelected(FanMode),
    FanSliderMoved(u16),
    FanSliderReleased,
    Applied(Result<(), DaemonError>),
}

pub fn load(ac: bool) -> Task<Message> {
    Task::perform(
        daemon::blocking(move || {
            let (pwr, cpu, gpu) = daemon::power(ac)?;
            let fan = daemon::fan_speed(ac)?;
            Ok((pwr, cpu, gpu, fan))
        }),
        move |result| Message::Loaded(ac, result),
    )
}

fn apply(
    ac: bool,
    work: impl FnOnce() -> Result<(), DaemonError> + Send + 'static,
) -> Task<Message> {
    // Reload after every write: state only ever reflects daemon truth. The
    // reload is chained, not batched: a batch runs both tasks concurrently
    // and the reload could read the pre-write value.
    Task::perform(daemon::blocking(work), Message::Applied).chain(load(ac))
}

pub fn update(state: &mut State, message: Message, capabilities: &Capabilities) -> Task<Message> {
    match message {
        Message::SourceSelected(ac) => {
            state.ac = ac;
            state.loaded = false;
            load(ac)
        }
        Message::Loaded(ac, Ok((wire, cpu, gpu, fan_rpm))) => {
            let (min, _) = fan_bounds(capabilities.is_2025, wire, capabilities.fan_range);
            state.ac = ac;
            state.loaded = true;
            state.wire = wire;
            state.cpu = cpu;
            state.gpu = gpu;
            state.fan_rpm = fan_rpm;
            state.slider_rpm = if fan_rpm > 0 { fan_rpm as u16 } else { min };
            Task::none()
        }
        Message::Loaded(_, Err(error)) => {
            // main.rs surfaces the failure through the Applied interception;
            // reuse that path so the user sees one consistent status line.
            Task::done(Message::Applied(Err(error)))
        }
        Message::ProfileSelected(mode) => {
            let (ac, cpu, gpu) = (state.ac, state.cpu, state.gpu);
            apply(ac, move || daemon::set_power(ac, mode.wire, cpu, gpu))
        }
        Message::CpuBoostSelected(choice) => {
            let (ac, wire, gpu) = (state.ac, state.wire, state.gpu);
            apply(ac, move || daemon::set_power(ac, wire, choice.value, gpu))
        }
        Message::GpuBoostSelected(choice) => {
            let (ac, wire, cpu) = (state.ac, state.wire, state.cpu);
            apply(ac, move || daemon::set_power(ac, wire, cpu, choice.value))
        }
        Message::FanModeSelected(FanMode::Automatic) => {
            let ac = state.ac;
            apply(ac, move || daemon::set_fan_speed(ac, 0))
        }
        Message::FanModeSelected(FanMode::Manual) => {
            let ac = state.ac;
            let rpm = i32::from(state.slider_rpm);
            apply(ac, move || daemon::set_fan_speed(ac, rpm))
        }
        Message::FanSliderMoved(rpm) => {
            state.slider_rpm = rpm;
            Task::none()
        }
        Message::FanSliderReleased => {
            let ac = state.ac;
            let rpm = i32::from(state.slider_rpm);
            apply(ac, move || daemon::set_fan_speed(ac, rpm))
        }
        Message::Applied(_) => Task::none(),
    }
}

fn source_toggle(state: &State) -> Element<'_, Message> {
    let source_button = |label: &'static str, ac: bool| {
        button(text(label).size(13))
            .style(theme::sidebar_item(state.ac == ac))
            .on_press(Message::SourceSelected(ac))
    };
    row![source_button("AC", true), source_button("Battery", false)]
        .spacing(6)
        .into()
}

pub fn view<'a>(
    state: &'a State,
    capabilities: &'a Capabilities,
    snapshot: &'a Snapshot,
) -> Element<'a, Message> {
    if !state.loaded {
        return text("Loading profile...").color(theme::MUTED).into();
    }
    let choices = mode_choices(capabilities.is_2025, state.ac);
    let selected = choices.iter().copied().find(|c| c.wire == state.wire);

    let mut power_rows: Vec<Element<'_, Message>> = vec![setting_row(
        "Profile",
        mode_description(capabilities.is_2025, state.wire),
        pick_list(choices, selected, Message::ProfileSelected).into(),
    )];
    if state.wire == 4 {
        let cpu_choices = boost_choices(capabilities.is_2025, capabilities.can_boost);
        let cpu_selected = cpu_choices.iter().copied().find(|c| c.value == state.cpu);
        let gpu_selected = GPU_BOOST.iter().copied().find(|c| c.value == state.gpu);
        power_rows.push(setting_row(
            "CPU Performance",
            "Processor performance level",
            pick_list(cpu_choices, cpu_selected, Message::CpuBoostSelected).into(),
        ));
        power_rows.push(setting_row(
            "GPU Performance",
            "Graphics performance level",
            pick_list(
                GPU_BOOST.as_slice(),
                gpu_selected,
                Message::GpuBoostSelected,
            )
            .into(),
        ));
    }

    let fan_mode = if state.fan_rpm == 0 {
        FanMode::Automatic
    } else {
        FanMode::Manual
    };
    let (min, max) = fan_bounds(capabilities.is_2025, state.wire, capabilities.fan_range);
    let mut fan_rows: Vec<Element<'_, Message>> = vec![setting_row(
        "Fan Mode",
        "Firmware automatic control or a fixed manual speed",
        pick_list(
            [FanMode::Automatic, FanMode::Manual],
            Some(fan_mode),
            Message::FanModeSelected,
        )
        .into(),
    )];
    if fan_mode == FanMode::Manual {
        fan_rows.push(setting_row(
            "Fan Speed (RPM)",
            "Committed when the slider is released",
            column![
                slider(min..=max, state.slider_rpm, Message::FanSliderMoved)
                    .on_release(Message::FanSliderReleased)
                    .step(100u16),
                text(format!("{} RPM", state.slider_rpm))
                    .size(11)
                    .color(theme::MUTED),
            ]
            .spacing(4)
            .width(220)
            .into(),
        ));
    }
    let tachometer = match snapshot.thermal.as_ref().filter(|t| t.error.is_none()) {
        Some(status) => format!("CPU {} RPM · GPU {} RPM", status.cpu_rpm, status.gpu_rpm),
        None => "Telemetry unavailable".to_string(),
    };
    fan_rows.push(setting_row(
        "Current Fan Speed",
        "",
        text(tachometer).size(13).color(theme::ACCENT).into(),
    ));

    column![
        source_toggle(state),
        section(Some("Power Profile"), power_rows),
        section(Some("Cooling"), fan_rows),
    ]
    .spacing(14)
    .width(Fill)
    .into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn boost_lists_match_the_legacy_gui() {
        let labels =
            |choices: &[BoostChoice]| -> Vec<&str> { choices.iter().map(|c| c.label).collect() };
        assert_eq!(
            labels(boost_choices(true, false)),
            vec!["Low", "Medium", "High", "Extreme"]
        );
        assert_eq!(
            labels(boost_choices(false, false)),
            vec!["Low", "Medium", "High"]
        );
        assert_eq!(
            labels(boost_choices(false, true)),
            vec!["Low", "Medium", "High", "Boost"]
        );
    }

    #[test]
    fn fan_bounds_use_mode_ranges_on_2025_and_device_ranges_elsewhere() {
        assert_eq!(
            fan_bounds(true, 4, (0, 0)),
            razer_core::provisional_rpm_range(4)
        );
        assert_eq!(fan_bounds(false, 4, (3500, 5000)), (3500, 5000));
    }

    #[test]
    fn loaded_message_replaces_state_with_daemon_confirmed_values() {
        let capabilities = crate::Capabilities {
            device_name: "Blade 16 2025".to_string(),
            is_2025: true,
            has_logo: false,
            can_boost: false,
            fan_range: (3300, 5400),
        };
        let mut state = State::new();
        let _ = update(
            &mut state,
            Message::Loaded(true, Ok((2, 1, 3, 0))),
            &capabilities,
        );
        assert!(state.loaded);
        assert!(state.ac);
        assert_eq!(state.wire, 2);
        assert_eq!(state.cpu, 1);
        assert_eq!(state.gpu, 3);
        assert_eq!(state.fan_rpm, 0);
        // slider parks at the mode's lower bound while the fan is automatic
        assert_eq!(state.slider_rpm, razer_core::provisional_rpm_range(2).0);
    }
}
