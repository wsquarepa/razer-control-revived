#[allow(dead_code)]
mod daemon;
#[allow(dead_code)]
mod telemetry;
mod theme;
#[allow(dead_code)]
mod widgets;

use iced::widget::{container, text};
use iced::{Element, Task, Theme, window};

#[derive(Debug, Clone)]
enum Message {
    WindowOpened(window::Id),
    CloseRequested(window::Id),
}

struct App {
    window: Option<window::Id>,
}

fn main() -> iced::Result {
    env_logger::init();
    iced::daemon(App::new, App::update, App::view)
        .title(App::title)
        .theme(App::theme)
        .subscription(App::subscription)
        .run()
}

impl App {
    fn new() -> (Self, Task<Message>) {
        let (_id, open) = window::open(window::Settings {
            size: iced::Size::new(980.0, 680.0),
            min_size: Some(iced::Size::new(760.0, 520.0)),
            ..window::Settings::default()
        });
        (Self { window: None }, open.map(Message::WindowOpened))
    }

    fn title(&self, _window: window::Id) -> String {
        "Razer Control".to_string()
    }

    fn theme(&self, _window: window::Id) -> Theme {
        theme::app_theme()
    }

    fn subscription(&self) -> iced::Subscription<Message> {
        window::close_requests().map(Message::CloseRequested)
    }

    fn update(&mut self, message: Message) -> Task<Message> {
        match message {
            Message::WindowOpened(id) => {
                self.window = Some(id);
                Task::none()
            }
            // Tray integration lands in Task 12; until then closing the window exits.
            Message::CloseRequested(id) => {
                self.window = None;
                iced::Task::batch([window::close(id), iced::exit()])
            }
        }
    }

    fn view(&self, _window: window::Id) -> Element<'_, Message> {
        container(text("Razer Control").color(theme::TEXT_BRIGHT).size(24))
            .center(iced::Fill)
            .into()
    }
}
