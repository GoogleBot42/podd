//! Thermostat / sleep-profile interpolation math.
//!
//! Extracted from opensleep's `src/frozen/profile.rs`. Kept SoC- and
//! config-agnostic: these functions operate on raw profile parameters
//! (`temperatures`, `sleep`, `wake`) rather than on the daemon's `Config`
//! types, so `pod-proto` stays free of the control-core config layer.

use jiff::{SignedDuration, Timestamp, civil::Time, tz::TimeZone};

use crate::frozen::packet::FrozenTarget;

impl FrozenTarget {
    /// Computes the wanted target for a single side, evaluated at the current
    /// wall-clock time in `timezone`. Away mode -> disabled target.
    pub fn calc_wanted(
        timezone: &TimeZone,
        away_mode: bool,
        temperatures: &[f32],
        sleep: Time,
        wake: Time,
    ) -> Self {
        if away_mode {
            // disabled
            return FrozenTarget::default();
        }

        let now = Timestamp::now().to_zoned(timezone.clone()).time();
        calc_target(temperatures, sleep, wake, now)
    }
}

/// Resolves the target for `now` given a side's profile.
pub fn calc_target(temperatures: &[f32], sleep: Time, wake: Time, now: Time) -> FrozenTarget {
    if !temperatures.is_empty()
        && let Some(t) = calc_progress(sleep, wake, now)
    {
        return FrozenTarget {
            enabled: true,
            // NOTE: also converts celcius -> centideg celcius
            temp: lerp(temperatures, t),
        };
    }

    // disabled
    FrozenTarget::default()
}

/// Finds the current progress into the profile (0-1).
/// Returns None if not in the profile.
pub fn calc_progress(sleep: Time, wake: Time, now: Time) -> Option<f32> {
    let profile_duration = forward_duration(sleep, wake);
    let relative_progress = forward_duration(sleep, now);
    if relative_progress > profile_duration {
        None
    } else {
        Some(relative_progress.div_duration_f32(profile_duration))
    }
}

/// Linearly interpolates a temperature profile with `t` from 0.-1.
/// `temperatures` is in degrees celcius, return value is centidegrees celcius.
#[inline]
pub fn lerp(temperatures: &[f32], t: f32) -> u16 {
    assert!(
        !temperatures.is_empty(),
        "lerp called with empty `temperatures`!"
    );

    assert!((0.0..=1.0).contains(&t), "lerp called with invalid `t`!");

    let len = temperatures.len();
    if len == 1 {
        return (temperatures[0] * 100.0) as u16;
    }

    let pos = t * (len - 1) as f32;
    let lo_idx = pos as usize;
    let lo_val = temperatures[lo_idx];

    if lo_idx == len - 1 {
        // last el
        (lo_val * 100.0) as u16
    } else {
        let hi_val = temperatures[lo_idx + 1];
        let frac = pos - lo_idx as f32;
        (frac.mul_add((hi_val - lo_val) * 100.0, lo_val * 100.0)) as u16
    }
}

/// Finds the duration between two civil times, forward from A
/// Ex:
///  1. a=18:00, b=6:00 -> 12 hours
///  2. a=16:00, b=6:00 -> 14 hours
///  3. a=4:00,  b=5:00 -> 1 hour
fn forward_duration(a: Time, b: Time) -> SignedDuration {
    if b >= a {
        b.duration_until(a).abs()
    } else {
        let to_midnight = a.duration_until(Time::MAX);
        let from_midnight = Time::MIN.duration_until(b);
        to_midnight + from_midnight + SignedDuration::from_nanos(1)
    }
}

#[cfg(test)]
mod tests {
    use jiff::civil::time;

    use super::*;

    #[test]
    fn test_lerp() {
        let temps = vec![0.0, 10.0, 20.0];
        assert_eq!(lerp(&temps, 0.0), 0);
        assert_eq!(lerp(&temps, 0.25), 500);
        assert_eq!(lerp(&temps, 0.5), 1000);
        assert_eq!(lerp(&temps, 0.75), 1500);
        assert_eq!(lerp(&temps, 1.0), 2000);
    }

    #[test]
    fn test_calc_profile_progress() {
        let sleep = time(18, 0, 0, 0);
        let wake = time(6, 0, 0, 0);

        assert_eq!(calc_progress(sleep, wake, time(17, 0, 0, 0)), None);
        assert_eq!(calc_progress(sleep, wake, time(18, 0, 0, 0)), Some(0.0));
        assert_eq!(calc_progress(sleep, wake, time(21, 0, 0, 0)), Some(0.25));
        assert_eq!(calc_progress(sleep, wake, time(0, 0, 0, 0)), Some(0.5));
        assert_eq!(calc_progress(sleep, wake, time(3, 0, 0, 0)), Some(0.75));
        assert_eq!(calc_progress(sleep, wake, time(6, 0, 0, 0)), Some(1.00));
        assert_eq!(calc_progress(sleep, wake, time(7, 0, 0, 0)), None);
    }

    #[test]
    fn test_calc_target() {
        let temps = vec![0.0, 10.0, 20.0];
        let sleep = time(18, 0, 0, 0);
        let wake = time(6, 0, 0, 0);

        // outside window -> disabled
        assert_eq!(
            calc_target(&temps, sleep, wake, time(12, 0, 0, 0)),
            FrozenTarget::default()
        );
        // midpoint -> 10.0 C -> 1000 centi-C, enabled
        assert_eq!(
            calc_target(&temps, sleep, wake, time(0, 0, 0, 0)),
            FrozenTarget {
                enabled: true,
                temp: 1000
            }
        );
    }

    #[test]
    fn test_normalize() {
        let sleep_time = time(18, 0, 0, 0);
        let wake_time = time(6, 0, 0, 0);
        let now = Time::midnight();

        let wake_time_norm = Time::midnight() + forward_duration(sleep_time, wake_time);
        let now_norm = Time::midnight() + forward_duration(sleep_time, now);

        assert_eq!(wake_time_norm, time(12, 0, 0, 0));
        assert_eq!(now_norm, time(6, 0, 0, 0));
    }

    #[test]
    fn test_forward_duration() {
        assert_eq!(
            forward_duration(time(18, 0, 0, 0), time(6, 0, 0, 0)),
            SignedDuration::from_hours(12)
        );
        assert_eq!(
            forward_duration(time(16, 0, 0, 0), time(6, 0, 0, 0)),
            SignedDuration::from_hours(14)
        );
        assert_eq!(
            forward_duration(time(4, 0, 0, 0), time(5, 0, 0, 0)),
            SignedDuration::from_hours(1)
        );
    }
}
