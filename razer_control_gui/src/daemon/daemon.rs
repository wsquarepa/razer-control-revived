use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::sync::Mutex;
use std::thread::{self, JoinHandle};
use std::time;

use log::*;
use lazy_static::lazy_static;
use signal_hook::iterator::Signals;
use signal_hook::consts::{SIGINT, SIGTERM};
use dbus::blocking::Connection;
use dbus::{Message, arg};

#[path = "../comms.rs"]
mod comms;
mod config;
mod kbd;
mod device;
mod thermal;
mod gpu;
mod battery;
mod dbus_mutter_displayconfig;
mod dbus_mutter_idlemonitor;
mod screensaver;
mod login1;

use crate::kbd::Effect;

// The dGPU's power zone (custom-mode GPU boost / TGP) only latches while the
// dGPU is runtime-active. At boot — and any time no GPU client is running — the
// dGPU is runtime-suspended, so the profile applied at startup does not stick
// for the GPU; a game then wakes the dGPU at the balanced TGP. Re-latching GPU
// boost each time the dGPU resumes makes custom-mode GPU boost take effect.
const DGPU_RESUME_POLL_SECS: u64 = 2;

// After a fixed fan speed is applied on the Blade 16 2025, let the fans spin up
// before the first tachometer check, then verify every couple of seconds. A
// fresh fixed target restarts the settling window.
const THERMAL_MONITOR_SETTLE_SECS: u64 = 5;
const THERMAL_MONITOR_POLL_SECS: u64 = 2;

lazy_static! {
    static ref EFFECT_MANAGER: Mutex<kbd::EffectManager> = Mutex::new(kbd::EffectManager::new());
    // static ref CONFIG: Mutex<config::Configuration> = {
        // match config::Configuration::read_from_config() {
            // Ok(c) => Mutex::new(c),
            // Err(_) => Mutex::new(config::Configuration::new()),
        // }
    // };
    static ref DEV_MANAGER: Mutex<device::DeviceManager> = {
        match device::DeviceManager::read_laptops_file() {
            Ok(c) => Mutex::new(c),
            Err(_) => Mutex::new(device::DeviceManager::new()),
        }
    };
}

// Main function for daemon
fn main() {
    setup_panic_hook();
    init_logging();

    let flags: Vec<String> = std::env::args().skip(1).collect();
    match thermal::parse_execution(&flags) {
        Ok(thermal::DaemonExecution::Service) => run_service(),
        Ok(thermal::DaemonExecution::PreflightOnly) => run_preflight_only(),
        Ok(thermal::DaemonExecution::CollectThermalLimits) => run_limit_collection(),
        Err(error) => {
            eprintln!("invalid arguments: {error}");
            std::process::exit(2);
        }
    }
}

/// Discover the supported device and run the one-time config migration, exiting
/// nonzero on any failure. Shared by every execution mode.
fn discover_and_migrate_or_exit() {
    if let Ok(mut d) = DEV_MANAGER.lock() {
        d.discover_devices();
        if let Some(laptop) = d.get_device() {
            println!("supported device: {:?}", laptop.get_name());
        } else {
            println!("no supported device found");
            std::process::exit(1);
        }
        // Migrate the saved configuration for the discovered PID before any
        // hardware state is applied from it.
        if let Err(error) = d.migrate_configuration() {
            eprintln!("configuration migration failed: {error}");
            std::process::exit(1);
        }
    } else {
        println!("error loading supported devices");
        std::process::exit(1);
    }
}

/// `--preflight-only`: run the getter-only sweep and exit with its result. A
/// failed preflight exits nonzero without touching thermal or power state.
fn run_preflight_only() {
    discover_and_migrate_or_exit();
    let state = match DEV_MANAGER.lock() {
        Ok(mut d) => d.preflight(),
        Err(_) => {
            eprintln!("device manager unavailable");
            std::process::exit(1);
        }
    };
    // preflight() only ever returns Ready or Disabled; any non-Ready posture is
    // treated as a failed sweep for exit-status purposes.
    if state == thermal::ThermalSafetyState::Ready {
        println!("thermal preflight passed");
        std::process::exit(0);
    }
    eprintln!("thermal preflight failed");
    std::process::exit(1);
}

/// `--collect-thermal-limits`: run the supervised mode-global limit sweep and
/// print the readings, exiting nonzero if collection or restoration failed.
fn run_limit_collection() {
    discover_and_migrate_or_exit();
    let result = match DEV_MANAGER.lock() {
        Ok(mut d) => d.collect_thermal_limits(),
        Err(_) => {
            eprintln!("device manager unavailable");
            std::process::exit(1);
        }
    };
    match result {
        Ok(limits) => {
            for entry in &limits {
                println!("{entry:?}");
            }
            std::process::exit(0);
        }
        Err(error) => {
            eprintln!("thermal limit collection failed: {error:?}");
            std::process::exit(1);
        }
    }
}

/// Normal daemon service: discover, migrate, run preflight, then apply saved
/// state only if preflight passed, and enter the socket/event loop regardless.
fn run_service() {
    discover_and_migrate_or_exit();

    // Automatic saved-state application is gated on a passing preflight. On a
    // failed sweep the daemon stays up in the Disabled state and sends no
    // thermal or power writes; the socket loop below still runs.
    let safety: thermal::ThermalSafetyState = match DEV_MANAGER.lock() {
        Ok(mut d) => d.preflight(),
        Err(_) => thermal::ThermalSafetyState::Disabled,
    };
    if safety == thermal::ThermalSafetyState::Disabled {
        eprintln!(
            "thermal preflight failed: automatic thermal/power application disabled; socket stays up"
        );
    }

    if let Ok(mut d) = DEV_MANAGER.lock() {
        let dbus_system = match Connection::new_system() {
            Ok(conn) => conn,
            Err(e) => {
                eprintln!("Failed to connect to D-Bus system bus: {}", e);
                std::process::exit(1);
            }
        };
        let proxy_ac = dbus_system.with_proxy("org.freedesktop.UPower", "/org/freedesktop/UPower/devices/line_power_AC0", time::Duration::from_millis(5000));
        use battery::OrgFreedesktopUPowerDevice;
        if let Ok(online) = proxy_ac.online() {
            println!("Online AC0: {:?}", online);
            // Ready applies the saved profile; any other posture (Disabled after a
            // failed sweep) only tracks the AC index and withholds power writes.
            if safety == thermal::ThermalSafetyState::Ready {
                d.set_ac_state(online);
            } else {
                d.set_ac_index(online);
            }
            d.restore_standard_effect();
            // Disabled means no saved thermal/power state may reach the EC; BHO restore is a power write.
            if safety == thermal::ThermalSafetyState::Ready {
                d.restore_bho();
            } else {
                eprintln!("thermal preflight failed: skipping BHO restore");
            }
            // Only load per-key RGB effects if device supports custom frames.
            // Sending custom frame HID reports to unsupported devices can
            // overwhelm the USB/HID subsystem and trigger kernel panics.
            if d.device_has_feature("per_key_rgb") {
                if let Ok(json) = config::Configuration::read_effects_file() {
                    if let Ok(mut mgr) = EFFECT_MANAGER.lock() {
                        mgr.load_from_save(json);
                    }
                } else {
                    println!("No effects save, creating a new one");
                    if let Ok(mut mgr) = EFFECT_MANAGER.lock() {
                        mgr.push_effect(
                            kbd::effects::Static::new(vec![0, 255, 0]),
                            [true; 90]
                        );
                    }
                }
            } else {
                println!("Device does not support per-key RGB, skipping keyboard effects");
            }
        } else {
            println!("error getting current power state");
            std::process::exit(1);
        }
    }

    // Only run the keyboard animation loop if the device supports per-key RGB.
    // Sending custom frame reports to unsupported devices causes kernel panics.
    if let Ok(d) = DEV_MANAGER.lock() {
        if d.device_has_feature("per_key_rgb") {
            start_keyboard_animator_task();
        } else {
            println!("Keyboard animation disabled (device has no per_key_rgb)");
        }
    }
    start_screensaver_monitor_task();
    start_battery_monitor_task();
    start_dgpu_resume_watch_task();
    // The tachometer monitor only applies to the Blade 16 2025; other models have
    // no fixed-fan verification to run.
    let run_thermal_monitor: bool = match DEV_MANAGER.lock() {
        Ok(mut d) => d.is_blade_16_2025(),
        Err(_) => false,
    };
    if run_thermal_monitor {
        start_thermal_monitor_task();
    }
    let clean_thread = start_shutdown_task();

    if let Some(listener) = comms::create() {
        for stream in listener.incoming() {
            match stream {
                Ok(stream) => handle_data(stream),
                Err(_) => {} // Don't care about this
            }
        }
    } else {
        eprintln!("Could not create Unix socket!");
        std::process::exit(1);
    }
    clean_thread.join().unwrap();
}

/// Installs a custom panic hook to perform cleanup when the daemon crashes
fn setup_panic_hook() {
    let default_panic_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        error!("Something went wrong! Removing the socket path");
        let _ = std::fs::remove_file(comms::socket_path());
        default_panic_hook(info);
    }));
}

fn init_logging() {
    let mut builder = env_logger::Builder::from_default_env();
    builder.target(env_logger::Target::Stderr);
    builder.filter_level(log::LevelFilter::Info);
    builder.format_timestamp_millis();
    builder.parse_env("RAZER_LAPTOP_CONTROL_LOG");
    builder.init();
}

/// Handles keyboard animations
pub fn start_keyboard_animator_task() -> JoinHandle<()> {
    // Start the keyboard animator thread,
    thread::spawn(|| {
        loop {
            if let Ok(mut dev) = DEV_MANAGER.lock() {
                if let Some(laptop) = dev.get_device() {
                    if let Ok(mut mgr) = EFFECT_MANAGER.lock() {
                        mgr.update(laptop);
                    }
                }
            }
            thread::sleep(std::time::Duration::from_millis(kbd::ANIMATION_SLEEP_MS));
        }
    })
}

fn start_screensaver_monitor_task() -> JoinHandle<()> {
    thread::spawn(move || {
        let dbus_session = match Connection::new_session() {
            Ok(conn) => conn,
            Err(e) => {
                eprintln!("Screensaver monitor: D-Bus session unavailable ({}), skipping", e);
                return;
            }
        };
        let  proxy = dbus_session.with_proxy("org.gnome.Mutter.DisplayConfig", "/org/gnome/Mutter/DisplayConfig", time::Duration::from_millis(5000));
        let _id = proxy.match_signal(|h: dbus_mutter_displayconfig::OrgFreedesktopDBusPropertiesPropertiesChanged, _: &Connection, _: &Message| {
            let online: Option<&i32> = arg::prop_cast(&h.changed_properties, "PowerSaveMode");
            if let Some(online) = online {
                if *online == 3 {
                    if let Ok(mut d) = DEV_MANAGER.lock() {
                        d.light_off();
                    }
                }
                else if *online == 0 {
                    if let Ok(mut d) = DEV_MANAGER.lock() {
                        d.restore_light();
                    }
                }

            } 
            true
        });
        let  proxy_idle = dbus_session.with_proxy("org.gnome.Mutter.IdleMonitor", "/org/gnome/Mutter/IdleMonitor/Core", time::Duration::from_millis(5000));
        let _id = proxy_idle.match_signal(|h: dbus_mutter_idlemonitor::OrgGnomeMutterIdleMonitorWatchFired, _: &Connection, _: &Message| {
            if let Ok(mut d) = DEV_MANAGER.lock() {
                if d.idle_id == h.id {
                    println!("idle trigger {:?}", h.id);
                    d.light_off();
                } else if d.active_id == h.id {
                    println!("active trigger {:?}", h.id);
                    d.restore_light();
                }
            }
            true
        });
        let proxy = dbus_session.with_proxy("org.freedesktop.ScreenSaver", "/org/freedesktop/ScreenSaver", time::Duration::from_millis(5000));
        let _id = proxy.match_signal(|h: screensaver::OrgFreedesktopScreenSaverActiveChanged, _: &Connection, _: &Message| {
            println!("ActiveChanged {:?}", h.arg0);
            if let Ok(mut d) = DEV_MANAGER.lock() {
                if h.arg0 {
                    d.light_off();
                } else {
                    d.restore_light();
                }
            }
            true
        });

        loop { 
            if let Ok(res) = dbus_session.process(time::Duration::from_millis(1000)) {
                if res {
                    if let Ok(mut d) = DEV_MANAGER.lock() {
                        d.add_active_watch(&proxy_idle);
                    }
                }
                if let Ok(mut d) = DEV_MANAGER.lock() {
                    d.add_idle_watch(&proxy_idle);
                }
            }
        }

    })
}

fn start_battery_monitor_task() -> JoinHandle<()> {
    thread::spawn(move || {
        let dbus_system = match Connection::new_system() {
            Ok(conn) => conn,
            Err(e) => {
                eprintln!("Battery monitor: D-Bus system unavailable ({}), skipping", e);
                return;
            }
        };
        let proxy_ac = dbus_system.with_proxy("org.freedesktop.UPower", "/org/freedesktop/UPower/devices/line_power_AC0", time::Duration::from_millis(5000));
        let _id = proxy_ac.match_signal(|h: battery::OrgFreedesktopDBusPropertiesPropertiesChanged, _: &Connection, _: &Message| {
            let online: Option<&bool> = arg::prop_cast(&h.changed_properties, "Online");
            if let Some(online) = online {
                println!("Online AC0: {:?}", online);
                if let Ok(mut d) = DEV_MANAGER.lock() {
                    d.set_ac_state(*online);
                }
            }
            true
        });

        let proxy_battery = dbus_system.with_proxy("org.freedesktop.UPower", "/org/freedesktop/UPower/devices/battery_BAT0", time::Duration::from_millis(5000));
        // use battery::OrgFreedesktopUPowerDevice;
        // if let Ok(perc) = proxy_battery.percentage() {
            // println!("battery percentage: {:.1}", perc);
        // }
        let _id = proxy_battery.match_signal(|h: battery::OrgFreedesktopDBusPropertiesPropertiesChanged, _: &Connection, _: &Message| {
            let perc: Option<&f64> = arg::prop_cast(&h.changed_properties, "Percentage");
            if let Some(perc) = perc {
                println!("battery percentage: {:.1}", perc);
            }
            true
        });

        let proxy_login = dbus_system.with_proxy("org.freedesktop.login1", "/org/freedesktop/login1", time::Duration::from_millis(5000));
        let _id = proxy_login.match_signal(|h: login1::OrgFreedesktopLogin1ManagerPrepareForSleep, _: &Connection, _: &Message| {
            println!("PrepareForSleep {:?}", h.start);
            if h.start {
                if let Ok(mut d) = DEV_MANAGER.lock() {
                    d.set_ac_state_get();
                    d.light_off();
                }
                return true;
            }
            // The system just woke up. Run the burst-free post-wake sequence: one
            // immediate verified apply, then one getter-only re-verification
            // WAKE_DELAYED_VERIFY_SECS later that authorizes at most one repair.
            // The immediate apply is universal (non-02C6 keeps its single re-apply);
            // the delayed getter-only readback is 02C6-only (see wake_delayed_reverify).
            let is_blade = match DEV_MANAGER.lock() {
                Ok(mut d) => {
                    d.restore_light();
                    d.is_blade_16_2025()
                }
                Err(_) => return true,
            };
            for step in thermal::wake_sequence() {
                if !is_blade && step.kind == thermal::WakeStepKind::DelayedReadback {
                    continue;
                }
                thread::spawn(move || {
                    if step.delay_secs > 0 {
                        thread::sleep(time::Duration::from_secs(step.delay_secs));
                    }
                    if let Ok(mut d) = DEV_MANAGER.lock() {
                        match step.kind {
                            thermal::WakeStepKind::ApplyAndVerify => {
                                d.set_ac_state_get();
                                d.log_hw_power_state("post-wake");
                            }
                            thermal::WakeStepKind::DelayedReadback => d.wake_delayed_reverify(),
                        }
                    }
                });
            }
            true
        });
        // use login1::OrgFreedesktopLogin1ManagerPrepareForSleep;
        loop {
            if let Err(e) = dbus_system.process(time::Duration::from_millis(1000)) {
                eprintln!("Battery monitor D-Bus error: {}", e);
            }
        }
    })
}

/// Re-latches GPU boost once whenever the dGPU transitions from runtime-suspended
/// to active, so custom-mode GPU boost latches when a GPU client (e.g. a game)
/// powers the dGPU up. The confirmed-write transport (see send_report) means one
/// re-latch per transition is enough; no settling burst is needed. See
/// `relatch_dgpu_boost` for the 02C6 GPU-only path.
fn start_dgpu_resume_watch_task() -> JoinHandle<()> {
    thread::spawn(|| {
        let mut dgpu_path = gpu::find_dgpu_sysfs_path();
        let mut was_active = false;
        loop {
            thread::sleep(time::Duration::from_secs(DGPU_RESUME_POLL_SECS));
            if dgpu_path.is_none() {
                dgpu_path = gpu::find_dgpu_sysfs_path();
            }
            let active = dgpu_path
                .as_ref()
                .and_then(|p| std::fs::read_to_string(p.join("power/runtime_status")).ok())
                .map_or(false, |s| s.trim() == "active");
            if active && !was_active {
                if let Ok(mut d) = DEV_MANAGER.lock() {
                    d.relatch_dgpu_boost();
                }
            }
            was_active = active;
        }
    })
}

/// Verifies applied fixed fan speeds against the Blade 16 2025 tachometer and
/// fails both fans back to firmware-automatic control on repeated failure.
///
/// The lock is never held across a sleep: each tick reads whether a fixed target
/// is armed, and only re-locks to run a verification cycle. A newly-applied fixed
/// target restarts a settling window (`THERMAL_MONITOR_SETTLE_SECS`) before its
/// first check; the pure decision lives in `thermal::advance_safety`.
fn start_thermal_monitor_task() -> JoinHandle<()> {
    thread::spawn(|| {
        let settle = time::Duration::from_secs(THERMAL_MONITOR_SETTLE_SECS);
        let poll = time::Duration::from_secs(THERMAL_MONITOR_POLL_SECS);
        let mut watching: Option<(thermal::FanRpm, time::Instant)> = None;
        loop {
            thread::sleep(poll);
            let target = match DEV_MANAGER.lock() {
                Ok(mut d) => d.thermal_manual_target(),
                Err(_) => continue,
            };
            let target = match target {
                Some(target) => target,
                None => {
                    watching = None;
                    continue;
                }
            };
            match watching {
                Some((watched, settle_deadline)) if watched == target => {
                    if time::Instant::now() < settle_deadline {
                        continue; // still inside this target's settling window
                    }
                }
                _ => {
                    // Newly-applied (or changed) fixed target: start its window.
                    watching = Some((target, time::Instant::now() + settle));
                    continue;
                }
            }
            if let Ok(mut d) = DEV_MANAGER.lock() {
                d.run_thermal_verification_cycle();
            }
        }
    })
}

/// Monitors signals and stops the daemon when receiving one
pub fn start_shutdown_task() -> JoinHandle<()> {
    thread::spawn(|| {
        let mut signals = Signals::new([SIGINT, SIGTERM]).unwrap();
        let _ = signals.forever().next();
        
        // If we reach this point, we have a signal and it is time to exit
        println!("Received signal, cleaning up");
        // Return the fans to firmware-automatic control before exit if a fixed
        // speed is armed, so no manual fan target is left latched with no monitor.
        if let Ok(mut d) = DEV_MANAGER.lock() {
            d.restore_automatic_on_exit();
        }
        let json = match EFFECT_MANAGER.lock() {
            Ok(mut mgr) => mgr.save(),
            Err(e) => {
                eprintln!("Failed to lock effect manager for save: {}", e);
                serde_json::json!({"effects": []})
            }
        };
        if let Err(error) = config::Configuration::write_effects_save(json) {
            error!("Error writing config {}", error);
        }
        let _ = std::fs::remove_file(comms::socket_path());
        std::process::exit(0);
    })
}

fn handle_data(mut stream: UnixStream) {
    let mut buffer = Vec::new();
    if let Err(error) = stream.read_to_end(&mut buffer) {
        eprintln!("Failed to read request from socket: {error}");
        return;
    }

    if buffer.is_empty() {
        eprintln!("Received empty request payload");
        return;
    }

    if let Some(cmd) = comms::read_from_socket_req(&buffer) {
        if let Some(s) = process_client_request(cmd) {
            if let Ok(x) = bincode::serialize(&s) {
                let result = stream.write_all(&x);

                if let Err(error) = result {
                    println!("Client disconnected with error: {error}");
                }
            }
        } else {
            eprintln!("No response for client request — closing connection");
        }
    } else {
        eprintln!("Failed to deserialize client request");
    }
}

pub fn process_client_request(cmd: comms::DaemonCommand) -> Option<comms::DaemonResponse> {
    // GPU commands don't need DEV_MANAGER, handle them first
    match &cmd {
        comms::DaemonCommand::GetGpuStatus => {
            let gpus = gpu::discover_gpus();
            let dgpu_rpm = gpu::get_dgpu_runtime_pm();
            let ec_available = gpu::envycontrol_available();
            let ec_mode = if ec_available {
                gpu::get_envycontrol_mode()
            } else {
                "unknown".to_string()
            };
            return Some(comms::DaemonResponse::GetGpuStatus {
                gpus,
                dgpu_runtime_pm: dgpu_rpm,
                envycontrol_mode: ec_mode,
                envycontrol_available: ec_available,
            });
        }
        comms::DaemonCommand::SetDgpuRuntimePM { enabled } => {
            return Some(comms::DaemonResponse::SetDgpuRuntimePM {
                result: gpu::set_dgpu_runtime_pm(*enabled),
            });
        }
        comms::DaemonCommand::SetGpuMode { mode } => {
            let (ok, msg) = gpu::set_envycontrol_mode(mode);
            return Some(comms::DaemonResponse::SetGpuMode { result: ok, message: msg });
        }
        _ => {}
    }

    if let Ok(mut d) = DEV_MANAGER.lock() {
        return match cmd {
            comms::DaemonCommand::SetPowerMode { ac, pwr, cpu, gpu } if ac < 2 => {
                Some(comms::DaemonResponse::SetPowerMode { result: d.set_power_mode(ac, pwr, cpu, gpu) })
            },
            comms::DaemonCommand::SetFanSpeed { ac, rpm } if ac < 2 => {
                Some(comms::DaemonResponse::SetFanSpeed { result: d.set_fan_rpm(ac, rpm) })
            },
            comms::DaemonCommand::SetLogoLedState{ ac, logo_state } if ac < 2 => {
                Some(comms::DaemonResponse::SetLogoLedState { result: d.set_logo_led_state(ac, logo_state) })
            },
            comms::DaemonCommand::SetBrightness { ac, val } if ac < 2 => {
                Some(comms::DaemonResponse::SetBrightness {result: d.set_brightness(ac, val) })
            }
            comms::DaemonCommand::SetIdle { ac, val } if ac < 2 => {
                Some(comms::DaemonResponse::SetIdle { result: d.change_idle(ac, val) })
            }
            comms::DaemonCommand::SetSync { sync } => {
                Some(comms::DaemonResponse::SetSync { result: d.set_sync(sync) })
            }
            comms::DaemonCommand::GetBrightness{ac} if ac < 2 =>  {
                Some(comms::DaemonResponse::GetBrightness { result: d.get_brightness(ac)})
            },
            comms::DaemonCommand::GetLogoLedState{ac} if ac < 2 => Some(comms::DaemonResponse::GetLogoLedState {logo_state: d.get_logo_led_state(ac) }),
            comms::DaemonCommand::GetKeyboardRGB { layer } => {
                if let Ok(mut mgr) = EFFECT_MANAGER.lock() {
                    Some(comms::DaemonResponse::GetKeyboardRGB {
                        layer,
                        rgbdata: mgr.get_map(layer),
                    })
                } else {
                    None
                }
            }
            comms::DaemonCommand::GetSync() => Some(comms::DaemonResponse::GetSync { sync: d.get_sync() }),
            comms::DaemonCommand::GetFanSpeed{ac} if ac < 2 => Some(comms::DaemonResponse::GetFanSpeed { rpm: d.get_fan_rpm(ac)}),
            comms::DaemonCommand::GetPwrLevel{ac} if ac < 2 => Some(comms::DaemonResponse::GetPwrLevel { pwr: d.get_power_mode(ac) }),
            comms::DaemonCommand::GetCPUBoost{ac} if ac < 2 => Some(comms::DaemonResponse::GetCPUBoost { cpu: d.get_cpu_boost(ac) }),
            comms::DaemonCommand::GetGPUBoost{ac} if ac < 2 => Some(comms::DaemonResponse::GetGPUBoost { gpu: d.get_gpu_boost(ac) }),
            comms::DaemonCommand::SetEffect{ name, params } => {
                let mut res = false;
                let gui_idx = match name.as_str() {
                    "static" => 0u8,
                    "static_gradient" => 1,
                    "wave_gradient" => 2,
                    "breathing_single" => 3,
                    _ => 255,
                };
                // Persist GUI effect selection to config
                if gui_idx < 255 {
                    d.save_gui_effect(gui_idx, params.clone());
                }

                if d.device_has_feature("per_key_rgb") {
                    // Per-key RGB: push to EFFECT_MANAGER for animation loop
                    if let Ok(mut k) = EFFECT_MANAGER.lock() {
                        res = true;
                        let effect = match name.as_str() {
                            "static" => Some(kbd::effects::Static::new(params)),
                            "static_gradient" => Some(kbd::effects::StaticGradient::new(params)),
                            "wave_gradient" => Some(kbd::effects::WaveGradient::new(params)),
                            "breathing_single" => Some(kbd::effects::BreathSingle::new(params)),
                            _ => None
                        };

                        if let Some(laptop) = d.get_device() {
                            if let Some(e) = effect {
                                k.pop_effect(laptop); // Remove old layer
                                k.push_effect(
                                    e,
                                    [true; 90]
                                    );
                            } else {
                                res = false
                            }
                        } else {
                            res = false;
                        }
                    }
                } else {
                    // No per-key RGB: map GUI effects to standard hardware effects
                    let (effect_id, hw_params) = match name.as_str() {
                        "static" => (device::RazerLaptop::STATIC, params),
                        "breathing_single" => (device::RazerLaptop::BREATHING, params),
                        "wave_gradient" => (device::RazerLaptop::WAVE, params),
                        "static_gradient" => (device::RazerLaptop::STATIC, params),
                        _ => (device::RazerLaptop::SPECTRUM, vec![]),
                    };
                    res = d.set_standard_effect(effect_id, hw_params);
                }
                Some(comms::DaemonResponse::SetEffect{result: res})
            }

            comms::DaemonCommand::SetStandardEffect{ name, params } => {
                // TODO save standart effect may be struct ?
                let mut res = false;
                if let Some(laptop) = d.get_device() {
                    if let Ok(mut k) = EFFECT_MANAGER.lock() {
                        k.pop_effect(laptop); // Remove old layer
                        let _res = match name.as_str() {
                            "off" => d.set_standard_effect(device::RazerLaptop::OFF, params),
                            "wave" => d.set_standard_effect(device::RazerLaptop::WAVE, params),
                            "reactive" => d.set_standard_effect(device::RazerLaptop::REACTIVE, params),
                            "breathing" => d.set_standard_effect(device::RazerLaptop::BREATHING, params),
                            "spectrum" => d.set_standard_effect(device::RazerLaptop::SPECTRUM, params),
                            "static" => d.set_standard_effect(device::RazerLaptop::STATIC, params),
                            "starlight" => d.set_standard_effect(device::RazerLaptop::STARLIGHT, params), 
                            _ => false,
                        };
                        res = _res;
                    }
                } else {
                    res = false;
                }
                Some(comms::DaemonResponse::SetStandardEffect{result: res})
            }
            comms::DaemonCommand::SetBatteryHealthOptimizer { is_on, threshold } => { 
                return Some(comms::DaemonResponse::SetBatteryHealthOptimizer { result: d.set_bho_handler(is_on, threshold)});
            }
            comms::DaemonCommand::GetBatteryHealthOptimizer() => {
                return d.get_bho_handler().map(|result| 
                    comms::DaemonResponse::GetBatteryHealthOptimizer {
                        is_on: (result.0), 
                        threshold: (result.1) 
                    }
                );
            }
            comms::DaemonCommand::GetActualFanRpm => {
                Some(comms::DaemonResponse::GetActualFanRpm { rpm: d.get_actual_fan_rpm() })
            },
            comms::DaemonCommand::GetDeviceName => {
                let name = match &d.device {
                    Some(device) => device.get_name(),
                    None => "Unknown Device".into()
                };
                return Some(comms::DaemonResponse::GetDeviceName { name });
            }
            comms::DaemonCommand::GetStandardEffect => {
                let (effect, params) = d.get_standard_effect();
                Some(comms::DaemonResponse::GetStandardEffect { effect, params })
            }
            // Reject commands with invalid ac index (>= 2)
            _ => {
                eprintln!("Rejected command with invalid ac index: {:?}", cmd);
                None
            }
        };
    } else {
        return None;
    }
}


