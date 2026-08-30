//! Sleep-session and movement detection from the live sensor streams (#141).
//!
//! Ported from free-sleep's Python `biometrics/sleep_detection/` (MIT); the
//! rule set is the same, the plumbing is not. free-sleep dumps raw sensor
//! files and analyses a finished night offline with pandas; podd has no raw
//! dump, so the same rules run **streaming**, one decision per second per
//! side, and a record is written when a sleep session closes.
//!
//! Per-second presence, per side (free-sleep `detect_presence_piezo` /
//! `detect_presence_cap`):
//!
//! * piezo — mean of that second's samples; over a 10 s rolling window the
//!   max−min *range* must be >= 20 000 raw counts, and that must hold for
//!   >= 70 % of the last 10 s;
//! * capacitance — the side must read occupied for >= 90 % of the last 10 s;
//! * occupied = both (free-sleep sums the two flags and requires 2).
//!
//! Sessions (free-sleep `_get_presence_intervals` / `_identify_sleep_intervals`):
//! a presence run shorter than 60 s is ignored; runs separated by <= 15 min
//! merge into one session (each merge is one "exited bed"); a session is only
//! recorded once its accumulated presence exceeds 3 h.
//!
//! Movement (free-sleep `detect_movement`): per capacitance sample, the sum of
//! the absolute change of the side's three channels, max-pooled into 2-minute
//! buckets.
//!
//! Deviations from free-sleep, all forced by the streaming shape:
//!
//! * free-sleep drops the 2nd/98th percentile of the piezo means before
//!   thresholding (an offline outlier trim). Streaming has no future samples,
//!   so the raw per-second means are used.
//! * free-sleep derives capacitance occupancy from a calibrated per-channel
//!   z-score sum (`> 5`). podd already has a calibrated capacitance presence
//!   detector with persisted baselines (`sensor::presence`), so its debounced
//!   per-side verdict feeds the 90 %-of-10 s rolling confirm instead of a
//!   second, separately-calibrated baseline.
//! * movement is only recorded while the side reads occupied — free-sleep
//!   records the whole analysis window and lets the UI clip it.
//! * a session in progress is lost if podd restarts (the trackers are rebuilt
//!   with the sensor task); free-sleep re-derives everything from raw files.

use super::store::{JsonlStore, StoredRecord};
use pod_proto::packet::BedSide;
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;

/// Piezo samples per second (Pod 4 stream; also the Pod 3 rate).
const SAMPLE_RATE: usize = 500;

/// Rolling window for both presence rules, in seconds.
const WINDOW_S: usize = 10;
/// free-sleep `range_threshold`: min→max spread of the per-second piezo means.
const PIEZO_RANGE: f64 = 20_000.0;
/// free-sleep `threshold_percent` for the piezo rule.
const PIEZO_COVERAGE: f64 = 0.70;
/// free-sleep `threshold_percent` for the capacitance rule.
const CAP_COVERAGE: f64 = 0.90;
/// Shortest presence run that counts as being in bed.
const MIN_PRESENCE_S: i64 = 60;
/// free-sleep `max_gap_in_minutes`: out-of-bed gaps up to this merge.
const MAX_GAP_S: i64 = 15 * 60;
/// Shortest accumulated presence that gets recorded as a sleep session.
const MIN_SLEEP_S: i64 = 3 * 3600;
/// A stream gap longer than this ends the current presence run (dropout, not
/// an interval).
const MAX_STREAM_GAP_S: i64 = 60;
/// free-sleep resamples movement to `2T`, max-pooled.
const MOVEMENT_BUCKET_S: i64 = 120;

/// One detected sleep session. All timestamps are epoch seconds; the API layer
/// converts them to the ISO-8601 strings the UI's schema expects.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SleepRecord {
    /// Stable synthetic id, assigned once when the session closes: the UI
    /// keys, edits and deletes records by a number. Derived from the detected
    /// start and the side (two sessions on one side can't start in the same
    /// second) and then never changed, so editing the bed times keeps it.
    pub id: i64,
    pub side: BedSide,
    pub entered_bed_at: i64,
    pub left_bed_at: i64,
    /// Time actually in bed (sum of the presence runs), excluding the merged
    /// out-of-bed gaps.
    pub sleep_period_seconds: i64,
    pub times_exited_bed: i64,
    pub present_intervals: Vec<(i64, i64)>,
    pub not_present_intervals: Vec<(i64, i64)>,
}

impl SleepRecord {
    /// The id a session starting at `entered_bed_at` on `side` gets.
    pub fn make_id(entered_bed_at: i64, side: BedSide) -> i64 {
        entered_bed_at * 2 + matches!(side, BedSide::Right) as i64
    }

    /// Recompute the derived fields after `entered_bed_at` / `left_bed_at`
    /// were edited (the UI's "Edit sleep record" dialog), clipping the stored
    /// intervals to the new window like free-sleep's `_filter_intervals`.
    pub fn reclip(&mut self) {
        let (start, end) = (self.entered_bed_at, self.left_bed_at);
        let clip = |v: &Vec<(i64, i64)>| -> Vec<(i64, i64)> {
            v.iter()
                .filter(|(s, e)| *e > start && *s < end)
                .map(|(s, e)| ((*s).max(start), (*e).min(end)))
                .collect()
        };
        self.present_intervals = clip(&self.present_intervals);
        self.not_present_intervals = clip(&self.not_present_intervals);
        self.sleep_period_seconds = self
            .present_intervals
            .iter()
            .map(|(s, e)| (e - s).max(0))
            .sum();
        self.times_exited_bed = self.not_present_intervals.len() as i64;
    }
}

impl StoredRecord for SleepRecord {
    const LABEL: &'static str = "sleep";
    fn timestamp(&self) -> i64 {
        self.entered_bed_at
    }
    fn side(&self) -> BedSide {
        self.side
    }
}

/// One 2-minute movement bucket: the largest per-sample capacitance change
/// seen in it. `timestamp` is the bucket start, epoch seconds.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MovementRecord {
    pub side: BedSide,
    pub timestamp: i64,
    pub total_movement: i64,
}

impl StoredRecord for MovementRecord {
    const LABEL: &'static str = "movement";
    fn timestamp(&self) -> i64 {
        self.timestamp
    }
    fn side(&self) -> BedSide {
        self.side
    }
}

/// History of detected sleep sessions.
pub type SleepStore = JsonlStore<SleepRecord>;
/// History of 2-minute movement buckets.
pub type MovementStore = JsonlStore<MovementRecord>;

/// A sleep session being accumulated.
#[derive(Debug)]
struct Session {
    start: i64,
    end: i64,
    total_present: i64,
    present: Vec<(i64, i64)>,
    not_present: Vec<(i64, i64)>,
}

/// Per-side streaming sleep + movement detector.
pub struct SideTracker {
    side: BedSide,

    // piezo → per-second means
    sec_sum: f64,
    sec_samples: usize,
    means: VecDeque<f64>,
    piezo_flags: VecDeque<bool>,

    // capacitance
    cap_present: bool,
    cap_flags: VecDeque<bool>,

    // presence runs / sessions
    occupied: bool,
    last_second: Option<i64>,
    run: Option<(i64, i64)>,
    session: Option<Session>,

    // movement
    last_cap: Option<[u16; 3]>,
    bucket: Option<(i64, i64)>,
}

impl SideTracker {
    pub fn new(side: BedSide) -> Self {
        SideTracker {
            side,
            sec_sum: 0.0,
            sec_samples: 0,
            means: VecDeque::with_capacity(WINDOW_S),
            piezo_flags: VecDeque::with_capacity(WINDOW_S),
            cap_present: false,
            cap_flags: VecDeque::with_capacity(WINDOW_S),
            occupied: false,
            last_second: None,
            run: None,
            session: None,
            last_cap: None,
            bucket: None,
        }
    }

    /// Whether the side currently reads occupied (both rules satisfied).
    pub fn occupied(&self) -> bool {
        self.occupied
    }

    /// Feed one piezo packet's samples for this side. Returns the sessions
    /// that closed while consuming them (usually none).
    pub fn push_piezo<I: IntoIterator<Item = f64>>(
        &mut self,
        samples: I,
        now_unix: i64,
    ) -> Vec<SleepRecord> {
        let mut out = Vec::new();
        for s in samples {
            self.sec_sum += s;
            self.sec_samples += 1;
            if self.sec_samples == SAMPLE_RATE {
                let mean = self.sec_sum / SAMPLE_RATE as f64;
                self.sec_sum = 0.0;
                self.sec_samples = 0;
                self.tick(mean, now_unix, &mut out);
            }
        }
        out
    }

    /// Feed this side's three capacitance channels plus the calibrated
    /// presence verdict from [`crate::sensor::presence`]. Returns a movement
    /// record when a 2-minute bucket closes.
    pub fn push_cap(
        &mut self,
        channels: [u16; 3],
        present: bool,
        now_unix: i64,
    ) -> Option<MovementRecord> {
        self.cap_present = present;

        let prev = self.last_cap.replace(channels);
        // Movement is only meaningful while someone is on the side; an empty
        // bed's capacitance drift would otherwise fill the history with noise.
        if !present {
            self.bucket = None;
            return None;
        }
        let prev = prev?;
        let delta: i64 = channels
            .iter()
            .zip(prev.iter())
            .map(|(&a, &b)| (a as i64 - b as i64).abs())
            .sum();

        let bucket_start = now_unix - now_unix.rem_euclid(MOVEMENT_BUCKET_S);
        match self.bucket {
            Some((start, max)) if start == bucket_start => {
                self.bucket = Some((start, max.max(delta)));
                None
            }
            Some((start, max)) => {
                self.bucket = Some((bucket_start, delta));
                Some(MovementRecord {
                    side: self.side,
                    timestamp: start,
                    total_movement: max,
                })
            }
            None => {
                self.bucket = Some((bucket_start, delta));
                None
            }
        }
    }

    /// One second of signal: evaluate both presence rules, then advance the
    /// interval/session state machine.
    fn tick(&mut self, mean: f64, now_unix: i64, out: &mut Vec<SleepRecord>) {
        push_capped(&mut self.means, mean, WINDOW_S);
        // A partial window can't show a range yet; free-sleep's centred
        // rolling min/max is NaN there, i.e. absent.
        let piezo_flag = if self.means.len() == WINDOW_S {
            let min = self.means.iter().copied().fold(f64::INFINITY, f64::min);
            let max = self.means.iter().copied().fold(f64::NEG_INFINITY, f64::max);
            max - min >= PIEZO_RANGE
        } else {
            false
        };
        push_capped(&mut self.piezo_flags, piezo_flag, WINDOW_S);
        push_capped(&mut self.cap_flags, self.cap_present, WINDOW_S);

        let piezo_present = covered(&self.piezo_flags, PIEZO_COVERAGE);
        let cap_present = covered(&self.cap_flags, CAP_COVERAGE);
        self.occupied = piezo_present && cap_present;

        self.observe(self.occupied, now_unix, out);
    }

    /// Presence-run bookkeeping for one observed second.
    fn observe(&mut self, occupied: bool, now_unix: i64, out: &mut Vec<SleepRecord>) {
        // A stream dropout must not stretch a run across the gap.
        let gap = self
            .last_second
            .is_some_and(|last| now_unix - last > MAX_STREAM_GAP_S);
        self.last_second = Some(now_unix);
        if gap {
            out.extend(self.end_run());
        }

        // A session nobody has come back to within the merge window is over.
        // Checked before the current second opens a new run, so the record for
        // last night lands as soon as the evidence is in.
        let session_timed_out = self
            .session
            .as_ref()
            .is_some_and(|s| now_unix - s.end > MAX_GAP_S);
        if self.run.is_none() && session_timed_out {
            out.extend(self.close_session());
        }

        if occupied {
            match &mut self.run {
                Some((_, end)) => *end = now_unix,
                None => self.run = Some((now_unix, now_unix)),
            }
        } else {
            out.extend(self.end_run());
        }
    }

    /// Close the current presence run, folding it into the session if it was
    /// long enough. Returns a record if that displaced an older session.
    fn end_run(&mut self) -> Option<SleepRecord> {
        let (start, end) = self.run.take()?;
        if end - start < MIN_PRESENCE_S {
            return None; // too short to count as being in bed
        }
        match &mut self.session {
            Some(s) if start - s.end <= MAX_GAP_S => {
                s.not_present.push((s.end, start));
                s.present.push((start, end));
                s.total_present += end - start;
                s.end = end;
                None
            }
            _ => {
                let closed = self.close_session();
                self.session = Some(Session {
                    start,
                    end,
                    total_present: end - start,
                    present: vec![(start, end)],
                    not_present: Vec::new(),
                });
                closed
            }
        }
    }

    /// Finish the accumulated session, emitting a record if it is long enough.
    fn close_session(&mut self) -> Option<SleepRecord> {
        let s = self.session.take()?;
        if s.total_present <= MIN_SLEEP_S {
            log::debug!(
                "sleep: {} session {}s discarded (under the {}h minimum)",
                self.side,
                s.total_present,
                MIN_SLEEP_S / 3600
            );
            return None;
        }
        Some(SleepRecord {
            id: SleepRecord::make_id(s.start, self.side),
            side: self.side,
            entered_bed_at: s.start,
            left_bed_at: s.end,
            sleep_period_seconds: s.total_present,
            times_exited_bed: s.not_present.len() as i64,
            present_intervals: s.present,
            not_present_intervals: s.not_present,
        })
    }
}

fn push_capped<T>(q: &mut VecDeque<T>, value: T, cap: usize) {
    if q.len() == cap {
        q.pop_front();
    }
    q.push_back(value);
}

/// free-sleep's `rolling(min_periods=1).sum() >= ceil(pct * window)`: the flag
/// count over the (possibly partial) window against a fixed target.
fn covered(flags: &VecDeque<bool>, pct: f64) -> bool {
    let needed = (pct * WINDOW_S as f64).ceil() as usize;
    flags.iter().filter(|f| **f).count() >= needed
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One second of piezo samples whose mean is `mean`.
    fn flat_second(mean: f64) -> Vec<f64> {
        vec![mean; SAMPLE_RATE]
    }

    /// Drive `tracker` for `seconds` seconds starting at `t0`.
    ///
    /// `occupied` picks the signal: an occupied second's mean swings across a
    /// >= 20 000-count range (breathing), an empty one sits flat.
    fn feed(
        tracker: &mut SideTracker,
        t0: i64,
        seconds: i64,
        occupied: bool,
    ) -> (i64, Vec<SleepRecord>) {
        let mut records = Vec::new();
        for i in 0..seconds {
            let t = t0 + i;
            let mean = if occupied {
                // alternate high/low so any 10 s window spans the range
                -420_000.0 + if i % 2 == 0 { 30_000.0 } else { 0.0 }
            } else {
                -420_000.0
            };
            tracker.push_cap([1000, 1000, 1000], occupied, t);
            records.extend(tracker.push_piezo(flat_second(mean), t));
        }
        (t0 + seconds, records)
    }

    #[test]
    fn a_full_night_produces_one_record() {
        let mut t = SideTracker::new(BedSide::Left);
        let t0 = 1_800_000_000;
        let (t1, recs) = feed(&mut t, t0, 8 * 3600, true);
        assert!(recs.is_empty(), "no record while still in bed");
        assert!(t.occupied());
        // out of bed: the record lands once the 15-minute merge window lapses
        let (_, recs) = feed(&mut t, t1, 20 * 60, false);
        assert_eq!(recs.len(), 1, "one session should close");
        let rec = &recs[0];
        assert_eq!(rec.side, BedSide::Left);
        assert_eq!(rec.times_exited_bed, 0);
        // presence latches once both rolling windows are satisfied (~15 s)
        assert!((t0..t0 + 60).contains(&rec.entered_bed_at));
        // ~8 h of presence, minus the window the rules need to latch
        assert!(
            (8 * 3600 - 60..=8 * 3600).contains(&rec.sleep_period_seconds),
            "sleep_period_seconds = {}",
            rec.sleep_period_seconds
        );
        assert_eq!(rec.present_intervals.len(), 1);
        assert!(rec.not_present_intervals.is_empty());
        assert!(!t.occupied());
    }

    #[test]
    fn a_short_trip_out_of_bed_merges_and_counts_as_an_exit() {
        let mut t = SideTracker::new(BedSide::Left);
        let t0 = 1_800_000_000;
        let (t1, _) = feed(&mut t, t0, 4 * 3600, true);
        let (t2, recs) = feed(&mut t, t1, 5 * 60, false); // bathroom trip
        assert!(recs.is_empty(), "a 5-minute gap must not close the session");
        let (t3, _) = feed(&mut t, t2, 3 * 3600, true);
        let (_, recs) = feed(&mut t, t3, 20 * 60, false);
        assert_eq!(recs.len(), 1);
        let rec = &recs[0];
        assert_eq!(rec.times_exited_bed, 1);
        assert_eq!(rec.present_intervals.len(), 2);
        assert_eq!(rec.not_present_intervals.len(), 1);
        // the gap is excluded from the in-bed total but inside the window
        assert!(rec.sleep_period_seconds < rec.left_bed_at - rec.entered_bed_at);
        assert!((7 * 3600 - 120..=7 * 3600).contains(&rec.sleep_period_seconds));
    }

    #[test]
    fn a_nap_is_below_the_three_hour_minimum() {
        let mut t = SideTracker::new(BedSide::Right);
        let t0 = 1_800_000_000;
        let (t1, _) = feed(&mut t, t0, 2 * 3600, true);
        let (_, recs) = feed(&mut t, t1, 20 * 60, false);
        assert!(recs.is_empty(), "a 2 h nap must not be recorded");
    }

    #[test]
    fn two_nights_separated_by_a_day_are_two_records() {
        let mut t = SideTracker::new(BedSide::Left);
        let t0 = 1_800_000_000;
        let (t1, _) = feed(&mut t, t0, 8 * 3600, true);
        let (t2, recs) = feed(&mut t, t1, 60 * 60, false);
        assert_eq!(recs.len(), 1, "first night closes on the gap");
        let (t3, _) = feed(&mut t, t2, 8 * 3600, true);
        let (_, recs) = feed(&mut t, t3, 20 * 60, false);
        assert_eq!(recs.len(), 1, "second night closes too");
        assert!(recs[0].entered_bed_at > t2);
    }

    #[test]
    fn presence_needs_both_the_piezo_and_the_capacitance_rule() {
        // capacitance says empty: the piezo range alone must not read occupied
        let mut t = SideTracker::new(BedSide::Left);
        for i in 0..600i64 {
            t.push_cap([1000, 1000, 1000], false, i);
            let mean = -420_000.0 + if i % 2 == 0 { 30_000.0 } else { 0.0 };
            t.push_piezo(flat_second(mean), i);
        }
        assert!(!t.occupied());

        // piezo flat (nobody breathing on it): capacitance alone isn't enough
        let mut t = SideTracker::new(BedSide::Right);
        for i in 0..600i64 {
            t.push_cap([1000, 1000, 1000], true, i);
            t.push_piezo(flat_second(-420_000.0), i);
        }
        assert!(!t.occupied());
    }

    #[test]
    fn a_sub_minute_stir_is_not_a_presence_run() {
        let mut t = SideTracker::new(BedSide::Left);
        let (t1, _) = feed(&mut t, 0, 40, true);
        let (_, recs) = feed(&mut t, t1, 30 * 60, false);
        assert!(recs.is_empty());
        // and no session was opened by it
        assert!(t.session.is_none());
    }

    #[test]
    fn movement_buckets_are_two_minute_maxima() {
        let mut t = SideTracker::new(BedSide::Right);
        let t0 = 1_800_000_000; // bucket-aligned
        let mut recs = Vec::new();
        // first bucket: a big stir, then quiet
        for (i, v) in [1000u16, 1200, 1205, 1206].iter().enumerate() {
            if let Some(r) = t.push_cap([*v, *v, *v], true, t0 + i as i64) {
                recs.push(r);
            }
        }
        // crossing into the next bucket flushes the first
        if let Some(r) = t.push_cap([1206, 1206, 1206], true, t0 + MOVEMENT_BUCKET_S) {
            recs.push(r);
        }
        assert_eq!(recs.len(), 1);
        assert_eq!(recs[0].timestamp, t0);
        assert_eq!(recs[0].total_movement, 600); // 3 channels x 200
        assert_eq!(recs[0].side, BedSide::Right);
    }

    #[test]
    fn movement_is_not_recorded_for_an_empty_side() {
        let mut t = SideTracker::new(BedSide::Left);
        let t0 = 1_800_000_000;
        for i in 0..200i64 {
            let v = 1000 + (i as u16 % 3) * 50;
            assert!(t.push_cap([v, v, v], false, t0 + i).is_none());
        }
    }

    #[test]
    fn a_stream_dropout_ends_the_run_instead_of_spanning_it() {
        let mut t = SideTracker::new(BedSide::Left);
        let t0 = 1_800_000_000;
        let (t1, _) = feed(&mut t, t0, 4 * 3600, true);
        // podd was down for two hours; the next second must not extend the run
        let (t2, recs) = feed(&mut t, t1 + 2 * 3600, 4 * 3600, true);
        assert_eq!(recs.len(), 1, "the pre-dropout session closes on its own");
        assert!(recs[0].left_bed_at <= t1);
        let (_, recs) = feed(&mut t, t2, 20 * 60, false);
        assert_eq!(recs.len(), 1, "and the post-dropout session is separate");
    }

    #[test]
    fn reclip_recomputes_derived_fields() {
        let mut rec = SleepRecord {
            id: SleepRecord::make_id(1000, BedSide::Left),
            side: BedSide::Left,
            entered_bed_at: 1000,
            left_bed_at: 5000,
            sleep_period_seconds: 3800,
            times_exited_bed: 1,
            present_intervals: vec![(1000, 2000), (2200, 5000)],
            not_present_intervals: vec![(2000, 2200)],
        };
        rec.entered_bed_at = 2100;
        rec.left_bed_at = 4000;
        rec.reclip();
        assert_eq!(rec.present_intervals, vec![(2200, 4000)]);
        assert_eq!(rec.not_present_intervals, vec![(2100, 2200)]);
        assert_eq!(rec.sleep_period_seconds, 1800);
        assert_eq!(rec.times_exited_bed, 1);
    }

    #[test]
    fn ids_are_stable_and_side_unique() {
        let mk = |side| SleepRecord::make_id(1_800_000_000, side);
        assert_ne!(mk(BedSide::Left), mk(BedSide::Right));
        assert_eq!(mk(BedSide::Left), mk(BedSide::Left));

        // an edit to the bed times must not renumber the record
        let mut rec = SleepRecord {
            id: mk(BedSide::Left),
            side: BedSide::Left,
            entered_bed_at: 1_800_000_000,
            left_bed_at: 1_800_020_000,
            sleep_period_seconds: 20_000,
            times_exited_bed: 0,
            present_intervals: vec![(1_800_000_000, 1_800_020_000)],
            not_present_intervals: vec![],
        };
        rec.entered_bed_at += 3600;
        rec.reclip();
        assert_eq!(rec.id, mk(BedSide::Left));
    }
}
