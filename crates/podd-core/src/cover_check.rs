//! Cover-vs-water plausibility check — OBSERVE ONLY.
//!
//! Everything podd knows about the water comes from one probe on the Frozen
//! MCU. If that MCU hangs, or its probe reads high, the PID keeps cooling
//! while podd and the UI show the target as reached. On 2026-10-05 Jeremy
//! woke to a bed far colder than set while the UI said it was on temperature
//! and ignored new targets; a power cycle fixed it (the logs were lost).
//! The cover's six bed-zone thermistors are on a different MCU and are read
//! independently, so they can catch that disagreement.
//!
//! The water runs through the cover, so with a side on its zones should sit
//! near the water temperature (body heat only warms them). The check flags a
//! side whose *warmest* zone stays [`COLD_MARGIN`] below both the water
//! reading and the target for [`HOLD`].
//!
//! It only logs: an `ERROR` line when a side trips, and an `INFO` line with
//! every side's target / water / zones each [`REPORT_INT`] so thresholds can
//! be tuned from real nights before it is allowed to act (the first freeze
//! guard acted on an untuned threshold and false-tripped 55 times in three
//! nights). The zone-to-side split is also unverified; the report line
//! shows it against the two sides' targets.

use tokio::sync::watch;
use tokio::time::{Duration, Instant};

use crate::bus::{DeviceSnapshot, SideSnapshot};

/// Warmest zone this far below both the water reading and the target.
const COLD_MARGIN: f64 = 5.0;
/// ...continuously for this long.
const HOLD: Duration = Duration::from_secs(30 * 60);
/// Readings that have not changed for this long are treated as stale.
const STALE: Duration = Duration::from_secs(10 * 60);
const REPORT_INT: Duration = Duration::from_secs(5 * 60);
const TICK: Duration = Duration::from_secs(10);

#[derive(Debug, Default)]
struct SideCheck {
    /// Since when the side has looked implausibly cold.
    cold_since: Option<Instant>,
    tripped: bool,
    water: Option<(f64, Instant)>,
    cover: Option<([f64; 3], Instant)>,
}

/// Remember a reading and when it last changed.
fn track<T: PartialEq + Copy>(slot: &mut Option<(T, Instant)>, v: Option<T>, now: Instant) {
    *slot = match (v, *slot) {
        (None, _) => None,
        (Some(v), Some((old, at))) if old == v => Some((old, at)),
        (Some(v), _) => Some((v, now)),
    };
}

fn fresh<T: Copy>(slot: Option<(T, Instant)>, now: Instant) -> Option<T> {
    slot.filter(|(_, at)| now.duration_since(*at) <= STALE).map(|(v, _)| v)
}

impl SideCheck {
    /// Fold in one snapshot; returns a line to log when the verdict changes.
    fn observe(&mut self, name: &str, s: &SideSnapshot, now: Instant) -> Option<String> {
        track(&mut self.water, s.current_temp_c, now);
        track(&mut self.cover, s.cover_temps_c, now);
        let water = fresh(self.water, now);
        let cover = fresh(self.cover, now);

        let cold = match (s.is_on, s.target_temp_c, water, cover) {
            (true, Some(target), Some(water), Some(cover)) => {
                let warmest = cover.iter().copied().fold(f64::MIN, f64::max);
                warmest < water - COLD_MARGIN && warmest < target - COLD_MARGIN
            }
            _ => false,
        };
        if !cold {
            self.cold_since = None;
            if self.tripped {
                self.tripped = false;
                return Some(format!("Cover check [{name}]: cover and water agree again"));
            }
            return None;
        }
        let since = *self.cold_since.get_or_insert(now);
        if self.tripped || now.duration_since(since) < HOLD {
            return None;
        }
        self.tripped = true;
        Some(format!(
            "Cover check [{name}]: the cover has read over {COLD_MARGIN:.0}°C colder than both the \
             water probe and the target for {} min — the Frozen MCU or its water probe may be \
             wrong ({})",
            HOLD.as_secs() / 60,
            describe(s),
        ))
    }
}

fn describe(s: &SideSnapshot) -> String {
    let t = |v: Option<f64>| v.map_or("n/a".to_string(), |v| format!("{v:.2}"));
    let zones = s.cover_temps_c.map_or("n/a".to_string(), |z| {
        format!("{:.2}/{:.2}/{:.2}", z[0], z[1], z[2])
    });
    format!(
        "{} target {} water {} cover {}",
        if s.is_on { "on" } else { "off" },
        t(s.target_temp_c),
        t(s.current_temp_c),
        zones
    )
}

/// Run the check against the state bus forever.
pub async fn run(status: watch::Receiver<DeviceSnapshot>) {
    log::info!(
        "Cover check on (observe only): warmest zone {COLD_MARGIN:.0}°C under water and target for {} min",
        HOLD.as_secs() / 60
    );
    let mut sides = [SideCheck::default(), SideCheck::default()];
    let mut tick = tokio::time::interval(TICK);
    let mut last_report: Option<Instant> = None;
    loop {
        tick.tick().await;
        let now = Instant::now();
        let snap = status.borrow().clone();
        for (check, (name, side)) in sides
            .iter_mut()
            .zip([("Left", &snap.left), ("Right", &snap.right)])
        {
            match check.observe(name, side, now) {
                Some(line) if check.tripped => log::error!("{line}"),
                Some(line) => log::warn!("{line}"),
                None => {}
            }
        }
        if last_report.is_none_or(|at| now.duration_since(at) >= REPORT_INT) {
            last_report = Some(now);
            log::info!(
                "Cover check: left {} | right {}",
                describe(&snap.left),
                describe(&snap.right)
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MIN: Duration = Duration::from_secs(60);

    fn side(on: bool, target: f64, water: f64, cover: [f64; 3]) -> SideSnapshot {
        SideSnapshot {
            is_on: on,
            target_temp_c: Some(target),
            current_temp_c: Some(water),
            cover_temps_c: Some(cover),
            ..SideSnapshot::default()
        }
    }

    /// Feed one reading a minute for `mins` minutes, nudging the values so
    /// they never look stale; returns the lines produced.
    fn run(c: &mut SideCheck, t0: Instant, mins: u32, f: impl Fn(f64) -> SideSnapshot) -> Vec<String> {
        (0..mins)
            .filter_map(|m| c.observe("Left", &f(m as f64 * 0.001), t0 + MIN * m))
            .collect()
    }

    #[test]
    fn a_cover_tracking_the_water_never_trips() {
        let mut c = SideCheck::default();
        let lines = run(&mut c, Instant::now(), 600, |e| side(true, 27.2, 27.0 + e, [25.0, 27.5, 33.0]));
        assert!(lines.is_empty(), "{lines:?}");
    }

    #[test]
    fn a_cover_far_colder_than_water_and_target_trips_after_the_hold() {
        let mut c = SideCheck::default();
        let t0 = Instant::now();
        let lines = run(&mut c, t0, 29, |e| side(true, 27.2, 27.1 + e, [18.0 + e, 19.0, 20.0]));
        assert!(lines.is_empty());
        let lines = run(&mut c, t0 + MIN * 29, 11, |e| side(true, 27.2, 27.1 + e, [18.0 + e, 19.0, 20.0]));
        assert_eq!(lines.len(), 1, "{lines:?}");
        assert!(lines[0].contains("may be wrong"), "{}", lines[0]);
        assert!(c.tripped);
    }

    #[test]
    fn recovery_is_reported_once() {
        let mut c = SideCheck::default();
        let t0 = Instant::now();
        run(&mut c, t0, 40, |e| side(true, 27.2, 27.1 + e, [18.0 + e, 19.0, 20.0]));
        let later = t0 + MIN * 41;
        let line = c.observe("Left", &side(true, 27.2, 27.0, [26.0, 27.0, 28.0]), later);
        assert!(line.unwrap().contains("agree again"));
        assert!(c.observe("Left", &side(true, 27.2, 27.01, [26.0, 27.0, 28.0]), later + MIN).is_none());
    }

    #[test]
    fn a_side_that_is_off_or_cooling_on_purpose_is_not_flagged() {
        let mut c = SideCheck::default();
        // off: the cover drifts to room temperature
        assert!(run(&mut c, Instant::now(), 90, |e| side(false, 27.2, 27.0 + e, [18.0, 18.0, 18.0])).is_empty());
        // a deep cooling target the cover is following
        let mut c = SideCheck::default();
        assert!(run(&mut c, Instant::now(), 90, |e| side(true, 16.0, 17.0 + e, [15.0, 15.5, 16.0])).is_empty());
    }

    #[test]
    fn stale_readings_do_not_count() {
        let mut c = SideCheck::default();
        let t0 = Instant::now();
        // identical snapshot every minute: stale after 10 min, never 30 cold
        let s = side(true, 27.2, 27.1, [18.0, 19.0, 20.0]);
        let lines: Vec<_> = (0..120).filter_map(|m| c.observe("Left", &s, t0 + MIN * m)).collect();
        assert!(lines.is_empty(), "{lines:?}");
    }
}
