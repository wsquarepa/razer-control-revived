use crate::daemon::{self, DaemonError};
use crate::theme;
use crate::widgets::{section, setting_row};
use iced::widget::{column, slider, text, toggler};
use iced::{Element, Fill, Task};

pub const MIN_THRESHOLD: u8 = 50;
pub const MAX_THRESHOLD: u8 = 80;

pub fn clamp_threshold(value: u8) -> u8 {
    value.clamp(MIN_THRESHOLD, MAX_THRESHOLD)
}

pub struct State {
    loaded: bool,
    on: bool,
    threshold: u8,
    slider_threshold: u8,
}

impl State {
    pub fn new() -> State {
        State {
            loaded: false,
            on: false,
            threshold: MAX_THRESHOLD,
            slider_threshold: MAX_THRESHOLD,
        }
    }
}

#[derive(Debug, Clone)]
pub enum Message {
    Loaded(Result<(bool, u8), DaemonError>),
    Toggled(bool),
    ThresholdMoved(u8),
    ThresholdReleased,
    Applied(Result<(), DaemonError>),
}

pub fn load() -> Task<Message> {
    Task::perform(daemon::blocking(daemon::bho), Message::Loaded)
}

fn apply(on: bool, threshold: u8) -> Task<Message> {
    // Chained, not batched: a batched reload could read pre-write state.
    Task::perform(
        daemon::blocking(move || daemon::set_bho(on, clamp_threshold(threshold))),
        Message::Applied,
    )
    .chain(load())
}

pub fn update(state: &mut State, message: Message) -> Task<Message> {
    match message {
        Message::Loaded(Ok((on, threshold))) => {
            state.loaded = true;
            state.on = on;
            state.threshold = clamp_threshold(threshold);
            state.slider_threshold = state.threshold;
            Task::none()
        }
        Message::Loaded(Err(error)) => Task::done(Message::Applied(Err(error))),
        Message::Toggled(on) => apply(on, state.slider_threshold),
        Message::ThresholdMoved(value) => {
            state.slider_threshold = value;
            Task::none()
        }
        Message::ThresholdReleased => apply(state.on, state.slider_threshold),
        Message::Applied(_) => Task::none(),
    }
}

pub fn view(state: &State) -> Element<'_, Message> {
    if !state.loaded {
        return text("Loading battery settings...")
            .color(theme::MUTED)
            .into();
    }
    let mut rows: Vec<Element<'_, Message>> = vec![setting_row(
        "Limit Charging",
        "Cap maximum charge to extend battery lifespan",
        toggler(state.on).on_toggle(Message::Toggled).into(),
    )];
    if state.on {
        rows.push(setting_row(
            "Charge Limit",
            "Maximum battery charge level (%)",
            column![
                slider(
                    MIN_THRESHOLD..=MAX_THRESHOLD,
                    state.slider_threshold,
                    Message::ThresholdMoved,
                )
                .on_release(Message::ThresholdReleased)
                .step(5u8),
                text(format!("{}%", state.slider_threshold))
                    .size(11)
                    .color(theme::MUTED),
            ]
            .spacing(4)
            .width(220)
            .into(),
        ));
    }
    column![section(Some("Battery Health Optimizer"), rows)]
        .spacing(14)
        .width(Fill)
        .into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn threshold_stays_inside_the_synapse_bios_window() {
        // Synapse and the BIOS only accept 50..=80; the daemon does not clamp.
        assert_eq!(clamp_threshold(0), 50);
        assert_eq!(clamp_threshold(50), 50);
        assert_eq!(clamp_threshold(65), 65);
        assert_eq!(clamp_threshold(80), 80);
        assert_eq!(clamp_threshold(100), 80);
    }
}
