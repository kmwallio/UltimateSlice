//! Color matching: sample frames from two clips and compute adjustments
//! (slider values + optional 3D LUT) that make one clip's colors resemble another.
//!
//! The algorithm uses a Reinhard-style statistical color transfer in CIE L*a*b*
//! space: match per-channel mean and standard deviation between source and
//! reference clips, then map the resulting deltas back to the existing clip
//! color parameters (brightness, contrast, saturation, temperature, tint, etc.).
//!
//! An optional second pass generates a 3D LUT (.cube) for finer non-linear
//! matching that sliders alone cannot express.

use anyhow::{anyhow, Result};
use gstreamer as gst;
use gstreamer::prelude::*;
use gstreamer_app::AppSink;

use crate::media::cube_lut::CubeLut;
use crate::media::thumbnail::path_to_uri;
use crate::model::clip::Clip;

// ---------------------------------------------------------------------------
// Frame sampling
// ---------------------------------------------------------------------------

/// Resolution used for sampled frames (matches ScopeFrame dimensions).
const SAMPLE_WIDTH: u32 = 320;
const SAMPLE_HEIGHT: u32 = 180;

/// A raw RGBA frame sampled from a source file.
pub struct SampledFrame {
    pub data: Vec<u8>,
    pub width: u32,
    pub height: u32,
}

/// Extract `count` evenly-spaced RGBA frames from `source_path` between
/// `source_in_ns` and `source_out_ns`. Returns up to `count` frames
/// (fewer if the clip is very short or seeking fails for some positions).
pub fn extract_frames_rgba(
    source_path: &str,
    source_in_ns: u64,
    source_out_ns: u64,
    count: usize,
) -> Result<Vec<SampledFrame>> {
    if count == 0 {
        return Ok(Vec::new());
    }
    let duration = source_out_ns.saturating_sub(source_in_ns);
    if duration == 0 {
        return Err(anyhow!("clip has zero duration"));
    }

    gst::init()?;
    let uri = path_to_uri(source_path);

    let pipeline_desc = format!(
        "uridecodebin uri=\"{uri}\" ! videoconvert ! videoscale ! \
         video/x-raw,format=RGBA,width={SAMPLE_WIDTH},height={SAMPLE_HEIGHT} ! \
         appsink name=sink sync=false"
    );

    let guard = super::PipelineGuard(
        gst::parse::launch(&pipeline_desc)?
            .downcast::<gst::Pipeline>()
            .map_err(|_| anyhow!("not a pipeline"))?,
    );
    let pipeline = &guard.0;

    let appsink = pipeline
        .by_name("sink")
        .ok_or_else(|| anyhow!("no appsink"))?
        .downcast::<AppSink>()
        .map_err(|_| anyhow!("not an appsink"))?;

    appsink.set_property("max-buffers", 1u32);
    appsink.set_property("drop", true);

    pipeline.set_state(gst::State::Paused)?;
    let (res, _, _) = pipeline.state(Some(gst::ClockTime::from_seconds(10)));
    res.map_err(|e| anyhow!("pipeline failed to reach Paused: {e:?}"))?;

    let mut frames = Vec::with_capacity(count);

    // Compute seek positions evenly spaced within the source window.
    let step = if count == 1 {
        0
    } else {
        duration / (count as u64)
    };

    for i in 0..count {
        let seek_ns = source_in_ns + step * (i as u64) + step / 2;
        let seek_ns = seek_ns.min(source_out_ns.saturating_sub(1));

        if pipeline
            .seek_simple(
                gst::SeekFlags::FLUSH | gst::SeekFlags::KEY_UNIT,
                gst::ClockTime::from_nseconds(seek_ns),
            )
            .is_err()
        {
            log::warn!("color_match: seek to {seek_ns} ns failed, skipping");
            continue;
        }

        // Brief state-change wait for the seek to settle.
        let _ = pipeline.state(Some(gst::ClockTime::from_seconds(2)));

        // Pull one frame after seek.
        pipeline.set_state(gst::State::Playing)?;
        match appsink.pull_sample() {
            Ok(sample) => {
                if let Some(buf) = sample.buffer() {
                    if let Ok(map) = buf.map_readable() {
                        frames.push(SampledFrame {
                            data: map.as_slice().to_vec(),
                            width: SAMPLE_WIDTH,
                            height: SAMPLE_HEIGHT,
                        });
                    }
                }
            }
            Err(_) => {
                log::warn!("color_match: failed to pull frame at {seek_ns} ns");
            }
        }
        pipeline.set_state(gst::State::Paused)?;
        let _ = pipeline.state(Some(gst::ClockTime::from_seconds(2)));
    }

    // PipelineGuard sets pipeline to Null on drop.
    Ok(frames)
}

// ---------------------------------------------------------------------------
// CIE L*a*b* conversion (BT.709 sRGB → XYZ D65 → Lab)
// ---------------------------------------------------------------------------

/// sRGB gamma → linear.
#[inline]
fn srgb_to_linear(c: f64) -> f64 {
    if c <= 0.04045 {
        c / 12.92
    } else {
        ((c + 0.055) / 1.055).powf(2.4)
    }
}

/// Linear → sRGB gamma.
#[inline]
fn linear_to_srgb(c: f64) -> f64 {
    if c <= 0.0031308 {
        c * 12.92
    } else {
        1.055 * c.powf(1.0 / 2.4) - 0.055
    }
}

/// Convert linear RGB (0–1) to CIE XYZ D65 using sRGB/BT.709 matrix.
#[inline]
fn linear_rgb_to_xyz(r: f64, g: f64, b: f64) -> (f64, f64, f64) {
    let x = 0.4124564 * r + 0.3575761 * g + 0.1804375 * b;
    let y = 0.2126729 * r + 0.7151522 * g + 0.0721750 * b;
    let z = 0.0193339 * r + 0.1191920 * g + 0.9503041 * b;
    (x, y, z)
}

/// Convert CIE XYZ D65 to linear RGB (sRGB/BT.709 matrix inverse).
#[inline]
fn xyz_to_linear_rgb(x: f64, y: f64, z: f64) -> (f64, f64, f64) {
    let r = 3.2404542 * x - 1.5371385 * y - 0.4985314 * z;
    let g = -0.9692660 * x + 1.8760108 * y + 0.0415560 * z;
    let b = 0.0556434 * x - 0.2040259 * y + 1.0572252 * z;
    (r, g, b)
}

/// D65 white point.
const XN: f64 = 0.95047;
const YN: f64 = 1.00000;
const ZN: f64 = 1.08883;

/// CIE Lab transfer function.
#[inline]
fn lab_f(t: f64) -> f64 {
    const DELTA: f64 = 6.0 / 29.0;
    if t > DELTA * DELTA * DELTA {
        t.cbrt()
    } else {
        t / (3.0 * DELTA * DELTA) + 4.0 / 29.0
    }
}

/// Inverse CIE Lab transfer function.
#[inline]
fn lab_f_inv(t: f64) -> f64 {
    const DELTA: f64 = 6.0 / 29.0;
    if t > DELTA {
        t * t * t
    } else {
        3.0 * DELTA * DELTA * (t - 4.0 / 29.0)
    }
}

/// Convert sRGB (0–255) to CIE L*a*b*.
#[inline]
pub fn srgb_to_lab(r: u8, g: u8, b: u8) -> (f64, f64, f64) {
    let rl = srgb_to_linear(r as f64 / 255.0);
    let gl = srgb_to_linear(g as f64 / 255.0);
    let bl = srgb_to_linear(b as f64 / 255.0);
    let (x, y, z) = linear_rgb_to_xyz(rl, gl, bl);
    let fx = lab_f(x / XN);
    let fy = lab_f(y / YN);
    let fz = lab_f(z / ZN);
    let l = 116.0 * fy - 16.0;
    let a = 500.0 * (fx - fy);
    let b_ch = 200.0 * (fy - fz);
    (l, a, b_ch)
}

/// Convert CIE L*a*b* to sRGB (0–255, clamped).
#[inline]
pub fn lab_to_srgb(l: f64, a: f64, b_ch: f64) -> (u8, u8, u8) {
    let fy = (l + 16.0) / 116.0;
    let fx = a / 500.0 + fy;
    let fz = fy - b_ch / 200.0;
    let x = XN * lab_f_inv(fx);
    let y = YN * lab_f_inv(fy);
    let z = ZN * lab_f_inv(fz);
    let (rl, gl, bl) = xyz_to_linear_rgb(x, y, z);
    let r = (linear_to_srgb(rl) * 255.0 + 0.5).clamp(0.0, 255.0) as u8;
    let g = (linear_to_srgb(gl) * 255.0 + 0.5).clamp(0.0, 255.0) as u8;
    let b = (linear_to_srgb(bl) * 255.0 + 0.5).clamp(0.0, 255.0) as u8;
    (r, g, b)
}

// ---------------------------------------------------------------------------
// Color statistics
// ---------------------------------------------------------------------------

/// Per-channel mean and standard deviation in Lab and RGB spaces.
#[derive(Debug, Clone)]
pub struct ColorStats {
    pub mean_l: f64,
    pub mean_a: f64,
    pub mean_b: f64,
    pub std_l: f64,
    pub std_a: f64,
    pub std_b: f64,
    pub mean_r: f64,
    pub mean_g: f64,
    pub mean_b_ch: f64,
    pub std_r: f64,
    pub std_g: f64,
    pub std_b_ch: f64,
}

/// Compute aggregate color statistics from one or more RGBA frames.
pub fn compute_color_stats(frames: &[SampledFrame]) -> Result<ColorStats> {
    if frames.is_empty() {
        return Err(anyhow!("no frames to analyse"));
    }

    let total_pixels: usize = frames
        .iter()
        .map(|f| (f.width as usize) * (f.height as usize))
        .sum();

    if total_pixels == 0 {
        return Err(anyhow!("frames contain no pixels"));
    }

    // Accumulate sums for mean computation.
    let mut sum_l = 0.0_f64;
    let mut sum_a = 0.0_f64;
    let mut sum_b = 0.0_f64;
    let mut sum_r = 0.0_f64;
    let mut sum_g = 0.0_f64;
    let mut sum_b_ch = 0.0_f64;

    // Accumulate sums of squares for std dev.
    let mut sq_l = 0.0_f64;
    let mut sq_a = 0.0_f64;
    let mut sq_b = 0.0_f64;
    let mut sq_r = 0.0_f64;
    let mut sq_g = 0.0_f64;
    let mut sq_b_ch = 0.0_f64;

    for frame in frames {
        for pixel in frame.data.chunks_exact(4) {
            let (r, g, b) = (pixel[0], pixel[1], pixel[2]);
            let (l, a, b_val) = srgb_to_lab(r, g, b);

            let rf = r as f64 / 255.0;
            let gf = g as f64 / 255.0;
            let bf = b as f64 / 255.0;

            sum_l += l;
            sum_a += a;
            sum_b += b_val;
            sum_r += rf;
            sum_g += gf;
            sum_b_ch += bf;

            sq_l += l * l;
            sq_a += a * a;
            sq_b += b_val * b_val;
            sq_r += rf * rf;
            sq_g += gf * gf;
            sq_b_ch += bf * bf;
        }
    }

    let n = total_pixels as f64;
    let mean_l = sum_l / n;
    let mean_a = sum_a / n;
    let mean_b = sum_b / n;
    let mean_r = sum_r / n;
    let mean_g = sum_g / n;
    let mean_b_ch = sum_b_ch / n;

    // Variance = E[X²] - E[X]²; std = sqrt(max(0, variance)) for safety.
    let std_l = (sq_l / n - mean_l * mean_l).max(0.0).sqrt();
    let std_a = (sq_a / n - mean_a * mean_a).max(0.0).sqrt();
    let std_b = (sq_b / n - mean_b * mean_b).max(0.0).sqrt();
    let std_r = (sq_r / n - mean_r * mean_r).max(0.0).sqrt();
    let std_g = (sq_g / n - mean_g * mean_g).max(0.0).sqrt();
    let std_b_ch = (sq_b_ch / n - mean_b_ch * mean_b_ch).max(0.0).sqrt();

    Ok(ColorStats {
        mean_l,
        mean_a,
        mean_b,
        std_l,
        std_a,
        std_b,
        mean_r,
        mean_g,
        mean_b_ch,
        std_r,
        std_g,
        std_b_ch,
    })
}

// ---------------------------------------------------------------------------
// Slider estimation
// ---------------------------------------------------------------------------

/// Result of matching a source clip's color to a reference clip.
/// Contains suggested adjustments for the existing clip color parameters.
#[derive(Debug, Clone)]
pub struct MatchColorResult {
    pub brightness: f32,
    pub contrast: f32,
    pub saturation: f32,
    pub temperature: f32,
    pub tint: f32,
    pub exposure: f32,
    pub black_point: f32,
    pub shadows: f32,
    pub midtones: f32,
    pub highlights: f32,
    pub highlights_warmth: f32,
    pub highlights_tint: f32,
    pub midtones_warmth: f32,
    pub midtones_tint: f32,
    pub shadows_warmth: f32,
    pub shadows_tint: f32,
}

/// Estimate clip color parameter adjustments that would shift `source_stats`
/// towards `reference_stats`.
pub fn estimate_slider_adjustments(
    source_stats: &ColorStats,
    reference_stats: &ColorStats,
) -> MatchColorResult {
    // --- Brightness ---
    // L* ranges from 0–100.  Map mean-L* delta to brightness range (−1..1).
    let l_delta = reference_stats.mean_l - source_stats.mean_l;
    let brightness = (l_delta / 100.0).clamp(-1.0, 1.0) as f32;

    // --- Contrast ---
    // Ratio of L* standard deviations → contrast multiplier (0..2).
    let contrast = if source_stats.std_l > 0.001 {
        let ratio = reference_stats.std_l / source_stats.std_l;
        ratio.clamp(0.2, 2.0) as f32
    } else {
        1.0
    };

    // --- Saturation ---
    // Chroma magnitude = sqrt(a² + b²). Ratio of mean chroma → saturation.
    let src_chroma =
        (source_stats.std_a * source_stats.std_a + source_stats.std_b * source_stats.std_b).sqrt();
    let ref_chroma = (reference_stats.std_a * reference_stats.std_a
        + reference_stats.std_b * reference_stats.std_b)
        .sqrt();
    let saturation = if src_chroma > 0.001 {
        let ratio = ref_chroma / src_chroma;
        ratio.clamp(0.0, 2.0) as f32
    } else {
        1.0
    };

    // --- Temperature ---
    // Lab b* axis: positive = yellow (warm), negative = blue (cool).
    // Map b* mean delta to Kelvin offset from neutral 6500K.
    // Empirical scale: ±50 Lab b* units ≈ full 2000–10000K range.
    let b_delta = reference_stats.mean_b - source_stats.mean_b;
    let kelvin_shift = b_delta * (4000.0 / 50.0); // ~80K per Lab b* unit
    let temperature = (6500.0 + kelvin_shift).clamp(2000.0, 10000.0) as f32;

    // --- Tint ---
    // Lab a* axis: positive = red/magenta, negative = green.
    // Map a* mean delta to tint range (−1..1).
    let a_delta = reference_stats.mean_a - source_stats.mean_a;
    let tint = (a_delta / 40.0).clamp(-1.0, 1.0) as f32;

    // --- Exposure ---
    // Use overall luminance delta scaled differently from brightness.
    // Exposure is more like a stop-based adjustment.
    let exposure = (l_delta / 50.0).clamp(-1.0, 1.0) as f32;

    // Leave tonal zone controls at neutral for slider-based matching.
    // These require more sophisticated analysis (per-zone histograms)
    // that would be better addressed by the LUT path.
    MatchColorResult {
        brightness,
        contrast,
        saturation,
        temperature,
        tint,
        exposure,
        black_point: 0.0,
        shadows: 0.0,
        midtones: 0.0,
        highlights: 0.0,
        highlights_warmth: 0.0,
        highlights_tint: 0.0,
        midtones_warmth: 0.0,
        midtones_tint: 0.0,
        shadows_warmth: 0.0,
        shadows_tint: 0.0,
    }
}

/// Apply a `MatchColorResult` to a clip, overwriting its color parameters.
/// Returns `true` if any field actually changed.
pub fn apply_match_result(clip: &mut Clip, result: &MatchColorResult) -> bool {
    let before = clip.clone();
    clip.brightness = result.brightness;
    clip.contrast = result.contrast;
    clip.saturation = result.saturation;
    clip.temperature = result.temperature;
    clip.tint = result.tint;
    clip.exposure = result.exposure;
    clip.black_point = result.black_point;
    clip.shadows = result.shadows;
    clip.midtones = result.midtones;
    clip.highlights = result.highlights;
    clip.highlights_warmth = result.highlights_warmth;
    clip.highlights_tint = result.highlights_tint;
    clip.midtones_warmth = result.midtones_warmth;
    clip.midtones_tint = result.midtones_tint;
    clip.shadows_warmth = result.shadows_warmth;
    clip.shadows_tint = result.shadows_tint;
    before != *clip
}

// ---------------------------------------------------------------------------
// 3D LUT generation (Reinhard transfer in Lab space)
// ---------------------------------------------------------------------------

/// Default LUT grid size for color matching (17³ = 4913 entries).
pub const MATCH_LUT_SIZE: usize = 17;

/// Generate a 3D LUT that applies a Reinhard-style color transfer from
/// `source_stats` to `reference_stats` in Lab space.
pub fn generate_match_lut(
    source_stats: &ColorStats,
    reference_stats: &ColorStats,
    size: usize,
) -> CubeLut {
    let n = size as f64 - 1.0;
    let total = size * size * size;
    let mut data = Vec::with_capacity(total);

    for bi in 0..size {
        for gi in 0..size {
            for ri in 0..size {
                // Grid point in sRGB 0–255 space.
                let r8 = ((ri as f64 / n) * 255.0 + 0.5) as u8;
                let g8 = ((gi as f64 / n) * 255.0 + 0.5) as u8;
                let b8 = ((bi as f64 / n) * 255.0 + 0.5) as u8;

                // Convert to Lab.
                let (l, a, b_val) = srgb_to_lab(r8, g8, b8);

                // Reinhard transfer: shift mean + scale std dev.
                let l_out = transfer_channel(
                    l,
                    source_stats.mean_l,
                    source_stats.std_l,
                    reference_stats.mean_l,
                    reference_stats.std_l,
                );
                let a_out = transfer_channel(
                    a,
                    source_stats.mean_a,
                    source_stats.std_a,
                    reference_stats.mean_a,
                    reference_stats.std_a,
                );
                let b_out = transfer_channel(
                    b_val,
                    source_stats.mean_b,
                    source_stats.std_b,
                    reference_stats.mean_b,
                    reference_stats.std_b,
                );

                // Convert back to sRGB.
                let (ro, go, bo) = lab_to_srgb(l_out, a_out, b_out);

                // Store as 0–1 float triplet for .cube format.
                data.push([ro as f32 / 255.0, go as f32 / 255.0, bo as f32 / 255.0]);
            }
        }
    }

    CubeLut {
        size,
        domain_min: [0.0, 0.0, 0.0],
        domain_max: [1.0, 1.0, 1.0],
        data,
    }
}

/// Single-channel Reinhard transfer: subtract source mean, scale by std ratio,
/// add reference mean.
#[inline]
fn transfer_channel(
    val: f64,
    src_mean: f64,
    src_std: f64,
    ref_mean: f64,
    ref_std: f64,
) -> f64 {
    if src_std < 0.001 {
        // Source has near-zero variance — just shift to reference mean.
        val - src_mean + ref_mean
    } else {
        (val - src_mean) * (ref_std / src_std) + ref_mean
    }
}

// ---------------------------------------------------------------------------
// High-level orchestration
// ---------------------------------------------------------------------------

/// Parameters for a match-color operation.
pub struct MatchColorParams {
    /// Source clip (the one being adjusted).
    pub source_path: String,
    pub source_in_ns: u64,
    pub source_out_ns: u64,
    /// Reference clip (the look to match).
    pub reference_path: String,
    pub reference_in_ns: u64,
    pub reference_out_ns: u64,
    /// Number of frames to sample from each clip.
    pub sample_count: usize,
    /// Whether to generate a .cube LUT in addition to slider adjustments.
    pub generate_lut: bool,
    /// Directory where the generated LUT file should be saved.
    pub lut_output_dir: Option<String>,
}

/// Outcome of a match-color operation.
pub struct MatchColorOutcome {
    pub slider_result: MatchColorResult,
    pub source_stats: ColorStats,
    pub reference_stats: ColorStats,
    pub lut_path: Option<String>,
}

/// Run the full match-color pipeline: sample frames, compute stats,
/// estimate slider adjustments, and optionally generate a LUT.
///
/// This function blocks (GStreamer pipeline I/O) and should be called
/// from a background thread.
pub fn run_match_color(params: &MatchColorParams) -> Result<MatchColorOutcome> {
    let sample_count = params.sample_count.max(1).min(20);

    log::info!(
        "color_match: sampling {} frames from source={} and reference={}",
        sample_count,
        params.source_path,
        params.reference_path,
    );

    let source_frames = extract_frames_rgba(
        &params.source_path,
        params.source_in_ns,
        params.source_out_ns,
        sample_count,
    )?;
    if source_frames.is_empty() {
        return Err(anyhow!("failed to sample any frames from source clip"));
    }

    let reference_frames = extract_frames_rgba(
        &params.reference_path,
        params.reference_in_ns,
        params.reference_out_ns,
        sample_count,
    )?;
    if reference_frames.is_empty() {
        return Err(anyhow!(
            "failed to sample any frames from reference clip"
        ));
    }

    let source_stats = compute_color_stats(&source_frames)?;
    let reference_stats = compute_color_stats(&reference_frames)?;

    log::info!(
        "color_match: source  L*={:.1}±{:.1} a*={:.1}±{:.1} b*={:.1}±{:.1}",
        source_stats.mean_l,
        source_stats.std_l,
        source_stats.mean_a,
        source_stats.std_a,
        source_stats.mean_b,
        source_stats.std_b,
    );
    log::info!(
        "color_match: ref     L*={:.1}±{:.1} a*={:.1}±{:.1} b*={:.1}±{:.1}",
        reference_stats.mean_l,
        reference_stats.std_l,
        reference_stats.mean_a,
        reference_stats.std_a,
        reference_stats.mean_b,
        reference_stats.std_b,
    );

    let slider_result = estimate_slider_adjustments(&source_stats, &reference_stats);

    let lut_path = if params.generate_lut {
        let dir = params
            .lut_output_dir
            .as_deref()
            .unwrap_or("/tmp");
        let filename = format!(
            "ultimateslice-color-match-{}.cube",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0)
        );
        let path = format!("{}/{}", dir, filename);

        log::info!("color_match: generating {MATCH_LUT_SIZE}³ LUT → {path}");
        let lut = generate_match_lut(&source_stats, &reference_stats, MATCH_LUT_SIZE);
        lut.write_cube_file(std::path::Path::new(&path))
            .map_err(|e| anyhow!("{e}"))?;
        Some(path)
    } else {
        None
    };

    Ok(MatchColorOutcome {
        slider_result,
        source_stats,
        reference_stats,
        lut_path,
    })
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn srgb_lab_roundtrip() {
        // Test that sRGB → Lab → sRGB is lossless (within ±1 due to rounding).
        for r in (0..=255).step_by(17) {
            for g in (0..=255).step_by(17) {
                for b in (0..=255).step_by(17) {
                    let (l, a, bv) = srgb_to_lab(r as u8, g as u8, b as u8);
                    let (r2, g2, b2) = lab_to_srgb(l, a, bv);
                    assert!(
                        (r2 as i16 - r as i16).unsigned_abs() <= 1
                            && (g2 as i16 - g as i16).unsigned_abs() <= 1
                            && (b2 as i16 - b as i16).unsigned_abs() <= 1,
                        "roundtrip failed: ({r},{g},{b}) → L={l:.2} a={a:.2} b={bv:.2} → ({r2},{g2},{b2})"
                    );
                }
            }
        }
    }

    #[test]
    fn lab_known_values() {
        // Black → L*=0, a*=0, b*=0
        let (l, a, b) = srgb_to_lab(0, 0, 0);
        assert!((l - 0.0).abs() < 0.1, "black L*={l}");
        assert!(a.abs() < 0.1, "black a*={a}");
        assert!(b.abs() < 0.1, "black b*={b}");

        // White → L*≈100
        let (l, a, b) = srgb_to_lab(255, 255, 255);
        assert!((l - 100.0).abs() < 0.5, "white L*={l}");
        assert!(a.abs() < 1.0, "white a*={a}");
        assert!(b.abs() < 1.0, "white b*={b}");

        // Pure red → L*≈53.2, a*≈80.1, b*≈67.2
        let (l, a, b) = srgb_to_lab(255, 0, 0);
        assert!((l - 53.2).abs() < 1.0, "red L*={l}");
        assert!((a - 80.1).abs() < 1.0, "red a*={a}");
        assert!((b - 67.2).abs() < 1.0, "red b*={b}");
    }

    #[test]
    fn compute_stats_uniform_color() {
        // A single frame of solid grey (128,128,128).
        let w = 4u32;
        let h = 4u32;
        let pixel = [128u8, 128, 128, 255];
        let data: Vec<u8> = pixel.iter().copied().cycle().take((w * h * 4) as usize).collect();
        let frame = SampledFrame {
            data,
            width: w,
            height: h,
        };
        let stats = compute_color_stats(&[frame]).unwrap();

        // Uniform color → std dev ≈ 0.
        assert!(stats.std_l < 0.01, "uniform std_l={}", stats.std_l);
        assert!(stats.std_a < 0.01, "uniform std_a={}", stats.std_a);
        assert!(stats.std_b < 0.01, "uniform std_b={}", stats.std_b);

        // L* for (128,128,128) ≈ 53.6.
        assert!(
            (stats.mean_l - 53.6).abs() < 1.0,
            "mean_l={}",
            stats.mean_l
        );
    }

    #[test]
    fn estimate_sliders_identical_clips() {
        let stats = ColorStats {
            mean_l: 50.0,
            mean_a: 0.0,
            mean_b: 0.0,
            std_l: 20.0,
            std_a: 10.0,
            std_b: 10.0,
            mean_r: 0.5,
            mean_g: 0.5,
            mean_b_ch: 0.5,
            std_r: 0.1,
            std_g: 0.1,
            std_b_ch: 0.1,
        };
        let result = estimate_slider_adjustments(&stats, &stats);

        // When source and reference are identical, adjustments should be neutral.
        assert!(
            result.brightness.abs() < 0.01,
            "brightness={}",
            result.brightness
        );
        assert!(
            (result.contrast - 1.0).abs() < 0.01,
            "contrast={}",
            result.contrast
        );
        assert!(
            (result.saturation - 1.0).abs() < 0.01,
            "saturation={}",
            result.saturation
        );
        assert!(
            (result.temperature - 6500.0).abs() < 1.0,
            "temperature={}",
            result.temperature
        );
        assert!(result.tint.abs() < 0.01, "tint={}", result.tint);
    }

    #[test]
    fn estimate_sliders_bright_to_dark() {
        let bright = ColorStats {
            mean_l: 80.0,
            mean_a: 0.0,
            mean_b: 0.0,
            std_l: 15.0,
            std_a: 5.0,
            std_b: 5.0,
            mean_r: 0.7,
            mean_g: 0.7,
            mean_b_ch: 0.7,
            std_r: 0.1,
            std_g: 0.1,
            std_b_ch: 0.1,
        };
        let dark = ColorStats {
            mean_l: 30.0,
            mean_a: 0.0,
            mean_b: 0.0,
            std_l: 15.0,
            std_a: 5.0,
            std_b: 5.0,
            mean_r: 0.3,
            mean_g: 0.3,
            mean_b_ch: 0.3,
            std_r: 0.1,
            std_g: 0.1,
            std_b_ch: 0.1,
        };

        let result = estimate_slider_adjustments(&bright, &dark);
        // Should suggest darkening (negative brightness).
        assert!(result.brightness < -0.1, "brightness={}", result.brightness);
        // Contrast should stay ~1.0 (same std dev).
        assert!(
            (result.contrast - 1.0).abs() < 0.1,
            "contrast={}",
            result.contrast
        );
    }

    #[test]
    fn estimate_sliders_warm_to_cool() {
        let warm = ColorStats {
            mean_l: 50.0,
            mean_a: 0.0,
            mean_b: 20.0, // Yellow-shifted
            std_l: 15.0,
            std_a: 5.0,
            std_b: 5.0,
            mean_r: 0.5,
            mean_g: 0.5,
            mean_b_ch: 0.4,
            std_r: 0.1,
            std_g: 0.1,
            std_b_ch: 0.1,
        };
        let cool = ColorStats {
            mean_l: 50.0,
            mean_a: 0.0,
            mean_b: -20.0, // Blue-shifted
            std_l: 15.0,
            std_a: 5.0,
            std_b: 5.0,
            mean_r: 0.4,
            mean_g: 0.5,
            mean_b_ch: 0.6,
            std_r: 0.1,
            std_g: 0.1,
            std_b_ch: 0.1,
        };

        let result = estimate_slider_adjustments(&warm, &cool);
        // Should suggest cooler temperature (lower Kelvin).
        assert!(
            result.temperature < 6500.0,
            "temperature={}",
            result.temperature
        );
    }

    #[test]
    fn generate_identity_lut() {
        // When source and reference stats are identical, the LUT should be
        // nearly an identity transform.
        let stats = ColorStats {
            mean_l: 50.0,
            mean_a: 0.0,
            mean_b: 0.0,
            std_l: 20.0,
            std_a: 10.0,
            std_b: 10.0,
            mean_r: 0.5,
            mean_g: 0.5,
            mean_b_ch: 0.5,
            std_r: 0.1,
            std_g: 0.1,
            std_b_ch: 0.1,
        };
        let lut = generate_match_lut(&stats, &stats, 9);
        assert_eq!(lut.size, 9);
        assert_eq!(lut.data.len(), 9 * 9 * 9);

        // Check that mid-grey passes through approximately unchanged.
        let mut buf = vec![128u8, 128, 128, 255];
        lut.apply_to_rgba_buffer(&mut buf);
        let (r, g, b) = (buf[0], buf[1], buf[2]);
        assert!(
            (r as i16 - 128).unsigned_abs() <= 2,
            "identity LUT mid-grey: r={r}"
        );
        assert!(
            (g as i16 - 128).unsigned_abs() <= 2,
            "identity LUT mid-grey: g={g}"
        );
        assert!(
            (b as i16 - 128).unsigned_abs() <= 2,
            "identity LUT mid-grey: b={b}"
        );
    }

    #[test]
    fn transfer_channel_basic() {
        // No-op when stats are the same.
        let v = transfer_channel(50.0, 50.0, 10.0, 50.0, 10.0);
        assert!((v - 50.0).abs() < 0.001);

        // Shift mean: 50 → 60 (same std).
        let v = transfer_channel(50.0, 50.0, 10.0, 60.0, 10.0);
        assert!((v - 60.0).abs() < 0.001);

        // Scale std: std 10→20, value at +1 std → should be at +2 std = 70.
        let v = transfer_channel(60.0, 50.0, 10.0, 50.0, 20.0);
        assert!((v - 70.0).abs() < 0.001, "scaled transfer: v={v}");
    }
}
