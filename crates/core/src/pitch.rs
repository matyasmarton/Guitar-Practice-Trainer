//! Pitch detection — a small vendored YIN implementation.
//!
//! We deliberately vendor YIN (≈60 lines) instead of depending on the
//! `pitch-detection` crate, per the plan's sanctioned fallback, to avoid any
//! risk of an incompatible/unmaintained external API. The algorithm is the
//! classic de Cheveigné & Kawahara YIN:
//!   1. difference function `d(τ) = Σ (x[j] − x[j+τ])²`;
//!   2. cumulative mean normalized difference `d'(τ)`;
//!   3. first `τ` whose `d'(τ)` drops below `threshold` (absolute threshold),
//!      refined with parabolic interpolation;
//!   4. `f0 = sample_rate / τ`.
//!
//! Frame math is rate-independent: a fixed `FRAME_SAMPLES` window is analyzed
//! whatever `sample_rate` cpal hands us (at 44.1 kHz min detectable ≈ 43 Hz,
//! at 48 kHz ≈ 47 Hz — both below E2 ≈ 82.41 Hz).

/// YIN absolute threshold (typical 0.10–0.20; 0.15 from the plan).
pub const YIN_THRESHOLD: f64 = 0.15;
/// Min detectable frequency (Hz). Margin below E2 ≈ 82.41 Hz.
pub const MIN_FREQ: f64 = 70.0;
/// Max detectable frequency (Hz).
pub const MAX_FREQ: f64 = 1400.0;

/// Window length (samples) analyzed per detection frame.
pub const FRAME_SAMPLES: usize = 2048;
/// Hop between successive frames (50% overlap).
pub const HOP_SAMPLES: usize = 1024;

/// Detect the fundamental frequency in `frame` (mono f32, any sample rate).
///
/// Returns `Some(hz)` if a confident pitch is found, else `None` (silence or
/// unvoiced frame). `frame` need not be exactly `FRAME_SAMPLES` long but
/// detection quality degrades below it.
pub fn detect(frame: &[f32], sample_rate: u32) -> Option<f64> {
    let n = frame.len();
    if n < 4 {
        return None;
    }

    // Silence / near-silence guard.
    let rms = frame.iter().map(|s| (*s as f64) * (*s as f64)).sum::<f64>() / n as f64;
    if rms < 1e-7 {
        return None;
    }

    // τ search bounds derived from frequency bounds.
    let sr = sample_rate as f64;
    let max_tau = (sr / MIN_FREQ).floor() as usize;
    let min_tau = (sr / MAX_FREQ).ceil() as usize;
    // Limit τ so x[j+τ] stays in range.
    let half = (n / 2).min(max_tau);
    let max_tau = half.min(max_tau);
    if max_tau <= min_tau {
        return None;
    }

    // 1. Difference function d(τ) for τ in 0..=max_tau.
    let mut diff = vec![0.0f64; max_tau + 1];
    for tau in 1..=max_tau {
        let mut s = 0.0;
        for j in 0..(n - max_tau) {
            let delta = frame[j] as f64 - frame[j + tau] as f64;
            s += delta * delta;
        }
        diff[tau] = s;
    }

    // 2. Cumulative mean normalized difference d'(τ).
    let mut cmnd = vec![0.0f64; max_tau + 1];
    cmnd[0] = 1.0;
    let mut running = 0.0;
    for tau in 1..=max_tau {
        running += diff[tau];
        cmnd[tau] = if running > 0.0 {
            diff[tau] * (tau as f64) / running
        } else {
            1.0
        };
    }

    // 3. First τ in [min_tau..=max_tau] below threshold (absolute threshold).
    let mut tau_best: Option<usize> = None;
    for tau in min_tau..=max_tau {
        if cmnd[tau] < YIN_THRESHOLD {
            // Local minimum: keep stepping while decreasing.
            let mut t = tau;
            while t < max_tau && cmnd[t + 1] < cmnd[t] {
                t += 1;
            }
            tau_best = Some(t);
            break;
        }
    }

    // 4. Fallback: if nothing cleared the absolute threshold, use the global
    //    minimum of d'(τ) but only accept it if it's reasonably low (< 0.5).
    let tau = match tau_best {
        Some(t) => t,
        None => {
            let (t, v) = (min_tau..=max_tau)
                .map(|t| (t, cmnd[t]))
                .min_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal))?;
            if v >= 0.5 {
                return None;
            }
            t
        }
    };

    // Parabolic interpolation around the minimum for sub-sample τ.
    let better_tau = if tau > 0 && tau < max_tau {
        let s0 = cmnd[tau - 1];
        let s1 = cmnd[tau];
        let s2 = cmnd[tau + 1];
        let denom = 2.0 * (2.0 * s1 - s2 - s0);
        if denom.abs() > 1e-12 {
            tau as f64 + (s0 - s2) / denom
        } else {
            tau as f64
        }
    } else {
        tau as f64
    };

    if better_tau <= 0.0 {
        return None;
    }
    let hz = sr / better_tau;
    if !(MIN_FREQ..=MAX_FREQ).contains(&hz) {
        return None;
    }
    Some(hz)
}

/// Test utility: synthesize a mono f32 sine buffer at `hz` for `n` samples.
#[cfg(test)]
pub(crate) fn sine_buffer(hz: f64, n: usize, sr: u32) -> Vec<f32> {
    let mut out = vec![0.0f32; n];
    let phase_step = 2.0 * std::f64::consts::PI * hz / sr as f64;
    let mut phase: f64 = 0.0;
    for s in out.iter_mut() {
        *s = (phase.sin() * 0.9) as f32;
        phase += phase_step;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const SR: u32 = 44100;

    #[test]
    fn silence_returns_none() {
        let buf = vec![0.0f32; FRAME_SAMPLES];
        assert!(detect(&buf, SR).is_none());
    }

    #[test]
    fn detects_e2_low_string() {
        // E2 ≈ 82.41 Hz. Tolerate ±2 Hz (nearest MIDI is what matters).
        let buf = sine_buffer(82.41, FRAME_SAMPLES, SR);
        let f = detect(&buf, SR).expect("E2 should be detected");
        assert!(
            (80.0..=84.0).contains(&f),
            "E2 detection {f} out of [80,84]"
        );
    }

    #[test]
    fn detects_a4() {
        let buf = sine_buffer(440.0, FRAME_SAMPLES, SR);
        let f = detect(&buf, SR).expect("A4 should be detected");
        assert!(
            (438.0..=442.0).contains(&f),
            "A4 detection {f} out of [438,442]"
        );
    }

    #[test]
    fn detects_at_48k() {
        let sr = 48000;
        let buf = sine_buffer(82.41, FRAME_SAMPLES, sr);
        let f = detect(&buf, sr).expect("E2 @48k should be detected");
        assert!((80.0..=84.0).contains(&f), "E2 @48k {f} out of range");
    }
}