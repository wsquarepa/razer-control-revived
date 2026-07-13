#[path = "../comms.rs"]
mod comms;
use clap::{error::ErrorKind, CommandFactory, Parser, Subcommand, ValueEnum};

#[derive(Parser)]
#[command(version="0.5.0", about="razer laptop configuration for linux", name="razer-cli")]
struct Cli {
    #[command(subcommand)]
    args: Args,
}

#[derive(Subcommand)]
enum Args {
    /// Read the current configuration of the device for some attribute
    Read {
        #[command(subcommand)]
        attr: ReadAttr,
    },
    /// Write a new configuration to the device for some attribute
    Write {
        #[command(subcommand)]
        attr: WriteAttr,
    },
    /// Write a standard effect
    StandardEffect {
        #[command(subcommand)]
        effect: StandardEffect,
    },
    /// Write a custom effect
    Effect {
        #[command(subcommand)]
        effect: Effect,
    },
}

#[derive(Copy, Clone, PartialEq, Eq, PartialOrd, Ord, ValueEnum)]
enum OnOff {
    On,
    Off,
}

impl OnOff {
    pub fn is_on(&self) -> bool {
        matches!(self, Self::On)
    }
}

#[derive(Subcommand)]
enum ReadAttr {
    /// Read the current fan speed
    Fan(AcStateParam),
    /// Read the current power mode
    Power(AcStateParam),
    /// Read the current brightness
    Brightness(AcStateParam),
    /// Read the current logo mode
    Logo(AcStateParam),
    /// Read the current sync mode
    Sync,
    /// Read the current bho mode
    Bho,
    /// Read actual fan RPM from hardware
    FanRpm,
    /// Read GPU status information
    Gpu,
}

#[derive(Subcommand)]
enum WriteAttr {
    /// Set the fan speed
    Fan(FanParams),
    /// Set the power mode
    Power(PowerParams),
    /// Set the brightness of the keyboard
    Brightness(BrightnessParams),
    /// Set the logo mode
    Logo(LogoParams),
    /// Set sync
    Sync(SyncParams),
    /// Set battery health optimization
    Bho(BhoParams),
    /// Set dGPU runtime power management
    RuntimePm(RuntimePmParams),
    /// Set GPU mode via envycontrol (hybrid, integrated, nvidia)
    GpuMode(GpuModeParams),
}

#[derive(Parser)]
struct PowerParams {
    /// battery/plugged in
    ac_state: AcState,
    /// power mode (0, 1, 2, 3 or 4)
    pwr: u8,
    /// cpu boost (0, 1, 2 or 3)
    cpu_mode: Option<u8>,
    /// gpu boost (0, 1, 2 or 3)
    gpu_mode: Option<u8>,
}

#[derive(Parser)]
struct FanParams {
    /// battery/plugged in
    ac_state: AcState,
    /// fan speed in RPM
    speed: i32,
}

#[derive(Parser)]
struct BrightnessParams {
    /// battery/plugged in
    ac_state: AcState,
    /// brightness
    brightness: i32,
}

#[derive(Parser)]
struct LogoParams {
    /// battery/plugged in
    ac_state: AcState,
    /// logo mode (0, 1 or 2)
    logo_state: i32,
}

#[derive(Parser)]
struct SyncParams {
    sync_state: OnOff,
}

#[derive(Parser)]
struct BhoParams {
    state: OnOff,
    /// charging threshold
    threshold: Option<u8>,
}

#[derive(Parser)]
struct RuntimePmParams {
    /// on (suspend dGPU) or off (keep active)
    state: OnOff,
}

#[derive(Parser)]
struct GpuModeParams {
    /// GPU mode: hybrid, integrated, or nvidia
    mode: String,
}

#[derive(ValueEnum, Clone)]
enum AcState {
    /// battery
    Bat,
    /// plugged in
    Ac,
}

impl AcState {
    fn as_index(&self) -> usize {
        match self {
            AcState::Bat => 0,
            AcState::Ac => 1,
        }
    }
}

#[derive(Parser, Clone)]
struct AcStateParam {
    /// battery/plugged in
    ac_state: AcState,
}

#[derive(Subcommand)]
enum StandardEffect {
    Off,
    Wave(WaveParams),
    Reactive(ReactiveParams),
    Breathing(BreathingParams),
    Spectrum,
    Static(StaticParams),
    Starlight(StarlightParams),
}

#[derive(Parser)]
struct WaveParams {
    /// direction (0 or 1)
    direction: u8,
}

#[derive(Parser)]
struct ReactiveParams {
    /// speed (0-255)
    speed: u8,
    /// red (0-255)
    red: u8,
    /// green (0-255)
    green: u8,
    /// blue (0-255)
    blue: u8,
}

#[derive(Parser)]
struct BreathingParams {
    /// kind (0-2)
    kind: u8,
    /// red1 (0-255)
    red1: u8,
    /// green1 (0-255)
    green1: u8,
    /// blue1 (0-255)
    blue1: u8,
    /// red2 (0-255)
    red2: u8,
    /// green2 (0-255)
    green2: u8,
    /// blue2 (0-255)
    blue2: u8,
}

#[derive(Parser)]
struct StarlightParams {
    /// kind (0-2)
    kind: u8,
    /// speed (0-255)
    speed: u8,
    /// red1 (0-255)
    red1: u8,
    /// green1 (0-255)
    green1: u8,
    /// blue1 (0-255)
    blue1: u8,
    /// red2 (0-255)
    red2: u8,
    /// green2 (0-255)
    green2: u8,
    /// blue2 (0-255)
    blue2: u8,
}

#[derive(Subcommand)]
enum Effect {
    Static(StaticParams),
    StaticGradient(StaticGradientParams),
    WaveGradient(WaveGradientParams),
    BreathingSingle(BreathingSingleParams),
}

#[derive(Parser)]
struct StaticParams {
    /// red (0-255)
    red: u8,
    /// green (0-255)
    green: u8,
    /// blue (0-255)
    blue: u8,
}

#[derive(Parser)]
struct StaticGradientParams {
    /// red1 (0-255)
    red1: u8,
    /// green1 (0-255)
    green1: u8,
    /// blue1 (0-255)
    blue1: u8,
    /// red2 (0-255)
    red2: u8,
    /// green2 (0-255)
    green2: u8,
    /// blue2 (0-255)
    blue2: u8,
}

#[derive(Parser)]
struct WaveGradientParams {
    /// red1 (0-255)
    red1: u8,
    /// green1 (0-255)
    green1: u8,
    /// blue1 (0-255)
    blue1: u8,
    /// red2 (0-255)
    red2: u8,
    /// green2 (0-255)
    green2: u8,
    /// blue2 (0-255)
    blue2: u8,
}

#[derive(Parser)]
struct BreathingSingleParams {
    /// red (0-255)
    red: u8,
    /// green (0-255)
    green: u8,
    /// blue (0-255)
    blue: u8,
    /// duration (0-255)
    duration: u8,
}

fn main() {
    if std::fs::metadata(comms::socket_path()).is_err() {
        eprintln!("Error. Socket doesn't exit. Is daemon running?");
        std::process::exit(1);
    }

    let cli = Cli::parse();

    match cli.args {
        Args::Read { attr } => match attr {
            ReadAttr::Fan(AcStateParam { ac_state }) => read_fan_rpm(ac_state.as_index()),
            ReadAttr::Power(AcStateParam { ac_state }) => read_power_mode(ac_state.as_index()),
            ReadAttr::Brightness(AcStateParam { ac_state }) => read_brightness(ac_state.as_index()),
            ReadAttr::Logo(AcStateParam { ac_state }) => read_logo_mode(ac_state.as_index()),
            ReadAttr::Sync => read_sync(),
            ReadAttr::Bho => read_bho(),
            ReadAttr::FanRpm => read_actual_fan_rpm(),
            ReadAttr::Gpu => read_gpu_status(),
        },
        Args::Write { attr } => match attr {
            WriteAttr::Fan(FanParams { ac_state, speed }) => {
                write_fan_speed(ac_state.as_index(), speed)
            }
            WriteAttr::Power(PowerParams {
                ac_state,
                pwr,
                cpu_mode,
                gpu_mode,
            }) => write_pwr_mode(ac_state.as_index(), pwr, cpu_mode, gpu_mode),
            WriteAttr::Brightness(BrightnessParams {
                ac_state,
                brightness,
            }) => write_brightness(ac_state.as_index(), brightness as u8),
            WriteAttr::Sync(SyncParams { sync_state }) => write_sync(sync_state.is_on()),
            WriteAttr::Logo(LogoParams {
                ac_state,
                logo_state,
            }) => write_logo_mode(ac_state.as_index(), logo_state as u8),
            WriteAttr::Bho(BhoParams { state, threshold }) => {
                validate_and_write_bho(threshold, state)
            }
            WriteAttr::RuntimePm(RuntimePmParams { state }) => {
                write_runtime_pm(state.is_on())
            }
            WriteAttr::GpuMode(GpuModeParams { mode }) => {
                write_gpu_mode(&mode)
            }
        },
        Args::Effect { effect } => match effect {
            Effect::Static(params) => send_effect(
                "static".to_string(),
                vec![params.red, params.green, params.blue],
            ),
            Effect::StaticGradient(params) => send_effect(
                "static_gradient".to_string(),
                vec![
                    params.red1,
                    params.green1,
                    params.blue1,
                    params.red2,
                    params.green2,
                    params.blue2,
                ],
            ),
            Effect::WaveGradient(params) => send_effect(
                "wave_gradient".to_string(),
                vec![
                    params.red1,
                    params.green1,
                    params.blue1,
                    params.red2,
                    params.green2,
                    params.blue2,
                ],
            ),
            Effect::BreathingSingle(params) => send_effect(
                "breathing_single".to_string(),
                vec![params.red, params.green, params.blue, params.duration],
            ),
        },
        Args::StandardEffect { effect } => match effect {
            StandardEffect::Off => send_standard_effect("off".to_string(), vec![]),
            StandardEffect::Spectrum => send_standard_effect("spectrum".to_string(), vec![]),
            StandardEffect::Breathing(params) => send_standard_effect(
                "breathing".to_string(),
                vec![
                    params.kind,
                    params.red1,
                    params.green1,
                    params.blue1,
                    params.red2,
                    params.green2,
                    params.blue2,
                ],
            ),
            StandardEffect::Reactive(params) => send_standard_effect(
                "reactive".to_string(),
                vec![params.speed, params.red, params.green, params.blue],
            ),
            StandardEffect::Starlight(params) => send_standard_effect(
                "starlight".to_string(),
                vec![
                    params.kind,
                    params.speed,
                    params.red1,
                    params.green1,
                    params.blue1,
                    params.red2,
                    params.green2,
                    params.blue2,
                ],
            ),
            StandardEffect::Static(params) => send_standard_effect(
                "static".to_string(),
                vec![params.red, params.green, params.blue],
            ),
            StandardEffect::Wave(params) => {
                send_standard_effect("wave".to_string(), vec![params.direction])
            }
        },
    }
}

fn validate_and_write_bho(threshold: Option<u8>, state: OnOff) {
    match threshold {
        Some(threshold) => {
            if !valid_bho_threshold(threshold) {
                Cli::command()
                    .error(
                        ErrorKind::InvalidValue,
                        "Threshold must be multiple of 5 between 50 and 80",
                    )
                    .exit()
            }
            write_bho(state.is_on(), threshold)
        }
        None => {
            if state.is_on() {
                Cli::command()
                    .error(
                        ErrorKind::MissingRequiredArgument,
                        "Threshold is required when BHO is on",
                    )
                    .exit()
            }
            write_bho(state.is_on(), 80)
        }
    }
}

fn read_bho() {
    send_data(comms::DaemonCommand::GetBatteryHealthOptimizer()).map_or_else(
        || eprintln!("Unknown error occured when getting bho"),
        |result| {
            if let comms::DaemonResponse::GetBatteryHealthOptimizer { is_on, threshold } = result {
                match is_on {
                    true => {
                        println!(
                            "Battery health optimization is on with a threshold of {}",
                            threshold
                        );
                    }
                    false => {
                        eprintln!("Battery health optimization is off");
                    }
                }
            }
        },
    );
}

fn write_bho(on: bool, threshold: u8) {
    if !on {
        bho_toggle_off();
        return;
    }

    bho_toggle_on(threshold);
}

fn bho_toggle_on(threshold: u8) {
    if !valid_bho_threshold(threshold) {
        eprintln!("Threshold value must be a multiple of five between 50 and 80");
        return;
    }

    send_data(comms::DaemonCommand::SetBatteryHealthOptimizer {
        is_on: true,
        threshold: threshold,
    })
    .map_or_else(
        || eprintln!("Unknown error occured when toggling bho"),
        |result| {
            if let comms::DaemonResponse::SetBatteryHealthOptimizer { result } = result {
                match result {
                    true => {
                        println!(
                            "Battery health optimization is on with a threshold of {}",
                            threshold
                        );
                    }
                    false => {
                        eprintln!("Failed to turn on bho with threshold of {}", threshold);
                    }
                }
            }
        },
    );
}

fn valid_bho_threshold(threshold: u8) -> bool {
    if threshold % 5 != 0 {
        return false;
    }

    if threshold < 50 || threshold > 80 {
        return false;
    }

    return true;
}

fn bho_toggle_off() {
    send_data(comms::DaemonCommand::SetBatteryHealthOptimizer {
        is_on: false,
        threshold: 80,
    })
    .map_or_else(
        || eprintln!("Unknown error occured when toggling bho"),
        |result| {
            if let comms::DaemonResponse::SetBatteryHealthOptimizer { result } = result {
                match result {
                    true => {
                        println!("Successfully turned off bho");
                    }
                    false => {
                        eprintln!("Failed to turn off bho");
                    }
                }
            }
        },
    );
}

fn send_standard_effect(name: String, params: Vec<u8>) {
    match send_data(comms::DaemonCommand::SetStandardEffect { name, params }) {
        Some(comms::DaemonResponse::SetStandardEffect { result }) => {
            if result {
                println!("Effect set OK!");
            } else {
                eprintln!("Effect set FAIL!");
            }
        },
        Some(_) => eprintln!("Unexpected response from daemon!"),
        None => eprintln!("Unknown daemon error!"),
    }
}

fn send_effect(name: String, params: Vec<u8>) {
    match send_data(comms::DaemonCommand::SetEffect { name, params }) {
        Some(comms::DaemonResponse::SetEffect { result }) => {
            if result {
                println!("Effect set OK!");
            } else {
                eprintln!("Effect set FAIL!");
            }
        },
        Some(_) => eprintln!("Unexpected response from daemon!"),
        None => eprintln!("Unknown daemon error!"),
    }
}

fn send_data(opt: comms::DaemonCommand) -> Option<comms::DaemonResponse> {
    match comms::bind() {
        Some(socket) => comms::send_to_daemon(opt, socket),
        None => {
            eprintln!("Error. Cannot bind to socket");
            None
        },
    }
}

fn read_fan_rpm(ac: usize) {
    match send_data(comms::DaemonCommand::GetFanSpeed { ac }) {
        Some(comms::DaemonResponse::GetFanSpeed { rpm }) => {
            let rpm_desc: String = match rpm {
                f if f < 0 => String::from("Unknown"),
                0 => String::from("Auto (0)"),
                _ => format!("{} RPM", rpm),
            };
            println!("Current fan setting: {}", rpm_desc);
        },
        Some(_) => eprintln!("Daemon responded with invalid data!"),
        None => eprintln!("Unknown daemon error!"),
    }
}

fn read_actual_fan_rpm() {
    match send_data(comms::DaemonCommand::GetThermalStatus) {
        Some(comms::DaemonResponse::GetThermalStatus { status }) => print_thermal_status(&status),
        Some(_) => eprintln!("Daemon responded with invalid data!"),
        None => eprintln!("Unknown daemon error!"),
    }
}

fn print_thermal_status(status: &comms::ThermalStatus) {
    let safety: &str = match status.safety_state {
        comms::ThermalSafetyStateDto::Preflight => "Preflight",
        comms::ThermalSafetyStateDto::Ready => "Ready",
        comms::ThermalSafetyStateDto::Manual => "Manual",
        comms::ThermalSafetyStateDto::Disabled => "Disabled",
    };
    let fan_mode: &str = match status.fan_mode {
        comms::FanControlModeDto::Automatic => "Automatic",
        comms::FanControlModeDto::Fixed => "Fixed",
    };
    println!("Thermal safety: {}", safety);
    println!("Power mode: {}", comms::performance_mode_display(status.performance_mode));
    println!("Fan mode: {}", fan_mode);
    // rpm fields are meaningful only when no error is present.
    match &status.error {
        None => {
            println!("CPU fan: {} RPM", status.cpu_rpm);
            println!("GPU fan: {} RPM", status.gpu_rpm);
        }
        Some(error) => {
            eprintln!("Fan telemetry unavailable: {}", error.message);
        }
    }
}

fn read_logo_mode(ac: usize) {
    match send_data(comms::DaemonCommand::GetLogoLedState { ac }) {
        Some(comms::DaemonResponse::GetLogoLedState { logo_state }) => {
            let logo_state_desc: &str = match logo_state {
                0 => "Off",
                1 => "On",
                2 => "Breathing",
                _ => "Unknown",
            };
            println!("Current logo setting: {}", logo_state_desc);
        },
        Some(_) => eprintln!("Daemon responded with invalid data!"),
        None => eprintln!("Unknown daemon error!"),
    }
}

/// Custom CPU/GPU boost level label. Only Low/Medium/High are valid on this SKU;
/// level 3 (Extreme) is gated and is never presented as a real level.
fn boost_level_label(level: u8) -> &'static str {
    match level {
        0 => "Low",
        1 => "Medium",
        2 => "High",
        _ => "Unknown",
    }
}

fn read_power_mode(ac: usize) {
    if let Some(resp) = send_data(comms::DaemonCommand::GetPwrLevel { ac }) {
        if let comms::DaemonResponse::GetPwrLevel { pwr } = resp {
            // Stable KDE-widget contract: `Name (N)` with the wire number in parens.
            println!("Current power setting: {}", comms::performance_mode_display(pwr));
            if pwr == 4 {
                if let Some(resp) = send_data(comms::DaemonCommand::GetCPUBoost { ac }) {
                    if let comms::DaemonResponse::GetCPUBoost { cpu } = resp {
                        println!("Current CPU setting: {}", boost_level_label(cpu));
                    };
                }
                if let Some(resp) = send_data(comms::DaemonCommand::GetGPUBoost { ac }) {
                    if let comms::DaemonResponse::GetGPUBoost { gpu } = resp {
                        println!("Current GPU setting: {}", boost_level_label(gpu));
                    };
                }
            }
        } else {
            eprintln!("Daemon responded with invalid data!");
        }
    }
}

fn write_pwr_mode(ac: usize, pwr_mode: u8, cpu_mode: Option<u8>, gpu_mode: Option<u8>) {
    if pwr_mode == 7 {
        Cli::command()
            .error(
                ErrorKind::InvalidValue,
                "Hyperboost (mode 7) is not validated on this unit and is disabled",
            )
            .exit()
    }
    if !matches!(pwr_mode, 0 | 2 | 3 | 4 | 5) {
        Cli::command()
            .error(
                ErrorKind::InvalidValue,
                "Power mode must be 0 (Balanced), 2 (Maximum Performance), 3 (Battery Saver), 4 (Custom) or 5 (Silent)",
            )
            .exit()
    }

    // Custom mode without explicit levels defaults to Low/Low so a quick profile
    // toggle (e.g. the panel widget) never needs to pass boost arguments.
    let cm = cpu_mode.unwrap_or(0);
    validate_boost_level(cm, "CPU");

    let gm = gpu_mode.unwrap_or(0);
    validate_boost_level(gm, "GPU");

    match send_data(comms::DaemonCommand::SetPowerMode {
        ac,
        pwr: pwr_mode,
        cpu: cm,
        gpu: gm,
    }) {
        Some(comms::DaemonResponse::SetPowerMode { result }) => report_command_result(result, || read_power_mode(ac)),
        Some(_) => eprintln!("Daemon responded with invalid data!"),
        None => {
            Cli::command()
                .error(
                    ErrorKind::DisplayHelp,
                    "An error occurred while sending the command to the daemon",
                )
                .exit()
        },
    }
}

/// Reject a custom boost level the UI must not offer. Low/Medium/High (0-2) pass;
/// level 3 (Extreme) is gated on this unit; anything higher is not a level.
fn validate_boost_level(level: u8, zone: &str) {
    if level == 3 {
        Cli::command()
            .error(
                ErrorKind::InvalidValue,
                format!("{zone} level 3 (Extreme) is not validated on this unit and is disabled"),
            )
            .exit()
    }
    if level > 3 {
        Cli::command()
            .error(
                ErrorKind::InvalidValue,
                format!("{zone} level must be 0 (Low), 1 (Medium) or 2 (High)"),
            )
            .exit()
    }
}

/// Surface a setter outcome: run `on_applied` when the daemon applied the write,
/// otherwise print the daemon's rejection reason.
fn report_command_result<F: FnOnce()>(result: comms::CommandResult, on_applied: F) {
    match result {
        comms::CommandResult::Applied => on_applied(),
        comms::CommandResult::Rejected { reason } => eprintln!("Command rejected: {reason}"),
    }
}

fn read_brightness(ac: usize) {
    match send_data(comms::DaemonCommand::GetBrightness { ac }) {
        Some(comms::DaemonResponse::GetBrightness { result }) => {
            println!("Current brightness: {}", result);
        },
        Some(_) => eprintln!("Daemon responded with invalid data!"),
        None => eprintln!("Unknown daemon error!"),
    }
}

fn read_sync() {
    match send_data(comms::DaemonCommand::GetSync()) {
        Some(comms::DaemonResponse::GetSync { sync }) => {
            println!("Current sync: {:?}", sync);
        },
        Some(_) => eprintln!("Daemon responded with invalid data!"),
        None => eprintln!("Unknown daemon error!"),
    }
}

fn write_brightness(ac: usize, val: u8) {
    match send_data(comms::DaemonCommand::SetBrightness { ac, val }) {
        Some(_) => read_brightness(ac),
        None => eprintln!("Unknown error!"),
    }
}

fn write_fan_speed(ac: usize, x: i32) {
    match send_data(comms::DaemonCommand::SetFanSpeed { ac, rpm: x }) {
        Some(comms::DaemonResponse::SetFanSpeed { result }) => report_command_result(result, || read_fan_rpm(ac)),
        Some(_) => eprintln!("Daemon responded with invalid data!"),
        None => eprintln!("Unknown error!"),
    }
}

fn write_logo_mode(ac: usize, x: u8) {
    match send_data(comms::DaemonCommand::SetLogoLedState { ac, logo_state: x }) {
        Some(_) => read_logo_mode(ac),
        None => eprintln!("Unknown error!"),
    }
}

fn write_sync(sync: bool) {
    match send_data(comms::DaemonCommand::SetSync { sync }) {
        Some(_) => read_sync(),
        None => eprintln!("Unknown error!"),
    }
}

fn read_gpu_status() {
    match send_data(comms::DaemonCommand::GetGpuStatus) {
        Some(comms::DaemonResponse::GetGpuStatus { gpus, dgpu_runtime_pm, envycontrol_mode, envycontrol_available }) => {
            println!("Detected GPUs:");
            for gpu in &gpus {
                let type_label = if gpu.gpu_type == "dgpu" { "dGPU" } else { "iGPU" };
                println!("  {} [{}] {} (driver: {}, status: {})", type_label, gpu.pci_slot, gpu.name, gpu.driver, gpu.runtime_status);
            }
            println!("dGPU Runtime PM: {}", if dgpu_runtime_pm { "auto (power saving)" } else { "on (always active)" });
            if envycontrol_available {
                println!("envycontrol mode: {}", envycontrol_mode);
            } else {
                println!("envycontrol: not installed");
            }
        },
        Some(_) => eprintln!("Daemon responded with invalid data!"),
        None => eprintln!("Unknown daemon error!"),
    }
}

fn write_runtime_pm(enabled: bool) {
    match send_data(comms::DaemonCommand::SetDgpuRuntimePM { enabled }) {
        Some(comms::DaemonResponse::SetDgpuRuntimePM { result }) => {
            if result {
                println!("dGPU runtime PM set to {}", if enabled { "auto (power saving)" } else { "on (always active)" });
            } else {
                eprintln!("Failed to set dGPU runtime PM (permission denied?)");
            }
        },
        Some(_) => eprintln!("Daemon responded with invalid data!"),
        None => eprintln!("Unknown daemon error!"),
    }
}

fn write_gpu_mode(mode: &str) {
    match send_data(comms::DaemonCommand::SetGpuMode { mode: mode.to_string() }) {
        Some(comms::DaemonResponse::SetGpuMode { result, message }) => {
            if result {
                println!("{}", message);
            } else {
                eprintln!("Failed: {}", message);
            }
        },
        Some(_) => eprintln!("Daemon responded with invalid data!"),
        None => eprintln!("Unknown daemon error!"),
    }
}

