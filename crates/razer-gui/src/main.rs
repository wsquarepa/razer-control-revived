mod daemon;
mod pages;
mod telemetry;
mod theme;
mod tray;
mod widgets;

use daemon::DaemonError;
use iced::widget::{button, column, container, scrollable, text};
use iced::{Element, Fill, Subscription, Task, Theme, window};
use serde::Deserialize;

fn main() -> iced::Result {
    env_logger::init();
    iced::daemon(App::new, App::update, App::view)
        .title(App::title)
        .theme(App::theme)
        .subscription(App::subscription)
        .run()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Page {
    Overview,
    Performance,
    Gpu,
    Lighting,
    Battery,
    About,
}

const PAGES: [(Page, &str); 6] = [
    (Page::Overview, "Overview"),
    (Page::Performance, "Performance"),
    (Page::Gpu, "GPU"),
    (Page::Lighting, "Lighting"),
    (Page::Battery, "Battery"),
    (Page::About, "About"),
];

/// What the GUI may show for this laptop, resolved once at startup from the
/// daemon's device name matched against the packaged laptops.json.
#[derive(Debug, Clone)]
struct Capabilities {
    device_name: String,
    is_2025: bool,
    has_logo: bool,
    has_bho: bool,
    can_boost: bool,
    fan_range: (u16, u16),
}

#[derive(Debug, Deserialize)]
struct DeviceEntry {
    name: String,
    pid: String,
    features: Vec<String>,
    fan: Vec<u16>,
}

fn device_file_path() -> String {
    std::env::var("RAZER_DEVICE_FILE")
        .unwrap_or_else(|_| "/usr/share/razercontrol/laptops.json".to_string())
}

impl Capabilities {
    fn load() -> Result<Capabilities, DaemonError> {
        let device_name = daemon::device_name()?;
        let path = device_file_path();
        let contents = std::fs::read_to_string(&path)
            .map_err(|e| DaemonError::Io(format!("read device file {path}: {e}")))?;
        let devices: Vec<DeviceEntry> = serde_json::from_str(&contents)
            .map_err(|e| DaemonError::Io(format!("parse device file {path}: {e}")))?;
        let entry = devices
            .into_iter()
            .find(|d| d.name == device_name)
            .ok_or_else(|| {
                DaemonError::Io(format!("device {device_name:?} not found in {path}"))
            })?;
        Ok(Capabilities {
            device_name,
            is_2025: entry.pid.eq_ignore_ascii_case("02C6"),
            has_logo: entry.features.iter().any(|f| f == "logo"),
            has_bho: entry.features.iter().any(|f| f == "bho"),
            can_boost: entry.features.iter().any(|f| f == "boost"),
            fan_range: (
                entry.fan.first().copied().unwrap_or(3300),
                entry.fan.get(1).copied().unwrap_or(5400),
            ),
        })
    }
}

#[derive(Debug, Clone)]
enum Message {
    WindowOpened(window::Id),
    CloseRequested(window::Id),
    Navigate(Page),
    Bootstrapped(Result<Capabilities, DaemonError>),
    Retry,
    Status(String),
    StatusExpired,
    About(pages::about::Message),
    Overview(pages::overview::Message),
    Performance(pages::performance::Message),
    Gpu(pages::gpu::Message),
    Lighting(pages::lighting::Message),
    Battery(pages::battery::Message),
    Telemetry(telemetry::Snapshot),
    Tray(tray::TrayEvent),
}

enum Connection {
    Connecting,
    Connected(Capabilities),
    Unreachable(String),
}

struct App {
    window: Option<window::Id>,
    page: Page,
    connection: Connection,
    status: Option<String>,
    telemetry: telemetry::Snapshot,
    overview: pages::overview::State,
    performance: pages::performance::State,
    gpu: pages::gpu::State,
    lighting: pages::lighting::State,
    battery: pages::battery::State,
    tray_active: bool,
}

fn bootstrap() -> Task<Message> {
    Task::perform(daemon::blocking(Capabilities::load), Message::Bootstrapped)
}

// Called by page tasks (Tasks 7-11) that emit a transient status line.
fn status_task(message: String) -> Task<Message> {
    Task::batch([
        Task::done(Message::Status(message)),
        Task::perform(
            tokio::time::sleep(std::time::Duration::from_secs(3)),
            |_| Message::StatusExpired,
        ),
    ])
}

fn open_window() -> Task<Message> {
    let (_id, open) = window::open(window::Settings {
        size: iced::Size::new(980.0, 680.0),
        min_size: Some(iced::Size::new(760.0, 520.0)),
        // Without this, iced_winit closes the window itself on the OS close
        // button and never forwards the event to `window::close_requests()`,
        // so `Message::CloseRequested` (and the tray_active close-to-tray
        // branch) would never run.
        exit_on_close_request: false,
        platform_specific: window::settings::PlatformSpecific {
            // Wayland compositors match windows to desktop entries by app id,
            // which must equal the desktop file basename (razer-gui.desktop);
            // iced's derived default ("razer_gui") breaks focus and taskbar
            // association on KWin, as the GTK app's changelog records.
            application_id: "razer-gui".to_string(),
            ..window::settings::PlatformSpecific::default()
        },
        ..window::Settings::default()
    });
    open.map(Message::WindowOpened)
}

impl App {
    fn new() -> (Self, Task<Message>) {
        let tray_active = tray::spawn();
        let app = Self {
            window: None,
            page: Page::Overview,
            connection: Connection::Connecting,
            status: None,
            telemetry: telemetry::Snapshot::EMPTY,
            overview: pages::overview::State::new(),
            performance: pages::performance::State::new(),
            gpu: pages::gpu::State::new(),
            lighting: pages::lighting::State::new(),
            battery: pages::battery::State::new(),
            tray_active,
        };
        (app, Task::batch([open_window(), bootstrap()]))
    }

    fn title(&self, _window: window::Id) -> String {
        "Razer Control".to_string()
    }

    fn theme(&self, _window: window::Id) -> Theme {
        theme::app_theme()
    }

    fn subscription(&self) -> Subscription<Message> {
        Subscription::batch([
            window::close_requests().map(Message::CloseRequested),
            telemetry::subscription().map(Message::Telemetry),
            tray::subscription().map(Message::Tray),
        ])
    }

    fn update(&mut self, message: Message) -> Task<Message> {
        match message {
            Message::WindowOpened(id) => {
                self.window = Some(id);
                telemetry::WINDOW_VISIBLE.store(true, std::sync::atomic::Ordering::Relaxed);
                Task::none()
            }
            Message::CloseRequested(id) => {
                self.window = None;
                telemetry::WINDOW_VISIBLE.store(false, std::sync::atomic::Ordering::Relaxed);
                if self.tray_active {
                    window::close(id)
                } else {
                    Task::batch([window::close(id), iced::exit()])
                }
            }
            Message::Navigate(page) => {
                self.page = page;
                Task::none()
            }
            Message::Bootstrapped(Ok(capabilities)) => {
                let ac = telemetry::read_ac_online(std::path::Path::new("/sys/class/power_supply"))
                    .unwrap_or(true);
                let has_bho = capabilities.has_bho;
                self.connection = Connection::Connected(capabilities);
                let mut tasks = vec![
                    pages::overview::load(ac).map(Message::Overview),
                    pages::performance::load(ac).map(Message::Performance),
                    pages::gpu::load().map(Message::Gpu),
                    pages::lighting::load(ac).map(Message::Lighting),
                ];
                if has_bho {
                    tasks.push(pages::battery::load().map(Message::Battery));
                }
                Task::batch(tasks)
            }
            Message::Bootstrapped(Err(error)) => {
                self.connection = Connection::Unreachable(error.to_string());
                Task::none()
            }
            Message::Retry => {
                self.connection = Connection::Connecting;
                bootstrap()
            }
            Message::Status(status) => {
                self.status = Some(status);
                Task::none()
            }
            Message::StatusExpired => {
                self.status = None;
                Task::none()
            }
            Message::About(message) => {
                pages::about::update(message);
                Task::none()
            }
            Message::Overview(message) => {
                let follow_up = match &message {
                    pages::overview::Message::ProfileApplied(Ok(())) => {
                        let ac = telemetry::read_ac_online(std::path::Path::new(
                            "/sys/class/power_supply",
                        ))
                        .unwrap_or(true);
                        Task::batch([
                            status_task("Profile applied".to_string()),
                            pages::performance::load(ac).map(Message::Performance),
                        ])
                    }
                    pages::overview::Message::ProfileApplied(Err(error)) => {
                        status_task(format!("Profile change failed: {error}"))
                    }
                    pages::overview::Message::PreflightFinished(Ok(state)) => status_task(format!(
                        "Preflight finished: thermal safety {}",
                        razer_core::thermal_safety_label(*state)
                    )),
                    pages::overview::Message::PreflightFinished(Err(error)) => {
                        status_task(format!("Preflight failed: {error}"))
                    }
                    pages::overview::Message::BrightnessApplied(Err(error)) => {
                        status_task(format!("Brightness change failed: {error}"))
                    }
                    _ => Task::none(),
                };
                Task::batch([
                    pages::overview::update(&mut self.overview, message).map(Message::Overview),
                    follow_up,
                ])
            }
            Message::Performance(message) => {
                let Connection::Connected(capabilities) = &self.connection else {
                    return Task::none();
                };
                let follow_up = match &message {
                    pages::performance::Message::Applied(Ok(())) => {
                        status_task("Applied".to_string())
                    }
                    pages::performance::Message::Applied(Err(error)) => {
                        status_task(format!("Change failed: {error}"))
                    }
                    _ => Task::none(),
                };
                let capabilities = capabilities.clone();
                Task::batch([
                    pages::performance::update(&mut self.performance, message, &capabilities)
                        .map(Message::Performance),
                    follow_up,
                ])
            }
            Message::Gpu(message) => {
                let follow_up = match &message {
                    // envycontrol's reply text tells the user a reboot is needed.
                    pages::gpu::Message::Applied(Ok(text)) => status_task(text.clone()),
                    pages::gpu::Message::Applied(Err(error)) => {
                        status_task(format!("GPU change failed: {error}"))
                    }
                    _ => Task::none(),
                };
                Task::batch([
                    pages::gpu::update(&mut self.gpu, message).map(Message::Gpu),
                    follow_up,
                ])
            }
            Message::Lighting(message) => {
                let follow_up = match &message {
                    pages::lighting::Message::Applied(Ok(())) => status_task("Applied".to_string()),
                    pages::lighting::Message::Applied(Err(error)) => {
                        status_task(format!("Lighting change failed: {error}"))
                    }
                    _ => Task::none(),
                };
                Task::batch([
                    pages::lighting::update(&mut self.lighting, message).map(Message::Lighting),
                    follow_up,
                ])
            }
            Message::Battery(message) => {
                let follow_up = match &message {
                    pages::battery::Message::Applied(Ok(())) => status_task("Applied".to_string()),
                    pages::battery::Message::Applied(Err(error)) => {
                        status_task(format!("Battery change failed: {error}"))
                    }
                    _ => Task::none(),
                };
                Task::batch([
                    pages::battery::update(&mut self.battery, message).map(Message::Battery),
                    follow_up,
                ])
            }
            Message::Telemetry(snapshot) => {
                // The daemon holds a separate keyboard brightness per power
                // source; re-read it whenever the live source flips so the
                // overview slider tracks the source it will write to.
                let reload = match snapshot.ac_online {
                    Some(ac) if !self.overview.loaded || ac != self.overview.ac => {
                        pages::overview::load(ac).map(Message::Overview)
                    }
                    _ => Task::none(),
                };
                self.telemetry = snapshot;
                reload
            }
            Message::Tray(tray::TrayEvent::Open) => match self.window {
                Some(id) => window::gain_focus(id),
                None => open_window(),
            },
            Message::Tray(tray::TrayEvent::Quit) => iced::exit(),
        }
    }

    fn view(&self, _window: window::Id) -> Element<'_, Message> {
        let content: Element<'_, Message> = match &self.connection {
            Connection::Connecting => {
                container(text("Connecting to daemon...").color(theme::MUTED))
                    .center(Fill)
                    .into()
            }
            Connection::Unreachable(error) => self.view_unreachable(error),
            Connection::Connected(capabilities) => self.view_page(capabilities),
        };
        let shell = iced::widget::row![self.view_sidebar(), content];
        let mut root = column![shell.height(Fill)];
        if let Some(status) = &self.status {
            root = root.push(
                container(text(status).size(12).color(theme::TEXT_BRIGHT))
                    .style(theme::card)
                    .padding([6, 12])
                    .width(Fill),
            );
        }
        root.into()
    }

    fn view_sidebar(&self) -> Element<'_, Message> {
        let mut items = column![].spacing(4).padding(10).width(150);
        for (page, label) in PAGES {
            items = items.push(
                button(text(label).size(13))
                    .style(theme::sidebar_item(self.page == page))
                    .on_press(Message::Navigate(page))
                    .width(Fill),
            );
        }
        container(items).style(theme::sidebar).height(Fill).into()
    }

    fn view_unreachable<'a>(&self, error: &'a str) -> Element<'a, Message> {
        container(
            column![
                text("Daemon not running")
                    .size(20)
                    .color(theme::TEXT_BRIGHT),
                text(error).size(12).color(theme::MUTED),
                text("Start it with: systemctl --user start razerdaemon")
                    .size(12)
                    .color(theme::MUTED),
                button(text("Retry")).on_press(Message::Retry),
            ]
            .spacing(10)
            .align_x(iced::Center),
        )
        .center(Fill)
        .into()
    }

    fn view_page<'a>(&'a self, capabilities: &'a Capabilities) -> Element<'a, Message> {
        let body: Element<'_, Message> = match self.page {
            Page::About => pages::about::view(&capabilities.device_name).map(Message::About),
            Page::Overview => pages::overview::view(&self.overview, &self.telemetry, capabilities)
                .map(Message::Overview),
            Page::Performance => {
                pages::performance::view(&self.performance, capabilities, &self.telemetry)
                    .map(Message::Performance)
            }
            Page::Gpu => pages::gpu::view(&self.gpu).map(Message::Gpu),
            Page::Lighting => {
                pages::lighting::view(&self.lighting, capabilities).map(Message::Lighting)
            }
            Page::Battery => {
                pages::battery::view(&self.battery, capabilities).map(Message::Battery)
            }
        };
        scrollable(container(body).padding(18).width(Fill))
            .height(Fill)
            .into()
    }
}
