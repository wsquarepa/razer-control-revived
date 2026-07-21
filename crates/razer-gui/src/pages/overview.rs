use crate::Capabilities;
use crate::daemon::{self, DaemonError};
use crate::telemetry::Snapshot;
use crate::theme;
use crate::widgets::{Gauge, gauge_card, gauge_fraction};
use iced::widget::{button, column, container, pick_list, row, text};
use iced::{Element, Fill, Task};
use razer_core::ThermalSafetyStateDto;
use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ModeChoice {
    pub label: &'static str,
    pub wire: u8,
}

impl fmt::Display for ModeChoice {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.label)
    }
}

const fn choice(label: &'static str, wire: u8) -> ModeChoice {
    ModeChoice { label, wire }
}

/// The `(label, wire)` power-mode lists carried over from the GTK app on main.
/// The 2025 SKU offers different modes on AC vs battery with non-sequential
/// wire values; legacy models keep the sequential 0..=4 set on both sources.
pub fn mode_choices(is_2025: bool, ac: bool) -> Vec<ModeChoice> {
    if is_2025 {
        if ac {
            vec![
                choice("Balanced", 0),
                choice("Silent", 5),
                choice("Maximum Performance", 2),
                choice("Custom", 4),
                choice("Hyperboost", 7),
            ]
        } else {
            vec![choice("Balanced", 6), choice("Battery Saver", 3)]
        }
    } else {
        vec![
            choice("Balanced", 0),
            choice("Performance", 1),
            choice("Studio", 2),
            choice("Silent", 3),
            choice("Custom", 4),
        ]
    }
}

#[derive(Debug, Clone)]
pub enum Message {
    ProfileSelected(ModeChoice),
    ProfileApplied(Result<(), DaemonError>),
    RunPreflight,
    PreflightFinished(Result<ThermalSafetyStateDto, DaemonError>),
}

pub fn update(message: Message) -> Task<Message> {
    match message {
        Message::ProfileSelected(mode) => Task::perform(
            daemon::blocking(move || {
                // The quick switcher edits the live power source's profile and
                // keeps its current boost levels; `power` reads them first.
                let ac = crate::telemetry::SHARED
                    .lock()
                    .expect("telemetry snapshot lock")
                    .ac_online
                    .unwrap_or(true);
                let (_, cpu, gpu) = daemon::power(ac)?;
                daemon::set_power(ac, mode.wire, cpu, gpu)
            }),
            Message::ProfileApplied,
        ),
        Message::RunPreflight => Task::perform(
            daemon::blocking(daemon::run_preflight),
            Message::PreflightFinished,
        ),
        // main.rs turns these results into status lines; nothing to store here.
        Message::ProfileApplied(_) | Message::PreflightFinished(_) => Task::none(),
    }
}

/// Tachometer display scale: zero to the mode's manual-set maximum. The
/// manual-set MINIMUM must not bound the display: idle fans spin well below
/// it (or are stopped at 0 rpm), which would render an empty arc under a
/// min-based scale.
pub fn fan_display_max(is_2025: bool, performance_mode: u8, device_range: (u16, u16)) -> u16 {
    if is_2025 {
        razer_core::provisional_rpm_range(performance_mode).1
    } else {
        device_range.1
    }
}

fn cards<'a>(snapshot: &'a Snapshot, capabilities: &'a Capabilities) -> Vec<Element<'a, Message>> {
    let gpu = snapshot.gpu.as_ref();
    let gpu_unit = |unit: &str| {
        if snapshot.gpu_suspended {
            "SUSPENDED".to_string()
        } else {
            unit.to_string()
        }
    };
    let thermal = snapshot.thermal.as_ref().filter(|t| t.error.is_none());
    let fan_max = if capabilities.is_2025 {
        thermal.map(|t| fan_display_max(true, t.performance_mode, capabilities.fan_range))
    } else {
        Some(fan_display_max(false, 0, capabilities.fan_range))
    };
    let fan_gauge = |rpm: Option<u16>| Gauge {
        fraction: rpm
            .zip(fan_max)
            .map(|(rpm, max)| gauge_fraction(f32::from(rpm), 0.0, f32::from(max))),
        value: rpm.map_or_else(String::new, |r| r.to_string()),
        unit: "RPM".to_string(),
        warm: false,
    };
    vec![
        gauge_card(
            "CPU Usage",
            Gauge {
                fraction: snapshot.cpu_usage_percent.map(|p| p / 100.0),
                value: snapshot
                    .cpu_usage_percent
                    .map_or_else(String::new, |p| format!("{p:.0}%")),
                unit: "LOAD".to_string(),
                warm: false,
            },
        ),
        gauge_card(
            "GPU Usage",
            Gauge {
                fraction: gpu.map(|g| g.usage_percent / 100.0),
                value: gpu.map_or_else(String::new, |g| format!("{:.0}%", g.usage_percent)),
                unit: gpu_unit("LOAD"),
                warm: false,
            },
        ),
        gauge_card(
            "CPU Watts",
            Gauge {
                // 120 W spans the sustained package range of every supported SKU.
                fraction: snapshot.cpu_watts.map(|w| gauge_fraction(w, 0.0, 120.0)),
                value: snapshot
                    .cpu_watts
                    .map_or_else(String::new, |w| format!("{w:.0}")),
                unit: "WATTS".to_string(),
                warm: false,
            },
        ),
        gauge_card(
            "GPU Watts",
            Gauge {
                fraction: gpu.map(|g| gauge_fraction(g.watts, 0.0, 175.0)),
                value: gpu.map_or_else(String::new, |g| format!("{:.0}", g.watts)),
                unit: gpu_unit("WATTS"),
                warm: false,
            },
        ),
        gauge_card("CPU Fan", fan_gauge(thermal.map(|t| t.cpu_rpm))),
        gauge_card("GPU Fan", fan_gauge(thermal.map(|t| t.gpu_rpm))),
        gauge_card(
            "CPU Temp",
            Gauge {
                fraction: snapshot.cpu_temp_c.map(|t| gauge_fraction(t, 30.0, 100.0)),
                value: snapshot
                    .cpu_temp_c
                    .map_or_else(String::new, |t| format!("{t:.0}°")),
                unit: "CELSIUS".to_string(),
                warm: true,
            },
        ),
        gauge_card(
            "GPU Temp",
            Gauge {
                fraction: gpu.map(|g| gauge_fraction(g.temp_c, 30.0, 95.0)),
                value: gpu.map_or_else(String::new, |g| format!("{:.0}°", g.temp_c)),
                unit: gpu_unit("CELSIUS"),
                warm: true,
            },
        ),
    ]
}

fn status_strip<'a>(
    snapshot: &'a Snapshot,
    capabilities: &'a Capabilities,
) -> Element<'a, Message> {
    let ac = snapshot.ac_online.unwrap_or(true);
    let choices = mode_choices(capabilities.is_2025, ac);
    let selected = snapshot.thermal.as_ref().and_then(|t| {
        choices
            .iter()
            .copied()
            .find(|c| c.wire == t.performance_mode)
    });
    let profile = container(crate::widgets::setting_row(
        "Power profile",
        "Applies to the current power source",
        pick_list(choices, selected, Message::ProfileSelected).into(),
    ))
    .style(theme::card)
    .padding(12)
    .width(Fill);

    let battery: Element<'_, Message> = match &snapshot.battery {
        Some(info) => {
            let mut parts =
                row![text(format!("{}%", info.percent)).color(theme::TEXT_BRIGHT)].spacing(8);
            if let Some(watts) = info.watts {
                let color = if watts < 0.0 {
                    theme::DANGER
                } else {
                    theme::ACCENT
                };
                parts = parts.push(text(format!("{watts:+.1} W")).color(color));
            }
            parts = parts.push(text(if ac { "AC" } else { "Battery" }).color(theme::MUTED));
            parts.into()
        }
        None => text("No battery detected").color(theme::MUTED).into(),
    };
    let battery = container(crate::widgets::setting_row("Battery", "", battery))
        .style(theme::card)
        .padding(12)
        .width(Fill);

    row![profile, battery].spacing(12).into()
}

fn danger_pane<'a>() -> Element<'a, Message> {
    container(crate::widgets::setting_row(
        "Force preflight re-run",
        "Re-probes the EC and overwrites the thermal-safety posture with the \
         result; a manual fan target and its monitoring are dropped",
        button(text("Re-run preflight").size(13))
            .style(theme::danger_button)
            .on_press(Message::RunPreflight)
            .into(),
    ))
    .style(theme::danger_card)
    .padding(12)
    .width(Fill)
    .into()
}

pub fn view<'a>(snapshot: &'a Snapshot, capabilities: &'a Capabilities) -> Element<'a, Message> {
    let mut grid = column![].spacing(12);
    let mut cards = cards(snapshot, capabilities);
    // Two rows of four. The spec's 4-to-2 responsive collapse is deferred:
    // iced's `responsive` demands 'static content, so it needs window-resize
    // plumbing through App state; the min window width keeps 4-wide readable.
    while !cards.is_empty() {
        let row_cards: Vec<_> = cards.drain(..cards.len().min(4)).collect();
        let mut r = row![].spacing(12);
        for card in row_cards {
            r = r.push(card);
        }
        grid = grid.push(r);
    }
    column![grid, status_strip(snapshot, capabilities), danger_pane()]
        .spacing(14)
        .width(Fill)
        .into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mode_choices_match_the_legacy_gui_lists() {
        let ac_2025: Vec<u8> = mode_choices(true, true).iter().map(|c| c.wire).collect();
        assert_eq!(ac_2025, vec![0, 5, 2, 4, 7]);
        let battery_2025: Vec<u8> = mode_choices(true, false).iter().map(|c| c.wire).collect();
        assert_eq!(battery_2025, vec![6, 3]);
        let legacy: Vec<u8> = mode_choices(false, true).iter().map(|c| c.wire).collect();
        assert_eq!(legacy, vec![0, 1, 2, 3, 4]);
        assert_eq!(mode_choices(false, true), mode_choices(false, false));
    }

    #[test]
    fn fan_display_max_scales_from_zero_to_the_mode_maximum() {
        assert_eq!(fan_display_max(true, 0, (0, 0)), 5200);
        assert_eq!(fan_display_max(true, 2, (0, 0)), 5400);
        assert_eq!(fan_display_max(false, 0, (3500, 5000)), 5000);
        // idle fans (~2100 rpm) must land in the lower part of the arc, not at 0
        let fraction = gauge_fraction(2100.0, 0.0, 5200.0);
        assert!((0.3..0.5).contains(&fraction), "got {fraction}");
    }
}
