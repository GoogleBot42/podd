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
use jiff::{SignedDuration, Span, Timestamp};
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
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, Default)]
#[serde(rename_all = "camelCase", default)]
pub struct TemperatureScheduleOverride {
    pub disabled: bool,
    pub expires_at: String,
}

impl TemperatureScheduleOverride {
    /// Is the side's temperature schedule suspended at `now`? True only while
    /// `disabled` with a parseable, unexpired `expires_at` — a malformed
    /// expiry deactivates the override, so a hand-edited settings.json can
    /// never suspend a schedule forever by accident.
    pub fn suspended_at(&self, now: Timestamp) -> bool {
        self.disabled
            && self
                .expires_at
                .parse::<Timestamp>()
                .is_ok_and(|expires| now < expires)
    }
}

/// One-shot alarm override: skip the next alarm (`disabled`) or move it to
/// `time_override` ("HH:mm"), until `expires_at` (RFC 3339; empty = none).
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, Default)]
#[serde(rename_all = "camelCase", default)]
pub struct AlarmOverride {
    pub disabled: bool,
    pub time_override: String,
    pub expires_at: String,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, Default)]
#[serde(rename_all = "camelCase", default)]
pub struct ScheduleOverrides {
    pub temperature_schedules: TemperatureScheduleOverride,
    pub alarm: AlarmOverride,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
#[serde(rename_all = "camelCase", default)]
pub struct SideSettings {
    pub name: String,
    pub away_mode: bool,
    pub schedule_overrides: ScheduleOverrides,
    pub taps: TapsConfig,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
#[serde(rename_all = "camelCase", default)]
pub struct PrimePodDaily {
    pub enabled: bool,
    pub time: HhMm,
}

impl Default for PrimePodDaily {
    fn default() -> Self {
        PrimePodDaily {
            enabled: false,
            time: "14:00".to_string(),
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub enum TemperatureFormat {
    Celsius,
    #[default]
    Fahrenheit,
}

/// Every level of the document carries `#[serde(default)]`: a settings.json
/// from an older schema (or a partially-written one) parses with defaults for
/// what it lacks instead of failing wholesale — a whole-document parse failure
/// would silently revert *everything* to defaults, including a user's alarm
/// override (#106 review).
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
#[serde(rename_all = "camelCase", default)]
pub struct Settings {
    pub id: String,
    pub time_zone: String,
    pub left: SideSettings,
    pub right: SideSettings,
    pub prime_pod_daily: PrimePodDaily,
    pub temperature_format: TemperatureFormat,
    pub reboot_daily: bool,
}

impl Default for TapsConfig {
    fn default() -> Self {
        TapsConfig {
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
        }
    }
}

/// NB: the side-agnostic default name. [`Settings::default`] renames the right
/// side; a present-but-nameless side object keeps this (cosmetic only).
impl Default for SideSettings {
    fn default() -> Self {
        SideSettings {
            name: "Left".to_string(),
            away_mode: false,
            schedule_overrides: ScheduleOverrides::default(),
            taps: TapsConfig::default(),
        }
    }
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            id: "1".to_string(),
            time_zone: "UTC".to_string(),
            left: SideSettings::default(),
            right: SideSettings {
                name: "Right".to_string(),
                ..SideSettings::default()
            },
            prime_pod_daily: PrimePodDaily::default(),
            temperature_format: TemperatureFormat::Fahrenheit,
            // Deliberately NOT free-sleep's `true`: podd installs have never
            // rebooted on a schedule (the scheduler is new), so the default
            // that preserves existing behavior is off. A file that says
            // `true` — restored from free-sleep, or via the (now working) UI
            // toggle — is an explicit opt-in and is honored.
            reboot_daily: false,
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
    fn temperature_suspension_needs_disabled_and_a_live_expiry() {
        let ov = |disabled: bool, expires_at: &str| TemperatureScheduleOverride {
            disabled,
            expires_at: expires_at.to_string(),
        };
        let now: Timestamp = "2026-08-17T12:00:00-06:00".parse().unwrap();

        assert!(ov(true, "2026-08-17T14:00:00-06:00").suspended_at(now));
        assert!(!ov(true, "2026-08-17T11:00:00-06:00").suspended_at(now), "expired");
        assert!(!ov(false, "2026-08-17T14:00:00-06:00").suspended_at(now), "not disabled");
        assert!(!ov(true, "").suspended_at(now), "empty expiry");
        assert!(!ov(true, "garbage").suspended_at(now), "malformed expiry");
    }

    #[test]
    fn the_default_document_round_trips_as_camel_case() {
        let s = Settings::default();
        let v = serde_json::to_value(&s).unwrap();
        // Opt-in only: podd never rebooted on a schedule before this existed.
        assert_eq!(v["rebootDaily"], serde_json::json!(false));
        assert_eq!(v["primePodDaily"]["time"], serde_json::json!("14:00"));
        assert_eq!(
            v["left"]["scheduleOverrides"]["alarm"]["timeOverride"],
            serde_json::json!("")
        );
        assert_eq!(v["right"]["name"], serde_json::json!("Right"));
        let back: Settings = serde_json::from_value(v).unwrap();
        assert_eq!(back, s);
    }

    /// A document from an older schema (fields missing at any level) must
    /// parse with defaults for what it lacks — a whole-document failure would
    /// silently discard a user's alarm override and reboot opt-in (#106).
    #[test]
    fn partial_documents_parse_with_defaults() {
        let partial = serde_json::json!({
            "rebootDaily": true,
            "left": { "name": "Cris", "scheduleOverrides": { "alarm": { "disabled": true } } },
        });
        let s: Settings = serde_json::from_value(partial).unwrap();
        assert!(s.reboot_daily, "explicit opt-in honored");
        assert_eq!(s.left.name, "Cris");
        assert!(s.left.schedule_overrides.alarm.disabled);
        assert_eq!(s.left.schedule_overrides.alarm.expires_at, "");
        assert!(!s.right.away_mode);
        assert_eq!(s.time_zone, "UTC");

        // and the empty document is exactly the defaults
        let empty: Settings = serde_json::from_value(serde_json::json!({})).unwrap();
        assert_eq!(empty, Settings::default());
    }
}
