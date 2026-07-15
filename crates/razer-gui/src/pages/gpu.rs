use crate::daemon::{self, DaemonError, GpuStatus};
use crate::theme;
use crate::widgets::{section, setting_row};
use iced::widget::{column, pick_list, text, toggler};
use iced::{Element, Fill, Task};
use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EnvyMode {
    Hybrid,
    Integrated,
    Nvidia,
}

impl EnvyMode {
    /// The exact strings `razer-daemon/src/gpu.rs` validates.
    pub fn wire(self) -> &'static str {
        match self {
            Self::Hybrid => "hybrid",
            Self::Integrated => "integrated",
            Self::Nvidia => "nvidia",
        }
    }

    pub fn from_wire(wire: &str) -> Option<EnvyMode> {
        match wire {
            "hybrid" => Some(Self::Hybrid),
            "integrated" => Some(Self::Integrated),
            "nvidia" => Some(Self::Nvidia),
            _ => None,
        }
    }
}

impl fmt::Display for EnvyMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Hybrid => "Hybrid",
            Self::Integrated => "Integrated",
            Self::Nvidia => "NVIDIA Only",
        })
    }
}

pub struct State {
    status: Option<GpuStatus>,
}

impl State {
    pub fn new() -> State {
        State { status: None }
    }
}

#[derive(Debug, Clone)]
pub enum Message {
    Loaded(Result<GpuStatus, DaemonError>),
    DgpuPmToggled(bool),
    ModeSelected(EnvyMode),
    Applied(Result<String, DaemonError>),
}

pub fn load() -> Task<Message> {
    Task::perform(daemon::blocking(daemon::gpu_status), Message::Loaded)
}

pub fn update(state: &mut State, message: Message) -> Task<Message> {
    match message {
        Message::Loaded(Ok(status)) => {
            state.status = Some(status);
            Task::none()
        }
        Message::Loaded(Err(error)) => Task::done(Message::Applied(Err(error))),
        // Reloads are chained after the write, never batched: a batch runs
        // both tasks concurrently and the reload could read pre-write state.
        Message::DgpuPmToggled(enabled) => Task::perform(
            daemon::blocking(move || {
                daemon::set_dgpu_runtime_pm(enabled)?;
                Ok("dGPU suspend updated".to_string())
            }),
            Message::Applied,
        )
        .chain(load()),
        Message::ModeSelected(mode) => Task::perform(
            daemon::blocking(move || daemon::set_gpu_mode(mode.wire())),
            Message::Applied,
        )
        .chain(load()),
        Message::Applied(_) => Task::none(),
    }
}

pub fn view(state: &State) -> Element<'_, Message> {
    let Some(status) = &state.status else {
        return text("Loading GPU status...").color(theme::MUTED).into();
    };
    let gpu_rows: Vec<Element<'_, Message>> = if status.gpus.is_empty() {
        vec![text("No GPUs detected").color(theme::MUTED).into()]
    } else {
        status
            .gpus
            .iter()
            .map(|gpu| {
                let kind = if gpu.gpu_type == "dgpu" {
                    "Discrete"
                } else {
                    "Integrated"
                };
                setting_row(
                    &gpu.name,
                    kind,
                    text(&gpu.runtime_status)
                        .size(12)
                        .color(theme::MUTED)
                        .into(),
                )
            })
            .collect()
    };
    let has_dgpu = status.gpus.iter().any(|gpu| gpu.gpu_type == "dgpu");
    let pm_row = setting_row(
        "Suspend dGPU",
        if has_dgpu {
            "Let the discrete GPU power down when idle"
        } else {
            "No discrete GPU detected"
        },
        toggler(status.dgpu_runtime_pm)
            .on_toggle_maybe(has_dgpu.then_some(Message::DgpuPmToggled))
            .into(),
    );
    let mut sections = vec![
        section(Some("Detected GPUs"), gpu_rows),
        section(Some("Power Management"), vec![pm_row]),
    ];
    if status.envycontrol_available {
        sections.push(section(
            Some("GPU Mode (envycontrol)"),
            vec![setting_row(
                "GPU Mode",
                "Switching requires a reboot",
                pick_list(
                    [EnvyMode::Hybrid, EnvyMode::Integrated, EnvyMode::Nvidia],
                    EnvyMode::from_wire(&status.envycontrol_mode),
                    Message::ModeSelected,
                )
                .into(),
            )],
        ));
    }
    let mut page = column![].spacing(14).width(Fill);
    for s in sections {
        page = page.push(s);
    }
    page.into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn envycontrol_labels_map_to_the_daemon_wire_modes() {
        assert_eq!(EnvyMode::Hybrid.wire(), "hybrid");
        assert_eq!(EnvyMode::Integrated.wire(), "integrated");
        assert_eq!(EnvyMode::Nvidia.wire(), "nvidia");
        assert_eq!(EnvyMode::from_wire("nvidia"), Some(EnvyMode::Nvidia));
        assert_eq!(EnvyMode::from_wire("what"), None);
    }
}
