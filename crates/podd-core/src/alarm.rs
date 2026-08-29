//! Scheduled-alarm resolution: which alarm, if any, should be vibrating on a
//! side right now.
//!
//! Two sources, mirroring the temperature path's ownership rule
//! (`docs/ARCHITECTURE.md`): a side owned by the weekly schedule (any weekday
//! row `power.enabled`) takes its alarms from `schedules.json`'s per-day alarm
//! blocks; an unowned side falls back to the legacy `config.ron` profile
//! alarm (wake − offset). On top of whichever source applies, the side's
//! one-shot override from `settings.json` (`scheduleOverrides.alarm`) can skip
//! the next alarm or move it to another time.
//!
//! Everything here is pure civil/instant math — no I/O, no MCU frames. The
//! sensor manager turns a [`ResolvedAlarm`] into a `SetAlarm` command (still
//! behind `dry_run` and the NTP-sync gate: this module decides *whether* an
//! alarm is due, never whether it is safe to actuate).
//!
//! Weekly day attribution matches the UI (`getAdjustedDayOfWeek`, the
//! "now − 12 h" rule): a row's alarm before noon rings the morning *after*
//! that row's day — Monday's row with a 07:00 alarm rings Tuesday 07:00 —
//! while an alarm at noon or later rings on the row's own day.

use jiff::civil::{Time, Weekday};
use jiff::{Span, Timestamp, Zoned};
use pod_proto::packet::BedSide;
use pod_proto::sensor::command::AlarmPattern;

use crate::config::{SideConfig, SidesConfig};
use crate::schedule::{self, SideSchedule, VibrationPattern, parse_hhmm};
use crate::settings::{AlarmOverride, Settings};

/// The alarm that should be vibrating: its window plus vibration parameters.
#[derive(Clone, Debug, PartialEq)]
pub struct ResolvedAlarm {
    pub intensity: u8,
    pub duration_s: u32,
    pub pattern: AlarmPattern,
}

/// One concrete upcoming/ongoing alarm instance: a start instant + params.
#[derive(Clone, Debug)]
struct Candidate {
    start: Zoned,
    alarm: ResolvedAlarm,
}

/// Map the wire vibration pattern to a firmware pattern. The firmware has no
/// distinct "rise" ramp, so `Rise` degrades to `Single` (same mapping the API
/// layer uses for test alarms).
fn to_alarm_pattern(p: VibrationPattern) -> AlarmPattern {
    match p {
        VibrationPattern::Double => AlarmPattern::Double,
        VibrationPattern::Rise => AlarmPattern::Single,
    }
}

/// The scheduled alarm whose window contains `now`, if any.
///
/// `now` must be zoned in the *configured* timezone (weekday and civil time
/// both come from it). `weekly` is the side's `schedules.json` document,
/// `profile` the side's `config.ron` legacy config, `override_` the side's
/// `settings.json` one-shot override.
///
/// Away mode and dismissals are deliberately not considered here — the caller
/// (sensor manager) checks them, exactly as it did for the profile-only path.
pub fn resolve_alarm(
    weekly: &SideSchedule,
    profile: &SideConfig,
    override_: &AlarmOverride,
    now: &Zoned,
) -> Option<ResolvedAlarm> {
    let candidates = if schedule::side_owned(weekly) {
        weekly_candidates(weekly, now)
    } else {
        profile_candidates(profile, now)
    };

    let now_ts = now.timestamp();
    apply_override(candidates, override_)
        .into_iter()
        .find(|c| {
            let start = c.start.timestamp();
            let end = start + jiff::SignedDuration::from_secs(i64::from(c.alarm.duration_s));
            start <= now_ts && now_ts < end
        })
        .map(|c| c.alarm)
}

/// [`resolve_alarm`] for one *physical* side, honoring the profile layout:
/// Solo profiles have no per-side split — both bed sides read the weekly
/// `left` document and the left override (the UI's side concept collapses to
/// one bed, exactly as the frozen manager's temperature path maps Solo).
pub fn resolve_for_side(
    profile: &SidesConfig,
    schedules: &schedule::Schedules,
    settings: &Settings,
    side: &BedSide,
    now: &Zoned,
) -> Option<ResolvedAlarm> {
    let (weekly, override_) = match (profile, side) {
        (SidesConfig::Solo(_), _) | (SidesConfig::Couples { .. }, BedSide::Left) => {
            (&schedules.left, &settings.left.schedule_overrides.alarm)
        }
        (SidesConfig::Couples { .. }, BedSide::Right) => {
            (&schedules.right, &settings.right.schedule_overrides.alarm)
        }
    };
    resolve_alarm(weekly, profile.get_side(side), override_, now)
}

/// True when a side's `config.ron` profile alarm can never ring because the
/// weekly schedule owns the side but has no enabled alarm on any row — the
/// state a pre-weekly install lands in after adopting the weekly schedule for
/// temperatures only. Callers warn, so the silent regression is visible
/// before someone oversleeps.
pub fn profile_alarm_shadowed(weekly: &SideSchedule, profile: &SideConfig) -> bool {
    schedule::side_owned(weekly)
        && profile.alarm.is_some()
        && !Weekday::Sunday
            .cycle_forward()
            .take(7)
            .any(|wd| weekly.day(wd).alarm.enabled)
}

/// Warn (once per call) for each side whose profile alarm is shadowed — the
/// sensor manager calls this at startup and whenever the schedules change.
pub fn warn_shadowed(profile: &SidesConfig, schedules: &schedule::Schedules) {
    let sides: Vec<(&'static str, &SideSchedule, &SideConfig)> = match profile {
        SidesConfig::Solo(cfg) => vec![("bed", &schedules.left, cfg)],
        SidesConfig::Couples { left, right } => vec![
            ("left", &schedules.left, left),
            ("right", &schedules.right, right),
        ],
    };
    for (name, weekly, cfg) in sides {
        if profile_alarm_shadowed(weekly, cfg) {
            log::warn!(
                "{name}: the config.ron profile alarm is IGNORED — the weekly schedule owns \
                 this side and has no alarm enabled on any weekday. Enable one on the \
                 Schedule page if you still want to be woken (#106)."
            );
        }
    }
}

/// Weekly candidates near `now`: yesterday's and today's rows are the only
/// ones whose alarms can be ringing now (windows are ≤ 600 s, so an instance
/// attributed further back can't reach the present).
fn weekly_candidates(side: &SideSchedule, now: &Zoned) -> Vec<Candidate> {
    let today = now.date();
    let Ok(yesterday) = today.yesterday() else {
        return Vec::new();
    };
    let noon = Time::new(12, 0, 0, 0).expect("noon is a valid time");

    [yesterday, today]
        .into_iter()
        .filter_map(|row_date| {
            let day = side.day(row_date.weekday());
            let a = &day.alarm;
            // `power.enabled` is the row's master switch everywhere the UI is
            // concerned (the alarm accordion is disabled and the alarm banner
            // hidden on a powered-off day) — a disabled day must not ring.
            if !day.power.enabled || !a.enabled {
                return None;
            }
            let t = parse_hhmm(&a.time)?;
            // Before noon = the morning after the row's day (UI convention).
            let fire_date = if t < noon {
                row_date.tomorrow().ok()?
            } else {
                row_date
            };
            let start = fire_date.at(t.hour(), t.minute(), 0, 0).to_zoned(now.time_zone().clone()).ok()?;
            Some(Candidate {
                start,
                alarm: ResolvedAlarm {
                    intensity: a.vibration_intensity.clamp(0, 100) as u8,
                    duration_s: a.duration.max(0) as u32,
                    pattern: to_alarm_pattern(a.vibration_pattern),
                },
            })
        })
        .collect()
}

/// Profile candidates near `now`: the legacy daily alarm (wake − offset),
/// instantiated on yesterday's and today's civil dates so a window that wraps
/// midnight (e.g. start 23:50, 20 min duration) is still found just after
/// 00:00.
fn profile_candidates(profile: &SideConfig, now: &Zoned) -> Vec<Candidate> {
    let Some(cfg) = profile.alarm.as_ref() else {
        return Vec::new();
    };
    // Civil-time arithmetic wraps at midnight (start > wake is fine).
    let start_time = profile.wake - Span::new().seconds(i64::from(cfg.offset));
    let today = now.date();
    let Ok(yesterday) = today.yesterday() else {
        return Vec::new();
    };
    [yesterday, today]
        .into_iter()
        .filter_map(|date| {
            let start = date
                .at(start_time.hour(), start_time.minute(), start_time.second(), 0)
                .to_zoned(now.time_zone().clone())
                .ok()?;
            Some(Candidate {
                start,
                alarm: ResolvedAlarm {
                    intensity: cfg.intensity,
                    duration_s: cfg.duration,
                    pattern: cfg.pattern.clone(),
                },
            })
        })
        .collect()
}

/// Apply the side's one-shot override to the candidate set.
///
/// The override holds until `expires_at` (RFC 3339; the UI stamps it two
/// minutes past the alarm it targets). Applicability is judged against alarm
/// *starts* — not against "now" — so an override stays in force for the whole
/// window it targeted instead of un-applying mid-ring:
///
/// * `disabled`: drop every candidate whose start is before the expiry.
/// * `time_override` ("HH:mm"): the override targets ONE alarm instance (the
///   expiry is stamped two minutes past the moved time), so exactly one
///   candidate is moved — the one whose moved start lands latest before the
///   expiry (ties: earliest original start). Moving every candidate in range
///   would silently relocate a second same-day alarm out of existence.
///
/// A malformed `expires_at` or `time_override` deactivates the override — the
/// API validates them, but a hand-edited `settings.json` must not take the
/// resolver down.
fn apply_override(mut candidates: Vec<Candidate>, o: &AlarmOverride) -> Vec<Candidate> {
    let Ok(expires) = o.expires_at.parse::<Timestamp>() else {
        return candidates;
    };
    if o.disabled {
        candidates.retain(|c| c.start.timestamp() >= expires);
        return candidates;
    }
    let Some(t) = parse_hhmm(&o.time_override) else {
        return candidates;
    };
    let moved_start = |c: &Candidate| {
        c.start
            .date()
            .at(t.hour(), t.minute(), 0, 0)
            .to_zoned(c.start.time_zone().clone())
            .ok()
            .filter(|m| m.timestamp() < expires)
    };
    let target = candidates
        .iter()
        .enumerate()
        .filter_map(|(i, c)| moved_start(c).map(|m| (i, m)))
        .max_by(|(i, m), (j, n)| {
            (m.timestamp(), std::cmp::Reverse(candidates[*i].start.timestamp()))
                .cmp(&(n.timestamp(), std::cmp::Reverse(candidates[*j].start.timestamp())))
        });
    if let Some((i, moved)) = target {
        candidates[i].start = moved;
    }
    candidates
}

// ---------------------------------------------------------------------------
// tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::AlarmConfig;
    use crate::schedule::{AlarmSchedule, DailySchedule};
    use jiff::civil::date;
    use jiff::tz::TimeZone;

    fn tz() -> TimeZone {
        TimeZone::get("America/Denver").unwrap()
    }

    fn at(y: i16, m: i8, d: i8, hh: i8, mm: i8) -> Zoned {
        date(y, m, d).at(hh, mm, 0, 0).to_zoned(tz()).unwrap()
    }

    fn no_override() -> AlarmOverride {
        AlarmOverride {
            disabled: false,
            time_override: String::new(),
            expires_at: String::new(),
        }
    }

    fn weekly_alarm(time: &str) -> AlarmSchedule {
        AlarmSchedule {
            vibration_intensity: 60,
            vibration_pattern: VibrationPattern::Rise,
            duration: 120,
            time: time.to_string(),
            enabled: true,
            alarm_temperature: 80,
        }
    }

    /// A side owned by the weekly schedule with one enabled alarm on `monday`.
    fn owned_side(alarm: AlarmSchedule) -> SideSchedule {
        let mut side = SideSchedule::default();
        side.monday = DailySchedule {
            alarm,
            ..DailySchedule::default()
        };
        side.monday.power.enabled = true; // owns the side
        side
    }

    fn profile(wake: &str, alarm: Option<AlarmConfig>) -> SideConfig {
        SideConfig {
            temperatures: vec![25.0],
            sleep: "21:00".parse().unwrap(),
            wake: wake.parse().unwrap(),
            alarm,
        }
    }

    fn profile_alarm(offset: u32, duration: u32) -> AlarmConfig {
        AlarmConfig {
            pattern: AlarmPattern::Double,
            intensity: 70,
            duration,
            offset,
        }
    }

    // 2026-08-17 is a Monday.

    #[test]
    fn weekly_alarm_before_noon_rings_the_next_morning() {
        let side = owned_side(weekly_alarm("07:00"));
        let prof = profile("08:00", Some(profile_alarm(0, 600)));

        // Monday 07:00: monday's row governs *Tuesday* morning — nothing today
        // (sunday's row is disabled).
        assert_eq!(
            resolve_alarm(&side, &prof, &no_override(), &at(2026, 8, 17, 7, 1)),
            None
        );
        // Tuesday 07:01 — inside the 120 s window
        let got = resolve_alarm(&side, &prof, &no_override(), &at(2026, 8, 18, 7, 1)).unwrap();
        assert_eq!(got.intensity, 60);
        assert_eq!(got.duration_s, 120);
        assert_eq!(got.pattern, AlarmPattern::Single); // Rise degrades
        // Tuesday 07:03 — window over
        assert_eq!(
            resolve_alarm(&side, &prof, &no_override(), &at(2026, 8, 18, 7, 3)),
            None
        );
    }

    #[test]
    fn weekly_alarm_at_noon_or_later_rings_the_same_day() {
        let side = owned_side(weekly_alarm("13:00"));
        let prof = profile("08:00", None);

        assert!(
            resolve_alarm(&side, &prof, &no_override(), &at(2026, 8, 17, 13, 1)).is_some(),
            "monday 13:00 alarm rings monday"
        );
        assert_eq!(
            resolve_alarm(&side, &prof, &no_override(), &at(2026, 8, 18, 13, 1)),
            None,
            "tuesday's row is disabled"
        );
    }

    #[test]
    fn owned_side_ignores_the_profile_alarm() {
        // The weekly side has no enabled alarm, but IS owned (monday power on):
        // the profile alarm must not leak through.
        let mut side = SideSchedule::default();
        side.monday.power.enabled = true;
        let prof = profile("08:00", Some(profile_alarm(0, 600)));

        assert_eq!(
            resolve_alarm(&side, &prof, &no_override(), &at(2026, 8, 17, 8, 1)),
            None
        );
    }

    #[test]
    fn unowned_side_uses_the_profile_alarm_with_offset() {
        let side = SideSchedule::default(); // nothing enabled -> unowned
        // wake 08:00, offset 600 s -> window 07:50..08:00 (600 s duration)
        let prof = profile("08:00", Some(profile_alarm(600, 600)));

        assert!(resolve_alarm(&side, &prof, &no_override(), &at(2026, 8, 17, 7, 55)).is_some());
        assert_eq!(
            resolve_alarm(&side, &prof, &no_override(), &at(2026, 8, 17, 8, 5)),
            None
        );
        let got =
            resolve_alarm(&side, &prof, &no_override(), &at(2026, 8, 17, 7, 55)).unwrap();
        assert_eq!(got.intensity, 70);
        assert_eq!(got.pattern, AlarmPattern::Double);
    }

    #[test]
    fn profile_alarm_window_wrapping_midnight_is_found_after_midnight() {
        let side = SideSchedule::default();
        // wake 00:10, offset 20 min -> start 23:50 (yesterday), 20 min duration
        let prof = profile("00:10", Some(profile_alarm(20 * 60, 20 * 60)));

        assert!(resolve_alarm(&side, &prof, &no_override(), &at(2026, 8, 18, 0, 5)).is_some());
        assert!(resolve_alarm(&side, &prof, &no_override(), &at(2026, 8, 17, 23, 55)).is_some());
        assert_eq!(
            resolve_alarm(&side, &prof, &no_override(), &at(2026, 8, 18, 0, 15)),
            None
        );
    }

    #[test]
    fn disabled_override_skips_one_alarm_then_expires() {
        let side = owned_side(weekly_alarm("07:00"));
        let prof = profile("08:00", None);
        // Skip Tuesday's 07:00 alarm: expiry two minutes past it (UI stamps
        // the -06:00 America/Denver offset).
        let o = AlarmOverride {
            disabled: true,
            time_override: String::new(),
            expires_at: "2026-08-18T07:02:00-06:00".to_string(),
        };

        assert_eq!(
            resolve_alarm(&side, &prof, &o, &at(2026, 8, 18, 7, 1)),
            None,
            "tuesday's alarm is skipped"
        );
        // A week (well, a day short — next monday-row firing) later: expired.
        assert!(
            resolve_alarm(&side, &prof, &o, &at(2026, 8, 25, 7, 1)).is_some(),
            "the following tuesday rings again"
        );
    }

    #[test]
    fn time_override_moves_the_alarm_earlier_for_one_day() {
        let side = owned_side(weekly_alarm("07:00"));
        let prof = profile("08:00", None);
        let o = AlarmOverride {
            disabled: false,
            time_override: "06:30".to_string(),
            expires_at: "2026-08-18T06:32:00-06:00".to_string(),
        };

        assert!(
            resolve_alarm(&side, &prof, &o, &at(2026, 8, 18, 6, 31)).is_some(),
            "rings at the moved time"
        );
        assert_eq!(
            resolve_alarm(&side, &prof, &o, &at(2026, 8, 18, 7, 1)),
            None,
            "and no longer at the base time"
        );
        assert!(
            resolve_alarm(&side, &prof, &o, &at(2026, 8, 25, 7, 1)).is_some(),
            "next week the base time is back"
        );
    }

    #[test]
    fn time_override_applies_to_the_profile_path_too() {
        let side = SideSchedule::default();
        let prof = profile("08:00", Some(profile_alarm(0, 120)));
        let o = AlarmOverride {
            disabled: false,
            time_override: "07:15".to_string(),
            expires_at: "2026-08-17T07:17:00-06:00".to_string(),
        };

        assert!(resolve_alarm(&side, &prof, &o, &at(2026, 8, 17, 7, 16)).is_some());
        assert_eq!(resolve_alarm(&side, &prof, &o, &at(2026, 8, 17, 8, 1)), None);
    }

    #[test]
    fn override_keeps_holding_for_a_window_it_started() {
        // Duration 600 s, moved to 06:30, expiry 06:32: at 06:35 the window
        // that STARTED under the override is still ringing — applicability is
        // judged at the start, so it must not revert (and cancel) mid-ring.
        let side = owned_side(AlarmSchedule {
            duration: 600,
            ..weekly_alarm("07:00")
        });
        let prof = profile("08:00", None);
        let o = AlarmOverride {
            disabled: false,
            time_override: "06:30".to_string(),
            expires_at: "2026-08-18T06:32:00-06:00".to_string(),
        };

        assert!(resolve_alarm(&side, &prof, &o, &at(2026, 8, 18, 6, 35)).is_some());
    }

    #[test]
    fn garbage_override_fields_deactivate_the_override() {
        let side = owned_side(weekly_alarm("07:00"));
        let prof = profile("08:00", None);
        let o = AlarmOverride {
            disabled: true,
            time_override: "99:99".to_string(),
            expires_at: "not-a-timestamp".to_string(),
        };

        assert!(
            resolve_alarm(&side, &prof, &o, &at(2026, 8, 18, 7, 1)).is_some(),
            "unparseable expiry must not skip real alarms"
        );
    }

    #[test]
    fn powered_off_day_never_rings_even_with_alarm_enabled() {
        // Copy-to-all-days then flipping one day's power off leaves
        // alarm.enabled true but unreachable in the UI; the daemon must agree
        // with the UI that power.enabled is the row's master switch.
        let mut side = owned_side(weekly_alarm("07:00")); // monday: power+alarm on
        side.tuesday = DailySchedule {
            alarm: weekly_alarm("07:00"),
            ..DailySchedule::default()
        };
        assert!(!side.tuesday.power.enabled, "premise: tuesday power off");
        let prof = profile("08:00", None);

        // Monday's row (power on) rings Tuesday morning...
        assert!(resolve_alarm(&side, &prof, &no_override(), &at(2026, 8, 18, 7, 1)).is_some());
        // ...but Tuesday's row (power off) must not ring Wednesday morning.
        assert_eq!(
            resolve_alarm(&side, &prof, &no_override(), &at(2026, 8, 19, 7, 1)),
            None
        );
    }

    #[test]
    fn time_override_moves_only_the_alarm_it_targets() {
        // Monday row 07:00 (rings Tue 07:00) AND Tuesday row 13:00 (rings Tue
        // 13:00). Moving the morning alarm to 06:30 must not relocate the
        // afternoon alarm out of existence.
        let mut side = owned_side(weekly_alarm("07:00"));
        side.tuesday = DailySchedule {
            alarm: weekly_alarm("13:00"),
            ..DailySchedule::default()
        };
        side.tuesday.power.enabled = true;
        let prof = profile("08:00", None);
        let o = AlarmOverride {
            disabled: false,
            time_override: "06:30".to_string(),
            expires_at: "2026-08-18T06:32:00-06:00".to_string(),
        };

        assert!(
            resolve_alarm(&side, &prof, &o, &at(2026, 8, 18, 6, 31)).is_some(),
            "morning alarm rings at the moved time"
        );
        assert_eq!(
            resolve_alarm(&side, &prof, &o, &at(2026, 8, 18, 7, 1)),
            None,
            "and not at its base time"
        );
        assert!(
            resolve_alarm(&side, &prof, &o, &at(2026, 8, 18, 13, 1)).is_some(),
            "the afternoon alarm is untouched"
        );
    }

    #[test]
    fn solo_profile_maps_both_physical_sides_to_left() {
        let mut settings = Settings::default();
        // Whole-bed skip written (by the UI) to the left override only.
        settings.left.schedule_overrides.alarm = AlarmOverride {
            disabled: true,
            time_override: String::new(),
            expires_at: "2026-08-18T07:02:00-06:00".to_string(),
        };
        let schedules = schedule::Schedules {
            left: owned_side(weekly_alarm("07:00")),
            ..Default::default()
        };
        let solo = SidesConfig::Solo(profile("08:00", None));

        for side in [BedSide::Left, BedSide::Right] {
            assert_eq!(
                resolve_for_side(&solo, &schedules, &settings, &side, &at(2026, 8, 18, 7, 1)),
                None,
                "{side:?}: the skip silences the whole bed"
            );
        }
        // A couples profile keeps the sides separate: right ignores left's skip.
        let couples = SidesConfig::Couples {
            left: profile("08:00", None),
            right: profile("08:00", None),
        };
        let schedules = schedule::Schedules {
            left: owned_side(weekly_alarm("07:00")),
            right: owned_side(weekly_alarm("07:00")),
        };
        assert!(
            resolve_for_side(&couples, &schedules, &settings, &BedSide::Right, &at(2026, 8, 18, 7, 1))
                .is_some()
        );
    }

    #[test]
    fn shadowed_profile_alarm_is_detected() {
        // Owned side (monday power on), no weekly alarm enabled anywhere, and
        // a profile alarm that used to ring: shadowed.
        let mut owned_no_alarm = SideSchedule::default();
        owned_no_alarm.monday.power.enabled = true;
        let prof_with_alarm = profile("08:00", Some(profile_alarm(0, 600)));

        assert!(profile_alarm_shadowed(&owned_no_alarm, &prof_with_alarm));
        // Not shadowed when: unowned, no profile alarm, or a weekly alarm exists.
        assert!(!profile_alarm_shadowed(&SideSchedule::default(), &prof_with_alarm));
        assert!(!profile_alarm_shadowed(&owned_no_alarm, &profile("08:00", None)));
        assert!(!profile_alarm_shadowed(
            &owned_side(weekly_alarm("07:00")),
            &prof_with_alarm
        ));
    }

    #[test]
    fn disabled_weekly_alarm_never_rings() {
        let side = owned_side(AlarmSchedule {
            enabled: false,
            ..weekly_alarm("07:00")
        });
        let prof = profile("08:00", None);
        assert_eq!(
            resolve_alarm(&side, &prof, &no_override(), &at(2026, 8, 18, 7, 1)),
            None
        );
    }
}
