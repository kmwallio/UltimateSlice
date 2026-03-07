use gstreamer as gst;
use gstreamer::prelude::*;
use gstreamer_app::AppSink;
use rustfft::num_complex::Complex;
use rustfft::FftPlanner;

/// Result of syncing one clip against the anchor.
pub struct AudioSyncResult {
    pub clip_id: String,
    /// Offset in nanoseconds relative to the anchor clip's timeline_start.
    /// Positive means the clip's audio event happens later in its source.
    pub offset_ns: i64,
    /// Confidence score: peak / mean of cross-correlation magnitudes.
    /// Higher = more confident. Values below ~3.0 are likely noise.
    pub confidence: f32,
}

const SAMPLE_RATE: i32 = 22050;
const NS_PER_SAMPLE: f64 = 1_000_000_000.0 / SAMPLE_RATE as f64;

/// Maximum seconds of audio to extract per clip for correlation.
/// 15 seconds provides enough data for reliable sync while keeping
/// FFT sizes manageable even when clips differ greatly in length.
const MAX_EXTRACT_SECONDS: f64 = 15.0;

/// Sync multiple clips by audio cross-correlation against the first (anchor) clip.
///
/// `clips` is a slice of `(clip_id, source_path, source_in_ns, source_out_ns)`.
/// Returns one `AudioSyncResult` per non-anchor clip with the computed offset.
///
/// Uses GCC-PHAT (Generalized Cross-Correlation with Phase Transform) for
/// robust alignment across different microphone types and reverberant environments.
/// Audio is bandpass-filtered to 300–3000 Hz before correlation to focus on the
/// frequency range shared by virtually all microphones.
pub fn sync_clips_by_audio(
    clips: &[(String, String, u64, u64)],
) -> Vec<AudioSyncResult> {
    if clips.len() < 2 {
        return Vec::new();
    }

    let anchor = &clips[0];
    let anchor_audio = match extract_and_prepare(&anchor.1, anchor.2, anchor.3) {
        Some(a) => a,
        None => return Vec::new(),
    };

    let mut results = Vec::with_capacity(clips.len() - 1);
    for clip in &clips[1..] {
        let clip_audio = match extract_and_prepare(&clip.1, clip.2, clip.3) {
            Some(a) => a,
            None => {
                results.push(AudioSyncResult {
                    clip_id: clip.0.clone(),
                    offset_ns: 0,
                    confidence: 0.0,
                });
                continue;
            }
        };

        let (sample_offset, confidence) = gcc_phat(&anchor_audio, &clip_audio);
        let offset_ns = (sample_offset as f64 * NS_PER_SAMPLE) as i64;

        results.push(AudioSyncResult {
            clip_id: clip.0.clone(),
            offset_ns,
            confidence,
        });
    }

    results
}

/// Extract raw audio, then apply bandpass filter for correlation.
fn extract_and_prepare(path: &str, source_in_ns: u64, source_out_ns: u64) -> Option<Vec<f32>> {
    let mut samples = extract_raw_audio(path, source_in_ns, source_out_ns)?;
    bandpass_filter(&mut samples, SAMPLE_RATE as f32, 300.0, 3000.0);
    if samples.iter().all(|&s| s == 0.0) {
        return None;
    }
    Some(samples)
}

// ── Bandpass filter ────────────────────────────────────────────────────────

/// Second-order IIR biquad filter coefficients.
struct Biquad {
    b0: f32,
    b1: f32,
    b2: f32,
    a1: f32,
    a2: f32,
}

impl Biquad {
    /// Design a second-order Butterworth high-pass filter.
    fn highpass(sample_rate: f32, cutoff: f32) -> Self {
        let w0 = 2.0 * std::f32::consts::PI * cutoff / sample_rate;
        let (sin_w0, cos_w0) = w0.sin_cos();
        let alpha = sin_w0 / (2.0 * std::f32::consts::FRAC_1_SQRT_2); // Q = 1/√2
        let a0 = 1.0 + alpha;
        Biquad {
            b0: ((1.0 + cos_w0) / 2.0) / a0,
            b1: (-(1.0 + cos_w0)) / a0,
            b2: ((1.0 + cos_w0) / 2.0) / a0,
            a1: (-2.0 * cos_w0) / a0,
            a2: (1.0 - alpha) / a0,
        }
    }

    /// Design a second-order Butterworth low-pass filter.
    fn lowpass(sample_rate: f32, cutoff: f32) -> Self {
        let w0 = 2.0 * std::f32::consts::PI * cutoff / sample_rate;
        let (sin_w0, cos_w0) = w0.sin_cos();
        let alpha = sin_w0 / (2.0 * std::f32::consts::FRAC_1_SQRT_2);
        let a0 = 1.0 + alpha;
        Biquad {
            b0: ((1.0 - cos_w0) / 2.0) / a0,
            b1: (1.0 - cos_w0) / a0,
            b2: ((1.0 - cos_w0) / 2.0) / a0,
            a1: (-2.0 * cos_w0) / a0,
            a2: (1.0 - alpha) / a0,
        }
    }

    /// Apply filter in-place (Direct Form I).
    fn apply(&self, samples: &mut [f32]) {
        let mut x1 = 0.0f32;
        let mut x2 = 0.0f32;
        let mut y1 = 0.0f32;
        let mut y2 = 0.0f32;
        for s in samples.iter_mut() {
            let x0 = *s;
            let y0 = self.b0 * x0 + self.b1 * x1 + self.b2 * x2
                - self.a1 * y1 - self.a2 * y2;
            x2 = x1;
            x1 = x0;
            y2 = y1;
            y1 = y0;
            *s = y0;
        }
    }
}

/// Apply a bandpass filter (highpass then lowpass) in-place.
/// Focuses audio energy on the 300–3000 Hz range where ambient sound
/// and transients are strongest across all microphone types.
fn bandpass_filter(samples: &mut [f32], sample_rate: f32, low_cut: f32, high_cut: f32) {
    Biquad::highpass(sample_rate, low_cut).apply(samples);
    Biquad::lowpass(sample_rate, high_cut).apply(samples);
}

// ── Audio extraction ───────────────────────────────────────────────────────

/// Extract raw mono F32 audio samples from a media file at 22050 Hz.
/// Caps extraction at MAX_EXTRACT_SECONDS to keep FFT sizes manageable
/// when clips have very different durations.
fn extract_raw_audio(path: &str, source_in_ns: u64, source_out_ns: u64) -> Option<Vec<f32>> {
    let uri = if path.starts_with("file://") {
        path.to_string()
    } else {
        format!("file://{path}")
    };

    let guard = super::PipelineGuard(gst::Pipeline::new());
    let pipeline = &guard.0;

    let src = gst::ElementFactory::make("uridecodebin")
        .property("uri", &uri)
        .build()
        .ok()?;
    let conv = gst::ElementFactory::make("audioconvert").build().ok()?;
    let resample = gst::ElementFactory::make("audioresample").build().ok()?;
    let capsf = gst::ElementFactory::make("capsfilter").build().ok()?;
    let sink = gst::ElementFactory::make("appsink").build().ok()?;

    let caps = gst::Caps::builder("audio/x-raw")
        .field("format", "F32LE")
        .field("channels", 1i32)
        .field("rate", SAMPLE_RATE)
        .build();
    capsf.set_property("caps", &caps);

    let appsink = sink.clone().dynamic_cast::<AppSink>().ok()?;
    appsink.set_property("sync", false);
    appsink.set_property("max-buffers", 200u32);
    appsink.set_property("drop", false);
    appsink.set_property("emit-signals", false);

    if let Ok(src_bin) = src.clone().dynamic_cast::<gst::Bin>() {
        src_bin.connect_element_added(|_, element| {
            if element.find_property("max-threads").is_some() {
                element.set_property_from_str("max-threads", "1");
            }
            if element.find_property("threads").is_some() {
                element.set_property_from_str("threads", "1");
            }
        });
    }

    pipeline
        .add_many([&src, &conv, &resample, &capsf, &sink])
        .ok()?;
    gst::Element::link_many([&conv, &resample, &capsf, &sink]).ok()?;

    {
        let conv = conv.clone();
        src.connect_pad_added(move |_, pad| {
            let caps = pad.current_caps().unwrap_or_else(|| pad.query_caps(None));
            let name = caps.structure(0).map(|s| s.name()).unwrap_or_default();
            if name.starts_with("audio/") {
                let sink_pad = conv.static_pad("sink").unwrap();
                if sink_pad.is_linked() {
                    return;
                }
                let _ = pad.link(&sink_pad);
            }
        });
    }

    pipeline.set_state(gst::State::Playing).ok()?;

    // Seek to source_in if > 0
    if source_in_ns > 0 {
        let _ = pipeline.seek_simple(
            gst::SeekFlags::FLUSH | gst::SeekFlags::KEY_UNIT,
            gst::ClockTime::from_nseconds(source_in_ns),
        );
    }

    // Cap extraction length: use the shorter of clip duration and MAX_EXTRACT_SECONDS
    let clip_duration_s = source_out_ns.saturating_sub(source_in_ns) as f64 / 1_000_000_000.0;
    let extract_s = clip_duration_s.min(MAX_EXTRACT_SECONDS);
    let max_samples =
        (extract_s * SAMPLE_RATE as f64) as usize + SAMPLE_RATE as usize; // small buffer

    let mut samples: Vec<f32> = Vec::new();
    let bus = pipeline.bus()?;

    loop {
        if let Some(s) = appsink.try_pull_sample(gst::ClockTime::from_mseconds(100)) {
            let buffer = s.buffer()?;
            let map = buffer.map_readable().ok()?;
            let raw_bytes = map.as_slice();
            let floats: &[f32] = unsafe {
                std::slice::from_raw_parts(
                    raw_bytes.as_ptr() as *const f32,
                    raw_bytes.len() / 4,
                )
            };
            samples.extend_from_slice(floats);

            if samples.len() >= max_samples {
                break;
            }
        }

        if let Some(msg) = bus.pop() {
            use gst::MessageView;
            match msg.view() {
                MessageView::Eos(_) | MessageView::Error(_) => break,
                _ => {}
            }
        }

        if appsink.is_eos() {
            break;
        }
    }

    // Trim to max_samples
    if samples.len() > max_samples {
        samples.truncate(max_samples);
    }

    if samples.is_empty() {
        return None;
    }

    Some(samples)
}

// ── GCC-PHAT cross-correlation ─────────────────────────────────────────────

/// GCC-PHAT (Generalized Cross-Correlation with Phase Transform).
///
/// Like standard cross-correlation but normalizes the cross-power spectrum
/// by its magnitude: `R = IFFT(FFT(a) * conj(FFT(b)) / |FFT(a) * conj(FFT(b))|)`.
///
/// This produces a much sharper peak than plain cross-correlation, making it
/// robust to:
/// - Different microphone frequency responses (GoPro vs. Rode vs. lav mic)
/// - Room reverb and echo
/// - Background noise differences
/// - Different recording levels
///
/// Returns (sample_offset, confidence).
fn gcc_phat(a: &[f32], b: &[f32]) -> (i64, f32) {
    let n = a.len() + b.len() - 1;
    let fft_len = n.next_power_of_two();

    let mut planner = FftPlanner::<f32>::new();
    let fft = planner.plan_fft_forward(fft_len);
    let ifft = planner.plan_fft_inverse(fft_len);

    // Prepare complex buffers, zero-padded to fft_len
    let mut fa: Vec<Complex<f32>> = Vec::with_capacity(fft_len);
    for &v in a {
        fa.push(Complex::new(v, 0.0));
    }
    fa.resize(fft_len, Complex::new(0.0, 0.0));

    let mut fb: Vec<Complex<f32>> = Vec::with_capacity(fft_len);
    for &v in b {
        fb.push(Complex::new(v, 0.0));
    }
    fb.resize(fft_len, Complex::new(0.0, 0.0));

    // Forward FFT
    fft.process(&mut fa);
    fft.process(&mut fb);

    // Cross-power spectrum with smoothed PHAT normalization.
    // Uses |cross|^β with β=0.73 instead of full |cross| to retain some
    // magnitude weighting — pure PHAT (β=1) is too aggressive for signals
    // with narrow spectral support, while β≈0.73 gives sharp peaks and
    // still works across different mic frequency responses.
    let epsilon = 1e-10f32;
    let beta = 0.73f32;
    let mut r: Vec<Complex<f32>> = fa
        .iter()
        .zip(fb.iter())
        .map(|(a, b)| {
            let cross = a * b.conj();
            let mag = cross.norm();
            let denom = mag.powf(beta);
            if denom > epsilon {
                cross / denom
            } else {
                Complex::new(0.0, 0.0)
            }
        })
        .collect();

    // Inverse FFT
    ifft.process(&mut r);

    // Find peak
    let mut peak_idx = 0usize;
    let mut peak_mag = 0.0f32;
    let mut sum_mag = 0.0f32;

    for (i, c) in r.iter().enumerate() {
        let mag = c.norm();
        sum_mag += mag;
        if mag > peak_mag {
            peak_mag = mag;
            peak_idx = i;
        }
    }

    // Convert circular index to signed lag
    let sample_offset = if peak_idx <= fft_len / 2 {
        peak_idx as i64
    } else {
        peak_idx as i64 - fft_len as i64
    };

    let mean_mag = if r.len() > 1 {
        (sum_mag - peak_mag) / (r.len() - 1) as f32
    } else {
        1.0
    };
    let confidence = if mean_mag > 0.0 {
        (peak_mag / mean_mag).min(100.0)
    } else {
        100.0
    };

    (sample_offset, confidence)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Generate pseudo-random broadband noise (simulates real audio).
    fn pseudo_noise(len: usize, seed: u32) -> Vec<f32> {
        let mut x = seed;
        (0..len)
            .map(|_| {
                x = x.wrapping_mul(1103515245).wrapping_add(12345);
                (x >> 16) as i16 as f32 / 32768.0
            })
            .collect()
    }

    #[test]
    fn gcc_phat_identical_signals() {
        let signal = pseudo_noise(4000, 42);
        let (offset, confidence) = gcc_phat(&signal, &signal);
        assert_eq!(offset, 0, "offset={offset}");
        assert!(confidence > 2.0, "confidence={confidence}");
    }

    #[test]
    fn gcc_phat_shifted_signal() {
        let shift = 100;
        let base = pseudo_noise(5000, 123);
        let a = &base[..3000];
        let b = &base[shift..3000 + shift];
        let (offset, confidence) = gcc_phat(a, b);
        assert!(
            (offset - shift as i64).abs() <= 2,
            "offset={offset}, expected ~{shift}"
        );
        assert!(confidence > 2.0, "confidence={confidence}");
    }

    #[test]
    fn gcc_phat_very_different_lengths() {
        // Short 500-sample clip vs long 5000-sample clip with shared content
        let shift = 200;
        let base = pseudo_noise(8000, 456);
        let long = &base[..5000];
        let short = &base[shift..shift + 500];
        let (offset, confidence) = gcc_phat(long, short);
        assert!(
            (offset - shift as i64).abs() <= 2,
            "offset={offset}, expected ~{shift}"
        );
        assert!(confidence > 2.0, "confidence={confidence}");
    }

    #[test]
    fn gcc_phat_negative_offset() {
        // b starts before a in the shared content
        let shift = 150;
        let base = pseudo_noise(6000, 789);
        let a = &base[shift..shift + 3000];
        let b = &base[..3000];
        let (offset, _confidence) = gcc_phat(a, b);
        // b's content appears `shift` samples earlier, so offset should be ~ -shift
        assert!(
            (offset + shift as i64).abs() <= 2,
            "offset={offset}, expected ~-{shift}"
        );
    }

    #[test]
    fn bandpass_preserves_mid_frequencies() {
        let sr = 22050.0f32;
        // 1 kHz sine — should pass through
        let mut mid: Vec<f32> = (0..2000)
            .map(|i| (2.0 * std::f32::consts::PI * 1000.0 * i as f32 / sr).sin())
            .collect();
        let energy_before: f32 = mid.iter().map(|s| s * s).sum();
        bandpass_filter(&mut mid, sr, 300.0, 3000.0);
        let energy_after: f32 = mid.iter().map(|s| s * s).sum();
        assert!(
            energy_after > energy_before * 0.5,
            "mid-band energy ratio = {:.2}",
            energy_after / energy_before
        );
    }

    #[test]
    fn bandpass_attenuates_extremes() {
        let sr = 22050.0f32;
        // 50 Hz sine — should be attenuated
        let mut low: Vec<f32> = (0..4000)
            .map(|i| (2.0 * std::f32::consts::PI * 50.0 * i as f32 / sr).sin())
            .collect();
        let energy_before: f32 = low.iter().map(|s| s * s).sum();
        bandpass_filter(&mut low, sr, 300.0, 3000.0);
        let energy_after: f32 = low.iter().map(|s| s * s).sum();
        assert!(
            energy_after < energy_before * 0.3,
            "50Hz energy ratio = {:.2} (expected < 0.3)",
            energy_after / energy_before
        );
    }
}
