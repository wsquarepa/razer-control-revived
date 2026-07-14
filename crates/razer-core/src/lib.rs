use serde::{Deserialize, Serialize};
use std::io::{Read, Write};
use std::net::Shutdown;
use std::os::unix::net::{UnixListener, UnixStream};
use libc::umask;

/// Razer laptop control socket path.
/// Prefer XDG_RUNTIME_DIR (/run/user/<uid>) which persists for the session.
/// Fall back to /tmp for AppImage or environments without XDG_RUNTIME_DIR.
pub fn socket_path() -> String {
    if let Ok(dir) = std::env::var("XDG_RUNTIME_DIR") {
        format!("{}/razercontrol-socket", dir)
    } else {
        "/tmp/razercontrol-socket".to_string()
    }
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct GpuInfo {
    pub name: String,
    pub pci_slot: String,
    pub driver: String,
    pub gpu_type: String,
    pub runtime_status: String,
}

/// The class of a thermal failure, carried over IPC without the daemon's full
/// transport/decode detail (that stays in the daemon log). Enough for a frontend
/// to explain why a reading or a write did not succeed.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub enum ThermalFailureCode {
    /// A command did not complete over the HID transport.
    Transport,
    /// A readback decoded for its own zone but did not match what was requested.
    ReadbackMismatch,
    /// A request was rejected by thermal policy (mode/level/RPM not allowed).
    Policy,
    /// The safety state machine is Disabled, so every write is refused.
    WritesDisabled,
}

/// A typed thermal failure delivered to a frontend: a machine-readable class and
/// a human-readable message with the debugging context already interpolated.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct ThermalFailureDto {
    pub code: ThermalFailureCode,
    pub message: String,
}

/// The daemon's live thermal-safety posture, projected for a frontend. The
/// `Manual` variant intentionally drops the target RPM and failure counter the
/// daemon tracks internally: a frontend only needs the posture, not the counter.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThermalSafetyStateDto {
    Preflight,
    Ready,
    Manual,
    Disabled,
}

/// Whether the fans run under firmware-automatic control or a fixed manual speed.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
pub enum FanControlModeDto {
    Automatic,
    Fixed,
}

/// Live thermal telemetry for a frontend.
///
/// `cpu_rpm` and `gpu_rpm` are meaningful only when `error` is `None`. A failed
/// tachometer read or transport failure sets `error` and leaves the rpm fields
/// unspecified; they are never a real zero passed off as a valid reading (that
/// legacy zero-on-failure sentinel is gone from the typed path).
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct ThermalStatus {
    pub safety_state: ThermalSafetyStateDto,
    pub performance_mode: u8,
    pub fan_mode: FanControlModeDto,
    pub cpu_rpm: u16,
    pub gpu_rpm: u16,
    pub error: Option<ThermalFailureDto>,
}

/// The outcome of a thermal setter (power mode or fan speed). A rejection carries
/// the reason so the frontend can surface why the write was refused instead of a
/// bare boolean.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub enum CommandResult {
    Applied,
    Rejected { reason: String },
}

/// Blade 16 2025 performance-mode display name for a wire value. Only the modes
/// this SKU offers have names; unrecognized values (including the hidden
/// mode-1 "Gaming ghost") are "Unknown". No pre-2025 label taxonomy is used.
#[allow(dead_code)]
pub fn performance_mode_label(wire_mode: u8) -> &'static str {
    match wire_mode {
        // 0 is the AC Balanced slot, 6 the battery slot; one logical mode.
        0 | 6 => "Balanced",
        2 => "Maximum Performance",
        3 => "Battery Saver",
        4 => "Custom",
        5 => "Silent",
        7 => "Hyperboost",
        _ => "Unknown",
    }
}

/// The stable KDE-widget contract rendering `Name (N)`: the trailing wire number
/// in parentheses is what the widget parses, so it must never be dropped.
#[allow(dead_code)]
pub fn performance_mode_display(wire_mode: u8) -> String {
    format!("{} ({})", performance_mode_label(wire_mode), wire_mode)
}

/// Provisional `(min, max)` fan RPM bounds for a Blade 16 2025 performance mode,
/// used only to bound the manual fan slider in a frontend. These mirror the
/// daemon's `thermal::provisional_rpm_range`; the daemon re-validates every fan
/// write against the same policy, so a stale UI bound can never cause an
/// out-of-range write. Unknown modes fall back to the widest common bound.
#[allow(dead_code)]
pub fn provisional_rpm_range(wire_mode: u8) -> (u16, u16) {
    match wire_mode {
        0 | 5 | 6 => (3400, 5200),
        2 | 3 => (3300, 5400),
        4 => (4000, 5300),
        7 => (3700, 5300),
        _ => (3300, 5400),
    }
}

#[derive(Serialize, Deserialize, Debug)]
/// Represents data sent TO the daemon
pub enum DaemonCommand {
    SetFanSpeed { ac: usize, rpm: i32 },      // Fan speed
    GetFanSpeed { ac: usize },                 // Get (Fan speed)
    SetPowerMode { ac: usize, pwr: u8, cpu: u8, gpu: u8}, // Power mode
    GetPwrLevel { ac: usize },                 // Get (Power mode)
    GetCPUBoost { ac: usize },                 // Get (CPU boost)
    GetGPUBoost { ac: usize },                 // Get (GPU boost)
    SetLogoLedState{ ac:usize, logo_state: u8 },
    GetLogoLedState { ac: usize },
    GetKeyboardRGB { layer: i32 }, // Layer ID
    SetEffect { name: String, params: Vec<u8> }, // Set keyboard colour
    SetStandardEffect { name: String, params: Vec<u8> }, // Set keyboard colour
    SetBrightness { ac:usize, val: u8 },
    SetIdle {ac: usize, val: u32 },
    GetBrightness { ac: usize },
    SetSync { sync: bool },
    GetSync (),
    SetBatteryHealthOptimizer { is_on: bool, threshold: u8 },
    GetBatteryHealthOptimizer (),
    GetDeviceName,
    GetThermalStatus,
    GetStandardEffect,
    GetGpuStatus,
    SetDgpuRuntimePM { enabled: bool },
    SetGpuMode { mode: String },
}

#[derive(Serialize, Deserialize, Debug)]
/// Represents data sent back from Daemon after it receives
/// a command.
pub enum DaemonResponse {
    SetFanSpeed { result: CommandResult },           // Response
    GetFanSpeed { rpm: i32 },                        // Get (Fan speed)
    SetPowerMode { result: CommandResult },          // Response
    GetPwrLevel { pwr: u8 },                         // Get (Power mode)
    GetCPUBoost { cpu: u8 },                         // Get (CPU boost)
    GetGPUBoost { gpu: u8 },                         // Get (GPU boost)
    SetLogoLedState {result: bool },
    GetLogoLedState { logo_state: u8 },
    GetKeyboardRGB { layer: i32, rgbdata: Vec<u8> }, // Response (RGB) of 90 keys
    SetEffect { result: bool },                       // Set keyboard colour
    SetStandardEffect { result: bool },                       // Set keyboard colour
    SetBrightness { result: bool },
    SetIdle { result: bool },
    GetBrightness { result: u8 },
    SetSync { result: bool },
    GetSync { sync: bool },
    SetBatteryHealthOptimizer { result: bool },
    GetBatteryHealthOptimizer { is_on: bool, threshold: u8 },
    GetDeviceName { name: String },
    GetThermalStatus { status: ThermalStatus },
    GetStandardEffect { effect: u8, params: Vec<u8> },
    GetGpuStatus {
        gpus: Vec<GpuInfo>,
        dgpu_runtime_pm: bool,
        envycontrol_mode: String,
        envycontrol_available: bool,
    },
    SetDgpuRuntimePM { result: bool },
    SetGpuMode { result: bool, message: String },
}

#[allow(dead_code)]
pub fn bind() -> Option<UnixStream> {
    UnixStream::connect(socket_path()).ok()
}

#[allow(dead_code)]
/// We use this from the app, but it should replace bind
pub fn try_bind() -> std::io::Result<UnixStream> {
    UnixStream::connect(socket_path())
}

#[allow(dead_code)]
pub fn create() -> Option<UnixListener> {
    let path = socket_path();
    if std::fs::metadata(&path).is_ok() {
        // Socket file exists — check if a daemon is actually listening
        if UnixStream::connect(&path).is_ok() {
            eprintln!("UNIX Socket already exists and a daemon is responding. Is another daemon running?");
            return None;
        }
        // Stale socket from a previous crash — remove it
        eprintln!("Removing stale socket file");
        if std::fs::remove_file(&path).is_err() {
            eprintln!("Could not remove stale socket file");
            return None;
        }
    }
    // Set permissive umask so non-root GUI/CLI can connect to the daemon socket
    let old_umask = unsafe { umask(0o000) };
    let result = UnixListener::bind(&path);
    unsafe { umask(old_umask) };
    match result {
        Ok(listener) => Some(listener),
        Err(e) => {
            eprintln!("Failed to bind socket: {}", e);
            None
        }
    }
}

#[allow(dead_code)]
pub fn send_to_daemon(command: DaemonCommand, mut sock: UnixStream) -> Option<DaemonResponse> {
    // Prevent blocking the GTK main thread forever if daemon is unresponsive
    let timeout = Some(std::time::Duration::from_secs(5));
    let _ = sock.set_read_timeout(timeout);
    let _ = sock.set_write_timeout(timeout);

    if let Ok(encoded) = bincode::serialize(&command) {
        if sock.write_all(&encoded).is_ok() {
            // Signal request EOF to daemon so it can read the full command.
            let _ = sock.shutdown(Shutdown::Write);

            let mut response = Vec::new();
            return match sock.read_to_end(&mut response) {
                Ok(readed) if readed > 0 => read_from_socked_resp(&response),
                Ok(_) => {
                    eprintln!("No response from daemon");
                    None
                }
                Err(error) => {
                    eprintln!("Read failed: {error}");
                    None
                }
            };
        } else {
            eprintln!("Socket write failed!");
        }
    }
    None
}

/// Deserializes incomming bytes in order to return
/// a `DaemonResponse`. None is returned if deserializing failed
fn read_from_socked_resp(bytes: &[u8]) -> Option<DaemonResponse> {
    match bincode::deserialize::<DaemonResponse>(bytes) {
        Ok(res) => {
            println!("RES: {:?}", res);
            Some(res)
        }
        Err(e) => {
            println!("RES ERROR: {}", e);
            None
        }
    }
}

/// Deserializes incomming bytes in order to return
/// a `DaemonCommand`. None is returned if deserializing failed
#[allow(dead_code)]
pub fn read_from_socket_req(bytes: &[u8]) -> Option<DaemonCommand> {
    match bincode::deserialize::<DaemonCommand>(bytes) {
        Ok(res) => {
            println!("REQ: {:?}", res);
            Some(res)
        }
        Err(e) => {
            println!("REQ ERROR: {}", e);
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn round_trip<T>(value: &T) -> T
    where
        T: Serialize + serde::de::DeserializeOwned,
    {
        let bytes: Vec<u8> = bincode::serialize(value).expect("serialize");
        bincode::deserialize::<T>(&bytes).expect("deserialize")
    }

    #[test]
    fn thermal_status_round_trips_when_reading_succeeded() {
        let status: ThermalStatus = ThermalStatus {
            safety_state: ThermalSafetyStateDto::Manual,
            performance_mode: 2,
            fan_mode: FanControlModeDto::Fixed,
            cpu_rpm: 4200,
            gpu_rpm: 4600,
            error: None,
        };
        assert_eq!(round_trip(&status), status);
    }

    #[test]
    fn thermal_status_round_trips_when_reading_failed() {
        let status: ThermalStatus = ThermalStatus {
            safety_state: ThermalSafetyStateDto::Disabled,
            performance_mode: 0,
            fan_mode: FanControlModeDto::Automatic,
            cpu_rpm: 0,
            gpu_rpm: 0,
            error: Some(ThermalFailureDto {
                code: ThermalFailureCode::Transport,
                message: "tachometer read failed".to_string(),
            }),
        };
        assert_eq!(round_trip(&status), status);
    }

    #[test]
    fn thermal_failure_dto_round_trips_for_every_code() {
        for code in [
            ThermalFailureCode::Transport,
            ThermalFailureCode::ReadbackMismatch,
            ThermalFailureCode::Policy,
            ThermalFailureCode::WritesDisabled,
        ] {
            let dto: ThermalFailureDto =
                ThermalFailureDto { code: code.clone(), message: "context".to_string() };
            assert_eq!(round_trip(&dto), dto);
        }
    }

    #[test]
    fn safety_state_dto_round_trips_for_every_variant() {
        for state in [
            ThermalSafetyStateDto::Preflight,
            ThermalSafetyStateDto::Ready,
            ThermalSafetyStateDto::Manual,
            ThermalSafetyStateDto::Disabled,
        ] {
            assert_eq!(round_trip(&state), state);
        }
    }

    #[test]
    fn fan_control_mode_dto_round_trips_for_every_variant() {
        for mode in [FanControlModeDto::Automatic, FanControlModeDto::Fixed] {
            assert_eq!(round_trip(&mode), mode);
        }
    }

    #[test]
    fn command_result_round_trips_for_both_outcomes() {
        let applied: CommandResult = CommandResult::Applied;
        assert_eq!(round_trip(&applied), applied);
        let rejected: CommandResult =
            CommandResult::Rejected { reason: "mode not selectable".to_string() };
        assert_eq!(round_trip(&rejected), rejected);
    }

    #[test]
    fn formats_every_2025_mode_with_a_stable_numeric_suffix() {
        assert_eq!(performance_mode_display(0), "Balanced (0)");
        assert_eq!(performance_mode_display(2), "Maximum Performance (2)");
        assert_eq!(performance_mode_display(3), "Battery Saver (3)");
        assert_eq!(performance_mode_display(4), "Custom (4)");
        assert_eq!(performance_mode_display(5), "Silent (5)");
    }

    #[test]
    fn labels_every_offered_mode_and_hides_unknown_values() {
        assert_eq!(performance_mode_label(6), "Balanced");
        assert_eq!(provisional_rpm_range(6), (3400, 5200));
        assert_eq!(performance_mode_label(7), "Hyperboost");
        assert_eq!(provisional_rpm_range(7), (3700, 5300));
        assert_eq!(performance_mode_label(1), "Unknown");
        assert_eq!(performance_mode_display(7), "Hyperboost (7)");
    }

    #[test]
    fn provisional_rpm_range_matches_each_selectable_mode() {
        assert_eq!(provisional_rpm_range(0), (3400, 5200));
        assert_eq!(provisional_rpm_range(5), (3400, 5200));
        assert_eq!(provisional_rpm_range(2), (3300, 5400));
        assert_eq!(provisional_rpm_range(3), (3300, 5400));
        assert_eq!(provisional_rpm_range(4), (4000, 5300));
    }
}
