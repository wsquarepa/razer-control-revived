use razer_core::{CommandResult, DaemonCommand, DaemonResponse, GpuInfo, ThermalStatus};
use std::fmt;
use std::io::{Read, Write};
use std::net::Shutdown;
use std::os::unix::net::UnixStream;
use std::time::Duration;

const IO_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Debug, Clone)]
pub enum DaemonError {
    Unreachable(String),
    Io(String),
    Protocol { expected: &'static str, got: String },
    Rejected(String),
}

impl fmt::Display for DaemonError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unreachable(context) => write!(f, "daemon unreachable: {context}"),
            Self::Io(context) => write!(f, "daemon i/o failed: {context}"),
            Self::Protocol { expected, got } => {
                write!(
                    f,
                    "daemon protocol mismatch: expected {expected}, got {got}"
                )
            }
            Self::Rejected(reason) => write!(f, "daemon rejected the request: {reason}"),
        }
    }
}

impl std::error::Error for DaemonError {}

pub fn request_at(path: &str, command: &DaemonCommand) -> Result<DaemonResponse, DaemonError> {
    let mut sock = UnixStream::connect(path)
        .map_err(|e| DaemonError::Unreachable(format!("connect {path}: {e}")))?;
    sock.set_read_timeout(Some(IO_TIMEOUT))
        .map_err(|e| DaemonError::Io(format!("set read timeout: {e}")))?;
    sock.set_write_timeout(Some(IO_TIMEOUT))
        .map_err(|e| DaemonError::Io(format!("set write timeout: {e}")))?;
    let encoded = bincode::serialize(command)
        .map_err(|e| DaemonError::Io(format!("encode {command:?}: {e}")))?;
    sock.write_all(&encoded)
        .map_err(|e| DaemonError::Io(format!("write {command:?}: {e}")))?;
    // Half-close signals request EOF; the daemon reads to EOF before replying.
    sock.shutdown(Shutdown::Write)
        .map_err(|e| DaemonError::Io(format!("shutdown write: {e}")))?;
    let mut response = Vec::new();
    sock.read_to_end(&mut response)
        .map_err(|e| DaemonError::Io(format!("read response to {command:?}: {e}")))?;
    if response.is_empty() {
        return Err(DaemonError::Io(format!("empty response to {command:?}")));
    }
    bincode::deserialize(&response)
        .map_err(|e| DaemonError::Io(format!("decode response to {command:?}: {e}")))
}

pub fn request(command: &DaemonCommand) -> Result<DaemonResponse, DaemonError> {
    let path = razer_core::socket_path();
    match request_at(&path, command) {
        Ok(response) => Ok(response),
        Err(first @ (DaemonError::Unreachable(_) | DaemonError::Io(_))) => {
            log::warn!("daemon request failed, retrying once: {first}");
            request_at(&path, command)
        }
        Err(other) => Err(other),
    }
}

/// Runs a blocking daemon call off the UI thread. Pair with `Task::perform`.
pub async fn blocking<T: Send + 'static>(
    work: impl FnOnce() -> Result<T, DaemonError> + Send + 'static,
) -> Result<T, DaemonError> {
    tokio::task::spawn_blocking(work)
        .await
        .expect("daemon call task panicked")
}

macro_rules! expect_variant {
    // Parsers for wrappers already wired into a page: no allow needed.
    (live $fn_name:ident, $variant:ident { $($field:ident),+ } => $out:ty) => {
        fn $fn_name(response: DaemonResponse) -> Result<$out, DaemonError> {
            match response {
                DaemonResponse::$variant { $($field),+ } => Ok(($($field),+)),
                other => Err(DaemonError::Protocol {
                    expected: stringify!($variant),
                    got: format!("{other:?}"),
                }),
            }
        }
    };
    ($fn_name:ident, $variant:ident { $($field:ident),+ } => $out:ty) => {
        // These parsers only serve wrappers for pages not yet wired
        // (Tasks 10-11); attribute forwarding through a macro invocation
        // doesn't reach the generated item, so the allow lives here instead.
        #[allow(dead_code)]
        fn $fn_name(response: DaemonResponse) -> Result<$out, DaemonError> {
            match response {
                DaemonResponse::$variant { $($field),+ } => Ok(($($field),+)),
                other => Err(DaemonError::Protocol {
                    expected: stringify!($variant),
                    got: format!("{other:?}"),
                }),
            }
        }
    };
}

expect_variant!(expect_device_name, GetDeviceName { name } => String);
expect_variant!(expect_thermal_status, GetThermalStatus { status } => ThermalStatus);
expect_variant!(expect_pwr, GetPwrLevel { pwr } => u8);
expect_variant!(expect_cpu_boost, GetCPUBoost { cpu } => u8);
expect_variant!(expect_gpu_boost, GetGPUBoost { gpu } => u8);
expect_variant!(expect_set_power, SetPowerMode { result } => CommandResult);
expect_variant!(expect_fan_speed, GetFanSpeed { rpm } => i32);
expect_variant!(expect_set_fan, SetFanSpeed { result } => CommandResult);
expect_variant!(expect_brightness, GetBrightness { result } => u8);
expect_variant!(expect_set_brightness, SetBrightness { result } => bool);
expect_variant!(expect_logo, GetLogoLedState { logo_state } => u8);
expect_variant!(expect_set_logo, SetLogoLedState { result } => bool);
expect_variant!(expect_standard_effect, GetStandardEffect { effect, params } => (u8, Vec<u8>));
expect_variant!(expect_set_effect, SetEffect { result } => bool);
expect_variant!(expect_bho, GetBatteryHealthOptimizer { is_on, threshold } => (bool, u8));
expect_variant!(expect_set_bho, SetBatteryHealthOptimizer { result } => bool);
expect_variant!(
    live expect_gpu_status,
    GetGpuStatus { gpus, dgpu_runtime_pm, envycontrol_mode, envycontrol_available }
        => (Vec<GpuInfo>, bool, String, bool)
);
expect_variant!(live expect_set_dgpu_pm, SetDgpuRuntimePM { result } => bool);
expect_variant!(live expect_set_gpu_mode, SetGpuMode { result, message } => (bool, String));

fn ac_wire(ac: bool) -> usize {
    if ac { 1 } else { 0 }
}

fn applied(result: CommandResult) -> Result<(), DaemonError> {
    match result {
        CommandResult::Applied => Ok(()),
        CommandResult::Rejected { reason } => Err(DaemonError::Rejected(reason)),
    }
}

fn accepted(context: &'static str, ok: bool) -> Result<(), DaemonError> {
    if ok {
        Ok(())
    } else {
        Err(DaemonError::Rejected(context.to_string()))
    }
}

#[derive(Debug, Clone)]
pub struct GpuStatus {
    pub gpus: Vec<GpuInfo>,
    pub dgpu_runtime_pm: bool,
    pub envycontrol_mode: String,
    pub envycontrol_available: bool,
}

pub fn device_name() -> Result<String, DaemonError> {
    expect_device_name(request(&DaemonCommand::GetDeviceName)?)
}

pub fn thermal_status() -> Result<ThermalStatus, DaemonError> {
    expect_thermal_status(request(&DaemonCommand::GetThermalStatus)?)
}

pub fn power(ac: bool) -> Result<(u8, u8, u8), DaemonError> {
    let ac = ac_wire(ac);
    let pwr = expect_pwr(request(&DaemonCommand::GetPwrLevel { ac })?)?;
    let cpu = expect_cpu_boost(request(&DaemonCommand::GetCPUBoost { ac })?)?;
    let gpu = expect_gpu_boost(request(&DaemonCommand::GetGPUBoost { ac })?)?;
    Ok((pwr, cpu, gpu))
}

pub fn set_power(ac: bool, pwr: u8, cpu: u8, gpu: u8) -> Result<(), DaemonError> {
    let response = request(&DaemonCommand::SetPowerMode {
        ac: ac_wire(ac),
        pwr,
        cpu,
        gpu,
    })?;
    applied(expect_set_power(response)?)
}

pub fn fan_speed(ac: bool) -> Result<i32, DaemonError> {
    expect_fan_speed(request(&DaemonCommand::GetFanSpeed { ac: ac_wire(ac) })?)
}

pub fn set_fan_speed(ac: bool, rpm: i32) -> Result<(), DaemonError> {
    let response = request(&DaemonCommand::SetFanSpeed {
        ac: ac_wire(ac),
        rpm,
    })?;
    applied(expect_set_fan(response)?)
}

#[allow(dead_code)]
pub fn brightness(ac: bool) -> Result<u8, DaemonError> {
    expect_brightness(request(&DaemonCommand::GetBrightness { ac: ac_wire(ac) })?)
}

#[allow(dead_code)]
pub fn set_brightness(ac: bool, val: u8) -> Result<(), DaemonError> {
    let response = request(&DaemonCommand::SetBrightness {
        ac: ac_wire(ac),
        val,
    })?;
    accepted("set brightness", expect_set_brightness(response)?)
}

#[allow(dead_code)]
pub fn logo(ac: bool) -> Result<u8, DaemonError> {
    expect_logo(request(&DaemonCommand::GetLogoLedState {
        ac: ac_wire(ac),
    })?)
}

#[allow(dead_code)]
pub fn set_logo(ac: bool, state: u8) -> Result<(), DaemonError> {
    let response = request(&DaemonCommand::SetLogoLedState {
        ac: ac_wire(ac),
        logo_state: state,
    })?;
    accepted("set logo state", expect_set_logo(response)?)
}

#[allow(dead_code)]
pub fn standard_effect() -> Result<(u8, Vec<u8>), DaemonError> {
    expect_standard_effect(request(&DaemonCommand::GetStandardEffect)?)
}

#[allow(dead_code)]
pub fn set_effect(name: &str, params: Vec<u8>) -> Result<(), DaemonError> {
    let response = request(&DaemonCommand::SetEffect {
        name: name.to_string(),
        params,
    })?;
    accepted("set keyboard effect", expect_set_effect(response)?)
}

#[allow(dead_code)]
pub fn bho() -> Result<(bool, u8), DaemonError> {
    expect_bho(request(&DaemonCommand::GetBatteryHealthOptimizer())?)
}

#[allow(dead_code)]
pub fn set_bho(on: bool, threshold: u8) -> Result<(), DaemonError> {
    let response = request(&DaemonCommand::SetBatteryHealthOptimizer {
        is_on: on,
        threshold,
    })?;
    accepted("set battery health optimizer", expect_set_bho(response)?)
}

pub fn gpu_status() -> Result<GpuStatus, DaemonError> {
    let (gpus, dgpu_runtime_pm, envycontrol_mode, envycontrol_available) =
        expect_gpu_status(request(&DaemonCommand::GetGpuStatus)?)?;
    Ok(GpuStatus {
        gpus,
        dgpu_runtime_pm,
        envycontrol_mode,
        envycontrol_available,
    })
}

pub fn set_dgpu_runtime_pm(enabled: bool) -> Result<(), DaemonError> {
    let response = request(&DaemonCommand::SetDgpuRuntimePM { enabled })?;
    accepted("set dgpu runtime pm", expect_set_dgpu_pm(response)?)
}

pub fn set_gpu_mode(mode: &str) -> Result<String, DaemonError> {
    let (ok, message) = expect_set_gpu_mode(request(&DaemonCommand::SetGpuMode {
        mode: mode.to_string(),
    })?)?;
    if ok {
        Ok(message)
    } else {
        Err(DaemonError::Rejected(message))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use razer_core::{DaemonCommand, DaemonResponse};
    use std::io::{Read, Write};
    use std::os::unix::net::UnixListener;

    /// A one-shot fake daemon: accepts a single connection on `path`, decodes
    /// the request, replies with `response`, then exits.
    fn fake_daemon(
        path: String,
        response: DaemonResponse,
    ) -> std::thread::JoinHandle<DaemonCommand> {
        let listener = UnixListener::bind(&path).expect("bind fake daemon socket");
        std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept");
            let mut buf = Vec::new();
            stream.read_to_end(&mut buf).expect("read request");
            let command: DaemonCommand = bincode::deserialize(&buf).expect("decode request");
            let encoded = bincode::serialize(&response).expect("encode response");
            stream.write_all(&encoded).expect("write response");
            command
        })
    }

    fn temp_socket(name: &str) -> String {
        let dir = std::env::temp_dir().join(format!("razer-gui-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("create temp dir");
        dir.join(name).to_string_lossy().into_owned()
    }

    #[test]
    fn round_trips_a_request_through_a_socket() {
        let path = temp_socket("ok.sock");
        let handle = fake_daemon(
            path.clone(),
            DaemonResponse::GetDeviceName {
                name: "Blade 16 2025".into(),
            },
        );
        let response = request_at(&path, &DaemonCommand::GetDeviceName).expect("request");
        assert!(matches!(response, DaemonResponse::GetDeviceName { .. }));
        let seen = handle.join().expect("fake daemon");
        assert!(matches!(seen, DaemonCommand::GetDeviceName));
    }

    #[test]
    fn reports_unreachable_when_no_daemon_listens() {
        let path = temp_socket("nobody-home.sock");
        let error = request_at(&path, &DaemonCommand::GetDeviceName).unwrap_err();
        assert!(matches!(error, DaemonError::Unreachable(_)));
    }

    #[test]
    fn wrong_variant_is_a_protocol_error() {
        let path = temp_socket("wrong-variant.sock");
        fake_daemon(path.clone(), DaemonResponse::GetSync { sync: true });
        let got = expect_device_name(request_at(&path, &DaemonCommand::GetDeviceName).unwrap());
        let error = got.unwrap_err();
        assert!(matches!(
            error,
            DaemonError::Protocol {
                expected: "GetDeviceName",
                ..
            }
        ));
    }

    #[test]
    fn every_gui_issued_command_round_trips_through_bincode() {
        let commands: Vec<DaemonCommand> = vec![
            DaemonCommand::GetDeviceName,
            DaemonCommand::GetThermalStatus,
            DaemonCommand::GetPwrLevel { ac: 1 },
            DaemonCommand::GetCPUBoost { ac: 1 },
            DaemonCommand::GetGPUBoost { ac: 0 },
            DaemonCommand::SetPowerMode {
                ac: 1,
                pwr: 4,
                cpu: 2,
                gpu: 3,
            },
            DaemonCommand::GetFanSpeed { ac: 0 },
            DaemonCommand::SetFanSpeed { ac: 1, rpm: 4200 },
            DaemonCommand::GetBrightness { ac: 1 },
            DaemonCommand::SetBrightness { ac: 0, val: 55 },
            DaemonCommand::GetLogoLedState { ac: 1 },
            DaemonCommand::SetLogoLedState {
                ac: 1,
                logo_state: 2,
            },
            DaemonCommand::GetStandardEffect,
            DaemonCommand::SetEffect {
                name: "static".to_string(),
                params: vec![1, 2, 3],
            },
            DaemonCommand::GetBatteryHealthOptimizer(),
            DaemonCommand::SetBatteryHealthOptimizer {
                is_on: true,
                threshold: 80,
            },
            DaemonCommand::GetGpuStatus,
            DaemonCommand::SetDgpuRuntimePM { enabled: true },
            DaemonCommand::SetGpuMode {
                mode: "hybrid".to_string(),
            },
        ];
        for command in commands {
            let bytes = bincode::serialize(&command).expect("serialize");
            let decoded: DaemonCommand = bincode::deserialize(&bytes).expect("deserialize");
            assert_eq!(format!("{decoded:?}"), format!("{command:?}"));
        }
    }
}
