//! Pure thermal-safety policy for the Razer Blade 16 2025 (PID 0x02C6).
//!
//! Every function here is a pure decision: no HID I/O, no daemon state. The
//! RPM ranges are Synapse product-710 configuration defaults and remain
//! provisional until the supervised `0x86` limit collection confirms them on
//! the physical unit. Hyperboost and custom level 3 (Extreme) are recognized
//! for decoding but rejected by every setter-facing validation until the
//! gated hardware validation passes.

pub const BLADE_16_2025_PID: u16 = 0x02c6;

/// EC fan zones as carried in the fan-ID byte of every 0x0d thermal command.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FanId {
    Cpu,
    Gpu,
}

impl FanId {
    pub const fn wire_value(self) -> u8 {
        match self {
            FanId::Cpu => 1,
            FanId::Gpu => 2,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PowerSource {
    Ac,
    Battery,
}

/// Performance modes the 02C6 EC recognizes. Recognized is wider than
/// selectable: Hyperboost decodes but is never offered (see
/// `selectable_modes`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PerformanceMode {
    Balanced,
    MaximumPerformance,
    BatterySaver,
    Custom,
    Silent,
    Hyperboost,
}

impl PerformanceMode {
    pub const fn wire_value(self) -> u8 {
        match self {
            PerformanceMode::Balanced => 0,
            PerformanceMode::MaximumPerformance => 2,
            PerformanceMode::BatterySaver => 3,
            PerformanceMode::Custom => 4,
            PerformanceMode::Silent => 5,
            PerformanceMode::Hyperboost => 7,
        }
    }
}

impl TryFrom<u8> for PerformanceMode {
    type Error = ThermalPolicyError;

    fn try_from(wire_value: u8) -> Result<PerformanceMode, ThermalPolicyError> {
        match wire_value {
            0 => Ok(PerformanceMode::Balanced),
            2 => Ok(PerformanceMode::MaximumPerformance),
            3 => Ok(PerformanceMode::BatterySaver),
            4 => Ok(PerformanceMode::Custom),
            5 => Ok(PerformanceMode::Silent),
            7 => Ok(PerformanceMode::Hyperboost),
            _ => Err(ThermalPolicyError::UnknownMode { wire_value }),
        }
    }
}

/// A validated fixed manual fan speed in RPM. Never constructed from an
/// out-of-range request; zero never reaches this type (zero selects firmware
/// automatic control).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FanRpm(pub u16);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RpmRange {
    pub min: u16,
    pub max: u16,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThermalPolicyError {
    UnknownMode { wire_value: u8 },
    ModeNotSelectable { mode: PerformanceMode, source: PowerSource },
    RpmOutOfRange { mode: PerformanceMode, requested_rpm: i32, range: RpmRange },
    ExtremeNotValidated,
    LevelOutOfRange { level: u8 },
}

impl std::fmt::Display for ThermalPolicyError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ThermalPolicyError::UnknownMode { wire_value } => {
                write!(formatter, "unknown performance-mode wire value {wire_value}")
            }
            ThermalPolicyError::ModeNotSelectable { mode, source } => {
                write!(formatter, "mode {mode:?} is not selectable on {source:?}")
            }
            ThermalPolicyError::RpmOutOfRange { mode, requested_rpm, range } => {
                write!(
                    formatter,
                    "requested {requested_rpm} RPM is outside {}..={} for mode {mode:?}",
                    range.min, range.max
                )
            }
            ThermalPolicyError::ExtremeNotValidated => {
                write!(
                    formatter,
                    "custom level 3 (Extreme) is not validated on this unit and stays disabled"
                )
            }
            ThermalPolicyError::LevelOutOfRange { level } => {
                write!(formatter, "custom level {level} is not a recognized level (0-3)")
            }
        }
    }
}

const AC_SELECTABLE_MODES: [PerformanceMode; 4] = [
    PerformanceMode::Balanced,
    PerformanceMode::Silent,
    PerformanceMode::MaximumPerformance,
    PerformanceMode::Custom,
];

const BATTERY_SELECTABLE_MODES: [PerformanceMode; 2] =
    [PerformanceMode::Balanced, PerformanceMode::BatterySaver];

/// Modes the daemon offers for the given power source. Hyperboost is
/// recognized but never offered: Synapse runtime-gates it and this SKU has
/// not passed the supervised write/readback validation.
pub fn selectable_modes(source: PowerSource) -> &'static [PerformanceMode] {
    match source {
        PowerSource::Ac => &AC_SELECTABLE_MODES,
        PowerSource::Battery => &BATTERY_SELECTABLE_MODES,
    }
}

pub fn is_mode_selectable(source: PowerSource, mode: PerformanceMode) -> bool {
    selectable_modes(source).contains(&mode)
}

/// Synapse product-710 configuration defaults; provisional until the live
/// 0x86 limit collection on the repaired unit confirms or replaces them.
pub const fn provisional_rpm_range(mode: PerformanceMode) -> RpmRange {
    match mode {
        PerformanceMode::Balanced | PerformanceMode::Silent => RpmRange { min: 3400, max: 5200 },
        PerformanceMode::MaximumPerformance | PerformanceMode::BatterySaver => {
            RpmRange { min: 3300, max: 5400 }
        }
        PerformanceMode::Custom => RpmRange { min: 4000, max: 5300 },
        PerformanceMode::Hyperboost => RpmRange { min: 3700, max: 5300 },
    }
}

/// Maximum accepted deviation between a fixed manual target and the 0x88
/// tachometer reading before a verification cycle counts as failed.
pub fn manual_tolerance(target_rpm: u16) -> u16 {
    500_u16.max(target_rpm / 4)
}

/// Validate a requested fixed fan speed against the mode's provisional range.
/// Zero selects firmware automatic control and is not range-checked. Nonzero
/// requests are never clamped: out-of-range requests are rejected outright.
pub fn validate_fixed_rpm(
    mode: PerformanceMode,
    requested_rpm: i32,
) -> Result<Option<FanRpm>, ThermalPolicyError> {
    if requested_rpm == 0 {
        return Ok(None);
    }
    let range: RpmRange = provisional_rpm_range(mode);
    if requested_rpm < i32::from(range.min) || requested_rpm > i32::from(range.max) {
        return Err(ThermalPolicyError::RpmOutOfRange { mode, requested_rpm, range });
    }
    Ok(Some(FanRpm(requested_rpm as u16)))
}

/// Validate a custom CPU/GPU level for the 0x07 setter. Levels 0-2 pass;
/// level 3 (Extreme) is recognized for display but stays write-disabled until
/// the supervised hardware gate proves it on this unit.
pub fn validate_custom_level(level: u8) -> Result<u8, ThermalPolicyError> {
    match level {
        0..=2 => Ok(level),
        3 => Err(ThermalPolicyError::ExtremeNotValidated),
        _ => Err(ThermalPolicyError::LevelOutOfRange { level }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exposes_only_valid_modes() {
        assert_eq!(
            selectable_modes(PowerSource::Ac),
            &[
                PerformanceMode::Balanced,
                PerformanceMode::Silent,
                PerformanceMode::MaximumPerformance,
                PerformanceMode::Custom,
            ]
        );
        assert_eq!(
            selectable_modes(PowerSource::Battery),
            &[PerformanceMode::Balanced, PerformanceMode::BatterySaver]
        );
    }

    #[test]
    fn validates_fixed_rpm_without_clamping() {
        assert_eq!(validate_fixed_rpm(PerformanceMode::Balanced, 0), Ok(None));
        assert_eq!(
            validate_fixed_rpm(PerformanceMode::Balanced, 3400),
            Ok(Some(FanRpm(3400)))
        );
        assert!(matches!(
            validate_fixed_rpm(PerformanceMode::Balanced, 3399),
            Err(ThermalPolicyError::RpmOutOfRange { .. })
        ));
        assert!(matches!(
            validate_fixed_rpm(PerformanceMode::Balanced, 5201),
            Err(ThermalPolicyError::RpmOutOfRange { .. })
        ));
    }

    #[test]
    fn gates_unvalidated_features() {
        assert!(!is_mode_selectable(PowerSource::Ac, PerformanceMode::Hyperboost));
        assert_eq!(
            validate_custom_level(3),
            Err(ThermalPolicyError::ExtremeNotValidated)
        );
    }

    #[test]
    fn decodes_recognized_wire_values_only() {
        assert_eq!(PerformanceMode::try_from(0u8), Ok(PerformanceMode::Balanced));
        assert_eq!(
            PerformanceMode::try_from(2u8),
            Ok(PerformanceMode::MaximumPerformance)
        );
        assert_eq!(
            PerformanceMode::try_from(3u8),
            Ok(PerformanceMode::BatterySaver)
        );
        assert_eq!(PerformanceMode::try_from(4u8), Ok(PerformanceMode::Custom));
        assert_eq!(PerformanceMode::try_from(5u8), Ok(PerformanceMode::Silent));
        assert_eq!(
            PerformanceMode::try_from(7u8),
            Ok(PerformanceMode::Hyperboost)
        );
        assert_eq!(
            PerformanceMode::try_from(1u8),
            Err(ThermalPolicyError::UnknownMode { wire_value: 1 })
        );
        assert_eq!(
            PerformanceMode::try_from(6u8),
            Err(ThermalPolicyError::UnknownMode { wire_value: 6 })
        );
    }

    #[test]
    fn wire_values_round_trip() {
        for mode in [
            PerformanceMode::Balanced,
            PerformanceMode::MaximumPerformance,
            PerformanceMode::BatterySaver,
            PerformanceMode::Custom,
            PerformanceMode::Silent,
            PerformanceMode::Hyperboost,
        ] {
            assert_eq!(PerformanceMode::try_from(mode.wire_value()), Ok(mode));
        }
    }

    #[test]
    fn fan_ids_match_ec_zones() {
        assert_eq!(FanId::Cpu.wire_value(), 1);
        assert_eq!(FanId::Gpu.wire_value(), 2);
    }

    #[test]
    fn rejects_unavailable_mode_for_power_source() {
        assert!(!is_mode_selectable(PowerSource::Battery, PerformanceMode::MaximumPerformance));
        assert!(!is_mode_selectable(PowerSource::Battery, PerformanceMode::Silent));
        assert!(!is_mode_selectable(PowerSource::Battery, PerformanceMode::Custom));
        assert!(!is_mode_selectable(PowerSource::Ac, PerformanceMode::BatterySaver));
        assert!(!is_mode_selectable(PowerSource::Battery, PerformanceMode::Hyperboost));
        assert!(is_mode_selectable(PowerSource::Ac, PerformanceMode::MaximumPerformance));
    }

    #[test]
    fn validates_custom_levels() {
        assert_eq!(validate_custom_level(0), Ok(0));
        assert_eq!(validate_custom_level(1), Ok(1));
        assert_eq!(validate_custom_level(2), Ok(2));
        assert_eq!(
            validate_custom_level(4),
            Err(ThermalPolicyError::LevelOutOfRange { level: 4 })
        );
    }

    #[test]
    fn rpm_boundaries_for_every_mode() {
        let cases: [(PerformanceMode, u16, u16); 6] = [
            (PerformanceMode::Balanced, 3400, 5200),
            (PerformanceMode::Silent, 3400, 5200),
            (PerformanceMode::MaximumPerformance, 3300, 5400),
            (PerformanceMode::BatterySaver, 3300, 5400),
            (PerformanceMode::Custom, 4000, 5300),
            (PerformanceMode::Hyperboost, 3700, 5300),
        ];
        for (mode, min, max) in cases {
            assert_eq!(provisional_rpm_range(mode), RpmRange { min, max });
            assert!(matches!(
                validate_fixed_rpm(mode, i32::from(min) - 1),
                Err(ThermalPolicyError::RpmOutOfRange { .. })
            ));
            assert_eq!(
                validate_fixed_rpm(mode, i32::from(min)),
                Ok(Some(FanRpm(min)))
            );
            assert_eq!(
                validate_fixed_rpm(mode, i32::from(max)),
                Ok(Some(FanRpm(max)))
            );
            assert!(matches!(
                validate_fixed_rpm(mode, i32::from(max) + 1),
                Err(ThermalPolicyError::RpmOutOfRange { .. })
            ));
            assert_eq!(validate_fixed_rpm(mode, 0), Ok(None));
        }
    }

    #[test]
    fn tolerance_scales_with_target() {
        assert_eq!(manual_tolerance(3400), 850);
        assert_eq!(manual_tolerance(4000), 1000);
        assert_eq!(manual_tolerance(5400), 1350);
        assert_eq!(manual_tolerance(1000), 500);
    }
}
