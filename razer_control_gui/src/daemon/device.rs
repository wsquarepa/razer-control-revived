// mod kbd;
use serde::{Deserialize, Serialize};
use serde_big_array::BigArray;
use std::{thread, time, io, fs};
use std::ffi::CString;
use hidapi::HidApi;
use crate::dbus_mutter_idlemonitor;
use crate::config;
use crate::battery;
use crate::thermal;
use dbus::blocking::Connection;

const RAZER_VENDOR_ID: u16 = 0x1532;

/// The command class every 0x0d thermal command (get/set fan speed, mode, boost,
/// limits) is framed under.
const THERMAL_COMMAND_CLASS: u8 = 0x0d;

#[derive(Serialize, Deserialize, Debug)]
pub struct SupportedDevice {
    pub name: String,
    pub vid: String,
    pub pid: String,
    pub features: Vec<String>,
    pub fan: Vec<u16>,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct RazerPacket {
    report: u8,
    status: u8,
    id: u8,
    remaining_packets: u16,
    protocol_type: u8,
    data_size: u8,
    command_class: u8,
    command_id: u8,
    #[serde(with = "BigArray")]
    args: [u8; 80],
    crc: u8,
    reserved: u8,
}

impl RazerPacket {
// Command status
    const RAZER_CMD_NEW:u8 = 0x00;
    const RAZER_CMD_BUSY:u8 = 0x01;
    const RAZER_CMD_SUCCESSFUL:u8 = 0x02;
    const RAZER_CMD_FAILURE:u8 = 0x03;
    const RAZER_CMD_TIMEOUT:u8 = 0x04;
    const RAZER_CMD_NOT_SUPPORTED:u8 = 0x05;

    fn new(command_class: u8, command_id: u8, data_size: u8) -> RazerPacket {
        return RazerPacket {
            report: 0x00,
            status: RazerPacket::RAZER_CMD_NEW,
            id: 0x1F,
            remaining_packets: 0x0000,
            protocol_type: 0x00,
            data_size,
            command_class,
            command_id,
            args: [0x00; 80],
            crc: 0x00,
            reserved: 0x00,
        };
    }

    /// Serialize to the 91-byte HID feature report, computing the Razer CRC.
    /// Pure: does not mutate the packet. The CRC = XOR of the 90-byte report's
    /// bytes [2..88); index 0 of this struct is the prepended HID report-id, so
    /// that range maps to buf[3..89] and the crc byte itself sits at buf[89].
    fn serialize(&self) -> Result<[u8; 91], TransportError> {
        let mut buf: Vec<u8> = bincode::serialize(self).map_err(|error| {
            TransportError::Serialization {
                command_class: self.command_class,
                command_id: self.command_id,
                detail: error.to_string(),
            }
        })?;
        let mut crc: u8 = 0x00;
        for byte in &buf[3..89] {
            crc ^= *byte;
        }
        buf[89] = crc;
        buf.try_into().map_err(|buf: Vec<u8>| TransportError::Serialization {
            command_class: self.command_class,
            command_id: self.command_id,
            detail: format!("serialized packet was {} bytes, expected 91", buf.len()),
        })
    }
}

/// A typed failure from the HID feature-report transport. Every variant carries
/// enough context (device PID, transaction id, command class/id, EC status,
/// attempt count) to debug a failure without re-deriving it from logs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TransportError {
    /// bincode failed to encode the request, or the encoded length was not 91.
    Serialization { command_class: u8, command_id: u8, detail: String },
    /// The HID write (send_feature_report) failed.
    Write { pid: u16, transaction_id: u8, command_class: u8, command_id: u8, detail: String },
    /// The HID read (get_feature_report) failed.
    Read { pid: u16, transaction_id: u8, command_class: u8, command_id: u8, detail: String },
    /// The EC returned a report of an unexpected length.
    Size { pid: u16, command_class: u8, command_id: u8, size: usize },
    /// The reply bytes could not be decoded into a packet.
    Decode { pid: u16, command_class: u8, command_id: u8, detail: String },
    /// The EC reported the command is not supported.
    Unsupported { pid: u16, command_class: u8, command_id: u8 },
    /// The EC reported an explicit failure status for the command.
    CommandFailure { pid: u16, transaction_id: u8, command_class: u8, command_id: u8, status: u8 },
    /// Write and read attempts were exhausted while the EC stayed busy.
    ExhaustedPolls { pid: u16, command_class: u8, command_id: u8, attempts: usize },
}

/// A preflight sweep failure: either a getter did not complete over the HID
/// transport, or a reply decoded to something the policy layer rejects (fan set
/// not exactly {CPU, GPU}, or an RPM reply for the wrong zone).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PreflightError {
    Transport(TransportError),
    Decode(thermal::ThermalDecodeError),
}

impl From<TransportError> for PreflightError {
    fn from(error: TransportError) -> PreflightError {
        PreflightError::Transport(error)
    }
}

impl From<thermal::ThermalDecodeError> for PreflightError {
    fn from(error: thermal::ThermalDecodeError) -> PreflightError {
        PreflightError::Decode(error)
    }
}

/// A supervised limit-collection failure. Restoration failures reuse these
/// variants and win primacy via `thermal::resolve_with_restoration`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LimitCollectionError {
    Transport(TransportError),
    Decode(thermal::ThermalDecodeError),
    Policy(thermal::ThermalPolicyError),
    /// The EC did not read back the mode that was just written.
    ModeReadbackMismatch { requested: u8, observed: u8 },
    /// Limit collection is only defined for the Blade 16 2025 EC.
    UnsupportedDevice { pid: u16 },
    /// The current physical power source could not be read, so the set of modes
    /// to sweep is unknown.
    PowerSourceUnavailable,
}

impl From<TransportError> for LimitCollectionError {
    fn from(error: TransportError) -> LimitCollectionError {
        LimitCollectionError::Transport(error)
    }
}

impl From<thermal::ThermalDecodeError> for LimitCollectionError {
    fn from(error: thermal::ThermalDecodeError) -> LimitCollectionError {
        LimitCollectionError::Decode(error)
    }
}

impl From<thermal::ThermalPolicyError> for LimitCollectionError {
    fn from(error: thermal::ThermalPolicyError) -> LimitCollectionError {
        LimitCollectionError::Policy(error)
    }
}

/// A failure from the verified thermal application sequence or the telemetry
/// monitor (Blade 16 2025 only). Transport and decode failures carry the full
/// typed cause; a readback that did not match the request or a write refused
/// because the daemon is in the Disabled safety posture are distinct variants.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ThermalError {
    Transport(TransportError),
    Decode(thermal::ThermalDecodeError),
    Policy(thermal::ThermalPolicyError),
    /// A readback (0x82 mode, 0x87 boost, or 0x81 setpoint) decoded cleanly for
    /// its own zone but did not equal what was written.
    ReadbackMismatch { command_id: u8, requested: u8, observed: u8 },
    /// The safety state machine is Disabled, so the write is refused before any
    /// command is constructed and no HID I/O is attempted.
    WritesDisabled,
}

impl From<TransportError> for ThermalError {
    fn from(error: TransportError) -> ThermalError {
        ThermalError::Transport(error)
    }
}

impl From<thermal::ThermalDecodeError> for ThermalError {
    fn from(error: thermal::ThermalDecodeError) -> ThermalError {
        ThermalError::Decode(error)
    }
}

impl From<thermal::ThermalPolicyError> for ThermalError {
    fn from(error: thermal::ThermalPolicyError) -> ThermalError {
        ThermalError::Policy(error)
    }
}

/// Map a verified-apply failure to the pure failure class the safety state
/// machine consumes. A decode failure means a reply could not be validated for
/// its own zone (a readback mismatch); anything else is a transport failure.
fn thermal_failure_class(error: &ThermalError) -> thermal::ThermalFailure {
    match error {
        ThermalError::Decode(_) => thermal::ThermalFailure::ReadbackMismatch,
        _ => thermal::ThermalFailure::Transport,
    }
}

/// Classify a per-zone failback attempt into the pure outcome the report retains.
fn zone_outcome(result: Result<(), ThermalError>) -> thermal::ZoneOutcome {
    match result {
        Ok(()) => thermal::ZoneOutcome::Restored,
        Err(error) => {
            eprintln!("thermal failback for one zone failed: {error:?}");
            thermal::ZoneOutcome::Failed
        }
    }
}

fn device_file_path() -> String {
    std::env::var("RAZER_DEVICE_FILE")
        .unwrap_or_else(|_| "/usr/share/razercontrol/laptops.json".to_string())
}

pub struct DeviceManager {
    pub device: Option <RazerLaptop>,
    supported_devices: Vec<SupportedDevice>,
    pub config: Option <config::Configuration>,
    pub idle_id: u32,
    pub active_id: u32,
    add_active: bool,
    pub change_idle: bool,
}

impl DeviceManager {
    /// Read the USB interface number for a /dev/hidrawX node from sysfs.
    fn hidraw_iface_number(hidraw_name: &str) -> Option<i32> {
        let iface_path = format!("/sys/class/hidraw/{}/device/../bInterfaceNumber", hidraw_name);
        let raw = fs::read_to_string(iface_path).ok()?;
        i32::from_str_radix(raw.trim(), 16).ok()
    }

    fn read_hex_u16(path: &std::path::Path) -> Option<u16> {
        let raw = fs::read_to_string(path).ok()?;
        let trimmed = raw.trim();
        u16::from_str_radix(trimmed, 16).ok()
    }

    /// Resolve VID/PID for a /dev/hidrawX node via /sys, walking up parents
    /// until we find idVendor/idProduct.
    fn hidraw_vid_pid(hidraw_name: &str) -> Option<(u16, u16)> {
        let mut current = fs::canonicalize(format!("/sys/class/hidraw/{}/device", hidraw_name)).ok()?;

        for _ in 0..6 {
            let vid_path = current.join("idVendor");
            let pid_path = current.join("idProduct");
            if vid_path.exists() && pid_path.exists() {
                let vid = Self::read_hex_u16(&vid_path)?;
                let pid = Self::read_hex_u16(&pid_path)?;
                return Some((vid, pid));
            }
            if !current.pop() {
                break;
            }
        }
        None
    }

    pub fn new () -> DeviceManager {
        return DeviceManager {
            device: None,
            supported_devices: vec![],
            config: None,
            idle_id: 0,
            active_id: 0,
            add_active: false,
            change_idle: false,
        };
    }

    pub fn add_idle_watch(&mut self, proxy_idle: &dyn dbus_mutter_idlemonitor::OrgGnomeMutterIdleMonitor) {
        if self.change_idle {
            let mut timeout: u64 = 0;
            let mut state: usize = 0;
            if let Some(laptop) = self.get_device() {
                state = laptop.get_ac_state();
            }
            if let Some(config) = self.get_config() {
                timeout = config.power[state].idle as u64 * 60 * 1000; // idle is in minutes timeout is in miliseconds
            }
            if timeout != 0 {
                if self.idle_id != 0 {
                    self.remove_watch(proxy_idle);
                }
                if let Ok(id) = proxy_idle.add_idle_watch(timeout) {
                    println!("idle handler {:?}", id);
                    self.idle_id = id;
                }
            } else {
                if self.idle_id != 0 {
                    self.remove_watch(proxy_idle);
                }
            }
            self.change_idle = false;
        }
    }

    pub fn set_sync(&mut self, sync: bool) -> bool {
        let mut ac: usize = 0;
        if let Some(laptop) = self.get_device() {
            ac = laptop.ac_state as usize;
        }
        let other = (ac + 1) & 0x01;
        if let Some(config) = self.get_config() {
            config.sync = sync;
            config.power[other].brightness = config.power[ac].brightness;
            config.power[other].logo_state = config.power[ac].logo_state;
            config.power[other].screensaver = config.power[ac].screensaver;
            config.power[other].idle = config.power[ac].idle;
            if let Err(e) = config.write_to_file() {
                eprintln!("Error write config {:?}", e);
            }
        }

        return true;
    }

    pub fn get_sync(&mut self) -> bool {
        if let Some(config) = self.get_config() {
            return config.sync;
        }

        return false;
    }

    fn remove_watch(&mut self, proxy_idle: &dyn dbus_mutter_idlemonitor::OrgGnomeMutterIdleMonitor) {
        if let Ok(_) = proxy_idle.remove_watch(self.idle_id) {
            println!("remove idle handler");
        }
    }

    pub fn add_active_watch(&mut self, proxy_idle: &dyn dbus_mutter_idlemonitor::OrgGnomeMutterIdleMonitor) {
        if self.add_active {
            if let Ok(id) = proxy_idle.add_user_active_watch() {
                println!("active handler {:?}", id);
                self.active_id = id;
            }
        }
    }

    pub fn read_laptops_file() -> io::Result<DeviceManager > {
        let path = device_file_path();
        let str: Vec<u8> = fs::read(&path)?;
        let mut res: DeviceManager = DeviceManager::new();
        res.supported_devices = serde_json::from_slice(str.as_slice())?;
        println!("suported devices found: {:?}", res.supported_devices.len());
        match config::Configuration::load() {
            Ok(c) => res.config = Some(c),
            Err(error) => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("failed to load configuration: {error}"),
                ));
            }
        }

        Ok(res)
    }

    /// Run the one-time schema migration for the discovered device's PID.
    /// Persists the configuration only when the migration actually changed it.
    pub fn migrate_configuration(&mut self) -> Result<(), config::ConfigurationError> {
        let pid: u16 = match self.device.as_ref() {
            Some(laptop) => laptop.pid(),
            None => return Ok(()),
        };
        let configuration: config::Configuration = match self.config.take() {
            Some(configuration) => configuration,
            None => return Ok(()),
        };
        let outcome: config::MigrationOutcome = config::migrate_for_pid(configuration, pid)?;
        for warning in &outcome.warnings {
            match serde_json::to_string(warning) {
                Ok(serialized) => println!("configuration migration warning: {serialized}"),
                Err(error) => eprintln!("failed to serialize migration warning: {error}"),
            }
        }
        if outcome.migrated {
            outcome.configuration.write_to_file()?;
        }
        self.config = Some(outcome.configuration);
        Ok(())
    }

    fn get_ac_config(&mut self, ac: usize) -> Option<config::PowerConfig> {
        if let Some(c) = self.get_config() {
            return Some(c.power[ac].clone());
        }

        return None;
    }

    pub fn light_off(&mut self) {
        if self.idle_id != 0 {
            self.add_active = true;
        }
        if let Some(laptop) = self.get_device() {
            laptop.set_screensaver(true);
            laptop.set_brightness(0);
            laptop.set_logo_led_state(0);
        }
    }

    pub fn restore_light(&mut self) {
        self.add_active = false;
        let mut brightness = 0;
        let mut logo_state = 0;
        let mut ac:usize = 0;
        if let Some(laptop) = self.get_device() {
            ac = laptop.get_ac_state();
        }
        if let Some(config) = self.get_ac_config(ac) {
            brightness = config.brightness;
            logo_state = config.logo_state;
        }
        if let Some(laptop) = self.get_device() {
            laptop.set_screensaver(false);
            laptop.set_brightness(brightness);
            laptop.set_logo_led_state(logo_state);
        }
    }

    /// Check whether the current device declares a given feature.
    pub fn device_has_feature(&self, feature: &str) -> bool {
        self.device.as_ref().map_or(false, |d| d.features.contains(&feature.to_string()))
    }

    pub fn restore_standard_effect(&mut self) {
        let mut effect = 0;
        let mut params: Vec<u8> = vec![];
        if let Some(config) = self.get_config() {
            effect = config.standard_effect;
            params = config.standard_effect_params.clone();
        }
        if let Some(laptop) = self.get_device() {
            laptop.set_standard_effect(effect, params);
        }
    }

    pub fn change_idle(&mut self, ac: usize, timeout: u32) -> bool {
        // let mut arm: bool = false;
        if let Some(config) = self.get_config() {
            if config.power[ac].idle != timeout {
                config.power[ac].idle = timeout;
                if config.sync {
                    let other = (ac + 1) & 0x01;
                    config.power[other].idle = timeout;
                }
                if let Err(e) = config.write_to_file() {
                    eprintln!("Error write config {:?}", e);
                }
                // arm = true;
                self.change_idle = true;
            }
        }

        return true;
    }

    /// Re-apply (to hardware only, no config write) the saved power mode for the
    /// current AC state. Used to re-latch GPU boost when the dGPU resumes.
    pub fn reapply_power_mode(&mut self) -> bool {
        let ac = match self.get_device() {
            Some(laptop) => laptop.get_ac_state(),
            None => return false,
        };
        let config = match self.get_ac_config(ac) {
            Some(config) => config,
            None => return false,
        };
        println!(
            "Re-applying power profile: ac={} power_mode={} cpu_boost={} gpu_boost={}",
            ac, config.power_mode, config.cpu_boost, config.gpu_boost
        );
        match self.get_device() {
            Some(laptop) => match laptop.set_power_mode(config.power_mode, config.cpu_boost, config.gpu_boost) {
                Ok(()) => true,
                Err(error) => {
                    eprintln!("reapply_power_mode: set_power_mode failed: {error:?}");
                    false
                }
            },
            None => false,
        }
    }

    /// Diagnostic: read the EC's actual active perf mode (per zone) and CPU/GPU
    /// boost levels and log them against the config the daemon believes is active.
    /// A mismatch shows the post-resume writes did not latch (firmware override or
    /// a silent HID failure that `set_power_mode`'s unconditional `true` hides).
    pub fn log_hw_power_state(&mut self, context: &str) {
        let ac = match self.get_device() {
            Some(laptop) => laptop.get_ac_state(),
            None => return,
        };
        let (cfg_mode, cfg_cpu, cfg_gpu) = match self.get_ac_config(ac) {
            Some(config) => (config.power_mode, config.cpu_boost, config.gpu_boost),
            None => return,
        };
        if let Some(laptop) = self.get_device() {
            let hw_mode_z1 = laptop.get_power_mode(0x01);
            let hw_mode_z2 = laptop.get_power_mode(0x02);
            let hw_cpu = laptop.get_cpu_boost();
            let hw_gpu = laptop.get_gpu_boost();
            println!(
                "[verify {}] ac={} cfg(mode={} cpu={} gpu={}) hw(mode_z1={:?} mode_z2={:?} cpu={:?} gpu={:?})",
                context, ac, cfg_mode, cfg_cpu, cfg_gpu, hw_mode_z1, hw_mode_z2, hw_cpu, hw_gpu
            );
        }
    }

    /// Read the current AC line-power state from UPower. Used by the delayed
    /// post-wake re-verification, where UPower may still have been mid-update at
    /// the wake instant, so the settled reading decides the expected profile.
    fn read_upower_online(&self) -> Option<bool> {
        let dbus_system = Connection::new_system().ok()?;
        let proxy_ac = dbus_system.with_proxy(
            "org.freedesktop.UPower",
            "/org/freedesktop/UPower/devices/line_power_AC0",
            time::Duration::from_millis(5000),
        );
        use battery::OrgFreedesktopUPowerDevice;
        proxy_ac.online().ok()
    }

    /// Re-apply the saved thermal profile (power mode, boosts, and fan) for the
    /// current AC state through the verified setters. The single post-wake repair.
    fn apply_current_profile_verified(&mut self) -> Result<(), ThermalError> {
        let ac: usize = match self.get_device() {
            Some(laptop) => laptop.get_ac_state(),
            None => return Ok(()),
        };
        let config: config::PowerConfig = match self.get_ac_config(ac) {
            Some(config) => config,
            None => return Ok(()),
        };
        match self.get_device() {
            Some(laptop) => {
                laptop.set_power_mode(config.power_mode, config.cpu_boost, config.gpu_boost)?;
                laptop.set_fan_rpm(config.fan_rpm as u16)?;
                Ok(())
            }
            None => Ok(()),
        }
    }

    /// The delayed post-wake re-verification (Blade 16 2025): re-read the settled
    /// AC state, read back the applied thermal state with getters only, and issue
    /// at most one verified repair, and only if the readback showed the firmware
    /// reset it. A failed repair feeds the same failure counter the monitor uses.
    pub fn wake_delayed_reverify(&mut self) {
        if !self.is_blade_16_2025() {
            return;
        }
        let online: bool = match self.read_upower_online() {
            Some(online) => online,
            None => {
                eprintln!("post-wake reverify: UPower online read failed; skipping");
                return;
            }
        };
        if let Some(laptop) = self.get_device() {
            laptop.set_ac_state(online);
        }
        let config: config::PowerConfig = match self.get_ac_config(online as usize) {
            Some(config) => config,
            None => return,
        };
        let expected_fan_hundreds: u8 = (config.fan_rpm / 100) as u8;
        let outcome: thermal::WakeVerifyOutcome = match self.get_device() {
            Some(laptop) => match laptop.wake_readback(
                config.power_mode,
                config.cpu_boost,
                config.gpu_boost,
                expected_fan_hundreds,
            ) {
                Ok(outcome) => outcome,
                Err(error) => {
                    eprintln!("post-wake reverify: readback failed: {error:?}");
                    return;
                }
            },
            None => return,
        };
        match thermal::decide_wake_repair(outcome) {
            thermal::WakeRepair::None => {
                println!("post-wake state held; no repair needed");
            }
            thermal::WakeRepair::ReapplyOnce => {
                println!("post-wake state was reset; issuing one repair");
                let state_before: thermal::ThermalSafetyState = self
                    .get_device()
                    .map_or(thermal::ThermalSafetyState::Ready, |laptop| laptop.thermal_safety());
                let repair: Result<(), thermal::ThermalFailure> = self
                    .apply_current_profile_verified()
                    .map_err(|error| {
                        eprintln!("post-wake repair failed: {error:?}");
                        thermal_failure_class(&error)
                    });
                let transition: thermal::SafetyTransition =
                    thermal::advance_after_repair(state_before, repair);
                if let Some(laptop) = self.get_device() {
                    laptop.set_thermal_safety(transition.state);
                }
                if transition.action == Some(thermal::SafetyAction::FailbackBothFans) {
                    if let Some(laptop) = self.get_device() {
                        let report: thermal::FailbackReport = laptop.failback_both_fans();
                        eprintln!("post-wake repair failback to firmware-automatic: {report:?}");
                    }
                }
                self.log_hw_power_state("post-wake-repair");
            }
        }
    }

    /// Re-latch GPU boost on a dGPU inactive-to-active transition. The Blade 16
    /// 2025 re-latches only the GPU custom level in Custom mode (touching no CPU or
    /// fan state); other models keep their existing full-profile re-apply.
    pub fn relatch_dgpu_boost(&mut self) {
        if !self.is_blade_16_2025() {
            self.reapply_power_mode();
            self.log_hw_power_state("dgpu-resume");
            return;
        }
        let ac: usize = match self.get_device() {
            Some(laptop) => laptop.get_ac_state(),
            None => return,
        };
        let config: config::PowerConfig = match self.get_ac_config(ac) {
            Some(config) => config,
            None => return,
        };
        let mode: thermal::PerformanceMode =
            match thermal::PerformanceMode::try_from(config.power_mode) {
                Ok(mode) => mode,
                Err(error) => {
                    eprintln!("dgpu relatch: unknown saved mode byte: {error}");
                    return;
                }
            };
        match self.get_device() {
            Some(laptop) => match laptop.relatch_gpu_custom_boost(mode, config.gpu_boost) {
                Ok(true) => println!("dGPU resumed: re-latched GPU custom boost"),
                Ok(false) => {}
                Err(error) => eprintln!("dgpu relatch failed: {error:?}"),
            },
            None => return,
        }
        self.log_hw_power_state("dgpu-resume");
    }

    /// On clean exit, if a fixed fan speed is armed (Manual), return both fan zones
    /// to firmware-automatic control, attempting the GPU zone even if the CPU zone
    /// errors and retaining both outcomes. Non-Manual postures need no restore.
    pub fn restore_automatic_on_exit(&mut self) {
        let armed: bool = matches!(
            self.get_device().map(|laptop| laptop.thermal_safety()),
            Some(thermal::ThermalSafetyState::Manual { .. })
        );
        if !armed {
            return;
        }
        if let Some(laptop) = self.get_device() {
            let report: thermal::FailbackReport = laptop.failback_both_fans();
            println!("clean exit: restored fans to firmware-automatic control: {report:?}");
        }
    }

    pub fn set_power_mode(&mut self, ac: usize, pwr: u8, cpu: u8, gpu: u8) -> bool {
        let mut res: bool = false;
        if let Some(config) = self.get_config() {
            config.power[ac].power_mode = pwr;
            config.power[ac].cpu_boost = cpu;
            config.power[ac].gpu_boost = gpu;
            if let Err(e) = config.write_to_file() {
                eprintln!("Error write config {:?}", e);
            }
        }
        if let Some(laptop) = self.get_device() {
            let state = laptop.get_ac_state();
            if state != ac {
                res = true;
            } else {
                res = match laptop.set_power_mode(pwr, cpu, gpu) {
                    Ok(()) => true,
                    Err(error) => {
                        eprintln!("set_power_mode failed: {error:?}");
                        false
                    }
                };
            }
        }

        return res;
    }

    pub fn get_standard_effect(&mut self) -> (u8, Vec<u8>) {
        if let Some(config) = self.get_config() {
            return (config.gui_effect, config.gui_effect_params.clone());
        }
        return (0, vec![]);
    }

    pub fn save_gui_effect(&mut self, effect_idx: u8, params: Vec<u8>) {
        if let Some(config) = self.get_config() {
            config.gui_effect = effect_idx;
            config.gui_effect_params = params;
            if let Err(e) = config.write_to_file() {
                eprintln!("Error write config {:?}", e);
            }
        }
    }

    pub fn set_standard_effect(&mut self, effect_id: u8, params: Vec<u8>) -> bool {
        if let Some(config) = self.get_config() {
            config.standard_effect = effect_id;
            config.standard_effect_params = params.clone();
            if let Err(e) = config.write_to_file() {
                eprintln!("Error write config {:?}", e);
            }
        }
        if let Some(laptop) = self.get_device() {
            laptop.set_standard_effect(effect_id, params);
        }

        return true;
    }

    pub fn set_fan_rpm(&mut self, ac:usize, rpm: i32) -> bool {
        let mut res: bool = false;
        if let Some(config) = self.get_config() {
            config.power[ac].fan_rpm = rpm;
            if let Err(e) = config.write_to_file() {
                eprintln!("Error write config {:?}", e);
            }
        }

        if let Some(laptop) = self.get_device() {
            let state = laptop.get_ac_state();
            if state != ac {
                res = true;
            } else {
                res = match laptop.set_fan_rpm(rpm as u16) {
                    Ok(()) => true,
                    Err(error) => {
                        eprintln!("set_fan_rpm failed: {error:?}");
                        false
                    }
                };
            }
        }

        return res;
    }

    pub fn set_logo_led_state(&mut self, ac:usize, logo_state: u8) -> bool {
        let mut res: bool = false;
        let mut is_synced = false;
        
        if let Some(config) = self.get_config() {
            is_synced = config.sync;
            config.power[ac].logo_state = logo_state;
            if config.sync {
                let other = (ac + 1) & 0x01;
                config.power[other].logo_state = logo_state;
            }
            if let Err(e) = config.write_to_file() {
                eprintln!("Error write config {:?}", e);
            }
        }
             
        if let Some(laptop) = self.get_device() {
            let state = laptop.get_ac_state();
           
            if state != ac && !is_synced {
                res = true;
            } else {
                res = laptop.set_logo_led_state(logo_state);
            }
        }

        return res;
    }

    pub fn get_logo_led_state(&mut self, ac: usize) -> u8 {
        // if let Some(laptop) = self.get_device() {
            // if laptop.ac_state as usize == ac {
                // return laptop.get_logo_led_state();
            // }
        // }
    
        if let Some(config) = self.get_ac_config(ac) {
            return config.logo_state;
        }

        return 0;
    }

    pub fn set_brightness(&mut self, ac:usize, brightness: u8) -> bool {
        let mut res: bool = false;
        let clamped = if brightness > 100 { 100u16 } else { brightness as u16 };
        let _val = clamped * 255 / 100;
        let mut is_synced = false;
        
        if let Some(config) = self.get_config() {
            is_synced = config.sync;
            config.power[ac].brightness = _val as u8;
            if config.sync {
                let other = (ac + 1) & 0x01;
                config.power[other].brightness = _val as u8;
            }
            if let Err(e) = config.write_to_file() {
                eprintln!("Error write config {:?}", e);
            }
        }
 
        if let Some(laptop) = self.get_device() {
            let state = laptop.get_ac_state();
            // If sync is enabled, the new brightness applies to both states, so update hardware regardless
            if state != ac && !is_synced {
                res = true;
            } else {
                res = laptop.set_brightness(_val as u8);
            }
        }

        return res;
    }

    pub fn get_brightness(&mut self, ac: usize) -> u8 {
        if let Some(config) = self.get_ac_config(ac) {
            let val = config.brightness as u32;
            let mut perc = val * 100 * 100/ 255;
            perc += 50;
            perc /= 100;
            return perc as u8;
        }

        return 0
    }

    pub fn get_actual_fan_rpm(&mut self) -> i32 {
        // IPC boundary: convert to the legacy i32 until Task 9 exposes a typed
        // status. The typed error is logged here and never silently defaulted;
        // Task 9 removes this zero-on-failure conversion.
        let pid: u16 = match self.get_device() {
            Some(laptop) => laptop.pid(),
            None => return 0,
        };
        if let Some(laptop) = self.get_device() {
            if pid == thermal::BLADE_16_2025_PID {
                // The Blade 16 2025 exposes a live tachometer (0x88); read the CPU
                // zone as the representative fan speed.
                match laptop.read_current_fan_rpm(thermal::FanId::Cpu) {
                    Ok(rpm) => return i32::from(rpm.0),
                    Err(error) => {
                        eprintln!("get_actual_fan_rpm: tachometer read failed: {error:?}");
                        return 0;
                    }
                }
            }
            match laptop.read_fan_rpm_from_ec() {
                Ok(rpm) => return rpm as i32,
                Err(error) => {
                    eprintln!("get_actual_fan_rpm: EC read failed: {error:?}");
                    return 0;
                }
            }
        }
        return 0;
    }

    pub fn get_fan_rpm(&mut self, ac: usize) -> i32 {
        let live_fan_setting = {
            if let Some(laptop) = self.get_device() {
                let state = laptop.get_ac_state();
                if state == ac {
                    match laptop.read_fan_setting() {
                        Ok(rpm) => Some(rpm as i32),
                        Err(error) => {
                            eprintln!("get_fan_rpm: live read failed: {error:?}");
                            None
                        }
                    }
                } else {
                    None
                }
            } else {
                None
            }
        };

        if let Some(rpm) = live_fan_setting {
            return rpm;
        }

        if let Some(config) = self.get_ac_config(ac) {
            return config.fan_rpm;
        }

        return 0;
    }

    pub fn get_power_mode(&mut self, ac:usize) -> u8 {
        if let Some(config) = self.get_ac_config(ac) {
            return config.power_mode;
        }

        return 0;
    }

    pub fn get_cpu_boost(&mut self, ac:usize) -> u8 {
        if let Some(config) = self.get_ac_config(ac) {
            return config.cpu_boost;
        }

        return 0;
    }

    pub fn get_gpu_boost(&mut self, ac:usize) -> u8 {
        if let Some(config) = self.get_ac_config(ac) {
            return config.gpu_boost;
        }

        return 0;
    }

    pub fn set_ac_state(&mut self, ac: bool) {
        if let Some(laptop) = self.get_device() {
            laptop.set_ac_state(ac);
        }
        self.change_idle = true;
        let config: Option<config::PowerConfig> = self.get_ac_config(ac as usize);
        if let Some(config) = config {
            if let Some(laptop) = self.get_device() {
                if let Err(error) = laptop.set_config(config) {
                    eprintln!("set_ac_state: apply config failed: {error:?}");
                }
            }
        }
    }

    pub fn set_ac_state_get(&mut self) {
        let dbus_system = match Connection::new_system() {
            Ok(conn) => conn,
            Err(e) => {
                eprintln!("Failed to connect to D-Bus system bus: {}", e);
                return;
            }
        };
        let proxy_ac = dbus_system.with_proxy("org.freedesktop.UPower", "/org/freedesktop/UPower/devices/line_power_AC0", time::Duration::from_millis(5000));
        use battery::OrgFreedesktopUPowerDevice;
        if let Ok(online) = proxy_ac.online() {
            if let Some(laptop) = self.get_device() {
                laptop.set_ac_state(online);
            }
            self.change_idle = true;
            let config: Option<config::PowerConfig> = self.get_ac_config(online as usize);
            if let Some(config) = config {
                println!(
                    "set_ac_state_get: UPower online={} applying mode={} cpu={} gpu={}",
                    online, config.power_mode, config.cpu_boost, config.gpu_boost
                );
                if let Some(laptop) = self.get_device() {
                    if let Err(error) = laptop.set_config(config) {
                        eprintln!("set_ac_state_get: apply config failed: {error:?}");
                    }
                }
            }
        } else {
            eprintln!("set_ac_state_get: UPower online() read failed; no profile applied this pass");
        }

    }

    /// Set the active AC-state index without applying the saved profile. Used on
    /// a failed preflight, where the index still drives idle timeouts but no
    /// thermal or power writes may be sent.
    pub fn set_ac_index(&mut self, online: bool) {
        if let Some(laptop) = self.get_device() {
            laptop.set_ac_state(online);
        }
        self.change_idle = true;
    }

    /// Run the getter-only diagnostic sweep. Non-02C6 devices have no thermal
    /// getters to trust, so their preflight is trivially Ready and their saved
    /// state applies unchanged. For the Blade 16 2025 a failed sweep returns
    /// Disabled so the caller withholds automatic thermal/power writes.
    pub fn preflight(&mut self) -> thermal::ThermalSafetyState {
        let pid: u16 = match self.get_device() {
            Some(laptop) => laptop.pid(),
            None => return thermal::ThermalSafetyState::Disabled,
        };
        let state: thermal::ThermalSafetyState = if pid != thermal::BLADE_16_2025_PID {
            thermal::ThermalSafetyState::Ready
        } else {
            match self.get_device() {
                Some(laptop) => match laptop.preflight_probe() {
                    Ok(report) => {
                        println!("thermal preflight passed: {report:?}");
                        thermal::ThermalSafetyState::Ready
                    }
                    Err(error) => {
                        eprintln!("thermal preflight failed: {error:?}");
                        thermal::ThermalSafetyState::Disabled
                    }
                },
                None => thermal::ThermalSafetyState::Disabled,
            }
        };
        // Store the posture so live power/fan writes and the monitor observe it.
        if let Some(laptop) = self.get_device() {
            laptop.set_thermal_safety(state);
        }
        state
    }

    /// The fixed fan target currently under tachometer monitoring, if any. Used
    /// by the thermal monitor thread to decide whether a verification cycle is due.
    pub fn thermal_manual_target(&mut self) -> Option<thermal::FanRpm> {
        match self.get_device() {
            Some(laptop) => laptop.thermal_manual_target(),
            None => None,
        }
    }

    /// Run one fixed-fan verification cycle against the EC (Blade 16 2025).
    pub fn run_thermal_verification_cycle(&mut self) {
        if let Some(laptop) = self.get_device() {
            laptop.run_manual_verification_cycle();
        }
    }

    /// Whether the discovered device is the Blade 16 2025, which alone runs the
    /// verified thermal application and tachometer monitor.
    pub fn is_blade_16_2025(&mut self) -> bool {
        self.get_device().map_or(false, |laptop| laptop.pid() == thermal::BLADE_16_2025_PID)
    }

    /// Read the current physical power source from UPower. The mode set to sweep
    /// during limit collection depends on it, so a read failure is a hard error
    /// rather than an assumed default.
    fn current_power_source(&self) -> Result<thermal::PowerSource, LimitCollectionError> {
        let dbus_system = Connection::new_system()
            .map_err(|_| LimitCollectionError::PowerSourceUnavailable)?;
        let proxy_ac = dbus_system.with_proxy(
            "org.freedesktop.UPower",
            "/org/freedesktop/UPower/devices/line_power_AC0",
            time::Duration::from_millis(5000),
        );
        use battery::OrgFreedesktopUPowerDevice;
        let online = proxy_ac
            .online()
            .map_err(|_| LimitCollectionError::PowerSourceUnavailable)?;
        Ok(if online {
            thermal::PowerSource::Ac
        } else {
            thermal::PowerSource::Battery
        })
    }

    /// Supervised limit collection: sweep the modes valid for the current power
    /// source, reading each mode's global fan limits, then restore the original
    /// mode. See `RazerLaptop::collect_thermal_limits` for the per-mode loop.
    pub fn collect_thermal_limits(
        &mut self,
    ) -> Result<Vec<thermal::ModeLimits>, LimitCollectionError> {
        let pid: u16 = match self.get_device() {
            Some(laptop) => laptop.pid(),
            None => return Err(LimitCollectionError::PowerSourceUnavailable),
        };
        if pid != thermal::BLADE_16_2025_PID {
            return Err(LimitCollectionError::UnsupportedDevice { pid });
        }
        let source: thermal::PowerSource = self.current_power_source()?;
        match self.get_device() {
            Some(laptop) => laptop.collect_thermal_limits(source),
            None => Err(LimitCollectionError::PowerSourceUnavailable),
        }
    }

    pub fn get_device(&mut self) -> Option<&mut RazerLaptop> {
        return self.device.as_mut();
    }

    pub fn set_bho_handler(&mut self, is_on: bool, threshold: u8) -> bool {
        let result = self.get_device()
            .map_or(false, |laptop| laptop.set_bho(is_on, threshold));
        if result {
            if let Some(config) = self.get_config() {
                config.bho_on = is_on;
                config.bho_threshold = threshold;
                if let Err(e) = config.write_to_file() {
                    eprintln!("Error write config {:?}", e);
                }
            }
        }
        return result;
    }

    pub fn get_bho_handler(&mut self) -> Option<(bool, u8)> {
        // Check if device supports BHO
        let has_bho = self.get_device()
            .map_or(false, |laptop| laptop.have_feature("bho".to_string()));
        if !has_bho {
            return None;
        }
        if let Some(config) = self.get_config() {
            return Some((config.bho_on, config.bho_threshold));
        }
        return None;
    }

    pub fn restore_bho(&mut self) {
        let (bho_on, bho_threshold) = {
            match self.get_config() {
                Some(config) => (config.bho_on, config.bho_threshold),
                None => return,
            }
        };
        if bho_on {
            if let Some(laptop) = self.get_device() {
                laptop.set_bho(bho_on, bho_threshold);
            }
        }
    }

    fn get_config(&mut  self) -> Option<&mut config::Configuration> {
        return self.config.as_mut();
    }

    // pub fn set_device(&mut self, device: RazerLaptop) {
        // self.device = Some(device);
    // }

    pub fn find_supported_device(&mut self, vid: u16, pid: u16) -> Option<&SupportedDevice> {
        for device in &self.supported_devices {
            // Unwrap: we control the strings and know they are are valid
            let svid = u16::from_str_radix(&device.vid, 16).unwrap();
            let spid = u16::from_str_radix(&device.pid, 16).unwrap();

            if svid == vid && spid == pid {
                return Some(device);
            }
        }

        None
    }

    pub fn discover_devices(&mut self)  {
        // Check if socket is OK
        match HidApi::new() {
            Ok(api) => {
                // Primary path: interface 0 via hidapi.
                // hidapi's linux-native (hidraw) backend returns -1 for
                // interface_number(), so resolve the real USB interface
                // number from sysfs when the value is unavailable.
                for device in api.device_list().filter(|d| d.vendor_id() == RAZER_VENDOR_ID) {
                    let iface = if device.interface_number() >= 0 {
                        device.interface_number()
                    } else {
                        // Derive interface from sysfs via the device path
                        let path_str = device.path().to_str().unwrap_or_default();
                        let hidraw_name = path_str.rsplit('/').next().unwrap_or("");
                        Self::hidraw_iface_number(hidraw_name).unwrap_or(-1)
                    };
                    if iface != 0 {
                        continue;
                    }

                    if let Some(supported_device) = self.find_supported_device(device.vendor_id(), device.product_id()) {
                        match api.open_path(device.path()) {
                            Ok(dev) => {
                                self.device = Some(RazerLaptop::new(
                                    supported_device.name.clone(),
                                    supported_device.features.clone(),
                                    supported_device.fan.clone(),
                                    device.product_id(),
                                    dev,
                                ));
                                return;
                            }
                            Err(e) => {
                                eprintln!(
                                    "Failed to open supported device on iface 0 ({:04x}:{:04x}): {}",
                                    device.vendor_id(),
                                    device.product_id(),
                                    e
                                );
                            }
                        }
                    }
                }

                // Fallback #1: direct /dev/hidrawX probing based on /sys VID/PID.
                // Collect candidates and sort by USB interface number so we
                // prefer interface 0 (the one that accepts feature reports).
                let mut candidates: Vec<(String, u16, u16, i32)> = Vec::new();
                if let Ok(entries) = fs::read_dir("/dev") {
                    for entry in entries.flatten() {
                        let name = match entry.file_name().into_string() {
                            Ok(n) => n,
                            Err(_) => continue,
                        };
                        if !name.starts_with("hidraw") {
                            continue;
                        }

                        let Some((vid, pid)) = Self::hidraw_vid_pid(&name) else {
                            continue;
                        };

                        if vid != RAZER_VENDOR_ID {
                            continue;
                        }

                        let iface = Self::hidraw_iface_number(&name).unwrap_or(999);
                        eprintln!("hidraw fallback candidate: /dev/{} vid={:04x} pid={:04x} iface={}", name, vid, pid, iface);
                        candidates.push((name, vid, pid, iface));
                    }
                }
                candidates.sort_by_key(|c| c.3); // prefer lowest interface number

                for (name, vid, pid, iface) in candidates {
                    if let Some(supported_device) = self.find_supported_device(vid, pid) {
                        let path = format!("/dev/{}", name);
                        let c_path = match CString::new(path.clone()) {
                            Ok(p) => p,
                            Err(_) => continue,
                        };
                        eprintln!(
                            "Trying hidraw fallback open for {} ({:04x}:{:04x}) on {} (iface {})",
                            supported_device.name,
                            vid,
                            pid,
                            path,
                            iface,
                        );
                        match api.open_path(c_path.as_c_str()) {
                            Ok(dev) => {
                                self.device = Some(RazerLaptop::new(
                                    supported_device.name.clone(),
                                    supported_device.features.clone(),
                                    supported_device.fan.clone(),
                                    pid,
                                    dev,
                                ));
                                return;
                            }
                            Err(e) => {
                                eprintln!(
                                    "hidraw fallback open failed for {} ({:04x}:{:04x}) on {}: {}",
                                    supported_device.name,
                                    vid,
                                    pid,
                                    path,
                                    e
                                );
                            }
                        }
                    }
                }

                eprintln!("No supported Razer HID device could be opened");
            },
            Err(e) => {
                eprintln!("Error: {}", e);
            },
        }
    }
}

pub struct RazerLaptop {
    name: String,
    pub(crate) features: Vec<String>,
    fan: Vec<u16>,
    pid: u16,
    device: hidapi::HidDevice,
    power: u8, // need for fan
    fan_rpm: u8, // need for power
    ac_state: u8, // index config array
    screensaver: bool,
    transaction_id: u8,
    // Live thermal-safety posture. Only ever leaves Preflight for the Blade 16
    // 2025; other models stay in a posture that permits their legacy writes.
    thermal_safety: thermal::ThermalSafetyState,
}
//
impl RazerLaptop {
// LED STORAGE Options
    const NOSTORE:u8 = 0x00;
    const VARSTORE:u8 = 0x01;
// LED definitions
    const LOGO_LED:u8 = 0x04;
    const BACKLIGHT_LED:u8 = 0x05;
// effects
    pub const OFF:u8 = 0x00;
    pub const WAVE:u8 = 0x01;
    pub const REACTIVE:u8 = 0x02; // Afterglo
    #[allow(dead_code)]
    pub const BREATHING:u8 = 0x03;
    pub const SPECTRUM:u8 = 0x04;
    pub const CUSTOMFRAME:u8 = 0x05;
    pub const STATIC:u8 = 0x06;
    #[allow(dead_code)]
    pub const STARLIGHT:u8 = 0x19;

    // Command-confirm tuning, mirroring Synapse's UsbRzDeviceAction handshake:
    // write the report, then re-read the reply until the EC reports SUCCESS. The
    // EC answers BUSY/NEW while it is still processing (notably right after
    // resume) and leaves the previous command's reply in the buffer when read too
    // early. The old path read once after a flat 1ms, so unconfirmed writes slipped
    // through and the GPU/fan re-apply bursts had to paper over them.
    const SEND_WRITE_ATTEMPTS: usize = 3;
    const SEND_READ_POLLS: usize = 20;
    const SEND_POLL_INTERVAL_MS: u64 = 5;

    pub fn new(name: String, features: Vec<String>, fan: Vec<u16>, pid: u16, device: hidapi::HidDevice) -> RazerLaptop {
        return RazerLaptop{
            name,
            features,
            fan,
            pid,
            device,
            power: 0,
            fan_rpm: 0,
            ac_state: 0,
            screensaver: false,
            transaction_id: 0,
            thermal_safety: thermal::ThermalSafetyState::Preflight,
        };
    }

    pub fn pid(&self) -> u16 {
        return self.pid;
    }

    /// Set the live thermal-safety posture from the preflight result.
    pub fn set_thermal_safety(&mut self, state: thermal::ThermalSafetyState) {
        self.thermal_safety = state;
    }

    /// The live thermal-safety posture, so the resume path can fold a failed
    /// repair into the same failure counter the monitor uses.
    pub fn thermal_safety(&self) -> thermal::ThermalSafetyState {
        self.thermal_safety
    }

    fn current_power_source(&self) -> thermal::PowerSource {
        // ac_state is 1 only when UPower reports line power online.
        if self.ac_state == 1 {
            thermal::PowerSource::Ac
        } else {
            thermal::PowerSource::Battery
        }
    }

    pub fn set_screensaver(&mut self, active: bool) {
        self.screensaver = active;
    }

    pub fn set_config(&mut self, config: config::PowerConfig) -> Result<(), ThermalError> {
        if !self.screensaver {
            self.set_brightness(config.brightness);
            self.set_logo_led_state(config.logo_state);
        } else {
            self.set_brightness(0);
            self.set_logo_led_state(0);
        }
        self.set_power_mode(config.power_mode, config.cpu_boost, config.gpu_boost)?;
        self.set_fan_rpm(config.fan_rpm as u16)?;
        Ok(())
    }

    pub fn set_ac_state(&mut self, online: bool) -> usize {
        if online {
            self.ac_state = 1;
        } else {
            self.ac_state = 0;
        }

        return  self.ac_state as usize;
    }

    pub fn get_ac_state(&mut self) -> usize {
        return self.ac_state as usize;
    }

    pub fn get_name(&self) -> String {
        return self.name.clone();
    }

    pub fn have_feature(&mut self, fch: String) -> bool {
        return self.features.contains(&fch);
    }

    fn clamp_fan(&mut self, rpm: u16) -> u8 {
        if rpm > self.fan[1] {
            return (self.fan[1] / 100) as u8;
        }
        if rpm < self.fan[0] {
            return (self.fan[0] / 100) as u8;
        }

        return (rpm / 100) as u8;
    }

    fn clamp_u8(&mut self, value: u8, min: u8, max: u8) ->u8 {
        if value > max {
            return max;
        }
        if value < min {
            return min;
        }

        return value;
    }

    pub fn set_standard_effect(&mut self, effect_id: u8, params: Vec<u8>) -> bool {
        let mut report: RazerPacket = RazerPacket::new(0x03, 0x0a, 80);
        report.args[0] = effect_id; // effect id
        if !params.is_empty() {
            let len = params.len().min(79); // args[0] is effect_id, so max 79 param bytes
            for idx in 0..len {
                report.args[idx+1] = params[idx];
            }
        }
        if let Some(_) = self.send_report_logging(report) {
            return true;
        }

        return false;
    }

    pub fn set_custom_frame_data(&mut self, row: u8, data: Vec<u8>) {
        // if data.len() == kbd::board::KEYS_PER_ROW {
        if data.len() == 45 {
            let mut report: RazerPacket = RazerPacket::new(0x03, 0x0b, 0x34);
            report.args[0] = 0xff;
            report.args[1] = row;
            report.args[2] = 0x00; // start col
            report.args[3] = 0x0f; // end col
            for idx in 0..data.len() {
                report.args[idx + 7] = data[idx];
            }
            self.send_report_logging(report);
        }
    }

    pub fn set_custom_frame(&mut self) -> bool {
        let mut report: RazerPacket = RazerPacket::new(0x03, 0x0a, 0x02);
        report.args[0] = RazerLaptop::CUSTOMFRAME; // effect id
        report.args[1] = RazerLaptop::NOSTORE;
        if let Some(_) = self.send_report_logging(report) {
            return true;
        }

        return false;
    }

    pub fn get_power_mode(&mut self, zone: u8) -> Result<u8, TransportError> {
        let (mode_byte, _manual_flag) = self.read_zone_fan_state(zone)?;
        Ok(mode_byte)
    }

    fn read_zone_fan_state(&mut self, zone: u8) -> Result<(u8, u8), TransportError> {
        let mut report: RazerPacket = RazerPacket::new(0x0d, 0x82, 0x04);
        // profileId=1 must match the write paths so readback queries the same slot.
        report.args[0] = 0x01;
        report.args[1] = zone;
        report.args[2] = 0x00;
        report.args[3] = 0x00;
        let response = self.send_report(report)?;
        Ok((response.args[2], response.args[3]))
    }

    /// Set a fan zone's mode via Set Thermal Fan Mode (0x0d/0x02).
    /// Wire layout matches Synapse: [profileId=1, fanId, fanMode, fanModeValue].
    /// `fanMode` (args[2]) MUST be the currently-active performance mode: the EC
    /// keys the per-zone manual/auto setting to that mode's slot, so a constant
    /// here writes the setting to an inactive slot. Using `self.power` keeps the
    /// write on the same slot `set_power()` activates.
    /// `manual_flag` (args[3]): 1 = manual, 0 = auto.
    fn set_zone_fan_state(&mut self, zone: u8, manual_flag: u8) -> Result<(), TransportError> {
        let mut report: RazerPacket = RazerPacket::new(0x0d, 0x02, 0x04);
        report.args[0] = 0x01;
        report.args[1] = zone;
        report.args[2] = self.power;
        report.args[3] = manual_flag;
        self.send_report(report)?;
        Ok(())
    }

    fn read_stored_fan_setpoint(&mut self, zone: u8) -> Result<u16, TransportError> {
        let mut report: RazerPacket = RazerPacket::new(0x0d, 0x81, 0x03);
        // profileId=1 must match the write paths so readback queries the same slot.
        report.args[0] = 0x01;
        report.args[1] = zone;
        report.args[2] = 0x00;
        let response = self.send_report(report)?;
        Ok(u16::from(response.args[2]) * 100)
    }

    pub fn read_fan_setting(&mut self) -> Result<u16, TransportError> {
        let (_mode_byte, manual_flag) = self.read_zone_fan_state(0x01)?;
        if manual_flag == 0 {
            return Ok(0);
        }
        self.read_stored_fan_setpoint(0x01)
    }

    fn set_power(&mut self, zone: u8) -> Result<(), TransportError> {
        let mut report: RazerPacket = RazerPacket::new(0x0d, 0x02, 0x04);
        // profileId=1 must match set_zone_fan_state so byte0 does not thrash 0<->1.
        report.args[0] = 0x01;
        report.args[1] = zone;
        report.args[2] = self.power;
        match self.fan_rpm {
            0 => report.args[3] = 0x00,
            _ => report.args[3] = 0x01
        }
        self.send_report(report)?;
        Ok(())
    }

    /// Read a zone's custom CPU/GPU boost level. For the Blade 16 2025 this uses
    /// the profile-1 0x87 builder with a validated readback; other models keep
    /// the legacy profile-0 read.
    pub fn get_cpu_boost(&mut self) -> Result<u8, ThermalError> {
        self.read_boost(thermal::FanId::Cpu)
    }

    pub fn get_gpu_boost(&mut self) -> Result<u8, ThermalError> {
        self.read_boost(thermal::FanId::Gpu)
    }

    fn read_boost(&mut self, fan: thermal::FanId) -> Result<u8, ThermalError> {
        if self.pid == thermal::BLADE_16_2025_PID {
            let reply: [u8; 80] = self.send_thermal(&thermal::get_boost(fan))?;
            return Ok(thermal::decode_boost(fan, &reply)?);
        }
        Ok(self.read_boost_legacy(fan.wire_value())?)
    }

    fn read_boost_legacy(&mut self, zone: u8) -> Result<u8, TransportError> {
        let mut report: RazerPacket = RazerPacket::new(0x0d, 0x87, 0x03);
        report.args[0] = 0x00;
        report.args[1] = zone;
        report.args[2] = 0x00;
        let response = self.send_report(report)?;
        Ok(response.args[2])
    }

    fn set_cpu_boost(&mut self, mut boost: u8) -> Result<(), TransportError> {
        let mut report: RazerPacket = RazerPacket::new(0x0d, 0x07, 0x03);
        if boost == 3 && !self.have_feature("boost".to_string()) {
            boost = 2;
        }
        report.args[0] = 0x00;
        report.args[1] = 0x01;
        report.args[2] = boost;
        self.send_report(report)?;
        Ok(())
    }

    fn set_gpu_boost(&mut self, boost: u8) -> Result<(), TransportError> {
        let mut report: RazerPacket = RazerPacket::new(0x0d, 0x07, 0x03);
        report.args[0] = 0x00;
        report.args[1] = 0x02;
        report.args[2] = boost;
        self.send_report(report)?;
        Ok(())
    }

    /// Apply a performance mode (and, in Custom, per-zone boost). The Blade 16
    /// 2025 goes through the verified profile-1 sequence; other models keep the
    /// legacy profile-0 boost path unchanged.
    pub fn set_power_mode(&mut self, mode: u8, cpu_boost: u8, gpu_boost: u8) -> Result<(), ThermalError> {
        if self.pid == thermal::BLADE_16_2025_PID {
            if self.thermal_safety.writes_disabled() {
                return Err(ThermalError::WritesDisabled);
            }
            return self.apply_power_verified(mode, cpu_boost, gpu_boost);
        }
        Ok(self.set_power_mode_legacy(mode, cpu_boost, gpu_boost)?)
    }

    fn set_power_mode_legacy(&mut self, mode: u8, cpu_boost: u8, gpu_boost: u8) -> Result<(), TransportError> {
        if mode <= 3 {
            self.power = mode;
            self.set_power(0x01)?;
            self.set_power(0x02)?;
        } else if mode == 4 {
            self.power =  mode;
            self.fan_rpm = 0;
            self.get_power_mode(0x01)?;
            self.set_power(0x01)?;
            self.read_boost_legacy(0x01)?;
            self.set_cpu_boost(cpu_boost)?;
            self.read_boost_legacy(0x02)?;
            self.set_gpu_boost(gpu_boost)?;
            self.get_power_mode(0x02)?;
            self.set_power(0x02)?;
        }

        Ok(())
    }

    /// Verified power/boost application (Blade 16 2025). The whole sequence gets
    /// exactly one bounded second attempt: on a first failure it is retried once,
    /// after which the failure stands.
    fn apply_power_verified(&mut self, mode: u8, cpu_boost: u8, gpu_boost: u8) -> Result<(), ThermalError> {
        match self.apply_power_once(mode, cpu_boost, gpu_boost) {
            Ok(()) => Ok(()),
            Err(first) => {
                eprintln!("verified power apply failed, retrying once: {first:?}");
                self.apply_power_once(mode, cpu_boost, gpu_boost)
            }
        }
    }

    fn apply_power_once(&mut self, mode_byte: u8, cpu_boost: u8, gpu_boost: u8) -> Result<(), ThermalError> {
        let mode: thermal::PerformanceMode = thermal::PerformanceMode::try_from(mode_byte)?;
        let source: thermal::PowerSource = self.current_power_source();
        if !thermal::is_mode_selectable(source, mode) {
            return Err(ThermalError::Policy(thermal::ThermalPolicyError::ModeNotSelectable { mode, source }));
        }
        self.power = mode_byte;
        // A fixed fan speed already stored keeps the manual flag set while the
        // mode is re-written, so the mode change does not silently release it.
        let manual: bool = self.fan_rpm != 0;
        for fan in [thermal::FanId::Cpu, thermal::FanId::Gpu] {
            self.send_thermal(&thermal::set_fan_mode(fan, mode, manual))?;
        }
        for fan in [thermal::FanId::Cpu, thermal::FanId::Gpu] {
            let reply: [u8; 80] = self.send_thermal(&thermal::get_fan_mode(fan))?;
            let (observed_mode, _observed_manual) = thermal::decode_fan_mode(fan, &reply)?;
            if observed_mode != mode.wire_value() {
                return Err(ThermalError::ReadbackMismatch {
                    command_id: 0x82,
                    requested: mode.wire_value(),
                    observed: observed_mode,
                });
            }
        }
        if mode == thermal::PerformanceMode::Custom {
            let cpu_level: u8 = thermal::validate_custom_level(cpu_boost)?;
            let gpu_level: u8 = thermal::validate_custom_level(gpu_boost)?;
            for (fan, level) in [(thermal::FanId::Cpu, cpu_level), (thermal::FanId::Gpu, gpu_level)] {
                self.send_thermal(&thermal::set_boost(fan, level))?;
            }
            for (fan, level) in [(thermal::FanId::Cpu, cpu_level), (thermal::FanId::Gpu, gpu_level)] {
                let reply: [u8; 80] = self.send_thermal(&thermal::get_boost(fan))?;
                let observed: u8 = thermal::decode_boost(fan, &reply)?;
                if observed != level {
                    return Err(ThermalError::ReadbackMismatch {
                        command_id: 0x87,
                        requested: level,
                        observed,
                    });
                }
            }
        }
        Ok(())
    }

    /// Apply a fan speed. The Blade 16 2025 goes through the verified profile-1
    /// sequence and arms tachometer monitoring for a fixed speed; other models
    /// keep the legacy profile-0 path unchanged.
    pub fn set_fan_rpm(&mut self, value: u16) -> Result<(), ThermalError> {
        if self.pid == thermal::BLADE_16_2025_PID {
            if self.thermal_safety.writes_disabled() {
                return Err(ThermalError::WritesDisabled);
            }
            return self.apply_fan_verified(value);
        }
        Ok(self.set_fan_rpm_legacy(value)?)
    }

    fn set_fan_rpm_legacy(&mut self, value: u16) -> Result<(), TransportError> {
        if value == 0 {
            self.fan_rpm = 0;
            self.set_zone_fan_state(0x01, 0x00)?;
            self.set_zone_fan_state(0x02, 0x00)?;
            return Ok(());
        }

        self.fan_rpm = self.clamp_fan(value);
        self.set_zone_fan_state(0x01, 0x01)?;
        self.set_zone_fan_state(0x02, 0x01)?;
        self.set_rpm(0x01)?;
        self.set_rpm(0x02)?;
        Ok(())
    }

    /// Verified fan application (Blade 16 2025), retried once on failure.
    fn apply_fan_verified(&mut self, value: u16) -> Result<(), ThermalError> {
        match self.apply_fan_once(value) {
            Ok(()) => Ok(()),
            Err(first) => {
                eprintln!("verified fan apply failed, retrying once: {first:?}");
                self.apply_fan_once(value)
            }
        }
    }

    fn apply_fan_once(&mut self, value: u16) -> Result<(), ThermalError> {
        let mode: thermal::PerformanceMode = thermal::PerformanceMode::try_from(self.power)?;
        match thermal::validate_fixed_rpm(mode, i32::from(value))? {
            None => {
                for fan in [thermal::FanId::Cpu, thermal::FanId::Gpu] {
                    self.send_thermal(&thermal::set_fan_mode(fan, mode, false))?;
                }
                self.fan_rpm = 0;
                self.thermal_safety = thermal::ThermalSafetyState::Ready;
                Ok(())
            }
            Some(target) => {
                // The 0x01 setter carries rpm/100, so the EC latches (and reads
                // back) a value rounded to the nearest lower 100 RPM; the monitor
                // must compare tachometer samples against that commanded value.
                let commanded_hundreds: u8 = (target.0 / 100) as u8;
                let commanded_rpm: u16 = u16::from(commanded_hundreds) * 100;
                for fan in [thermal::FanId::Cpu, thermal::FanId::Gpu] {
                    self.send_thermal(&thermal::set_fan_mode(fan, mode, true))?;
                }
                for fan in [thermal::FanId::Cpu, thermal::FanId::Gpu] {
                    self.send_thermal(&thermal::set_fan_speed(fan, thermal::FanRpm(commanded_rpm)))?;
                }
                for fan in [thermal::FanId::Cpu, thermal::FanId::Gpu] {
                    let reply: [u8; 80] = self.send_thermal(&thermal::get_fan_speed(fan))?;
                    let observed: u16 = thermal::decode_fan_setpoint(fan, &reply)?;
                    if observed != commanded_rpm {
                        return Err(ThermalError::ReadbackMismatch {
                            command_id: 0x81,
                            requested: commanded_hundreds,
                            observed: (observed / 100) as u8,
                        });
                    }
                }
                self.fan_rpm = commanded_hundreds;
                self.thermal_safety = thermal::ThermalSafetyState::Manual {
                    target: thermal::FanRpm(commanded_rpm),
                    consecutive_failures: 0,
                };
                Ok(())
            }
        }
    }

    /// The fixed fan target currently under tachometer monitoring, if any.
    pub fn thermal_manual_target(&self) -> Option<thermal::FanRpm> {
        match self.thermal_safety {
            thermal::ThermalSafetyState::Manual { target, .. } => Some(target),
            _ => None,
        }
    }

    /// Run one fixed-fan verification cycle: read both tachometers, classify the
    /// outcome with the pure state machine, and on the second consecutive failure
    /// fail both zones back to firmware-automatic control and enter Disabled.
    pub fn run_manual_verification_cycle(&mut self) {
        let target: thermal::FanRpm = match self.thermal_safety {
            thermal::ThermalSafetyState::Manual { target, .. } => target,
            _ => return,
        };
        let event: thermal::VerificationEvent = self.read_manual_cycle_event(target);
        if let thermal::VerificationEvent::Failed(failure) = event {
            eprintln!("thermal verification cycle failed: {failure:?}");
        }
        let transition: thermal::SafetyTransition = thermal::advance_safety(self.thermal_safety, event);
        self.thermal_safety = transition.state;
        if transition.action == Some(thermal::SafetyAction::FailbackBothFans) {
            let report: thermal::FailbackReport = self.failback_both_fans();
            eprintln!("thermal failback to firmware-automatic control: {report:?}");
        }
    }

    /// Read both zones' current tachometer RPM and classify the cycle. A failed
    /// read short-circuits to a Transport failure; otherwise each zone's sample
    /// is classified against the target and the first failure wins.
    fn read_manual_cycle_event(&mut self, target: thermal::FanRpm) -> thermal::VerificationEvent {
        for fan in [thermal::FanId::Cpu, thermal::FanId::Gpu] {
            match self.read_current_fan_rpm(fan) {
                Ok(reading) => match thermal::classify_manual_reading(target, reading.0) {
                    thermal::VerificationEvent::Succeeded => continue,
                    failed => return failed,
                },
                Err(error) => {
                    let failure: thermal::ThermalFailure = thermal_failure_class(&error);
                    eprintln!("thermal telemetry read failed for {fan:?}: {error:?}");
                    return thermal::VerificationEvent::Failed(failure);
                }
            }
        }
        thermal::VerificationEvent::Succeeded
    }

    /// Return both fan zones to firmware-automatic control. The GPU zone is
    /// attempted even when the CPU zone fails, and both outcomes are retained.
    fn failback_both_fans(&mut self) -> thermal::FailbackReport {
        let cpu: Result<(), ThermalError> = self.failback_zone(thermal::FanId::Cpu);
        let gpu: Result<(), ThermalError> = self.failback_zone(thermal::FanId::Gpu);
        thermal::FailbackReport { cpu: zone_outcome(cpu), gpu: zone_outcome(gpu) }
    }

    fn failback_zone(&mut self, fan: thermal::FanId) -> Result<(), ThermalError> {
        // Returning a zone to firmware-automatic control means clearing the manual
        // flag while keeping the active performance mode; the EC's own fan curve
        // then governs the zone. self.power is a validated mode byte set by the
        // last successful apply.
        let mode: thermal::PerformanceMode = thermal::PerformanceMode::try_from(self.power)?;
        self.send_thermal(&thermal::set_fan_mode(fan, mode, false))?;
        Ok(())
    }

    /// Getter-only post-wake re-verification (Blade 16 2025): read the current fan
    /// mode for both zones (0x82), the custom levels in Custom (0x87), and the fan
    /// setpoint when a fixed speed is expected (0x81), comparing each against what
    /// the daemon applied for the current power source. Any mismatch means the
    /// firmware reset the applied state during resume. Issues no writes.
    fn wake_readback(
        &mut self,
        expected_mode: u8,
        expected_cpu: u8,
        expected_gpu: u8,
        expected_fan_hundreds: u8,
    ) -> Result<thermal::WakeVerifyOutcome, ThermalError> {
        let mode: thermal::PerformanceMode = thermal::PerformanceMode::try_from(expected_mode)?;
        for fan in [thermal::FanId::Cpu, thermal::FanId::Gpu] {
            let reply: [u8; 80] = self.send_thermal(&thermal::get_fan_mode(fan))?;
            let (observed_mode, _manual) = thermal::decode_fan_mode(fan, &reply)?;
            if observed_mode != expected_mode {
                return Ok(thermal::WakeVerifyOutcome::StateReset);
            }
        }
        if mode == thermal::PerformanceMode::Custom {
            for (fan, level) in
                [(thermal::FanId::Cpu, expected_cpu), (thermal::FanId::Gpu, expected_gpu)]
            {
                let reply: [u8; 80] = self.send_thermal(&thermal::get_boost(fan))?;
                if thermal::decode_boost(fan, &reply)? != level {
                    return Ok(thermal::WakeVerifyOutcome::StateReset);
                }
            }
        }
        if expected_fan_hundreds != 0 {
            let reply: [u8; 80] = self.send_thermal(&thermal::get_fan_speed(thermal::FanId::Cpu))?;
            let observed: u16 = thermal::decode_fan_setpoint(thermal::FanId::Cpu, &reply)?;
            if observed != u16::from(expected_fan_hundreds) * 100 {
                return Ok(thermal::WakeVerifyOutcome::StateReset);
            }
        }
        Ok(thermal::WakeVerifyOutcome::StateHeld)
    }

    /// Re-latch only the GPU custom boost after a dGPU inactive-to-active
    /// transition (Blade 16 2025). In Custom mode, write then read back the GPU
    /// zone's custom level (0x07/0x87), touching no CPU state and no fan state;
    /// returns whether a command was issued. Non-Custom modes issue nothing.
    fn relatch_gpu_custom_boost(
        &mut self,
        mode: thermal::PerformanceMode,
        gpu_level: u8,
    ) -> Result<bool, ThermalError> {
        if self.thermal_safety.writes_disabled() {
            return Err(ThermalError::WritesDisabled);
        }
        let plan: Vec<thermal::ThermalCommand> = thermal::dgpu_resume_plan(mode, gpu_level);
        if plan.is_empty() {
            return Ok(false);
        }
        // The plan is only built in Custom; validate the level before any write so
        // an unvalidated level (e.g. Extreme) is rejected rather than sent.
        let level: u8 = thermal::validate_custom_level(gpu_level)?;
        for command in &plan {
            let reply: [u8; 80] = self.send_thermal(command)?;
            if command.command_id == 0x87 {
                let observed: u8 = thermal::decode_boost(thermal::FanId::Gpu, &reply)?;
                if observed != level {
                    return Err(ThermalError::ReadbackMismatch {
                        command_id: 0x87,
                        requested: level,
                        observed,
                    });
                }
            }
        }
        Ok(true)
    }

    fn set_rpm(&mut self, zone: u8) -> Result<(), TransportError> {
        let mut report:RazerPacket = RazerPacket::new(0x0d, 0x01, 0x03);
        // Set fan RPM. profileId=1 matches Synapse's classId (Set Thermal Fan Speed).
        report.args[0] = 0x01;
        report.args[1] = zone;
        report.args[2] = self.fan_rpm;
        self.send_report(report)?;
        Ok(())
    }

    #[allow(dead_code)]
    pub fn get_fan_rpm(&mut self) -> u16 {
        let res: u16 = self.fan_rpm as u16;
        return res * 100;
    }

    /// Read a zone's current tachometer RPM via the profile-1 0x88 builder,
    /// validating the reply's identity. A transport or decode failure is returned
    /// as a typed error, never converted into a zero or default reading.
    pub fn read_current_fan_rpm(&mut self, fan: thermal::FanId) -> Result<thermal::FanRpm, ThermalError> {
        let reply: [u8; 80] = self.send_thermal(&thermal::get_current_fan_rpm(fan))?;
        let rpm: u16 = thermal::decode_fan_rpm(fan, &reply)?;
        Ok(thermal::FanRpm(rpm))
    }

    /// Read the stored fan setpoint from the EC (profile-1 0x81, CPU zone). Used
    /// by non-Blade-16 models, which do not expose a live tachometer register.
    pub fn read_fan_rpm_from_ec(&mut self) -> Result<u16, TransportError> {
        self.read_stored_fan_setpoint(0x01)
    }

    pub fn set_logo_led_state(&mut self, mode: u8) -> bool {
        if mode > 0 {
            let mut report: RazerPacket = RazerPacket::new(0x03, 0x02, 0x03);
            report.args[0] = RazerLaptop::VARSTORE;
            report.args[1] = RazerLaptop::LOGO_LED;
            if mode == 1 {
                report.args[2] = 0x00;
            } else if mode == 2 {
                report.args[2] = 0x02;
            }
            self.send_report_logging(report);
        }

        let mut report: RazerPacket = RazerPacket::new(0x03, 0x00, 0x03);
        report.args[0] = RazerLaptop::VARSTORE;
        report.args[1] = RazerLaptop::LOGO_LED;
        report.args[2] = self.clamp_u8(mode, 0x00, 0x01);
        if let Some(_) = self.send_report_logging(report) {
            return true;
        }

        return false;
    }

    #[allow(dead_code)]
    pub fn get_logo_led_state(&mut self) -> u8 {
        let mut report: RazerPacket = RazerPacket::new(0x03, 0x82, 0x03);
        report.args[0] = RazerLaptop::VARSTORE;
        report.args[1] = RazerLaptop::LOGO_LED;
        if let Some(response) = self.send_report_logging(report){
            return response.args[2];
        }
        return 0;
    }

    pub fn set_brightness(&mut self, brightness: u8) -> bool {
        let mut report: RazerPacket = RazerPacket::new(0x03, 0x03, 0x03);
        report.args[0] = RazerLaptop::VARSTORE;
        report.args[1] = RazerLaptop::BACKLIGHT_LED;
        report.args[2] = brightness;
        if let Some(_) = self.send_report_logging(report) {
            return true;
        }

        return false;
    }

    #[allow(dead_code)]
    pub fn get_brightness(&mut self) -> u8 {
        let mut report: RazerPacket = RazerPacket::new(0x03, 0x83, 0x03);
        report.args[0] = RazerLaptop::VARSTORE;
        report.args[1] = RazerLaptop::BACKLIGHT_LED;
        report.args[2] = 0x00;
        if let Some(response) = self.send_report_logging(report){
            return response.args[2];
        }
        return 0;
    }

    #[allow(dead_code)]
    pub fn get_bho(&mut self) -> Option<u8> {
        if !self.have_feature("bho".to_string()) {
            return None;
        }

        let mut report: RazerPacket = RazerPacket::new(0x07, 0x92, 0x01);
        report.args[0] = 0x00;

        return self.send_report_logging(report)
            .map(|resp| resp.args[0]);
    }

    pub fn set_bho(&mut self, is_on: bool, threshold: u8) -> bool {
        if !self.have_feature("bho".to_string()) {
            return false;
        }

        let mut report = RazerPacket::new(0x07, 0x12, 0x01);
        report.args[0] = bho_to_byte(is_on, threshold);

        return self.send_report_logging(report)
            .map_or(false, |r| {
                println!("Response Packet:\n{:#?}", r); 
                true
            } 
        );
    }

    fn next_transaction_id(&mut self) -> u8 {
        // Razer transaction id cycles 0..=30; 31 is the reset boundary and never sent.
        if self.transaction_id == 31 {
            self.transaction_id = 0;
        }
        let id = self.transaction_id;
        self.transaction_id += 1;
        return id;
    }

    /// Frame a pure `ThermalCommand` into a 0x0d HID report, send it with the
    /// confirm-and-retry transport, and hand back the reply's 80 argument bytes.
    fn send_thermal(
        &mut self,
        command: &thermal::ThermalCommand,
    ) -> Result<[u8; 80], TransportError> {
        let mut report: RazerPacket =
            RazerPacket::new(THERMAL_COMMAND_CLASS, command.command_id, command.data_size);
        report.args = command.args;
        let response = self.send_report(report)?;
        Ok(response.args)
    }

    /// Run the getter-only preflight sweep against the EC. Every command in the
    /// plan must complete over the transport; the fan-ID list must decode to
    /// exactly {CPU, GPU}; and each 0x88 reply must decode for its own zone.
    pub fn preflight_probe(&mut self) -> Result<thermal::PreflightReport, PreflightError> {
        let plan: Vec<thermal::ThermalCommand> = thermal::preflight_plan();
        let mut fans: Option<[thermal::FanId; 2]> = None;
        let mut zones: Vec<thermal::ZoneTelemetry> = Vec::new();
        for command in &plan {
            let reply: [u8; 80] = self.send_thermal(command)?;
            match command.command_id {
                0x80 => fans = Some(thermal::decode_fan_ids(&reply)?),
                0x88 => {
                    // args[1] carries the zone this getter targeted, so the reply
                    // is decoded against the exact fan it was requested for.
                    if let Some(fan) = thermal::FanId::from_wire(command.args[1]) {
                        let current_rpm = thermal::decode_fan_rpm(fan, &reply)?;
                        zones.push(thermal::ZoneTelemetry { fan, current_rpm });
                    }
                }
                _ => {}
            }
        }
        // preflight_plan always begins with the 0x80 fan-ID query, so this is set.
        let fans = fans.expect("preflight_plan always includes the 0x80 fan-ID query");
        Ok(thermal::PreflightReport { fans, zones })
    }

    /// Read the currently active performance-mode byte via Get Thermal Fan Mode
    /// (0x82). The CPU zone is authoritative for the mode-global reading, and the
    /// reply's profile/fan identity is validated so a stale reply is never read
    /// as the active mode.
    fn read_active_mode(&mut self) -> Result<u8, LimitCollectionError> {
        let reply: [u8; 80] = self.send_thermal(&thermal::get_fan_mode(thermal::FanId::Cpu))?;
        let (mode_byte, _manual) = thermal::decode_fan_mode(thermal::FanId::Cpu, &reply)?;
        Ok(mode_byte)
    }

    /// Write a performance mode to both zones via Set Thermal Fan Mode (0x02),
    /// leaving the manual flag clear so the EC keeps automatic control while the
    /// mode-global limits are read.
    fn activate_mode(&mut self, mode: thermal::PerformanceMode) -> Result<(), TransportError> {
        for fan in [thermal::FanId::Cpu, thermal::FanId::Gpu] {
            self.send_thermal(&thermal::set_fan_mode(fan, mode, false))?;
        }
        Ok(())
    }

    /// Restore the mode the EC was in before collection started, verifying the
    /// readback. A failure here is the terminal error for the whole operation.
    fn restore_mode(&mut self, original_mode_byte: u8) -> Result<(), LimitCollectionError> {
        let mode: thermal::PerformanceMode =
            thermal::PerformanceMode::try_from(original_mode_byte)?;
        self.activate_mode(mode)?;
        let observed: u8 = self.read_active_mode()?;
        if observed != original_mode_byte {
            return Err(LimitCollectionError::ModeReadbackMismatch {
                requested: original_mode_byte,
                observed,
            });
        }
        Ok(())
    }

    /// Sweep the given modes, reading each mode's global fan limits after
    /// verifying the mode actually latched via a 0x82 readback. Exactly one
    /// argument-0 0x86 read is issued per mode.
    fn sweep_mode_limits(
        &mut self,
        modes: &[thermal::PerformanceMode],
    ) -> Result<Vec<thermal::ModeLimits>, LimitCollectionError> {
        let mut collected: Vec<thermal::ModeLimits> = Vec::with_capacity(modes.len());
        for &mode in modes {
            self.activate_mode(mode)?;
            let observed: u8 = self.read_active_mode()?;
            if observed != mode.wire_value() {
                return Err(LimitCollectionError::ModeReadbackMismatch {
                    requested: mode.wire_value(),
                    observed,
                });
            }
            let reply: [u8; 80] = self.send_thermal(&thermal::get_fan_limits())?;
            let limits: thermal::FanLimits = thermal::decode_fan_limits(&reply)?;
            collected.push(thermal::ModeLimits { mode, limits });
        }
        Ok(collected)
    }

    /// Supervised limit collection for the Blade 16 2025 EC: record the original
    /// mode, sweep the modes valid for `source`, then always restore the original
    /// mode. Restoration failure outranks any collection failure that preceded it
    /// (see `thermal::resolve_with_restoration`).
    pub fn collect_thermal_limits(
        &mut self,
        source: thermal::PowerSource,
    ) -> Result<Vec<thermal::ModeLimits>, LimitCollectionError> {
        if self.pid != thermal::BLADE_16_2025_PID {
            return Err(LimitCollectionError::UnsupportedDevice { pid: self.pid });
        }
        let original_mode_byte: u8 = self.read_active_mode()?;
        let collection: Result<Vec<thermal::ModeLimits>, LimitCollectionError> =
            self.sweep_mode_limits(thermal::selectable_modes(source));
        let restoration: Result<(), LimitCollectionError> = self.restore_mode(original_mode_byte);
        thermal::resolve_with_restoration(collection, restoration)
    }

    /// Convert a transport failure into the legacy `Option` shape, logging the
    /// full typed error. Only the non-thermal bool/effect setters use this;
    /// thermal and power methods propagate `TransportError` instead.
    fn send_report_logging(&mut self, report: RazerPacket) -> Option<RazerPacket> {
        match self.send_report(report) {
            Ok(response) => Some(response),
            Err(error) => {
                eprintln!("HID command failed: {error:?}");
                None
            }
        }
    }

    fn send_report(&mut self, mut report: RazerPacket) -> Result<RazerPacket, TransportError> {
        let poll_interval = time::Duration::from_millis(Self::SEND_POLL_INTERVAL_MS);
        let mut last_error: Option<TransportError> = None;

        for _ in 0..Self::SEND_WRITE_ATTEMPTS {
            // Rotate the transaction id per write so a resend is not mistaken for a
            // duplicate of the previous attempt.
            report.id = self.next_transaction_id();
            let request = report.serialize()?;
            if let Err(e) = self.device.send_feature_report(&request) {
                last_error = Some(TransportError::Write {
                    pid: self.pid,
                    transaction_id: report.id,
                    command_class: report.command_class,
                    command_id: report.command_id,
                    detail: e.to_string(),
                });
                thread::sleep(poll_interval);
                continue;
            }

            let mut resend = false;
            for poll in 0..Self::SEND_READ_POLLS {
                // Read immediately on the first poll: when the EC already has the
                // reply buffered, return without paying a full poll interval. A
                // not-yet-ready or stale reply still classifies as KeepPolling, so
                // later polls sleep and re-read exactly as before.
                if poll > 0 {
                    thread::sleep(poll_interval);
                }
                let mut buf: [u8; 91] = [0x00; 91];
                let size = match self.device.get_feature_report(&mut buf) {
                    Ok(size) => size,
                    Err(e) => {
                        last_error = Some(TransportError::Read {
                            pid: self.pid,
                            transaction_id: report.id,
                            command_class: report.command_class,
                            command_id: report.command_id,
                            detail: e.to_string(),
                        });
                        continue;
                    }
                };
                if size != 91 {
                    last_error = Some(TransportError::Size {
                        pid: self.pid,
                        command_class: report.command_class,
                        command_id: report.command_id,
                        size,
                    });
                    continue;
                }
                let response = match bincode::deserialize::<RazerPacket>(&buf) {
                    Ok(response) => response,
                    Err(e) => {
                        last_error = Some(TransportError::Decode {
                            pid: self.pid,
                            command_class: report.command_class,
                            command_id: report.command_id,
                            detail: e.to_string(),
                        });
                        continue;
                    }
                };
                match classify_response(&report, &response) {
                    ResponseAction::Accept => {
                        // Let the EC finish latching a thermal change before the next
                        // command races it (Synapse's post-write J(200)/J(100)).
                        let settle = thermal_settle_ms(report.command_class, report.command_id);
                        if settle > 0 {
                            thread::sleep(time::Duration::from_millis(settle));
                        }
                        return Ok(response);
                    }
                    ResponseAction::KeepPolling => continue,
                    ResponseAction::Resend => {
                        last_error = Some(TransportError::CommandFailure {
                            pid: self.pid,
                            transaction_id: report.id,
                            command_class: report.command_class,
                            command_id: report.command_id,
                            status: response.status,
                        });
                        resend = true;
                        break;
                    }
                    ResponseAction::Unsupported => {
                        return Err(TransportError::Unsupported {
                            pid: self.pid,
                            command_class: report.command_class,
                            command_id: report.command_id,
                        });
                    }
                }
            }

            if !resend {
                // Polls exhausted with the EC still busy: hammering it further rarely
                // helps once it has gone quiet, so stop instead of resending.
                break;
            }
        }

        Err(last_error.unwrap_or(TransportError::ExhaustedPolls {
            pid: self.pid,
            command_class: report.command_class,
            command_id: report.command_id,
            attempts: Self::SEND_WRITE_ATTEMPTS,
        }))
    }

}

/// How `send_report` should react to a feature-report reply, mirroring Synapse's
/// `getCommandSendStatus`.
#[derive(PartialEq, Debug)]
enum ResponseAction {
    /// Reply matches the request and reports success: hand it back.
    Accept,
    /// EC is still processing (BUSY/NEW/TIMEOUT) or the buffer still holds a
    /// previous command's reply: read again without resending.
    KeepPolling,
    /// EC reported an explicit failure: write the command again.
    Resend,
    /// EC does not support the command: give up, resending will not help.
    Unsupported,
}

/// Classify a feature-report reply against the request that was just written.
/// Pure decision logic, separated from the HID I/O so it can be unit-tested.
fn classify_response(request: &RazerPacket, response: &RazerPacket) -> ResponseAction {
    // Battery-health-optimizer replies come back with command id 0x92 whether the
    // request was the get (0x92) or the set (0x12); accept those for BHO requests
    // only, and still require a matching transaction id and command class so a
    // stale BHO reply is never taken as another command's response.
    if response.command_id == 0x92 && (request.command_id == 0x92 || request.command_id == 0x12) {
        if request.id == response.id && request.command_class == response.command_class {
            return ResponseAction::Accept;
        }
        return ResponseAction::KeepPolling;
    }
    if request.id != response.id
        || response.command_class != request.command_class
        || response.command_id != request.command_id
        || response.remaining_packets != request.remaining_packets
    {
        return ResponseAction::KeepPolling;
    }
    match response.status {
        RazerPacket::RAZER_CMD_SUCCESSFUL => ResponseAction::Accept,
        RazerPacket::RAZER_CMD_BUSY
        | RazerPacket::RAZER_CMD_NEW
        | RazerPacket::RAZER_CMD_TIMEOUT => ResponseAction::KeepPolling,
        RazerPacket::RAZER_CMD_NOT_SUPPORTED => ResponseAction::Unsupported,
        RazerPacket::RAZER_CMD_FAILURE => ResponseAction::Resend,
        // Any out-of-spec status: resend defensively rather than trust the reply.
        _ => ResponseAction::Resend,
    }
}

/// Settle delay Synapse waits after a thermal write before the next command, so
/// the EC finishes latching one change first: 200ms after Set Thermal Fan Mode
/// (0x0d/0x02), 100ms after Set Thermal Fan Speed (0x0d/0x01) and the boost write
/// (0x0d/0x07). Reads and non-thermal commands do not settle.
fn thermal_settle_ms(command_class: u8, command_id: u8) -> u64 {
    match (command_class, command_id) {
        (0x0d, 0x02) => 200,
        (0x0d, 0x01) | (0x0d, 0x07) => 100,
        _ => 0,
    }
}

// top bit flags whether battery health optimization is on or off
// bottom bits are the actual threshold that it is set to
#[allow(dead_code)]
fn byte_to_bho(u: u8) -> (bool, u8) {
    return (u & (1 << 7) != 0, (u & 0b0111_1111));
}

fn bho_to_byte(is_on: bool, threshold: u8) -> u8 {
    if is_on {
        return threshold | 0b1000_0000;
    }
    return threshold;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reply(command_class: u8, command_id: u8, status: u8) -> RazerPacket {
        let mut packet = RazerPacket::new(command_class, command_id, 0x00);
        packet.status = status;
        packet
    }

    #[test]
    fn serializes_synapse_fan_mode_request() {
        let mut request: RazerPacket = RazerPacket::new(0x0d, 0x02, 0x04);
        request.id = 7;
        request.args[..4].copy_from_slice(&[1, 1, 0, 0]);
        let encoded: [u8; 91] = request.serialize().unwrap();
        let mut expected: [u8; 91] = [0; 91];
        expected[2] = 7;
        expected[6] = 4;
        expected[7] = 0x0d;
        expected[8] = 0x02;
        expected[9..13].copy_from_slice(&[1, 1, 0, 0]);
        expected[89] = 0x0b;
        assert_eq!(encoded, expected);
    }

    #[test]
    fn keeps_polling_on_wrong_transaction_id() {
        let request = RazerPacket::new(0x0d, 0x02, 0x04);
        let mut response = reply(0x0d, 0x02, RazerPacket::RAZER_CMD_SUCCESSFUL);
        response.id = request.id.wrapping_add(1);
        assert_eq!(
            classify_response(&request, &response),
            ResponseAction::KeepPolling
        );
    }

    #[test]
    fn accepts_matching_success() {
        let request = RazerPacket::new(0x0d, 0x02, 0x04);
        let response = reply(0x0d, 0x02, RazerPacket::RAZER_CMD_SUCCESSFUL);
        assert_eq!(classify_response(&request, &response), ResponseAction::Accept);
    }

    #[test]
    fn keeps_polling_while_busy() {
        let request = RazerPacket::new(0x0d, 0x02, 0x04);
        for status in [
            RazerPacket::RAZER_CMD_BUSY,
            RazerPacket::RAZER_CMD_NEW,
            RazerPacket::RAZER_CMD_TIMEOUT,
        ] {
            let response = reply(0x0d, 0x02, status);
            assert_eq!(
                classify_response(&request, &response),
                ResponseAction::KeepPolling
            );
        }
    }

    #[test]
    fn keeps_polling_on_stale_mismatched_reply() {
        // A leftover reply to a different command must not be accepted as ours.
        let request = RazerPacket::new(0x0d, 0x02, 0x04);
        let response = reply(0x0d, 0x01, RazerPacket::RAZER_CMD_SUCCESSFUL);
        assert_eq!(
            classify_response(&request, &response),
            ResponseAction::KeepPolling
        );
    }

    #[test]
    fn resends_on_failure() {
        let request = RazerPacket::new(0x0d, 0x02, 0x04);
        let response = reply(0x0d, 0x02, RazerPacket::RAZER_CMD_FAILURE);
        assert_eq!(classify_response(&request, &response), ResponseAction::Resend);
    }

    #[test]
    fn unsupported_is_terminal() {
        let request = RazerPacket::new(0x0d, 0x02, 0x04);
        let response = reply(0x0d, 0x02, RazerPacket::RAZER_CMD_NOT_SUPPORTED);
        assert_eq!(
            classify_response(&request, &response),
            ResponseAction::Unsupported
        );
    }

    #[test]
    fn accepts_bho_reply_for_bho_request() {
        let request = RazerPacket::new(0x07, 0x92, 0x01);
        let response = reply(0x07, 0x92, RazerPacket::RAZER_CMD_SUCCESSFUL);
        assert_eq!(classify_response(&request, &response), ResponseAction::Accept);
    }

    #[test]
    fn ignores_stray_bho_reply_for_other_request() {
        let request = RazerPacket::new(0x0d, 0x02, 0x04);
        let mut response = reply(0x0d, 0x02, RazerPacket::RAZER_CMD_SUCCESSFUL);
        response.command_id = 0x92;
        assert_eq!(
            classify_response(&request, &response),
            ResponseAction::KeepPolling
        );
    }

    #[test]
    fn fan_mode_settles_longest() {
        assert_eq!(thermal_settle_ms(0x0d, 0x02), 200);
        assert_eq!(thermal_settle_ms(0x0d, 0x01), 100);
        assert_eq!(thermal_settle_ms(0x0d, 0x07), 100);
        assert_eq!(thermal_settle_ms(0x0d, 0x82), 0);
        assert_eq!(thermal_settle_ms(0x03, 0x00), 0);
    }
}
