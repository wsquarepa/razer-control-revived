use nvml_wrapper::Nvml;
use nvml_wrapper::enum_wrappers::device::TemperatureSensor;
use std::path::Path;
use std::sync::Mutex;
use std::sync::atomic::AtomicBool;
use std::time::{Duration, Instant};

#[derive(Debug, Clone)]
pub struct GpuTelemetry {
    pub usage_percent: f32,
    pub watts: f32,
    pub temp_c: f32,
    // Shown on the GPU page (Task 9); unused until that page is wired.
    #[allow(dead_code)]
    pub name: String,
}

#[derive(Debug, Clone)]
pub struct BatteryInfo {
    pub percent: u8,
    // Shown on the Battery page (Task 11); unused until that page is wired.
    #[allow(dead_code)]
    pub status: String,
    /// Positive while charging, negative while discharging, None when unknown.
    pub watts: Option<f32>,
}

#[derive(Debug, Clone)]
pub struct Snapshot {
    pub cpu_usage_percent: Option<f32>,
    pub cpu_watts: Option<f32>,
    pub cpu_temp_c: Option<f32>,
    pub gpu: Option<GpuTelemetry>,
    pub gpu_suspended: bool,
    pub battery: Option<BatteryInfo>,
    pub ac_online: Option<bool>,
    pub thermal: Option<razer_core::ThermalStatus>,
}

impl Snapshot {
    pub const EMPTY: Snapshot = Snapshot {
        cpu_usage_percent: None,
        cpu_watts: None,
        cpu_temp_c: None,
        gpu: None,
        gpu_suspended: false,
        battery: None,
        ac_online: None,
        thermal: None,
    };
}

/// The tray tooltip renders from this; the telemetry subscription writes it.
pub static SHARED: Mutex<Snapshot> = Mutex::new(Snapshot::EMPTY);

/// While the window is hidden the sampler skips usage/watts reads;
/// the tray tooltip only needs temps, fan rpm, and battery.
pub static WINDOW_VISIBLE: AtomicBool = AtomicBool::new(true);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CpuStat {
    pub idle: u64,
    pub total: u64,
}

pub fn parse_proc_stat(contents: &str) -> Option<CpuStat> {
    let line = contents.lines().find(|l| l.starts_with("cpu "))?;
    let fields: Vec<u64> = line
        .split_whitespace()
        .skip(1)
        .map_while(|f| f.parse().ok())
        .collect();
    // /proc/stat cpu fields: user nice system idle iowait irq softirq steal guest guest_nice
    if fields.len() < 5 {
        return None;
    }
    let idle = fields[3] + fields[4];
    let total = fields.iter().sum();
    Some(CpuStat { idle, total })
}

pub fn cpu_usage_percent(prev: &CpuStat, next: &CpuStat) -> Option<f32> {
    let total = next.total.checked_sub(prev.total)?;
    if total == 0 {
        return None;
    }
    let idle = next.idle.saturating_sub(prev.idle);
    Some(((total - idle) as f32 / total as f32) * 100.0)
}

pub fn rapl_watts(prev_uj: u64, next_uj: u64, elapsed: Duration) -> Option<f32> {
    if elapsed.is_zero() || next_uj < prev_uj {
        return None;
    }
    let joules = (next_uj - prev_uj) as f64 / 1_000_000.0;
    Some((joules / elapsed.as_secs_f64()) as f32)
}

fn read_trimmed(path: &Path) -> Option<String> {
    std::fs::read_to_string(path)
        .ok()
        .map(|s| s.trim().to_string())
}

pub fn read_battery(power_supply_dir: &Path) -> Option<BatteryInfo> {
    for name in ["BAT0", "BAT1"] {
        let bat = power_supply_dir.join(name);
        let Some(percent) = read_trimmed(&bat.join("capacity")).and_then(|s| s.parse().ok()) else {
            continue;
        };
        let status = read_trimmed(&bat.join("status")).unwrap_or_else(|| "Unknown".to_string());
        // power_now is unsigned microwatts; direction comes from status.
        let magnitude: Option<f32> = read_trimmed(&bat.join("power_now"))
            .and_then(|s| s.parse::<f64>().ok())
            .map(|uw| (uw / 1_000_000.0) as f32);
        let watts = magnitude.map(|w| if status == "Discharging" { -w } else { w });
        return Some(BatteryInfo {
            percent,
            status,
            watts,
        });
    }
    None
}

pub fn read_ac_online(power_supply_dir: &Path) -> Option<bool> {
    for name in ["AC0", "ADP0", "ADP1", "ACAD"] {
        if let Some(content) = read_trimmed(&power_supply_dir.join(name).join("online")) {
            return Some(content == "1");
        }
    }
    None
}

pub fn read_cpu_temp(hwmon_dir: &Path) -> Option<f32> {
    let entries = std::fs::read_dir(hwmon_dir).ok()?;
    for entry in entries.flatten() {
        let Some(name) = read_trimmed(&entry.path().join("name")) else {
            continue;
        };
        if matches!(name.as_str(), "k10temp" | "zenpower" | "coretemp")
            && let Some(temp) =
                read_trimmed(&entry.path().join("temp1_input")).and_then(|s| s.parse::<f64>().ok())
        {
            return Some((temp / 1000.0) as f32);
        }
    }
    None
}

/// The runtime PM status of the NVIDIA dGPU (vendor 0x10de, display class 0x03),
/// or None when no NVIDIA display device exists.
pub fn nvidia_runtime_status(pci_devices_dir: &Path) -> Option<String> {
    let entries = std::fs::read_dir(pci_devices_dir).ok()?;
    for entry in entries.flatten() {
        let vendor = read_trimmed(&entry.path().join("vendor"));
        let class = read_trimmed(&entry.path().join("class"));
        let is_nvidia_display =
            vendor.as_deref() == Some("0x10de") && class.is_some_and(|c| c.starts_with("0x03"));
        if is_nvidia_display {
            return read_trimmed(&entry.path().join("power/runtime_status"));
        }
    }
    None
}

fn find_rapl_energy_file() -> Option<std::path::PathBuf> {
    let base = Path::new("/sys/class/powercap");
    let entries = std::fs::read_dir(base).ok()?;
    for entry in entries.flatten() {
        let name = read_trimmed(&entry.path().join("name"));
        // "package-0" covers both intel-rapl and the AMD rapl driver.
        if name.is_some_and(|n| n.starts_with("package")) {
            return Some(entry.path().join("energy_uj"));
        }
    }
    None
}

pub struct Sampler {
    nvml: Option<Nvml>,
    prev_cpu: Option<CpuStat>,
    prev_rapl: Option<(u64, Instant)>,
    rapl_energy_file: Option<std::path::PathBuf>,
}

impl Sampler {
    pub fn new() -> Sampler {
        let nvml = match Nvml::init() {
            Ok(nvml) => Some(nvml),
            Err(error) => {
                log::warn!("NVML unavailable, GPU telemetry disabled: {error}");
                None
            }
        };
        Sampler {
            nvml,
            prev_cpu: None,
            prev_rapl: None,
            rapl_energy_file: find_rapl_energy_file(),
        }
    }

    fn sample_cpu_usage(&mut self) -> Option<f32> {
        let contents = std::fs::read_to_string("/proc/stat").ok()?;
        let next = parse_proc_stat(&contents)?;
        let usage = self
            .prev_cpu
            .as_ref()
            .and_then(|prev| cpu_usage_percent(prev, &next));
        self.prev_cpu = Some(next);
        usage
    }

    fn sample_cpu_watts(&mut self) -> Option<f32> {
        let file = self.rapl_energy_file.as_ref()?;
        let next_uj: u64 = read_trimmed(file)?.parse().ok()?;
        let now = Instant::now();
        let watts = self
            .prev_rapl
            .and_then(|(prev_uj, at)| rapl_watts(prev_uj, next_uj, now.duration_since(at)));
        self.prev_rapl = Some((next_uj, now));
        watts
    }

    fn sample_gpu(&self) -> (Option<GpuTelemetry>, bool) {
        // Never touch NVML while the dGPU is asleep: the query itself wakes it,
        // which would defeat the dGPU suspend feature on the GPU page.
        let status = nvidia_runtime_status(Path::new("/sys/bus/pci/devices"));
        if status.as_deref() != Some("active") {
            return (None, status.is_some());
        }
        let Some(nvml) = self.nvml.as_ref() else {
            return (None, false);
        };
        let telemetry = (|| -> Result<GpuTelemetry, nvml_wrapper::error::NvmlError> {
            let device = nvml.device_by_index(0)?;
            Ok(GpuTelemetry {
                usage_percent: device.utilization_rates()?.gpu as f32,
                watts: device.power_usage()? as f32 / 1000.0,
                temp_c: device.temperature(TemperatureSensor::Gpu)? as f32,
                name: device.name()?,
            })
        })();
        match telemetry {
            Ok(telemetry) => (Some(telemetry), false),
            Err(error) => {
                log::warn!("NVML read failed: {error}");
                (None, false)
            }
        }
    }

    pub fn sample(&mut self, include_load: bool) -> Snapshot {
        let power_supply = Path::new("/sys/class/power_supply");
        let (cpu_usage_percent, cpu_watts) = if include_load {
            (self.sample_cpu_usage(), self.sample_cpu_watts())
        } else {
            (None, None)
        };
        let (gpu, gpu_suspended) = self.sample_gpu();
        Snapshot {
            cpu_usage_percent,
            cpu_watts,
            cpu_temp_c: read_cpu_temp(Path::new("/sys/class/hwmon")),
            gpu,
            gpu_suspended,
            battery: read_battery(power_supply),
            ac_online: read_ac_online(power_supply),
            thermal: None, // the subscription (Task 6) fills this from the daemon
        }
    }
}

use iced::Subscription;
use iced::futures::{SinkExt, Stream};

/// A failed daemon read, or a status carrying a thermal error, keeps the last
/// good reading on screen: EC tachometer reads fail sporadically and flickering
/// the gauges to their unavailable state every few seconds is worse than
/// showing a reading that is a tick or two stale. Only error-free statuses are
/// ever stored, so consumers never see `error: Some`.
fn latest_thermal(
    last: Option<razer_core::ThermalStatus>,
    fresh: Result<razer_core::ThermalStatus, crate::daemon::DaemonError>,
) -> Option<razer_core::ThermalStatus> {
    match fresh {
        Ok(status) if status.error.is_none() => Some(status),
        Ok(status) => {
            log::warn!(
                "thermal read reported an error, keeping last good reading: {:?}",
                status.error
            );
            last
        }
        Err(error) => {
            log::warn!("thermal telemetry unavailable, keeping last good reading: {error}");
            last
        }
    }
}

fn snapshot_stream() -> impl Stream<Item = Snapshot> {
    iced::stream::channel(1, async |mut out| {
        let mut sampler = Sampler::new();
        let mut last_thermal: Option<razer_core::ThermalStatus> = None;
        loop {
            tokio::time::sleep(Duration::from_secs(1)).await;
            let include_load = WINDOW_VISIBLE.load(std::sync::atomic::Ordering::Relaxed);
            let (returned_sampler, returned_thermal, snapshot) =
                tokio::task::spawn_blocking(move || {
                    let mut snapshot = sampler.sample(include_load);
                    let thermal = latest_thermal(last_thermal, crate::daemon::thermal_status());
                    snapshot.thermal = thermal.clone();
                    (sampler, thermal, snapshot)
                })
                .await
                .expect("telemetry sampling task panicked");
            sampler = returned_sampler;
            last_thermal = returned_thermal;
            *SHARED.lock().expect("telemetry snapshot lock") = snapshot.clone();
            if out.send(snapshot).await.is_err() {
                return;
            }
        }
    })
}

pub fn subscription() -> Subscription<Snapshot> {
    Subscription::run(snapshot_stream)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::time::Duration;

    fn fixture_dir(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("razer-gui-telemetry-{}-{name}", std::process::id()));
        if dir.exists() {
            std::fs::remove_dir_all(&dir).expect("clean fixture");
        }
        std::fs::create_dir_all(&dir).expect("create fixture");
        dir
    }

    #[test]
    fn parses_the_aggregate_cpu_line() {
        let stat = "cpu  100 20 30 400 50 0 6 0 0 0\ncpu0 1 2 3 4 5 0 0 0 0 0\n";
        let parsed = parse_proc_stat(stat).expect("parse");
        // idle = idle(400) + iowait(50); total = sum of all fields
        assert_eq!(parsed.idle, 450);
        assert_eq!(parsed.total, 606);
    }

    #[test]
    fn usage_is_the_non_idle_share_of_the_delta() {
        let prev = CpuStat {
            idle: 400,
            total: 1000,
        };
        let next = CpuStat {
            idle: 500,
            total: 1200,
        };
        // delta total 200, delta idle 100 -> 50 % busy
        assert_eq!(cpu_usage_percent(&prev, &next), Some(50.0));
        assert_eq!(cpu_usage_percent(&next, &next), None);
    }

    #[test]
    fn rapl_watts_converts_microjoules_over_time() {
        // 2_000_000 uJ over 1 s = 2 W
        assert_eq!(
            rapl_watts(1_000_000, 3_000_000, Duration::from_secs(1)),
            Some(2.0)
        );
        // counter wrapped -> unusable sample
        assert_eq!(
            rapl_watts(3_000_000, 1_000_000, Duration::from_secs(1)),
            None
        );
        assert_eq!(rapl_watts(0, 100, Duration::ZERO), None);
    }

    #[test]
    fn reads_battery_percent_status_and_signed_watts() {
        let dir = fixture_dir("battery");
        let bat = dir.join("BAT0");
        std::fs::create_dir_all(&bat).expect("mkdir BAT0");
        std::fs::write(bat.join("capacity"), "86\n").expect("capacity");
        std::fs::write(bat.join("status"), "Discharging\n").expect("status");
        std::fs::write(bat.join("power_now"), "32000000\n").expect("power_now");
        let info = read_battery(&dir).expect("battery");
        assert_eq!(info.percent, 86);
        assert_eq!(info.status, "Discharging");
        assert_eq!(info.watts, Some(-32.0));
    }

    #[test]
    fn finds_the_nvidia_dgpu_runtime_status() {
        let dir = fixture_dir("pci");
        let dev = dir.join("0000:01:00.0");
        std::fs::create_dir_all(dev.join("power")).expect("mkdir dev");
        std::fs::write(dev.join("vendor"), "0x10de\n").expect("vendor");
        std::fs::write(dev.join("class"), "0x030000\n").expect("class");
        std::fs::write(dev.join("power/runtime_status"), "suspended\n").expect("status");
        assert_eq!(nvidia_runtime_status(&dir).as_deref(), Some("suspended"));
    }

    fn thermal_status(
        cpu_rpm: u16,
        error: Option<razer_core::ThermalFailureDto>,
    ) -> razer_core::ThermalStatus {
        razer_core::ThermalStatus {
            safety_state: razer_core::ThermalSafetyStateDto::Ready,
            performance_mode: 0,
            fan_mode: razer_core::FanControlModeDto::Automatic,
            cpu_rpm,
            gpu_rpm: cpu_rpm,
            error,
        }
    }

    #[test]
    fn a_fresh_good_reading_replaces_the_stale_one() {
        let stale = thermal_status(1000, None);
        let fresh = thermal_status(2000, None);
        assert_eq!(latest_thermal(Some(stale), Ok(fresh.clone())), Some(fresh));
    }

    #[test]
    fn a_fresh_reading_with_an_error_keeps_the_last_good_one() {
        let last = thermal_status(1000, None);
        let fresh_with_error = thermal_status(
            0,
            Some(razer_core::ThermalFailureDto {
                code: razer_core::ThermalFailureCode::Transport,
                message: "tachometer readback failed".to_string(),
            }),
        );
        assert_eq!(
            latest_thermal(Some(last.clone()), Ok(fresh_with_error)),
            Some(last)
        );
    }

    #[test]
    fn a_failed_daemon_read_keeps_the_last_good_reading() {
        let last = thermal_status(1000, None);
        let error = crate::daemon::DaemonError::Unreachable("no daemon".to_string());
        assert_eq!(latest_thermal(Some(last.clone()), Err(error)), Some(last));
    }

    #[test]
    fn a_failed_daemon_read_with_no_prior_reading_stays_none() {
        let error = crate::daemon::DaemonError::Unreachable("no daemon".to_string());
        assert_eq!(latest_thermal(None, Err(error)), None);
    }
}
