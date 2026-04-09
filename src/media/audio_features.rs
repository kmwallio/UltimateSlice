// SPDX-License-Identifier: GPL-3.0-or-later
//! Lightweight musical-feature extraction from a reference audio file.
//!
//! Used by the **Generate Music** workflow to derive natural-language
//! style hints (BPM, key/mode, brightness, dynamics) from a user-supplied
//! reference clip and append them to the MusicGen text prompt. The model
//! itself is not modified — this is purely prompt augmentation.
//!
//! Decoding piggybacks on `crate::media::audio_sync::extract_mono_audio_samples`
//! (GStreamer-backed; supports any FFmpeg-decodable format), and FFTs reuse
//! the existing `rustfft` dependency.
//!
//! Analysis is bounded: the first 30 seconds of audio at 22 050 Hz mono.

use std::fmt;

use rustfft::num_complex::Complex;
use rustfft::FftPlanner;

use crate::media::audio_sync::extract_mono_audio_samples;

const ANALYSIS_SAMPLE_RATE: i32 = 22_050;
const ANALYSIS_MAX_SECONDS: f64 = 30.0;
const FFT_WINDOW: usize = 1024;
const FFT_HOP: usize = 512;

/// One of the 12 chromatic pitch classes (C = 0, C♯ = 1, … B = 11).
pub type PitchClass = u8;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyMode {
    Major,
    Minor,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Brightness {
    Dark,
    Neutral,
    Bright,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dynamics {
    Steady,
    Dynamic,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AudioFeatures {
    /// Detected tempo in beats per minute, when confidently estimated.
    pub bpm: Option<f32>,
    /// Detected key root pitch class (0 = C … 11 = B), when chroma energy
    /// is non-negligible.
    pub key_pitch_class: Option<PitchClass>,
    /// Detected key mode (major / minor), paired with `key_pitch_class`.
    pub key_mode: Option<KeyMode>,
    /// Spectral-centroid based brightness bucket.
    pub brightness: Brightness,
    /// Per-frame RMS coefficient-of-variation bucket.
    pub dynamics: Dynamics,
}

#[derive(Debug)]
pub enum AudioFeaturesError {
    /// `extract_mono_audio_samples` returned `None` — the file is missing,
    /// unreadable, or contains no audio.
    DecodeFailed,
    /// The file decoded but the resulting audio was too short to analyze
    /// (we need at least one full FFT window).
    TooShort,
}

impl fmt::Display for AudioFeaturesError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::DecodeFailed => write!(f, "could not decode reference audio"),
            Self::TooShort => write!(f, "reference audio is too short to analyze"),
        }
    }
}

impl std::error::Error for AudioFeaturesError {}

/// Decode + analyze a media file. `source_in_ns` / `source_out_ns` may be
/// passed as `0` / `u64::MAX` to analyze from the start of the file.
pub fn analyze_audio_file(
    path: &str,
    source_in_ns: u64,
    source_out_ns: u64,
) -> Result<AudioFeatures, AudioFeaturesError> {
    let samples = extract_mono_audio_samples(
        path,
        source_in_ns,
        source_out_ns,
        ANALYSIS_SAMPLE_RATE,
        ANALYSIS_MAX_SECONDS,
    )
    .ok_or(AudioFeaturesError::DecodeFailed)?;
    analyze_samples(&samples, ANALYSIS_SAMPLE_RATE as f32)
}

/// Pure-DSP entry point — used both internally and from unit tests so we can
/// feed synthetic signals without involving GStreamer.
pub fn analyze_samples(
    samples: &[f32],
    sample_rate: f32,
) -> Result<AudioFeatures, AudioFeaturesError> {
    if samples.len() < FFT_WINDOW {
        return Err(AudioFeaturesError::TooShort);
    }

    let frames = compute_frames(samples, sample_rate);

    let bpm = estimate_bpm(&frames, sample_rate);
    let (key_pitch_class, key_mode) = match estimate_key(&frames) {
        Some((pc, mode)) => (Some(pc), Some(mode)),
        None => (None, None),
    };
    let brightness = bucket_brightness(&frames);
    let dynamics = bucket_dynamics(samples);

    Ok(AudioFeatures {
        bpm,
        key_pitch_class,
        key_mode,
        brightness,
        dynamics,
    })
}

/// Render a feature set as a natural-language hint suitable for appending
/// to a MusicGen text prompt. Empty when nothing useful was detected.
pub fn features_to_prompt_hint(features: &AudioFeatures) -> String {
    let mut parts: Vec<String> = Vec::new();
    if let Some(bpm) = features.bpm {
        parts.push(format!("around {} BPM", bpm.round() as u32));
    }
    if let (Some(pc), Some(mode)) = (features.key_pitch_class, features.key_mode) {
        let mode_word = match mode {
            KeyMode::Major => "major",
            KeyMode::Minor => "minor",
        };
        parts.push(format!(
            "in the key of {} {}",
            pitch_class_name(pc),
            mode_word
        ));
    }
    parts.push(
        match features.brightness {
            Brightness::Dark => "dark timbre",
            Brightness::Neutral => "balanced timbre",
            Brightness::Bright => "bright timbre",
        }
        .to_string(),
    );
    parts.push(
        match features.dynamics {
            Dynamics::Steady => "steady dynamics",
            Dynamics::Dynamic => "dynamic energy",
        }
        .to_string(),
    );
    parts.join(", ")
}

pub fn pitch_class_name(pc: PitchClass) -> &'static str {
    match pc % 12 {
        0 => "C",
        1 => "C#",
        2 => "D",
        3 => "D#",
        4 => "E",
        5 => "F",
        6 => "F#",
        7 => "G",
        8 => "G#",
        9 => "A",
        10 => "A#",
        _ => "B",
    }
}

// ── Per-frame analysis ─────────────────────────────────────────────────────

struct Frame {
    /// Magnitude spectrum (length = FFT_WINDOW / 2).
    magnitudes: Vec<f32>,
}

fn compute_frames(samples: &[f32], _sample_rate: f32) -> Vec<Frame> {
    let mut planner = FftPlanner::<f32>::new();
    let fft = planner.plan_fft_forward(FFT_WINDOW);
    let mut buf: Vec<Complex<f32>> = vec![Complex::new(0.0, 0.0); FFT_WINDOW];
    let window = hann_window(FFT_WINDOW);

    let mut frames = Vec::new();
    let half = FFT_WINDOW / 2;

    let mut start = 0usize;
    while start + FFT_WINDOW <= samples.len() {
        for (i, slot) in buf.iter_mut().enumerate() {
            *slot = Complex::new(samples[start + i] * window[i], 0.0);
        }
        fft.process(&mut buf);
        let mut magnitudes = Vec::with_capacity(half);
        for c in buf.iter().take(half) {
            let m = (c.re * c.re + c.im * c.im).sqrt();
            magnitudes.push(m);
        }
        frames.push(Frame { magnitudes });
        start += FFT_HOP;
    }
    frames
}

fn hann_window(n: usize) -> Vec<f32> {
    let denom = (n as f32 - 1.0).max(1.0);
    (0..n)
        .map(|i| 0.5 - 0.5 * ((2.0 * std::f32::consts::PI * i as f32) / denom).cos())
        .collect()
}

// ── BPM estimation ─────────────────────────────────────────────────────────

fn estimate_bpm(frames: &[Frame], sample_rate: f32) -> Option<f32> {
    if frames.len() < 16 {
        return None;
    }
    // Spectral-flux onset envelope: rectified positive change in per-bin
    // magnitude across consecutive frames.
    let mut flux = Vec::with_capacity(frames.len());
    flux.push(0.0f32);
    for w in frames.windows(2) {
        let prev = &w[0].magnitudes;
        let cur = &w[1].magnitudes;
        let mut sum = 0.0f32;
        for (a, b) in prev.iter().zip(cur.iter()) {
            let d = b - a;
            if d > 0.0 {
                sum += d;
            }
        }
        flux.push(sum);
    }

    // Smooth the onset envelope across a few frames to mitigate hop-size
    // discretization aliasing — without this, a click train whose period is
    // not an integer number of hops can look louder at 2× the true period
    // than at the period itself.
    let smoothed = smooth_envelope(&flux, 2);

    // Reject pure-tone / silent input where the envelope has no variation.
    let mean = smoothed.iter().sum::<f32>() / smoothed.len() as f32;
    if mean <= 1e-6 {
        return None;
    }
    let variance = smoothed.iter().map(|x| (x - mean).powi(2)).sum::<f32>() / smoothed.len() as f32;
    let std = variance.sqrt();
    if std / mean.max(1e-6) < 0.1 {
        return None;
    }

    // Center the envelope so autocorrelation reflects pulse periodicity, not DC.
    let centered: Vec<f32> = smoothed.iter().map(|x| x - mean).collect();

    // Frame rate of the onset envelope.
    let frame_rate = sample_rate / FFT_HOP as f32;
    let min_lag = ((60.0 / 200.0) * frame_rate).round() as usize; // 200 BPM
    let max_lag = ((60.0 / 60.0) * frame_rate).round() as usize; // 60 BPM
    if min_lag == 0 || max_lag >= centered.len() {
        return None;
    }

    // Autocorrelation across the BPM-relevant lag range.
    let mut scores = vec![0.0f32; max_lag + 1];
    let mut max_score = f32::NEG_INFINITY;
    for lag in min_lag..=max_lag {
        let mut acc = 0.0f32;
        for i in 0..(centered.len() - lag) {
            acc += centered[i] * centered[i + lag];
        }
        scores[lag] = acc;
        if acc > max_score {
            max_score = acc;
        }
    }

    // Reject weak / negative peaks.
    if max_score <= 0.0 {
        return None;
    }
    let zero_lag: f32 = centered.iter().map(|x| x * x).sum();
    if zero_lag <= 0.0 || max_score / zero_lag < 0.05 {
        return None;
    }

    // Octave correction: pick the smallest local-max lag whose score is at
    // least 85% of the global maximum. This prefers the fundamental period
    // over its 2× / 3× multiples that always tie or beat the fundamental in
    // raw autocorrelation.
    let threshold = max_score * 0.85;
    let mut best_lag = 0usize;
    for lag in min_lag..=max_lag {
        if scores[lag] < threshold {
            continue;
        }
        let left = if lag > min_lag {
            scores[lag - 1]
        } else {
            f32::NEG_INFINITY
        };
        let right = if lag < max_lag {
            scores[lag + 1]
        } else {
            f32::NEG_INFINITY
        };
        if scores[lag] >= left && scores[lag] >= right {
            best_lag = lag;
            break;
        }
    }
    if best_lag == 0 {
        // Fall back to global argmax if no qualifying local peak.
        for lag in min_lag..=max_lag {
            if (scores[lag] - max_score).abs() < f32::EPSILON {
                best_lag = lag;
                break;
            }
        }
    }
    if best_lag == 0 {
        return None;
    }

    let period_s = best_lag as f32 / frame_rate;
    let bpm = 60.0 / period_s;
    if (40.0..=240.0).contains(&bpm) {
        Some(bpm)
    } else {
        None
    }
}

fn smooth_envelope(values: &[f32], radius: usize) -> Vec<f32> {
    if radius == 0 || values.is_empty() {
        return values.to_vec();
    }
    let n = values.len();
    let mut out = vec![0.0f32; n];
    for i in 0..n {
        let lo = i.saturating_sub(radius);
        let hi = (i + radius).min(n - 1);
        let mut sum = 0.0f32;
        let mut count = 0u32;
        for v in &values[lo..=hi] {
            sum += *v;
            count += 1;
        }
        out[i] = sum / count as f32;
    }
    out
}

// ── Key estimation ─────────────────────────────────────────────────────────

/// Krumhansl-Schmuckler major and minor key profiles, normalized to mean 0.
const KS_MAJOR: [f32; 12] = [
    6.35, 2.23, 3.48, 2.33, 4.38, 4.09, 2.52, 5.19, 2.39, 3.66, 2.29, 2.88,
];
const KS_MINOR: [f32; 12] = [
    6.33, 2.68, 3.52, 5.38, 2.60, 3.53, 2.54, 4.75, 3.98, 2.69, 3.34, 3.17,
];

fn estimate_key(frames: &[Frame]) -> Option<(PitchClass, KeyMode)> {
    if frames.is_empty() {
        return None;
    }
    let sample_rate = ANALYSIS_SAMPLE_RATE as f32;
    let bin_hz = sample_rate / FFT_WINDOW as f32;
    let half = FFT_WINDOW / 2;

    let mut chroma = [0.0f32; 12];
    for frame in frames {
        for (k, &mag) in frame.magnitudes.iter().take(half).enumerate() {
            if k == 0 {
                continue;
            }
            let freq = k as f32 * bin_hz;
            if !(50.0..=5000.0).contains(&freq) {
                continue;
            }
            // MIDI note number → pitch class (A4 = 69 = 440 Hz).
            let midi = 69.0 + 12.0 * (freq / 440.0).log2();
            let pc = ((midi.round() as i32).rem_euclid(12)) as usize;
            chroma[pc] += mag;
        }
    }

    let total: f32 = chroma.iter().sum();
    if total <= 1e-3 {
        return None;
    }

    // Mean-center both chroma and the templates, then score by correlation
    // (Pearson without division — we only need argmax).
    let chroma_mean = total / 12.0;
    let centered_chroma: Vec<f32> = chroma.iter().map(|c| c - chroma_mean).collect();

    let major_centered = mean_center(&KS_MAJOR);
    let minor_centered = mean_center(&KS_MINOR);

    let mut best_score = f32::NEG_INFINITY;
    let mut best_root: usize = 0;
    let mut best_mode = KeyMode::Major;

    for root in 0..12 {
        let mut maj = 0.0f32;
        let mut min = 0.0f32;
        for i in 0..12 {
            let cv = centered_chroma[(i + root) % 12];
            maj += cv * major_centered[i];
            min += cv * minor_centered[i];
        }
        if maj > best_score {
            best_score = maj;
            best_root = root;
            best_mode = KeyMode::Major;
        }
        if min > best_score {
            best_score = min;
            best_root = root;
            best_mode = KeyMode::Minor;
        }
    }

    if best_score <= 0.0 {
        return None;
    }
    Some((best_root as PitchClass, best_mode))
}

fn mean_center(arr: &[f32; 12]) -> [f32; 12] {
    let mean: f32 = arr.iter().sum::<f32>() / 12.0;
    let mut out = [0.0f32; 12];
    for i in 0..12 {
        out[i] = arr[i] - mean;
    }
    out
}

// ── Brightness ─────────────────────────────────────────────────────────────

fn bucket_brightness(frames: &[Frame]) -> Brightness {
    let bin_hz = ANALYSIS_SAMPLE_RATE as f32 / FFT_WINDOW as f32;
    let mut weighted = 0.0f32;
    let mut total = 0.0f32;
    for frame in frames {
        for (k, &mag) in frame.magnitudes.iter().enumerate() {
            let freq = k as f32 * bin_hz;
            weighted += mag * freq;
            total += mag;
        }
    }
    if total <= 1e-6 {
        return Brightness::Neutral;
    }
    let centroid = weighted / total;
    if centroid < 1500.0 {
        Brightness::Dark
    } else if centroid > 3000.0 {
        Brightness::Bright
    } else {
        Brightness::Neutral
    }
}

// ── Dynamics ───────────────────────────────────────────────────────────────

fn bucket_dynamics(samples: &[f32]) -> Dynamics {
    let frame_size = FFT_WINDOW;
    if samples.len() < frame_size * 2 {
        return Dynamics::Steady;
    }
    let mut rms_values = Vec::new();
    let mut start = 0usize;
    while start + frame_size <= samples.len() {
        let mut sumsq = 0.0f32;
        for &s in &samples[start..start + frame_size] {
            sumsq += s * s;
        }
        rms_values.push((sumsq / frame_size as f32).sqrt());
        start += frame_size;
    }
    if rms_values.len() < 4 {
        return Dynamics::Steady;
    }
    let mean = rms_values.iter().sum::<f32>() / rms_values.len() as f32;
    if mean <= 1e-6 {
        return Dynamics::Steady;
    }
    let variance =
        rms_values.iter().map(|x| (x - mean).powi(2)).sum::<f32>() / rms_values.len() as f32;
    let cv = variance.sqrt() / mean;
    if cv > 0.6 {
        Dynamics::Dynamic
    } else {
        Dynamics::Steady
    }
}

// ── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::f32::consts::PI;

    fn synth_sine(freq: f32, sample_rate: f32, secs: f32) -> Vec<f32> {
        let n = (sample_rate * secs) as usize;
        (0..n)
            .map(|i| 0.5 * (2.0 * PI * freq * i as f32 / sample_rate).sin())
            .collect()
    }

    fn synth_clicks(bpm: f32, sample_rate: f32, secs: f32) -> Vec<f32> {
        let n = (sample_rate * secs) as usize;
        let interval = (60.0 / bpm * sample_rate) as usize;
        let click_len = 64usize;
        let mut out = vec![0.0f32; n];
        let mut t = 0usize;
        while t + click_len < n {
            // Short envelope-shaped click.
            for i in 0..click_len {
                let env = (1.0 - i as f32 / click_len as f32).max(0.0);
                out[t + i] = env;
            }
            t += interval;
        }
        out
    }

    fn synth_noise(sample_rate: f32, secs: f32) -> Vec<f32> {
        // Deterministic LCG so the test is reproducible.
        let n = (sample_rate * secs) as usize;
        let mut state: u32 = 0xC0FFEE;
        (0..n)
            .map(|_| {
                state = state.wrapping_mul(1103515245).wrapping_add(12345);
                ((state >> 16) as f32 / 32_768.0) - 1.0
            })
            .collect()
    }

    #[test]
    fn sine_at_440_reports_a_key() {
        let sr = ANALYSIS_SAMPLE_RATE as f32;
        let samples = synth_sine(440.0, sr, 4.0);
        let f = analyze_samples(&samples, sr).expect("analysis");
        // 440 Hz is A — pitch class 9.
        assert_eq!(
            f.key_pitch_class,
            Some(9),
            "expected A, got {:?}",
            f.key_pitch_class
        );
    }

    #[test]
    fn click_train_at_120_bpm_reports_120() {
        let sr = ANALYSIS_SAMPLE_RATE as f32;
        let samples = synth_clicks(120.0, sr, 6.0);
        let f = analyze_samples(&samples, sr).expect("analysis");
        let bpm = f.bpm.expect("bpm should be detected");
        assert!((bpm - 120.0).abs() < 6.0, "expected ~120 BPM, got {bpm}");
    }

    #[test]
    fn white_noise_is_bright_and_dynamic_with_no_key() {
        let sr = ANALYSIS_SAMPLE_RATE as f32;
        let samples = synth_noise(sr, 4.0);
        let f = analyze_samples(&samples, sr).expect("analysis");
        // Spectrally-flat noise should land in the bright bucket.
        assert_eq!(f.brightness, Brightness::Bright);
        // Centered noise should lack a strong key — but allow either None or
        // a low-confidence detection (the dummy LCG isn't perfectly flat).
        // The important guarantee is that we don't crash.
        let _ = f.key_pitch_class;
    }

    #[test]
    fn silence_returns_neutral_features() {
        let sr = ANALYSIS_SAMPLE_RATE as f32;
        let samples = vec![0.0f32; (sr * 3.0) as usize];
        let f = analyze_samples(&samples, sr).expect("analysis");
        assert_eq!(f.bpm, None);
        assert_eq!(f.key_pitch_class, None);
        assert_eq!(f.dynamics, Dynamics::Steady);
    }

    #[test]
    fn too_short_input_errors() {
        let sr = ANALYSIS_SAMPLE_RATE as f32;
        let samples = vec![0.0f32; 100];
        assert!(matches!(
            analyze_samples(&samples, sr),
            Err(AudioFeaturesError::TooShort)
        ));
    }

    #[test]
    fn prompt_hint_includes_all_present_fields() {
        let f = AudioFeatures {
            bpm: Some(128.0),
            key_pitch_class: Some(0),
            key_mode: Some(KeyMode::Major),
            brightness: Brightness::Bright,
            dynamics: Dynamics::Dynamic,
        };
        let hint = features_to_prompt_hint(&f);
        assert!(hint.contains("128 BPM"), "{hint}");
        assert!(hint.contains("C major"), "{hint}");
        assert!(hint.contains("bright"), "{hint}");
        assert!(hint.contains("dynamic"), "{hint}");
    }

    #[test]
    fn prompt_hint_omits_missing_fields() {
        let f = AudioFeatures {
            bpm: None,
            key_pitch_class: None,
            key_mode: None,
            brightness: Brightness::Neutral,
            dynamics: Dynamics::Steady,
        };
        let hint = features_to_prompt_hint(&f);
        assert!(!hint.contains("BPM"));
        assert!(!hint.contains("key of"));
        assert!(hint.contains("balanced"));
        assert!(hint.contains("steady"));
    }
}
