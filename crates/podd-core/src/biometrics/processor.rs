//! Per-side streaming vitals processor: buffers the 500 Hz piezo stream,
//! gates on piezo-derived presence, runs the heart pipeline at free-sleep's
//! cadences, and emits one [`VitalsRecord`] per side per ~minute (#12).
//!
//! Cadences and thresholds mirror free-sleep's stream path:
//! presence = peak-to-peak of the last 3 s > 200 000 raw ADC counts (10
//! consecutive absent seconds reset all state); HR every 1 s after 3 s of
//! presence (3 s window); breathing every 10 s after 30 s (30 s window);
//! HRV every 30 s after 300 s (300 s window). Acceptance gates: breathing
//! 8–20 br/min (last ≤6 averaged), HRV 8–200 ms SDNN (last ≤10 averaged),
//! HR ≤90 and inside the rolling p15/p80 band once 120 samples exist,
//! slew-limited to ±clamp(2σ, 1, 10) around the 120 s moving average.
//!
//! Deviations (flagged in the port report): podd has one piezo channel per
//! side (Pod 4), so free-sleep's asymmetric two-sensor fusion is dropped;
//! emitted records carry the *current* timestamp, and a side that produced
//! no accepted HR for 90 s emits nothing rather than repeating stale values.
//!
//! On a Pod 3 the 0x32 piezo stream's u16 samples can never exceed the
//! 200 000-count presence threshold, so the processor is an inert no-op
//! there — deliberate until Pod-3-scale thresholds are calibrated.

use super::heart;
use super::store::VitalsRecord;
use pod_proto::packet::BedSide;
use std::collections::VecDeque;

const SAMPLE_RATE: usize = 500;
const HR_WINDOW: usize = 3 * SAMPLE_RATE;
const BREATH_WINDOW: usize = 30 * SAMPLE_RATE;
const HRV_WINDOW: usize = 300 * SAMPLE_RATE;

const PRESENCE_PTP: f64 = 200_000.0;
const NO_PRESENCE_TOLERANCE: u32 = 10;

const HR_DEQUE: usize = 120;
const HR_EMIT_EVERY: u64 = 60;
const HR_EMIT_MEAN_OF: usize = 25;
/// Don't emit if no HR sample was accepted for this long (staleness guard;
/// the Python would keep republishing old values forever).
const HR_STALE_S: i64 = 90;

pub struct SideProcessor {
    side: BedSide,
    /// Rolling raw-sample history, at most [`HRV_WINDOW`] samples.
    samples: Vec<f64>,
    /// Samples accumulated since the last 1 Hz iteration.
    pending: usize,

    iteration: u64,
    present: bool,
    present_for: u32,
    not_present_for: u32,

    heart_rates: VecDeque<f64>,
    hr_moving_avg: Option<f64>,
    hr_bounds: Option<(f64, f64)>,
    hr_std_2: f64,
    hr_iterations: u64,
    last_valid_hr_unix: Option<i64>,

    breath_rates: VecDeque<f64>,
    hrv_vals: VecDeque<f64>,
}

impl SideProcessor {
    pub fn new(side: BedSide) -> Self {
        SideProcessor {
            side,
            samples: Vec::with_capacity(HRV_WINDOW + SAMPLE_RATE),
            pending: 0,
            iteration: 0,
            present: false,
            present_for: 0,
            not_present_for: 0,
            heart_rates: VecDeque::with_capacity(HR_DEQUE),
            hr_moving_avg: None,
            hr_bounds: None,
            hr_std_2: 10.0,
            hr_iterations: 0,
            last_valid_hr_unix: None,
            breath_rates: VecDeque::with_capacity(6),
            hrv_vals: VecDeque::with_capacity(10),
        }
    }

    pub fn present(&self) -> bool {
        self.present
    }

    /// Feed one packet's samples; returns a record when the ~60 s emit fires.
    pub fn push_samples<I: IntoIterator<Item = f64>>(
        &mut self,
        samples: I,
        now_unix: i64,
    ) -> Option<VitalsRecord> {
        for s in samples {
            self.samples.push(s);
            self.pending += 1;
        }
        if self.samples.len() > HRV_WINDOW {
            let excess = self.samples.len() - HRV_WINDOW;
            self.samples.drain(..excess);
        }
        let mut out = None;
        // One iteration per accumulated second of signal.
        while self.pending >= SAMPLE_RATE {
            self.pending -= SAMPLE_RATE;
            if let Some(rec) = self.iterate(now_unix) {
                out = Some(rec);
            }
        }
        out
    }

    fn window(&self, len: usize) -> Option<&[f64]> {
        (self.samples.len() >= len).then(|| &self.samples[self.samples.len() - len..])
    }

    fn iterate(&mut self, now_unix: i64) -> Option<VitalsRecord> {
        self.iteration += 1;
        let hr_win = self.window(HR_WINDOW)?;

        // Presence: peak-to-peak of the 3 s window.
        let min = hr_win.iter().copied().fold(f64::INFINITY, f64::min);
        let max = hr_win.iter().copied().fold(f64::NEG_INFINITY, f64::max);
        if max - min > PRESENCE_PTP {
            self.not_present_for = 0;
            self.present_for += 1;
            if !self.present {
                self.present = true;
                log::info!("biometrics: {} side occupied (piezo)", self.side);
            }
        } else {
            self.not_present_for += 1;
            if self.not_present_for == NO_PRESENCE_TOLERANCE && self.present {
                log::info!("biometrics: {} side vacated (piezo)", self.side);
                self.reset();
            }
            // While absent, nothing below runs (present_for stays 0 after a
            // reset; a short dropout keeps present_for and resumes).
        }
        if self.present_for <= 3 {
            return None;
        }

        // Heart rate, every second.
        self.hr_iterations += 1;
        if let Ok(m) = heart::analyze_window(self.window(HR_WINDOW).unwrap()) {
            if self.is_valid_hr(m.bpm) {
                let hr = self.slew_limit(m.bpm);
                if self.heart_rates.len() == HR_DEQUE {
                    self.heart_rates.pop_front();
                }
                self.heart_rates.push_back(hr);
                self.last_valid_hr_unix = Some(now_unix);
                self.update_hr_stats();
            }
        }

        // Breathing, every 10 s once 30 s present.
        if self.iteration.is_multiple_of(10) && self.present_for >= 30 {
            if let Some(win) = self.window(BREATH_WINDOW) {
                if let Ok(m) = heart::analyze_window(win) {
                    if let Some(hz) = heart::breathing_rate_hz(&m.rr_cor) {
                        let br = hz * 60.0;
                        if (8.0..=20.0).contains(&br) {
                            if self.breath_rates.len() == 6 {
                                self.breath_rates.pop_front();
                            }
                            self.breath_rates.push_back(br);
                        }
                    }
                }
            }
        }

        // HRV, every 30 s once 300 s present.
        if self.iteration.is_multiple_of(30) && self.present_for >= 300 {
            if let Some(win) = self.window(HRV_WINDOW) {
                if let Ok(m) = heart::analyze_window(win) {
                    if (8.0..=200.0).contains(&m.sdnn) {
                        if self.hrv_vals.len() == 10 {
                            self.hrv_vals.pop_front();
                        }
                        self.hrv_vals.push_back(m.sdnn);
                    }
                }
            }
        }

        // Emit every 60 HR iterations, if we have fresh accepted data.
        if self.hr_iterations.is_multiple_of(HR_EMIT_EVERY)
            && !self.heart_rates.is_empty()
            && self
                .last_valid_hr_unix
                .is_some_and(|t| now_unix - t <= HR_STALE_S)
        {
            let recent: Vec<f64> = self
                .heart_rates
                .iter()
                .rev()
                .take(HR_EMIT_MEAN_OF)
                .copied()
                .collect();
            let hr = super::dsp::mean(&recent);
            let deque_mean = |d: &VecDeque<f64>| {
                if d.is_empty() {
                    0.0
                } else {
                    d.iter().sum::<f64>() / d.len() as f64
                }
            };
            return Some(VitalsRecord {
                side: self.side,
                timestamp: now_unix,
                heart_rate: hr.floor() as i64,
                hrv: deque_mean(&self.hrv_vals).floor() as i64,
                breathing_rate: deque_mean(&self.breath_rates).floor() as i64,
            });
        }
        None
    }

    fn is_valid_hr(&self, bpm: f64) -> bool {
        if bpm.is_nan() || bpm > 90.0 {
            return false;
        }
        match self.hr_bounds {
            Some((lower, upper)) => lower < bpm && bpm < upper,
            None => true,
        }
    }

    fn slew_limit(&self, bpm: f64) -> f64 {
        match self.hr_moving_avg {
            Some(avg) if (bpm - avg).abs() > self.hr_std_2 => {
                if bpm > avg {
                    avg + self.hr_std_2
                } else {
                    avg - self.hr_std_2
                }
            }
            _ => bpm,
        }
    }

    /// Moving average / bounds / slew width, once the deque is full.
    fn update_hr_stats(&mut self) {
        if self.heart_rates.len() < HR_DEQUE {
            return;
        }
        let hrs: Vec<f64> = self.heart_rates.iter().copied().collect();
        let avg = super::dsp::mean(&hrs);
        self.hr_moving_avg = Some(avg);
        let mut lower = super::dsp::percentile(&hrs, 15.0);
        let mut upper = super::dsp::percentile(&hrs, 80.0);
        if upper - lower < 25.0 {
            upper = avg + 12.5;
            lower = avg - 12.5;
        }
        self.hr_bounds = Some((lower, upper));
        self.hr_std_2 = (2.0 * super::dsp::std_pop(&hrs)).clamp(1.0, 10.0);
    }

    /// Full state reset after 10 consecutive absent seconds (free-sleep
    /// behaviour: the HR history and bounds must re-accumulate).
    fn reset(&mut self) {
        self.present = false;
        self.present_for = 0;
        self.iteration = 0;
        self.hr_iterations = 0;
        self.heart_rates.clear();
        self.breath_rates.clear();
        self.hrv_vals.clear();
        self.hr_moving_avg = None;
        self.hr_bounds = None;
        self.hr_std_2 = 10.0;
        self.last_valid_hr_unix = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Same synthetic beat generator as the heart tests, one second at a time.
    fn beat_second(second: usize, bpm: f64, seed: u64) -> Vec<f64> {
        let period = 60.0 / bpm;
        let mut state = (seed + second as u64).wrapping_mul(0x9E3779B97F4A7C15) | 1;
        let mut rng = move || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            (state as f64 / u64::MAX as f64) - 0.5
        };
        (0..SAMPLE_RATE)
            .map(|i| {
                let ts = second as f64 + i as f64 / SAMPLE_RATE as f64;
                let phase = (ts / period).fract() * period;
                // amplitude comfortably above the 200k presence ptp gate
                let pulse = if phase < 0.08 {
                    (std::f64::consts::PI * phase / 0.08).sin() * 300_000.0
                } else {
                    0.0
                };
                -420_000.0 + pulse + rng() * 8_000.0
            })
            .collect()
    }

    #[test]
    fn occupied_side_emits_a_plausible_record_after_a_minute() {
        let mut p = SideProcessor::new(BedSide::Left);
        let mut emitted = Vec::new();
        for sec in 0..180 {
            if let Some(rec) = p.push_samples(beat_second(sec, 62.0, 5), 1_700_000_000 + sec as i64)
            {
                emitted.push(rec);
            }
        }
        assert!(p.present());
        assert!(
            !emitted.is_empty(),
            "3 min of occupied signal must emit records"
        );
        for rec in &emitted {
            assert_eq!(rec.side, BedSide::Left);
            assert!(
                (50..=75).contains(&rec.heart_rate),
                "hr {} out of range",
                rec.heart_rate
            );
            // breathing/hrv may be 0 (not yet accepted) but never negative
            assert!(rec.breathing_rate >= 0 && rec.hrv >= 0);
        }
    }

    #[test]
    fn empty_bed_emits_nothing_and_resets() {
        let mut p = SideProcessor::new(BedSide::Right);
        // Flat, low-ptp signal: never present, never emits.
        for sec in 0..120 {
            let flat: Vec<f64> = (0..SAMPLE_RATE).map(|i| (i % 7) as f64).collect();
            assert!(p.push_samples(flat, sec as i64).is_none());
        }
        assert!(!p.present());
    }

    #[test]
    fn vacating_resets_history() {
        let mut p = SideProcessor::new(BedSide::Left);
        for sec in 0..70 {
            p.push_samples(beat_second(sec, 60.0, 9), sec as i64);
        }
        assert!(p.present());
        for sec in 70..85 {
            let flat: Vec<f64> = vec![0.0; SAMPLE_RATE];
            p.push_samples(flat, sec as i64);
        }
        assert!(!p.present(), "10 flat seconds must clear presence");
        assert!(p.heart_rates.is_empty(), "history must reset");
    }
}
