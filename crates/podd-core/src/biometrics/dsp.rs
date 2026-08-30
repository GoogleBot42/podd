//! Numeric primitives for the vitals pipeline, ported from free-sleep's
//! trimmed HeartPy vendor (MIT) — see `docs/research/` and issue #12.
//!
//! Fidelity notes: everything here reproduces the *stream* pipeline's numeric
//! behaviour (numpy linear percentiles, scipy `filtfilt` with odd padding and
//! `lfilter_zi` initial conditions, the exact filter coefficients scipy
//! designs for the three fixed filters). Filter *design* is not ported — only
//! three designs are ever used, so their coefficients are hard-coded.

/// numpy-style linear-interpolation percentile (`np.percentile`, default
/// method). `q` in 0..=100. Input need not be sorted.
pub fn percentile(data: &[f64], q: f64) -> f64 {
    debug_assert!(!data.is_empty());
    let mut sorted: Vec<f64> = data.to_vec();
    sorted.sort_by(|a, b| a.total_cmp(b));
    let idx = q / 100.0 * (sorted.len() - 1) as f64;
    let lo = idx.floor() as usize;
    let hi = idx.ceil() as usize;
    let frac = idx - lo as f64;
    sorted[lo] + (sorted[hi] - sorted[lo]) * frac
}

/// Percentile-outlier interpolation (free-sleep `vitals/cleaning.py`):
/// samples outside the [0.2, 99.8] percentiles are replaced by linear
/// interpolation over the surviving samples (`np.interp` semantics: clamped
/// at the edges). A window with no survivors is clipped instead.
pub fn interpolate_outliers(data: &mut [f64], lo_q: f64, hi_q: f64) {
    if data.is_empty() {
        return;
    }
    let lo = percentile(data, lo_q);
    let hi = percentile(data, hi_q);
    let valid: Vec<usize> = (0..data.len())
        .filter(|&i| data[i] >= lo && data[i] <= hi)
        .collect();
    if valid.is_empty() {
        for v in data.iter_mut() {
            *v = v.clamp(lo, hi);
        }
        return;
    }
    let snapshot: Vec<f64> = valid.iter().map(|&i| data[i]).collect();
    for i in 0..data.len() {
        if data[i] < lo || data[i] > hi {
            data[i] = interp_at(i as f64, &valid, &snapshot);
        }
    }
}

/// `np.interp` at a single point: piecewise linear over (xs, ys), clamped
/// outside the range. `xs` (as usize indices) must be ascending.
fn interp_at(x: f64, xs: &[usize], ys: &[f64]) -> f64 {
    match xs.binary_search_by(|&p| (p as f64).total_cmp(&x)) {
        Ok(i) => ys[i],
        Err(0) => ys[0],
        Err(i) if i >= xs.len() => *ys.last().unwrap(),
        Err(i) => {
            let (x0, x1) = (xs[i - 1] as f64, xs[i] as f64);
            let (y0, y1) = (ys[i - 1], ys[i]);
            y0 + (y1 - y0) * (x - x0) / (x1 - x0)
        }
    }
}

/// Min/max rescale to [0, 1024] (`heart/preprocessing.py:scale_data`).
/// Returns `None` on a flat window — the Python divides by zero there and
/// silences the NaNs; a flat window has no heartbeat in it anyway (#12).
pub fn scale_data(data: &mut [f64]) -> Option<()> {
    let min = data.iter().copied().fold(f64::INFINITY, f64::min);
    let max = data.iter().copied().fold(f64::NEG_INFINITY, f64::max);
    let rng = max - min;
    if !(rng > 0.0) || !rng.is_finite() {
        return None;
    }
    for v in data.iter_mut() {
        *v = 1024.0 * (*v - min) / rng;
    }
    Some(())
}

/// An IIR filter as scipy emits it: `b` (numerator) and `a` (denominator,
/// `a[0] == 1`), equal lengths.
pub struct Iir<const N: usize> {
    pub b: [f64; N],
    pub a: [f64; N],
}

/// "Baseline wander removal": `iirnotch(0.05 Hz, Q=0.005, fs=500)` — in
/// effect a DC/sub-Hz killer. Coefficients verified against scipy 1.18.
pub const NOTCH_BASELINE: Iir<3> = Iir {
    b: [0.9408092961815945, -1.8816182209465784, 0.9408092961815945],
    a: [1.0, -1.8816182209465784, 0.881618592363189],
};

/// Heartbeat bandpass: `butter(2, [0.5, 20] Hz, fs=500, 'band')`.
pub const BANDPASS_HEART: Iir<5> = Iir {
    b: [
        0.01274959143607955,
        0.0,
        -0.0254991828721591,
        0.0,
        0.01274959143607955,
    ],
    a: [
        1.0,
        -3.6532501987434314,
        5.014116728581302,
        -3.0680139084424254,
        0.70714949595306,
    ],
};

/// Breathing bandpass: `butter(2, [0.1, 0.4] Hz, fs=1000, 'band')`, applied
/// to the resampled RR tachogram.
pub const BANDPASS_BREATH: Iir<5> = Iir {
    b: [
        8.8708177365062189e-07,
        0.0,
        -1.7741635473012438e-06,
        0.0,
        8.8708177365062189e-07,
    ],
    a: [
        1.0,
        -3.9973311156433815,
        5.99200005563283,
        -3.992006760126587,
        0.9973378201396293,
    ],
};

/// `scipy.signal.lfilter` (direct form II transposed) with initial
/// conditions `zi`, returning the filtered signal.
fn lfilter<const N: usize>(f: &Iir<N>, x: &[f64], zi: &[f64; N]) -> Vec<f64> {
    // state has N-1 active slots; keep an N-array for simplicity.
    let mut z = *zi;
    let mut y = Vec::with_capacity(x.len());
    for &xn in x {
        let yn = f.b[0] * xn + z[0];
        for k in 0..N - 1 {
            let znext = if k + 1 < N - 1 { z[k + 1] } else { 0.0 };
            z[k] = f.b[k + 1] * xn + znext - f.a[k + 1] * yn;
        }
        y.push(yn);
    }
    y
}

/// `scipy.signal.lfilter_zi`: steady-state initial conditions such that a
/// constant input yields a constant output from the first sample. Solves
/// `(I - A^T) zi = B` for the companion form (small n, Gaussian elimination).
fn lfilter_zi<const N: usize>(f: &Iir<N>) -> [f64; N] {
    let n = N - 1; // state dimension
    // M = I - companion(a)^T ; companion(a)[0, :] = -a[1..]/a[0],
    // companion(a)[i, i-1] = 1. Transposed: col 0 = -a[1..], subdiag -> superdiag.
    let mut m = vec![vec![0.0f64; n]; n];
    for i in 0..n {
        for j in 0..n {
            let comp_t = if j == 0 {
                -f.a[i + 1]
            } else if j == i + 1 {
                1.0
            } else {
                0.0
            };
            m[i][j] = (if i == j { 1.0 } else { 0.0 }) - comp_t;
        }
    }
    let mut rhs: Vec<f64> = (0..n).map(|i| f.b[i + 1] - f.a[i + 1] * f.b[0]).collect();
    // Gaussian elimination with partial pivoting (n <= 4).
    for col in 0..n {
        let piv = (col..n)
            .max_by(|&r1, &r2| m[r1][col].abs().total_cmp(&m[r2][col].abs()))
            .unwrap();
        m.swap(col, piv);
        rhs.swap(col, piv);
        for row in col + 1..n {
            let factor = m[row][col] / m[col][col];
            for k in col..n {
                m[row][k] -= factor * m[col][k];
            }
            rhs[row] -= factor * rhs[col];
        }
    }
    let mut zi_v = vec![0.0f64; n];
    for row in (0..n).rev() {
        let mut acc = rhs[row];
        for k in row + 1..n {
            acc -= m[row][k] * zi_v[k];
        }
        zi_v[row] = acc / m[row][row];
    }
    let mut zi = [0.0f64; N];
    zi[..n].copy_from_slice(&zi_v);
    zi
}

/// `scipy.signal.filtfilt` with the defaults the pipeline uses
/// (`method='pad'`, `padtype='odd'`, `padlen = 3 * max(len(a), len(b))`).
/// Forward-backward filtering: zero phase, squared magnitude response.
pub fn filtfilt<const N: usize>(f: &Iir<N>, x: &[f64]) -> Vec<f64> {
    let padlen = 3 * N;
    assert!(
        x.len() > padlen,
        "filtfilt input ({}) must exceed padlen ({padlen})",
        x.len()
    );
    // odd extension: 2*x[0] - x[padlen..0], x, 2*x[last] - x[n-2..n-2-padlen]
    let mut ext = Vec::with_capacity(x.len() + 2 * padlen);
    for i in (1..=padlen).rev() {
        ext.push(2.0 * x[0] - x[i]);
    }
    ext.extend_from_slice(x);
    for i in 1..=padlen {
        ext.push(2.0 * x[x.len() - 1] - x[x.len() - 1 - i]);
    }

    let zi = lfilter_zi(f);
    let scale = |zi: &[f64; N], v: f64| {
        let mut s = *zi;
        for e in s.iter_mut() {
            *e *= v;
        }
        s
    };
    let y = lfilter(f, &ext, &scale(&zi, ext[0]));
    let mut rev: Vec<f64> = y.into_iter().rev().collect();
    let y2 = lfilter(f, &rev, &scale(&zi, *rev.first().unwrap()));
    rev = y2.into_iter().rev().collect();
    rev[padlen..rev.len() - padlen].to_vec()
}

/// HeartPy's edge-padded centered rolling mean (`datautils.rolling_mean`):
/// valid-window moving average, then the first/last value replicated
/// `(N - len) / 2` times on each side and the result truncated/padded to N.
pub fn rolling_mean(data: &[f64], window: usize) -> Vec<f64> {
    debug_assert!(window >= 1 && window <= data.len());
    let mut out = Vec::with_capacity(data.len());
    // prefix sums for O(n) sliding mean
    let mut prefix = Vec::with_capacity(data.len() + 1);
    prefix.push(0.0);
    for &v in data {
        prefix.push(prefix.last().unwrap() + v);
    }
    let valid_len = data.len() - window + 1;
    let n_miss = (data.len() - valid_len) / 2;
    let mean_at = |i: usize| (prefix[i + window] - prefix[i]) / window as f64;
    for _ in 0..n_miss {
        out.push(mean_at(0));
    }
    for i in 0..valid_len {
        out.push(mean_at(i));
    }
    while out.len() < data.len() {
        out.push(mean_at(valid_len - 1));
    }
    out.truncate(data.len());
    out
}

/// Population standard deviation (numpy `std`, ddof=0).
pub fn std_pop(data: &[f64]) -> f64 {
    if data.is_empty() {
        return f64::NAN;
    }
    let mean = data.iter().sum::<f64>() / data.len() as f64;
    (data.iter().map(|v| (v - mean).powi(2)).sum::<f64>() / data.len() as f64).sqrt()
}

pub fn mean(data: &[f64]) -> f64 {
    if data.is_empty() {
        return f64::NAN;
    }
    data.iter().sum::<f64>() / data.len() as f64
}

/// Single-bin DFT magnitude² at `freq_hz` (Goertzel-style direct sum) for a
/// signal sampled at `fs`. Used to scan the respiratory band instead of a
/// full FFT — post-bandpass, the argmax can only be in-band anyway.
pub fn dft_power_at(data: &[f64], fs: f64, freq_hz: f64) -> f64 {
    let n = data.len() as f64;
    let w = -2.0 * std::f64::consts::PI * freq_hz / fs;
    let (mut re, mut im) = (0.0f64, 0.0f64);
    for (i, &v) in data.iter().enumerate() {
        let phase = w * i as f64;
        re += v * phase.cos();
        im += v * phase.sin();
    }
    // matches numpy's `|fft(x)/N|^2`
    (re / n).powi(2) + (im / n).powi(2)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn percentile_matches_numpy_linear() {
        let d = [1.0, 2.0, 3.0, 4.0];
        assert_eq!(percentile(&d, 0.0), 1.0);
        assert_eq!(percentile(&d, 100.0), 4.0);
        assert_eq!(percentile(&d, 50.0), 2.5);
        assert_eq!(percentile(&d, 25.0), 1.75);
    }

    #[test]
    fn outlier_interpolation_replaces_spikes() {
        let mut d: Vec<f64> = (0..1000).map(|i| (i as f64 * 0.1).sin()).collect();
        d[500] = 1e9; // spike far outside the 99.8th percentile
        interpolate_outliers(&mut d, 0.2, 99.8);
        assert!(d[500].abs() < 2.0, "spike survived: {}", d[500]);
    }

    #[test]
    fn scale_data_flat_window_is_rejected() {
        let mut flat = vec![7.0; 100];
        assert!(scale_data(&mut flat).is_none());
        let mut ok = vec![0.0, 1.0, 2.0];
        assert!(scale_data(&mut ok).is_some());
        assert_eq!(ok, vec![0.0, 512.0, 1024.0]);
    }

    #[test]
    fn filtfilt_preserves_a_constant() {
        // lfilter_zi steady-state ICs mean a constant input must pass
        // through both filters unchanged (up to fp noise).
        let x = vec![42.0; 200];
        for y in filtfilt(&NOTCH_BASELINE, &x) {
            assert!((y - 42.0).abs() < 1e-6, "notch: {y}");
        }
        // The bandpass rejects DC entirely — a constant must come out ~0.
        let y = filtfilt(&BANDPASS_HEART, &x);
        // interior samples (edges see padding transients)
        for &v in &y[50..150] {
            assert!(v.abs() < 1.0, "bandpass DC leak: {v}");
        }
    }

    #[test]
    fn bandpass_passes_in_band_rejects_out_of_band() {
        let fs = 500.0;
        let n = 5000;
        let tone = |f: f64| -> Vec<f64> {
            (0..n)
                .map(|i| (2.0 * std::f64::consts::PI * f * i as f64 / fs).sin())
                .collect()
        };
        let rms = |v: &[f64]| (v[1000..4000].iter().map(|x| x * x).sum::<f64>() / 3000.0).sqrt();
        let in_band = filtfilt(&BANDPASS_HEART, &tone(2.0));
        let out_band = filtfilt(&BANDPASS_HEART, &tone(100.0));
        assert!(rms(&in_band) > 0.6, "2 Hz attenuated: {}", rms(&in_band));
        assert!(rms(&out_band) < 0.05, "100 Hz passed: {}", rms(&out_band));
    }

    #[test]
    fn rolling_mean_shape_and_edges() {
        let d: Vec<f64> = (0..10).map(|i| i as f64).collect();
        let rm = rolling_mean(&d, 3);
        assert_eq!(rm.len(), 10);
        assert_eq!(rm[0], 1.0); // first valid mean, replicated
        assert_eq!(rm[1], 1.0);
        assert_eq!(rm[9], 8.0); // last valid mean, replicated
    }

    #[test]
    fn dft_power_finds_the_tone() {
        let fs = 10.0;
        let n = 300;
        let sig: Vec<f64> = (0..n)
            .map(|i| (2.0 * std::f64::consts::PI * 0.25 * i as f64 / fs).sin())
            .collect();
        let p_at = |f: f64| dft_power_at(&sig, fs, f);
        assert!(p_at(0.25) > 10.0 * p_at(0.15));
        assert!(p_at(0.25) > 10.0 * p_at(0.35));
    }
}
