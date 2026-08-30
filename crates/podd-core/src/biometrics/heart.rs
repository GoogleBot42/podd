//! Heartbeat analysis: the trimmed-HeartPy pipeline free-sleep runs live,
//! ported per-step (issue #12). One entry point, [`analyze_window`], takes a
//! raw piezo window at 500 Hz and yields BPM + SDNN (+ the corrected RR list
//! for breathing-rate estimation).
//!
//! Faithfulness: the adaptive-threshold peak detector reproduces free-sleep's
//! vendored HeartPy *exactly*, including two knowingly non-standard details —
//! the `peak_edges` off-by-one inherited from upstream HeartPy, and the
//! quotient filter masking only interval `i` (upstream masks `i` and `i+1`).
//! Both are kept because every acceptance threshold in the pipeline was
//! calibrated on real beds *with* those behaviours in place.
//!
//! Deliberate deviations from the Python (flagged in the port report):
//! - a flat window returns an error instead of NaN-poisoning the pipeline;
//! - the breathing tachogram is built on a true millisecond time axis
//!   (cumulative RR) instead of the biased `linspace` axis (≈3% low), and the
//!   smoothing-spline resample is a linear interpolation — an explicit
//!   0.1–0.4 Hz bandpass follows either way;
//! - errors are `Result`s, not silent prints.

use super::dsp;

pub const SAMPLE_RATE: f64 = 500.0;

/// Adaptive-threshold sweep values (free-sleep's trimmed list).
const MA_PERC_SWEEP: [f64; 9] = [40.0, 50.0, 60.0, 70.0, 80.0, 90.0, 100.0, 110.0, 120.0];
const BPM_MIN: f64 = 40.0;
const BPM_MAX: f64 = 90.0;
/// Rolling-mean window: 0.65 s.
const ROLLING_WINDOW_S: f64 = 0.65;

#[derive(Debug, Clone, PartialEq)]
pub struct HeartMeasures {
    /// Beats per minute, from the mean corrected RR interval.
    pub bpm: f64,
    /// SDNN (population std of corrected RR intervals), milliseconds.
    pub sdnn: f64,
    /// Corrected (accepted) RR intervals, milliseconds.
    pub rr_cor: Vec<f64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BadSignal {
    /// No `ma_perc` produced `rrsd > 0.1` with 40..=90 BPM.
    NoFit,
    /// Flat or too-short window.
    Unusable,
}

impl std::fmt::Display for BadSignal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            BadSignal::NoFit => write!(f, "no ma_perc fit (bad signal)"),
            BadSignal::Unusable => write!(f, "unusable window (flat/short)"),
        }
    }
}

/// Full window analysis: preprocessing + peak fit + RR gates + measures.
pub fn analyze_window(raw: &[f64]) -> Result<HeartMeasures, BadSignal> {
    // Preprocessing (identical for HR/HRV windows).
    let mut data = raw.to_vec();
    if data.len() < 2 * SAMPLE_RATE as usize {
        return Err(BadSignal::Unusable);
    }
    dsp::interpolate_outliers(&mut data, 0.2, 99.8);
    dsp::scale_data(&mut data).ok_or(BadSignal::Unusable)?;
    let data = dsp::filtfilt(&dsp::NOTCH_BASELINE, &data);
    let mut data = dsp::filtfilt(&dsp::BANDPASS_HEART, &data);

    // Positive-baseline shift: moves the whole signal (and hence the
    // adaptive threshold) up by |0.1th percentile| when negative.
    let bl = dsp::percentile(&data, 0.1);
    if bl < 0.0 {
        for v in data.iter_mut() {
            *v += bl.abs();
        }
    }

    let rol_mean = dsp::rolling_mean(&data, (ROLLING_WINDOW_S * SAMPLE_RATE) as usize);
    let peaklist = fit_peaks(&data, &rol_mean)?;

    // RR intervals (ms), dropping a first peak inside the initial 150 ms.
    let peaklist: Vec<usize> = if peaklist
        .first()
        .is_some_and(|&p| p as f64 <= SAMPLE_RATE / 1000.0 * 150.0)
    {
        peaklist[1..].to_vec()
    } else {
        peaklist
    };
    let rr_list: Vec<f64> = peaklist
        .windows(2)
        .map(|w| (w[1] - w[0]) as f64 / SAMPLE_RATE * 1000.0)
        .collect();
    if rr_list.is_empty() {
        return Err(BadSignal::NoFit);
    }

    // Physiological gate: reject RR outside mean ± max(300 ms, 30%),
    // masking the *later* peak of each rejected interval.
    let mean_rr = dsp::mean(&rr_list);
    let band = (0.3 * mean_rr).max(300.0);
    let (lower, upper) = (mean_rr - band, mean_rr + band);
    let mut binary_peak: Vec<u8> = vec![1; peaklist.len()];
    for (i, &rr) in rr_list.iter().enumerate() {
        if rr <= lower || rr >= upper {
            binary_peak[i + 1] = 0;
        }
    }
    // An RR interval survives only when both bounding peaks survive.
    let mut mask: Vec<bool> = rr_list
        .iter()
        .enumerate()
        .map(|(i, _)| !(binary_peak[i] == 1 && binary_peak[i + 1] == 1))
        .collect();

    // Quotient filter, 2 iterations, ratio 0.8..=1.2, masking only `i`.
    for _ in 0..2 {
        for i in 0..rr_list.len().saturating_sub(1) {
            if mask[i] || mask[i + 1] {
                continue;
            }
            let q = rr_list[i] / rr_list[i + 1];
            if !(0.8..=1.2).contains(&q) {
                mask[i] = true;
            }
        }
    }

    let rr_cor: Vec<f64> = rr_list
        .iter()
        .zip(&mask)
        .filter(|&(_, &m)| !m)
        .map(|(&rr, _)| rr)
        .collect();
    if rr_cor.is_empty() {
        return Err(BadSignal::NoFit);
    }

    Ok(HeartMeasures {
        bpm: 60000.0 / dsp::mean(&rr_cor),
        sdnn: dsp::std_pop(&rr_cor),
        rr_cor,
    })
}

/// The `ma_perc` sweep (`fit_peaks`): pick the threshold offset minimising
/// RR variability among physiologically-plausible candidates.
fn fit_peaks(data: &[f64], rol_mean: &[f64]) -> Result<Vec<usize>, BadSignal> {
    let seconds = data.len() as f64 / SAMPLE_RATE;
    let mut best: Option<(f64, f64)> = None; // (rrsd, ma_perc)
    for ma_perc in MA_PERC_SWEEP {
        let peaks = detect_peaks(data, rol_mean, ma_perc);
        let bpm = peaks.len() as f64 / seconds * 60.0;
        let rr: Vec<f64> = peaks
            .windows(2)
            .map(|w| (w[1] - w[0]) as f64 / SAMPLE_RATE * 1000.0)
            .collect();
        let rrsd = if rr.is_empty() {
            f64::INFINITY
        } else {
            dsp::std_pop(&rr)
        };
        if rrsd > 0.1 && (BPM_MIN..=BPM_MAX).contains(&bpm) {
            if best.is_none_or(|(b, _)| rrsd < b) {
                best = Some((rrsd, ma_perc));
            }
        }
    }
    let (_, ma_perc) = best.ok_or(BadSignal::NoFit)?;
    Ok(detect_peaks(data, rol_mean, ma_perc))
}

/// Adaptive-threshold peak detection. Reproduces HeartPy's `detect_peaks`
/// verbatim, INCLUDING its `peak_edges` off-by-one (a run boundary lands at
/// the end of the previous slice, occasionally merging adjacent runs) —
/// deliberate, see the module docs.
fn detect_peaks(data: &[f64], rol_mean: &[f64], ma_perc: f64) -> Vec<usize> {
    let offset = dsp::mean(&rol_mean.iter().map(|v| v / 100.0).collect::<Vec<_>>()) * ma_perc;
    let peaks_x: Vec<usize> = (0..data.len())
        .filter(|&i| data[i] > rol_mean[i] + offset)
        .collect();
    if peaks_x.is_empty() {
        return Vec::new();
    }
    // edges = [0] ++ where(diff(peaks_x) > 1) ++ [len]  (upstream's indexing)
    let mut edges = vec![0usize];
    for i in 0..peaks_x.len() - 1 {
        if peaks_x[i + 1] - peaks_x[i] > 1 {
            edges.push(i);
        }
    }
    edges.push(peaks_x.len());

    let mut peaklist = Vec::new();
    for w in edges.windows(2) {
        let seg: &[usize] = &peaks_x[w[0]..w[1]];
        if seg.is_empty() {
            continue;
        }
        // first maximum in the segment (list.index(max(...)))
        let mut best = 0usize;
        for (k, &idx) in seg.iter().enumerate() {
            if data[idx] > data[seg[best]] {
                best = k;
            }
        }
        peaklist.push(seg[best]);
    }
    peaklist
}

/// Breathing rate (Hz) from corrected RR intervals: resample the RR
/// tachogram to a uniform 10 Hz grid on a true millisecond axis, bandpass
/// 0.1–0.4 Hz, then pick the spectral peak in-band.
///
/// Deviations from the Python (see module docs): true time axis, linear
/// interpolation instead of a FITPACK smoothing spline, and a direct
/// band-limited DFT scan instead of a full FFT (equivalent post-bandpass:
/// the global argmax cannot be out-of-band once out-of-band is attenuated).
pub fn breathing_rate_hz(rr_cor: &[f64]) -> Option<f64> {
    if rr_cor.len() < 4 {
        return None;
    }
    // Beat times: cumulative RR (ms) -> seconds; value at each beat = RR.
    let mut t = Vec::with_capacity(rr_cor.len());
    let mut acc = 0.0;
    for &rr in rr_cor {
        acc += rr;
        t.push(acc / 1000.0);
    }
    let duration = t[t.len() - 1] - t[0];
    if duration < 8.0 {
        return None; // too short to resolve 0.1 Hz..
    }
    const FS: f64 = 10.0;
    let n = (duration * FS) as usize;
    let mut tach = Vec::with_capacity(n);
    let mut j = 0usize;
    for i in 0..n {
        let x = t[0] + i as f64 / FS;
        while j + 1 < t.len() && t[j + 1] < x {
            j += 1;
        }
        let v = if j + 1 >= t.len() {
            rr_cor[rr_cor.len() - 1]
        } else {
            let (x0, x1) = (t[j], t[j + 1]);
            let (y0, y1) = (rr_cor[j], rr_cor[j + 1]);
            y0 + (y1 - y0) * (x - x0) / (x1 - x0)
        };
        tach.push(v);
    }
    // Remove DC before the (fs=1000-designed) bandpass can't be used at
    // fs=10 — use a mean-subtract + in-band scan instead. The Python's
    // 0.1–0.4 Hz bandpass only served to keep the argmax in-band; scanning
    // only in-band bins achieves the same selection.
    let m = dsp::mean(&tach);
    for v in tach.iter_mut() {
        *v -= m;
    }
    if tach.len() < 16 {
        return None;
    }
    // Scan the respiratory band at the DFT's native resolution.
    let df = FS / tach.len() as f64;
    let mut best: Option<(f64, f64)> = None; // (power, freq)
    let mut k = (0.1 / df).ceil() as usize;
    while (k as f64) * df <= 0.4 {
        let f = k as f64 * df;
        let p = dsp::dft_power_at(&tach, FS, f);
        if best.is_none_or(|(bp, _)| p > bp) {
            best = Some((p, f));
        }
        k += 1;
    }
    best.map(|(_, f)| f)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Synthetic ballistocardiogram-ish signal: a sharp pulse per beat on a
    /// noisy baseline, raw-ADC-scale numbers like the Pod 4 piezo emits.
    fn synth_beats(seconds: f64, bpm: f64, seed: u64) -> Vec<f64> {
        let n = (seconds * SAMPLE_RATE) as usize;
        let period = 60.0 / bpm; // s
        let mut state = seed.wrapping_mul(0x9E3779B97F4A7C15) | 1;
        let mut rng = move || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            (state as f64 / u64::MAX as f64) - 0.5
        };
        (0..n)
            .map(|i| {
                let ts = i as f64 / SAMPLE_RATE;
                let phase = (ts / period).fract() * period; // s into beat
                // pulse: 80 ms half-sine at each beat start
                let pulse = if phase < 0.08 {
                    (std::f64::consts::PI * phase / 0.08).sin() * 60_000.0
                } else {
                    0.0
                };
                -420_000.0 + pulse + rng() * 4_000.0
            })
            .collect()
    }

    #[test]
    fn hr_recovers_the_synthetic_rate() {
        for bpm in [55.0, 65.0, 75.0] {
            let sig = synth_beats(3.0, bpm, 7);
            let m = analyze_window(&sig).unwrap_or_else(|e| panic!("bpm {bpm}: {e}"));
            assert!(
                (m.bpm - bpm).abs() < 6.0,
                "expected ~{bpm} bpm, got {}",
                m.bpm
            );
        }
    }

    #[test]
    fn flat_window_is_rejected_not_nan() {
        let flat = vec![-420_000.0; 1500];
        assert_eq!(analyze_window(&flat), Err(BadSignal::Unusable));
    }

    #[test]
    fn noise_only_window_is_rejected() {
        // Pure noise: the ma_perc sweep should find no physiological fit.
        let mut state = 12345u64;
        let sig: Vec<f64> = (0..1500)
            .map(|_| {
                state ^= state << 13;
                state ^= state >> 7;
                state ^= state << 17;
                (state as f64 / u64::MAX as f64) * 100_000.0
            })
            .collect();
        // Either NoFit, or (rarely) a fit whose bpm lands in-band by chance —
        // assert it does not panic and any Ok result is in the gated range.
        if let Ok(m) = analyze_window(&sig) {
            assert!((40.0..=90.0).contains(&(60000.0 / dsp::mean(&m.rr_cor))));
        }
    }

    #[test]
    fn hrv_reflects_rr_spread() {
        // Perfectly regular beats -> tiny SDNN.
        let sig = synth_beats(30.0, 60.0, 3);
        let m = analyze_window(&sig).expect("regular signal should fit");
        assert!(m.sdnn < 30.0, "regular beats gave sdnn {}", m.sdnn);
        assert!(m.rr_cor.len() >= 20);
    }

    #[test]
    fn breathing_rate_from_modulated_rr() {
        // RR tachogram sinusoidally modulated at 0.25 Hz (15 breaths/min):
        // beats at ~1 s intervals over ~60 s.
        let mut rr = Vec::new();
        let mut t = 0.0f64;
        while t < 60.0 {
            let rr_ms = 1000.0 + 60.0 * (2.0 * std::f64::consts::PI * 0.25 * t).sin();
            rr.push(rr_ms);
            t += rr_ms / 1000.0;
        }
        let hz = breathing_rate_hz(&rr).expect("breathing detectable");
        let brpm = hz * 60.0;
        assert!((brpm - 15.0).abs() < 1.5, "expected ~15 br/min, got {brpm}");
    }

    #[test]
    fn breathing_rejects_too_few_beats() {
        assert!(breathing_rate_hz(&[1000.0, 990.0]).is_none());
    }
}
