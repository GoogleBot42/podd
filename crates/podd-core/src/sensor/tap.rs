//! Piezo tap-gesture detection (double tap on a bed side = dismiss its alarm,
//! the stock Eight Sleep gesture).
//!
//! Works on the raw piezo sample frames the sensor MCU streams (~50 ms per
//! frame; Pod 4: i32 @ 500 Hz, Pod 3: u16 @ 1 kHz). A tap candidate is a frame
//! whose peak deviation from the DC level stands well above the noise floor —
//! the median of recent frame peaks, which shrugs off single spikes but tracks
//! sustained loudness (movement, or the alarm's own vibration) within a couple
//! of seconds. A candidate only *counts* as a tap if the following frame is
//! quiet again; that separates a knock (one loud frame) from the onset of
//! sustained loudness, so the alarm can never dismiss itself with its own
//! rumble. Thresholds are deliberately conservative: a false dismiss (alarm
//! silently stops) is worse than a missed one (tap again, harder).

use std::collections::VecDeque;
use std::time::{Duration, Instant};

use pod_proto::packet::BedSide;

/// A tap-candidate frame must exceed the noise floor by this factor.
const NOISE_MULT: f64 = 6.0;
/// The frame after a candidate must be back below this multiple of the floor
/// for the candidate to count as a tap.
const RELEASE_MULT: f64 = 3.0;
/// Frame peaks kept for the median noise floor (~2 s at 20 frames/s). Also the
/// warmup: no taps until the history is full.
const PEAK_HISTORY: usize = 40;
/// DC-level EMA weight per frame.
const DC_ALPHA: f64 = 0.2;
/// Ignore further tap candidates for this long after one.
const REFRACTORY: Duration = Duration::from_millis(200);
/// Two taps this far apart count as a double tap.
const DOUBLE_MIN_GAP: Duration = Duration::from_millis(200);
const DOUBLE_MAX_GAP: Duration = Duration::from_millis(1500);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tap {
    Single,
    Double,
}

#[derive(Debug, Default)]
pub struct TapDetector {
    left: SideDetector,
    right: SideDetector,
}

#[derive(Debug, Default)]
struct SideDetector {
    dc: f64,
    peaks: VecDeque<f64>,
    /// A loud frame awaiting its quiet-again confirmation.
    pending: Option<Instant>,
    last_tap: Option<Instant>,
    refractory_until: Option<Instant>,
}

impl TapDetector {
    /// Feed one side's samples from one stream frame. Returns a tap gesture
    /// confirmed by this frame, if any (a tap is reported one frame — ~50 ms —
    /// after the knock itself, once the signal is quiet again).
    pub fn feed(
        &mut self,
        side: &BedSide,
        samples: impl Iterator<Item = f64>,
        now: Instant,
    ) -> Option<Tap> {
        let det = match side {
            BedSide::Left => &mut self.left,
            BedSide::Right => &mut self.right,
        };
        det.feed(samples, now)
    }
}

impl SideDetector {
    fn noise_floor(&self) -> f64 {
        let mut sorted: Vec<f64> = self.peaks.iter().copied().collect();
        sorted.sort_by(|a, b| a.total_cmp(b));
        sorted.get(sorted.len() / 2).copied().unwrap_or(0.0).max(1.0)
    }

    fn feed(&mut self, samples: impl Iterator<Item = f64>, now: Instant) -> Option<Tap> {
        let mut n = 0u32;
        let mut sum = 0.0;
        let mut min = f64::INFINITY;
        let mut max = f64::NEG_INFINITY;
        for s in samples {
            n += 1;
            sum += s;
            min = min.min(s);
            max = max.max(s);
        }
        if n == 0 {
            return None;
        }
        let mean = sum / n as f64;

        if self.peaks.is_empty() {
            self.dc = mean;
            self.peaks.push_back((max - mean).max(mean - min));
            return None;
        }
        let peak = (max - self.dc).max(self.dc - min);

        let floor = self.noise_floor();
        let mut confirmed_tap_at = None;
        if let Some(candidate_at) = self.pending.take() {
            if peak < RELEASE_MULT * floor {
                confirmed_tap_at = Some(candidate_at);
            } else {
                // Still loud: onset of movement/vibration, not a knock.
                log::debug!("tap candidate rejected: sustained loudness (peak {peak:.0})");
            }
        } else if self.peaks.len() >= PEAK_HISTORY
            && peak > NOISE_MULT * floor
            && self.refractory_until.is_none_or(|t| now >= t)
        {
            log::debug!("tap candidate: peak {peak:.0} vs floor {floor:.0}");
            self.pending = Some(now);
            self.refractory_until = Some(now + REFRACTORY);
            // Candidate frames touch neither the DC level nor the peak
            // history: a spike must not skew either.
            return None;
        }

        self.dc += DC_ALPHA * (mean - self.dc);
        if self.peaks.len() >= PEAK_HISTORY {
            self.peaks.pop_front();
        }
        self.peaks.push_back(peak);

        let tapped_at = confirmed_tap_at?;
        let gesture = match self.last_tap {
            Some(prev) => {
                let gap = tapped_at.duration_since(prev);
                if gap >= DOUBLE_MIN_GAP && gap <= DOUBLE_MAX_GAP {
                    self.last_tap = None;
                    return Some(Tap::Double);
                }
                Tap::Single
            }
            None => Tap::Single,
        };
        self.last_tap = Some(tapped_at);
        Some(gesture)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FRAME: Duration = Duration::from_millis(50);

    /// A flat frame around `dc` with mild noise.
    fn quiet(dc: f64) -> Vec<f64> {
        (0..25)
            .map(|i| dc + if i % 2 == 0 { 50.0 } else { -50.0 })
            .collect()
    }

    /// A quiet frame with one sharp transient.
    fn spike(dc: f64, amp: f64) -> Vec<f64> {
        let mut f = quiet(dc);
        f[12] = dc + amp;
        f
    }

    fn warmed_up(det: &mut TapDetector, side: &BedSide, t0: Instant) -> Instant {
        let mut t = t0;
        for _ in 0..=PEAK_HISTORY {
            assert_eq!(det.feed(side, quiet(1000.0).into_iter(), t), None);
            t += FRAME;
        }
        t
    }

    /// Feed a spike then a quiet confirm frame; returns (gesture, time of the
    /// confirm frame).
    fn tap(det: &mut TapDetector, side: &BedSide, mut t: Instant) -> (Option<Tap>, Instant) {
        assert_eq!(det.feed(side, spike(1000.0, 50_000.0).into_iter(), t), None);
        t += FRAME;
        let g = det.feed(side, quiet(1000.0).into_iter(), t);
        (g, t)
    }

    #[test]
    fn double_tap_detected() {
        let mut det = TapDetector::default();
        let side = BedSide::Left;
        let t = warmed_up(&mut det, &side, Instant::now());

        let (g, mut t2) = tap(&mut det, &side, t);
        assert_eq!(g, Some(Tap::Single));

        // Second knock 300 ms after the first — inside the double-tap window.
        for _ in 0..5 {
            t2 += FRAME;
            assert_eq!(det.feed(&side, quiet(1000.0).into_iter(), t2), None);
        }
        let (g, _) = tap(&mut det, &side, t2 + FRAME);
        assert_eq!(g, Some(Tap::Double));
    }

    #[test]
    fn taps_too_far_apart_are_singles() {
        let mut det = TapDetector::default();
        let side = BedSide::Left;
        let t = warmed_up(&mut det, &side, Instant::now());

        let (g, mut t2) = tap(&mut det, &side, t);
        assert_eq!(g, Some(Tap::Single));
        for _ in 0..40 {
            t2 += FRAME;
            det.feed(&side, quiet(1000.0).into_iter(), t2);
        }
        let (g, _) = tap(&mut det, &side, t2 + FRAME);
        assert_eq!(g, Some(Tap::Single));
    }

    #[test]
    fn vibration_onset_and_rumble_are_not_taps() {
        let mut det = TapDetector::default();
        let side = BedSide::Left;
        let mut t = warmed_up(&mut det, &side, Instant::now());

        // Alarm vibration: sudden sustained high amplitude. Onset frames become
        // candidates, but the still-loud follow-ups must reject them — the
        // alarm must never dismiss itself.
        let rumble: Vec<f64> = (0..25)
            .map(|i| 1000.0 + if i % 2 == 0 { 20_000.0 } else { -20_000.0 })
            .collect();
        let mut false_taps = 0;
        for _ in 0..100 {
            if det.feed(&side, rumble.clone().into_iter(), t).is_some() {
                false_taps += 1;
            }
            t += FRAME;
        }
        assert_eq!(false_taps, 0, "vibration must never read as taps");

        // ...but a hard knock above the settled rumble still registers.
        let mut knock = rumble.clone();
        knock[12] = 1000.0 + 900_000.0;
        assert_eq!(det.feed(&side, knock.into_iter(), t), None);
        t += FRAME;
        assert_eq!(
            det.feed(&side, rumble.clone().into_iter(), t),
            Some(Tap::Single)
        );
    }

    #[test]
    fn sides_are_independent() {
        let mut det = TapDetector::default();
        let t0 = Instant::now();
        warmed_up(&mut det, &BedSide::Left, t0);
        let t = warmed_up(&mut det, &BedSide::Right, t0);

        let (g, t2) = tap(&mut det, &BedSide::Left, t);
        assert_eq!(g, Some(Tap::Single));

        // Right side saw one knock only; a Double needs both on the same side.
        let (g, _) = tap(&mut det, &BedSide::Right, t2 + Duration::from_millis(300));
        assert_eq!(g, Some(Tap::Single));
    }
}
