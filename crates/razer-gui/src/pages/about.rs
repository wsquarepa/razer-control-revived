use crate::theme;
use crate::widgets::section;
use iced::widget::{button, column, text};
use iced::{Element, Fill};

pub const REPO_URL: &str = "https://github.com/encomjp/razer-control";
pub const DONATE_URL: &str = "https://www.paypal.com/donate/?hosted_button_id=H4SCC24R8KS4A";

#[derive(Debug, Clone)]
pub enum Message {
    OpenUrl(&'static str),
}

pub fn update(message: Message) {
    match message {
        Message::OpenUrl(url) => {
            if let Err(error) = std::process::Command::new("xdg-open").arg(url).spawn() {
                log::warn!("failed to open {url}: {error}");
            }
        }
    }
}

pub fn view(device_name: &str) -> Element<'_, Message> {
    let info = section(
        Some("About"),
        vec![
            text(format!("Razer Control {}", env!("CARGO_PKG_VERSION")))
                .size(16)
                .color(theme::TEXT_BRIGHT)
                .into(),
            text(format!("Device: {device_name}"))
                .size(13)
                .color(theme::TEXT)
                .into(),
        ],
    );
    let links = section(
        Some("Links"),
        vec![
            button(text("Source repository").size(13))
                .on_press(Message::OpenUrl(REPO_URL))
                .style(button::text)
                .into(),
            button(text("Support development").size(13))
                .on_press(Message::OpenUrl(DONATE_URL))
                .style(button::text)
                .into(),
        ],
    );
    column![info, links].spacing(14).width(Fill).into()
}
