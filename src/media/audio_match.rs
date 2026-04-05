//! Reference-based audio matching: analyse a source clip and a reference clip,
//! then derive a conservative loudness delta plus adaptive 3-band EQ target
//! that nudges the source toward the reference's speech-oriented tonal balance.

use anyhow::{anyhow, Result};
use rustfft::num_complex::Complex;
use rustfft::FftPlanner;

use crate::model::clip::{default_eq_bands, EqBand};

const SAMPLE_RATE: i32 = 22_050;
const MAX_EXTRACT_SECONDS: f64 = 20.0;
const FFT_SIZE: usize = 2048;
const HOP_SIZE: usize = FFT_SIZE / 2;
const PROFILE_FLOOR: f64 = 1e-12;
const EQ_RESPONSE_SCALE: f64 = 0.75;
const EQ_GAIN_LIMIT_DB: f64 = 9.0;
const EQ_GAIN_DEADZONE_DB: f64 = 0.5;
const EQ_Q_MIN: f64 = 0.7;
const EQ_Q_MAX: f64 = 2.5;
const ENERGY_ACTIVE_RANGE_DB: f64 = 26.0;
const ENERGY_WEIGHT_SOFTNESS_DB: f64 = 10.0;
const REGION_MIN_WEIGHT_SUM: f64 = 0.75;
const REGION_MIN_ACTIVE_FRAMES: usize = 4;

const ANALYSIS_BAND_COUNT: usize = 11;
const ANALYSIS_BANDS: [(f64, f64); ANALYSIS_BAND_COUNT] = [
    (80.0, 120.0),
    (120.0, 180.0),
    (180.0, 270.0),
    (270.0, 400.0),
    (400.0, 650.0),
    (650.0, 1_000.0),
    (1_000.0, 1_600.0),
    (1_600.0, 2_500.0),
    (2_500.0, 4_000.0),
    (4_000.0, 6_500.0),
    (6_500.0, 9_000.0),
];

const PROFILE_LOW_BANDS: [usize; 4] = [0, 1, 2, 3];
const PROFILE_MID_BANDS: [usize; 4] = [4, 5, 6, 7];
const PROFILE_HIGH_BANDS: [usize; 3] = [8, 9, 10];

const EQ_LOW_FIT_BANDS: [usize; 4] = [0, 1, 2, 3];
const EQ_MID_FIT_BANDS: [usize; 5] = [3, 4, 5, 6, 7];
const EQ_HIGH_FIT_BANDS: [usize; 4] = [7, 8, 9, 10];

const SPEECH_PRESENCE_BANDS: [usize; 7] = [2, 3, 4, 5, 6, 7, 8];
const SPEECH_CORE_BANDS: [usize; 5] = [3, 4, 5, 6, 7];
const LOW_NOISE_BANDS: [usize; 2] = [0, 1];
const HIGH_NOISE_BANDS: [usize; 2] = [9, 10];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AnalysisRegionNs {
    pub start_ns: u64,
    pub end_ns: u64,
}

#[derive(Debug, Clone)]
pub struct AudioMatchParams {
    pub source_path: String,
    pub source_in_ns: u64,
    pub source_out_ns: u64,
    pub source_speech_regions: Vec<AnalysisRegionNs>,
    pub reference_path: String,
    pub reference_in_ns: u64,
    pub reference_out_ns: u64,
    pub reference_speech_regions: Vec<AnalysisRegionNs>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SpectralProfile {
    pub low_db: f64,
    pub mid_db: f64,
    pub high_db: f64,
}

impl SpectralProfile {
    fn from_band_ratios(ratios: [f64; 3]) -> Self {
        Self {
            low_db: 10.0 * ratios[0].max(PROFILE_FLOOR).log10(),
            mid_db: 10.0 * ratios[1].max(PROFILE_FLOOR).log10(),
            high_db: 10.0 * ratios[2].max(PROFILE_FLOOR).log10(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct DetailedSpectrum {
    band_ratios: [f64; ANALYSIS_BAND_COUNT],
}

impl DetailedSpectrum {
    fn from_band_ratios(mut band_ratios: [f64; ANALYSIS_BAND_COUNT]) -> Self {
        for ratio in &mut band_ratios {
            *ratio = ratio.max(PROFILE_FLOOR);
        }
        Self { band_ratios }
    }

    fn band_db(&self, idx: usize) -> f64 {
        10.0 * self.band_ratios[idx].max(PROFILE_FLOOR).log10()
    }

    fn grouped_ratio(&self, indices: &[usize]) -> f64 {
        indices
            .iter()
            .map(|&idx| self.band_ratios[idx])
            .sum::<f64>()
    }

    fn collapsed_profile(&self) -> SpectralProfile {
        SpectralProfile::from_band_ratios([
            self.grouped_ratio(&PROFILE_LOW_BANDS),
            self.grouped_ratio(&PROFILE_MID_BANDS),
            self.grouped_ratio(&PROFILE_HIGH_BANDS),
        ])
    }

    fn deltas_to(self, target: Self) -> [f64; ANALYSIS_BAND_COUNT] {
        let mut deltas = [0.0f64; ANALYSIS_BAND_COUNT];
        for (idx, delta) in deltas.iter_mut().enumerate() {
            *delta = target.band_db(idx) - self.band_db(idx);
        }
        deltas
    }

    #[cfg(test)]
    fn from_db_levels(levels_db: [f64; ANALYSIS_BAND_COUNT]) -> Self {
        let mut band_ratios = [0.0f64; ANALYSIS_BAND_COUNT];
        for (idx, level_db) in levels_db.into_iter().enumerate() {
            band_ratios[idx] = 10f64.powf(level_db / 10.0);
        }
        let total = band_ratios.iter().sum::<f64>().max(PROFILE_FLOOR);
        for ratio in &mut band_ratios {
            *ratio /= total;
        }
        Self::from_band_ratios(band_ratios)
    }
}

#[derive(Debug, Clone, Copy)]
struct FrameAnalysis {
    band_ratios: [f64; ANALYSIS_BAND_COUNT],
    total_energy: f64,
    start_sample: usize,
    available_samples: usize,
}

#[derive(Debug, Clone)]
pub struct AudioMatchOutcome {
    pub source_loudness_lufs: f64,
    pub reference_loudness_lufs: f64,
    pub volume_gain: f64,
    pub eq_bands: [EqBand; 3],
    pub source_profile: SpectralProfile,
    pub reference_profile: SpectralProfile,
}

pub fn run_audio_match(params: &AudioMatchParams) -> Result<AudioMatchOutcome> {
    let source_loudness_lufs = crate::media::export::analyze_loudness_lufs(
        &params.source_path,
        params.source_in_ns,
        params.source_out_ns,
    )?;
    let reference_loudness_lufs = crate::media::export::analyze_loudness_lufs(
        &params.reference_path,
        params.reference_in_ns,
        params.reference_out_ns,
    )?;
    let source_samples = crate::media::audio_sync::extract_mono_audio_samples(
        &params.source_path,
        params.source_in_ns,
        params.source_out_ns,
        SAMPLE_RATE,
        MAX_EXTRACT_SECONDS,
    )
    .ok_or_else(|| anyhow!("Could not extract source audio samples"))?;
    let reference_samples = crate::media::audio_sync::extract_mono_audio_samples(
        &params.reference_path,
        params.reference_in_ns,
        params.reference_out_ns,
        SAMPLE_RATE,
        MAX_EXTRACT_SECONDS,
    )
    .ok_or_else(|| anyhow!("Could not extract reference audio samples"))?;

    let source_spectrum = detailed_spectrum_from_samples(
        &source_samples,
        SAMPLE_RATE as f64,
        &params.source_speech_regions,
    )?;
    let reference_spectrum = detailed_spectrum_from_samples(
        &reference_samples,
        SAMPLE_RATE as f64,
        &params.reference_speech_regions,
    )?;
    let source_profile = source_spectrum.collapsed_profile();
    let reference_profile = reference_spectrum.collapsed_profile();

    Ok(AudioMatchOutcome {
        source_loudness_lufs,
        reference_loudness_lufs,
        volume_gain: crate::media::export::compute_lufs_gain(
            source_loudness_lufs,
            reference_loudness_lufs,
        ),
        eq_bands: matched_eq_bands(source_spectrum, reference_spectrum),
        source_profile,
        reference_profile,
    })
}

fn matched_eq_bands(
    source_spectrum: DetailedSpectrum,
    reference_spectrum: DetailedSpectrum,
) -> [EqBand; 3] {
    let defaults = default_eq_bands();
    let deltas = source_spectrum.deltas_to(reference_spectrum);
    let mean_delta = deltas.iter().sum::<f64>() / deltas.len() as f64;
    let mut shaped_deltas = [0.0f64; ANALYSIS_BAND_COUNT];
    for (idx, delta) in deltas.into_iter().enumerate() {
        shaped_deltas[idx] = delta - mean_delta;
    }

    [
        matched_eq_band_for_group(defaults[0], &EQ_LOW_FIT_BANDS, &shaped_deltas),
        matched_eq_band_for_group(defaults[1], &EQ_MID_FIT_BANDS, &shaped_deltas),
        matched_eq_band_for_group(defaults[2], &EQ_HIGH_FIT_BANDS, &shaped_deltas),
    ]
}

fn matched_eq_band_for_group(
    default_band: EqBand,
    fit_indices: &[usize],
    shaped_deltas: &[f64; ANALYSIS_BAND_COUNT],
) -> EqBand {
    let positive_strength = fit_indices
        .iter()
        .map(|&idx| shaped_deltas[idx].max(0.0))
        .sum::<f64>();
    let negative_strength = fit_indices
        .iter()
        .map(|&idx| (-shaped_deltas[idx]).max(0.0))
        .sum::<f64>();

    if positive_strength <= PROFILE_FLOOR && negative_strength <= PROFILE_FLOOR {
        return default_band;
    }

    let select_positive = positive_strength >= negative_strength;
    let selected: Vec<(f64, f64)> = fit_indices
        .iter()
        .filter_map(|&idx| {
            let delta = shaped_deltas[idx];
            let matches_sign = if select_positive {
                delta > 0.0
            } else {
                delta < 0.0
            };
            matches_sign.then(|| (analysis_band_center_hz(idx), delta))
        })
        .collect();

    if selected.is_empty() {
        return default_band;
    }

    let total_weight = selected.iter().map(|(_, delta)| delta.abs()).sum::<f64>();
    if total_weight <= PROFILE_FLOOR {
        return default_band;
    }

    let weighted_delta = selected
        .iter()
        .map(|(_, delta)| delta * delta.abs())
        .sum::<f64>()
        / total_weight;
    let gain = (weighted_delta * EQ_RESPONSE_SCALE).clamp(-EQ_GAIN_LIMIT_DB, EQ_GAIN_LIMIT_DB);
    if gain.abs() < EQ_GAIN_DEADZONE_DB {
        return default_band;
    }

    let center_log2 = selected
        .iter()
        .map(|(freq, delta)| freq.log2() * delta.abs())
        .sum::<f64>()
        / total_weight;
    let octave_spread = (selected
        .iter()
        .map(|(freq, delta)| {
            let distance = freq.log2() - center_log2;
            delta.abs() * distance * distance
        })
        .sum::<f64>()
        / total_weight)
        .sqrt();

    let (min_freq, max_freq) = analysis_band_bounds(fit_indices);
    EqBand {
        freq: 2f64.powf(center_log2).clamp(min_freq, max_freq),
        gain,
        q: q_from_octave_spread(octave_spread),
    }
}

fn detailed_spectrum_from_samples(
    samples: &[f32],
    sample_rate: f64,
    speech_regions: &[AnalysisRegionNs],
) -> Result<DetailedSpectrum> {
    let ratios = average_band_ratios(samples, sample_rate, speech_regions)?;
    Ok(DetailedSpectrum::from_band_ratios(ratios))
}

fn average_band_ratios(
    samples: &[f32],
    sample_rate: f64,
    speech_regions: &[AnalysisRegionNs],
) -> Result<[f64; ANALYSIS_BAND_COUNT]> {
    if samples.is_empty() {
        return Err(anyhow!("No audio samples available for analysis"));
    }

    let frames = collect_frame_analyses(samples, sample_rate)?;
    if frames.is_empty() {
        return Err(anyhow!(
            "Audio match could not derive a usable spectral profile"
        ));
    }

    let heuristic_weights = heuristic_frame_weights(&frames);
    let sample_ranges = speech_regions_to_sample_ranges(speech_regions, sample_rate, samples.len());
    let weights = if sample_ranges.is_empty() {
        heuristic_weights
    } else {
        let region_weights: Vec<f64> = frames
            .iter()
            .zip(heuristic_weights.iter())
            .map(|(frame, heuristic)| {
                heuristic * frame_region_overlap_weight(frame, &sample_ranges)
            })
            .collect();
        if region_weights
            .iter()
            .filter(|&&weight| weight > 0.01)
            .count()
            >= REGION_MIN_ACTIVE_FRAMES
            && region_weights.iter().sum::<f64>() >= REGION_MIN_WEIGHT_SUM
        {
            region_weights
        } else {
            heuristic_weights
        }
    };

    weighted_band_average(&frames, &weights)
}

fn collect_frame_analyses(samples: &[f32], sample_rate: f64) -> Result<Vec<FrameAnalysis>> {
    let window = hann_window(FFT_SIZE);
    let mut planner = FftPlanner::<f32>::new();
    let fft = planner.plan_fft_forward(FFT_SIZE);
    let mut frames = Vec::new();
    let mut start = 0usize;

    loop {
        let (frame, available_samples) = frame_with_padding(samples, start, FFT_SIZE);
        if let Some(frame_analysis) = analyze_frame(
            &frame,
            &window,
            sample_rate,
            fft.as_ref(),
            start,
            available_samples,
        ) {
            frames.push(frame_analysis);
        }
        if start + FFT_SIZE >= samples.len() {
            break;
        }
        start += HOP_SIZE;
    }

    if frames.is_empty() {
        Err(anyhow!(
            "Audio match could not derive a usable spectral profile"
        ))
    } else {
        Ok(frames)
    }
}

fn analyze_frame(
    frame: &[f32],
    window: &[f32],
    sample_rate: f64,
    fft: &dyn rustfft::Fft<f32>,
    start_sample: usize,
    available_samples: usize,
) -> Option<FrameAnalysis> {
    let mut bins: Vec<Complex<f32>> = frame
        .iter()
        .zip(window.iter())
        .map(|(sample, win)| Complex::new(sample * win, 0.0))
        .collect();
    fft.process(&mut bins);

    let freq_step = sample_rate / FFT_SIZE as f64;
    let mut energies = [0.0f64; ANALYSIS_BAND_COUNT];
    for (idx, bin) in bins.iter().enumerate().take(FFT_SIZE / 2 + 1).skip(1) {
        let freq = idx as f64 * freq_step;
        if let Some(band_idx) = analysis_band_index(freq) {
            energies[band_idx] += bin.norm_sqr() as f64;
        }
    }

    let total_energy = energies.iter().sum::<f64>();
    if total_energy <= PROFILE_FLOOR || available_samples == 0 {
        None
    } else {
        let mut band_ratios = [0.0f64; ANALYSIS_BAND_COUNT];
        for (idx, energy) in energies.into_iter().enumerate() {
            band_ratios[idx] = energy / total_energy;
        }
        Some(FrameAnalysis {
            band_ratios,
            total_energy,
            start_sample,
            available_samples,
        })
    }
}

fn heuristic_frame_weights(frames: &[FrameAnalysis]) -> Vec<f64> {
    let max_energy_db = frames
        .iter()
        .map(|frame| 10.0 * frame.total_energy.max(PROFILE_FLOOR).log10())
        .fold(f64::NEG_INFINITY, f64::max);

    frames
        .iter()
        .map(|frame| {
            let energy_db = 10.0 * frame.total_energy.max(PROFILE_FLOOR).log10();
            let energy_weight = ((energy_db - (max_energy_db - ENERGY_ACTIVE_RANGE_DB))
                / ENERGY_WEIGHT_SOFTNESS_DB)
                .clamp(0.0, 1.0);
            let speech_core = sum_band_ratios(&frame.band_ratios, &SPEECH_CORE_BANDS);
            let speech_presence = sum_band_ratios(&frame.band_ratios, &SPEECH_PRESENCE_BANDS);
            let low_noise = sum_band_ratios(&frame.band_ratios, &LOW_NOISE_BANDS);
            let high_noise = sum_band_ratios(&frame.band_ratios, &HIGH_NOISE_BANDS);
            let speech_focus = (speech_core * 0.7 + speech_presence * 0.3).clamp(0.0, 1.0);
            let noise_penalty = (low_noise * 0.65 + high_noise * 0.45).clamp(0.0, 1.0);
            let speech_shape = (speech_focus - noise_penalty).clamp(0.0, 1.0);
            energy_weight * (0.05 + 0.95 * speech_shape)
        })
        .collect()
}

fn weighted_band_average(
    frames: &[FrameAnalysis],
    weights: &[f64],
) -> Result<[f64; ANALYSIS_BAND_COUNT]> {
    let mut band_totals = [0.0f64; ANALYSIS_BAND_COUNT];
    let total_weight = weights.iter().sum::<f64>();
    if total_weight <= PROFILE_FLOOR {
        return Err(anyhow!(
            "Audio match could not derive a speech-focused spectral profile"
        ));
    }

    for (frame, weight) in frames.iter().zip(weights.iter()) {
        if *weight <= PROFILE_FLOOR {
            continue;
        }
        for (idx, ratio) in frame.band_ratios.into_iter().enumerate() {
            band_totals[idx] += ratio * *weight;
        }
    }

    let mut averaged = [0.0f64; ANALYSIS_BAND_COUNT];
    for (idx, total) in band_totals.into_iter().enumerate() {
        averaged[idx] = total / total_weight;
    }
    Ok(averaged)
}

fn speech_regions_to_sample_ranges(
    speech_regions: &[AnalysisRegionNs],
    sample_rate: f64,
    max_len_samples: usize,
) -> Vec<(usize, usize)> {
    let mut ranges: Vec<(usize, usize)> = speech_regions
        .iter()
        .filter_map(|region| {
            let start_sample =
                ((region.start_ns as f64 / 1_000_000_000.0) * sample_rate).floor() as usize;
            let end_sample =
                ((region.end_ns as f64 / 1_000_000_000.0) * sample_rate).ceil() as usize;
            let start_sample = start_sample.min(max_len_samples);
            let end_sample = end_sample.min(max_len_samples);
            (end_sample > start_sample).then_some((start_sample, end_sample))
        })
        .collect();
    if ranges.is_empty() {
        return ranges;
    }
    ranges.sort_by_key(|range| range.0);
    let mut merged = Vec::with_capacity(ranges.len());
    let mut current = ranges[0];
    for (start, end) in ranges.into_iter().skip(1) {
        if start <= current.1 {
            current.1 = current.1.max(end);
        } else {
            merged.push(current);
            current = (start, end);
        }
    }
    merged.push(current);
    merged
}

fn frame_region_overlap_weight(frame: &FrameAnalysis, sample_ranges: &[(usize, usize)]) -> f64 {
    let frame_start = frame.start_sample;
    let frame_end = frame.start_sample.saturating_add(frame.available_samples);
    if frame_end <= frame_start {
        return 0.0;
    }
    let overlap = sample_ranges
        .iter()
        .map(|(start, end)| frame_end.min(*end).saturating_sub(frame_start.max(*start)))
        .sum::<usize>();
    overlap as f64 / frame.available_samples as f64
}

fn sum_band_ratios(ratios: &[f64; ANALYSIS_BAND_COUNT], indices: &[usize]) -> f64 {
    indices.iter().map(|&idx| ratios[idx]).sum::<f64>()
}

fn analysis_band_index(freq: f64) -> Option<usize> {
    ANALYSIS_BANDS
        .iter()
        .enumerate()
        .find_map(|(idx, (start_hz, end_hz))| ((*start_hz..*end_hz).contains(&freq)).then_some(idx))
}

fn analysis_band_center_hz(idx: usize) -> f64 {
    let (start_hz, end_hz) = ANALYSIS_BANDS[idx];
    (start_hz * end_hz).sqrt()
}

fn analysis_band_bounds(indices: &[usize]) -> (f64, f64) {
    let first = indices.first().copied().unwrap_or(0);
    let last = indices.last().copied().unwrap_or(first);
    (ANALYSIS_BANDS[first].0, ANALYSIS_BANDS[last].1)
}

fn q_from_octave_spread(octave_spread: f64) -> f64 {
    (1.6 / (octave_spread * 2.8 + 0.55)).clamp(EQ_Q_MIN, EQ_Q_MAX)
}

fn frame_with_padding(samples: &[f32], start: usize, size: usize) -> (Vec<f32>, usize) {
    let mut frame = vec![0.0f32; size];
    let available = samples.len().saturating_sub(start).min(size);
    if available > 0 {
        frame[..available].copy_from_slice(&samples[start..start + available]);
    }
    (frame, available)
}

fn hann_window(size: usize) -> Vec<f32> {
    (0..size)
        .map(|idx| {
            let phase = (2.0 * std::f64::consts::PI * idx as f64) / size as f64;
            (0.5 - 0.5 * phase.cos()) as f32
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sine_wave(freq_hz: f64, seconds: f64, sample_rate: i32) -> Vec<f32> {
        let len = (seconds * sample_rate as f64) as usize;
        (0..len)
            .map(|idx| {
                let t = idx as f64 / sample_rate as f64;
                (2.0 * std::f64::consts::PI * freq_hz * t).sin() as f32
            })
            .collect()
    }

    #[test]
    fn spectral_profile_detects_low_heavy_signal() {
        let samples = sine_wave(180.0, 1.0, SAMPLE_RATE);
        let spectrum = detailed_spectrum_from_samples(&samples, SAMPLE_RATE as f64, &[])
            .expect("low sine should analyse");
        let profile = spectrum.collapsed_profile();
        assert!(profile.low_db > profile.mid_db);
        assert!(profile.low_db > profile.high_db);
    }

    #[test]
    fn spectral_profile_detects_high_heavy_signal() {
        let samples = sine_wave(5_000.0, 1.0, SAMPLE_RATE);
        let spectrum = detailed_spectrum_from_samples(&samples, SAMPLE_RATE as f64, &[])
            .expect("high sine should analyse");
        let profile = spectrum.collapsed_profile();
        assert!(profile.high_db > profile.mid_db);
        assert!(profile.high_db > profile.low_db);
    }

    #[test]
    fn speech_regions_focus_profile_on_selected_dialogue_slice() {
        let mut samples = sine_wave(95.0, 0.6, SAMPLE_RATE);
        samples.extend(sine_wave(2_000.0, 0.6, SAMPLE_RATE));
        let speech_region = AnalysisRegionNs {
            start_ns: 600_000_000,
            end_ns: 1_200_000_000,
        };
        let spectrum =
            detailed_spectrum_from_samples(&samples, SAMPLE_RATE as f64, &[speech_region])
                .expect("subtitle-guided speech slice should analyse");
        let profile = spectrum.collapsed_profile();
        assert!(profile.mid_db > profile.low_db);
        assert!(profile.mid_db > profile.high_db);
    }

    #[test]
    fn heuristic_weighting_downweights_rumble_frames_when_dialogue_is_shorter() {
        let mut samples = sine_wave(90.0, 1.0, SAMPLE_RATE);
        samples.extend(sine_wave(1_000.0, 0.2, SAMPLE_RATE));
        let spectrum = detailed_spectrum_from_samples(&samples, SAMPLE_RATE as f64, &[])
            .expect("heuristic speech weighting should analyse");
        let profile = spectrum.collapsed_profile();
        assert!(profile.mid_db > profile.low_db);
    }

    #[test]
    fn matched_eq_bands_boost_reference_heavy_band_and_cut_source_heavy_band() {
        let source = DetailedSpectrum::from_db_levels([
            -1.0, -1.0, -2.0, -4.0, -6.0, -7.0, -8.0, -9.0, -12.0, -12.0, -12.0,
        ]);
        let reference = DetailedSpectrum::from_db_levels([
            -12.0, -12.0, -11.0, -9.0, -8.0, -7.0, -6.0, -4.0, -2.0, -1.0, -1.0,
        ]);
        let eq_bands = matched_eq_bands(source, reference);
        assert!(eq_bands[0].gain < 0.0);
        assert!(eq_bands[2].gain > 0.0);
        assert!(eq_bands[2].gain.abs() <= EQ_GAIN_LIMIT_DB);
    }

    #[test]
    fn matched_eq_bands_adapt_frequency_to_dominant_mismatch() {
        let source = DetailedSpectrum::from_db_levels([0.0; ANALYSIS_BAND_COUNT]);
        let reference = DetailedSpectrum::from_db_levels([
            8.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0,
        ]);
        let eq_bands = matched_eq_bands(source, reference);
        assert!(eq_bands[0].gain > 0.0);
        assert!(eq_bands[0].freq < 130.0);
        assert!(eq_bands[0].q >= 2.0);
    }

    #[test]
    fn matched_eq_bands_use_narrower_q_for_tighter_mid_peak() {
        let source = DetailedSpectrum::from_db_levels([0.0; ANALYSIS_BAND_COUNT]);
        let narrow_reference = DetailedSpectrum::from_db_levels([
            0.0, 0.0, 0.0, 0.0, 0.0, 8.0, 0.0, 0.0, 0.0, 0.0, 0.0,
        ]);
        let broad_reference = DetailedSpectrum::from_db_levels([
            0.0, 0.0, 0.0, 0.0, 8.0, 8.0, 8.0, 8.0, 0.0, 0.0, 0.0,
        ]);

        let narrow_eq = matched_eq_bands(source, narrow_reference);
        let broad_eq = matched_eq_bands(source, broad_reference);

        assert!(narrow_eq[1].gain > 0.0);
        assert!(broad_eq[1].gain > 0.0);
        assert!(narrow_eq[1].q > broad_eq[1].q);
    }

    #[test]
    fn matched_eq_bands_zero_small_differences() {
        let source = DetailedSpectrum::from_db_levels([0.0; ANALYSIS_BAND_COUNT]);
        let reference = DetailedSpectrum::from_db_levels([
            0.15, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0,
        ]);
        let eq_bands = matched_eq_bands(source, reference);
        assert_eq!(eq_bands[0], default_eq_bands()[0]);
        assert_eq!(eq_bands[1], default_eq_bands()[1]);
        assert_eq!(eq_bands[2], default_eq_bands()[2]);
    }
}
