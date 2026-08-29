//! User settings: the free-sleep `settings.json` DTOs plus the pure helpers
//! that turn them into daemon behavior.
//!
//! Like the schedule DTOs (`crate::schedule`), these live in `podd-core`
//! rather than the `api` crate because both layers need them: `api` owns the
//! wire shape (it re-exports these verbatim from `api::wire`) and the control
//! core consumes them — today the daily-reboot flag, next the per-side
//! schedule overrides (#106). Serde attributes are free-sleep's (`camelCase`).
//!
//! Ownership rule (same as `schedules.json`): the api layer's `StateStore`
//! *writes* `settings.json`; podd-core only ever reads it (once at boot, then
//! live edits arrive as `Command::SetSettings`). Fields that must survive in
//! `config.ron` (prime, away mode, timezone) are additionally bridged onto the
//! config watch by their own commands — this document is the source of truth
//! only for the settings.json-native fields.

use jiff::civil::Time;
use jiff::{SignedDuration, Span};
use serde::{Deserialize, Serialize};

use crate::schedule::HhMm;

// ---------------------------------------------------------------------------
// DTOs
// ---------------------------------------------------------------------------

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum TempChange {
    Increment,
    Decrement,
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum AlarmBehavior {
    Snooze,
    Dismiss,
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum InactiveAlarmBehavior {
    Power,
    None,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
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

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct TapsConfig {
    pub double_tap: TapConfig,
    pub triple_tap: TapConfig,
    pub quad_tap: TapConfig,
}

/// Suspend a side's temperature schedule until `expires_at` (RFC 3339; empty =
/// no override). Persisted today, consumed by the control core with the alarm
/// engine (#106).
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct TemperatureScheduleOverride {
    pub disabled: bool,
    pub expires_at: String,
}

/// One-shot alarm override: skip the next alarm (`disabled`) or move it to
/// `time_override` ("HH:mm"), until `expires_at` (RFC 3339; empty = none).
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct AlarmOverride {
    pub disabled: bool,
    pub time_override: String,
    pub expires_at: String,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ScheduleOverrides {
    pub temperature_schedules: TemperatureScheduleOverride,
    pub alarm: AlarmOverride,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct SideSettings {
    pub name: String,
    pub away_mode: bool,
    pub schedule_overrides: ScheduleOverrides,
    pub taps: TapsConfig,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct PrimePodDaily {
    pub enabled: bool,
    pub time: HhMm,
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum TemperatureFormat {
    Celsius,
    Fahrenheit,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
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
            // free-sleep's default. The daemon-side reboot scheduler and the
            // api's `GET /settings` must agree on what a missing file means,
            // or the UI would show a toggle state the bed doesn't follow.
            reboot_daily: true,
        }
    }
}

// ---------------------------------------------------------------------------
// daily-reboot helpers
// ---------------------------------------------------------------------------

/// When the daily reboot fires: one hour before the daily prime time
/// (free-sleep's rule, quoted verbatim in the UI's "Reboot once a day" copy).
/// Civil-time arithmetic wraps at midnight, so a 00:30 prime reboots at 23:30.
pub fn reboot_time(prime: Time) -> Time {
    prime - Span::new().hours(1)
}

/// True when `now` is within 30 s (either side) of `at` on the wrapping 24 h
/// clock. Mirrors the frozen manager's prime window: a plain
/// `duration_until(..).abs()` reads ~24 h for a target just across midnight.
pub fn in_daily_window(now: Time, at: Time) -> bool {
    let d = now.duration_until(at).abs();
    d.min(SignedDuration::from_hours(24) - d) < SignedDuration::from_secs(30)
}

// ---------------------------------------------------------------------------
// tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn t(h: i8, m: i8) -> Time {
        Time::new(h, m, 0, 0).unwrap()
    }

    #[test]
    fn reboot_is_an_hour_before_prime_and_wraps() {
        assert_eq!(reboot_time(t(14, 0)), t(13, 0));
        assert_eq!(reboot_time(t(0, 30)), t(23, 30));
    }

    #[test]
    fn daily_window_is_30s_either_side_including_midnight_wrap() {
        assert!(in_daily_window(t(13, 0), t(13, 0)));
        assert!(in_daily_window(Time::new(12, 59, 31, 0).unwrap(), t(13, 0)));
        assert!(in_daily_window(Time::new(13, 0, 29, 0).unwrap(), t(13, 0)));
        assert!(!in_daily_window(Time::new(12, 59, 29, 0).unwrap(), t(13, 0)));
        assert!(!in_daily_window(Time::new(13, 0, 31, 0).unwrap(), t(13, 0)));
        // across midnight: 23:59:45 is 45 s before a 00:00:10 target
        assert!(in_daily_window(
            Time::new(23, 59, 45, 0).unwrap(),
            Time::new(0, 0, 10, 0).unwrap()
        ));
    }

    #[test]
    fn the_default_document_round_trips_as_camel_case() {
        let s = Settings::default();
        let v = serde_json::to_value(&s).unwrap();
        assert_eq!(v["rebootDaily"], serde_json::json!(true));
        assert_eq!(v["primePodDaily"]["time"], serde_json::json!("14:00"));
        assert_eq!(
            v["left"]["scheduleOverrides"]["alarm"]["timeOverride"],
            serde_json::json!("")
        );
        let back: Settings = serde_json::from_value(v).unwrap();
        assert_eq!(back.reboot_daily, s.reboot_daily);
    }
}
