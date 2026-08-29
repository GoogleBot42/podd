//! Per-weekday heating schedule: the free-sleep `schedules.json` DTOs plus the
//! pure resolver that turns them into a target temperature for "now".
//!
//! The DTOs live here rather than in the `api` crate because both layers need
//! them: `api` owns the wire shape (it re-exports these verbatim from
//! `api::wire`) and the control core consumes them to drive the bed. Serde
//! attributes are free-sleep's (`camelCase`); unknown-key handling is the API
//! layer's job, not the DTOs'.
//!
//! Ownership rule (see `docs/ARCHITECTURE.md`): a side is driven by its weekly
//! schedule iff *any* weekday row has `power.enabled`; enabled days heat per
//! their window and disabled days are off. With every day disabled — the
//! shipped default — callers fall back to the legacy `config.ron` profile, so
//! existing installs see no behavior change until a day is turned on.
//!
//! Alarm blocks are resolved by `crate::alarm`: an owned side's per-day alarms
//! drive the sensor manager's vibration alarms; unowned sides keep the legacy
//! `config.ron` profile alarm.

use std::collections::BTreeMap;

use jiff::civil::{Time, Weekday};
use jiff::{SignedDuration, Zoned};
use serde::{Deserialize, Serialize};

/// "HH:mm" (validated against `^([01]\d|2[0-3]):([0-5]\d)$` upstream).
pub type HhMm = String;
/// Integer °F, 55..=110 on the wire.
pub type TempF = i32;

// ---------------------------------------------------------------------------
// DTOs
// ---------------------------------------------------------------------------

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum VibrationPattern {
    Double,
    Rise,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct AlarmSchedule {
    pub vibration_intensity: i32,
    pub vibration_pattern: VibrationPattern,
    pub duration: i32,
    pub time: HhMm,
    pub enabled: bool,
    pub alarm_temperature: TempF,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct PowerBlock {
    pub on: HhMm,
    pub off: HhMm,
    pub on_temperature: TempF,
    pub enabled: bool,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct DailySchedule {
    pub temperatures: BTreeMap<HhMm, TempF>,
    pub alarm: AlarmSchedule,
    pub power: PowerBlock,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq, Eq)]
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

#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Schedules {
    pub left: SideSchedule,
    pub right: SideSchedule,
}

/// The two side keys of a `schedules.json` document, in wire order.
pub const SIDE_KEYS: [&str; 2] = ["left", "right"];

/// The seven day keys of a [`SideSchedule`], in wire order.
pub const DAY_KEYS: [&str; 7] = [
    "sunday",
    "monday",
    "tuesday",
    "wednesday",
    "thursday",
    "friday",
    "saturday",
];

impl Schedules {
    /// The two sides with their wire key. Lets callers (API validation) report
    /// offenses as `left.monday.…` without re-deriving the key names.
    pub fn sides(&self) -> [(&'static str, &SideSchedule); 2] {
        [("left", &self.left), ("right", &self.right)]
    }
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
// resolver
// ---------------------------------------------------------------------------

/// What the weekly schedule wants right now for one side.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ResolvedTarget {
    pub temp_f: TempF,
}

impl SideSchedule {
    /// The row for `weekday`. A window is owned by the day it *starts* on, so
    /// Monday 21:00→08:00 lives under `monday` even though most of it is
    /// Tuesday (matches the UI's `getAdjustedDayOfWeek`).
    pub fn day(&self, weekday: Weekday) -> &DailySchedule {
        match weekday {
            Weekday::Sunday => &self.sunday,
            Weekday::Monday => &self.monday,
            Weekday::Tuesday => &self.tuesday,
            Weekday::Wednesday => &self.wednesday,
            Weekday::Thursday => &self.thursday,
            Weekday::Friday => &self.friday,
            Weekday::Saturday => &self.saturday,
        }
    }

    /// All seven rows with their wire key, in [`DAY_KEYS`] order. Used by the
    /// API layer to validate a whole document and name each offending row.
    pub fn days(&self) -> [(&'static str, &DailySchedule); 7] {
        [
            ("sunday", &self.sunday),
            ("monday", &self.monday),
            ("tuesday", &self.tuesday),
            ("wednesday", &self.wednesday),
            ("thursday", &self.thursday),
            ("friday", &self.friday),
            ("saturday", &self.saturday),
        ]
    }
}

/// Does the weekly schedule own this side? True iff any weekday row is
/// enabled; false means the caller falls back to the `config.ron` profile.
pub fn side_owned(side: &SideSchedule) -> bool {
    Weekday::Sunday
        .cycle_forward()
        .take(7)
        .any(|wd| side.day(wd).power.enabled)
}

/// The weekly target for `now`, or `None` when no window is active (= off).
///
/// `now` must be zoned in the *configured* timezone: weekday and civil time
/// both come from it, never from UTC.
///
/// Solo profiles have no per-side split — callers map them to the `left` side
/// of [`Schedules`].
///
/// Away mode is deliberately not considered here; it is checked by the caller
/// alongside the legacy profile path.
pub fn resolve_target(side: &SideSchedule, now: &Zoned) -> Option<ResolvedTarget> {
    let time = now.time();

    // Today's row first: once `now` has reached today's `on`, today owns the
    // bed regardless of whether yesterday's window is still nominally open.
    let today = side.day(now.weekday());
    if let Some(target) = row_target(today, time, Position::SameDay) {
        return Some(target);
    }

    // Early morning: yesterday's row may still own the bed if it wraps.
    let yesterday_weekday = match now.date().yesterday() {
        Ok(d) => d.weekday(),
        // `Date::MIN` only; a clock that far back has bigger problems.
        Err(_) => return None,
    };
    row_target(side.day(yesterday_weekday), time, Position::NextDay)
}

/// Which civil day `now` falls on relative to the row being evaluated.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Position {
    /// `now` is on the row's own weekday.
    SameDay,
    /// `now` is the day *after* the row's weekday; only a wrapping window
    /// reaches here, and only before its `off`.
    NextDay,
}

/// Resolves one weekday row against `now`, or `None` if the row is disabled,
/// unparseable, or not active at `now`.
fn row_target(day: &DailySchedule, now: Time, pos: Position) -> Option<ResolvedTarget> {
    if !day.power.enabled {
        return None;
    }
    let on = parse_hhmm(&day.power.on)?;
    let off = parse_hhmm(&day.power.off)?;
    let wraps = off <= on;

    let active = match pos {
        Position::SameDay if wraps => now >= on,
        Position::SameDay => on <= now && now < off,
        Position::NextDay => wraps && now < off,
    };
    if !active {
        return None;
    }

    let window = forward_duration(on, off);
    let elapsed = forward_duration(on, now);
    Some(ResolvedTarget {
        temp_f: step_temp(day, on, window, elapsed),
    })
}

/// The step-function target: start at `power.onTemperature`, then apply every
/// `temperatures` stop whose time has passed. Free-sleep semantics — steps,
/// not the profile path's lerp.
///
/// Stops are ordered by forward-distance from `on` so post-midnight keys of a
/// wrapping window sort after the evening ones; stops at or past the end of
/// the window are ignored, as are keys that aren't "HH:mm".
fn step_temp(
    day: &DailySchedule,
    on: Time,
    window: SignedDuration,
    elapsed: SignedDuration,
) -> TempF {
    let mut stops: Vec<(SignedDuration, TempF)> = day
        .temperatures
        .iter()
        .filter_map(|(k, &temp)| {
            let dist = forward_duration(on, parse_hhmm(k)?);
            (dist < window).then_some((dist, temp))
        })
        .collect();
    stops.sort_by_key(|&(dist, _)| dist);

    // The last stop that has already passed wins.
    stops
        .iter()
        .rev()
        .find(|&&(dist, _)| dist <= elapsed)
        .map_or(day.power.on_temperature, |&(_, temp)| temp)
}

/// "HH:mm" → civil time. Unparseable keys are treated as absent: the API layer
/// rejects them long before they land, but a hand-edited `schedules.json` must
/// not panic the control core.
fn parse_hhmm(s: &str) -> Option<Time> {
    Time::strptime("%H:%M", s).ok()
}

/// Duration between two civil times, going forward from `a`.
/// Ex: a=18:00, b=06:00 -> 12h; a=04:00, b=05:00 -> 1h.
///
/// Mirrors `pod_proto::frozen::profile::forward_duration` (private there).
fn forward_duration(a: Time, b: Time) -> SignedDuration {
    if b >= a {
        a.duration_until(b)
    } else {
        let to_midnight = a.duration_until(Time::MAX);
        let from_midnight = Time::MIN.duration_until(b);
        to_midnight + from_midnight + SignedDuration::from_nanos(1)
    }
}

// ---------------------------------------------------------------------------
// tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use jiff::civil::date;
    use jiff::tz::TimeZone;

    /// A fixed non-UTC zone: weekday/civil math must come from the zoned value.
    fn tz() -> TimeZone {
        TimeZone::get("America/Denver").unwrap()
    }

    /// `now` at a civil date/time in [`tz`]. Never `Timestamp::now()`.
    fn at(y: i16, m: i8, d: i8, hh: i8, mm: i8) -> Zoned {
        date(y, m, d)
            .at(hh, mm, 0, 0)
            .to_zoned(tz())
            .expect("unambiguous civil time")
    }

    fn row(on: &str, off: &str, on_temp: TempF, stops: &[(&str, TempF)]) -> DailySchedule {
        DailySchedule {
            temperatures: stops
                .iter()
                .map(|&(k, v)| (k.to_string(), v))
                .collect::<BTreeMap<_, _>>(),
            power: PowerBlock {
                on: on.to_string(),
                off: off.to_string(),
                on_temperature: on_temp,
                enabled: true,
            },
            ..DailySchedule::default()
        }
    }

    /// A side whose only enabled row is Monday.
    fn monday_only(monday: DailySchedule) -> SideSchedule {
        SideSchedule {
            monday,
            ..Default::default()
        }
    }

    fn temp(side: &SideSchedule, now: &Zoned) -> Option<TempF> {
        resolve_target(side, now).map(|t| t.temp_f)
    }

    // 2026-08-17 is a Monday; 08-18 Tuesday, 08-19 Wednesday.

    #[test]
    fn picks_the_row_for_todays_weekday() {
        let side = SideSchedule {
            monday: row("21:00", "23:00", 70, &[]),
            tuesday: row("21:00", "23:00", 90, &[]),
            ..Default::default()
        };

        assert_eq!(temp(&side, &at(2026, 8, 17, 22, 0)), Some(70));
        assert_eq!(temp(&side, &at(2026, 8, 18, 22, 0)), Some(90));
        // Wednesday's row is the default: disabled.
        assert_eq!(temp(&side, &at(2026, 8, 19, 22, 0)), None);
    }

    #[test]
    fn wrapping_window_is_owned_by_its_start_day() {
        let side = monday_only(row("21:00", "08:00", 75, &[]));

        assert_eq!(temp(&side, &at(2026, 8, 17, 20, 59)), None, "before on");
        assert_eq!(temp(&side, &at(2026, 8, 17, 21, 0)), Some(75), "at on");
        assert_eq!(
            temp(&side, &at(2026, 8, 18, 3, 0)),
            Some(75),
            "past midnight"
        );
        assert_eq!(temp(&side, &at(2026, 8, 18, 7, 59)), Some(75), "before off");
        assert_eq!(temp(&side, &at(2026, 8, 18, 8, 0)), None, "at off");
        assert_eq!(temp(&side, &at(2026, 8, 18, 9, 0)), None, "after off");
    }

    #[test]
    fn non_wrapping_window() {
        let side = monday_only(row("13:00", "17:00", 80, &[]));

        assert_eq!(temp(&side, &at(2026, 8, 17, 12, 59)), None);
        assert_eq!(temp(&side, &at(2026, 8, 17, 13, 0)), Some(80));
        assert_eq!(temp(&side, &at(2026, 8, 17, 16, 59)), Some(80));
        assert_eq!(temp(&side, &at(2026, 8, 17, 17, 0)), None);
        // A non-wrapping row never leaks into the next day.
        assert_eq!(temp(&side, &at(2026, 8, 18, 3, 0)), None);
    }

    #[test]
    fn step_stops_including_post_midnight_keys() {
        let side = monday_only(row("21:00", "08:00", 82, &[("23:00", 76), ("06:00", 88)]));

        assert_eq!(temp(&side, &at(2026, 8, 17, 21, 0)), Some(82), "on temp");
        assert_eq!(temp(&side, &at(2026, 8, 17, 22, 59)), Some(82));
        assert_eq!(temp(&side, &at(2026, 8, 17, 23, 0)), Some(76), "first stop");
        assert_eq!(temp(&side, &at(2026, 8, 18, 2, 0)), Some(76));
        assert_eq!(temp(&side, &at(2026, 8, 18, 6, 0)), Some(88), "second stop");
        assert_eq!(temp(&side, &at(2026, 8, 18, 7, 59)), Some(88));
    }

    #[test]
    fn stops_outside_the_window_are_ignored() {
        // 12:00 is past `off` (and so past the end of the window): not a stop.
        let side = monday_only(row("21:00", "08:00", 82, &[("12:00", 60)]));

        assert_eq!(temp(&side, &at(2026, 8, 17, 23, 0)), Some(82));
        assert_eq!(temp(&side, &at(2026, 8, 18, 7, 0)), Some(82));
    }

    #[test]
    fn unparseable_stop_key_is_skipped() {
        let side = monday_only(row("21:00", "08:00", 82, &[("25:99", 60), ("23:00", 76)]));

        assert_eq!(temp(&side, &at(2026, 8, 17, 22, 0)), Some(82));
        assert_eq!(temp(&side, &at(2026, 8, 17, 23, 30)), Some(76));
    }

    #[test]
    fn unparseable_power_times_disable_the_row() {
        let side = monday_only(row("nope", "08:00", 82, &[]));

        assert_eq!(temp(&side, &at(2026, 8, 17, 22, 0)), None);
    }

    #[test]
    fn all_days_disabled_means_unowned_and_off() {
        let side = SideSchedule::default();

        assert!(!side_owned(&side));
        // The default row is 21:00→07:00, i.e. it *would* be active if enabled.
        assert_eq!(temp(&side, &at(2026, 8, 17, 22, 0)), None);
        assert_eq!(temp(&side, &at(2026, 8, 18, 3, 0)), None);
    }

    #[test]
    fn one_enabled_day_owns_the_side() {
        let side = SideSchedule {
            thursday: row("21:00", "08:00", 75, &[]),
            ..Default::default()
        };

        assert!(side_owned(&side));
    }

    #[test]
    fn disabled_day_between_enabled_days_is_off() {
        // tuesday stays the disabled default
        let side = SideSchedule {
            monday: row("21:00", "08:00", 75, &[]),
            wednesday: row("21:00", "08:00", 75, &[]),
            ..Default::default()
        };

        assert_eq!(temp(&side, &at(2026, 8, 17, 22, 0)), Some(75), "monday on");
        // Tuesday evening: Monday's window already closed at 08:00, Tuesday's
        // row is disabled.
        assert_eq!(temp(&side, &at(2026, 8, 18, 22, 0)), None, "tuesday off");
        assert_eq!(temp(&side, &at(2026, 8, 19, 3, 0)), None, "tuesday's wrap");
        assert_eq!(temp(&side, &at(2026, 8, 19, 22, 0)), Some(75), "wednesday");
    }

    #[test]
    fn todays_row_wins_over_yesterdays_still_open_wrap() {
        // Monday runs long (into Tuesday 10:00); Tuesday starts at 09:00.
        let side = SideSchedule {
            monday: row("21:00", "10:00", 70, &[]),
            tuesday: row("09:00", "12:00", 90, &[]),
            ..Default::default()
        };

        assert_eq!(
            temp(&side, &at(2026, 8, 18, 8, 0)),
            Some(70),
            "monday's wrap"
        );
        assert_eq!(
            temp(&side, &at(2026, 8, 18, 9, 30)),
            Some(90),
            "tuesday owns"
        );
    }

    #[test]
    fn day_accessor_maps_every_weekday() {
        // Each row carries its index as `onTemperature`, so a mis-wired match
        // arm shows up as the wrong number rather than as nothing at all.
        let side = SideSchedule {
            sunday: row("21:00", "22:00", 1, &[]),
            monday: row("21:00", "22:00", 2, &[]),
            tuesday: row("21:00", "22:00", 3, &[]),
            wednesday: row("21:00", "22:00", 4, &[]),
            thursday: row("21:00", "22:00", 5, &[]),
            friday: row("21:00", "22:00", 6, &[]),
            saturday: row("21:00", "22:00", 7, &[]),
        };

        let got: Vec<TempF> = Weekday::Sunday
            .cycle_forward()
            .take(7)
            .map(|wd| side.day(wd).power.on_temperature)
            .collect();
        assert_eq!(got, vec![1, 2, 3, 4, 5, 6, 7]);
    }

    #[test]
    fn day_keys_match_the_day_accessor() {
        // `days()` is what the API validator walks; it must line up with both
        // DAY_KEYS and the weekday accessor, or an offense gets reported
        // against the wrong row.
        let side = SideSchedule {
            sunday: row("21:00", "22:00", 1, &[]),
            monday: row("21:00", "22:00", 2, &[]),
            tuesday: row("21:00", "22:00", 3, &[]),
            wednesday: row("21:00", "22:00", 4, &[]),
            thursday: row("21:00", "22:00", 5, &[]),
            friday: row("21:00", "22:00", 6, &[]),
            saturday: row("21:00", "22:00", 7, &[]),
        };

        let keys: Vec<&str> = side.days().iter().map(|&(k, _)| k).collect();
        assert_eq!(keys, DAY_KEYS.to_vec());

        for (wd, (_, day)) in Weekday::Sunday.cycle_forward().take(7).zip(side.days()) {
            assert_eq!(side.day(wd).power.on_temperature, day.power.on_temperature);
        }
    }

    #[test]
    fn side_keys_match_the_sides_accessor() {
        let s = Schedules {
            left: monday_only(row("21:00", "22:00", 70, &[])),
            right: SideSchedule::default(),
        };
        let keys: Vec<&str> = s.sides().iter().map(|&(k, _)| k).collect();
        assert_eq!(keys, SIDE_KEYS.to_vec());
        assert!(side_owned(s.sides()[0].1));
        assert!(!side_owned(s.sides()[1].1));
    }

    #[test]
    fn forward_duration_wraps() {
        let t = |h, m| Time::new(h, m, 0, 0).unwrap();
        assert_eq!(
            forward_duration(t(4, 0), t(5, 0)),
            SignedDuration::from_hours(1)
        );
        assert_eq!(
            forward_duration(t(18, 0), t(6, 0)),
            SignedDuration::from_hours(12)
        );
        assert_eq!(forward_duration(t(21, 0), t(21, 0)), SignedDuration::ZERO);
    }
}
