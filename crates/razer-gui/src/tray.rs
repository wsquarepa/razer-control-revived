use crate::telemetry::{self, Snapshot};
use iced::Subscription;
use iced::futures::channel::mpsc;
use ksni::blocking::TrayMethods;
use std::sync::OnceLock;

#[derive(Debug, Clone, Copy)]
pub enum TrayEvent {
    Open,
    Quit,
}

/// The tray thread produces events before the iced subscription machinery
/// exists, so the sender side lives in a process-wide slot the subscription
/// fills on startup. Events fired in the tiny window before that are dropped,
/// which can only lose a click that raced the app's own launch.
static TRAY_TX: OnceLock<mpsc::UnboundedSender<TrayEvent>> = OnceLock::new();

fn send(event: TrayEvent) {
    if let Some(tx) = TRAY_TX.get()
        && tx.unbounded_send(event).is_err()
    {
        log::warn!("tray event dropped: app stream closed");
    }
}

pub fn subscription() -> Subscription<TrayEvent> {
    Subscription::run(|| {
        let (tx, rx) = mpsc::unbounded();
        if TRAY_TX.set(tx).is_err() {
            log::warn!("tray event channel initialized twice");
        }
        rx
    })
}

pub fn tooltip_lines(snapshot: &Snapshot) -> String {
    let mut lines: Vec<String> = Vec::new();
    if let Some(temp) = snapshot.cpu_temp_c {
        lines.push(format!("CPU: {temp:.0}°C"));
    }
    if let Some(gpu) = &snapshot.gpu {
        lines.push(format!("GPU: {:.0}°C", gpu.temp_c));
    }
    if let Some(thermal) = snapshot.thermal.as_ref().filter(|t| t.error.is_none()) {
        lines.push(format!(
            "Fan: {} / {} RPM",
            thermal.cpu_rpm, thermal.gpu_rpm
        ));
    }
    if let Some(battery) = &snapshot.battery {
        let mut line = format!("Battery {}%", battery.percent);
        if let Some(watts) = battery.watts {
            line.push_str(&format!(" · {:.1} W", watts.abs()));
        }
        if snapshot.ac_online == Some(true) {
            line.push_str(" · AC");
        }
        lines.push(line);
    }
    if lines.is_empty() {
        "Razer Control".to_string()
    } else {
        lines.join("\n")
    }
}

struct RazerTray;

impl ksni::Tray for RazerTray {
    fn id(&self) -> String {
        "razer-control".to_string()
    }

    fn title(&self) -> String {
        "Razer Control".to_string()
    }

    fn icon_name(&self) -> String {
        "razer-gui".to_string()
    }

    fn tool_tip(&self) -> ksni::ToolTip {
        let snapshot = telemetry::SHARED
            .lock()
            .expect("telemetry snapshot lock")
            .clone();
        ksni::ToolTip {
            title: "Razer Control".to_string(),
            description: tooltip_lines(&snapshot),
            ..ksni::ToolTip::default()
        }
    }

    fn menu(&self) -> Vec<ksni::MenuItem<Self>> {
        use ksni::menu::*;
        vec![
            StandardItem {
                label: "Open Razer Control".to_string(),
                activate: Box::new(|_: &mut RazerTray| send(TrayEvent::Open)),
                ..StandardItem::default()
            }
            .into(),
            MenuItem::Separator,
            StandardItem {
                label: "Quit".to_string(),
                activate: Box::new(|_: &mut RazerTray| send(TrayEvent::Quit)),
                ..StandardItem::default()
            }
            .into(),
        ]
    }
}

/// ksni 0.3.5's blocking API is `TrayMethods::spawn(self)` on the tray value
/// itself (there is no `TrayService` type in this release; that was an
/// earlier-API guess). `spawn()` returns `Result<Handle<T>, Error>` and the
/// `Handle` only holds a `Weak` back-reference: the strong `Arc` running the
/// service lives in the background thread `spawn` starts, so dropping the
/// handle does not stop the tray (confirmed by reading `ksni-0.3.5/src/blocking.rs`
/// and mirrored from the GTK app's own `Ok(_handle) => {}` on `main`).
pub fn spawn() -> bool {
    match RazerTray.spawn() {
        Ok(_handle) => true,
        Err(error) => {
            log::warn!("tray unavailable, window close will exit: {error}");
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::telemetry::{BatteryInfo, Snapshot};

    #[test]
    fn tooltip_summarizes_temps_fan_and_battery() {
        let mut snapshot = Snapshot::EMPTY;
        snapshot.cpu_temp_c = Some(72.4);
        snapshot.battery = Some(BatteryInfo {
            percent: 86,
            status: "Discharging".to_string(),
            watts: Some(-32.0),
        });
        snapshot.ac_online = Some(false);
        let lines = tooltip_lines(&snapshot);
        assert!(lines.contains("CPU: 72°C"), "got: {lines}");
        assert!(lines.contains("Battery 86%"), "got: {lines}");
        assert!(lines.contains("32.0 W"), "got: {lines}");
    }

    #[test]
    fn tooltip_with_no_data_names_the_app() {
        assert_eq!(tooltip_lines(&Snapshot::EMPTY), "Razer Control");
    }
}
