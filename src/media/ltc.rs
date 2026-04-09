use crate::model::clip::AudioChannelMode;
use crate::model::project::FrameRate;

const LTC_SAMPLE_RATE: i32 = 48_000;
const LTC_MAX_ANALYZE_SECONDS: f64 = 10.0;
const LTC_SYNC_BITS: [u8; 16] = [0, 0, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 0, 1];
const EDGE_ENTER_RATIO: f32 = 0.2;
const EDGE_EXIT_RATIO: f32 = 0.08;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LtcChannelSelection {
    Auto,
    Left,
    Right,
    MonoMix,
}

impl LtcChannelSelection {
    pub const ALL: [LtcChannelSelection; 4] = [
        LtcChannelSelection::Auto,
        LtcChannelSelection::Left,
        LtcChannelSelection::Right,
        LtcChannelSelection::MonoMix,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::Auto => "Auto Detect",
            Self::Left => "Left Channel",
            Self::Right => "Right Channel",
            Self::MonoMix => "Mono Mix",
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Left => "left",
            Self::Right => "right",
            Self::MonoMix => "mono_mix",
        }
    }

    pub fn from_str(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "auto" | "" => Some(Self::Auto),
            "left" => Some(Self::Left),
            "right" => Some(Self::Right),
            "mono" | "monomix" | "mono_mix" | "mix" => Some(Self::MonoMix),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResolvedLtcChannel {
    Left,
    Right,
    MonoMix,
}

impl ResolvedLtcChannel {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Left => "left",
            Self::Right => "right",
            Self::MonoMix => "mono_mix",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LtcAudioRepair {
    pub channel_mode: Option<AudioChannelMode>,
    pub mute: bool,
}

impl LtcAudioRepair {
    pub fn description(self) -> &'static str {
        if self.mute {
            "muted clip audio"
        } else {
            match self.channel_mode.unwrap_or(AudioChannelMode::Stereo) {
                AudioChannelMode::Left => "using left-channel program audio",
                AudioChannelMode::Right => "using right-channel program audio",
                AudioChannelMode::MonoMix => "using mono-mix program audio",
                AudioChannelMode::Stereo => "using stereo program audio",
            }
        }
    }
}

#[derive(Debug, Clone)]
pub struct LtcDecodeResult {
    pub source_timecode_base_ns: u64,
    pub resolved_channel: ResolvedLtcChannel,
    pub channel_count: usize,
    pub frame_start_sample: usize,
    pub decoded_frame_count: usize,
}

#[derive(Debug, Clone)]
struct DecodedBit {
    value: u8,
    start_sample: usize,
}

#[derive(Debug, Clone)]
struct CandidateDecode {
    source_timecode_base_ns: u64,
    frame_start_sample: usize,
    decoded_frame_count: usize,
}

pub fn decode_ltc_from_clip(
    path: &str,
    source_in_ns: u64,
    source_out_ns: u64,
    selection: LtcChannelSelection,
    frame_rate: &FrameRate,
) -> Result<LtcDecodeResult, String> {
    let _ = gstreamer::init();
    let (interleaved, channel_count) = super::audio_sync::extract_interleaved_audio_samples(
        path,
        source_in_ns,
        source_out_ns,
        LTC_SAMPLE_RATE,
        LTC_MAX_ANALYZE_SECONDS,
    )
    .ok_or_else(|| "Could not read audio samples for LTC decoding.".to_string())?;
    if channel_count == 0 {
        return Err("Source clip does not contain decodable audio channels.".to_string());
    }

    let candidates = candidate_channel_samples(&interleaved, channel_count, selection);
    for (resolved_channel, samples) in candidates {
        if let Ok(candidate) = decode_ltc_from_samples(&samples, LTC_SAMPLE_RATE, frame_rate) {
            return Ok(LtcDecodeResult {
                source_timecode_base_ns: candidate.source_timecode_base_ns,
                resolved_channel,
                channel_count,
                frame_start_sample: candidate.frame_start_sample,
                decoded_frame_count: candidate.decoded_frame_count,
            });
        }
    }

    Err("No LTC signal was detected in the selected audio.".to_string())
}

pub fn audio_repair_for_ltc_channel(
    channel_count: usize,
    resolved_channel: ResolvedLtcChannel,
) -> LtcAudioRepair {
    if channel_count <= 1 {
        return LtcAudioRepair {
            channel_mode: None,
            mute: true,
        };
    }

    match resolved_channel {
        ResolvedLtcChannel::Left => LtcAudioRepair {
            channel_mode: Some(AudioChannelMode::Right),
            mute: false,
        },
        ResolvedLtcChannel::Right => LtcAudioRepair {
            channel_mode: Some(AudioChannelMode::Left),
            mute: false,
        },
        ResolvedLtcChannel::MonoMix => LtcAudioRepair {
            channel_mode: None,
            mute: true,
        },
    }
}

fn candidate_channel_samples(
    interleaved: &[f32],
    channel_count: usize,
    selection: LtcChannelSelection,
) -> Vec<(ResolvedLtcChannel, Vec<f32>)> {
    if channel_count <= 1 {
        let mono =
            super::audio_sync::mix_down_interleaved_audio_samples(interleaved, channel_count);
        return vec![(ResolvedLtcChannel::MonoMix, mono)];
    }

    match selection {
        LtcChannelSelection::Auto => vec![
            (
                ResolvedLtcChannel::Left,
                super::audio_sync::extract_interleaved_audio_channel(interleaved, channel_count, 0),
            ),
            (
                ResolvedLtcChannel::Right,
                super::audio_sync::extract_interleaved_audio_channel(interleaved, channel_count, 1),
            ),
            (
                ResolvedLtcChannel::MonoMix,
                super::audio_sync::mix_down_interleaved_audio_samples(interleaved, channel_count),
            ),
        ],
        LtcChannelSelection::Left => vec![(
            ResolvedLtcChannel::Left,
            super::audio_sync::extract_interleaved_audio_channel(interleaved, channel_count, 0),
        )],
        LtcChannelSelection::Right => vec![(
            ResolvedLtcChannel::Right,
            super::audio_sync::extract_interleaved_audio_channel(interleaved, channel_count, 1),
        )],
        LtcChannelSelection::MonoMix => vec![(
            ResolvedLtcChannel::MonoMix,
            super::audio_sync::mix_down_interleaved_audio_samples(interleaved, channel_count),
        )],
    }
}

fn decode_ltc_from_samples(
    samples: &[f32],
    sample_rate: i32,
    frame_rate: &FrameRate,
) -> Result<CandidateDecode, String> {
    let transitions = detect_transitions(samples)?;
    let half_bit_samples = expected_half_bit_samples(sample_rate, frame_rate)?;

    let mut best: Option<CandidateDecode> = None;
    for start_offset in 0..2 {
        let bits = decode_bits_from_transitions(&transitions, start_offset, half_bit_samples);
        if let Some(candidate) = find_best_candidate(&bits, sample_rate, frame_rate) {
            let replace = best.as_ref().is_none_or(|current| {
                candidate.decoded_frame_count > current.decoded_frame_count
                    || (candidate.decoded_frame_count == current.decoded_frame_count
                        && candidate.frame_start_sample < current.frame_start_sample)
            });
            if replace {
                best = Some(candidate);
            }
        }
    }

    best.ok_or_else(|| "No decodable LTC frames were found.".to_string())
}

fn detect_transitions(samples: &[f32]) -> Result<Vec<usize>, String> {
    let peak = samples
        .iter()
        .copied()
        .fold(0.0_f32, |max_value, sample| max_value.max(sample.abs()));
    if peak < 0.01 {
        return Err("Audio is too quiet for LTC decoding.".to_string());
    }

    let enter_threshold = peak * EDGE_ENTER_RATIO;
    let exit_threshold = peak * EDGE_EXIT_RATIO;
    let mut state = None;
    let mut transitions = Vec::new();

    for (idx, sample) in samples.iter().copied().enumerate() {
        match state {
            None => {
                if sample >= enter_threshold {
                    state = Some(true);
                } else if sample <= -enter_threshold {
                    state = Some(false);
                }
            }
            Some(true) => {
                if sample <= -exit_threshold {
                    state = Some(false);
                    transitions.push(idx);
                }
            }
            Some(false) => {
                if sample >= exit_threshold {
                    state = Some(true);
                    transitions.push(idx);
                }
            }
        }
    }

    if transitions.len() < 160 {
        return Err("Not enough LTC transitions were detected.".to_string());
    }

    Ok(transitions)
}

fn expected_half_bit_samples(sample_rate: i32, frame_rate: &FrameRate) -> Result<f64, String> {
    let fps = frame_rate.as_f64();
    if fps <= 0.0 {
        return Err("Frame rate must be greater than zero.".to_string());
    }
    let half_bit_samples = sample_rate as f64 / (fps * 160.0);
    if half_bit_samples < 2.0 {
        return Err(
            "Frame rate is too high for LTC decoding at the current sample rate.".to_string(),
        );
    }
    Ok(half_bit_samples)
}

fn decode_bits_from_transitions(
    transitions: &[usize],
    start_offset: usize,
    half_bit_samples: f64,
) -> Vec<DecodedBit> {
    let mut decoded = Vec::new();
    let min_half = half_bit_samples * 0.35;
    let max_half = half_bit_samples * 1.65;
    let max_full = half_bit_samples * 2.85;
    let mut index = start_offset;

    while index + 1 < transitions.len() {
        let interval = (transitions[index + 1] - transitions[index]) as f64;
        if interval < min_half || interval > max_full {
            index += 1;
            continue;
        }

        if interval <= max_half {
            if index + 2 >= transitions.len() {
                break;
            }
            let next_interval = (transitions[index + 2] - transitions[index + 1]) as f64;
            if next_interval >= min_half && next_interval <= max_half {
                decoded.push(DecodedBit {
                    value: 1,
                    start_sample: transitions[index],
                });
                index += 2;
                continue;
            }
            index += 1;
            continue;
        }

        decoded.push(DecodedBit {
            value: 0,
            start_sample: transitions[index],
        });
        index += 1;
    }

    decoded
}

fn find_best_candidate(
    bits: &[DecodedBit],
    sample_rate: i32,
    frame_rate: &FrameRate,
) -> Option<CandidateDecode> {
    if bits.len() < 80 {
        return None;
    }

    let mut decoded_frames = Vec::new();
    for sync_start in 64..=bits.len().saturating_sub(16) {
        if bits[sync_start..sync_start + 16]
            .iter()
            .zip(LTC_SYNC_BITS)
            .all(|(bit, sync)| bit.value == sync)
        {
            let frame_bits = &bits[sync_start - 64..sync_start];
            if let Some(frame_timecode_ns) = decode_frame_timecode_ns(frame_bits, frame_rate) {
                let frame_start_sample =
                    frame_bits.first().map(|bit| bit.start_sample).unwrap_or(0);
                let base_ns = frame_timecode_ns
                    .saturating_sub(samples_to_ns(frame_start_sample, sample_rate));
                decoded_frames.push((frame_start_sample, base_ns));
            }
        }
    }

    decoded_frames
        .first()
        .map(|(frame_start_sample, base_ns)| CandidateDecode {
            source_timecode_base_ns: *base_ns,
            frame_start_sample: *frame_start_sample,
            decoded_frame_count: decoded_frames.len(),
        })
}

fn decode_frame_timecode_ns(bits: &[DecodedBit], frame_rate: &FrameRate) -> Option<u64> {
    if bits.len() != 64 {
        return None;
    }

    let frames = read_bits(bits, &[0, 1, 2, 3]) + 10 * read_bits(bits, &[8, 9]);
    let seconds = read_bits(bits, &[16, 17, 18, 19]) + 10 * read_bits(bits, &[24, 25, 26]);
    let minutes = read_bits(bits, &[32, 33, 34, 35]) + 10 * read_bits(bits, &[40, 41, 42]);
    let hours = read_bits(bits, &[48, 49, 50, 51]) + 10 * read_bits(bits, &[56, 57]);
    let nominal_fps = nominal_fps(frame_rate);

    if frames >= nominal_fps || seconds >= 60 || minutes >= 60 || hours >= 24 {
        return None;
    }

    let total_frames = (((hours as u64 * 60 + minutes as u64) * 60 + seconds as u64)
        * nominal_fps as u64)
        + frames as u64;
    let fps_num = u128::from(frame_rate.numerator.max(1));
    let fps_den = u128::from(frame_rate.denominator.max(1));
    let ns = (u128::from(total_frames) * fps_den * 1_000_000_000u128) / fps_num;
    Some(ns.min(u128::from(u64::MAX)) as u64)
}

fn read_bits(bits: &[DecodedBit], positions: &[usize]) -> u8 {
    positions
        .iter()
        .enumerate()
        .fold(0u8, |value, (shift, position)| {
            value | (bits[*position].value << shift)
        })
}

fn nominal_fps(frame_rate: &FrameRate) -> u8 {
    frame_rate.as_f64().round().max(1.0) as u8
}

fn samples_to_ns(sample_index: usize, sample_rate: i32) -> u64 {
    let ns = (u128::from(sample_index as u64) * 1_000_000_000u128) / u128::from(sample_rate as u32);
    ns.min(u128::from(u64::MAX)) as u64
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fps_24() -> FrameRate {
        FrameRate {
            numerator: 24,
            denominator: 1,
        }
    }

    fn fps_30() -> FrameRate {
        FrameRate {
            numerator: 30,
            denominator: 1,
        }
    }

    fn write_bits(frame: &mut [u8; 80], value: u8, positions: &[usize]) {
        for (shift, position) in positions.iter().enumerate() {
            frame[*position] = (value >> shift) & 1;
        }
    }

    fn build_ltc_frame(hours: u8, minutes: u8, seconds: u8, frames: u8) -> [u8; 80] {
        let mut frame = [0u8; 80];
        write_bits(&mut frame, frames % 10, &[0, 1, 2, 3]);
        write_bits(&mut frame, frames / 10, &[8, 9]);
        write_bits(&mut frame, seconds % 10, &[16, 17, 18, 19]);
        write_bits(&mut frame, seconds / 10, &[24, 25, 26]);
        write_bits(&mut frame, minutes % 10, &[32, 33, 34, 35]);
        write_bits(&mut frame, minutes / 10, &[40, 41, 42]);
        write_bits(&mut frame, hours % 10, &[48, 49, 50, 51]);
        write_bits(&mut frame, hours / 10, &[56, 57]);
        for (idx, bit) in LTC_SYNC_BITS.iter().enumerate() {
            frame[64 + idx] = *bit;
        }
        frame
    }

    fn synthesize_ltc_samples(
        frames: &[[u8; 80]],
        sample_rate: i32,
        frame_rate: &FrameRate,
        leading_silence_samples: usize,
    ) -> Vec<f32> {
        let half_bit_samples = expected_half_bit_samples(sample_rate, frame_rate).unwrap();
        let mut signal = Vec::new();
        let mut emitted_segments = 0usize;
        let mut current_level = -1.0f32;

        for frame in frames {
            for bit in frame {
                current_level = -current_level;
                emitted_segments += 1;
                append_segment(
                    &mut signal,
                    current_level,
                    half_bit_samples,
                    emitted_segments,
                );
                if *bit == 1 {
                    current_level = -current_level;
                }
                emitted_segments += 1;
                append_segment(
                    &mut signal,
                    current_level,
                    half_bit_samples,
                    emitted_segments,
                );
            }
        }

        let mut samples = vec![0.0; leading_silence_samples];
        samples.extend(signal);
        samples
    }

    fn append_segment(
        samples: &mut Vec<f32>,
        level: f32,
        half_bit_samples: f64,
        emitted_segments: usize,
    ) {
        let target_len = (emitted_segments as f64 * half_bit_samples).round() as usize;
        let count = target_len.saturating_sub(samples.len());
        if count > 0 {
            samples.extend(std::iter::repeat_n(level, count));
        }
    }

    #[test]
    fn decodes_generated_ltc_frame() {
        let frame_rate = fps_24();
        let frames = [
            build_ltc_frame(1, 2, 3, 4),
            build_ltc_frame(1, 2, 3, 5),
            build_ltc_frame(1, 2, 3, 6),
        ];
        let samples = synthesize_ltc_samples(&frames, LTC_SAMPLE_RATE, &frame_rate, 0);

        let decoded = decode_ltc_from_samples(&samples, LTC_SAMPLE_RATE, &frame_rate)
            .expect("ltc should decode");

        let expected = (((1u64 * 60 + 2) * 60 + 3) * 24 + 4) * 1_000_000_000 / 24;
        assert!(decoded.source_timecode_base_ns.abs_diff(expected) <= 1);
        assert!(decoded.decoded_frame_count >= 1);
    }

    #[test]
    fn decodes_ltc_with_leading_silence_into_source_base() {
        let frame_rate = fps_30();
        let leading_samples = 24_000usize;
        let frames = [
            build_ltc_frame(0, 1, 0, 0),
            build_ltc_frame(0, 1, 0, 1),
            build_ltc_frame(0, 1, 0, 2),
        ];
        let samples =
            synthesize_ltc_samples(&frames, LTC_SAMPLE_RATE, &frame_rate, leading_samples);

        let decoded = decode_ltc_from_samples(&samples, LTC_SAMPLE_RATE, &frame_rate)
            .expect("ltc should decode with leading silence");

        let frame_ns = 60_000_000_000u64;
        let expected = frame_ns.saturating_sub(samples_to_ns(leading_samples, LTC_SAMPLE_RATE));
        assert!(decoded.source_timecode_base_ns.abs_diff(expected) <= 1);
        assert!(decoded.frame_start_sample >= leading_samples);
    }

    #[test]
    fn audio_repair_routes_other_channel_or_mutes() {
        assert_eq!(
            audio_repair_for_ltc_channel(2, ResolvedLtcChannel::Left),
            LtcAudioRepair {
                channel_mode: Some(AudioChannelMode::Right),
                mute: false,
            }
        );
        assert_eq!(
            audio_repair_for_ltc_channel(2, ResolvedLtcChannel::Right),
            LtcAudioRepair {
                channel_mode: Some(AudioChannelMode::Left),
                mute: false,
            }
        );
        assert_eq!(
            audio_repair_for_ltc_channel(1, ResolvedLtcChannel::MonoMix),
            LtcAudioRepair {
                channel_mode: None,
                mute: true,
            }
        );
    }
}
