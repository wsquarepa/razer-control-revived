use serde::{Deserialize, Serialize};
use std::{fs, fs::File, io, env, fmt};
use std::io::prelude::*;
use std::path::PathBuf;
use crate::thermal::{self, PerformanceMode, PowerSource};

const SETTINGS_FILE: &str = "/.local/share/razercontrol/daemon.json";
const EFFECTS_FILE: &str = "/.local/share/razercontrol/effects.json";

/// Configurations without a `schema_version` (serde default 0) are legacy and
/// undergo the one-time PID-specific migration in `migrate_for_pid`.
pub const CURRENT_SCHEMA_VERSION: u32 = 1;

#[derive(Debug)]
pub enum ConfigurationError {
    Io(io::Error),
    Json(serde_json::Error),
    UnknownLegacyMode { power_source: &'static str, wire_value: u8 },
    UnknownLegacyLevel { power_source: &'static str, zone: BoostZone, level: u8 },
}

impl fmt::Display for ConfigurationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ConfigurationError::Io(error) => write!(formatter, "configuration I/O failed: {error}"),
            ConfigurationError::Json(error) => {
                write!(formatter, "configuration JSON is invalid: {error}")
            }
            ConfigurationError::UnknownLegacyMode { power_source, wire_value } => write!(
                formatter,
                "legacy {power_source} profile has unknown power mode {wire_value} (expected 0-4)"
            ),
            ConfigurationError::UnknownLegacyLevel { power_source, zone, level } => write!(
                formatter,
                "legacy {power_source} profile has unknown {zone:?} boost level {level} (expected 0-3)"
            ),
        }
    }
}

impl From<io::Error> for ConfigurationError {
    fn from(error: io::Error) -> ConfigurationError {
        ConfigurationError::Io(error)
    }
}

impl From<serde_json::Error> for ConfigurationError {
    fn from(error: serde_json::Error) -> ConfigurationError {
        ConfigurationError::Json(error)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum BoostZone {
    Cpu,
    Gpu,
}

/// A lossy-but-safe transformation the one-time migration applied to a legacy
/// profile. Serialized as JSON into the daemon log.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub enum MigrationWarning {
    ModeUnavailableOnSource {
        power_source: &'static str,
        requested_wire_value: u8,
        migrated_wire_value: u8,
    },
    ExtremeLevelGated {
        power_source: &'static str,
        zone: BoostZone,
        migrated_level: u8,
    },
    RpmOutOfRange {
        power_source: &'static str,
        requested_rpm: i32,
        mode_wire_value: u8,
    },
}

pub struct MigrationOutcome {
    pub configuration: Configuration,
    pub warnings: Vec<MigrationWarning>,
    /// True when the configuration was transformed and must be persisted.
    pub migrated: bool,
}

fn power_source_label(source: PowerSource) -> &'static str {
    match source {
        PowerSource::Ac => "ac",
        PowerSource::Battery => "battery",
    }
}

/// Map a legacy generic mode name onto the 2025 wire value: Balanced 0 becomes
/// the source's Balanced slot (0 on AC, 6 on battery), Gaming 1 and Creator 2
/// both name Maximum Performance 2, Silent 3 becomes 5, Custom 4 stays. Values
/// outside the legacy set are configuration errors.
fn migrate_legacy_mode(
    wire_value: u8,
    source: PowerSource,
) -> Result<PerformanceMode, ConfigurationError> {
    match wire_value {
        0 => Ok(thermal::balanced_for(source)),
        1 | 2 => Ok(PerformanceMode::MaximumPerformance),
        3 => Ok(PerformanceMode::Silent),
        4 => Ok(PerformanceMode::Custom),
        _ => Err(ConfigurationError::UnknownLegacyMode {
            power_source: power_source_label(source),
            wire_value,
        }),
    }
}

/// Intent is migrated before availability: a name-migrated mode that is not
/// selectable for its profile's power source lands on Balanced with a warning.
fn migrate_availability(
    named_mode: PerformanceMode,
    source: PowerSource,
    warnings: &mut Vec<MigrationWarning>,
) -> PerformanceMode {
    if thermal::is_mode_selectable(source, named_mode) {
        return named_mode;
    }
    let fallback: PerformanceMode = thermal::balanced_for(source);
    warnings.push(MigrationWarning::ModeUnavailableOnSource {
        power_source: power_source_label(source),
        requested_wire_value: named_mode.wire_value(),
        migrated_wire_value: fallback.wire_value(),
    });
    fallback
}

fn migrate_level(
    level: u8,
    source: PowerSource,
    zone: BoostZone,
    warnings: &mut Vec<MigrationWarning>,
) -> Result<u8, ConfigurationError> {
    match level {
        0..=2 => Ok(level),
        3 => {
            warnings.push(MigrationWarning::ExtremeLevelGated {
                power_source: power_source_label(source),
                zone,
                migrated_level: 2,
            });
            Ok(2)
        }
        _ => Err(ConfigurationError::UnknownLegacyLevel {
            power_source: power_source_label(source),
            zone,
            level,
        }),
    }
}

/// Range-check a legacy fixed RPM against the mode that results after both the
/// name and availability migrations. Invalid values select firmware automatic
/// control (zero) with a warning instead of stranding startup.
fn migrate_rpm(
    fan_rpm: i32,
    mode: PerformanceMode,
    source: PowerSource,
    warnings: &mut Vec<MigrationWarning>,
) -> i32 {
    match thermal::validate_fixed_rpm(mode, fan_rpm) {
        Ok(None) => 0,
        Ok(Some(valid)) => i32::from(valid.0),
        // The only validation failure for a fixed RPM is an out-of-range value.
        Err(_) => {
            warnings.push(MigrationWarning::RpmOutOfRange {
                power_source: power_source_label(source),
                requested_rpm: fan_rpm,
                mode_wire_value: mode.wire_value(),
            });
            0
        }
    }
}

/// One-time deterministic schema migration. Versioned configurations and
/// models other than PID 02C6 pass through unchanged; legacy 02C6
/// configurations have every profile's mode, boost levels, and fixed RPM
/// migrated to the 2025 semantics.
pub fn migrate_for_pid(
    configuration: Configuration,
    pid: u16,
) -> Result<MigrationOutcome, ConfigurationError> {
    if configuration.schema_version >= CURRENT_SCHEMA_VERSION
        || pid != thermal::BLADE_16_2025_PID
    {
        return Ok(MigrationOutcome { configuration, warnings: vec![], migrated: false });
    }

    let mut migrated: Configuration = configuration;
    let mut warnings: Vec<MigrationWarning> = Vec::new();
    let profile_sources: [(usize, PowerSource); 2] =
        [(0, PowerSource::Battery), (1, PowerSource::Ac)];
    for (index, source) in profile_sources {
        let profile: &mut PowerConfig = &mut migrated.power[index];
        let named_mode: PerformanceMode = migrate_legacy_mode(profile.power_mode, source)?;
        let available_mode: PerformanceMode =
            migrate_availability(named_mode, source, &mut warnings);
        let cpu_boost: u8 = migrate_level(profile.cpu_boost, source, BoostZone::Cpu, &mut warnings)?;
        let gpu_boost: u8 = migrate_level(profile.gpu_boost, source, BoostZone::Gpu, &mut warnings)?;
        let fan_rpm: i32 = migrate_rpm(profile.fan_rpm, available_mode, source, &mut warnings);
        profile.power_mode = available_mode.wire_value();
        profile.cpu_boost = cpu_boost;
        profile.gpu_boost = gpu_boost;
        profile.fan_rpm = fan_rpm;
    }
    migrated.schema_version = CURRENT_SCHEMA_VERSION;
    Ok(MigrationOutcome { configuration: migrated, warnings, migrated: true })
}

#[derive(Serialize, Deserialize, Clone)]
pub struct PowerConfig {
    pub power_mode: u8,
    pub cpu_boost: u8,
    pub gpu_boost: u8,
    pub fan_rpm: i32,
    pub brightness: u8,
    pub logo_state: u8,
    pub screensaver: bool, // turno of keyboard light if screen is blank
    pub idle: u32,
}

impl PowerConfig {
    pub fn new() -> PowerConfig {
        return PowerConfig{
            power_mode: 0,
            cpu_boost: 1,
            gpu_boost: 0,
            fan_rpm: 0,
            brightness: 128,
            logo_state: 0,
            screensaver: false,
            idle: 0,
        }
    }
}

#[derive(Serialize, Deserialize)]
pub struct Configuration {
    /// Absent in legacy files, so serde defaults it to 0 (pre-migration).
    #[serde(default)]
    pub schema_version: u32,
    pub power: [PowerConfig; 2],
    pub sync: bool, // sync light settings between ac and battery
    pub no_light: f64, // no light bellow this percentage of battery
    pub standard_effect: u8,
    pub standard_effect_params: Vec<u8>,
    #[serde(default)]
    pub bho_on: bool,
    #[serde(default = "default_bho_threshold")]
    pub bho_threshold: u8,
    #[serde(default)]
    pub gui_effect: u8, // GUI custom effect index (0=Static, 1=StaticGradient, 2=WaveGradient, 3=Breathing)
    #[serde(default)]
    pub gui_effect_params: Vec<u8>, // GUI effect color params (RGB bytes)
}

fn default_bho_threshold() -> u8 { 80 }

impl Configuration {
    pub fn new() -> Configuration {
        return Configuration {
            schema_version: CURRENT_SCHEMA_VERSION,
            power: [PowerConfig::new(), PowerConfig::new()],
            sync: false,
            no_light: 0.0,
            standard_effect: 0x04, // spectrum cycling
            standard_effect_params: vec![],
            bho_on: false,
            bho_threshold: 80,
            gui_effect: 0,
            gui_effect_params: vec![],
        };
    }

    /// Serialize first, write a sibling temporary file, then rename: a crash
    /// mid-write can never leave a truncated configuration behind.
    pub fn write_to_file(&self) -> Result<(), ConfigurationError> {
        ensure_config_dir()?;
        let serialized: String = serde_json::to_string_pretty(self)?;
        let path: PathBuf = configuration_path();
        let temporary: PathBuf = path.with_extension("json.tmp");
        fs::write(&temporary, serialized.as_bytes())?;
        fs::rename(&temporary, &path)?;
        Ok(())
    }

    /// Load the saved configuration. Only a missing file creates a first-run
    /// configuration; malformed JSON and every other I/O failure propagate.
    pub fn load() -> Result<Configuration, ConfigurationError> {
        match fs::read_to_string(configuration_path()) {
            Ok(raw) => Ok(serde_json::from_str::<Configuration>(&raw)?),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(Configuration::new()),
            Err(error) => Err(ConfigurationError::Io(error)),
        }
    }

    pub fn write_effects_save(json: serde_json::Value) -> io::Result<()> {
        ensure_config_dir()?;
        let j: String = serde_json::to_string_pretty(&json)?;
        File::create(get_home_directory() + EFFECTS_FILE)?.write_all(j.as_bytes())?;
        Ok(())
    }

    pub fn read_effects_file() -> io::Result<serde_json::Value> {
        let str = fs::read_to_string(get_home_directory() + EFFECTS_FILE)?;
        let res: serde_json::Value = serde_json::from_str(str.as_str())?;
        Ok(res)
    }
}

fn configuration_path() -> PathBuf {
    PathBuf::from(get_home_directory() + SETTINGS_FILE)
}

fn get_home_directory() -> String {
    env::var("HOME").unwrap_or_else(|_| {
        eprintln!("WARNING: HOME environment variable not set, falling back to /tmp");
        "/tmp".to_string()
    })
}

fn ensure_config_dir() -> io::Result<()> {
    let dir = get_home_directory() + "/.local/share/razercontrol";
    fs::create_dir_all(dir)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::thermal;

    fn legacy_configuration() -> Configuration {
        let mut configuration: Configuration = Configuration::new();
        configuration.schema_version = 0;
        configuration
    }

    fn migrate(configuration: Configuration) -> MigrationOutcome {
        migrate_for_pid(configuration, thermal::BLADE_16_2025_PID).unwrap()
    }

    #[test]
    fn migrates_every_legacy_mode_on_ac() {
        let mode_map: [(u8, u8); 5] = [(0, 0), (1, 2), (2, 2), (3, 5), (4, 4)];
        for (legacy_mode, expected_mode) in mode_map {
            let mut configuration: Configuration = legacy_configuration();
            configuration.power[1].power_mode = legacy_mode;
            let outcome: MigrationOutcome = migrate(configuration);
            assert_eq!(outcome.configuration.power[1].power_mode, expected_mode);
            assert!(outcome.migrated);
        }
    }

    #[test]
    fn rejects_unknown_legacy_mode() {
        let mut configuration: Configuration = legacy_configuration();
        configuration.power[1].power_mode = 7;
        let result = migrate_for_pid(configuration, thermal::BLADE_16_2025_PID);
        assert!(matches!(
            result,
            Err(ConfigurationError::UnknownLegacyMode { wire_value: 7, .. })
        ));
    }

    #[test]
    fn migrates_unavailable_battery_mode_to_balanced() {
        // Legacy battery-side Gaming (1) names Maximum Performance, which is
        // AC-only on the 2025 model; availability migration lands on the
        // battery-domain Balanced slot (6).
        let mut configuration: Configuration = legacy_configuration();
        configuration.power[0].power_mode = 1;
        let outcome: MigrationOutcome = migrate(configuration);
        assert_eq!(outcome.configuration.power[0].power_mode, 6);
        assert!(outcome
            .warnings
            .iter()
            .any(|warning| matches!(warning, MigrationWarning::ModeUnavailableOnSource { .. })));
    }

    #[test]
    fn migrates_battery_balanced_to_dc_slot() {
        // Legacy Balanced (0) on the battery profile keeps its intent but moves
        // to the battery-domain wire value 6; no warning, the mode is unchanged
        // logically. The AC profile keeps wire 0.
        let mut configuration: Configuration = legacy_configuration();
        configuration.power[0].power_mode = 0;
        configuration.power[1].power_mode = 0;
        let outcome: MigrationOutcome = migrate(configuration);
        assert_eq!(outcome.configuration.power[0].power_mode, 6);
        assert_eq!(outcome.configuration.power[1].power_mode, 0);
        assert!(outcome.warnings.is_empty());
    }

    #[test]
    fn migrates_extreme_levels_to_high_in_both_zones() {
        for legacy_mode in [0u8, 4u8] {
            for ac in [0usize, 1usize] {
                let mut configuration: Configuration = legacy_configuration();
                configuration.power[ac].power_mode = legacy_mode;
                configuration.power[ac].cpu_boost = 3;
                configuration.power[ac].gpu_boost = 3;
                let outcome: MigrationOutcome = migrate(configuration);
                assert_eq!(outcome.configuration.power[ac].cpu_boost, 2);
                assert_eq!(outcome.configuration.power[ac].gpu_boost, 2);
                let gated_warnings = outcome
                    .warnings
                    .iter()
                    .filter(|warning| matches!(warning, MigrationWarning::ExtremeLevelGated { .. }))
                    .count();
                assert_eq!(gated_warnings, 2);
            }
        }
    }

    #[test]
    fn rejects_unknown_legacy_level() {
        let mut configuration: Configuration = legacy_configuration();
        configuration.power[1].cpu_boost = 4;
        let result = migrate_for_pid(configuration, thermal::BLADE_16_2025_PID);
        assert!(matches!(
            result,
            Err(ConfigurationError::UnknownLegacyLevel { level: 4, .. })
        ));
    }

    #[test]
    fn migrates_out_of_range_rpm_to_automatic() {
        for out_of_range_rpm in [1i32, 3399, 5201, 9000] {
            let mut configuration: Configuration = legacy_configuration();
            configuration.power[1].power_mode = 0;
            configuration.power[1].fan_rpm = out_of_range_rpm;
            let outcome: MigrationOutcome = migrate(configuration);
            assert_eq!(outcome.configuration.power[1].fan_rpm, 0);
            assert!(outcome
                .warnings
                .iter()
                .any(|warning| matches!(warning, MigrationWarning::RpmOutOfRange { .. })));
        }
    }

    #[test]
    fn keeps_valid_rpm_for_migrated_mode() {
        let mut configuration: Configuration = legacy_configuration();
        configuration.power[1].power_mode = 0;
        configuration.power[1].fan_rpm = 4000;
        let outcome: MigrationOutcome = migrate(configuration);
        assert_eq!(outcome.configuration.power[1].fan_rpm, 4000);
    }

    #[test]
    fn checks_rpm_against_post_availability_mode() {
        // Battery legacy Gaming (1) lands on the battery Balanced slot (6);
        // 5300 is valid for the named Maximum Performance range but invalid for
        // Balanced, so the RPM check must run against the availability-migrated
        // mode.
        let mut configuration: Configuration = legacy_configuration();
        configuration.power[0].power_mode = 1;
        configuration.power[0].fan_rpm = 5300;
        let outcome: MigrationOutcome = migrate(configuration);
        assert_eq!(outcome.configuration.power[0].power_mode, 6);
        assert_eq!(outcome.configuration.power[0].fan_rpm, 0);
    }

    #[test]
    fn versioned_configuration_passes_through() {
        let mut configuration: Configuration = Configuration::new();
        // BatterySaver (3) is a valid 2025 battery mode and must not be
        // reinterpreted as legacy Silent.
        configuration.power[0].power_mode = 3;
        let outcome: MigrationOutcome = migrate(configuration);
        assert!(!outcome.migrated);
        assert!(outcome.warnings.is_empty());
        assert_eq!(outcome.configuration.power[0].power_mode, 3);
    }

    #[test]
    fn other_models_pass_through_unchanged() {
        let mut configuration: Configuration = legacy_configuration();
        configuration.power[1].power_mode = 1;
        configuration.power[1].cpu_boost = 3;
        let outcome: MigrationOutcome = migrate_for_pid(configuration, 0x0233).unwrap();
        assert!(!outcome.migrated);
        assert!(outcome.warnings.is_empty());
        assert_eq!(outcome.configuration.power[1].power_mode, 1);
        assert_eq!(outcome.configuration.power[1].cpu_boost, 3);
    }

    #[test]
    fn migrates_complete_legacy_fixture() {
        let legacy: Configuration =
            serde_json::from_str(include_str!("../../testdata/legacy-02c6-config.json")).unwrap();
        let outcome: MigrationOutcome =
            migrate_for_pid(legacy, thermal::BLADE_16_2025_PID).unwrap();
        assert_eq!(outcome.configuration.power[0].power_mode, 6);
        assert_eq!(outcome.configuration.power[1].power_mode, 4);
        assert_eq!(outcome.configuration.power[0].cpu_boost, 2);
        assert_eq!(outcome.configuration.power[1].gpu_boost, 2);
        assert_eq!(outcome.configuration.power[0].fan_rpm, 0);
        assert_eq!(outcome.configuration.power[1].fan_rpm, 0);
        assert_eq!(outcome.configuration.schema_version, CURRENT_SCHEMA_VERSION);
        assert!(outcome.migrated);

        let rewritten: String = serde_json::to_string_pretty(&outcome.configuration).unwrap();
        assert!(!rewritten.contains("fan_curve"));
    }

    #[test]
    fn warnings_serialize_as_structured_json() {
        let mut configuration: Configuration = legacy_configuration();
        configuration.power[0].power_mode = 1;
        let outcome: MigrationOutcome = migrate(configuration);
        for warning in &outcome.warnings {
            let serialized: String = serde_json::to_string(warning).unwrap();
            assert!(serialized.starts_with('{') || serialized.starts_with('"'));
        }
    }
}
