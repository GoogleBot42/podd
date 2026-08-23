//! Wire-format types for the free-sleep-compatible JSON API.
//!
//! Every struct uses `#[serde(rename_all = "camelCase")]` so the JSON keys match
//! free-sleep's zod schemas exactly (`targetTemperatureF`, `currentTemperatureLevel`,
//! `waterLevel`, `isPriming`, `coverVersion`, `freeSleep`, `wifiStrength`,
//! `ledBrightness`, `doubleTap`, ...). Temperatures are integer °F 55–110 on the
//! wire. Sides serialize as `"left"` / `"right"`.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// °F ↔ internal "level" scale used by the cover firmware.
/// `level = (F - 82.5) / 27.5 * 100`.
pub fn f_to_level(f: f64) -> f64 {
    (f - 82.5) / 27.5 * 100.0
}

/// °C → °F. podd-core reports temperatures in Celsius; the wire uses °F.
pub fn c_to_f(c: f64) -> f64 {
    c * 9.0 / 5.0 + 32.0
}

// ---------------------------------------------------------------------------
// shared
// ---------------------------------------------------------------------------

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug, Hash)]
#[serde(rename_all = "lowercase")]
pub enum Side {
    Left,
    Right,
}

/// "HH:mm" (validated against `^([01]\d|2[0-3]):([0-5]\d)$` upstream).
pub type HhMm = String;
/// Integer °F, 55..=110 on the wire.
pub type TempF = i32;

// ---------------------------------------------------------------------------
// GET /api/deviceStatus
// ---------------------------------------------------------------------------

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct Taps {
    pub double_tap: i64,
    pub triple_tap: i64,
    pub quad_tap: i64,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct SideStatus {
    pub current_temperature_level: f64,
    pub current_temperature_f: f64,
    pub target_temperature_f: TempF,
    pub seconds_remaining: i64,
    pub is_on: bool,
    pub is_alarm_vibrating: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub taps: Option<Taps>,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct DevSettingsBlock {
    pub v: i64,
    pub gain_left: f64,
    pub gain_right: f64,
    pub led_brightness: i64,
}

/// Build identity of the running daemon, under free-sleep's `freeSleep` key.
///
/// The field *names* are free-sleep's and stay as-is (the UI's zod schema is
/// `.strict()`), but the values are podd's build stamp: `version` is
/// `git describe`, and `branch` carries the short commit hash — branch names
/// don't survive a reproducible Nix build, and a hardcoded `"main"` was worse
/// than useless during deploy verification (issue #109).
#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct FreeSleepInfo {
    pub version: String,
    pub branch: String,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct DeviceStatus {
    pub left: SideStatus,
    pub right: SideStatus,
    /// NB: a string on the wire, e.g. `"true"`.
    pub water_level: String,
    pub is_priming: bool,
    pub settings: DevSettingsBlock,
    pub cover_version: String,
    pub hub_version: String,
    pub free_sleep: FreeSleepInfo,
    pub wifi_strength: i32,
}

impl SideStatus {
    fn default_side() -> Self {
        SideStatus {
            current_temperature_level: 0.0,
            current_temperature_f: 82.5,
            target_temperature_f: 82,
            seconds_remaining: 0,
            is_on: false,
            is_alarm_vibrating: false,
            taps: Some(Taps {
                double_tap: 0,
                triple_tap: 0,
                quad_tap: 0,
            }),
        }
    }
}

impl Default for DeviceStatus {
    fn default() -> Self {
        DeviceStatus {
            left: SideStatus::default_side(),
            right: SideStatus::default_side(),
            water_level: "true".to_string(),
            is_priming: false,
            settings: DevSettingsBlock {
                v: 1,
                gain_left: 1.0,
                gain_right: 1.0,
                led_brightness: 100,
            },
            cover_version: "Pod 3".to_string(),
            // "Version not found" / 0 are the UI's hide-the-chip sentinels;
            // real values come from the host-info task in the podd binary.
            hub_version: "Version not found".to_string(),
            free_sleep: FreeSleepInfo {
                version: podd_core::VERSION.to_string(),
                branch: podd_core::GIT_REV.to_string(),
            },
            wifi_strength: 0,
        }
    }
}

// ---------------------------------------------------------------------------
// POST /api/deviceStatus (DeepPartial<DeviceStatus>, interpreted as commands)
// ---------------------------------------------------------------------------

#[derive(Deserialize, Clone, Debug, Default)]
#[serde(rename_all = "camelCase")]
pub struct SideStatusPatch {
    pub target_temperature_f: Option<TempF>,
    pub is_on: Option<bool>,
    pub is_alarm_vibrating: Option<bool>,
}

#[derive(Deserialize, Clone, Debug, Default)]
#[serde(rename_all = "camelCase")]
pub struct DeviceStatusPatch {
    pub left: Option<SideStatusPatch>,
    pub right: Option<SideStatusPatch>,
    pub is_priming: Option<bool>,
    /// CBOR device-settings block (kept opaque here; the control impl encodes it).
    pub settings: Option<serde_json::Value>,
}

// ---------------------------------------------------------------------------
// Alarm / AlarmJob
// ---------------------------------------------------------------------------

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum VibrationPattern {
    Double,
    Rise,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct AlarmJob {
    pub vibration_intensity: i32,
    pub vibration_pattern: VibrationPattern,
    pub duration: i32,
    pub side: Side,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub force: Option<bool>,
}

// ---------------------------------------------------------------------------
// Schedules
// ---------------------------------------------------------------------------

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct AlarmSchedule {
    pub vibration_intensity: i32,
    pub vibration_pattern: VibrationPattern,
    pub duration: i32,
    pub time: HhMm,
    pub enabled: bool,
    pub alarm_temperature: TempF,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct PowerBlock {
    pub on: HhMm,
    pub off: HhMm,
    pub on_temperature: TempF,
    pub enabled: bool,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct DailySchedule {
    pub temperatures: BTreeMap<HhMm, TempF>,
    pub alarm: AlarmSchedule,
    pub power: PowerBlock,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
#[serde(rename_all = "camelCase")]
pub struct SideSchedule {
    pub sunday: DailySchedule,
    pub monday: DailySchedule,
    pub tuesday: DailySchedule,
    pub wednesday: DailySchedule,
    pub thursday: DailySchedule,
    pub friday: DailySchedule,
    pub saturday: DailySchedule,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
#[serde(rename_all = "camelCase")]
pub struct Schedules {
    pub left: SideSchedule,
    pub right: SideSchedule,
}

impl Default for DailySchedule {
    fn default() -> Self {
        DailySchedule {
            temperatures: BTreeMap::new(),
            alarm: AlarmSchedule {
                vibration_intensity: 50,
                vibration_pattern: VibrationPattern::Rise,
                duration: 30,
                time: "07:00".to_string(),
                enabled: false,
                alarm_temperature: 80,
            },
            power: PowerBlock {
                on: "21:00".to_string(),
                off: "07:00".to_string(),
                on_temperature: 82,
                enabled: false,
            },
        }
    }
}

// ---------------------------------------------------------------------------
// Settings
// ---------------------------------------------------------------------------

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(tag = "type", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum TapConfig {
    Temperature {
        change: TempChange,
        amount: i32,
    },
    Alarm {
        behavior: AlarmBehavior,
        snooze_duration: i32,
        inactive_alarm_behavior: InactiveAlarmBehavior,
    },
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug)]
#[serde(rename_all = "lowercase")]
pub enum TempChange {
    Increment,
    Decrement,
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug)]
#[serde(rename_all = "lowercase")]
pub enum AlarmBehavior {
    Snooze,
    Dismiss,
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug)]
#[serde(rename_all = "lowercase")]
pub enum InactiveAlarmBehavior {
    Power,
    None,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct TapsConfig {
    pub double_tap: TapConfig,
    pub triple_tap: TapConfig,
    pub quad_tap: TapConfig,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct TemperatureScheduleOverride {
    pub disabled: bool,
    pub expires_at: String,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct AlarmOverride {
    pub disabled: bool,
    pub time_override: String,
    pub expires_at: String,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct ScheduleOverrides {
    pub temperature_schedules: TemperatureScheduleOverride,
    pub alarm: AlarmOverride,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct SideSettings {
    pub name: String,
    pub away_mode: bool,
    pub schedule_overrides: ScheduleOverrides,
    pub taps: TapsConfig,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct PrimePodDaily {
    pub enabled: bool,
    pub time: HhMm,
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug)]
#[serde(rename_all = "lowercase")]
pub enum TemperatureFormat {
    Celsius,
    Fahrenheit,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct Settings {
    pub id: String,
    pub time_zone: String,
    pub left: SideSettings,
    pub right: SideSettings,
    pub prime_pod_daily: PrimePodDaily,
    pub temperature_format: TemperatureFormat,
    pub reboot_daily: bool,
}

impl SideSettings {
    fn defaults(name: &str) -> Self {
        SideSettings {
            name: name.to_string(),
            away_mode: false,
            schedule_overrides: ScheduleOverrides {
                temperature_schedules: TemperatureScheduleOverride {
                    disabled: false,
                    expires_at: String::new(),
                },
                alarm: AlarmOverride {
                    disabled: false,
                    time_override: String::new(),
                    expires_at: String::new(),
                },
            },
            taps: TapsConfig {
                double_tap: TapConfig::Temperature {
                    change: TempChange::Decrement,
                    amount: 1,
                },
                triple_tap: TapConfig::Temperature {
                    change: TempChange::Increment,
                    amount: 1,
                },
                quad_tap: TapConfig::Alarm {
                    behavior: AlarmBehavior::Dismiss,
                    snooze_duration: 540,
                    inactive_alarm_behavior: InactiveAlarmBehavior::None,
                },
            },
        }
    }
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            id: "1".to_string(),
            time_zone: "UTC".to_string(),
            left: SideSettings::defaults("Left"),
            right: SideSettings::defaults("Right"),
            prime_pod_daily: PrimePodDaily {
                enabled: false,
                time: "14:00".to_string(),
            },
            temperature_format: TemperatureFormat::Fahrenheit,
            reboot_daily: true,
        }
    }
}

// ---------------------------------------------------------------------------
// Jobs
// ---------------------------------------------------------------------------

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum Job {
    AnalyzeSleepLeft,
    AnalyzeSleepRight,
    BiometricsCalibrationLeft,
    BiometricsCalibrationRight,
    Reboot,
    Update,
}

// ---------------------------------------------------------------------------
// Health / Services / ServerStatus
// ---------------------------------------------------------------------------

#[derive(Serialize, Deserialize, Clone, Copy, Debug)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    Failed,
    Healthy,
    NotStarted,
    Restarting,
    Retrying,
    Started,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct StatusInfo {
    pub name: String,
    pub status: Status,
    pub description: String,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timestamp: Option<String>,
}

impl StatusInfo {
    pub fn healthy(name: &str, description: &str) -> Self {
        StatusInfo {
            name: name.to_string(),
            status: Status::Healthy,
            description: description.to_string(),
            message: "OK".to_string(),
            timestamp: None,
        }
    }

    /// A service podd does not implement. Same wire shape (the SPA's zod
    /// schema is `.strict()`), honest contents: `not_started` + a message
    /// saying so, instead of the hardcoded "healthy/OK" a nonexistent stack
    /// used to report (#107).
    pub fn not_implemented(name: &str, description: &str) -> Self {
        StatusInfo {
            name: name.to_string(),
            status: Status::NotStarted,
            description: description.to_string(),
            message: "not implemented in podd".to_string(),
            timestamp: None,
        }
    }

    /// A subsystem that hasn't reported yet (podd-core not running, or the
    /// manager hasn't reached its first transition).
    pub fn not_started(name: &str, description: &str) -> Self {
        StatusInfo {
            name: name.to_string(),
            status: Status::NotStarted,
            description: description.to_string(),
            message: "no report yet".to_string(),
            timestamp: None,
        }
    }

    /// Render one [`podd_core::health::Subsystem`] entry, falling back to
    /// [`Self::not_started`] when the registry has no entry for `key`.
    pub fn from_health(
        health: &podd_core::health::HealthMap,
        key: &str,
        name: &str,
        description: &str,
    ) -> Self {
        match health.get(key) {
            Some(sub) => StatusInfo {
                name: name.to_string(),
                status: sub.health.into(),
                description: description.to_string(),
                message: sub.message.clone(),
                timestamp: Some(sub.since.to_string()),
            },
            None => Self::not_started(name, description),
        }
    }
}

impl From<podd_core::health::Health> for Status {
    fn from(h: podd_core::health::Health) -> Self {
        use podd_core::health::Health as H;
        match h {
            H::NotStarted => Status::NotStarted,
            H::Started => Status::Started,
            H::Restarting => Status::Restarting,
            H::Retrying => Status::Retrying,
            H::Failed => Status::Failed,
            H::Healthy => Status::Healthy,
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct BiometricsJobs {
    pub analyze_sleep_left: StatusInfo,
    pub analyze_sleep_right: StatusInfo,
    pub installation: StatusInfo,
    pub stream: StatusInfo,
    pub calibrate_left: StatusInfo,
    pub calibrate_right: StatusInfo,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct Biometrics {
    pub enabled: bool,
    pub jobs: BiometricsJobs,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct Services {
    pub biometrics: Biometrics,
}

impl Default for Services {
    /// The only `Services` value podd has: there is no biometrics stack here,
    /// so every job reports "not implemented" rather than the hardcoded
    /// healthy/OK free-sleep's Python layer used to justify (#107).
    fn default() -> Self {
        Services {
            biometrics: Biometrics {
                enabled: false,
                jobs: BiometricsJobs {
                    analyze_sleep_left: StatusInfo::not_implemented(
                        "analyzeSleepLeft",
                        "Sleep analysis (left)",
                    ),
                    analyze_sleep_right: StatusInfo::not_implemented(
                        "analyzeSleepRight",
                        "Sleep analysis (right)",
                    ),
                    installation: StatusInfo::not_implemented(
                        "installation",
                        "Biometrics installation",
                    ),
                    stream: StatusInfo::not_implemented("stream", "Biometrics stream"),
                    calibrate_left: StatusInfo::not_implemented(
                        "calibrateLeft",
                        "Calibration (left)",
                    ),
                    calibrate_right: StatusInfo::not_implemented(
                        "calibrateRight",
                        "Calibration (right)",
                    ),
                },
            },
        }
    }
}

/// podd's real subsystems, as reported by [`podd_core::health`].
///
/// These are podd's own components, not free-sleep's Node internals: the
/// vendored UI's `express`/`database`/`franken`/... keys described a server
/// that doesn't exist here, and every one of them was hardcoded healthy. The
/// UI iterates `Object.keys`, so it renders whatever is here.
#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct ServerStatus {
    pub api: StatusInfo,
    pub clock: StatusInfo,
    pub cover_control: StatusInfo,
    pub mqtt: StatusInfo,
    pub sensor: StatusInfo,
}

/// Human descriptions, one per subsystem. `(health key, display name,
/// description)` — the health keys come from `podd_core::health`.
const SENSOR_DESC: &str = "Sensor MCU: presence, piezo/HR, taps, alarms";
const COVER_DESC: &str = "Cover control MCU: TEC, pump, water level";
const MQTT_DESC: &str = "MQTT broker link (Home Assistant)";
const CLOCK_DESC: &str = "System clock / NTP sync (gates scheduled alarms)";
const API_DESC: &str = "This HTTP API";

impl ServerStatus {
    /// Render the live health registry. `api` is always healthy here — the
    /// handler answering at all is the proof.
    pub fn from_health(health: &podd_core::health::HealthMap) -> Self {
        use podd_core::health as h;
        ServerStatus {
            api: StatusInfo::healthy("api", API_DESC),
            clock: StatusInfo::from_health(health, h::CLOCK, "clock", CLOCK_DESC),
            cover_control: StatusInfo::from_health(
                health,
                h::COVER_CONTROL,
                "coverControl",
                COVER_DESC,
            ),
            mqtt: StatusInfo::from_health(health, h::MQTT, "mqtt", MQTT_DESC),
            sensor: StatusInfo::from_health(health, h::SENSOR, "sensor", SENSOR_DESC),
        }
    }
}

impl Default for ServerStatus {
    /// Everything podd-core owns is "not started" until it reports; only the
    /// API can vouch for itself.
    fn default() -> Self {
        Self::from_health(&podd_core::health::HealthMap::new())
    }
}

// ---------------------------------------------------------------------------
// Presence
// ---------------------------------------------------------------------------

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct SidePresence {
    pub present: bool,
    pub last_updated_at: String,
}

impl Default for SidePresence {
    fn default() -> Self {
        SidePresence {
            present: false,
            last_updated_at: jiff::Timestamp::now().to_string(),
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
#[serde(rename_all = "camelCase")]
pub struct PresenceState {
    pub left: SidePresence,
    pub right: SidePresence,
}

#[derive(Deserialize, Clone, Debug, Default)]
#[serde(rename_all = "camelCase")]
pub struct SidePresencePatch {
    pub present: bool,
}

#[derive(Deserialize, Clone, Debug, Default)]
#[serde(rename_all = "camelCase")]
pub struct PresencePatch {
    pub left: Option<SidePresencePatch>,
    pub right: Option<SidePresencePatch>,
}

// ---------------------------------------------------------------------------
// Execute
// ---------------------------------------------------------------------------

#[derive(Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct ExecuteRequest {
    pub command: String,
    #[serde(default)]
    pub arg: Option<String>,
}

#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct ExecuteResponse {
    pub success: bool,
    pub message: String,
}
