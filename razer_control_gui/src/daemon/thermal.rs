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

/// A single class-0x0d thermal command payload, independent of HID transport.
/// The command class is always 0x0d; only the id, the declared request data
/// size, and the 80 argument bytes vary. This is data, not behavior: the
/// builders below fill it and the device layer frames it into a HID report.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ThermalCommand {
    pub command_id: u8,
    pub data_size: u8,
    pub args: [u8; 80],
}

/// EC-reported fan RPM limits for one performance mode, already scaled to RPM.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FanLimits {
    pub min: u16,
    pub default: u16,
    pub max: u16,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThermalDecodeError {
    UnexpectedFanCount { expected: u8, actual: u8 },
    UnknownFanId { id: u8, index: usize },
    DuplicateFanId { id: u8 },
    MissingFanId { fan: FanId },
    UnexpectedProfile { expected: u8, actual: u8 },
    UnexpectedFan { expected: u8, actual: u8 },
    ZeroLimit { min: u16, default: u16, max: u16 },
    InvertedLimits { min: u16, default: u16, max: u16 },
}

/// Profile slot every per-fan thermal command targets. Pinning this to 1 keeps
/// reads and writes on the same EC slot; earlier builds wrote boost commands to
/// profile 0, an inactive slot.
const THERMAL_PROFILE: u8 = 1;

/// Build the 0x80 Get Thermal Fan ID List request. Request data size 80 mirrors
/// Synapse's variable-length list read (request tuple `[80, 13, 128]`).
pub fn get_fan_ids() -> ThermalCommand {
    ThermalCommand { command_id: 0x80, data_size: 80, args: [0u8; 80] }
}

/// Build the 0x01 Set Thermal Fan Speed request: `[profile=1, fan_id, rpm/100]`.
pub fn set_fan_speed(fan: FanId, rpm: FanRpm) -> ThermalCommand {
    let mut args: [u8; 80] = [0; 80];
    args[0] = THERMAL_PROFILE;
    args[1] = fan.wire_value();
    args[2] = (rpm.0 / 100) as u8;
    ThermalCommand { command_id: 0x01, data_size: 3, args }
}

/// Build the 0x02 Set Thermal Fan Mode request:
/// `[profile=1, fan_id, performance_mode, manual_flag]`.
pub fn set_fan_mode(fan: FanId, mode: PerformanceMode, manual: bool) -> ThermalCommand {
    let mut args: [u8; 80] = [0; 80];
    args[0] = THERMAL_PROFILE;
    args[1] = fan.wire_value();
    args[2] = mode.wire_value();
    args[3] = u8::from(manual);
    ThermalCommand { command_id: 0x02, data_size: 4, args }
}

/// Build the 0x07 Set Custom CPU/GPU Level request: `[profile=1, fan_id, level]`.
pub fn set_boost(fan: FanId, level: u8) -> ThermalCommand {
    let mut args: [u8; 80] = [0; 80];
    args[0] = THERMAL_PROFILE;
    args[1] = fan.wire_value();
    args[2] = level;
    ThermalCommand { command_id: 0x07, data_size: 3, args }
}

/// Build the 0x81 Get Thermal Fan Speed request: `[profile=1, fan_id]`.
pub fn get_fan_speed(fan: FanId) -> ThermalCommand {
    let mut args: [u8; 80] = [0; 80];
    args[0] = THERMAL_PROFILE;
    args[1] = fan.wire_value();
    ThermalCommand { command_id: 0x81, data_size: 3, args }
}

/// Build the 0x82 Get Thermal Fan Mode request: `[profile=1, fan_id]`.
pub fn get_fan_mode(fan: FanId) -> ThermalCommand {
    let mut args: [u8; 80] = [0; 80];
    args[0] = THERMAL_PROFILE;
    args[1] = fan.wire_value();
    ThermalCommand { command_id: 0x82, data_size: 4, args }
}

/// Build the 0x86 Get Thermal Fan Information request. Limits are per
/// performance mode, not per fan zone, so the payload is a single argument-0
/// byte (Synapse request tuple `[7, 13, 134]`).
pub fn get_fan_limits() -> ThermalCommand {
    ThermalCommand { command_id: 0x86, data_size: 7, args: [0u8; 80] }
}

/// Build the 0x87 Get Custom CPU/GPU Level request: `[profile=1, fan_id]`.
pub fn get_boost(fan: FanId) -> ThermalCommand {
    let mut args: [u8; 80] = [0; 80];
    args[0] = THERMAL_PROFILE;
    args[1] = fan.wire_value();
    ThermalCommand { command_id: 0x87, data_size: 3, args }
}

/// Build the 0x88 Get Thermal Fan Current Speed request: `[profile=1, fan_id]`.
pub fn get_current_fan_rpm(fan: FanId) -> ThermalCommand {
    let mut args: [u8; 80] = [0; 80];
    args[0] = THERMAL_PROFILE;
    args[1] = fan.wire_value();
    ThermalCommand { command_id: 0x88, data_size: 3, args }
}

/// Decode the 0x80 fan-ID list reply. The payload is count-prefixed: `args[0]`
/// is the count, `args[1..1 + count]` are the IDs, and any following bytes are
/// padding. The Blade 16 2025 preflight requires exactly the set `{CPU, GPU}`;
/// order is normalized to `[Cpu, Gpu]` on success.
pub fn decode_fan_ids(args: &[u8; 80]) -> Result<[FanId; 2], ThermalDecodeError> {
    const EXPECTED_COUNT: u8 = 2;
    let count: u8 = args[0];
    if count != EXPECTED_COUNT {
        return Err(ThermalDecodeError::UnexpectedFanCount { expected: EXPECTED_COUNT, actual: count });
    }
    // count == 2 is verified before any indexing, so args[1] and args[2] are in bounds.
    let first: FanId = decode_fan_id_byte(args[1], 1)?;
    let second: FanId = decode_fan_id_byte(args[2], 2)?;
    if first == second {
        return Err(ThermalDecodeError::DuplicateFanId { id: args[1] });
    }
    if first != FanId::Cpu && second != FanId::Cpu {
        return Err(ThermalDecodeError::MissingFanId { fan: FanId::Cpu });
    }
    if first != FanId::Gpu && second != FanId::Gpu {
        return Err(ThermalDecodeError::MissingFanId { fan: FanId::Gpu });
    }
    Ok([FanId::Cpu, FanId::Gpu])
}

fn decode_fan_id_byte(raw: u8, index: usize) -> Result<FanId, ThermalDecodeError> {
    match raw {
        1 => Ok(FanId::Cpu),
        2 => Ok(FanId::Gpu),
        _ => Err(ThermalDecodeError::UnknownFanId { id: raw, index }),
    }
}

/// Decode a 0x88 Get Thermal Fan Current Speed reply: `[profile=1, fan_id,
/// rpm/100]`. Rejects a reply whose profile or fan byte does not match the
/// request so a stale reply is never read as the requested zone.
pub fn decode_fan_rpm(fan: FanId, args: &[u8; 80]) -> Result<u16, ThermalDecodeError> {
    if args[0] != THERMAL_PROFILE {
        return Err(ThermalDecodeError::UnexpectedProfile { expected: THERMAL_PROFILE, actual: args[0] });
    }
    let expected_fan: u8 = fan.wire_value();
    if args[1] != expected_fan {
        return Err(ThermalDecodeError::UnexpectedFan { expected: expected_fan, actual: args[1] });
    }
    Ok(u16::from(args[2]) * 100)
}

/// Decode a 0x86 Get Thermal Fan Information reply. Minimum, default, and
/// maximum RPM sit at fields B/D/F (args indices 6/8/10), each in units of 100
/// RPM. Rejects a zero limit or a non-ascending min/default/max ordering.
pub fn decode_fan_limits(args: &[u8; 80]) -> Result<FanLimits, ThermalDecodeError> {
    let min: u16 = u16::from(args[6]) * 100;
    let default: u16 = u16::from(args[8]) * 100;
    let max: u16 = u16::from(args[10]) * 100;
    if min == 0 || default == 0 || max == 0 {
        return Err(ThermalDecodeError::ZeroLimit { min, default, max });
    }
    if min > default || default > max {
        return Err(ThermalDecodeError::InvertedLimits { min, default, max });
    }
    Ok(FanLimits { min, default, max })
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

    #[test]
    fn builds_get_fan_ids_request() {
        let command: ThermalCommand = get_fan_ids();
        assert_eq!(command.command_id, 0x80);
        assert_eq!(command.data_size, 80);
        assert_eq!(command.args, [0u8; 80]);
    }

    #[test]
    fn builds_set_fan_speed_request() {
        let command: ThermalCommand = set_fan_speed(FanId::Cpu, FanRpm(3400));
        assert_eq!(command.command_id, 0x01);
        assert_eq!(command.data_size, 3);
        assert_eq!(&command.args[..3], &[1, 1, 34]);
    }

    #[test]
    fn builds_set_fan_mode_request() {
        let command: ThermalCommand =
            set_fan_mode(FanId::Gpu, PerformanceMode::MaximumPerformance, true);
        assert_eq!(command.command_id, 0x02);
        assert_eq!(command.data_size, 4);
        assert_eq!(&command.args[..4], &[1, 2, 2, 1]);
    }

    #[test]
    fn builds_set_boost_request_with_profile_one() {
        // Regression guard: earlier builds shipped boost with profile byte 0.
        let command: ThermalCommand = set_boost(FanId::Cpu, 2);
        assert_eq!(command.command_id, 0x07);
        assert_eq!(command.data_size, 3);
        assert_eq!(&command.args[..3], &[1, 1, 2]);
    }

    #[test]
    fn builds_get_fan_speed_request() {
        let command: ThermalCommand = get_fan_speed(FanId::Cpu);
        assert_eq!(command.command_id, 0x81);
        assert_eq!(command.data_size, 3);
        assert_eq!(&command.args[..2], &[1, 1]);
    }

    #[test]
    fn builds_get_fan_mode_request() {
        let command: ThermalCommand = get_fan_mode(FanId::Gpu);
        assert_eq!(command.command_id, 0x82);
        assert_eq!(command.data_size, 4);
        assert_eq!(&command.args[..2], &[1, 2]);
    }

    #[test]
    fn builds_get_fan_limits_request_mode_global() {
        // 0x86 is mode-global: a single argument-0 payload, never profile-pinned or per fan.
        let command: ThermalCommand = get_fan_limits();
        assert_eq!(command.command_id, 0x86);
        assert_eq!(command.data_size, 7);
        assert_eq!(command.args, [0u8; 80]);
    }

    #[test]
    fn builds_get_boost_request_with_profile_one() {
        let command: ThermalCommand = get_boost(FanId::Gpu);
        assert_eq!(command.command_id, 0x87);
        assert_eq!(command.data_size, 3);
        assert_eq!(&command.args[..2], &[1, 2]);
    }

    #[test]
    fn builds_get_current_fan_rpm_request() {
        let command: ThermalCommand = get_current_fan_rpm(FanId::Cpu);
        assert_eq!(command.command_id, 0x88);
        assert_eq!(command.data_size, 3);
        assert_eq!(&command.args[..2], &[1, 1]);
    }

    #[test]
    fn decodes_current_fan_rpm() {
        let mut args: [u8; 80] = [0; 80];
        args[..3].copy_from_slice(&[1, 2, 34]);
        assert_eq!(decode_fan_rpm(FanId::Gpu, &args), Ok(3400));
    }

    #[test]
    fn rejects_fan_rpm_with_wrong_profile() {
        let mut args: [u8; 80] = [0; 80];
        args[..3].copy_from_slice(&[0, 1, 34]);
        assert_eq!(
            decode_fan_rpm(FanId::Cpu, &args),
            Err(ThermalDecodeError::UnexpectedProfile { expected: 1, actual: 0 })
        );
    }

    #[test]
    fn rejects_fan_rpm_with_wrong_fan() {
        let mut args: [u8; 80] = [0; 80];
        args[..3].copy_from_slice(&[1, 2, 34]);
        assert_eq!(
            decode_fan_rpm(FanId::Cpu, &args),
            Err(ThermalDecodeError::UnexpectedFan { expected: 1, actual: 2 })
        );
    }

    #[test]
    fn decodes_fan_limits() {
        let mut args: [u8; 80] = [0; 80];
        args[6] = 34;
        args[8] = 40;
        args[10] = 54;
        assert_eq!(
            decode_fan_limits(&args),
            Ok(FanLimits { min: 3400, default: 4000, max: 5400 })
        );
    }

    #[test]
    fn rejects_zero_fan_limits() {
        let mut args: [u8; 80] = [0; 80];
        args[6] = 34;
        args[8] = 0;
        args[10] = 54;
        assert_eq!(
            decode_fan_limits(&args),
            Err(ThermalDecodeError::ZeroLimit { min: 3400, default: 0, max: 5400 })
        );
    }

    #[test]
    fn rejects_inverted_fan_limits() {
        let mut args: [u8; 80] = [0; 80];
        args[6] = 54;
        args[8] = 40;
        args[10] = 34;
        assert_eq!(
            decode_fan_limits(&args),
            Err(ThermalDecodeError::InvertedLimits { min: 5400, default: 4000, max: 3400 })
        );
    }

    #[test]
    fn decodes_count_prefixed_fan_ids() {
        let mut args: [u8; 80] = [0; 80];
        args[..3].copy_from_slice(&[2, 1, 2]);
        assert_eq!(decode_fan_ids(&args), Ok([FanId::Cpu, FanId::Gpu]));
    }

    #[test]
    fn normalizes_reversed_fan_id_order() {
        let mut args: [u8; 80] = [0; 80];
        args[..3].copy_from_slice(&[2, 2, 1]);
        assert_eq!(decode_fan_ids(&args), Ok([FanId::Cpu, FanId::Gpu]));
    }

    #[test]
    fn ignores_padding_after_counted_ids() {
        let mut args: [u8; 80] = [0; 80];
        args[..5].copy_from_slice(&[2, 1, 2, 9, 9]);
        assert_eq!(decode_fan_ids(&args), Ok([FanId::Cpu, FanId::Gpu]));
    }

    #[test]
    fn rejects_wrong_fan_count() {
        for count in [0_u8, 1, 3, 80] {
            let mut args: [u8; 80] = [0; 80];
            args[0] = count;
            assert!(matches!(
                decode_fan_ids(&args),
                Err(ThermalDecodeError::UnexpectedFanCount { expected: 2, actual }) if actual == count
            ));
        }
    }

    #[test]
    fn rejects_duplicate_fan_ids() {
        let mut args: [u8; 80] = [0; 80];
        args[..3].copy_from_slice(&[2, 1, 1]);
        assert_eq!(
            decode_fan_ids(&args),
            Err(ThermalDecodeError::DuplicateFanId { id: 1 })
        );
    }

    #[test]
    fn rejects_unknown_fan_id() {
        let mut args: [u8; 80] = [0; 80];
        args[..3].copy_from_slice(&[2, 1, 3]);
        assert_eq!(
            decode_fan_ids(&args),
            Err(ThermalDecodeError::UnknownFanId { id: 3, index: 2 })
        );
    }
}
