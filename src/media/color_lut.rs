//! Compiled per-clip colour LUT: the preview's colour chain, measured.
//!
//! The Program Monitor grades a clip through a chain of pointwise stages —
//! user `.cube` LUTs (CPU pad probe), GStreamer `videobalance`, frei0r
//! `coloradj_RGB`, frei0r `3-point-color-balance`, and the HSL qualifier
//! (CPU pad probe). Export, Render-and-Replace bakes and background
//! prerender used to re-express each stage as an FFmpeg filter (`eq`,
//! `colortemperature` / frei0r bridge, `lutrgb`, …) bridged by calibration
//! polynomials, so the two never matched exactly.
//!
//! This module instead *measures* the preview chain: a Hald image holding
//! every 8-bit RGB triple is pushed through the very same GStreamer elements,
//! configured by the same parameter functions `build_effects_bin` uses, with
//! the CPU stages applied around it exactly as the pad probes do. The result
//! is the preview's transfer function on every integer input; it is sampled
//! onto a `COLOR_LUT_SIZE`³ `.cube` that FFmpeg applies with one `lut3d`
//! stage. Whatever the editor shows for a static grade is, by construction,
//! what export renders.
//!
//! Scope: static grades only. A clip with keyframes on brightness, contrast,
//! saturation, temperature or tint keeps the per-stage filter chain (the
//! callers check `clip_color_is_static` / `program_clip_color_is_static`).
//!
//! Compiled LUTs are cached under `$XDG_CACHE_HOME/ultimateslice/color_luts/`
//! keyed by a SHA-256 of every input (slider values, LUT files with their
//! size and mtime, HSL fields, which GStreamer elements were available).

use std::path::{Path, PathBuf};

use gstreamer as gst;
use gstreamer::prelude::*;
use gstreamer_app::{AppSink, AppSrc};
use gstreamer_video::prelude::*;

use crate::media::color_math;
use crate::media::cube_lut::CubeLut;
use crate::media::hsl_qualifier::apply_hsl_qualifier_to_rgba_buffer;
use crate::media::program_player::ProgramClip;
use crate::model::clip::{Clip, HslQualifier};

/// Grid size of the compiled LUT (`COLOR_LUT_SIZE`³ entries, ~7 MB `.cube`).
///
/// 65 puts a node every 255/64 ≈ 4 code values, and its nodes coincide with
/// those of the usual 17³ / 33³ / 65³ user LUTs, so a user LUT that the
/// preview interpolates trilinearly is reproduced exactly rather than
/// resampled.
pub const COLOR_LUT_SIZE: usize = 65;

/// Upper bound on compiled LUT files kept in the cache directory.
const COLOR_LUT_CACHE_MAX_FILES: usize = 24;

/// Side of the Hald measurement image: 4096² pixels = every 8-bit RGB triple.
const HALD_SIDE: usize = 4096;

/// Everything that determines a clip's static colour transfer function, in
/// the order the preview applies it.
#[derive(Clone, Debug, PartialEq)]
pub struct ColorGradeSpec {
    pub brightness: f64,
    pub contrast: f64,
    pub saturation: f64,
    pub exposure: f64,
    pub temperature: f64,
    pub tint: f64,
    pub shadows: f64,
    pub midtones: f64,
    pub highlights: f64,
    pub black_point: f64,
    pub highlights_warmth: f64,
    pub highlights_tint: f64,
    pub midtones_warmth: f64,
    pub midtones_tint: f64,
    pub shadows_warmth: f64,
    pub shadows_tint: f64,
    /// User `.cube` LUTs, applied first, in order.
    pub lut_paths: Vec<String>,
    /// Secondary colour correction, applied last.
    pub hsl_qualifier: Option<HslQualifier>,
}

impl ColorGradeSpec {
    /// Neutral spec (identity transfer).
    pub fn neutral() -> Self {
        Self {
            brightness: 0.0,
            contrast: 1.0,
            saturation: 1.0,
            exposure: 0.0,
            temperature: 6500.0,
            tint: 0.0,
            shadows: 0.0,
            midtones: 0.0,
            highlights: 0.0,
            black_point: 0.0,
            highlights_warmth: 0.0,
            highlights_tint: 0.0,
            midtones_warmth: 0.0,
            midtones_tint: 0.0,
            shadows_warmth: 0.0,
            shadows_tint: 0.0,
            lut_paths: Vec::new(),
            hsl_qualifier: None,
        }
    }

    pub fn from_clip(clip: &Clip) -> Self {
        Self {
            brightness: clip.brightness as f64,
            contrast: clip.contrast as f64,
            saturation: clip.saturation as f64,
            exposure: clip.exposure as f64,
            temperature: clip.temperature as f64,
            tint: clip.tint as f64,
            shadows: clip.shadows as f64,
            midtones: clip.midtones as f64,
            highlights: clip.highlights as f64,
            black_point: clip.black_point as f64,
            highlights_warmth: clip.highlights_warmth as f64,
            highlights_tint: clip.highlights_tint as f64,
            midtones_warmth: clip.midtones_warmth as f64,
            midtones_tint: clip.midtones_tint as f64,
            shadows_warmth: clip.shadows_warmth as f64,
            shadows_tint: clip.shadows_tint as f64,
            lut_paths: existing_lut_paths(&clip.lut_paths),
            hsl_qualifier: clip.hsl_qualifier.clone().filter(|q| !q.is_neutral()),
        }
    }

    /// `include_luts` is false when the source already carries the LUTs
    /// (a LUT-baked proxy), matching `prerender_build_lut_filter`.
    pub fn from_program_clip(clip: &ProgramClip, include_luts: bool) -> Self {
        Self {
            brightness: clip.brightness,
            contrast: clip.contrast,
            saturation: clip.saturation,
            exposure: clip.exposure,
            temperature: clip.temperature,
            tint: clip.tint,
            shadows: clip.shadows,
            midtones: clip.midtones,
            highlights: clip.highlights,
            black_point: clip.black_point,
            highlights_warmth: clip.highlights_warmth,
            highlights_tint: clip.highlights_tint,
            midtones_warmth: clip.midtones_warmth,
            midtones_tint: clip.midtones_tint,
            shadows_warmth: clip.shadows_warmth,
            shadows_tint: clip.shadows_tint,
            lut_paths: if include_luts {
                existing_lut_paths(&clip.lut_paths)
            } else {
                Vec::new()
            },
            hsl_qualifier: clip.hsl_qualifier.clone().filter(|q| !q.is_neutral()),
        }
    }

    /// Same predicate as `build_effects_bin`'s `need_balance` (static part).
    pub fn needs_videobalance(&self) -> bool {
        self.brightness != 0.0
            || self.contrast != 1.0
            || self.saturation != 1.0
            || self.exposure.abs() > f64::EPSILON
    }

    /// Same predicate as `build_effects_bin`'s `need_coloradj` (static part).
    pub fn needs_coloradj(&self) -> bool {
        (self.temperature - 6500.0).abs() > 1.0 || self.tint.abs() > 0.001
    }

    /// Same predicate as `build_effects_bin`'s `need_3point`.
    pub fn needs_3point(&self) -> bool {
        self.shadows.abs() > f64::EPSILON
            || self.midtones.abs() > f64::EPSILON
            || self.highlights.abs() > f64::EPSILON
            || self.black_point.abs() > f64::EPSILON
            || self.highlights_warmth.abs() > f64::EPSILON
            || self.highlights_tint.abs() > f64::EPSILON
            || self.midtones_warmth.abs() > f64::EPSILON
            || self.midtones_tint.abs() > f64::EPSILON
            || self.shadows_warmth.abs() > f64::EPSILON
            || self.shadows_tint.abs() > f64::EPSILON
    }

    pub fn needs_gst_stage(&self) -> bool {
        self.needs_videobalance() || self.needs_coloradj() || self.needs_3point()
    }

    pub fn is_identity(&self) -> bool {
        !self.needs_gst_stage() && self.lut_paths.is_empty() && self.hsl_qualifier.is_none()
    }
}

fn existing_lut_paths(paths: &[String]) -> Vec<String> {
    paths
        .iter()
        .filter(|p| !p.is_empty() && Path::new(p).exists())
        .cloned()
        .collect()
}

/// Kill switch for bisecting: with `ULTIMATESLICE_DISABLE_COLOR_LUT=1` every
/// caller keeps the per-stage FFmpeg colour chain.
pub fn color_lut_disabled_by_env() -> bool {
    std::env::var_os("ULTIMATESLICE_DISABLE_COLOR_LUT")
        .map(|v| !v.is_empty() && v != "0")
        .unwrap_or(false)
}

/// True when none of the colour sliders the LUT folds are keyframed.
pub fn clip_color_is_static(clip: &Clip) -> bool {
    clip.brightness_keyframes.is_empty()
        && clip.contrast_keyframes.is_empty()
        && clip.saturation_keyframes.is_empty()
        && clip.temperature_keyframes.is_empty()
        && clip.tint_keyframes.is_empty()
}

/// `clip_color_is_static` for flattened preview clips.
pub fn program_clip_color_is_static(clip: &ProgramClip) -> bool {
    clip.brightness_keyframes.is_empty()
        && clip.contrast_keyframes.is_empty()
        && clip.saturation_keyframes.is_empty()
        && clip.temperature_keyframes.is_empty()
        && clip.tint_keyframes.is_empty()
}

// ── Measurement ────────────────────────────────────────────────────────────

/// Which GStreamer elements the chain could instantiate. Folded into the cache
/// key because `build_effects_bin` falls back to videobalance approximations
/// when a frei0r element is missing, and the LUT must mirror that.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ChainElements {
    coloradj: bool,
    three_point: bool,
}

fn probe_chain_elements() -> ChainElements {
    if gst::init().is_err() {
        return ChainElements {
            coloradj: false,
            three_point: false,
        };
    }
    ChainElements {
        coloradj: gst::ElementFactory::find("frei0r-filter-coloradj-rgb").is_some(),
        three_point: gst::ElementFactory::find("frei0r-filter-3-point-color-balance").is_some(),
    }
}

/// Fill a `HALD_SIDE`² RGBA buffer so pixel `p = r + 256·g + 65536·b` holds
/// `(r, g, b, 255)`.
fn hald_identity() -> Vec<u8> {
    let mut buf = vec![0u8; HALD_SIDE * HALD_SIDE * 4];
    for (p, px) in buf.chunks_exact_mut(4).enumerate() {
        px[0] = (p & 0xff) as u8;
        px[1] = ((p >> 8) & 0xff) as u8;
        px[2] = ((p >> 16) & 0xff) as u8;
        px[3] = 255;
    }
    buf
}

/// Push `rgba` through `videoconvert ! [videobalance] ! [coloradj_RGB] !
/// [3-point-color-balance] ! videoconvert ! RGBA`, configured exactly like
/// the slot chain, and return the processed RGBA buffer.
fn run_gst_color_chain(
    rgba: Vec<u8>,
    width: usize,
    height: usize,
    spec: &ColorGradeSpec,
) -> Result<Vec<u8>, String> {
    gst::init().map_err(|e| format!("gstreamer init: {e}"))?;

    let pipeline = gst::Pipeline::new();
    let caps = gst::Caps::builder("video/x-raw")
        .field("format", "RGBA")
        .field("width", width as i32)
        .field("height", height as i32)
        .field("framerate", gst::Fraction::new(1, 1))
        .build();
    let appsrc = AppSrc::builder()
        .caps(&caps)
        .format(gst::Format::Time)
        .is_live(false)
        .build();
    let conv_in = gst::ElementFactory::make("videoconvert")
        .build()
        .map_err(|_| "videoconvert not available".to_string())?;

    // Mirror build_effects_bin: frei0r elements first, videobalance picks up
    // the hue-rotation approximations only for stages whose element is
    // missing.
    let coloradj = if spec.needs_coloradj() {
        gst::ElementFactory::make("frei0r-filter-coloradj-rgb")
            .build()
            .ok()
    } else {
        None
    };
    let three_point = if spec.needs_3point() {
        let elem = gst::ElementFactory::make("frei0r-filter-3-point-color-balance")
            .build()
            .ok();
        if let Some(ref e) = elem {
            e.set_property("split-preview", false);
        }
        elem
    } else {
        None
    };
    let need_balance = spec.needs_videobalance()
        || (spec.needs_coloradj() && coloradj.is_none())
        || (spec.needs_3point() && three_point.is_none());
    let videobalance = if need_balance {
        Some(
            gst::ElementFactory::make("videobalance")
                .build()
                .map_err(|_| "videobalance not available".to_string())?,
        )
    } else {
        None
    };

    if let Some(ref vb) = videobalance {
        let p = color_math::compute_videobalance_params(
            spec.brightness,
            spec.contrast,
            spec.saturation,
            spec.temperature,
            spec.tint,
            spec.shadows,
            spec.midtones,
            spec.highlights,
            spec.exposure,
            spec.black_point,
            spec.highlights_warmth,
            spec.highlights_tint,
            spec.midtones_warmth,
            spec.midtones_tint,
            spec.shadows_warmth,
            spec.shadows_tint,
            coloradj.is_some(),
            three_point.is_some(),
        );
        vb.set_property("brightness", p.brightness);
        vb.set_property("contrast", p.contrast);
        vb.set_property("saturation", p.saturation);
        vb.set_property("hue", p.hue);
    }
    if let Some(ref ca) = coloradj {
        ca.set_property("action", 0.333_f64);
        ca.set_property("keep-luma", false);
        let cp = color_math::compute_coloradj_params(spec.temperature, spec.tint);
        ca.set_property("r", cp.r);
        ca.set_property("g", cp.g);
        ca.set_property("b", cp.b);
    }
    if let Some(ref tp) = three_point {
        let p = color_math::compute_3point_params(
            spec.shadows,
            spec.midtones,
            spec.highlights,
            spec.black_point,
            spec.highlights_warmth,
            spec.highlights_tint,
            spec.midtones_warmth,
            spec.midtones_tint,
            spec.shadows_warmth,
            spec.shadows_tint,
        );
        tp.set_property("black-color-r", p.black_r as f32);
        tp.set_property("black-color-g", p.black_g as f32);
        tp.set_property("black-color-b", p.black_b as f32);
        tp.set_property("gray-color-r", p.gray_r as f32);
        tp.set_property("gray-color-g", p.gray_g as f32);
        tp.set_property("gray-color-b", p.gray_b as f32);
        tp.set_property("white-color-r", p.white_r as f32);
        tp.set_property("white-color-g", p.white_g as f32);
        tp.set_property("white-color-b", p.white_b as f32);
    }

    let conv_out = gst::ElementFactory::make("videoconvert")
        .build()
        .map_err(|_| "videoconvert not available".to_string())?;
    let out_caps = gst::Caps::builder("video/x-raw")
        .field("format", "RGBA")
        .field("width", width as i32)
        .field("height", height as i32)
        .build();
    let appsink = AppSink::builder()
        .caps(&out_caps)
        .sync(false)
        .max_buffers(1)
        .build();

    let mut chain: Vec<gst::Element> = vec![appsrc.clone().upcast(), conv_in];
    if let Some(e) = videobalance {
        chain.push(e);
    }
    if let Some(e) = coloradj {
        chain.push(e);
    }
    if let Some(e) = three_point {
        chain.push(e);
    }
    chain.push(conv_out);
    chain.push(appsink.clone().upcast());
    let refs: Vec<&gst::Element> = chain.iter().collect();
    pipeline
        .add_many(&refs)
        .map_err(|e| format!("pipeline add: {e}"))?;
    gst::Element::link_many(&refs).map_err(|e| format!("pipeline link: {e}"))?;

    let guard = crate::media::PipelineGuard(pipeline);
    let pipeline = &guard.0;
    pipeline
        .set_state(gst::State::Playing)
        .map_err(|e| format!("pipeline → Playing: {e}"))?;

    let mut buffer = gst::Buffer::from_mut_slice(rgba);
    {
        let b = buffer.get_mut().unwrap();
        b.set_pts(gst::ClockTime::ZERO);
        b.set_duration(gst::ClockTime::from_seconds(1));
    }
    appsrc
        .push_buffer(buffer)
        .map_err(|e| format!("appsrc push: {e:?}"))?;
    let _ = appsrc.end_of_stream();

    let sample = appsink
        .try_pull_sample(gst::ClockTime::from_seconds(20))
        .ok_or_else(|| "no frame came out of the colour chain within 20 s".to_string())?;
    let out_caps = sample
        .caps()
        .ok_or_else(|| "output sample without caps".to_string())?;
    let info =
        gstreamer_video::VideoInfo::from_caps(out_caps).map_err(|e| format!("output caps: {e}"))?;
    let out_buf = sample
        .buffer()
        .ok_or_else(|| "output sample without buffer".to_string())?;
    let frame = gstreamer_video::VideoFrameRef::from_buffer_ref_readable(out_buf, &info)
        .map_err(|_| "could not map output frame".to_string())?;
    let stride = frame.plane_stride()[0] as usize;
    let data = frame
        .plane_data(0)
        .map_err(|_| "could not read output plane".to_string())?;
    let row_bytes = width * 4;
    let mut out = vec![0u8; row_bytes * height];
    for y in 0..height {
        let src = &data[y * stride..y * stride + row_bytes];
        out[y * row_bytes..(y + 1) * row_bytes].copy_from_slice(src);
    }
    Ok(out)
}

/// Compile the clip's whole static colour stack into one `size`³ LUT by
/// measuring the preview chain on every 8-bit RGB input.
pub fn compile_color_lut(spec: &ColorGradeSpec, size: usize) -> Result<CubeLut, String> {
    let size = size.max(2);
    let mut buf = hald_identity();

    // 1. User LUTs — the preview's CPU pad probe (trilinear on RGBA).
    for path in &spec.lut_paths {
        let lut = CubeLut::from_file(Path::new(path))?;
        lut.apply_to_rgba_buffer(&mut buf);
    }

    // 2. videobalance / coloradj_RGB / 3-point — the real GStreamer elements.
    if spec.needs_gst_stage() {
        buf = run_gst_color_chain(buf, HALD_SIDE, HALD_SIDE, spec)?;
    }

    // 3. HSL qualifier — the preview's CPU pad probe.
    if let Some(ref q) = spec.hsl_qualifier {
        apply_hsl_qualifier_to_rgba_buffer(q, &mut buf, HALD_SIDE, HALD_SIDE);
    }

    Ok(sample_dense_to_cube(&buf, size))
}

/// Read the measured function at integer `(r, g, b)`.
#[inline]
fn dense_at(buf: &[u8], r: usize, g: usize, b: usize) -> [f64; 3] {
    let p = (r + (g << 8) + (b << 16)) * 4;
    [buf[p] as f64, buf[p + 1] as f64, buf[p + 2] as f64]
}

/// Trilinear read of the dense 256³ table at a real-valued coordinate.
fn dense_trilinear(buf: &[u8], r: f64, g: f64, b: f64) -> [f64; 3] {
    let (r0, g0, b0) = (
        (r.floor() as usize).min(254),
        (g.floor() as usize).min(254),
        (b.floor() as usize).min(254),
    );
    let (dr, dg, db) = (r - r0 as f64, g - g0 as f64, b - b0 as f64);
    let mut out = [0.0; 3];
    for (corner, w) in [
        ((0, 0, 0), (1.0 - dr) * (1.0 - dg) * (1.0 - db)),
        ((1, 0, 0), dr * (1.0 - dg) * (1.0 - db)),
        ((0, 1, 0), (1.0 - dr) * dg * (1.0 - db)),
        ((1, 1, 0), dr * dg * (1.0 - db)),
        ((0, 0, 1), (1.0 - dr) * (1.0 - dg) * db),
        ((1, 0, 1), dr * (1.0 - dg) * db),
        ((0, 1, 1), (1.0 - dr) * dg * db),
        ((1, 1, 1), dr * dg * db),
    ] {
        if w == 0.0 {
            continue;
        }
        let v = dense_at(buf, r0 + corner.0, g0 + corner.1, b0 + corner.2);
        for ch in 0..3 {
            out[ch] += v[ch] * w;
        }
    }
    out
}

fn sample_dense_to_cube(buf: &[u8], size: usize) -> CubeLut {
    let denom = (size - 1) as f64;
    let mut data = Vec::with_capacity(size * size * size);
    for bi in 0..size {
        let b = bi as f64 / denom * 255.0;
        for gi in 0..size {
            let g = gi as f64 / denom * 255.0;
            for ri in 0..size {
                let r = ri as f64 / denom * 255.0;
                let v = dense_trilinear(buf, r, g, b);
                data.push([
                    (v[0] / 255.0).clamp(0.0, 1.0) as f32,
                    (v[1] / 255.0).clamp(0.0, 1.0) as f32,
                    (v[2] / 255.0).clamp(0.0, 1.0) as f32,
                ]);
            }
        }
    }
    CubeLut {
        size,
        domain_min: [0.0; 3],
        domain_max: [1.0; 3],
        data,
    }
}

// ── Cache ──────────────────────────────────────────────────────────────────

/// SHA-256 over every input of the compiled LUT; doubles as the file name.
pub fn color_lut_key(spec: &ColorGradeSpec, size: usize) -> String {
    use sha2::{Digest, Sha256};
    let elems = probe_chain_elements();
    let mut hasher = Sha256::new();
    hasher.update(
        format!(
            "cg-v1|{size}|ca{}|tp{}|{:.6}|{:.6}|{:.6}|{:.6}|{:.6}|{:.6}|{:.6}|{:.6}|{:.6}|{:.6}|{:.6}|{:.6}|{:.6}|{:.6}|{:.6}|{:.6}",
            elems.coloradj as u8,
            elems.three_point as u8,
            spec.brightness,
            spec.contrast,
            spec.saturation,
            spec.exposure,
            spec.temperature,
            spec.tint,
            spec.shadows,
            spec.midtones,
            spec.highlights,
            spec.black_point,
            spec.highlights_warmth,
            spec.highlights_tint,
            spec.midtones_warmth,
            spec.midtones_tint,
            spec.shadows_warmth,
            spec.shadows_tint,
        )
        .as_bytes(),
    );
    for path in &spec.lut_paths {
        let (len, mtime) = std::fs::metadata(path)
            .map(|m| {
                (
                    m.len(),
                    m.modified()
                        .ok()
                        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                        .map(|d| d.as_secs())
                        .unwrap_or(0),
                )
            })
            .unwrap_or((0, 0));
        hasher.update(format!("|lut:{path}:{len}:{mtime}").as_bytes());
    }
    if let Some(ref q) = spec.hsl_qualifier {
        hasher.update(
            format!(
                "|hsl:{}",
                crate::media::hsl_qualifier::hsl_qualifier_lut_key(q, size)
            )
            .as_bytes(),
        );
    }
    format!("{:x}", hasher.finalize())
}

/// `$XDG_CACHE_HOME/ultimateslice/color_luts/`.
pub fn color_lut_cache_dir() -> PathBuf {
    crate::media::cache_support::cache_root_dir("color_luts")
}

/// Return the compiled `.cube` for `spec` in `dir`, compiling on first use.
/// Temp-and-rename write; an existing file is reused (its mtime refreshed for
/// the size-capped pruning).
pub fn ensure_color_lut_file_in(
    dir: &Path,
    spec: &ColorGradeSpec,
    size: usize,
) -> Result<PathBuf, String> {
    let key = color_lut_key(spec, size);
    let path = dir.join(format!("cg-{key}.cube"));
    if crate::media::cache_support::file_has_content(&path.to_string_lossy()) {
        if let Ok(file) = std::fs::File::options().append(true).open(&path) {
            let _ = file.set_modified(std::time::SystemTime::now());
        }
        return Ok(path);
    }
    std::fs::create_dir_all(dir).map_err(|e| format!("failed to create {}: {e}", dir.display()))?;
    let started = std::time::Instant::now();
    let lut = compile_color_lut(spec, size)?;
    let tmp = dir.join(format!("cg-{key}.{}.tmp", std::process::id()));
    lut.write_cube_file(&tmp)?;
    std::fs::rename(&tmp, &path).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        format!("failed to move {} into place: {e}", path.display())
    })?;
    log::info!(
        "color_lut: compiled {} ({}³, gst_stage={}, luts={}, hsl={}) in {} ms",
        path.display(),
        size,
        spec.needs_gst_stage(),
        spec.lut_paths.len(),
        spec.hsl_qualifier.is_some(),
        started.elapsed().as_millis()
    );
    crate::media::cube_lut::prune_cube_files(dir, "cg-", COLOR_LUT_CACHE_MAX_FILES);
    Ok(path)
}

/// FFmpeg filter stage applying the compiled LUT: `None` when `spec` is the
/// identity (nothing to apply), `Err` when it could not be compiled or
/// written (callers fall back to the per-stage filter chain).
///
/// `format=gbrap` keeps the alpha plane (transparent padding, crops, masks)
/// through the RGB stage; the trailing `format=yuva420p` returns the chain
/// to the YUV layout the surrounding filters are written for.
pub fn color_lut_filter_stage_in(
    spec: &ColorGradeSpec,
    dir: &Path,
    fallback_dir: Option<&Path>,
) -> Result<Option<String>, String> {
    if spec.is_identity() {
        return Ok(None);
    }
    let path = match ensure_color_lut_file_in(dir, spec, COLOR_LUT_SIZE) {
        Ok(path) => path,
        Err(err) => {
            let Some(fallback) = fallback_dir else {
                return Err(err);
            };
            log::warn!(
                "color_lut: could not write LUT to {}: {err}; trying {}",
                dir.display(),
                fallback.display()
            );
            ensure_color_lut_file_in(fallback, spec, COLOR_LUT_SIZE)?
        }
    };
    Ok(Some(format!(
        ",format=gbrap,lut3d=file={}:interp=trilinear,format=yuva420p",
        crate::media::export::escape_filter_path(&path.to_string_lossy())
    )))
}

/// `color_lut_filter_stage_in` against the standard cache directory with the
/// system temp directory as fallback.
pub fn color_lut_filter_stage(spec: &ColorGradeSpec) -> Result<Option<String>, String> {
    color_lut_filter_stage_in(
        spec,
        &color_lut_cache_dir(),
        Some(&std::env::temp_dir().join("ultimateslice-color-luts")),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn gst_ready() -> bool {
        gst::init().is_ok() && gst::ElementFactory::find("videobalance").is_some()
    }

    fn node(lut: &CubeLut, ri: usize, gi: usize, bi: usize) -> [f32; 3] {
        lut.data[ri + gi * lut.size + bi * lut.size * lut.size]
    }

    #[test]
    fn neutral_spec_is_identity_and_static_detection_follows_keyframes() {
        let spec = ColorGradeSpec::neutral();
        assert!(spec.is_identity());
        assert!(!spec.needs_gst_stage());
        let mut clip = Clip::new(
            "/tmp/x.mp4",
            1_000_000_000,
            0,
            crate::model::clip::ClipKind::Video,
        );
        assert!(clip_color_is_static(&clip));
        assert!(ColorGradeSpec::from_clip(&clip).is_identity());
        clip.temperature_keyframes
            .push(crate::model::clip::NumericKeyframe {
                time_ns: 0,
                value: 5000.0,
                interpolation: crate::model::clip::KeyframeInterpolation::Linear,
                bezier_controls: None,
            });
        assert!(!clip_color_is_static(&clip));
    }

    #[test]
    fn hald_identity_encodes_every_triple_once() {
        let buf = hald_identity();
        assert_eq!(buf.len(), HALD_SIDE * HALD_SIDE * 4);
        assert_eq!(dense_at(&buf, 0, 0, 0), [0.0, 0.0, 0.0]);
        assert_eq!(dense_at(&buf, 255, 0, 0), [255.0, 0.0, 0.0]);
        assert_eq!(dense_at(&buf, 7, 200, 13), [7.0, 200.0, 13.0]);
        assert_eq!(dense_at(&buf, 255, 255, 255), [255.0, 255.0, 255.0]);
    }

    #[test]
    fn sampling_identity_table_gives_identity_cube() {
        let buf = hald_identity();
        let lut = sample_dense_to_cube(&buf, 9);
        for bi in 0..9 {
            for gi in 0..9 {
                for ri in 0..9 {
                    let v = node(&lut, ri, gi, bi);
                    let expect = [ri as f32 / 8.0, gi as f32 / 8.0, bi as f32 / 8.0];
                    for ch in 0..3 {
                        assert!((v[ch] - expect[ch]).abs() < 1e-6, "{v:?} vs {expect:?}");
                    }
                }
            }
        }
    }

    #[test]
    fn user_lut_only_compiles_to_the_same_values_as_the_preview_probe() {
        // A 2-point LUT that swaps red and blue and dims green.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("swap.cube");
        std::fs::write(
            &path,
            "LUT_3D_SIZE 2\n0 0 0\n0 0 1\n0 0.5 0\n0 0.5 1\n1 0 0\n1 0 1\n1 0.5 0\n1 0.5 1\n",
        )
        .unwrap();
        let spec = ColorGradeSpec {
            lut_paths: vec![path.to_string_lossy().to_string()],
            ..ColorGradeSpec::neutral()
        };
        assert!(!spec.needs_gst_stage());
        let lut = compile_color_lut(&spec, 5).unwrap();
        let probe = CubeLut::from_file(&path).unwrap();
        for (ri, gi, bi) in [(4, 0, 0), (0, 4, 0), (0, 0, 4), (2, 2, 2), (1, 3, 4)] {
            let v = node(&lut, ri, gi, bi);
            let (r, g, b) = probe.apply_rgb(
                (ri * 255 / 4) as u8,
                (gi * 255 / 4) as u8,
                (bi * 255 / 4) as u8,
            );
            let expect = [r as f32 / 255.0, g as f32 / 255.0, b as f32 / 255.0];
            for ch in 0..3 {
                assert!(
                    (v[ch] - expect[ch]).abs() <= 1.0 / 255.0 + 1e-6,
                    "{v:?} vs {expect:?}"
                );
            }
        }
    }

    #[test]
    fn gst_stage_with_neutral_sliders_but_forced_balance_stays_near_identity() {
        if !gst_ready() {
            eprintln!("skipping: GStreamer videobalance unavailable");
            return;
        }
        // Tiny brightness so videobalance is instantiated; its 8-bit YCbCr
        // round trip may move values by a step or two but no more.
        let spec = ColorGradeSpec {
            brightness: 0.0005,
            ..ColorGradeSpec::neutral()
        };
        assert!(spec.needs_videobalance());
        let lut = compile_color_lut(&spec, 17).expect("compile");
        let mut max_err = 0.0f32;
        for bi in 0..17 {
            for gi in 0..17 {
                for ri in 0..17 {
                    let v = node(&lut, ri, gi, bi);
                    let expect = [ri as f32 / 16.0, gi as f32 / 16.0, bi as f32 / 16.0];
                    for ch in 0..3 {
                        max_err = max_err.max((v[ch] - expect[ch]).abs() * 255.0);
                    }
                }
            }
        }
        eprintln!("near-identity videobalance max error: {max_err:.2} / 255");
        // videobalance converts RGB → 8-bit limited-range YCbCr and back, so
        // even a near-neutral setting re-quantises every value; the preview
        // shows exactly that, and the LUT must reproduce it rather than hide it.
        assert!(max_err <= 6.0, "max error {max_err}");
    }

    #[test]
    fn gst_stage_brightness_lifts_midgrey() {
        if !gst_ready() {
            eprintln!("skipping: GStreamer videobalance unavailable");
            return;
        }
        let spec = ColorGradeSpec {
            brightness: 0.3,
            ..ColorGradeSpec::neutral()
        };
        let lut = compile_color_lut(&spec, 9).expect("compile");
        let mid = node(&lut, 4, 4, 4);
        assert!(
            mid[0] > 0.6 && mid[1] > 0.6 && mid[2] > 0.6,
            "mid grey {mid:?}"
        );
        let black = node(&lut, 0, 0, 0);
        assert!(black[0] >= 0.0 && black[0] <= 1.0);
        let white = node(&lut, 8, 8, 8);
        assert!(white[0] > 0.95, "white {white:?}");
    }

    #[test]
    fn key_is_stable_and_sensitive() {
        let a = ColorGradeSpec {
            contrast: 1.2,
            ..ColorGradeSpec::neutral()
        };
        let k1 = color_lut_key(&a, COLOR_LUT_SIZE);
        assert_eq!(k1, color_lut_key(&a.clone(), COLOR_LUT_SIZE));
        assert_eq!(k1.len(), 64);
        let b = ColorGradeSpec {
            contrast: 1.21,
            ..a.clone()
        };
        assert_ne!(k1, color_lut_key(&b, COLOR_LUT_SIZE));
        let c = ColorGradeSpec {
            hsl_qualifier: Some(HslQualifier {
                enabled: true,
                hue_min: 10.0,
                hue_max: 40.0,
                saturation: 0.5,
                ..HslQualifier::default()
            }),
            ..a.clone()
        };
        assert_ne!(k1, color_lut_key(&c, COLOR_LUT_SIZE));
        assert_ne!(k1, color_lut_key(&a, 33));
    }

    #[test]
    fn filter_stage_is_none_for_identity_and_lut3d_otherwise() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(
            color_lut_filter_stage_in(&ColorGradeSpec::neutral(), dir.path(), None).unwrap(),
            None
        );
        let spec = ColorGradeSpec {
            hsl_qualifier: Some(HslQualifier {
                enabled: true,
                hue_min: 200.0,
                hue_max: 260.0,
                saturation: 0.3,
                ..HslQualifier::default()
            }),
            ..ColorGradeSpec::neutral()
        };
        let stage = color_lut_filter_stage_in(&spec, dir.path(), None)
            .unwrap()
            .expect("stage");
        assert!(stage.starts_with(",format=gbrap,lut3d=file="), "{stage}");
        assert!(
            stage.ends_with(":interp=trilinear,format=yuva420p"),
            "{stage}"
        );
        let files: Vec<_> = std::fs::read_dir(dir.path()).unwrap().flatten().collect();
        assert_eq!(files.len(), 1);
        // Second call reuses the file.
        assert_eq!(
            stage,
            color_lut_filter_stage_in(&spec, dir.path(), None)
                .unwrap()
                .unwrap()
        );
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().flatten().count(), 1);
    }

    #[test]
    fn filter_stage_uses_fallback_dir_then_errors() {
        let not_a_dir = tempfile::NamedTempFile::new().unwrap();
        let fallback = tempfile::tempdir().unwrap();
        let spec = ColorGradeSpec {
            hsl_qualifier: Some(HslQualifier {
                enabled: true,
                hue_min: 200.0,
                hue_max: 260.0,
                saturation: 0.3,
                ..HslQualifier::default()
            }),
            ..ColorGradeSpec::neutral()
        };
        let stage = color_lut_filter_stage_in(&spec, not_a_dir.path(), Some(fallback.path()))
            .unwrap()
            .expect("stage");
        assert!(stage.contains(&crate::media::export::escape_filter_path(
            &fallback.path().to_string_lossy()
        )));
        assert!(
            color_lut_filter_stage_in(&spec, not_a_dir.path(), Some(not_a_dir.path())).is_err()
        );
        assert!(color_lut_filter_stage_in(&spec, not_a_dir.path(), None).is_err());
    }
}
