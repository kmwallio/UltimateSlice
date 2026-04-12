use crate::media::adjustment_scope::AdjustmentScopeShape;
use crate::media::cube_lut::CubeLut;
use crate::media::player::PlayerState;
use crate::model::clip::{Clip as ModelClip, ClipMask, MaskShape, NumericKeyframe};
use crate::model::transition::{
    canonicalize_transition_kind, transition_kind_from_xfade_name, transition_xfade_name_for_kind,
    TransitionAlignment, TransitionOverlapWindow,
};
use crate::ui_state::{
    clamp_prerender_crf, CrossfadeCurve, PlaybackPriority, PrerenderEncodingPreset,
    DEFAULT_PRERENDER_CRF,
};
/// A "program monitor" player that composites the assembled timeline.
///
/// Uses a GStreamer pipeline built around `compositor` (video) and `audiomixer`
/// (audio) so that multiple video tracks are layered correctly in real time.
/// Each active clip gets its own `uridecodebin → effects → compositor` branch.
/// Timeline position derives from a single pipeline running time — no per-clip
/// anchor heuristics needed.
use anyhow::{anyhow, Result};
use glib;
use gstreamer as gst;
use gstreamer::prelude::*;
use gstreamer_app::AppSink;
use serde::{Deserialize, Serialize};

/// Access a child of a GStreamer ChildProxy element by index.
/// Uses FFI because `gst::Element` doesn't statically implement `IsA<ChildProxy>`,
/// so `dynamic_cast_ref::<gst::ChildProxy>()` often fails even when the element
/// does implement the ChildProxy interface at the GObject level.
fn child_proxy_get_child(element: &gst::Element, index: u32) -> Option<glib::Object> {
    unsafe {
        let ptr = gstreamer::ffi::gst_child_proxy_get_child_by_index(
            element.as_ptr() as *mut gstreamer::ffi::GstChildProxy,
            index,
        );
        if ptr.is_null() {
            None
        } else {
            Some(glib::translate::from_glib_full(ptr))
        }
    }
}

/// Set freq/gain/bandwidth on a single EQ band child element.
fn eq_set_band(element: &gst::Element, band_idx: u32, freq: f64, gain: f64, q: f64) {
    if let Some(band) = child_proxy_get_child(element, band_idx) {
        let gain_clamped = gain.clamp(-24.0, 12.0);
        band.set_property("freq", freq);
        band.set_property("gain", gain_clamped);
        band.set_property("bandwidth", freq / q.max(0.1));
        log::debug!(
            "eq_set_band: band={} freq={:.0} gain={:.1} bw={:.0}",
            band_idx,
            freq,
            gain_clamped,
            freq / q.max(0.1)
        );
    } else {
        log::warn!(
            "eq_set_band: child_proxy returned None for band={}",
            band_idx
        );
    }
}

/// Set only the gain on a single EQ band child element (for keyframe updates).
fn eq_set_band_gain(element: &gst::Element, band_idx: u32, gain: f64) {
    if let Some(band) = child_proxy_get_child(element, band_idx) {
        band.set_property("gain", gain.clamp(-24.0, 12.0));
    }
}
use std::collections::hash_map::DefaultHasher;
use std::collections::{HashMap, HashSet};
use std::f64::consts::FRAC_1_SQRT_2;
use std::hash::{Hash, Hasher};
use std::io::Write;
use std::path::Path;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const MAX_PREVIEW_AUDIO_GAIN: f64 = 3.981_071_705_5; // +12 dB
const BACKGROUND_PRERENDER_CACHE_VERSION: u32 = 5;
const PROGRAM_PREVIEW_AUDIO_RATE: i32 = 48_000;
const PROGRAM_PREVIEW_AUDIO_CHANNELS: i32 = 2;
const MAX_READY_PRERENDER_SEGMENTS: usize = 24;

fn program_preview_audio_caps() -> gst::Caps {
    gst::Caps::builder("audio/x-raw")
        .field("rate", PROGRAM_PREVIEW_AUDIO_RATE)
        .field("channels", PROGRAM_PREVIEW_AUDIO_CHANNELS)
        .build()
}

fn default_prerender_cache_root() -> PathBuf {
    std::env::temp_dir()
        .join("ultimateslice")
        .join(format!("prerender-v{}", BACKGROUND_PRERENDER_CACHE_VERSION))
}

fn prerender_cache_root_for_project_path(
    project_file_path: Option<&str>,
    persist_next_to_project_file: bool,
) -> (PathBuf, bool) {
    if !persist_next_to_project_file {
        return (default_prerender_cache_root(), false);
    }
    match project_file_path {
        Some(project_file_path) if !project_file_path.is_empty() => {
            let project_path = Path::new(project_file_path);
            let parent = project_path.parent().unwrap_or_else(|| Path::new("."));
            let stem = project_path
                .file_stem()
                .and_then(|s| s.to_str())
                .filter(|s| !s.is_empty())
                .unwrap_or("project");
            let stem = sanitize_prerender_cache_component(stem);
            let mut hasher = DefaultHasher::new();
            project_file_path.hash(&mut hasher);
            let path_hash = hasher.finish();
            (
                parent
                    .join("UltimateSlice.cache")
                    .join(format!("prerender-v{}", BACKGROUND_PRERENDER_CACHE_VERSION))
                    .join(format!("{stem}-p{path_hash:016x}")),
                true,
            )
        }
        _ => (default_prerender_cache_root(), false),
    }
}

/// Remove prerender cache directories from old versions that are no longer
/// used.  Both the temp root (`$TMPDIR/ultimateslice/`) and persistent roots
/// (`UltimateSlice.cache/`) can accumulate `prerender-vN/` directories when
/// the cache version is bumped.
fn cleanup_old_prerender_cache_versions(current_root: &Path) {
    let current_version_dir = format!("prerender-v{BACKGROUND_PRERENDER_CACHE_VERSION}");
    // Walk up from current_root to find the parent that contains
    // `prerender-vN/` directories (skip current_root itself and any
    // intermediate path components that are part of the version dir name).
    let mut parent = None;
    for ancestor in current_root.ancestors().skip(1) {
        let is_version_dir = ancestor
            .file_name()
            .and_then(|n| n.to_str())
            .map(|n| n.starts_with("prerender-v"))
            .unwrap_or(false);
        if !is_version_dir {
            parent = Some(ancestor);
            break;
        }
    }
    let Some(parent) = parent else {
        return;
    };
    let Ok(entries) = std::fs::read_dir(parent) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(name_str) = name.to_str() else {
            continue;
        };
        if name_str.starts_with("prerender-v")
            && name_str != current_version_dir
            && entry.path().is_dir()
        {
            log::info!(
                "cleanup_old_prerender_cache_versions: removing {}",
                entry.path().display()
            );
            let _ = std::fs::remove_dir_all(entry.path());
        }
    }
}

fn sanitize_prerender_cache_component(component: &str) -> String {
    let sanitized: String = component
        .chars()
        .map(|ch| match ch {
            'a'..='z' | 'A'..='Z' | '0'..='9' | '-' | '_' => ch,
            _ => '_',
        })
        .collect();
    if sanitized.is_empty() {
        "project".to_string()
    } else {
        sanitized
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct PrerenderSourceSignature {
    len: u64,
    modified_secs: u64,
    modified_nanos: u32,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct PrerenderManifestInput {
    path: String,
    signature: PrerenderSourceSignature,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct PrerenderSegmentManifest {
    key: String,
    signature: u64,
    start_ns: u64,
    end_ns: u64,
    inputs: Vec<PrerenderManifestInput>,
}

fn prerender_source_signature_for_path(path: &Path) -> Option<PrerenderSourceSignature> {
    let metadata = std::fs::metadata(path).ok()?;
    let modified = metadata.modified().ok()?;
    let since_epoch = modified.duration_since(UNIX_EPOCH).ok()?;
    Some(PrerenderSourceSignature {
        len: metadata.len(),
        modified_secs: since_epoch.as_secs(),
        modified_nanos: since_epoch.subsec_nanos(),
    })
}

fn prerender_manifest_path(output_path: &Path) -> PathBuf {
    PathBuf::from(format!("{}.manifest.json", output_path.to_string_lossy()))
}

fn prerender_partial_output_path(output_path: &Path) -> PathBuf {
    PathBuf::from(format!("{}.partial", output_path.to_string_lossy()))
}

fn is_managed_prerender_segment_path(path: &Path) -> bool {
    path.extension().and_then(|s| s.to_str()) == Some("mp4")
        && path
            .file_stem()
            .and_then(|s| s.to_str())
            .map(|stem| stem.starts_with("seg_v"))
            .unwrap_or(false)
}

fn remove_prerender_segment_files(output_path: &Path) {
    let _ = std::fs::remove_file(output_path);
    let _ = std::fs::remove_file(prerender_manifest_path(output_path));
    let _ = std::fs::remove_file(prerender_partial_output_path(output_path));
}

/// Normalize mixer-bound clip audio so camera AAC (e.g. 16 kHz mono Ring MP4s)
/// negotiates cleanly with the fixed preview mix format.
/// Insert a mono capsfilter between `audioconvert` and the rest of the audio
/// chain to extract a single channel (Left/Right) or downmix (MonoMix).
/// The downstream stereo capsfilter then upmixes mono to both speakers.
/// Returns the element on success (caller must sync state), or None for Stereo.
/// Set `mix-matrix` on an `audioconvert` element for channel extraction.
/// Left: both outputs get input left. Right: both get input right.
/// MonoMix: both get 0.5*L + 0.5*R.  Stereo: no-op.
fn apply_channel_mode_to_audioconvert(
    audio_conv: &gst::Element,
    mode: crate::model::clip::AudioChannelMode,
) {
    use crate::model::clip::AudioChannelMode;
    let matrix: Vec<Vec<f32>> = match mode {
        AudioChannelMode::Stereo => return,
        AudioChannelMode::Left => vec![vec![1.0, 0.0], vec![1.0, 0.0]],
        AudioChannelMode::Right => vec![vec![0.0, 1.0], vec![0.0, 1.0]],
        AudioChannelMode::MonoMix => vec![vec![0.5, 0.5], vec![0.5, 0.5]],
    };
    if audio_conv.find_property("mix-matrix").is_some() {
        let gst_matrix: Vec<glib::SendValue> = matrix
            .iter()
            .map(|row| {
                let inner = gst::Array::new(row.iter().copied());
                inner.to_send_value()
            })
            .collect();
        let outer = gst::Array::from_values(gst_matrix);
        audio_conv.set_property("mix-matrix", &outer);
    } else {
        log::warn!(
            "apply_channel_mode_to_audioconvert: audioconvert lacks mix-matrix property, channel mode {:?} ignored",
            mode
        );
    }
}

fn attach_preview_audio_normalizer(
    pipeline: &gst::Pipeline,
    audio_conv: &gst::Element,
    log_context: &str,
) -> Option<(gst::Element, gst::Element, gst::Pad)> {
    attach_preview_audio_normalizer_with_channel_mode(
        pipeline,
        audio_conv,
        crate::model::clip::AudioChannelMode::Stereo,
        log_context,
    )
}

fn attach_preview_audio_normalizer_with_channel_mode(
    pipeline: &gst::Pipeline,
    audio_conv: &gst::Element,
    channel_mode: crate::model::clip::AudioChannelMode,
    log_context: &str,
) -> Option<(gst::Element, gst::Element, gst::Pad)> {
    let resample = match gst::ElementFactory::make("audioresample").build() {
        Ok(elem) => elem,
        Err(err) => {
            log::warn!("{log_context}: failed to create audioresample: {err}");
            return None;
        }
    };
    let mix_caps = program_preview_audio_caps();
    let capsfilter = match gst::ElementFactory::make("capsfilter")
        .property("caps", &mix_caps)
        .build()
    {
        Ok(elem) => elem,
        Err(err) => {
            log::warn!("{log_context}: failed to create preview audio capsfilter: {err}");
            return None;
        }
    };

    if pipeline.add_many([&resample, &capsfilter]).is_err() {
        log::warn!("{log_context}: failed to add preview audio normalizer to pipeline");
        return None;
    }

    let cleanup = |pipeline: &gst::Pipeline, resample: &gst::Element, capsfilter: &gst::Element| {
        pipeline.remove(capsfilter).ok();
        pipeline.remove(resample).ok();
    };

    // Apply channel extraction (Left/Right/MonoMix) via the audioconvert
    // mix-matrix.  The matrix maps input channels to output stereo so that
    // both speakers carry the selected channel(s).
    apply_channel_mode_to_audioconvert(audio_conv, channel_mode);

    let Some(conv_src) = audio_conv.static_pad("src") else {
        log::warn!("{log_context}: audioconvert src pad missing");
        cleanup(pipeline, &resample, &capsfilter);
        return None;
    };

    let Some(resample_sink) = resample.static_pad("sink") else {
        log::warn!("{log_context}: audioresample sink pad missing");
        cleanup(pipeline, &resample, &capsfilter);
        return None;
    };
    if conv_src.link(&resample_sink).is_err() {
        log::warn!("{log_context}: failed to link audioconvert to audioresample");
        cleanup(pipeline, &resample, &capsfilter);
        return None;
    }
    if gst::Element::link_many([&resample, &capsfilter]).is_err() {
        log::warn!("{log_context}: failed to link audioresample to capsfilter");
        cleanup(pipeline, &resample, &capsfilter);
        return None;
    }
    let Some(caps_src) = capsfilter.static_pad("src") else {
        log::warn!("{log_context}: preview audio capsfilter src pad missing");
        cleanup(pipeline, &resample, &capsfilter);
        return None;
    };

    Some((resample, capsfilter, caps_src))
}

fn frame_hash_u64(data: &[u8]) -> u64 {
    let mut h = 1469598103934665603u64;
    for &b in data {
        h ^= b as u64;
        h = h.wrapping_mul(1099511628211);
    }
    h
}

/// A single RGBA frame pulled from the scope appsink for colour scope analysis.
#[derive(Clone)]
pub struct ScopeFrame {
    pub data: Vec<u8>,
    pub width: usize,
    pub height: usize,
}

#[derive(Clone)]
struct CachedPlayheadFrame {
    frame_pos_ns: u64,
    signature: u64,
    scope_seq: u64,
    frame: ScopeFrame,
}

#[derive(Clone, Copy)]
struct PendingShortFrameCapture {
    frame_pos_ns: u64,
    signature: u64,
    min_scope_seq: u64,
}

#[derive(Default)]
struct ShortFrameCache {
    previous: Option<CachedPlayheadFrame>,
    current: Option<CachedPlayheadFrame>,
    next: Option<CachedPlayheadFrame>,
    hits: u64,
    misses: u64,
    invalidations: u64,
}

impl ShortFrameCache {
    fn lookup(&mut self, frame_pos_ns: u64, signature: u64) -> bool {
        let hit = [
            self.previous.as_ref(),
            self.current.as_ref(),
            self.next.as_ref(),
        ]
        .into_iter()
        .flatten()
        .find(|entry| entry.frame_pos_ns == frame_pos_ns && entry.signature == signature);
        let hit_found = hit.is_some();
        let _hit_scope_seq = hit.map(|entry| entry.scope_seq);
        if hit_found {
            self.hits = self.hits.saturating_add(1);
        } else {
            self.misses = self.misses.saturating_add(1);
        }
        hit_found
    }

    fn clear(&mut self) -> bool {
        let had_entries = self.previous.is_some() || self.current.is_some() || self.next.is_some();
        self.previous = None;
        self.current = None;
        self.next = None;
        if had_entries {
            self.invalidations = self.invalidations.saturating_add(1);
        }
        had_entries
    }

    fn store_current(&mut self, entry: CachedPlayheadFrame) {
        let current_key = (entry.frame_pos_ns, entry.signature);
        let mut entries: Vec<CachedPlayheadFrame> = [
            self.previous.take(),
            self.current.take(),
            self.next.take(),
            Some(entry.clone()),
        ]
        .into_iter()
        .flatten()
        .filter(|existing| (existing.frame_pos_ns, existing.signature) != current_key)
        .collect();
        entries.push(entry.clone());
        entries.sort_by_key(|existing| existing.frame_pos_ns);
        self.current = Some(entry.clone());
        self.previous = entries
            .iter()
            .filter(|existing| existing.frame_pos_ns < entry.frame_pos_ns)
            .max_by_key(|existing| existing.frame_pos_ns)
            .cloned();
        self.next = entries
            .iter()
            .filter(|existing| existing.frame_pos_ns > entry.frame_pos_ns)
            .min_by_key(|existing| existing.frame_pos_ns)
            .cloned();
    }
}

/// Calibrated videobalance output parameters (brightness, contrast,
/// saturation, hue) computed from clip colour settings by
/// `ProgramPlayer::compute_videobalance_params`.
#[derive(Debug, Clone, Copy)]
pub(crate) struct VBParams {
    pub(crate) brightness: f64,
    pub(crate) contrast: f64,
    pub(crate) saturation: f64,
    pub(crate) hue: f64,
}

/// Per-channel RGB gains for frei0r `coloradj_RGB` element.
/// Values are in frei0r's [0,1] range where 0.5 = neutral (gain 1.0).
#[derive(Debug, Clone, Copy)]
pub(crate) struct ColorAdjRGBParams {
    pub(crate) r: f64,
    pub(crate) g: f64,
    pub(crate) b: f64,
}

/// Parameters for frei0r `3-point-color-balance` element.
/// Maps shadows/midtones/highlights to the black/gray/white reference
/// points of a piecewise-linear transfer curve.
#[derive(Clone)]
pub(crate) struct ThreePointParams {
    pub(crate) black_r: f64,
    pub(crate) black_g: f64,
    pub(crate) black_b: f64,
    pub(crate) gray_r: f64,
    pub(crate) gray_g: f64,
    pub(crate) gray_b: f64,
    pub(crate) white_r: f64,
    pub(crate) white_g: f64,
    pub(crate) white_b: f64,
}

/// Quadratic (parabola) coefficients for one channel of the frei0r
/// 3-point-color-balance transfer curve.  The plugin fits y = a·x² + b·x + c
/// through (black_c, 0), (gray_c, 0.5), (white_c, 1.0) where x is the
/// normalised input pixel value and y is the output (both 0–1).
#[derive(Debug, Clone, Copy)]
pub(crate) struct ThreePointParabolaCoeffs {
    pub(crate) a: f64,
    pub(crate) b: f64,
    pub(crate) c: f64,
}

impl ThreePointParabolaCoeffs {
    /// Solve the quadratic passing through (x1, 0), (x2, 0.5), (x3, 1.0).
    pub(crate) fn from_control_points(x1: f64, x2: f64, x3: f64) -> Self {
        // System: a·x1² + b·x1 + c = 0
        //         a·x2² + b·x2 + c = 0.5
        //         a·x3² + b·x3 + c = 1.0
        let d1 = x2 - x1;
        let d2 = x3 - x1;
        let d3 = x3 - x2;
        // Guard against degenerate/nearly-coincident control points.
        if d1.abs() < 1e-9 || d2.abs() < 1e-9 || d3.abs() < 1e-9 {
            // Fall back to identity.
            return Self {
                a: 0.0,
                b: 1.0,
                c: 0.0,
            };
        }
        let a = (1.0 / d2 - 0.5 / d1) / d3;
        let b = 0.5 / d1 - a * (x2 + x1);
        let c = -(a * x1 * x1 + b * x1);
        Self { a, b, c }
    }
}

/// Per-channel parabola coefficients matching the frei0r 3-point plugin.
#[derive(Debug, Clone)]
pub(crate) struct ThreePointParabola {
    pub(crate) r: ThreePointParabolaCoeffs,
    pub(crate) g: ThreePointParabolaCoeffs,
    pub(crate) b: ThreePointParabolaCoeffs,
}

impl ThreePointParabola {
    pub(crate) fn from_params(p: &ThreePointParams) -> Self {
        Self {
            r: ThreePointParabolaCoeffs::from_control_points(p.black_r, p.gray_r, p.white_r),
            g: ThreePointParabolaCoeffs::from_control_points(p.black_g, p.gray_g, p.white_g),
            b: ThreePointParabolaCoeffs::from_control_points(p.black_b, p.gray_b, p.white_b),
        }
    }

    /// Format as an FFmpeg `lutrgb` filter expression.  `val` is 0–255 integer.
    /// output = clamp(a·(val/255)² + b·(val/255) + c, 0, 1) · 255
    ///        = clamp(A·val² + B·val + C, 0, 255)
    /// where A = a/255, B = b, C = c·255.
    pub(crate) fn to_lutrgb_filter(&self) -> String {
        fn chan_expr(tag: &str, c: &ThreePointParabolaCoeffs) -> String {
            let a = c.a / 255.0;
            let b = c.b;
            let cv = c.c * 255.0;
            format!("{tag}='clip({a:.6}*val*val+{b:.6}*val+{cv:.4},0,255)'")
        }
        format!(
            ",lutrgb={}:{}:{}",
            chan_expr("r", &self.r),
            chan_expr("g", &self.g),
            chan_expr("b", &self.b),
        )
    }
}

/// Role of a clip within a transition overlap region.
#[derive(Debug, Clone, Copy, PartialEq)]
enum TransitionRole {
    /// The clip is fading out (clip A in A→B transition).
    Outgoing,
    /// The clip is fading in (clip B in A→B transition).
    Incoming,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TransitionDirection {
    Left,
    Right,
    Up,
    Down,
}

impl TransitionDirection {
    fn unit_vector(self) -> (f64, f64) {
        match self {
            Self::Left => (-1.0, 0.0),
            Self::Right => (1.0, 0.0),
            Self::Up => (0.0, -1.0),
            Self::Down => (0.0, 1.0),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TransitionMotionKind {
    Cover,
    Reveal,
    Slide,
}

/// Per-slot transition state computed from timeline position.
#[derive(Debug, Clone)]
struct TransitionState {
    /// Transition kind (e.g. "cross_dissolve", "fade_to_black", "wipe_right", "wipe_left").
    kind: String,
    /// Progress through the transition: 0.0 = start, 1.0 = end.
    progress: f64,
    /// Whether this slot's clip is the outgoing or incoming clip.
    role: TransitionRole,
}

const TRANSITION_MASK_EDGE_FEATHER: f64 = 0.01;
// Half-width/height large enough for a centered circle to reach all four corners.
const TRANSITION_FULL_FRAME_CIRCLE_RADIUS: f64 = FRAC_1_SQRT_2 + 0.01;

fn transition_preview_background_pattern(
    active_states: &[Option<TransitionState>],
) -> &'static str {
    if active_states
        .iter()
        .flatten()
        .any(|ts| ts.kind == "fade_to_white")
    {
        "white"
    } else {
        "black"
    }
}

fn transition_preview_mask(kind: &str, role: TransitionRole, progress: f64) -> Option<ClipMask> {
    let progress = progress.clamp(0.0, 1.0);
    match (kind, role) {
        ("circle_open", TransitionRole::Incoming) => Some(transition_preview_circle_mask(
            TRANSITION_FULL_FRAME_CIRCLE_RADIUS * progress,
            false,
        )),
        ("circle_close", TransitionRole::Incoming) => Some(transition_preview_circle_mask(
            TRANSITION_FULL_FRAME_CIRCLE_RADIUS * (1.0 - progress),
            true,
        )),
        _ => None,
    }
}

fn transition_preview_circle_mask(radius: f64, invert: bool) -> ClipMask {
    let mut mask = ClipMask::new(MaskShape::Ellipse);
    let radius = radius.max(0.0);
    mask.width = radius;
    mask.height = radius;
    mask.feather = TRANSITION_MASK_EDGE_FEATHER;
    mask.invert = invert;
    mask
}

fn transition_direction(kind: &str) -> Option<TransitionDirection> {
    match kind {
        "wipe_left" | "cover_left" | "reveal_left" | "slide_left" => {
            Some(TransitionDirection::Left)
        }
        "wipe_right" | "cover_right" | "reveal_right" | "slide_right" => {
            Some(TransitionDirection::Right)
        }
        "wipeup" | "cover_up" | "reveal_up" | "slide_up" => Some(TransitionDirection::Up),
        "wipedown" | "cover_down" | "reveal_down" | "slide_down" => Some(TransitionDirection::Down),
        _ => None,
    }
}

fn transition_motion_kind(kind: &str) -> Option<TransitionMotionKind> {
    match kind {
        "cover_left" | "cover_right" | "cover_up" | "cover_down" => {
            Some(TransitionMotionKind::Cover)
        }
        "reveal_left" | "reveal_right" | "reveal_up" | "reveal_down" => {
            Some(TransitionMotionKind::Reveal)
        }
        "slide_left" | "slide_right" | "slide_up" | "slide_down" => {
            Some(TransitionMotionKind::Slide)
        }
        _ => None,
    }
}

fn transition_preview_canvas_offset(
    kind: &str,
    role: TransitionRole,
    progress: f64,
) -> Option<(f64, f64)> {
    let progress = progress.clamp(0.0, 1.0);
    let motion_kind = transition_motion_kind(kind)?;
    let (dir_x, dir_y) = transition_direction(kind)?.unit_vector();
    match (motion_kind, role) {
        (TransitionMotionKind::Cover, TransitionRole::Incoming)
        | (TransitionMotionKind::Slide, TransitionRole::Incoming) => {
            Some((-dir_x * (1.0 - progress), -dir_y * (1.0 - progress)))
        }
        (TransitionMotionKind::Reveal, TransitionRole::Outgoing)
        | (TransitionMotionKind::Slide, TransitionRole::Outgoing) => {
            Some((dir_x * progress, dir_y * progress))
        }
        _ => None,
    }
}

fn transition_preview_outgoing_should_draw_on_top(kind: &str) -> bool {
    matches!(
        kind,
        "reveal_left" | "reveal_right" | "reveal_up" | "reveal_down"
    )
}

fn transition_uses_wipe_crop(kind: &str) -> bool {
    matches!(kind, "wipe_right" | "wipe_left" | "wipeup" | "wipedown")
}

fn transition_preserves_slot_alpha(kind: &str) -> bool {
    transition_uses_wipe_crop(kind)
        || matches!(kind, "circle_open" | "circle_close")
        || transition_motion_kind(kind).is_some()
}

fn merged_transition_preview_masks(
    base_masks: &[ClipMask],
    tstate: Option<&TransitionState>,
) -> Vec<ClipMask> {
    let mut masks = base_masks.to_vec();
    if let Some(ts) = tstate {
        if let Some(mask) = transition_preview_mask(ts.kind.as_str(), ts.role, ts.progress) {
            masks.push(mask);
        }
    }
    masks
}

#[derive(Clone, Debug)]
pub struct ProgramClip {
    pub id: String,
    pub source_path: String,
    pub source_in_ns: u64,
    pub source_out_ns: u64,
    pub timeline_start_ns: u64,
    /// Color correction: brightness -1.0..1.0, contrast 0.0..2.0, saturation 0.0..2.0
    pub brightness: f64,
    pub contrast: f64,
    pub saturation: f64,
    /// Color temperature in Kelvin: 2000 (warm) to 10000 (cool). Default 6500.
    pub temperature: f64,
    /// Tint on green–magenta axis: −1.0 (green) to 1.0 (magenta). Default 0.0.
    pub tint: f64,
    pub brightness_keyframes: Vec<NumericKeyframe>,
    pub contrast_keyframes: Vec<NumericKeyframe>,
    pub saturation_keyframes: Vec<NumericKeyframe>,
    pub temperature_keyframes: Vec<NumericKeyframe>,
    pub tint_keyframes: Vec<NumericKeyframe>,
    /// Denoise strength: 0.0 (off) to 1.0 (heavy)
    pub denoise: f64,
    /// Sharpness: -1.0 (soften) to 1.0 (sharpen)
    pub sharpness: f64,
    /// Creative blur strength: 0.0 (off) to 1.0 (heavy)
    pub blur: f64,
    pub blur_keyframes: Vec<NumericKeyframe>,
    /// Video stabilization enabled (baked into proxy when proxy mode is on).
    pub vidstab_enabled: bool,
    pub vidstab_smoothing: f32,
    /// Volume multiplier: 0.0 (silent) to 2.0 (double), default 1.0
    pub volume: f64,
    pub voice_isolation: f64,
    pub voice_isolation_pad_ns: u64,
    pub voice_isolation_fade_ns: u64,
    pub voice_isolation_floor: f64,
    /// One-knob "Enhance Voice" toggle. When true, the realtime audio
    /// chain runs HPF + presence/mud EQ + gentle compression. Noise
    /// reduction is export-only (no GStreamer equivalent of `afftdn`).
    pub voice_enhance: bool,
    /// Strength of the realtime enhance chain, 0.0..=1.0. Mirrors the
    /// export-side scaling so the slider feels consistent across both.
    pub voice_enhance_strength: f32,
    pub volume_keyframes: Vec<NumericKeyframe>,
    /// Pre-merged speech intervals (clip-local source-time ns), padded by
    /// `voice_isolation_pad_ns`. Computed from either subtitle word timings
    /// or silence-detect analysis depending on the source clip's
    /// `voice_isolation_source`. Used by `volume_at_timeline_ns` to apply
    /// the gate during real-time playback.
    pub voice_isolation_merged_intervals_ns: Vec<(u64, u64)>,
    /// Audio pan: -1.0 (full left) to 1.0 (full right), default 0.0
    pub pan: f64,
    pub pan_keyframes: Vec<NumericKeyframe>,
    /// Audio channel extraction/downmix mode.
    pub audio_channel_mode: crate::model::clip::AudioChannelMode,
    /// 3-band parametric EQ settings.
    pub eq_bands: [crate::model::clip::EqBand; 3],
    pub eq_low_gain_keyframes: Vec<NumericKeyframe>,
    pub eq_mid_gain_keyframes: Vec<NumericKeyframe>,
    pub eq_high_gain_keyframes: Vec<NumericKeyframe>,
    /// 7-band match EQ from audio matching.
    pub match_eq_bands: Vec<crate::model::clip::EqBand>,
    pub rotate_keyframes: Vec<NumericKeyframe>,
    pub crop_left_keyframes: Vec<NumericKeyframe>,
    pub crop_right_keyframes: Vec<NumericKeyframe>,
    pub crop_top_keyframes: Vec<NumericKeyframe>,
    pub crop_bottom_keyframes: Vec<NumericKeyframe>,
    /// Crop pixels (left, right, top, bottom)
    pub crop_left: i32,
    pub crop_right: i32,
    pub crop_top: i32,
    pub crop_bottom: i32,
    /// Rotation in degrees (arbitrary angle).
    pub rotate: i32,
    pub flip_h: bool,
    pub flip_v: bool,
    /// Motion blur enabled (rendered in background prerender FFmpeg path
    /// only — not in the live GStreamer preview slots).
    pub motion_blur_enabled: bool,
    /// Motion blur shutter angle in degrees (0..=720).
    pub motion_blur_shutter_angle: f64,
    /// Title text overlay
    pub title_text: String,
    pub title_font: String,
    pub title_color: u32,
    pub title_x: f64,
    pub title_y: f64,
    /// Extended title styling fields.
    pub title_outline_color: u32,
    pub title_outline_width: f64,
    pub title_shadow: bool,
    pub title_shadow_color: u32,
    pub title_shadow_offset_x: f64,
    pub title_shadow_offset_y: f64,
    pub title_bg_box: bool,
    pub title_bg_box_color: u32,
    pub title_bg_box_padding: f64,
    pub title_clip_bg_color: u32,
    pub title_secondary_text: String,
    /// True when this clip is a standalone title clip (ClipKind::Title).
    pub is_title: bool,
    /// Playback speed multiplier (default 1.0). >1 = fast, <1 = slow.
    pub speed: f64,
    /// Optional variable speed keyframes over clip-local timeline.
    pub speed_keyframes: Vec<NumericKeyframe>,
    /// Slow-motion frame interpolation mode (export/prerender only).
    pub slow_motion_interp: crate::model::clip::SlowMotionInterp,
    /// Reverse playback when true.
    pub reverse: bool,
    /// Freeze-frame enabled for this clip (video-only hold semantics).
    pub freeze_frame: bool,
    /// Optional source timestamp (ns) used for freeze-frame sampling.
    pub freeze_frame_source_ns: Option<u64>,
    /// Optional explicit timeline hold duration (ns) for freeze-frames.
    pub freeze_frame_hold_duration_ns: Option<u64>,
    /// True for clips that have no video (audio-track clips). They are routed
    /// to a dedicated audio-only pipeline instead of the video player.
    pub is_audio_only: bool,
    /// Whether this clip's track has ducking enabled (volume reduces when dialogue is present).
    pub duck: bool,
    /// Per-track ducking amount in dB (negative, e.g. -6.0).
    pub duck_amount_db: f64,
    /// Applied LADSPA audio effects.
    pub ladspa_effects: Vec<crate::model::clip::LadspaEffect>,
    /// Pitch shift in semitones (−12 to +12). 0 = no shift.
    pub pitch_shift_semitones: f64,
    /// When true, preserve pitch during speed changes via Rubberband.
    pub pitch_preserve: bool,
    pub anamorphic_desqueeze: f64,
    /// Track index — higher index clips (B-roll, overlays) take priority in preview.
    pub track_index: usize,
    /// Transition to next clip on same track (e.g. "cross_dissolve").
    pub transition_after: String,
    /// Transition duration in nanoseconds.
    pub transition_after_ns: u64,
    /// Placement of the overlap relative to the cut.
    pub transition_alignment: TransitionAlignment,
    /// LUT file paths for color grading (used for proxy lookup when proxy mode is enabled).
    pub lut_paths: Vec<String>,
    /// Scale multiplier: 1.0 = fill frame, >1.0 = zoom in, <1.0 = shrink with black borders.
    pub scale: f64,
    pub scale_keyframes: Vec<NumericKeyframe>,
    /// Opacity multiplier for compositing: 0.0 = transparent, 1.0 = opaque.
    pub opacity: f64,
    pub opacity_keyframes: Vec<NumericKeyframe>,
    /// Compositing blend mode.
    pub blend_mode: crate::model::clip::BlendMode,
    /// Horizontal position offset: −1.0 (left) to 1.0 (right). Default 0.0.
    pub position_x: f64,
    pub position_x_keyframes: Vec<NumericKeyframe>,
    /// Vertical position offset: −1.0 (top) to 1.0 (bottom). Default 0.0.
    pub position_y: f64,
    pub position_y_keyframes: Vec<NumericKeyframe>,
    /// Shadow grading: −1.0 (crush) to 1.0 (lift). Default 0.0.
    pub shadows: f64,
    /// Midtone grading: −1.0 (darken) to 1.0 (brighten). Default 0.0.
    pub midtones: f64,
    /// Highlight grading: −1.0 (pull down) to 1.0 (boost). Default 0.0.
    pub highlights: f64,
    /// Exposure adjustment: −1.0 to 1.0. Default 0.0.
    pub exposure: f64,
    /// Black point adjustment: −1.0 to 1.0. Default 0.0.
    pub black_point: f64,
    /// Highlights warmth (orange–blue): −1.0 to 1.0. Default 0.0.
    pub highlights_warmth: f64,
    /// Highlights tint (green–magenta): −1.0 to 1.0. Default 0.0.
    pub highlights_tint: f64,
    /// Midtones warmth (orange–blue): −1.0 to 1.0. Default 0.0.
    pub midtones_warmth: f64,
    /// Midtones tint (green–magenta): −1.0 to 1.0. Default 0.0.
    pub midtones_tint: f64,
    /// Shadows warmth (orange–blue): −1.0 to 1.0. Default 0.0.
    pub shadows_warmth: f64,
    /// Shadows tint (green–magenta): −1.0 to 1.0. Default 0.0.
    pub shadows_tint: f64,
    /// Whether the source file contains an audio stream.
    pub has_audio: bool,
    /// True when this clip is a still image (PNG, JPEG, etc.).
    pub is_image: bool,
    /// True when this clip is an animated SVG image clip.
    pub animated_svg: bool,
    /// Authored animated duration for animated-SVG clips.
    pub media_duration_ns: Option<u64>,
    /// True when this clip is an adjustment layer (effects apply to composite below).
    pub is_adjustment: bool,
    /// Chroma key enabled flag.
    pub chroma_key_enabled: bool,
    /// Chroma key target color as 0xRRGGBB.
    pub chroma_key_color: u32,
    /// Chroma key tolerance: 0.0 (tight) to 1.0 (wide).
    pub chroma_key_tolerance: f32,
    /// Chroma key edge softness: 0.0 (hard) to 1.0 (soft).
    pub chroma_key_softness: f32,
    /// AI background removal enabled.
    pub bg_removal_enabled: bool,
    /// AI background removal threshold: 0.0 (aggressive) to 1.0 (conservative).
    pub bg_removal_threshold: f64,
    /// Animation to apply to a title clip.
    pub title_animation: crate::model::clip::TitleAnimation,
    /// Duration of the title animation (procedural part).
    pub title_animation_duration_ns: u64,
    /// Vector items for drawing clips.
    pub drawing_items: Vec<crate::model::clip::DrawingItem>,
    /// User-applied frei0r filter effects, ordered first-to-last.
    pub frei0r_effects: Vec<crate::model::clip::Frei0rEffect>,
    /// Optional motion-tracking attachment for the clip transform.
    pub tracking_binding: Option<crate::model::clip::TrackingBinding>,
    /// Shape masks applied to this clip.
    pub masks: Vec<crate::model::clip::ClipMask>,
    /// Optional HSL Qualifier (secondary color correction).
    pub hsl_qualifier: Option<crate::model::clip::HslQualifier>,
}

impl ProgramClip {
    pub fn source_duration_ns(&self) -> u64 {
        self.source_out_ns.saturating_sub(self.source_in_ns)
    }

    pub fn local_timeline_position_ns(&self, timeline_pos_ns: u64) -> u64 {
        timeline_pos_ns
            .saturating_sub(self.timeline_start_ns)
            .min(self.duration_ns())
    }

    pub fn scale_at_timeline_ns(&self, timeline_pos_ns: u64) -> f64 {
        ModelClip::evaluate_keyframed_value(
            &self.scale_keyframes,
            self.local_timeline_position_ns(timeline_pos_ns),
            self.scale,
        )
    }

    pub fn opacity_at_timeline_ns(&self, timeline_pos_ns: u64) -> f64 {
        ModelClip::evaluate_keyframed_value(
            &self.opacity_keyframes,
            self.local_timeline_position_ns(timeline_pos_ns),
            self.opacity,
        )
    }

    pub fn brightness_at_timeline_ns(&self, timeline_pos_ns: u64) -> f64 {
        ModelClip::evaluate_keyframed_value(
            &self.brightness_keyframes,
            self.local_timeline_position_ns(timeline_pos_ns),
            self.brightness,
        )
        .clamp(-1.0, 1.0)
    }

    pub fn contrast_at_timeline_ns(&self, timeline_pos_ns: u64) -> f64 {
        ModelClip::evaluate_keyframed_value(
            &self.contrast_keyframes,
            self.local_timeline_position_ns(timeline_pos_ns),
            self.contrast,
        )
        .clamp(0.0, 2.0)
    }

    pub fn saturation_at_timeline_ns(&self, timeline_pos_ns: u64) -> f64 {
        ModelClip::evaluate_keyframed_value(
            &self.saturation_keyframes,
            self.local_timeline_position_ns(timeline_pos_ns),
            self.saturation,
        )
        .clamp(0.0, 2.0)
    }

    pub fn temperature_at_timeline_ns(&self, timeline_pos_ns: u64) -> f64 {
        ModelClip::evaluate_keyframed_value(
            &self.temperature_keyframes,
            self.local_timeline_position_ns(timeline_pos_ns),
            self.temperature,
        )
        .clamp(2000.0, 10000.0)
    }

    pub fn tint_at_timeline_ns(&self, timeline_pos_ns: u64) -> f64 {
        ModelClip::evaluate_keyframed_value(
            &self.tint_keyframes,
            self.local_timeline_position_ns(timeline_pos_ns),
            self.tint,
        )
        .clamp(-1.0, 1.0)
    }

    pub fn blur_at_timeline_ns(&self, timeline_pos_ns: u64) -> f64 {
        ModelClip::evaluate_keyframed_value(
            &self.blur_keyframes,
            self.local_timeline_position_ns(timeline_pos_ns),
            self.blur,
        )
        .clamp(0.0, 1.0)
    }

    pub fn position_x_at_timeline_ns(&self, timeline_pos_ns: u64) -> f64 {
        ModelClip::evaluate_keyframed_value(
            &self.position_x_keyframes,
            self.local_timeline_position_ns(timeline_pos_ns),
            self.position_x,
        )
    }

    pub fn position_y_at_timeline_ns(&self, timeline_pos_ns: u64) -> f64 {
        ModelClip::evaluate_keyframed_value(
            &self.position_y_keyframes,
            self.local_timeline_position_ns(timeline_pos_ns),
            self.position_y,
        )
    }

    pub fn volume_at_timeline_ns(&self, timeline_pos_ns: u64) -> f64 {
        let local_ns = self.local_timeline_position_ns(timeline_pos_ns);
        let base_vol =
            ModelClip::evaluate_keyframed_value(&self.volume_keyframes, local_ns, self.volume);
        if self.voice_isolation > 0.0 && !self.voice_isolation_merged_intervals_ns.is_empty() {
            let rel_ns = timeline_pos_ns.saturating_sub(self.timeline_start_ns);
            let clip_local_ns = (rel_ns as f64 * self.speed) as u64;
            let fade_ns = self.voice_isolation_fade_ns;
            // Merged intervals are already padded — find the nearest boundary
            // for cosine fade. Distance 0 means we're inside a speech region.
            let mut min_dist: u64 = u64::MAX;
            for &(start, end) in &self.voice_isolation_merged_intervals_ns {
                if clip_local_ns >= start && clip_local_ns <= end {
                    min_dist = 0;
                    break;
                }
                let d = if clip_local_ns < start {
                    start - clip_local_ns
                } else {
                    clip_local_ns - end
                };
                min_dist = min_dist.min(d);
            }
            if min_dist > 0 {
                // Duck towards floor instead of silence.
                let duck_range = self.voice_isolation * (1.0 - self.voice_isolation_floor);
                if fade_ns > 0 && min_dist <= fade_ns {
                    let t = min_dist as f64 / fade_ns as f64;
                    let smooth = 0.5 * (1.0 - (t * std::f64::consts::PI).cos());
                    return base_vol * (1.0 - duck_range * smooth);
                }
                return base_vol * (1.0 - duck_range);
            }
        }
        base_vol
    }

    pub fn pan_at_timeline_ns(&self, timeline_pos_ns: u64) -> f64 {
        ModelClip::evaluate_keyframed_value(
            &self.pan_keyframes,
            self.local_timeline_position_ns(timeline_pos_ns),
            self.pan,
        )
        .clamp(-1.0, 1.0)
    }

    /// Evaluate keyframed EQ gain (dB) for band `idx` at a given timeline position.
    pub fn eq_gain_at_timeline_ns(&self, band_idx: usize, timeline_pos_ns: u64) -> f64 {
        let kfs = match band_idx {
            0 => &self.eq_low_gain_keyframes,
            1 => &self.eq_mid_gain_keyframes,
            _ => &self.eq_high_gain_keyframes,
        };
        ModelClip::evaluate_keyframed_value(
            kfs,
            self.local_timeline_position_ns(timeline_pos_ns),
            self.eq_bands[band_idx.min(2)].gain,
        )
        .clamp(-24.0, 24.0)
    }

    pub fn rotate_at_timeline_ns(&self, timeline_pos_ns: u64) -> i32 {
        ModelClip::evaluate_keyframed_value(
            &self.rotate_keyframes,
            self.local_timeline_position_ns(timeline_pos_ns),
            self.rotate as f64,
        )
        .round()
        .clamp(
            crate::model::transform_bounds::ROTATE_MIN_DEG,
            crate::model::transform_bounds::ROTATE_MAX_DEG,
        ) as i32
    }

    pub fn crop_left_at_timeline_ns(&self, timeline_pos_ns: u64) -> i32 {
        ModelClip::evaluate_keyframed_value(
            &self.crop_left_keyframes,
            self.local_timeline_position_ns(timeline_pos_ns),
            self.crop_left as f64,
        )
        .round()
        .clamp(
            crate::model::transform_bounds::CROP_MIN_PX,
            crate::model::transform_bounds::CROP_MAX_PX,
        ) as i32
    }

    pub fn crop_right_at_timeline_ns(&self, timeline_pos_ns: u64) -> i32 {
        ModelClip::evaluate_keyframed_value(
            &self.crop_right_keyframes,
            self.local_timeline_position_ns(timeline_pos_ns),
            self.crop_right as f64,
        )
        .round()
        .clamp(
            crate::model::transform_bounds::CROP_MIN_PX,
            crate::model::transform_bounds::CROP_MAX_PX,
        ) as i32
    }

    pub fn crop_top_at_timeline_ns(&self, timeline_pos_ns: u64) -> i32 {
        ModelClip::evaluate_keyframed_value(
            &self.crop_top_keyframes,
            self.local_timeline_position_ns(timeline_pos_ns),
            self.crop_top as f64,
        )
        .round()
        .clamp(
            crate::model::transform_bounds::CROP_MIN_PX,
            crate::model::transform_bounds::CROP_MAX_PX,
        ) as i32
    }

    pub fn crop_bottom_at_timeline_ns(&self, timeline_pos_ns: u64) -> i32 {
        ModelClip::evaluate_keyframed_value(
            &self.crop_bottom_keyframes,
            self.local_timeline_position_ns(timeline_pos_ns),
            self.crop_bottom as f64,
        )
        .round()
        .clamp(
            crate::model::transform_bounds::CROP_MIN_PX,
            crate::model::transform_bounds::CROP_MAX_PX,
        ) as i32
    }

    pub fn speed_at_local_timeline_ns(&self, local_timeline_ns: u64) -> f64 {
        if !self.speed_keyframes.is_empty() {
            let first_ns = self
                .speed_keyframes
                .iter()
                .map(|kf| kf.time_ns)
                .min()
                .unwrap_or(0);
            let last_ns = self
                .speed_keyframes
                .iter()
                .map(|kf| kf.time_ns)
                .max()
                .unwrap_or(0);
            if local_timeline_ns < first_ns || local_timeline_ns > last_ns {
                return self.speed.clamp(0.05, 16.0);
            }
        }
        ModelClip::evaluate_keyframed_value(&self.speed_keyframes, local_timeline_ns, self.speed)
            .clamp(0.05, 16.0)
    }

    pub fn speed_at_timeline_ns(&self, timeline_pos_ns: u64) -> f64 {
        let local_ns = self
            .local_timeline_position_ns(timeline_pos_ns)
            .min(self.duration_ns());
        self.speed_at_local_timeline_ns(local_ns)
    }

    fn integrated_source_distance_for_local_timeline_ns(&self, local_timeline_ns: u64) -> f64 {
        if local_timeline_ns == 0 {
            return 0.0;
        }
        if self.speed_keyframes.is_empty() {
            return local_timeline_ns as f64 * self.speed.clamp(0.05, 16.0);
        }
        let mut breakpoints: Vec<u64> = Vec::with_capacity(self.speed_keyframes.len() + 2);
        breakpoints.push(0);
        for kf in &self.speed_keyframes {
            if kf.time_ns > 0 && kf.time_ns < local_timeline_ns {
                breakpoints.push(kf.time_ns);
            }
        }
        breakpoints.push(local_timeline_ns);
        breakpoints.sort_unstable();
        breakpoints.dedup();

        const SAMPLES_PER_SEGMENT: u64 = 8;
        let mut integrated = 0.0f64;
        for win in breakpoints.windows(2) {
            let seg_start = win[0];
            let seg_end = win[1];
            let seg_len = seg_end - seg_start;
            if seg_len == 0 {
                continue;
            }
            let n = SAMPLES_PER_SEGMENT.min(seg_len / 1_000_000).max(1);
            for j in 0..n {
                let t0 = seg_start + (u128::from(seg_len) * u128::from(j) / u128::from(n)) as u64;
                let t1 =
                    seg_start + (u128::from(seg_len) * u128::from(j + 1) / u128::from(n)) as u64;
                let dt = t1 - t0;
                let mid = t0 + dt / 2;
                integrated += self.speed_at_local_timeline_ns(mid) * dt as f64;
            }
        }
        integrated
    }

    fn min_effective_speed_hint(&self) -> f64 {
        let mut min_speed = self.speed.clamp(0.05, 16.0);
        for kf in &self.speed_keyframes {
            min_speed = min_speed.min(kf.value.clamp(0.05, 16.0));
        }
        min_speed
    }

    fn playback_duration_ns(&self) -> u64 {
        let src = self.source_duration_ns();
        if !self.speed_keyframes.is_empty() {
            let min_speed = self.min_effective_speed_hint();
            let upper = (src as f64 / min_speed) as u64 + 1_000_000_000;
            let (mut lo, mut hi): (u64, u64) = (0, upper);
            for _ in 0..40 {
                let mid = lo + (hi - lo) / 2;
                if self.integrated_source_distance_for_local_timeline_ns(mid) < src as f64 {
                    lo = mid + 1;
                } else {
                    hi = mid;
                }
            }
            hi.max(1)
        } else if self.speed > 0.0 {
            (src as f64 / self.speed) as u64
        } else {
            src
        }
    }

    pub fn is_freeze_frame(&self) -> bool {
        self.freeze_frame && !self.is_audio_only
    }

    pub fn freeze_frame_source_time_ns(&self) -> Option<u64> {
        if !self.is_freeze_frame() {
            return None;
        }
        let requested = self.freeze_frame_source_ns.unwrap_or(self.source_in_ns);
        if self.source_out_ns > self.source_in_ns {
            Some(requested.clamp(self.source_in_ns, self.source_out_ns.saturating_sub(1)))
        } else {
            Some(self.source_in_ns)
        }
    }

    pub fn freeze_frame_duration_ns(&self) -> Option<u64> {
        if !self.is_freeze_frame() {
            return None;
        }
        let _source_time_ns = self.freeze_frame_source_time_ns()?;
        Some(
            self.freeze_frame_hold_duration_ns
                .filter(|duration| *duration > 0)
                .unwrap_or_else(|| self.playback_duration_ns()),
        )
    }

    pub fn has_embedded_audio(&self) -> bool {
        self.has_audio && !self.is_freeze_frame()
    }

    /// Timeline duration accounting for speed.
    pub fn duration_ns(&self) -> u64 {
        self.freeze_frame_duration_ns()
            .unwrap_or_else(|| self.playback_duration_ns())
    }

    pub fn has_outgoing_transition(&self) -> bool {
        !self.transition_after.trim().is_empty() && self.transition_after_ns > 0
    }

    pub fn transition_cut_split(&self) -> crate::model::transition::TransitionCutSplit {
        self.transition_alignment
            .split_duration(self.transition_after_ns)
    }

    pub fn timeline_end_ns(&self) -> u64 {
        self.timeline_start_ns + self.duration_ns()
    }
    /// Convert a timeline position offset to the corresponding source file position.
    pub fn source_pos_ns(&self, timeline_pos_ns: u64) -> u64 {
        if let Some(source_ns) = self.freeze_frame_source_time_ns() {
            return source_ns;
        }
        let offset = timeline_pos_ns.saturating_sub(self.timeline_start_ns);
        let src_span = self.source_duration_ns();
        if src_span == 0 {
            return self.source_in_ns;
        }
        let max_delta = src_span.saturating_sub(1);
        let delta = if !self.speed_keyframes.is_empty() {
            (self.integrated_source_distance_for_local_timeline_ns(offset) as u64).min(max_delta)
        } else {
            ((offset as f64 * self.speed) as u64).min(max_delta)
        };
        if self.reverse {
            self.source_out_ns.saturating_sub(1).saturating_sub(delta)
        } else {
            self.source_in_ns + delta
        }
    }

    pub fn seek_rate(&self) -> f64 {
        if self.is_freeze_frame() {
            return 1.0;
        }
        let base = if self.speed > 0.0 { self.speed } else { 1.0 };
        if self.reverse {
            -base
        } else {
            base
        }
    }

    pub fn seek_start_ns(&self, source_pos_ns: u64) -> u64 {
        if self.reverse {
            self.source_in_ns.min(source_pos_ns)
        } else {
            source_pos_ns
        }
    }

    pub fn seek_stop_ns(&self, source_pos_ns: u64) -> u64 {
        if self.is_freeze_frame() {
            return source_pos_ns.saturating_add(1);
        }
        if self.reverse {
            source_pos_ns.max(self.source_in_ns)
        } else {
            self.source_out_ns.max(source_pos_ns)
        }
    }

    pub fn audio_seek_flags(&self) -> gst::SeekFlags {
        if self.reverse {
            gst::SeekFlags::FLUSH | gst::SeekFlags::ACCURATE
        } else {
            gst::SeekFlags::FLUSH | gst::SeekFlags::KEY_UNIT
        }
    }
}

/// One decoder branch connected to the compositor/audiomixer.
struct VideoSlot {
    /// Index into `ProgramPlayer::clips` for the clip loaded in this slot.
    clip_idx: usize,
    /// `uridecodebin` element for this slot.
    decoder: gst::Element,
    /// True once the decoder's video pad has been linked into effects_bin.
    video_linked: Arc<AtomicBool>,
    /// True once the decoder's audio pad has been linked into audioconvert.
    audio_linked: Arc<AtomicBool>,
    /// Video effect chain bin between decoder and compositor.
    effects_bin: gst::Bin,
    /// Compositor sink pad for this slot's video.
    compositor_pad: Option<gst::Pad>,
    /// Audiomixer sink pad for this slot's audio.
    audio_mixer_pad: Option<gst::Pad>,
    /// `audioconvert` element between decoder audio and audiomixer (must be cleaned up).
    audio_conv: Option<gst::Element>,
    /// `audioresample` element normalizing clip audio to preview mix sample rate.
    audio_resample: Option<gst::Element>,
    /// `capsfilter` element locking clip audio to preview mix caps before audiomixer.
    audio_capsfilter: Option<gst::Element>,
    /// Optional per-slot `equalizer-nbands` element for 3-band parametric EQ.
    audio_equalizer: Option<gst::Element>,
    /// Optional per-slot `equalizer-nbands` element for 7-band match EQ (mic match).
    /// Linked before `audio_equalizer` so the user 3-band EQ can be applied on top.
    audio_match_equalizer: Option<gst::Element>,
    /// Optional per-slot `audiopanorama` element for pan automation.
    audio_panorama: Option<gst::Element>,
    /// Optional per-slot `level` element for per-track metering.
    audio_level: Option<gst::Element>,
    /// Per-slot video effect elements.
    videobalance: Option<gst::Element>,
    /// frei0r coloradj_RGB for per-channel RGB gains (temperature/tint).
    coloradj_rgb: Option<gst::Element>,
    /// frei0r 3-point-color-balance for shadows/midtones/highlights.
    colorbalance_3pt: Option<gst::Element>,
    gaussianblur: Option<gst::Element>,
    /// frei0r squareblur for creative blur effect (RGBA-native, no format conversion needed).
    squareblur: Option<gst::Element>,
    videocrop: Option<gst::Element>,
    videobox_crop_alpha: Option<gst::Element>,
    /// Shared crop values for alpha-based crop probe.
    /// (crop_left, crop_right, crop_top, crop_bottom, letterbox_h, letterbox_v, proj_w, proj_h)
    /// Crop values are in project pixels; the probe scales them to frame pixels.
    /// letterbox_h/v are in frame pixels (already at preview resolution).
    crop_alpha_state: Arc<Mutex<(i32, i32, i32, i32, i32, i32, i32, i32)>>,
    imagefreeze: Option<gst::Element>,
    videoflip_rotate: Option<gst::Element>,
    videoflip_flip: Option<gst::Element>,
    textoverlay: Option<gst::Element>,
    #[allow(dead_code)]
    alpha_filter: Option<gst::Element>,
    alpha_chroma_key: Option<gst::Element>,
    capsfilter_zoom: Option<gst::Element>,
    videobox_zoom: Option<gst::Element>,
    /// User-applied frei0r filter effect elements (from `clip.frei0r_effects`).
    frei0r_user_effects: Vec<gst::Element>,
    /// Queue between effects_bin and compositor to decouple caps negotiation.
    slot_queue: Option<gst::Element>,
    /// Monotonic counter incremented when a buffer passes through queue→compositor.
    /// Used to detect when post-seek buffers have reached the compositor.
    comp_arrival_seq: Arc<AtomicU64>,
    /// When true, the slot is hidden (alpha=0, volume=0) — kept alive for
    /// potential re-entry but not counted as "active" for boundary detection.
    hidden: bool,
    /// True when this slot uses a prerendered animated-SVG media file.
    animated_svg_rendered: bool,
    /// True for synthetic prerender-composite slots.
    is_prerender_slot: bool,
    /// Timeline start of the prerender segment for synthetic prerender slots.
    prerender_segment_start_ns: Option<u64>,
    /// Nonzero when this slot was created for a clip entering early via a
    /// transition overlap. The value equals the preceding clip's
    /// before-cut overlap amount and is added to timeline_pos when computing
    /// source-file seek positions.
    transition_enter_offset_ns: u64,
    /// True when the clip uses a non-Normal blend mode.  Zoom is forced
    /// through the effects-bin path (not compositor-pad) so the captured
    /// buffer is at project resolution with position baked in.
    is_blend_mode: bool,
    /// Intended compositor alpha for blend-mode clips (used by the blend
    /// probe since the actual compositor pad alpha is forced to 0).
    blend_alpha: Arc<Mutex<f64>>,
    /// Shared mask data read by the mask pad probe.  Updated live from
    /// inspector slider changes without requiring a pipeline rebuild.
    mask_data: Arc<Mutex<Vec<crate::model::clip::ClipMask>>>,
    /// Shared HSL qualifier read by the HSL pad probe.  Updated live from
    /// inspector slider changes without requiring a pipeline rebuild.
    hsl_data: Arc<Mutex<Option<crate::model::clip::HslQualifier>>>,
}

/// Result of `compute_reuse_plan()`: whether all current decoder slots can be
/// reused for the next set of desired clips (continuing decoder fast path).
struct SlotReusePlan {
    all_reusable: bool,
    /// For each slot index, the desired clip index it should switch to.
    mappings: Vec<(usize, usize)>,
}

#[derive(Clone)]
struct PrerenderSegment {
    key: String,
    path: String,
    start_ns: u64,
    end_ns: u64,
    signature: u64,
}

#[derive(Clone)]
struct TransitionPrerenderSpec {
    outgoing_input: usize,
    incoming_input: usize,
    xfade_transition: String,
    duration_ns: u64,
    before_cut_ns: u64,
    after_cut_ns: u64,
}

struct PrerenderJobResult {
    key: String,
    path: String,
    start_ns: u64,
    end_ns: u64,
    signature: u64,
    success: bool,
    generation: u64,
    cache_persistent: bool,
}

pub struct BackgroundPrerenderProgress {
    pub total: usize,
    pub completed: usize,
    pub in_flight: bool,
}

#[derive(Clone)]
pub struct TransitionPrerenderMetricPoint {
    pub kind: String,
    pub hit: u64,
    pub miss: u64,
    pub hit_rate_percent: f64,
}

#[derive(Clone)]
pub struct ProgramPerformanceSnapshot {
    pub player_state: String,
    pub playback_priority: String,
    pub timeline_pos_ns: u64,
    pub background_prerender_enabled: bool,
    pub prerender_total_requested: usize,
    pub prerender_pending: usize,
    pub prerender_ready: usize,
    pub prerender_failed: usize,
    pub prerender_cache_hits: u64,
    pub prerender_cache_misses: u64,
    pub prerender_cache_hit_rate_percent: f64,
    pub prewarmed_boundary_ns: Option<u64>,
    pub active_prerender_segment_key: Option<String>,
    pub rebuild_history_samples: usize,
    pub rebuild_history_recent_ms: Vec<u64>,
    pub rebuild_latest_ms: Option<u64>,
    pub rebuild_p50_ms: Option<u64>,
    pub rebuild_p75_ms: Option<u64>,
    pub transition_hits_total: u64,
    pub transition_misses_total: u64,
    pub transition_hit_rate_percent: f64,
    pub transition_low_hitrate_pressure: bool,
    pub prerender_queue_pressure: bool,
    pub transition_metrics: Vec<TransitionPrerenderMetricPoint>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum AudioCurrentSource {
    AudioClip(usize),
    ReverseVideoClip(usize),
}

/// Apply a single blend-mode pixel operation.  Returns the blended RGB.
/// `s` = source (overlay), `b` = base (background).
#[inline]
fn blend_pixel(
    mode: crate::model::clip::BlendMode,
    sr: f32,
    sg: f32,
    sb: f32,
    br: f32,
    bg: f32,
    bb: f32,
) -> (f32, f32, f32) {
    use crate::model::clip::BlendMode;
    match mode {
        BlendMode::Normal => (sr, sg, sb),
        BlendMode::Multiply => (sr * br, sg * bg, sb * bb),
        BlendMode::Screen => (
            1.0 - (1.0 - sr) * (1.0 - br),
            1.0 - (1.0 - sg) * (1.0 - bg),
            1.0 - (1.0 - sb) * (1.0 - bb),
        ),
        BlendMode::Overlay => {
            let ov = |s: f32, b: f32| -> f32 {
                if b < 0.5 {
                    2.0 * s * b
                } else {
                    1.0 - 2.0 * (1.0 - s) * (1.0 - b)
                }
            };
            (ov(sr, br), ov(sg, bg), ov(sb, bb))
        }
        BlendMode::Add => ((sr + br).min(1.0), (sg + bg).min(1.0), (sb + bb).min(1.0)),
        BlendMode::Difference => ((sr - br).abs(), (sg - bg).abs(), (sb - bb).abs()),
        BlendMode::SoftLight => {
            let sl = |s: f32, b: f32| -> f32 {
                if s <= 0.5 {
                    b - (1.0 - 2.0 * s) * b * (1.0 - b)
                } else {
                    let d = if b <= 0.25 {
                        ((16.0 * b - 12.0) * b + 4.0) * b
                    } else {
                        b.sqrt()
                    };
                    b + (2.0 * s - 1.0) * (d - b)
                }
            };
            (sl(sr, br), sl(sg, bg), sl(sb, bb))
        }
    }
}

#[inline]
fn coloradj_param_to_gain(param: f64) -> f64 {
    (param.clamp(0.0, 1.0) * 2.0).clamp(0.0, 2.0)
}

#[inline]
fn apply_videobalance_rgb(r: f64, g: f64, b: f64, params: VBParams) -> (f64, f64, f64) {
    let y = 0.299 * r + 0.587 * g + 0.114 * b;
    let mut u = 0.492 * (b - y);
    let mut v = 0.877 * (r - y);
    let y = ((y - 0.5) * params.contrast + 0.5 + params.brightness).clamp(0.0, 1.0);
    let hue_rad = params.hue * std::f64::consts::PI;
    let (sin_h, cos_h) = hue_rad.sin_cos();
    let rot_u = u * cos_h - v * sin_h;
    let rot_v = u * sin_h + v * cos_h;
    u = rot_u * params.saturation;
    v = rot_v * params.saturation;
    let r = (y + v / 0.877).clamp(0.0, 1.0);
    let b = (y + u / 0.492).clamp(0.0, 1.0);
    let g = ((y - 0.299 * r - 0.114 * b) / 0.587).clamp(0.0, 1.0);
    (r, g, b)
}

#[inline]
fn apply_three_point_channel(v: f64, coeffs: &ThreePointParabolaCoeffs) -> f64 {
    (coeffs.a * v * v + coeffs.b * v + coeffs.c).clamp(0.0, 1.0)
}

fn apply_adjustment_overlays_rgba(
    data: &mut [u8],
    width: usize,
    height: usize,
    overlays: &[AdjustmentOverlay],
) {
    if width == 0 || height == 0 || data.len() < width.saturating_mul(height).saturating_mul(4) {
        return;
    }
    for overlay in overlays {
        if overlay.opacity <= f64::EPSILON {
            continue;
        }
        let resolved_scope = overlay.scope.resolve(width, height);
        let Some((x0, y0, x1, y1)) = resolved_scope.pixel_bounds(width, height) else {
            continue;
        };
        for y in y0..y1 {
            let row_start = y * width * 4;
            for x in x0..x1 {
                if !resolved_scope.contains_pixel(x, y) {
                    continue;
                }
                let mask_alpha = overlay
                    .mask
                    .as_ref()
                    .map(|mask| mask.alpha_at_canvas_pixel(x, y, width, height))
                    .unwrap_or(1.0);
                if mask_alpha <= f64::EPSILON {
                    continue;
                }
                let effective_opacity = (overlay.opacity * mask_alpha).clamp(0.0, 1.0);
                if effective_opacity <= f64::EPSILON {
                    continue;
                }
                let idx = row_start + x * 4;
                let pixel = &mut data[idx..idx + 4];
                let base_r = pixel[0] as f64 / 255.0;
                let base_g = pixel[1] as f64 / 255.0;
                let base_b = pixel[2] as f64 / 255.0;
                let mut out_r = base_r;
                let mut out_g = base_g;
                let mut out_b = base_b;
                if let Some(ref lut) = overlay.lut {
                    let (r, g, b) = lut.apply_rgb(pixel[0], pixel[1], pixel[2]);
                    out_r = r as f64 / 255.0;
                    out_g = g as f64 / 255.0;
                    out_b = b as f64 / 255.0;
                }
                if let Some(vb) = overlay.vb_params {
                    (out_r, out_g, out_b) = apply_videobalance_rgb(out_r, out_g, out_b, vb);
                }
                if let Some(coloradj) = overlay.coloradj_params {
                    out_r = (out_r * coloradj_param_to_gain(coloradj.r)).clamp(0.0, 1.0);
                    out_g = (out_g * coloradj_param_to_gain(coloradj.g)).clamp(0.0, 1.0);
                    out_b = (out_b * coloradj_param_to_gain(coloradj.b)).clamp(0.0, 1.0);
                }
                if let Some(ref parabola) = overlay.three_point {
                    out_r = apply_three_point_channel(out_r, &parabola.r);
                    out_g = apply_three_point_channel(out_g, &parabola.g);
                    out_b = apply_three_point_channel(out_b, &parabola.b);
                }
                pixel[0] = ((base_r + (out_r - base_r) * effective_opacity).clamp(0.0, 1.0) * 255.0)
                    .round() as u8;
                pixel[1] = ((base_g + (out_g - base_g) * effective_opacity).clamp(0.0, 1.0) * 255.0)
                    .round() as u8;
                pixel[2] = ((base_b + (out_b - base_b) * effective_opacity).clamp(0.0, 1.0) * 255.0)
                    .round() as u8;
            }
        }
    }
}

/// Captured overlay frame for blend-mode compositing in the preview pipeline.
/// The blend probe on the compositor output reads these to apply pixel-accurate
/// blend math against the real base layers.
struct BlendOverlay {
    data: Vec<u8>,
    width: usize,
    height: usize,
    blend_mode: crate::model::clip::BlendMode,
    opacity: f64,
    zorder: u32,
}

/// Resolved live adjustment-layer preview state at the current playhead.
#[derive(Clone)]
struct AdjustmentOverlay {
    track_index: usize,
    scope: AdjustmentScopeShape,
    mask: Option<crate::media::mask_alpha::PreparedCanvasMasks>,
    opacity: f64,
    vb_params: Option<VBParams>,
    coloradj_params: Option<ColorAdjRGBParams>,
    three_point: Option<ThreePointParabola>,
    lut: Option<Arc<CubeLut>>,
}

pub struct ProgramPlayer {
    /// The single GStreamer pipeline containing compositor + audiomixer.
    pipeline: gst::Pipeline,
    /// Compositor element that merges all video layers.
    compositor: gst::Element,
    /// Audio mixer element that merges all audio layers.
    audiomixer: gst::Element,
    state: PlayerState,
    pub clips: Vec<ProgramClip>,
    pub audio_clips: Vec<ProgramClip>,
    /// Separate playbin for audio-only clips (music tracks etc.).
    audio_pipeline: gst::Element,
    audio_current_source: Option<AudioCurrentSource>,
    /// Indices of audio clips currently playing in the multi-clip audio mixer.
    audio_multi_active: Vec<usize>,
    /// Separate pipeline for mixing multiple simultaneous audio-track clips.
    /// Built on demand when >1 audio clip is active; torn down when ≤1 remain.
    audio_multi_pipeline: Option<gst::Pipeline>,
    /// Audiomixer pads in the multi pipeline, keyed by audio_clip index.
    /// Used for live volume updates during playback.
    audio_multi_pads: HashMap<usize, gst::Pad>,
    /// Audiopanorama elements in the multi pipeline, keyed by audio_clip index.
    audio_multi_pan_elems: HashMap<usize, gst::Element>,
    /// Decoder elements in the multi pipeline, keyed by audio_clip index.
    /// Retained so same-active-set boundary resyncs can seek in place without
    /// tearing down the whole multi-audio pipeline.
    audio_multi_decoders: HashMap<usize, gst::Element>,
    /// Clip index whose audiomixer pad is temporarily forced to 0.0 while
    /// reverse audio for that clip is routed through `audio_pipeline`.
    reverse_video_ducked_clip_idx: Option<usize>,
    /// Active video decoder slots.
    slots: Vec<VideoSlot>,
    /// Cached timeline position in nanoseconds (updated by `poll`).
    pub timeline_pos_ns: u64,
    /// Total timeline duration.
    pub timeline_dur_ns: u64,
    /// Timeline position at the most recent seek (pipeline running-time base).
    base_timeline_ns: u64,
    /// Project output dimensions (used for scale/position math). Default 1920×1080.
    project_width: u32,
    project_height: u32,
    playback_priority: PlaybackPriority,
    proxy_enabled: bool,
    preview_luts: bool,
    proxy_scale_divisor: u32,
    /// Use audio-only decoder slots for fully-occluded clips (experimental).
    experimental_preview_optimizations: bool,
    /// Pre-build upcoming decoder slots for near-instant clip transitions.
    realtime_preview: bool,
    /// Prewarm upcoming boundaries earlier during active playback.
    background_prerender: bool,
    prerender_preset: PrerenderEncodingPreset,
    prerender_crf: u32,
    crossfade_enabled: bool,
    crossfade_curve: CrossfadeCurve,
    crossfade_duration_ns: u64,
    /// Automatic ducking: reduce volume on duck-flagged tracks when dialogue is active.
    duck_enabled: bool,
    /// Ducking gain (linear multiplier, e.g., 0.5 for -6dB).
    duck_gain: f64,
    /// When true, all audio output is suppressed (voiceover recording mode).
    master_muted: bool,
    proxy_paths: HashMap<String, String>,
    /// Proxy cache keys already reported as fallback-to-original in this session.
    /// Prevents warning spam while proxies are still being generated.
    proxy_fallback_warned_keys: HashSet<String>,
    /// Animated-SVG render paths keyed by render key.
    animated_svg_paths: HashMap<String, String>,
    /// Bg-removed file paths: source_path → bg_removed file path.
    bg_removal_paths: HashMap<String, String>,
    /// Voice-enhance prerendered file paths, keyed by
    /// `voice_enhance_cache::cache_key(source_path, strength)`.
    /// Populated by [`Self::update_voice_enhance_paths`] from
    /// `VoiceEnhanceCache::paths`. The video stream of the cached
    /// file is a copy of the source; the audio has been re-encoded
    /// through the same FFmpeg chain that the export side uses, so
    /// preview and export are byte-identical.
    voice_enhance_paths: HashMap<String, String>,
    /// AI frame-interpolation sidecar paths: clip_id → sidecar file path.
    /// Populated by [`Self::update_frame_interp_paths`] from
    /// `FrameInterpCache::snapshot_paths_by_clip_id`.
    frame_interp_paths: HashMap<String, String>,
    /// Cache for per-path audio-stream probe results.
    audio_stream_probe_cache: HashMap<String, bool>,
    /// GStreamer `level` element on audiomixer output for metering.
    level_element: Option<gst::Element>,
    /// GStreamer `volume` element applied to the post-mix master bus.
    /// Set by `set_master_gain_db()` to implement the Loudness Radar
    /// project-level master gain without a pipeline rebuild.
    master_volume_element: Option<gst::Element>,
    /// GStreamer `level` element on the audio-only pipeline for metering.
    #[allow(dead_code)]
    level_element_audio: Option<gst::Element>,
    /// GStreamer `volume` element on the audio-only pipeline for volume control.
    /// Using a dedicated element avoids playbin's StreamVolume which can interact
    /// with PulseAudio/PipeWire flat-volume behaviour and inadvertently change
    /// the main pipeline's audio level.
    audio_volume_element: Option<gst::Element>,
    /// GStreamer `equalizer-nbands` element on the audio-only pipeline for EQ.
    audio_eq_element: Option<gst::Element>,
    /// GStreamer `audiopanorama` element on the audio-only pipeline for panning.
    audio_panorama_element: Option<gst::Element>,
    pub audio_peak_db: [f64; 2],
    pub audio_track_peak_db: Vec<[f64; 2]>,
    jkl_rate: f64,
    /// Latest RGBA frame captured from the scope appsink.
    latest_scope_frame: Arc<Mutex<Option<ScopeFrame>>>,
    /// Latest RGBA frame captured from compositor src (before preview downscale).
    latest_compositor_frame: Arc<Mutex<Option<ScopeFrame>>>,
    /// Captured overlay frames for blend-mode clips.  Keyed by clip_idx.
    /// The blend probe on compositor output reads these to apply per-pixel
    /// blend math against the real base layers.
    blend_overlays: Arc<Mutex<HashMap<usize, BlendOverlay>>>,
    /// Adjustment layer overlays for post-compositor color grading.
    adjustment_overlays: Arc<Mutex<Vec<AdjustmentOverlay>>>,
    /// Shared timeline position (ns) for the adjustment probe to check time ranges.
    adjustment_timeline_pos: Arc<AtomicU64>,
    /// Permanent adjustment layer videobalance element between compositor output
    /// and display chain.  Set to neutral when no adjustment layer is active.
    adj_videobalance: gst::Element,
    /// Permanent adjustment layer frei0r coloradj-rgb element (temperature/tint).
    /// None if frei0r is not available.
    adj_coloradj: Option<gst::Element>,
    /// Permanent adjustment layer frei0r three-point-color-balance element.
    /// None if frei0r is not available.
    adj_3point: Option<gst::Element>,
    /// When false, the scope appsink callback skips frame allocation (scopes panel hidden).
    scope_enabled: Arc<AtomicBool>,
    /// Monotonic counter incremented whenever a new scope frame is captured.
    scope_frame_seq: Arc<AtomicU64>,
    /// Monotonic counter incremented whenever a new compositor-src frame is captured.
    compositor_frame_seq: Arc<AtomicU64>,
    /// When true the compositor probe copies full-res frames into latest_compositor_frame.
    /// Default false — only enabled during MCP frame export to avoid ~250 MB/s of copies.
    compositor_capture_enabled: Arc<AtomicBool>,
    /// Index of the top-priority clip for `current_clip_idx()` / effects queries.
    current_idx: Option<usize>,
    /// Video sink bin (display + scope). Kept to avoid early drop.
    _video_sink_bin: gst::Element,
    /// Capsfilter on compositor output for current preview processing resolution.
    comp_capsfilter: gst::Element,
    /// Capsfilter on the black background source (must match compositor output).
    black_capsfilter: gst::Element,
    /// Capsfilter on final monitor output after preview-quality scaling.
    preview_capsfilter: gst::Element,
    /// The autoaudiosink element in the main pipeline (needed for locked-state management).
    audio_sink: gst::Element,
    /// The glsinkbin (or gtk4paintablesink) display element.  Locked during
    /// playing_pulse for 3+ tracks so the pipeline can reach Playing without
    /// waiting for the GTK main thread to service the display sink's preroll.
    display_sink: gst::Element,
    /// The always-on black videotestsrc (compositor background pad).
    /// Must be flushed along with decoders for 3+ tracks to trigger a fresh
    /// compositor re-preroll (the aggregator clears ALL pad buffers on flush).
    background_src: gst::Element,
    /// Requested compositor sink pad for the always-on black background branch.
    #[allow(dead_code)]
    background_compositor_pad: gst::Pad,
    /// Current preview quality divisor (used for preview processing and output caps).
    preview_divisor: u32,
    /// Display queue between tee and gtk4paintablesink.  Switched to leaky
    /// during transform live mode to prevent backpressure blocking.
    display_queue: Option<gst::Element>,
    /// True when playback is using drop-late display policy to keep displayed
    /// video aligned to the audio clock.
    playback_drop_late_active: bool,
    /// True when per-slot queues are in drop-late mode to avoid backlog during
    /// continuity-prioritized playback.
    slot_queue_drop_late_active: bool,
    /// True when the transform tool has temporarily set the pipeline to live
    /// mode for interactive preview during drag.
    transform_live: bool,
    /// Wall-clock instant when playback last entered Playing state.
    play_start: Option<Instant>,
    /// Last timeline boundary timestamp prewarmed for upcoming playback handoff.
    prewarmed_boundary_ns: Option<u64>,
    /// Ring buffer of recent playback-boundary rebuild durations (ms) for
    /// adaptive wait budget tuning. Newest entry at index
    /// `(rebuild_history_cursor - 1) % N`.
    rebuild_history_ms: [u64; 8],
    /// Write cursor into `rebuild_history_ms`.
    rebuild_history_cursor: usize,
    /// Number of entries actually recorded (≤ `rebuild_history_ms.len()`).
    rebuild_history_count: usize,
    /// Pre-preroll sidecar pipelines for upcoming boundary clips.
    /// These run asynchronously in background threads, decoding the first
    /// frame to warm OS file cache and codec state before handoff.
    prepreroll_sidecars: Vec<gst::Pipeline>,
    /// Disk-backed prerendered complex sections (video-only) keyed by boundary signature.
    prerender_segments: HashMap<String, PrerenderSegment>,
    /// In-flight prerender job keys.
    prerender_pending: HashSet<String>,
    /// Scheduling priority scores for in-flight prerender jobs.
    prerender_pending_priority: HashMap<String, u64>,
    /// Failed prerender job keys (skip immediate retry churn).
    prerender_failed: HashSet<String>,
    /// Keys whose prerender jobs have been superseded by a newer request for
    /// the same boundary region.  Results for these keys are discarded on arrival.
    prerender_superseded: HashSet<String>,
    /// Latest pending prerender key per boundary region `(start_ns, end_ns)`.
    /// Used to detect and supersede stale jobs when a new request arrives for
    /// the same region with a different signature.
    prerender_boundary_latest: HashMap<(u64, u64), String>,
    /// Result channel for background prerender jobs.
    prerender_result_rx: mpsc::Receiver<PrerenderJobResult>,
    /// Sender cloned into background prerender workers.
    prerender_result_tx: mpsc::Sender<PrerenderJobResult>,
    /// Runtime cache root for temporary prerendered sections.
    prerender_cache_root: PathBuf,
    /// Whether `prerender_cache_root` should survive project reloads/app restarts.
    prerender_cache_persistent: bool,
    /// Files created during this runtime, removed on cleanup.
    prerender_runtime_files: HashSet<String>,
    /// Monotonic token used to discard stale background prerender job results after
    /// project reloads, cache-root switches, or preference toggles.
    prerender_generation: u64,
    /// Active clip signature represented by the current prerender segment slot.
    prerender_active_clips: Option<Vec<usize>>,
    /// Current prerender segment key when a synthetic prerender slot is active.
    current_prerender_segment_key: Option<String>,
    /// Total prerender jobs requested this session (for status UI).
    prerender_total_requested: usize,
    /// Prerender segment cache lookup hit count.
    prerender_cache_hits: u64,
    /// Prerender segment cache lookup miss count.
    prerender_cache_misses: u64,
    /// Transition prerender usage counters keyed by transition kind:
    /// value = (hit_count, miss_count).
    transition_prerender_metrics: HashMap<String, (u64, u64)>,
    /// Last idle prerender scan time (throttles paused/stopped scheduling).
    last_idle_prerender_scan_at: Option<Instant>,
    /// Set when a prerender segment becomes ready while already inside its range;
    /// consumed in `poll()` to trigger a live→prerender swap rebuild.
    pending_prerender_promote: bool,
    /// Frame duration in nanoseconds, derived from project frame rate.
    /// Used to quantize seek positions and deduplicate same-frame seeks.
    frame_duration_ns: u64,
    frame_rate_num: u32,
    frame_rate_den: u32,
    /// Last frame-quantized seek position (nanoseconds). When a new seek
    /// lands on the same frame, the pipeline work is skipped entirely.
    last_seeked_frame_pos: Option<u64>,
    /// Instant of the last completed seek — used to detect rapid interactive
    /// scrubbing (seeks arriving <300ms apart) and reduce wait budgets.
    last_seek_instant: Option<Instant>,
    /// Small previous/current/next frame cache around the paused playhead.
    short_frame_cache: ShortFrameCache,
    /// Deferred cache capture ticket for non-blocking 3+ track playing pulses.
    pending_short_frame_capture: Option<PendingShortFrameCapture>,
    /// Last boundary-clip signature rebuilt during playback.
    last_boundary_rebuild_clips: Vec<usize>,
    /// Wall-clock time of the last playback boundary rebuild attempt.
    last_boundary_rebuild_at: Option<Instant>,
    /// Last time a main-pipeline not-negotiated recovery rebuild was attempted.
    last_not_negotiated_recover_at: Option<Instant>,
    /// Cache of parsed 3D LUT files keyed by file path.
    /// Avoids re-parsing the same `.cube` file on every slot rebuild.
    lut_cache: HashMap<String, Arc<CubeLut>>,
}

impl ProgramPlayer {
    pub fn new() -> Result<(Self, gdk4::Paintable, gdk4::Paintable)> {
        // -- Display paintable --------------------------------------------------
        let paintablesink = gst::ElementFactory::make("gtk4paintablesink")
            .build()
            .map_err(|_| anyhow!("gtk4paintablesink not available"))?;
        let paintable = {
            let obj = paintablesink.property::<glib::Object>("paintable");
            obj.dynamic_cast::<gdk4::Paintable>()
                .expect("gtk4paintablesink paintable must implement Paintable")
        };
        let video_sink_inner = match gst::ElementFactory::make("glsinkbin")
            .property("sink", &paintablesink)
            .build()
        {
            Ok(s) => s,
            Err(_) => paintablesink.clone(),
        };
        let display_sink_ref = video_sink_inner.clone();

        // Shared frame store for colour scope analysis.
        let latest_scope_frame: Arc<Mutex<Option<ScopeFrame>>> = Arc::new(Mutex::new(None));
        let latest_compositor_frame: Arc<Mutex<Option<ScopeFrame>>> = Arc::new(Mutex::new(None));
        let blend_overlays: Arc<Mutex<HashMap<usize, BlendOverlay>>> =
            Arc::new(Mutex::new(HashMap::new()));
        let adjustment_overlays: Arc<Mutex<Vec<AdjustmentOverlay>>> =
            Arc::new(Mutex::new(Vec::new()));
        let adjustment_timeline_pos: Arc<AtomicU64> = Arc::new(AtomicU64::new(0));
        // Flag set false when the scopes panel is hidden to skip the frame copy allocation.
        let scope_enabled: Arc<AtomicBool> = Arc::new(AtomicBool::new(true));
        let scope_frame_seq: Arc<AtomicU64> = Arc::new(AtomicU64::new(0));
        let compositor_frame_seq: Arc<AtomicU64> = Arc::new(AtomicU64::new(0));
        let compositor_capture_enabled: Arc<AtomicBool> = Arc::new(AtomicBool::new(false));

        // Build tee-based sink bin: display + scope appsink (320x180 RGBA).
        let (video_sink_bin, display_queue): (gst::Element, Option<gst::Element>) = (|| {
            let tee = gst::ElementFactory::make("tee")
                .property("allow-not-linked", true)
                .build()
                .ok()?;
            let q1 = gst::ElementFactory::make("queue")
                .property("max-size-buffers", 3u32)
                .property("max-size-bytes", 0u32)
                .property("max-size-time", 0u64)
                .build()
                .ok()?;
            let q2 = gst::ElementFactory::make("queue")
                .property_from_str("leaky", "downstream")
                .property("max-size-buffers", 1u32)
                .build()
                .ok()?;
            let scale = gst::ElementFactory::make("videoscale").build().ok()?;
            let conv = gst::ElementFactory::make("videoconvert").build().ok()?;
            let sink = AppSink::builder()
                .caps(
                    &gst::Caps::builder("video/x-raw")
                        .field("format", "RGBA")
                        .field("width", 320i32)
                        .field("height", 180i32)
                        .build(),
                )
                .max_buffers(1u32)
                .drop(true)
                .build();

            let lsf_preroll = latest_scope_frame.clone();
            let lsf_sample = latest_scope_frame.clone();
            let scope_en_preroll = scope_enabled.clone();
            let scope_en_sample = scope_enabled.clone();
            let seq_preroll = scope_frame_seq.clone();
            let seq_sample = scope_frame_seq.clone();
            sink.set_callbacks(
                gstreamer_app::AppSinkCallbacks::builder()
                    .new_preroll(move |appsink| {
                        if !scope_en_preroll.load(Ordering::Relaxed) {
                            // Scope disabled — drain the preroll buffer so
                            // the pipeline doesn't stall on a backed-up
                            // sink. Errors here are uninteresting because
                            // we're not consuming the sample anyway.
                            let _ = appsink.pull_preroll();
                            return Ok(gst::FlowSuccess::Ok);
                        }
                        if let Ok(sample) = appsink.pull_preroll() {
                            if let Some(buffer) = sample.buffer() {
                                let pts = buffer.pts().map(|p| p.nseconds()).unwrap_or(0);
                                if let Ok(map) = buffer.map_readable() {
                                    let hash = frame_hash_u64(map.as_slice());
                                    log::debug!(
                                        "scope new_preroll: size={} pts={} hash={:x}",
                                        map.size(),
                                        pts,
                                        hash
                                    );
                                    if map.size() >= 320 * 180 * 4 {
                                        if let Ok(mut frame) = lsf_preroll.lock() {
                                            *frame = Some(ScopeFrame {
                                                data: map.as_slice().to_vec(),
                                                width: 320,
                                                height: 180,
                                            });
                                            seq_preroll.fetch_add(1, Ordering::Relaxed);
                                        }
                                    }
                                }
                            }
                        }
                        Ok(gst::FlowSuccess::Ok)
                    })
                    .new_sample(move |appsink| {
                        if !scope_en_sample.load(Ordering::Relaxed) {
                            // Same drain-on-disable rationale as new_preroll
                            // above — discarding the sample on purpose.
                            let _ = appsink.pull_sample();
                            return Ok(gst::FlowSuccess::Ok);
                        }
                        if let Ok(sample) = appsink.pull_sample() {
                            if let Some(buffer) = sample.buffer() {
                                let pts = buffer.pts().map(|p| p.nseconds()).unwrap_or(0);
                                if let Ok(map) = buffer.map_readable() {
                                    let hash = frame_hash_u64(map.as_slice());
                                    log::debug!(
                                        "scope new_sample: size={} pts={} hash={:x}",
                                        map.size(),
                                        pts,
                                        hash
                                    );
                                    if map.size() >= 320 * 180 * 4 {
                                        if let Ok(mut frame) = lsf_sample.lock() {
                                            *frame = Some(ScopeFrame {
                                                data: map.as_slice().to_vec(),
                                                width: 320,
                                                height: 180,
                                            });
                                            seq_sample.fetch_add(1, Ordering::Relaxed);
                                        }
                                    }
                                }
                            }
                        }
                        Ok(gst::FlowSuccess::Ok)
                    })
                    .build(),
            );

            let bin = gst::Bin::new();
            bin.add_many([
                &tee,
                &q1,
                &video_sink_inner,
                &q2,
                &scale,
                &conv,
                sink.upcast_ref::<gst::Element>(),
            ])
            .ok()?;
            let tee_src0 = tee.request_pad_simple("src_%u")?;
            tee_src0.link(&q1.static_pad("sink")?).ok()?;
            gst::Element::link_many([&q1, &video_sink_inner]).ok()?;
            let tee_src1 = tee.request_pad_simple("src_%u")?;
            tee_src1.link(&q2.static_pad("sink")?).ok()?;
            gst::Element::link_many([&q2, &scale, &conv, sink.upcast_ref::<gst::Element>()])
                .ok()?;
            let ghost = gst::GhostPad::with_target(&tee.static_pad("sink")?).ok()?;
            bin.add_pad(&ghost).ok()?;
            Some((bin.upcast::<gst::Element>(), Some(q1)))
        })()
        .unwrap_or((video_sink_inner, None));

        // -- Compositor pipeline ------------------------------------------------
        let pipeline = gst::Pipeline::new();

        let compositor = gst::ElementFactory::make("compositor")
            .property_from_str("background", "black")
            .build()
            .map_err(|_| anyhow!("compositor element not available"))?;

        let comp_capsfilter = gst::ElementFactory::make("capsfilter")
            .property(
                "caps",
                &gst::Caps::builder("video/x-raw")
                    .field("format", "RGBA")
                    .field("width", 1920i32)
                    .field("height", 1080i32)
                    .field("pixel-aspect-ratio", gst::Fraction::new(1, 1))
                    .build(),
            )
            .build()
            .map_err(|_| anyhow!("capsfilter not available"))?;

        // videoconvert is required between compositor and glsinkbin to bridge
        // the GstVideoOverlayComposition meta feature that glsinkbin demands.
        let videoconvert_out = gst::ElementFactory::make("videoconvert")
            .build()
            .map_err(|_| anyhow!("videoconvert not available"))?;

        let preview_capsfilter = gst::ElementFactory::make("capsfilter")
            .property(
                "caps",
                &gst::Caps::builder("video/x-raw")
                    .field("format", "RGBA")
                    .field("width", 1920i32)
                    .field("height", 1080i32)
                    .field("pixel-aspect-ratio", gst::Fraction::new(1, 1))
                    .build(),
            )
            .build()
            .map_err(|_| anyhow!("capsfilter not available"))?;

        // Always-on black background so compositor has at least one input.
        let black_src = gst::ElementFactory::make("videotestsrc")
            .property_from_str("pattern", "black")
            .build()
            .map_err(|_| anyhow!("videotestsrc not available"))?;
        let black_caps = gst::ElementFactory::make("capsfilter")
            .property(
                "caps",
                &gst::Caps::builder("video/x-raw")
                    .field("format", "RGBA")
                    .field("width", 1920i32)
                    .field("height", 1080i32)
                    .field("framerate", gst::Fraction::new(30, 1))
                    .build(),
            )
            .build()?;

        // -- Audio mixer --------------------------------------------------------
        let audiomixer = gst::ElementFactory::make("audiomixer")
            .property("ignore-inactive-pads", true)
            .build()
            .map_err(|_| anyhow!("audiomixer element not available"))?;

        // Silent background so audiomixer always has input.
        // is-live=true makes the audiomixer aggregate in live mode (clock-paced),
        // so it won't stall waiting for unlinked pads from audio-less clips.
        let silence_src = gst::ElementFactory::make("audiotestsrc")
            .property_from_str("wave", "silence")
            .property("is-live", true)
            .build()
            .map_err(|_| anyhow!("audiotestsrc not available"))?;
        let silence_caps = gst::ElementFactory::make("capsfilter")
            .property("caps", &program_preview_audio_caps())
            .build()?;

        let audio_conv_out = gst::ElementFactory::make("audioconvert").build()?;
        // Project-level master gain element for the Loudness Radar
        // "Normalize to Target" workflow. Applied AFTER audiomixer so the
        // existing per-track meters see the pre-gain signal and the master
        // VU meter (via `level_element` below) sees the post-gain signal
        // — i.e. what the user will actually hear / export.
        let master_volume_element = gst::ElementFactory::make("volume").build().ok();
        if let Some(ref vol) = master_volume_element {
            vol.set_property("volume", 1.0_f64);
        }
        let level_element = gst::ElementFactory::make("level")
            .property("post-messages", true)
            .property("interval", 50_000_000u64)
            .build()
            .ok();
        let audio_sink = gst::ElementFactory::make("autoaudiosink").build()?;

        let videoscale_out = gst::ElementFactory::make("videoscale")
            .build()
            .map_err(|_| anyhow!("videoscale not available"))?;

        // -- Add everything to the pipeline ------------------------------------
        pipeline.add_many([
            &black_src,
            &black_caps,
            &compositor,
            &comp_capsfilter,
            &videoconvert_out,
            &videoscale_out,
            &preview_capsfilter,
            &video_sink_bin,
        ])?;
        pipeline.add_many([
            &silence_src,
            &silence_caps,
            &audiomixer,
            &audio_conv_out,
            &audio_sink,
        ])?;
        if let Some(ref mv) = master_volume_element {
            pipeline.add(mv)?;
        }
        if let Some(ref lv) = level_element {
            pipeline.add(lv)?;
        }

        // Link black source → compositor background pad
        gst::Element::link_many([&black_src, &black_caps])?;
        let comp_bg_pad = compositor
            .request_pad_simple("sink_%u")
            .ok_or_else(|| anyhow!("failed to request compositor background pad"))?;
        comp_bg_pad.set_property("zorder", 0u32);
        black_caps.static_pad("src").unwrap().link(&comp_bg_pad)?;

        // Permanent adjustment layer elements between compositor output and
        // display chain.  Properties are set to neutral values by default and
        // updated when an adjustment layer is active.  This avoids any pipeline
        // topology changes (which can deadlock GStreamer's state-change machinery).
        let adj_videoconvert = gst::ElementFactory::make("videoconvert")
            .build()
            .map_err(|_| anyhow!("videoconvert not available"))?;
        let adj_videobalance = gst::ElementFactory::make("videobalance")
            .build()
            .map_err(|_| anyhow!("videobalance not available"))?;
        // Neutral defaults (identity transform).
        adj_videobalance.set_property("brightness", 0.0_f64);
        adj_videobalance.set_property("contrast", 1.0_f64);
        adj_videobalance.set_property("saturation", 1.0_f64);
        adj_videobalance.set_property("hue", 0.0_f64);

        let adj_coloradj: Option<gst::Element> = None;
        let adj_3point: Option<gst::Element> = None;

        pipeline.add(&adj_videoconvert)?;
        pipeline.add(&adj_videobalance)?;

        // Link: compositor → caps → [adj_convert → adj_videobalance →] convert → scale → caps → display
        // The permanent videobalance handles brightness/contrast/saturation for
        // adjustment layers.  Temperature/tint/3-point and frei0r user effects
        // are applied on export only (frei0r elements cannot be safely added to
        // or removed from a live GStreamer pipeline without deadlocking).
        //
        // Try linking with the adjustment elements; fall back to direct link
        // if videobalance causes caps negotiation issues on this system.
        let adj_link_ok = gst::Element::link_many([
            &compositor,
            &comp_capsfilter,
            &adj_videoconvert,
            &adj_videobalance,
            &videoconvert_out,
            &videoscale_out,
            &preview_capsfilter,
            &video_sink_bin,
        ])
        .is_ok();
        if !adj_link_ok {
            log::warn!("ProgramPlayer: adjustment elements could not be linked — falling back");
            pipeline.remove(&adj_videoconvert).ok();
            pipeline.remove(&adj_videobalance).ok();
            gst::Element::link_many([
                &compositor,
                &comp_capsfilter,
                &videoconvert_out,
                &videoscale_out,
                &preview_capsfilter,
                &video_sink_bin,
            ])?;
        }
        // Blend-mode probe on compositor output.  For each frame, reads captured
        // overlay buffers from blend-mode clips and applies pixel-accurate blend
        // math against the compositor's normal "over" output (where the blend-mode
        // clips are hidden via alpha=0).  Must fire BEFORE the scope/capture probe
        // below so scopes see the blended result.
        {
            let blend_ov = blend_overlays.clone();
            if let Some(comp_src) = compositor.static_pad("src") {
                comp_src.add_probe(gst::PadProbeType::BUFFER, move |_pad, info| {
                    let overlays = match blend_ov.try_lock() {
                        Ok(g) => g,
                        Err(_) => return gst::PadProbeReturn::Ok,
                    };
                    if overlays.is_empty() {
                        return gst::PadProbeReturn::Ok;
                    }
                    if let Some(gst::PadProbeData::Buffer(ref mut buffer)) = info.data {
                        let buf = buffer.make_mut();
                        if let Ok(mut map) = buf.map_writable() {
                            let base = map.as_mut_slice();
                            let base_len = base.len();
                            // Sort overlays by zorder (lower first)
                            let mut sorted: Vec<&BlendOverlay> = overlays.values().collect();
                            sorted.sort_by_key(|o| o.zorder);
                            for ov in &sorted {
                                if ov.data.len() != base_len || ov.opacity <= 0.0 {
                                    continue;
                                }
                                let clip_opacity = ov.opacity as f32;
                                let ov_data = &ov.data;
                                for (bc, oc) in
                                    base.chunks_exact_mut(4).zip(ov_data.chunks_exact(4))
                                {
                                    let oa = (oc[3] as f32 / 255.0) * clip_opacity;
                                    if oa < 1.0 / 255.0 {
                                        continue;
                                    }
                                    let sr = oc[0] as f32 / 255.0;
                                    let sg = oc[1] as f32 / 255.0;
                                    let sb = oc[2] as f32 / 255.0;
                                    let br = bc[0] as f32 / 255.0;
                                    let bg = bc[1] as f32 / 255.0;
                                    let bb = bc[2] as f32 / 255.0;
                                    let (dr, dg, db) =
                                        blend_pixel(ov.blend_mode, sr, sg, sb, br, bg, bb);
                                    bc[0] = ((br + (dr - br) * oa).clamp(0.0, 1.0) * 255.0) as u8;
                                    bc[1] = ((bg + (dg - bg) * oa).clamp(0.0, 1.0) * 255.0) as u8;
                                    bc[2] = ((bb + (db - bb) * oa).clamp(0.0, 1.0) * 255.0) as u8;
                                }
                            }
                        }
                    }
                    gst::PadProbeReturn::Ok
                });
            }
        }

        // Adjustment-layer preview scopes are applied by a compositor-output
        // buffer probe so overlapping regions can stack without mutating live
        // pipeline topology.

        // Probe on compositor output: always increments cseq (cheap) but only
        // copies the full-res frame when compositor_capture_enabled is set (during
        // MCP frame export).  This avoids ~250 MB/s of unnecessary memcpy during
        // normal playback.
        if let Some(comp_src_pad) = comp_capsfilter.static_pad("src") {
            let adjustment_ov = adjustment_overlays.clone();
            let lcf = latest_compositor_frame.clone();
            let cseq = compositor_frame_seq.clone();
            let capture_en = compositor_capture_enabled.clone();
            let adjustment_caps_pad = comp_src_pad.clone();
            comp_src_pad.add_probe(gst::PadProbeType::BUFFER, move |_pad, info| {
                let overlays = match adjustment_ov.try_lock() {
                    Ok(guard) if !guard.is_empty() => guard.clone(),
                    _ => return gst::PadProbeReturn::Ok,
                };
                if let Some(gst::PadProbeData::Buffer(ref mut buffer)) = info.data {
                    let buf = buffer.make_mut();
                    if let Ok(mut map) = buf.map_writable() {
                        let (w, h) = adjustment_caps_pad
                            .current_caps()
                            .and_then(|caps| {
                                let s = caps.structure(0)?;
                                let w = s.get::<i32>("width").ok().unwrap_or(0).max(0) as usize;
                                let h = s.get::<i32>("height").ok().unwrap_or(0).max(0) as usize;
                                Some((w, h))
                            })
                            .unwrap_or((0, 0));
                        if w > 0 && h > 0 {
                            apply_adjustment_overlays_rgba(map.as_mut_slice(), w, h, &overlays);
                        }
                    }
                }
                gst::PadProbeReturn::Ok
            });
            let capture_caps_pad = comp_src_pad.clone();
            comp_src_pad.add_probe(gst::PadProbeType::BUFFER, move |_pad, info| {
                cseq.fetch_add(1, Ordering::Relaxed);
                if !capture_en.load(Ordering::Relaxed) {
                    return gst::PadProbeReturn::Ok;
                }
                if let Some(buffer) = info.buffer() {
                    if let Ok(map) = buffer.map_readable() {
                        let (w, h) = capture_caps_pad
                            .current_caps()
                            .and_then(|caps| {
                                let s = caps.structure(0)?;
                                let w = s.get::<i32>("width").ok().unwrap_or(0).max(0) as usize;
                                let h = s.get::<i32>("height").ok().unwrap_or(0).max(0) as usize;
                                Some((w, h))
                            })
                            .unwrap_or((0, 0));
                        if w > 0 && h > 0 {
                            let data = map.as_slice().to_vec();
                            if let Ok(mut frame) = lcf.lock() {
                                *frame = Some(ScopeFrame {
                                    data,
                                    width: w,
                                    height: h,
                                });
                            }
                        }
                    }
                }
                gst::PadProbeReturn::Ok
            });
        }

        // Link silence → audiomixer background pad
        gst::Element::link_many([&silence_src, &silence_caps])?;
        let amix_bg_pad = audiomixer
            .request_pad_simple("sink_%u")
            .ok_or_else(|| anyhow!("failed to request audiomixer background pad"))?;
        amix_bg_pad.set_property("volume", 1.0_f64);
        silence_caps.static_pad("src").unwrap().link(&amix_bg_pad)?;

        // Link audiomixer → [scaletempo] → audioconvert → [master_volume →] [level →] autoaudiosink
        // scaletempo preserves pitch during rate-based seeks (J/K/L shuttle).
        // master_volume applies the project-level Loudness Radar gain *before*
        // the level tap so the master VU meter reflects the post-gain mix.
        let scaletempo = gst::ElementFactory::make("scaletempo").build().ok();
        {
            let mut chain: Vec<&gst::Element> = vec![&audiomixer];
            if let Some(ref st) = scaletempo {
                pipeline.add(st)?;
                chain.push(st);
            } else {
                log::warn!("scaletempo element not available — J/K/L shuttle will shift pitch");
            }
            chain.push(&audio_conv_out);
            if let Some(ref mv) = master_volume_element {
                chain.push(mv);
            }
            if let Some(ref lv) = level_element {
                chain.push(lv);
            }
            chain.push(&audio_sink);
            gst::Element::link_many(&chain)?;
        }

        // -- Audio-only pipeline (playbin, unchanged) --------------------------
        let fakevideo = gst::ElementFactory::make("fakesink")
            .build()
            .unwrap_or_else(|_| gst::ElementFactory::make("autovideosink").build().unwrap());
        let audio_pipeline = gst::ElementFactory::make("playbin")
            .property("video-sink", &fakevideo)
            .build()
            .unwrap_or_else(|_| gst::ElementFactory::make("playbin").build().unwrap());
        let level_element_audio = gst::ElementFactory::make("level")
            .property("post-messages", true)
            .property("interval", 50_000_000u64)
            .build()
            .ok();
        // Use a dedicated GStreamer volume element for audio-only clip volume.
        // playbin's own "volume" property delegates to pulsesink's StreamVolume
        // interface, which under PulseAudio/PipeWire flat-volume mode can also
        // affect the main compositor pipeline's audio level.
        let audio_volume_element = gst::ElementFactory::make("volume").build().ok();
        let audio_panorama_element = gst::ElementFactory::make("audiopanorama").build().ok();
        if let Some(ref pan) = audio_panorama_element {
            pan.set_property("panorama", 0.0_f32);
        }
        let audio_eq_element = gst::ElementFactory::make("equalizer-nbands")
            .property("num-bands", 3u32)
            .build()
            .ok();
        if let Some(ref eq) = audio_eq_element {
            // Set default band frequencies.
            let defaults = crate::model::clip::default_eq_bands();
            for (i, b) in defaults.iter().enumerate() {
                eq_set_band(eq, i as u32, b.freq, b.gain, b.q);
            }
        }
        let mut audio_filters: Vec<gst::Element> = Vec::new();
        if let Some(ref vol) = audio_volume_element {
            audio_filters.push(vol.clone());
        }
        if let Some(ref eq) = audio_eq_element {
            audio_filters.push(eq.clone());
        }
        if let Some(ref pan) = audio_panorama_element {
            audio_filters.push(pan.clone());
        }
        if let Some(ref lv) = level_element_audio {
            audio_filters.push(lv.clone());
        }
        match audio_filters.len() {
            0 => {}
            1 => {
                audio_pipeline.set_property("audio-filter", &audio_filters[0]);
            }
            _ => {
                let bin = gst::Bin::builder().name("audio-filter-bin").build();
                for elem in &audio_filters {
                    bin.add(elem).unwrap();
                }
                for pair in audio_filters.windows(2) {
                    pair[0].link(&pair[1]).unwrap();
                }
                if let (Some(first), Some(last)) = (audio_filters.first(), audio_filters.last()) {
                    if let (Some(sink_pad), Some(src_pad)) =
                        (first.static_pad("sink"), last.static_pad("src"))
                    {
                        bin.add_pad(&gst::GhostPad::with_target(&sink_pad).unwrap())
                            .unwrap();
                        bin.add_pad(&gst::GhostPad::with_target(&src_pad).unwrap())
                            .unwrap();
                        audio_pipeline.set_property("audio-filter", &bin.upcast::<gst::Element>());
                    }
                }
            }
        }

        // Dummy second paintable (API compat — window.rs expects two).
        let paintablesink2 = gst::ElementFactory::make("gtk4paintablesink")
            .build()
            .map_err(|_| anyhow!("gtk4paintablesink not available for dummy"))?;
        let paintable2 = {
            let obj = paintablesink2.property::<glib::Object>("paintable");
            obj.dynamic_cast::<gdk4::Paintable>()
                .expect("paintable must implement Paintable")
        };
        let (prerender_result_tx, prerender_result_rx) = mpsc::channel::<PrerenderJobResult>();
        let prerender_cache_root = default_prerender_cache_root();
        let _ = std::fs::create_dir_all(&prerender_cache_root);

        Ok((
            Self {
                pipeline,
                compositor,
                audiomixer,
                state: PlayerState::Stopped,
                clips: Vec::new(),
                audio_clips: Vec::new(),
                audio_pipeline,
                audio_current_source: None,
                audio_multi_active: Vec::new(),
                audio_multi_pipeline: None,
                audio_multi_pads: HashMap::new(),
                audio_multi_pan_elems: HashMap::new(),
                audio_multi_decoders: HashMap::new(),
                reverse_video_ducked_clip_idx: None,
                slots: Vec::new(),
                timeline_pos_ns: 0,
                timeline_dur_ns: 0,
                base_timeline_ns: 0,
                project_width: 1920,
                project_height: 1080,
                playback_priority: PlaybackPriority::default(),
                proxy_enabled: false,
                preview_luts: false,
                proxy_scale_divisor: 2,
                experimental_preview_optimizations: false,
                realtime_preview: true,
                background_prerender: false,
                prerender_preset: PrerenderEncodingPreset::default(),
                prerender_crf: DEFAULT_PRERENDER_CRF,
                crossfade_enabled: false,
                crossfade_curve: CrossfadeCurve::default(),
                crossfade_duration_ns: 200_000_000,
                duck_enabled: false,
                duck_gain: 1.0,
                master_muted: false,
                proxy_paths: HashMap::new(),
                proxy_fallback_warned_keys: HashSet::new(),
                animated_svg_paths: HashMap::new(),
                bg_removal_paths: HashMap::new(),
                voice_enhance_paths: HashMap::new(),
                frame_interp_paths: HashMap::new(),
                audio_stream_probe_cache: HashMap::new(),
                level_element,
                master_volume_element,
                level_element_audio,
                audio_volume_element,
                audio_eq_element,
                audio_panorama_element,
                audio_peak_db: [-60.0, -60.0],
                audio_track_peak_db: Vec::new(),
                jkl_rate: 0.0,
                latest_scope_frame,
                latest_compositor_frame,
                blend_overlays,
                adjustment_overlays,
                adjustment_timeline_pos,
                adj_videobalance,
                adj_coloradj,
                adj_3point,
                scope_enabled,
                scope_frame_seq,
                compositor_frame_seq,
                compositor_capture_enabled,
                current_idx: None,
                _video_sink_bin: video_sink_bin,
                comp_capsfilter,
                black_capsfilter: black_caps,
                preview_capsfilter,
                audio_sink,
                display_sink: display_sink_ref,
                background_src: black_src,
                background_compositor_pad: comp_bg_pad,
                preview_divisor: 1,
                display_queue,
                playback_drop_late_active: false,
                slot_queue_drop_late_active: false,
                transform_live: false,
                play_start: None,
                prewarmed_boundary_ns: None,
                rebuild_history_ms: [0; 8],
                rebuild_history_cursor: 0,
                rebuild_history_count: 0,
                prepreroll_sidecars: Vec::new(),
                prerender_segments: HashMap::new(),
                prerender_pending: HashSet::new(),
                prerender_pending_priority: HashMap::new(),
                prerender_failed: HashSet::new(),
                prerender_superseded: HashSet::new(),
                prerender_boundary_latest: HashMap::new(),
                prerender_result_rx,
                prerender_result_tx,
                prerender_cache_root,
                prerender_cache_persistent: false,
                prerender_runtime_files: HashSet::new(),
                prerender_generation: 0,
                prerender_active_clips: None,
                current_prerender_segment_key: None,
                prerender_total_requested: 0,
                prerender_cache_hits: 0,
                prerender_cache_misses: 0,
                transition_prerender_metrics: HashMap::new(),
                last_idle_prerender_scan_at: None,
                pending_prerender_promote: false,
                // Default 24 fps ≈ 41_666_666 ns per frame
                frame_duration_ns: 1_000_000_000 / 24,
                frame_rate_num: 24,
                frame_rate_den: 1,
                last_seeked_frame_pos: None,
                last_seek_instant: None,
                short_frame_cache: ShortFrameCache::default(),
                pending_short_frame_capture: None,
                last_boundary_rebuild_clips: Vec::new(),
                last_boundary_rebuild_at: None,
                last_not_negotiated_recover_at: None,
                lut_cache: HashMap::new(),
            },
            paintable,
            paintable2,
        ))
    }

    // ── Simple getters / setters ───────────────────────────────────────────

    pub fn set_playback_priority(&mut self, playback_priority: PlaybackPriority) {
        self.playback_priority = playback_priority;
    }

    pub fn set_proxy_enabled(&mut self, enabled: bool) {
        if self.proxy_enabled != enabled {
            self.invalidate_short_frame_cache("proxy-mode-changed");
            self.proxy_fallback_warned_keys.clear();
        }
        self.proxy_enabled = enabled;
        self.prewarmed_boundary_ns = None;
    }

    pub fn set_preview_luts(&mut self, enabled: bool) {
        if self.preview_luts != enabled {
            self.invalidate_short_frame_cache("preview-luts-changed");
        }
        self.preview_luts = enabled;
        self.prewarmed_boundary_ns = None;
    }

    pub fn set_proxy_scale_divisor(&mut self, divisor: u32) {
        let new_divisor = divisor.max(1);
        if self.proxy_scale_divisor != new_divisor {
            self.invalidate_short_frame_cache("proxy-scale-divisor-changed");
        }
        self.proxy_scale_divisor = new_divisor;
    }

    pub fn set_experimental_preview_optimizations(&mut self, enabled: bool) {
        self.experimental_preview_optimizations = enabled;
    }

    pub fn set_realtime_preview(&mut self, enabled: bool) {
        self.realtime_preview = enabled;
    }

    pub fn set_background_prerender(&mut self, enabled: bool) {
        self.background_prerender = enabled;
        if !enabled {
            self.prewarmed_boundary_ns = None;
            if !self.realtime_preview {
                self.teardown_prepreroll_sidecars();
            }
            self.cleanup_background_prerender_cache(true);
        } else if self.prerender_cache_persistent {
            self.prune_prerender_cache_root_files();
        }
    }

    pub fn set_prerender_quality(&mut self, preset: PrerenderEncodingPreset, crf: u32) {
        let crf = clamp_prerender_crf(crf);
        if self.prerender_preset == preset && self.prerender_crf == crf {
            return;
        }
        self.prerender_preset = preset;
        self.prerender_crf = crf;
        self.prewarmed_boundary_ns = None;
    }

    pub fn set_audio_crossfade_preview(
        &mut self,
        enabled: bool,
        curve: CrossfadeCurve,
        duration_ns: u64,
    ) {
        self.crossfade_enabled = enabled;
        self.crossfade_curve = curve;
        self.crossfade_duration_ns = duration_ns;
        self.sync_preview_audio_levels(self.timeline_pos_ns);
    }

    pub fn set_duck_settings(&mut self, enabled: bool, amount_db: f64) {
        self.duck_enabled = enabled;
        self.duck_gain = if enabled {
            10.0_f64.powf(amount_db.min(0.0) / 20.0)
        } else {
            1.0
        };
    }

    fn cleanup_background_prerender_cache(&mut self, purge_cache_files: bool) {
        self.prerender_generation = self.prerender_generation.wrapping_add(1);
        if purge_cache_files {
            self.remove_all_prerender_cache_files_from_root();
        }
        self.prerender_runtime_files.clear();
        self.prerender_segments.clear();
        self.prerender_pending.clear();
        self.prerender_pending_priority.clear();
        self.prerender_failed.clear();
        self.prerender_superseded.clear();
        self.prerender_boundary_latest.clear();
        self.prerender_active_clips = None;
        self.current_prerender_segment_key = None;
        self.prerender_total_requested = 0;
        self.prerender_cache_hits = 0;
        self.prerender_cache_misses = 0;
        self.transition_prerender_metrics.clear();
        self.last_idle_prerender_scan_at = None;
        self.pending_prerender_promote = false;
        while self.prerender_result_rx.try_recv().is_ok() {}
    }

    fn should_preserve_prerender_cache_files(&self) -> bool {
        self.background_prerender && self.prerender_cache_persistent
    }

    fn remove_all_prerender_cache_files_from_root(&self) {
        Self::clear_prerender_cache_root(&self.prerender_cache_root);
    }

    fn forget_prerender_segment_path(&mut self, path: &Path) {
        let path_str = path.to_string_lossy().to_string();
        self.prerender_runtime_files.remove(&path_str);
        self.prerender_segments
            .retain(|_, seg| seg.path != path_str);
        if let Some(current_key) = self.current_prerender_segment_key.as_ref() {
            if !self.prerender_segments.contains_key(current_key) {
                self.current_prerender_segment_key = None;
            }
        }
    }

    fn load_prerender_manifest_for_path(path: &Path) -> Option<PrerenderSegmentManifest> {
        let manifest_path = prerender_manifest_path(path);
        let payload = std::fs::read(manifest_path).ok()?;
        serde_json::from_slice(&payload).ok()
    }

    fn write_prerender_manifest_for_path(
        path: &Path,
        manifest: &PrerenderSegmentManifest,
    ) -> Result<()> {
        let manifest_path = prerender_manifest_path(path);
        let payload = serde_json::to_vec_pretty(manifest)?;
        std::fs::write(manifest_path, payload)?;
        Ok(())
    }

    fn prerender_manifest_inputs_are_fresh(inputs: &[PrerenderManifestInput]) -> bool {
        inputs.iter().all(|input| {
            prerender_source_signature_for_path(Path::new(&input.path)) == Some(input.signature)
        })
    }

    fn clear_prerender_cache_root(root: &Path) {
        if let Ok(entries) = std::fs::read_dir(root) {
            for entry in entries.flatten() {
                let path = entry.path();
                let name = path.file_name().and_then(|s| s.to_str()).unwrap_or("");
                if path.extension().and_then(|s| s.to_str()) == Some("mp4")
                    || name.ends_with(".manifest.json")
                    || name.ends_with(".partial")
                {
                    let _ = std::fs::remove_file(path);
                }
            }
        }
    }

    fn prune_prerender_cache_root_files(&mut self) {
        let mut kept: Vec<(PathBuf, SystemTime)> = Vec::new();
        let mut manifest_paths: HashSet<PathBuf> = HashSet::new();
        if let Ok(entries) = std::fs::read_dir(&self.prerender_cache_root) {
            for entry in entries.flatten() {
                let path = entry.path();
                let name = path.file_name().and_then(|s| s.to_str()).unwrap_or("");
                if name.ends_with(".partial") {
                    let _ = std::fs::remove_file(&path);
                    continue;
                }
                if name.ends_with(".manifest.json") {
                    manifest_paths.insert(path);
                    continue;
                }
                if path.extension().and_then(|s| s.to_str()) != Some("mp4") {
                    continue;
                }
                let managed_segment = is_managed_prerender_segment_path(&path);
                let Some(manifest) = Self::load_prerender_manifest_for_path(&path) else {
                    if managed_segment {
                        remove_prerender_segment_files(&path);
                        self.forget_prerender_segment_path(&path);
                    }
                    continue;
                };
                if !Self::prerender_manifest_inputs_are_fresh(&manifest.inputs) {
                    if managed_segment {
                        remove_prerender_segment_files(&path);
                        self.forget_prerender_segment_path(&path);
                    }
                    continue;
                }
                let modified = entry
                    .metadata()
                    .ok()
                    .and_then(|metadata| metadata.modified().ok())
                    .unwrap_or(UNIX_EPOCH);
                kept.push((path, modified));
            }
        }

        let kept_manifest_paths: HashSet<PathBuf> = kept
            .iter()
            .map(|(path, _)| prerender_manifest_path(path))
            .collect();
        for manifest_path in manifest_paths {
            if !kept_manifest_paths.contains(&manifest_path) {
                let _ = std::fs::remove_file(manifest_path);
            }
        }

        if kept.len() <= MAX_READY_PRERENDER_SEGMENTS {
            return;
        }

        let protected_paths: HashSet<String> = self
            .prerender_segments
            .values()
            .map(|segment| segment.path.clone())
            .collect();
        kept.sort_by_key(|(_, modified)| *modified);
        while kept.len() > MAX_READY_PRERENDER_SEGMENTS {
            let victim_index = kept
                .iter()
                .position(|(path, _)| !protected_paths.contains(path.to_string_lossy().as_ref()));
            let Some(victim_index) = victim_index else {
                break;
            };
            let (victim_path, _) = kept.remove(victim_index);
            remove_prerender_segment_files(&victim_path);
            self.forget_prerender_segment_path(&victim_path);
        }
    }

    pub fn background_prerender_progress(&self) -> BackgroundPrerenderProgress {
        let total = self.prerender_total_requested;
        let pending = self.prerender_pending.len();
        let completed = total.saturating_sub(pending).min(total);
        BackgroundPrerenderProgress {
            total,
            completed,
            in_flight: self.background_prerender && pending > 0,
        }
    }

    /// Enable or disable scope frame capture. When disabled (scopes panel hidden),
    /// the appsink callback drops frames without allocating, saving ~7MB/s at 30fps.
    pub fn set_scope_enabled(&self, enabled: bool) {
        self.scope_enabled.store(enabled, Ordering::Relaxed);
    }

    pub fn update_proxy_paths(&mut self, paths: HashMap<String, String>) {
        if self.proxy_paths != paths {
            self.proxy_fallback_warned_keys
                .retain(|key| !paths.contains_key(key));
            self.proxy_paths = paths;
            self.prewarmed_boundary_ns = None;
            self.invalidate_short_frame_cache("proxy-paths-updated");
        }
    }

    pub fn update_animated_svg_paths(&mut self, paths: HashMap<String, String>) {
        if self.animated_svg_paths != paths {
            self.animated_svg_paths = paths;
            self.prewarmed_boundary_ns = None;
            self.invalidate_short_frame_cache("animated-svg-paths-updated");
        }
    }

    pub fn update_bg_removal_paths(&mut self, paths: HashMap<String, String>) {
        if self.bg_removal_paths != paths {
            self.bg_removal_paths = paths;
            self.prewarmed_boundary_ns = None;
            self.invalidate_short_frame_cache("bg-removal-paths-updated");
        }
    }

    /// Hand off a freshly snapshotted voice-enhance cache key → output
    /// path map. The Program Monitor swaps in the prerendered file at
    /// `resolve_source_path_for_clip` time for any clip whose
    /// `(source_path, voice_enhance_strength)` matches a ready entry.
    pub fn update_voice_enhance_paths(&mut self, paths: HashMap<String, String>) {
        if self.voice_enhance_paths != paths {
            self.voice_enhance_paths = paths;
            self.prewarmed_boundary_ns = None;
            self.invalidate_short_frame_cache("voice-enhance-paths-updated");
        }
    }

    /// Hand off a freshly snapshotted clip-id → AI frame-interpolation
    /// sidecar map. The Program Monitor will swap in the sidecar at decoder
    /// build time for any clip in the map whose source path actually exists.
    pub fn update_frame_interp_paths(&mut self, paths: HashMap<String, String>) {
        if self.frame_interp_paths != paths {
            self.frame_interp_paths = paths;
            self.prewarmed_boundary_ns = None;
            self.invalidate_short_frame_cache("frame-interp-paths-updated");
        }
    }

    pub fn set_project_dimensions(&mut self, width: u32, height: u32) {
        if self.project_width != width || self.project_height != height {
            self.invalidate_short_frame_cache("project-dimensions-changed");
        }
        self.project_width = width;
        self.project_height = height;
        self.apply_compositor_caps();
    }

    /// Update the frame duration from the project frame rate.
    /// Called alongside `set_project_dimensions` when the project changes.
    pub fn set_frame_rate(&mut self, numerator: u32, denominator: u32) {
        if numerator > 0 && denominator > 0 {
            // frame_duration = 1e9 * denominator / numerator (nanoseconds)
            let frame_duration_ns = (1_000_000_000u64 * denominator as u64) / numerator as u64;
            if self.frame_duration_ns != frame_duration_ns {
                self.invalidate_short_frame_cache("frame-rate-changed");
            }
            self.frame_duration_ns = frame_duration_ns;
            self.frame_rate_num = numerator;
            self.frame_rate_den = denominator;
        }
    }

    pub fn set_prerender_project_path(
        &mut self,
        project_file_path: Option<&str>,
        persist_next_to_project_file: bool,
    ) {
        let (next_root, persistent) =
            prerender_cache_root_for_project_path(project_file_path, persist_next_to_project_file);
        if self.prerender_cache_root == next_root && self.prerender_cache_persistent == persistent {
            if persistent {
                self.prune_prerender_cache_root_files();
            }
            return;
        }

        let purge_old_root = !persistent || !self.should_preserve_prerender_cache_files();
        self.cleanup_background_prerender_cache(purge_old_root);
        self.prerender_cache_root = next_root;
        self.prerender_cache_persistent = persistent;
        let _ = std::fs::create_dir_all(&self.prerender_cache_root);
        cleanup_old_prerender_cache_versions(&self.prerender_cache_root);
        // Saved-project roots must keep compatible prerenders even when this path is
        // attached before the background-prerender preference has been restored.
        if persistent {
            self.prune_prerender_cache_root_files();
        } else {
            self.remove_all_prerender_cache_files_from_root();
        }
    }

    /// Set preview quality (preview processing/output divisor). Takes effect immediately.
    pub fn set_preview_quality(&mut self, divisor: u32) {
        let new_divisor = divisor.max(1);
        if self.preview_divisor == new_divisor {
            return;
        }
        self.invalidate_short_frame_cache("preview-quality-changed");
        self.preview_divisor = new_divisor;
        self.apply_compositor_caps();
        if !self.clips.is_empty() {
            // Force a clean renegotiation so monitor output reflects new caps
            // immediately without stale/cropped framing.
            self.rebuild_pipeline_at(self.timeline_pos_ns);
        }
    }

    /// Re-apply compositor and black-source capsfilter caps from project
    /// dimensions and preview quality divisor.
    fn apply_compositor_caps(&self) {
        let (proc_w, proc_h) = self.preview_processing_dimensions();
        let comp_w = proc_w as i32;
        let comp_h = proc_h as i32;
        let caps = gst::Caps::builder("video/x-raw")
            .field("format", "RGBA")
            .field("width", comp_w)
            .field("height", comp_h)
            .field("pixel-aspect-ratio", gst::Fraction::new(1, 1))
            .build();
        self.comp_capsfilter.set_property("caps", &caps);
        let bg_caps = gst::Caps::builder("video/x-raw")
            .field("format", "RGBA")
            .field("width", comp_w)
            .field("height", comp_h)
            .field("framerate", gst::Fraction::new(30, 1))
            .build();
        self.black_capsfilter.set_property("caps", &bg_caps);
        let out_w = (self.project_width / self.preview_divisor).max(2) as i32;
        let out_h = (self.project_height / self.preview_divisor).max(2) as i32;
        let out_caps = gst::Caps::builder("video/x-raw")
            .field("format", "RGBA")
            .field("width", out_w)
            .field("height", out_h)
            .field("pixel-aspect-ratio", gst::Fraction::new(1, 1))
            .build();
        self.preview_capsfilter.set_property("caps", &out_caps);
    }

    fn preview_processing_dimensions(&self) -> (u32, u32) {
        let divisor = self.preview_divisor.max(1);
        (
            (self.project_width / divisor).max(2),
            (self.project_height / divisor).max(2),
        )
    }

    fn quantize_frame_position_ns(&self, timeline_pos_ns: u64) -> u64 {
        if self.frame_duration_ns > 0 {
            (timeline_pos_ns / self.frame_duration_ns) * self.frame_duration_ns
        } else {
            timeline_pos_ns
        }
    }

    fn short_frame_cache_signature_for_frame(&self, frame_pos_ns: u64) -> u64 {
        let active = self.clips_active_at(frame_pos_ns);
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        "short-frame-cache-v1".hash(&mut hasher);
        self.prerender_signature_for_active(&active)
            .hash(&mut hasher);
        self.preview_luts.hash(&mut hasher);
        self.frame_duration_ns.hash(&mut hasher);
        for idx in active {
            if let Some(c) = self.clips.get(idx) {
                c.scale_at_timeline_ns(frame_pos_ns)
                    .to_bits()
                    .hash(&mut hasher);
                c.opacity_at_timeline_ns(frame_pos_ns)
                    .to_bits()
                    .hash(&mut hasher);
                c.position_x_at_timeline_ns(frame_pos_ns)
                    .to_bits()
                    .hash(&mut hasher);
                c.position_y_at_timeline_ns(frame_pos_ns)
                    .to_bits()
                    .hash(&mut hasher);
                c.volume_at_timeline_ns(frame_pos_ns)
                    .to_bits()
                    .hash(&mut hasher);
                c.brightness_at_timeline_ns(frame_pos_ns)
                    .to_bits()
                    .hash(&mut hasher);
                c.contrast_at_timeline_ns(frame_pos_ns)
                    .to_bits()
                    .hash(&mut hasher);
                c.saturation_at_timeline_ns(frame_pos_ns)
                    .to_bits()
                    .hash(&mut hasher);
                c.temperature_at_timeline_ns(frame_pos_ns)
                    .to_bits()
                    .hash(&mut hasher);
                c.tint_at_timeline_ns(frame_pos_ns)
                    .to_bits()
                    .hash(&mut hasher);
                Self::hash_transition_cache_state(
                    &mut hasher,
                    &c.transition_after,
                    c.transition_after_ns,
                    c.transition_alignment,
                );
                c.title_text.hash(&mut hasher);
                c.title_font.hash(&mut hasher);
                c.title_color.hash(&mut hasher);
                c.title_x.to_bits().hash(&mut hasher);
                c.title_y.to_bits().hash(&mut hasher);
                c.bg_removal_enabled.hash(&mut hasher);
                c.bg_removal_threshold.to_bits().hash(&mut hasher);
                for fe in &c.frei0r_effects {
                    fe.plugin_name.hash(&mut hasher);
                    fe.enabled.hash(&mut hasher);
                    for (k, v) in &fe.params {
                        k.hash(&mut hasher);
                        v.to_bits().hash(&mut hasher);
                    }
                }
            }
        }
        hasher.finish()
    }

    fn short_frame_cache_wait_budget_ms(&self) -> u64 {
        if self.slots.len() >= 3 {
            600
        } else {
            240
        }
    }

    fn invalidate_short_frame_cache(&mut self, reason: &str) {
        let had_entries = self.short_frame_cache.clear();
        let had_pending = self.pending_short_frame_capture.take().is_some();
        if had_entries || had_pending {
            log::debug!(
                "short_frame_cache: invalidated reason={} invalidations={} hits={} misses={}",
                reason,
                self.short_frame_cache.invalidations,
                self.short_frame_cache.hits,
                self.short_frame_cache.misses
            );
        }
    }

    fn cache_scope_frame_now(
        &mut self,
        frame_pos_ns: u64,
        signature: u64,
        min_scope_seq: u64,
        reason: &str,
    ) {
        let scope_seq_now = self.scope_frame_seq.load(Ordering::Relaxed);
        if scope_seq_now <= min_scope_seq {
            return;
        }
        let Some(frame) = self.try_pull_scope_frame() else {
            return;
        };
        self.short_frame_cache.store_current(CachedPlayheadFrame {
            frame_pos_ns,
            signature,
            scope_seq: scope_seq_now,
            frame,
        });
        log::debug!(
            "short_frame_cache: stored frame={} reason={} scope_seq={} hits={} misses={}",
            frame_pos_ns,
            reason,
            scope_seq_now,
            self.short_frame_cache.hits,
            self.short_frame_cache.misses
        );
    }

    fn queue_or_store_short_frame_cache(
        &mut self,
        frame_pos_ns: u64,
        signature: u64,
        min_scope_seq: u64,
        pending_async: bool,
        reason: &str,
    ) {
        if pending_async {
            self.pending_short_frame_capture = Some(PendingShortFrameCapture {
                frame_pos_ns,
                signature,
                min_scope_seq,
            });
            return;
        }
        self.pending_short_frame_capture = None;
        self.cache_scope_frame_now(frame_pos_ns, signature, min_scope_seq, reason);
    }

    fn consume_pending_short_frame_capture(&mut self) {
        let Some(pending) = self.pending_short_frame_capture else {
            return;
        };
        let scope_seq_now = self.scope_frame_seq.load(Ordering::Relaxed);
        if scope_seq_now <= pending.min_scope_seq {
            return;
        }
        self.pending_short_frame_capture = None;
        self.cache_scope_frame_now(
            pending.frame_pos_ns,
            pending.signature,
            pending.min_scope_seq,
            "async-pulse-complete",
        );
    }

    /// Returns (opacity_a, opacity_b) for the two program monitor pictures.
    /// With the compositor approach all layering is internal; picture_b is unused.
    pub fn transition_opacities(&self) -> (f64, f64) {
        (1.0, 0.0)
    }

    pub fn try_pull_scope_frame(&self) -> Option<ScopeFrame> {
        if let Ok(frame) = self.latest_scope_frame.lock() {
            if let Some(fresh) = frame.clone() {
                return Some(fresh);
            }
        }
        self.short_frame_cache
            .current
            .as_ref()
            .map(|cached| cached.frame.clone())
    }

    /// Export the current Program Monitor frame at project resolution as a PPM image (P6).
    /// If playback is active, capture runs while paused and playback resumes afterwards.
    pub fn export_displayed_frame_ppm(&mut self, path: &str) -> Result<()> {
        let was_playing = self.state == PlayerState::Playing;
        if was_playing {
            self.pause();
        }
        let start_scope_seq = self.scope_frame_seq.load(Ordering::Relaxed);
        let start_comp_seq = self.compositor_frame_seq.load(Ordering::Relaxed);
        let was_scope_enabled = self.scope_enabled.swap(true, Ordering::Relaxed);
        let was_comp_capture = self
            .compositor_capture_enabled
            .swap(true, Ordering::Relaxed);
        log::info!(
            "export_displayed_frame: start_scope_seq={} start_comp_seq={} slots={} current_idx={:?} state={:?} timeline_ns={} was_playing={}",
            start_scope_seq,
            start_comp_seq,
            self.slots.len(),
            self.current_idx,
            self.state,
            self.timeline_pos_ns,
            was_playing
        );
        let result = (|| -> Result<()> {
            self.reseek_all_slots_for_export();
            let deadline = Instant::now() + Duration::from_millis(1200);
            while self.compositor_frame_seq.load(Ordering::Relaxed) <= start_comp_seq
                && Instant::now() < deadline
            {
                std::thread::sleep(Duration::from_millis(10));
            }
            let comp_now = self.compositor_frame_seq.load(Ordering::Relaxed);
            if comp_now <= start_comp_seq {
                return Err(anyhow!("no fresh project-resolution frame available yet"));
            }
            self.write_compositor_frame_ppm(path)
        })();
        self.scope_enabled
            .store(was_scope_enabled, Ordering::Relaxed);
        self.compositor_capture_enabled
            .store(was_comp_capture, Ordering::Relaxed);
        if was_playing {
            self.play();
        }
        result
    }

    /// Phase 1 of async export: enable scope capture, trigger re-seek on all
    /// decoder slots + background, and return (start_seq, was_scope_enabled, left_playing).
    /// The caller should release the borrow on ProgramPlayer after this call,
    /// let the GTK main loop run (so gtk4paintablesink can complete its state
    /// transition), and poll `scope_frame_seq` for a change.
    /// When `left_playing` is true, the caller must call `set_paused_after_export()`
    /// after the export completes.
    #[allow(dead_code)]
    pub fn prepare_export(&self) -> (u64, bool, bool) {
        let start_seq = self.scope_frame_seq.load(Ordering::Relaxed);
        let was_enabled = self.scope_enabled.swap(true, Ordering::Relaxed);
        self.compositor_capture_enabled
            .store(true, Ordering::Relaxed);
        log::info!(
            "prepare_export: start_seq={} slots={} current_idx={:?} state={:?} timeline_ns={}",
            start_seq,
            self.slots.len(),
            self.current_idx,
            self.state,
            self.timeline_pos_ns
        );
        let left_playing = if self.current_idx.is_some() && self.state != PlayerState::Playing {
            self.reseek_all_slots_for_export()
        } else {
            false
        };
        (start_seq, was_enabled, left_playing)
    }

    /// Monotonic counter for scope frame captures.  Clone it for polling from
    /// an async glib timeout without holding the ProgramPlayer borrow.
    #[allow(dead_code)]
    pub fn scope_frame_seq_arc(&self) -> Arc<AtomicU64> {
        self.scope_frame_seq.clone()
    }

    /// Monotonic counter for compositor-src frame captures.
    #[allow(dead_code)]
    pub fn compositor_frame_seq_arc(&self) -> Arc<AtomicU64> {
        self.compositor_frame_seq.clone()
    }

    /// Phase 2 of async export: write the latest scope frame to disk as PPM
    /// and optionally restore scope_enabled.
    #[allow(dead_code)]
    pub fn finish_export(
        &self,
        path: &str,
        was_scope_enabled: bool,
        start_scope_seq: u64,
        start_compositor_seq: u64,
    ) -> Result<()> {
        let scope_now = self.scope_frame_seq.load(Ordering::Relaxed);
        let comp_now = self.compositor_frame_seq.load(Ordering::Relaxed);
        let result = if scope_now > start_scope_seq {
            log::info!(
                "finish_export: source=scope scope_seq {}->{} comp_seq {}->{}",
                start_scope_seq,
                scope_now,
                start_compositor_seq,
                comp_now
            );
            self.write_scope_frame_ppm(path)
        } else if comp_now > start_compositor_seq {
            log::info!(
                "finish_export: source=compositor scope_seq {}->{} comp_seq {}->{}",
                start_scope_seq,
                scope_now,
                start_compositor_seq,
                comp_now
            );
            self.write_compositor_frame_ppm(path)
        } else {
            log::warn!(
                "finish_export: no seq advance scope_seq {}->{} comp_seq {}->{}; falling back to scope",
                start_scope_seq,
                scope_now,
                start_compositor_seq,
                comp_now
            );
            self.write_scope_frame_ppm(path)
        };
        if !was_scope_enabled {
            self.scope_enabled.store(false, Ordering::Relaxed);
        }
        self.compositor_capture_enabled
            .store(false, Ordering::Relaxed);
        result
    }

    /// Write the latest scope frame to a PPM (P6) file.
    #[allow(dead_code)]
    fn write_scope_frame_ppm(&self, path: &str) -> Result<()> {
        let frame = self
            .try_pull_scope_frame()
            .ok_or_else(|| anyhow!("no displayed frame available yet"))?;
        self.write_frame_ppm(path, &frame)
    }

    fn write_compositor_frame_ppm(&self, path: &str) -> Result<()> {
        let frame = self
            .latest_compositor_frame
            .lock()
            .ok()
            .and_then(|f| f.clone())
            .ok_or_else(|| anyhow!("no compositor frame available yet"))?;
        self.write_frame_ppm(path, &frame)
    }

    fn write_frame_ppm(&self, path: &str, frame: &ScopeFrame) -> Result<()> {
        let pixel_count = frame.width.saturating_mul(frame.height);
        let needed = pixel_count.saturating_mul(4);
        if frame.data.len() < needed {
            return Err(anyhow!("frame buffer is incomplete"));
        }
        let mut bytes = Vec::with_capacity(32 + pixel_count.saturating_mul(3));
        write!(&mut bytes, "P6\n{} {}\n255\n", frame.width, frame.height)?;
        for rgba in frame.data[..needed].chunks_exact(4) {
            bytes.extend_from_slice(&rgba[..3]);
        }
        std::fs::write(path, bytes)?;
        Ok(())
    }

    pub fn jkl_rate(&self) -> f64 {
        self.jkl_rate
    }

    /// Quick diagnostic: check pipeline + compositor GStreamer state (10ms timeout).
    #[allow(dead_code)]
    pub fn pipeline_state_debug(&self) -> String {
        let (_, pipe_cur, pipe_pend) = self.pipeline.state(gst::ClockTime::from_mseconds(10));
        let (_, comp_cur, comp_pend) = self.compositor.state(gst::ClockTime::from_mseconds(10));
        format!(
            "pipe={:?}/{:?} comp={:?}/{:?}",
            pipe_cur, pipe_pend, comp_cur, comp_pend
        )
    }

    pub fn state(&self) -> &PlayerState {
        &self.state
    }

    /// Return the letterbox inset fractions (x, y) for the first active video slot.
    /// Each value is 0.0–0.5 representing the fraction of the canvas that is letterbox.
    /// Returns (0.0, 0.0) if no slot is active or source matches project aspect ratio.
    pub fn content_inset(&self) -> (f64, f64) {
        self.slots
            .first()
            .map(|slot| Self::content_inset_for_slot(slot, self.project_width, self.project_height))
            .unwrap_or((0.0, 0.0))
    }

    /// Return the letterbox inset fractions for a specific clip's live preview slot.
    /// Falls back to the first active video slot when the clip is not currently loaded.
    pub fn content_inset_for_clip(&self, clip_id: Option<&str>) -> (f64, f64) {
        if let Some(clip_id) = clip_id {
            if let Some(clip_idx) = self.clips.iter().position(|clip| clip.id == clip_id) {
                if let Some(slot) = self.slot_for_clip(clip_idx) {
                    return Self::content_inset_for_slot(
                        slot,
                        self.project_width,
                        self.project_height,
                    );
                }
            }
        }
        self.content_inset()
    }

    fn content_inset_for_slot(
        slot: &VideoSlot,
        project_width: u32,
        project_height: u32,
    ) -> (f64, f64) {
        let src_dims = slot
            .effects_bin
            .static_pad("sink")
            .and_then(|ghost| {
                ghost
                    .peer()
                    .and_then(|p| p.current_caps())
                    .or_else(|| ghost.current_caps())
            })
            .and_then(|caps| {
                caps.structure(0).map(|s| {
                    (
                        s.get::<i32>("width").unwrap_or(0),
                        s.get::<i32>("height").unwrap_or(0),
                    )
                })
            });
        src_dims
            .map(|(src_w, src_h)| {
                Self::content_inset_from_dimensions(src_w, src_h, project_width, project_height)
            })
            .unwrap_or((0.0, 0.0))
    }

    fn content_inset_from_dimensions(
        src_w: i32,
        src_h: i32,
        project_width: u32,
        project_height: u32,
    ) -> (f64, f64) {
        if src_w <= 0 || src_h <= 0 || project_width == 0 || project_height == 0 {
            return (0.0, 0.0);
        }
        let pw = project_width as f64;
        let ph = project_height as f64;
        let sw = src_w as f64;
        let sh = src_h as f64;
        let scale = (pw / sw).min(ph / sh);
        let scaled_w = sw * scale;
        let scaled_h = sh * scale;
        let ix = ((pw - scaled_w) / 2.0) / pw;
        let iy = ((ph - scaled_h) / 2.0) / ph;
        (ix.clamp(0.0, 0.5), iy.clamp(0.0, 0.5))
    }

    pub fn preview_divisor(&self) -> u32 {
        self.preview_divisor
    }

    #[allow(dead_code)]
    pub fn current_clip_transform(&self) -> Option<crate::ui::program_monitor::ClipTransform> {
        let c = self.clips.get(self.current_idx?)?;
        Some(crate::ui::program_monitor::ClipTransform {
            crop_left: c.crop_left,
            crop_right: c.crop_right,
            crop_top: c.crop_top,
            crop_bottom: c.crop_bottom,
            rotate: c.rotate,
            flip_h: c.flip_h,
            flip_v: c.flip_v,
        })
    }

    pub fn current_clip_idx(&self) -> Option<usize> {
        self.current_idx
    }

    pub fn is_playing(&self) -> bool {
        self.state == PlayerState::Playing
    }

    pub fn visual_clip_snapshot(&self, clip_id: &str) -> Option<ProgramClip> {
        self.clips.iter().find(|clip| clip.id == clip_id).cloned()
    }

    // ── Clip loading ───────────────────────────────────────────────────────

    pub fn load_clips(&mut self, mut clips: Vec<ProgramClip>) {
        self.invalidate_short_frame_cache("project-clips-reloaded");
        clips.sort_by_key(|c| c.timeline_start_ns);
        let (audio, video): (Vec<_>, Vec<_>) = clips.into_iter().partition(|c| c.is_audio_only);
        self.audio_clips = audio;
        self.clips = video;
        let max_track_index = self
            .clips
            .iter()
            .chain(self.audio_clips.iter())
            .map(|c| c.track_index)
            .max();
        self.audio_track_peak_db = max_track_index
            .map(|idx| vec![[-60.0, -60.0]; idx + 1])
            .unwrap_or_default();
        self.audio_peak_db = [-60.0, -60.0];
        self.audio_stream_probe_cache.clear();
        let vdur = self
            .clips
            .iter()
            .map(|c| c.timeline_end_ns())
            .max()
            .unwrap_or(0);
        let adur = self
            .audio_clips
            .iter()
            .map(|c| c.timeline_end_ns())
            .max()
            .unwrap_or(0);
        self.timeline_dur_ns = vdur.max(adur);
        self.current_idx = None;
        self.audio_current_source = None;
        self.reverse_video_ducked_clip_idx = None;
        self.teardown_slots();
        // Keep the main pipeline in Paused here; moving to Ready can block on
        // pad deactivation while slot teardown is still settling on some media.
        // The next seek/rebuild path will perform a safe Ready reset after this.
        let _ = self.pipeline.set_state(gst::State::Paused);
        let _ = self.audio_pipeline.set_state(gst::State::Null);
        self.audio_pipeline.set_property("uri", "");
        let _ = self.audio_pipeline.set_state(gst::State::Ready);
        self.teardown_audio_multi_pipeline();
        self.state = PlayerState::Stopped;
        self.timeline_pos_ns = 0;
        self.base_timeline_ns = 0;
        self.play_start = None;
        self.prewarmed_boundary_ns = None;
        self.rebuild_history_count = 0;
        self.rebuild_history_cursor = 0;
        self.last_seeked_frame_pos = None;
        self.last_boundary_rebuild_clips.clear();
        self.last_boundary_rebuild_at = None;
        self.pending_prerender_promote = false;
        self.current_prerender_segment_key = None;
        self.teardown_prepreroll_sidecars();
        self.cleanup_background_prerender_cache(!self.should_preserve_prerender_cache_files());

        // Populate adjustment overlays from the loaded clips.
        self.rebuild_adjustment_overlays();
    }

    /// Resolve the currently active adjustment-layer scopes and supported
    /// preview effects at the current playhead.
    fn rebuild_adjustment_overlays(&mut self) {
        let pos = self.timeline_pos_ns;
        let mut rebuilt: Vec<AdjustmentOverlay> = Vec::new();
        for idx in 0..self.clips.len() {
            let clip = self.clips[idx].clone();
            if !clip.is_adjustment || pos < clip.timeline_start_ns || pos >= clip.timeline_end_ns()
            {
                continue;
            }

            let local_time_ns = clip.local_timeline_position_ns(pos);
            let scale = clip.scale_at_timeline_ns(pos);
            let position_x = clip.position_x_at_timeline_ns(pos);
            let position_y = clip.position_y_at_timeline_ns(pos);
            let rotate = clip.rotate_at_timeline_ns(pos) as f64;
            let crop_left = clip.crop_left_at_timeline_ns(pos);
            let crop_right = clip.crop_right_at_timeline_ns(pos);
            let crop_top = clip.crop_top_at_timeline_ns(pos);
            let crop_bottom = clip.crop_bottom_at_timeline_ns(pos);
            let brightness = clip.brightness_at_timeline_ns(pos);
            let contrast = clip.contrast_at_timeline_ns(pos);
            let saturation = clip.saturation_at_timeline_ns(pos);
            let temperature = clip.temperature_at_timeline_ns(pos);
            let tint = clip.tint_at_timeline_ns(pos);
            let opacity = clip.opacity_at_timeline_ns(pos).clamp(0.0, 1.0);
            if opacity <= f64::EPSILON {
                continue;
            }

            let has_coloradj = (temperature - 6500.0).abs() > 1.0 || tint.abs() > 0.001;
            let has_3point = clip.shadows != 0.0
                || clip.midtones != 0.0
                || clip.highlights != 0.0
                || clip.black_point != 0.0
                || clip.highlights_warmth != 0.0
                || clip.highlights_tint != 0.0
                || clip.midtones_warmth != 0.0
                || clip.midtones_tint != 0.0
                || clip.shadows_warmth != 0.0
                || clip.shadows_tint != 0.0;
            let vb = Self::compute_videobalance_params(
                brightness,
                contrast,
                saturation,
                temperature,
                tint,
                clip.shadows,
                clip.midtones,
                clip.highlights,
                clip.exposure,
                clip.black_point,
                clip.highlights_warmth,
                clip.highlights_tint,
                clip.midtones_warmth,
                clip.midtones_tint,
                clip.shadows_warmth,
                clip.shadows_tint,
                has_coloradj,
                has_3point,
            );
            let vb_params = if vb.brightness.abs() > f64::EPSILON
                || (vb.contrast - 1.0).abs() > f64::EPSILON
                || (vb.saturation - 1.0).abs() > f64::EPSILON
                || vb.hue.abs() > f64::EPSILON
            {
                Some(vb)
            } else {
                None
            };
            let coloradj_params =
                has_coloradj.then(|| Self::compute_coloradj_params(temperature, tint));
            let three_point = if has_3point {
                let params = Self::compute_3point_params(
                    clip.shadows,
                    clip.midtones,
                    clip.highlights,
                    clip.black_point,
                    clip.highlights_warmth,
                    clip.highlights_tint,
                    clip.midtones_warmth,
                    clip.midtones_tint,
                    clip.shadows_warmth,
                    clip.shadows_tint,
                );
                Some(ThreePointParabola::from_params(&params))
            } else {
                None
            };
            let lut = if self.preview_luts {
                clip.lut_paths
                    .first()
                    .filter(|path| !path.is_empty())
                    .and_then(|path| self.get_or_parse_lut(path))
            } else {
                None
            };
            if vb_params.is_none()
                && coloradj_params.is_none()
                && three_point.is_none()
                && lut.is_none()
            {
                continue;
            }

            rebuilt.push(AdjustmentOverlay {
                track_index: clip.track_index,
                scope: AdjustmentScopeShape::from_transform(
                    self.project_width,
                    self.project_height,
                    scale,
                    position_x,
                    position_y,
                    rotate,
                    crop_left,
                    crop_right,
                    crop_top,
                    crop_bottom,
                ),
                mask: crate::media::mask_alpha::prepare_adjustment_canvas_masks(
                    &clip.masks,
                    local_time_ns,
                    scale,
                    position_x,
                    position_y,
                    rotate,
                ),
                opacity,
                vb_params,
                coloradj_params,
                three_point,
                lut,
            });
        }
        rebuilt.sort_by_key(|overlay| overlay.track_index);
        if let Ok(mut overlays) = self.adjustment_overlays.lock() {
            *overlays = rebuilt;
        }
    }

    /// Keep the permanent adjustment elements neutral; scoped adjustment
    /// preview now runs via the compositor-output probe.
    fn rebuild_adjustment_effects_chain(&mut self) {
        self.adj_videobalance.set_property("brightness", 0.0_f64);
        self.adj_videobalance.set_property("contrast", 1.0_f64);
        self.adj_videobalance.set_property("saturation", 1.0_f64);
        self.adj_videobalance.set_property("hue", 0.0_f64);

        if let Some(ref ca) = self.adj_coloradj {
            ca.set_property("r", 0.5_f64);
            ca.set_property("g", 0.5_f64);
            ca.set_property("b", 0.5_f64);
        }

        if let Some(ref cb) = self.adj_3point {
            cb.set_property("black-color-r", 0.5_f64);
            cb.set_property("black-color-g", 0.5_f64);
            cb.set_property("black-color-b", 0.5_f64);
            cb.set_property("gray-color-r", 0.5_f64);
            cb.set_property("gray-color-g", 0.5_f64);
            cb.set_property("gray-color-b", 0.5_f64);
            cb.set_property("white-color-r", 0.5_f64);
            cb.set_property("white-color-g", 0.5_f64);
            cb.set_property("white-color-b", 0.5_f64);
        }
    }

    /// Update the shared timeline position for the adjustment probe.
    fn sync_adjustment_timeline_pos(&self) {
        self.adjustment_timeline_pos
            .store(self.timeline_pos_ns, Ordering::Relaxed);
    }

    // ── Transport controls ─────────────────────────────────────────────────

    /// Seek to `timeline_pos_ns`.  Returns `true` if a non-blocking Playing
    /// pulse was started for 3+ tracks — the caller **must** schedule
    /// `complete_playing_pulse()` via a GTK idle/timeout callback so the main
    /// loop can run and `gtk4paintablesink` can complete its preroll.
    pub fn seek(&mut self, timeline_pos_ns: u64) -> bool {
        let seek_started = Instant::now();
        let resume_playback = self.state == PlayerState::Playing;
        self.consume_pending_short_frame_capture();

        // Frame-boundary deduplication: quantize to the nearest frame and
        // skip redundant pipeline work when the playhead hasn't moved to a
        // new frame.  This eliminates unnecessary decoder seeks during slow
        // timeline scrubbing where multiple pixel-level drag events land on
        // the same video frame.
        let frame_pos = self.quantize_frame_position_ns(timeline_pos_ns);
        if !resume_playback && self.last_seeked_frame_pos == Some(frame_pos) {
            self.timeline_pos_ns = timeline_pos_ns;
            self.base_timeline_ns = timeline_pos_ns;
            return false;
        }

        self.timeline_pos_ns = timeline_pos_ns;
        self.base_timeline_ns = timeline_pos_ns;
        self.play_start = None;
        self.prewarmed_boundary_ns = None;
        if self.clips.is_empty() && self.audio_clips.is_empty() {
            log::debug!(
                "seek: no clips timeline_pos={} elapsed_ms={}",
                timeline_pos_ns,
                seek_started.elapsed().as_millis()
            );
            return false;
        }
        let mut short_frame_signature = None;
        let mut short_frame_cache_hit = false;
        // Default budget for non-cached seeks. The effective_wait_timeout_ms()
        // function further caps this based on player state and slot count, so
        // during interactive scrubbing the actual wait is much shorter (~150ms).
        let mut arrival_wait_budget_ms: u64 = if resume_playback { 3000 } else { 500 };
        let scope_seq_before_seek = self.scope_frame_seq.load(Ordering::Relaxed);
        if !resume_playback {
            let signature = self.short_frame_cache_signature_for_frame(frame_pos);
            short_frame_cache_hit = self.short_frame_cache.lookup(frame_pos, signature);
            if short_frame_cache_hit {
                arrival_wait_budget_ms = self.short_frame_cache_wait_budget_ms();
                log::debug!(
                    "short_frame_cache: hit frame={} signature={} wait_budget_ms={}",
                    frame_pos,
                    signature,
                    arrival_wait_budget_ms
                );
            }
            short_frame_signature = Some(signature);
        }
        let mut fast_path = false;
        // Fast path: when paused with the same clips already loaded, seek the
        // existing decoders in-place instead of tearing down and rebuilding the
        // pipeline.  Rebuilding always goes through Ready state (black background
        // flash) and lets decoders preroll at position 0 (first-frame flash)
        // before the ACCURATE seek is applied — exactly the bug that caused the
        // monitor to show a black screen or the first frame during scrubbing.
        if !resume_playback
            && !self.slots.is_empty()
            && self.clips_match_current_slots(timeline_pos_ns)
        {
            fast_path = true;
            self.current_idx = self.clip_at(timeline_pos_ns);
            let needs_async = self.seek_slots_in_place(timeline_pos_ns, arrival_wait_budget_ms);
            self.apply_keyframed_video_slot_properties(timeline_pos_ns);
            self.apply_transition_effects(timeline_pos_ns);
            self.sync_audio_to(timeline_pos_ns);
            if let Some(signature) = short_frame_signature {
                self.queue_or_store_short_frame_cache(
                    frame_pos,
                    signature,
                    scope_seq_before_seek,
                    needs_async,
                    "seek-fast-path",
                );
            }
            log::info!(
                "seek: done timeline_pos={} fast_path={} needs_async={} slots={} cache_hit={} wait_budget_ms={} elapsed_ms={}",
                timeline_pos_ns,
                fast_path,
                needs_async,
                self.slots.len(),
                short_frame_cache_hit,
                arrival_wait_budget_ms,
                seek_started.elapsed().as_millis()
            );
            self.last_seeked_frame_pos = Some(frame_pos);
            self.last_seek_instant = Some(Instant::now());
            return needs_async;
        }
        // Full rebuild: needed when the set of active clips has changed (e.g.
        // crossing a clip boundary), on cold start, or when resuming from playing.
        self.rebuild_pipeline_at(timeline_pos_ns);
        self.apply_keyframed_video_slot_properties(timeline_pos_ns);
        self.apply_transition_effects(timeline_pos_ns);
        // Sync audio-only pipeline. While paused/stopped, avoid forcing the
        // multi-audio mixer to rebuild on every scrub if the active set is unchanged.
        if resume_playback {
            self.force_sync_audio_to(timeline_pos_ns);
        } else {
            self.sync_audio_to(timeline_pos_ns);
        }
        let needs_async = if resume_playback {
            self.play_start = Some(Instant::now());
            false
        } else if self.state == PlayerState::Stopped {
            // After a fresh project open (state Stopped), do NOT pulse to
            // Playing: the rebuild path already waited for compositor
            // preroll in Paused, so the display sink has the first frame.
            // A playing pulse here would briefly advance the pipeline,
            // rendering several frames and looking like autoplay.
            // Transition to Paused so subsequent scrubs get proper pulses.
            self.state = PlayerState::Paused;
            false
        } else if self.current_idx.is_some() {
            if self.slots.len() >= 3 {
                // For 3+ tracks, start a non-blocking Playing pulse so the GTK
                // main loop can service gtk4paintablesink's preroll.  The caller
                // must schedule complete_playing_pulse() via idle/timeout.
                self.start_playing_pulse();
                true
            } else {
                // ≤2 tracks: synchronous pulse works fine.
                self.playing_pulse();
                false
            }
        } else {
            false
        };
        if !resume_playback {
            if let Some(signature) = short_frame_signature {
                self.queue_or_store_short_frame_cache(
                    frame_pos,
                    signature,
                    scope_seq_before_seek,
                    needs_async,
                    "seek-rebuild-path",
                );
            }
        }
        log::info!(
            "seek: done timeline_pos={} fast_path={} needs_async={} slots={} cache_hit={} elapsed_ms={}",
            timeline_pos_ns,
            fast_path,
            needs_async,
            self.slots.len(),
            short_frame_cache_hit,
            seek_started.elapsed().as_millis()
        );
        self.last_seeked_frame_pos = Some(frame_pos);
        self.last_seek_instant = Some(Instant::now());
        needs_async
    }

    pub fn play(&mut self) {
        if self.clips.is_empty() && self.audio_clips.is_empty() {
            return;
        }
        self.pending_short_frame_capture = None;
        let pos = self.timeline_pos_ns;
        if self.slots.is_empty() {
            // When starting playback from a cold/stopped state, rebuild via the
            // playback path (not paused scrubbing path) to avoid paused-seek stalls.
            self.state = PlayerState::Playing;
            self.rebuild_pipeline_at(pos);
        }
        // Ensure playback starts with playback-rate seeks (including reverse)
        // even when slots were already loaded by paused-seek paths.
        //
        // Skip still-image clips: their source decoder has already EOSed into
        // `imagefreeze`, so a FLUSH|ACCURATE seek here is racy — it can clear
        // the imagefreeze cached buffer without managing to pull a fresh one
        // from the parked decoder, leaving the still's compositor pad empty
        // for the entire playback.  imagefreeze in PLAYING state will keep
        // re-emitting its cached buffer at the configured rate as long as we
        // don't disturb it.  Animated SVGs that have already been prerendered
        // to a video file behave like normal video and are reseeked.
        for slot in &self.slots {
            let Some(clip) = self.clips.get(slot.clip_idx) else {
                continue;
            };
            if clip.is_image && !clip.animated_svg && !slot.animated_svg_rendered {
                continue;
            }
            let _ = Self::seek_slot_decoder_with_retry(
                slot,
                clip,
                pos,
                gst::SeekFlags::FLUSH | gst::SeekFlags::ACCURATE,
                self.frame_duration_ns,
            );
        }
        self.apply_keyframed_video_slot_properties(pos);
        self.apply_transition_effects(pos);
        let _ = self.pipeline.set_state(gst::State::Playing);
        self.force_sync_audio_to(pos);
        match self.audio_current_source {
            Some(AudioCurrentSource::AudioClip(aidx)) => {
                let aclip = &self.audio_clips[aidx];
                let asrc = aclip.source_pos_ns(pos);
                let _ = self.audio_pipeline.seek(
                    aclip.seek_rate(),
                    aclip.audio_seek_flags(),
                    gst::SeekType::Set,
                    gst::ClockTime::from_nseconds(aclip.seek_start_ns(asrc)),
                    gst::SeekType::Set,
                    gst::ClockTime::from_nseconds(aclip.seek_stop_ns(asrc)),
                );
            }
            Some(AudioCurrentSource::ReverseVideoClip(vidx)) => {
                let vclip = &self.clips[vidx];
                let asrc = vclip.source_pos_ns(pos);
                let _ = self.audio_pipeline.seek(
                    vclip.seek_rate(),
                    vclip.audio_seek_flags(),
                    gst::SeekType::Set,
                    gst::ClockTime::from_nseconds(vclip.seek_start_ns(asrc)),
                    gst::SeekType::Set,
                    gst::ClockTime::from_nseconds(vclip.seek_stop_ns(asrc)),
                );
            }
            None => {}
        }
        self.resume_synced_audio_playback();
        self.state = PlayerState::Playing;
        self.base_timeline_ns = pos;
        self.play_start = Some(Instant::now());
        self.last_seeked_frame_pos = None;
        self.jkl_rate = 0.0;
        self.update_drop_late_policy();
        self.update_slot_queue_policy();
        // Re-apply master mute if active (voiceover recording mode).
        if self.master_muted {
            self.set_master_mute(true);
        }
    }

    pub fn pause(&mut self) {
        // Capture final position before stopping the clock.
        if let Some(start) = self.play_start.take() {
            let elapsed = start.elapsed().as_nanos() as u64;
            let speed = if self.jkl_rate != 0.0 {
                self.jkl_rate.abs()
            } else {
                1.0
            };
            self.timeline_pos_ns =
                (self.base_timeline_ns + (elapsed as f64 * speed) as u64).min(self.timeline_dur_ns);
        }
        let _ = self.pipeline.set_state(gst::State::Paused);
        let _ = self.audio_pipeline.set_state(gst::State::Paused);
        if let Some(ref mp) = self.audio_multi_pipeline {
            let _ = mp.set_state(gst::State::Paused);
        }
        self.state = PlayerState::Paused;
        self.jkl_rate = 0.0;
        self.update_drop_late_policy();
        self.update_slot_queue_policy();
    }

    pub fn toggle_play_pause(&mut self) {
        match self.state {
            PlayerState::Playing => self.pause(),
            _ => self.play(),
        }
    }

    pub fn stop(&mut self) {
        self.invalidate_short_frame_cache("transport-stop");
        // Don't teardown slots — keep decoder elements alive so the next
        // play() can reuse them via seek instead of a cold rebuild.
        // Tearing down + rebuilding causes a race where uridecodebin doesn't
        // produce pads fast enough during the preroll window, leaving video
        // unlinked (black screen with audio on the next play).
        self.teardown_prepreroll_sidecars();
        let _ = self.audio_pipeline.set_state(gst::State::Paused);
        self.teardown_audio_multi_pipeline();
        self.apply_reverse_video_main_audio_ducking(None);
        self.play_start = None;
        self.prewarmed_boundary_ns = None;
        self.last_seeked_frame_pos = None;
        self.last_boundary_rebuild_clips.clear();
        self.last_boundary_rebuild_at = None;
        self.current_idx = None;
        self.audio_current_source = None;
        self.audio_peak_db = [-60.0, -60.0];
        for peaks in &mut self.audio_track_peak_db {
            peaks[0] = -60.0;
            peaks[1] = -60.0;
        }
        if self.clips.is_empty() && self.audio_clips.is_empty() {
            self.state = PlayerState::Stopped;
            return;
        }
        // Seek to 0 while keeping slots alive, then pause.
        self.state = PlayerState::Paused;
        let _ = self.seek(0);
        self.state = PlayerState::Stopped;
        self.timeline_pos_ns = 0;
        self.base_timeline_ns = 0;
        self.jkl_rate = 0.0;
        self.update_drop_late_policy();
        self.update_slot_queue_policy();
    }

    pub fn set_jkl_rate(&mut self, rate: f64) {
        let old_rate = self.jkl_rate;
        self.jkl_rate = rate;
        if rate == 0.0 {
            self.pause();
            return;
        }
        // Capture current position before changing rate.
        if let Some(start) = self.play_start.take() {
            let elapsed = start.elapsed().as_nanos() as u64;
            let speed = if old_rate.abs() > 0.0 {
                old_rate.abs()
            } else {
                1.0
            };
            self.timeline_pos_ns =
                (self.base_timeline_ns + (elapsed as f64 * speed) as u64).min(self.timeline_dur_ns);
        }
        // For negative rates, just pause (reverse not yet supported with compositor).
        if rate < 0.0 {
            self.jkl_rate = 0.0;
            self.pause();
            return;
        }
        self.base_timeline_ns = self.timeline_pos_ns;
        self.play_start = Some(Instant::now());

        if self.state != PlayerState::Playing || self.slots.is_empty() {
            // Cold start — need full pipeline rebuild.
            self.rebuild_pipeline_at(self.timeline_pos_ns);
            let _ = self.pipeline.set_state(gst::State::Playing);
            self.state = PlayerState::Playing;
        } else {
            // Hot rate change — send a rate-seek to the pipeline so scaletempo
            // adjusts pitch preservation without tearing down the pipeline.
            // Seek each decoder slot at the new rate.
            for slot in &self.slots {
                let Some(clip) = self.clips.get(slot.clip_idx) else {
                    continue;
                };
                let source_ns =
                    Self::effective_slot_source_pos_ns(slot, clip, self.timeline_pos_ns);
                let _ = slot.decoder.seek(
                    rate,
                    gst::SeekFlags::FLUSH | gst::SeekFlags::ACCURATE,
                    gst::SeekType::Set,
                    gst::ClockTime::from_nseconds(source_ns),
                    gst::SeekType::None,
                    gst::ClockTime::NONE,
                );
            }
        }
        self.update_drop_late_policy();
        self.update_slot_queue_policy();
    }

    // ── Poll ───────────────────────────────────────────────────────────────

    fn push_master_peak(&mut self, l: f64, r: f64) {
        self.audio_peak_db[0] = self.audio_peak_db[0].max(l);
        self.audio_peak_db[1] = self.audio_peak_db[1].max(r);
    }

    fn push_track_peak(&mut self, track_index: usize, l: f64, r: f64) {
        if track_index >= self.audio_track_peak_db.len() {
            self.audio_track_peak_db
                .resize(track_index + 1, [-60.0, -60.0]);
        }
        self.audio_track_peak_db[track_index][0] = self.audio_track_peak_db[track_index][0].max(l);
        self.audio_track_peak_db[track_index][1] = self.audio_track_peak_db[track_index][1].max(r);
    }

    fn active_audio_track_index(&self) -> Option<usize> {
        match self.audio_current_source {
            Some(AudioCurrentSource::AudioClip(idx)) => {
                self.audio_clips.get(idx).map(|c| c.track_index)
            }
            Some(AudioCurrentSource::ReverseVideoClip(idx)) => {
                self.clips.get(idx).map(|c| c.track_index)
            }
            None => None,
        }
    }

    fn resume_synced_audio_playback(&mut self) {
        if self.audio_current_source.is_some() {
            let _ = self.audio_pipeline.set_state(gst::State::Playing);
        }
        if let Some(ref mp) = self.audio_multi_pipeline {
            if let Some(base) = self.pipeline.base_time() {
                mp.set_base_time(base);
            }
            let _ = mp.set_state(gst::State::Playing);
        }
    }

    fn prerender_meter_track_indices_for(clips: &[ProgramClip], active: &[usize]) -> Vec<usize> {
        let mut seen = HashSet::new();
        active
            .iter()
            .filter_map(|&clip_idx| clips.get(clip_idx).map(|c| c.track_index))
            .filter(|track_idx| seen.insert(*track_idx))
            .collect()
    }

    fn prerender_meter_track_indices(&self) -> Vec<usize> {
        let Some(active) = self.prerender_active_clips.as_ref() else {
            return Vec::new();
        };
        Self::prerender_meter_track_indices_for(&self.clips, active)
    }

    fn crossfade_curve_gain(curve: &CrossfadeCurve, progress: f64, incoming: bool) -> f64 {
        let t = progress.clamp(0.0, 1.0);
        match (curve, incoming) {
            (CrossfadeCurve::EqualPower, true) => (t * std::f64::consts::FRAC_PI_2).sin(),
            (CrossfadeCurve::EqualPower, false) => (t * std::f64::consts::FRAC_PI_2).cos(),
            (CrossfadeCurve::Linear, true) => t,
            (CrossfadeCurve::Linear, false) => 1.0 - t,
        }
    }

    fn adjacent_prev_same_track(clips: &[ProgramClip], clip_idx: usize) -> Option<usize> {
        let clip = clips.get(clip_idx)?;
        clips
            .iter()
            .enumerate()
            .filter(|(idx, c)| {
                *idx != clip_idx
                    && c.track_index == clip.track_index
                    && c.timeline_end_ns() == clip.timeline_start_ns
            })
            .max_by_key(|(_, c)| c.timeline_start_ns)
            .map(|(idx, _)| idx)
    }

    fn adjacent_next_same_track(clips: &[ProgramClip], clip_idx: usize) -> Option<usize> {
        let clip = clips.get(clip_idx)?;
        clips
            .iter()
            .enumerate()
            .filter(|(idx, c)| {
                *idx != clip_idx
                    && c.track_index == clip.track_index
                    && c.timeline_start_ns == clip.timeline_end_ns()
            })
            .min_by_key(|(_, c)| c.timeline_start_ns)
            .map(|(idx, _)| idx)
    }

    fn adjacent_prev_same_track_with_audio(
        clips: &[ProgramClip],
        clip_idx: usize,
    ) -> Option<usize> {
        Self::adjacent_prev_same_track(clips, clip_idx).filter(|&idx| {
            clips
                .get(idx)
                .map(|clip| clip.has_embedded_audio())
                .unwrap_or(false)
        })
    }

    fn adjacent_next_same_track_with_audio(
        clips: &[ProgramClip],
        clip_idx: usize,
    ) -> Option<usize> {
        Self::adjacent_next_same_track(clips, clip_idx).filter(|&idx| {
            clips
                .get(idx)
                .map(|clip| clip.has_embedded_audio())
                .unwrap_or(false)
        })
    }

    fn outgoing_transition_window_for_clip(clip: &ProgramClip) -> Option<TransitionOverlapWindow> {
        if !clip.has_outgoing_transition() {
            return None;
        }
        Some(
            clip.transition_cut_split()
                .overlap_window(clip.timeline_end_ns()),
        )
    }

    fn incoming_transition_window_for_clip(
        clips: &[ProgramClip],
        clip_idx: usize,
    ) -> Option<(usize, TransitionOverlapWindow)> {
        let prev_idx = Self::adjacent_prev_same_track(clips, clip_idx)?;
        let prev_clip = clips.get(prev_idx)?;
        Self::outgoing_transition_window_for_clip(prev_clip).map(|window| (prev_idx, window))
    }

    fn clip_active_window(clips: &[ProgramClip], clip_idx: usize) -> Option<(u64, u64)> {
        let clip = clips.get(clip_idx)?;
        let incoming_before_ns = Self::incoming_transition_window_for_clip(clips, clip_idx)
            .map(|(_, window)| window.before_cut_ns)
            .unwrap_or(0);
        let outgoing_after_ns = Self::outgoing_transition_window_for_clip(clip)
            .map(|window| window.after_cut_ns)
            .unwrap_or(0);
        Some((
            clip.timeline_start_ns.saturating_sub(incoming_before_ns),
            clip.timeline_end_ns().saturating_add(outgoing_after_ns),
        ))
    }

    fn clamped_crossfade_duration_ns(
        requested_duration_ns: u64,
        a: &ProgramClip,
        b: &ProgramClip,
    ) -> u64 {
        requested_duration_ns
            .min(a.duration_ns() / 2)
            .min(b.duration_ns() / 2)
    }

    fn clip_crossfade_gain(
        &self,
        clips: &[ProgramClip],
        clip_idx: usize,
        timeline_pos_ns: u64,
    ) -> f64 {
        Self::compute_clip_crossfade_gain(
            self.crossfade_enabled,
            &self.crossfade_curve,
            self.crossfade_duration_ns,
            clips,
            clip_idx,
            timeline_pos_ns,
        )
    }

    fn compute_clip_crossfade_gain(
        crossfade_enabled: bool,
        crossfade_curve: &CrossfadeCurve,
        crossfade_duration_ns: u64,
        clips: &[ProgramClip],
        clip_idx: usize,
        timeline_pos_ns: u64,
    ) -> f64 {
        if !crossfade_enabled || crossfade_duration_ns == 0 {
            return 1.0;
        }
        let Some(clip) = clips.get(clip_idx) else {
            return 1.0;
        };
        if !clip.has_embedded_audio() {
            return 1.0;
        }
        let mut gain = 1.0_f64;

        if let Some(prev_idx) = Self::adjacent_prev_same_track_with_audio(clips, clip_idx) {
            if let Some(prev) = clips.get(prev_idx) {
                let fade_ns =
                    Self::clamped_crossfade_duration_ns(crossfade_duration_ns, prev, clip);
                if fade_ns > 0 {
                    let fade_end = clip.timeline_start_ns.saturating_add(fade_ns);
                    if timeline_pos_ns >= clip.timeline_start_ns && timeline_pos_ns < fade_end {
                        let elapsed = timeline_pos_ns.saturating_sub(clip.timeline_start_ns) as f64;
                        let progress = elapsed / fade_ns as f64;
                        gain *= Self::crossfade_curve_gain(crossfade_curve, progress, true);
                    }
                }
            }
        }

        if let Some(next_idx) = Self::adjacent_next_same_track_with_audio(clips, clip_idx) {
            if let Some(next) = clips.get(next_idx) {
                let fade_ns =
                    Self::clamped_crossfade_duration_ns(crossfade_duration_ns, clip, next);
                if fade_ns > 0 {
                    let fade_start = clip.timeline_end_ns().saturating_sub(fade_ns);
                    if timeline_pos_ns >= fade_start && timeline_pos_ns < clip.timeline_end_ns() {
                        let elapsed = timeline_pos_ns.saturating_sub(fade_start) as f64;
                        let progress = elapsed / fade_ns as f64;
                        gain *= Self::crossfade_curve_gain(crossfade_curve, progress, false);
                    }
                }
            }
        }

        gain.clamp(0.0, 1.0)
    }

    fn effective_main_clip_volume(&self, clip_idx: usize, timeline_pos_ns: u64) -> f64 {
        let Some(clip) = self.clips.get(clip_idx) else {
            return 0.0;
        };
        if self.reverse_video_ducked_clip_idx == Some(clip_idx) {
            return 0.0;
        }
        let base = clip
            .volume_at_timeline_ns(timeline_pos_ns)
            .clamp(0.0, MAX_PREVIEW_AUDIO_GAIN);
        (base * self.clip_crossfade_gain(&self.clips, clip_idx, timeline_pos_ns))
            .clamp(0.0, MAX_PREVIEW_AUDIO_GAIN)
    }

    fn effective_main_clip_pan(&self, clip_idx: usize, timeline_pos_ns: u64) -> f64 {
        let Some(clip) = self.clips.get(clip_idx) else {
            return 0.0;
        };
        if self.reverse_video_ducked_clip_idx == Some(clip_idx) {
            return 0.0;
        }
        clip.pan_at_timeline_ns(timeline_pos_ns).clamp(-1.0, 1.0)
    }

    fn effective_audio_source_volume(
        &self,
        source: AudioCurrentSource,
        timeline_pos_ns: u64,
    ) -> f64 {
        match source {
            AudioCurrentSource::AudioClip(idx) => {
                let Some(clip) = self.audio_clips.get(idx) else {
                    return 0.0;
                };
                let base = clip
                    .volume_at_timeline_ns(timeline_pos_ns)
                    .clamp(0.0, MAX_PREVIEW_AUDIO_GAIN);
                (base * self.clip_crossfade_gain(&self.audio_clips, idx, timeline_pos_ns))
                    .clamp(0.0, MAX_PREVIEW_AUDIO_GAIN)
            }
            AudioCurrentSource::ReverseVideoClip(idx) => {
                let Some(clip) = self.clips.get(idx) else {
                    return 0.0;
                };
                let base = clip
                    .volume_at_timeline_ns(timeline_pos_ns)
                    .clamp(0.0, MAX_PREVIEW_AUDIO_GAIN);
                (base * self.clip_crossfade_gain(&self.clips, idx, timeline_pos_ns))
                    .clamp(0.0, MAX_PREVIEW_AUDIO_GAIN)
            }
        }
    }

    fn effective_audio_source_pan(&self, source: AudioCurrentSource, timeline_pos_ns: u64) -> f64 {
        match source {
            AudioCurrentSource::AudioClip(idx) => self
                .audio_clips
                .get(idx)
                .map(|clip| clip.pan_at_timeline_ns(timeline_pos_ns))
                .unwrap_or(0.0),
            AudioCurrentSource::ReverseVideoClip(idx) => self
                .clips
                .get(idx)
                .map(|clip| clip.pan_at_timeline_ns(timeline_pos_ns))
                .unwrap_or(0.0),
        }
    }

    /// Mute/unmute the main audio output (for voiceover recording).
    pub fn set_master_mute(&mut self, mute: bool) {
        self.master_muted = mute;
        // Mute the main pipeline's audio sink.
        if self.audio_sink.find_property("mute").is_some() {
            self.audio_sink.set_property("mute", mute);
        }
        // Also set volume to 0 on the audio sink for backends that don't support mute.
        if self.audio_sink.find_property("volume").is_some() {
            self.audio_sink
                .set_property("volume", if mute { 0.0_f64 } else { 1.0_f64 });
        }
        // Mute audiomixer pads (the actual per-clip mix volumes).
        if mute {
            for slot in &self.slots {
                if let Some(ref pad) = slot.audio_mixer_pad {
                    pad.set_property("volume", 0.0_f64);
                }
            }
        }
        // Also mute the audio-only pipeline.
        if let Some(ref vol) = self.audio_volume_element {
            vol.set_property("volume", if mute { 0.0 } else { 1.0 });
        }
        // Mute the multi-clip audio pipeline if active.
        if let Some(ref mp) = self.audio_multi_pipeline {
            if let Some(sink) = mp.by_name("autoaudiosink0") {
                if sink.find_property("mute").is_some() {
                    sink.set_property("mute", mute);
                }
            }
            // Also just set the pipeline to Ready when muted for a clean stop.
            if mute {
                let _ = mp.set_state(gst::State::Paused);
            }
        }
    }

    fn set_audio_pipeline_volume(&self, volume: f64) {
        if let Some(ref vol_elem) = self.audio_volume_element {
            vol_elem.set_property("volume", volume);
        } else {
            self.audio_pipeline.set_property("volume", volume);
        }
    }

    fn set_audio_pipeline_pan(&self, pan: f64) {
        if let Some(ref pan_elem) = self.audio_panorama_element {
            pan_elem.set_property("panorama", pan.clamp(-1.0, 1.0) as f32);
        }
    }

    fn set_audio_pipeline_eq(&self, eq_bands: &[crate::model::clip::EqBand; 3]) {
        if let Some(ref eq_elem) = self.audio_eq_element {
            for (i, b) in eq_bands.iter().enumerate() {
                eq_set_band(eq_elem, i as u32, b.freq, b.gain, b.q);
            }
        }
    }

    /// Sync EQ band gains (with keyframe evaluation) for the current audio-only clip.
    fn sync_audio_pipeline_eq(&self, timeline_pos_ns: u64) {
        if let Some(ref eq_elem) = self.audio_eq_element {
            if let Some(source) = self.audio_current_source {
                let clip = match source {
                    AudioCurrentSource::AudioClip(idx) => self.audio_clips.get(idx),
                    AudioCurrentSource::ReverseVideoClip(idx) => self.clips.get(idx),
                };
                if let Some(clip) = clip {
                    for i in 0..3u32 {
                        let b = &clip.eq_bands[i as usize];
                        eq_set_band(eq_elem, i, b.freq, b.gain, b.q);
                    }
                }
            }
        }
    }

    fn apply_main_audio_slot_volumes(&self, timeline_pos_ns: u64) {
        for slot in &self.slots {
            // When master-muted, we still need to zero prerender slot pads.
            if slot.is_prerender_slot && !self.master_muted {
                continue;
            }
            if let Some(ref pad) = slot.audio_mixer_pad {
                let volume = if self.master_muted || slot.hidden {
                    0.0
                } else {
                    self.effective_main_clip_volume(slot.clip_idx, timeline_pos_ns)
                };
                pad.set_property("volume", volume);
            }
            if let Some(ref pan_elem) = slot.audio_panorama {
                let pan = if slot.hidden {
                    0.0
                } else {
                    self.effective_main_clip_pan(slot.clip_idx, timeline_pos_ns)
                };
                pan_elem.set_property("panorama", pan as f32);
            }
            // Sync EQ band gains (including keyframe evaluation).
            if let Some(ref eq_elem) = slot.audio_equalizer {
                if !slot.hidden {
                    if let Some(clip) = self.clips.get(slot.clip_idx) {
                        for i in 0..3u32 {
                            let gain = clip.eq_gain_at_timeline_ns(i as usize, timeline_pos_ns);
                            eq_set_band_gain(eq_elem, i, gain);
                        }
                    }
                }
            }
        }
    }

    fn sync_preview_audio_levels(&self, timeline_pos_ns: u64) {
        self.apply_main_audio_slot_volumes(timeline_pos_ns);
        if let Some(source) = self.audio_current_source {
            if self.master_muted {
                self.set_audio_pipeline_volume(0.0);
            } else {
                let volume = self.effective_audio_source_volume(source, timeline_pos_ns);
                self.set_audio_pipeline_volume(volume);
                let pan = self.effective_audio_source_pan(source, timeline_pos_ns);
                self.set_audio_pipeline_pan(pan);
            }
        }
        if !self.master_muted {
            self.sync_audio_pipeline_eq(timeline_pos_ns);
        }
        // Sync multi-audio pipeline volumes (for keyframe animation + ducking).
        // Determine if any non-ducked audio is present at this position (triggers ducking).
        // Ducking is active when any track has duck=true AND non-ducked audio
        // (dialogue) is present at the current position. No global preference gate
        // needed — the per-track duck toggle is sufficient.
        let any_track_ducks = self.audio_clips.iter().any(|c| c.duck);
        let should_duck = any_track_ducks && {
            // Check if any video clip with embedded audio is active (dialogue source).
            let has_video_audio = self.clips.iter().any(|c| {
                c.has_embedded_audio()
                    && !c.is_audio_only
                    && timeline_pos_ns >= c.timeline_start_ns
                    && timeline_pos_ns < c.timeline_end_ns()
            });
            // Check if any non-ducked audio-only clip is active.
            let has_non_ducked_audio = self.audio_clips.iter().enumerate().any(|(i, c)| {
                !c.duck
                    && timeline_pos_ns >= c.timeline_start_ns
                    && timeline_pos_ns < c.timeline_end_ns()
                    && self.audio_multi_active.contains(&i)
            });
            has_video_audio || has_non_ducked_audio
        };
        for (&aidx, pad) in &self.audio_multi_pads {
            if let Some(clip) = self.audio_clips.get(aidx) {
                let mut vol = if self.master_muted {
                    0.0
                } else {
                    clip.volume_at_timeline_ns(timeline_pos_ns).clamp(0.0, 4.0)
                };
                // Apply ducking reduction to tracks marked as duck targets.
                if should_duck && clip.duck {
                    let gain = 10.0_f64.powf(clip.duck_amount_db.min(0.0) / 20.0);
                    vol *= gain;
                }
                pad.set_property("volume", vol);
            }
        }
        for (&aidx, pan_elem) in &self.audio_multi_pan_elems {
            if let Some(clip) = self.audio_clips.get(aidx) {
                let pan = clip.pan_at_timeline_ns(timeline_pos_ns);
                pan_elem.set_property("panorama", pan as f32);
            }
        }
    }

    pub fn poll(&mut self) -> bool {
        self.consume_pending_short_frame_capture();
        let _eos = self.poll_bus();
        self.poll_background_prerender_results();
        if self.pending_prerender_promote && self.state == PlayerState::Playing {
            self.pending_prerender_promote = false;
            log::info!(
                "background_prerender: promoting live playback to prerender at timeline_pos={}",
                self.timeline_pos_ns
            );
            // Force full rebuild path so prerender slot selection runs;
            // otherwise continue-decoder fast path would short-circuit.
            self.teardown_slots();
            self.rebuild_pipeline_at(self.timeline_pos_ns);
        }

        if self.state == PlayerState::Playing {
            self.audio_peak_db[0] = (self.audio_peak_db[0] - 3.0).max(-60.0);
            self.audio_peak_db[1] = (self.audio_peak_db[1] - 3.0).max(-60.0);
            for peaks in &mut self.audio_track_peak_db {
                peaks[0] = (peaks[0] - 3.0).max(-60.0);
                peaks[1] = (peaks[1] - 3.0).max(-60.0);
            }
        }

        if self.state != PlayerState::Playing {
            self.pending_prerender_promote = false;
            self.maybe_request_idle_background_prerender();
            return false;
        }

        // Timeline end reached?
        if self.timeline_dur_ns > 0 && self.timeline_pos_ns >= self.timeline_dur_ns {
            self.teardown_slots();
            let _ = self.pipeline.set_state(gst::State::Ready);
            let _ = self.audio_pipeline.set_state(gst::State::Ready);
            self.state = PlayerState::Stopped;
            self.current_idx = None;
            self.audio_current_source = None;
            self.apply_reverse_video_main_audio_ducking(None);
            self.play_start = None;
            self.timeline_pos_ns = self.timeline_dur_ns;
            self.prewarmed_boundary_ns = None;
            self.update_drop_late_policy();
            self.update_slot_queue_policy();
            return true;
        }

        // Advance timeline position from wall clock.
        let speed = if self.jkl_rate != 0.0 {
            self.jkl_rate.abs()
        } else {
            1.0
        };
        let new_pos = if let Some(start) = self.play_start {
            let elapsed = start.elapsed().as_nanos() as u64;
            (self.base_timeline_ns + (elapsed as f64 * speed) as u64).min(self.timeline_dur_ns)
        } else {
            self.timeline_pos_ns
        };

        let changed = new_pos != self.timeline_pos_ns;
        self.timeline_pos_ns = new_pos;
        self.sync_adjustment_timeline_pos();
        if changed {
            self.rebuild_adjustment_overlays();
        }
        self.prewarm_upcoming_boundary(new_pos);

        // Timeline end reached after update?
        if self.timeline_dur_ns > 0 && self.timeline_pos_ns >= self.timeline_dur_ns {
            self.teardown_slots();
            let _ = self.pipeline.set_state(gst::State::Ready);
            let _ = self.audio_pipeline.set_state(gst::State::Ready);
            self.state = PlayerState::Stopped;
            self.current_idx = None;
            self.audio_current_source = None;
            self.apply_reverse_video_main_audio_ducking(None);
            self.play_start = None;
            self.timeline_pos_ns = self.timeline_dur_ns;
            self.prewarmed_boundary_ns = None;
            self.update_drop_late_policy();
            self.update_slot_queue_policy();
            return true;
        }

        // Detect clip boundary changes: have the active clips changed?
        // Exclude adjustment layers — they have no slots and don't affect boundaries.
        let desired: Vec<usize> = self
            .clips_active_at(new_pos)
            .into_iter()
            .filter(|&i| i >= self.clips.len() || !self.clips[i].is_adjustment)
            .collect();
        let current: Vec<usize> = if let Some(active) = self.prerender_active_clips.clone() {
            active
        } else {
            self.slots
                .iter()
                .filter(|s| !s.hidden && !s.is_prerender_slot)
                .map(|s| s.clip_idx)
                .collect()
        };
        // Boundary membership changes are set-based; slot vector order can
        // differ (e.g. realtime path add/remove appends) without implying an
        // actual active-clip change. Normalize before comparison/debounce.
        let mut desired_norm = desired.clone();
        desired_norm.sort_unstable();
        let mut current_norm = current.clone();
        current_norm.sort_unstable();
        if desired_norm != current_norm {
            const BOUNDARY_REBUILD_DEBOUNCE_MS: u64 = 120;
            let debounce_duplicate = self
                .last_boundary_rebuild_at
                .map(|at| at.elapsed() < Duration::from_millis(BOUNDARY_REBUILD_DEBOUNCE_MS))
                .unwrap_or(false)
                && self.last_boundary_rebuild_clips == desired_norm;
            if debounce_duplicate {
                log::debug!(
                    "poll: debouncing duplicate boundary rebuild at {} desired={:?}",
                    new_pos,
                    desired
                );
            } else {
                log::info!(
                    "poll: boundary crossing at {} desired={:?} current={:?}",
                    new_pos,
                    desired,
                    current
                );
                // Determine whether the audio-pipeline source changes at this
                // boundary.  When it stays the same (e.g. only a video track
                // enters/exits), we leave audio_pipeline in Playing during the
                // rebuild to avoid an audible gap.  After the rebuild we always
                // re-sync so the audio position matches the reset wall clock;
                // without this the audio_pipeline drifts ahead by the rebuild
                // duration and can reach EOS before the timeline ends.
                let audio_wanted = if let Some(idx) = self.reverse_video_clip_at_for_audio(new_pos)
                {
                    Some(AudioCurrentSource::ReverseVideoClip(idx))
                } else {
                    self.audio_clip_at(new_pos)
                        .map(AudioCurrentSource::AudioClip)
                };
                let audio_source_changed = audio_wanted != self.audio_current_source;

                // When realtime_preview is enabled, try the incremental pad-offset
                // path first: hide departing clips, add entering clips with
                // pad.set_offset() for running-time alignment.  Fall back to full
                // rebuild on failure.
                let prefer_prerender_boundary = self.background_prerender
                    && self.active_supports_background_prerender_at(new_pos, &desired)
                    && !desired.iter().any(|&idx| {
                        self.clips
                            .get(idx)
                            .map(Self::clip_has_phase1_keyframes)
                            .unwrap_or(false)
                    });
                if prefer_prerender_boundary && self.realtime_preview {
                    log::info!(
                        "poll: skipping realtime boundary path at {} to allow background prerender usage",
                        new_pos
                    );
                }
                let removing_only_boundary = desired.iter().all(|idx| current.contains(idx))
                    && current.iter().any(|idx| !desired.contains(idx));
                let auto_realtime_in_smooth =
                    matches!(self.playback_priority, PlaybackPriority::Smooth)
                        && self.state == PlayerState::Playing
                        && removing_only_boundary;
                let realtime_boundary_enabled = self.realtime_preview || auto_realtime_in_smooth;
                let has_prerender_slot_active = self.slots.iter().any(|s| s.is_prerender_slot);
                let used_realtime = realtime_boundary_enabled
                    && !has_prerender_slot_active
                    && !prefer_prerender_boundary
                    && self.try_realtime_boundary_update(new_pos, &desired, &current);

                if !used_realtime {
                    self.last_boundary_rebuild_clips = desired_norm.clone();
                    self.last_boundary_rebuild_at = Some(Instant::now());
                    if audio_source_changed {
                        let _ = self.audio_pipeline.set_state(gst::State::Paused);
                    }
                    self.rebuild_pipeline_at(new_pos);
                    let _ = self.pipeline.set_state(gst::State::Playing);
                    self.force_sync_audio_to(new_pos);
                    self.resume_synced_audio_playback();
                    // Reset wall-clock base after rebuild.
                    self.base_timeline_ns = new_pos;
                    self.play_start = Some(Instant::now());
                } else {
                    self.last_boundary_rebuild_clips.clear();
                    self.last_boundary_rebuild_at = None;
                    self.prerender_active_clips = None;
                    // Realtime path succeeded — audio sync without full rebuild.
                    if audio_source_changed {
                        if let Some(ref _src) = audio_wanted {
                            let _ = self.audio_pipeline.set_state(gst::State::Paused);
                        }
                        self.force_sync_audio_to(new_pos);
                        self.resume_synced_audio_playback();
                    }
                }
            }
        } else {
            self.last_boundary_rebuild_clips.clear();
            self.last_boundary_rebuild_at = None;
        }

        // Update current_idx to highest-priority active clip.
        self.current_idx = self.clip_at(new_pos);

        self.apply_keyframed_video_slot_properties(new_pos);
        // Animate transition effects (alpha, crop) for active transition overlaps.
        self.apply_transition_effects(new_pos);

        // Advance audio pipeline across clip boundaries.
        self.poll_audio(new_pos);
        self.update_drop_late_policy();
        self.update_slot_queue_policy();

        changed
    }

    // ── Effects / transform updates ────────────────────────────────────────

    #[allow(dead_code)]
    pub fn update_current_color(&mut self, brightness: f64, contrast: f64, saturation: f64) {
        self.update_current_effects(
            brightness, contrast, saturation, 6500.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0,
            0.0, 0.0, 0.0, 0.0, 0.0, 0.0,
        );
    }

    pub fn update_current_effects(
        &mut self,
        brightness: f64,
        contrast: f64,
        saturation: f64,
        temperature: f64,
        tint: f64,
        denoise: f64,
        sharpness: f64,
        shadows: f64,
        midtones: f64,
        highlights: f64,
        exposure: f64,
        black_point: f64,
        highlights_warmth: f64,
        highlights_tint: f64,
        midtones_warmth: f64,
        midtones_tint: f64,
        shadows_warmth: f64,
        shadows_tint: f64,
        blur: f64,
    ) {
        // For adjustment layers, update the GStreamer effects chain directly (no per-clip slot).
        if let Some(clip_idx) = self.current_idx {
            if clip_idx < self.clips.len() && self.clips[clip_idx].is_adjustment {
                let clip = &mut self.clips[clip_idx];
                clip.brightness = brightness;
                clip.contrast = contrast;
                clip.saturation = saturation;
                clip.temperature = temperature;
                clip.tint = tint;
                clip.denoise = denoise;
                clip.sharpness = sharpness;
                clip.blur = blur;
                clip.shadows = shadows;
                clip.midtones = midtones;
                clip.highlights = highlights;
                clip.exposure = exposure;
                clip.black_point = black_point;
                clip.highlights_warmth = highlights_warmth;
                clip.highlights_tint = highlights_tint;
                clip.midtones_warmth = midtones_warmth;
                clip.midtones_tint = midtones_tint;
                clip.shadows_warmth = shadows_warmth;
                clip.shadows_tint = shadows_tint;
                self.rebuild_adjustment_overlays();
                self.rebuild_adjustment_effects_chain();
                // Flush compositor to force updated effects to take effect.
                if self.state != PlayerState::Playing {
                    self.reseek_slot_for_current();
                }
                return;
            }
        }

        // Check whether the required effect elements exist.  If the slider
        // values moved away from defaults but the slot was built without the
        // element (because values were at defaults when the slot was created),
        // we must do a one-time pipeline rebuild so the element gets created.
        let need_balance = brightness != 0.0
            || contrast != 1.0
            || saturation != 1.0
            || exposure.abs() > f64::EPSILON;
        let need_coloradj = (temperature - 6500.0).abs() > 1.0 || tint.abs() > 0.001;
        let need_3point = shadows.abs() > f64::EPSILON
            || midtones.abs() > f64::EPSILON
            || highlights.abs() > f64::EPSILON
            || black_point.abs() > f64::EPSILON
            || highlights_warmth.abs() > f64::EPSILON
            || highlights_tint.abs() > f64::EPSILON
            || midtones_warmth.abs() > f64::EPSILON
            || midtones_tint.abs() > f64::EPSILON
            || shadows_warmth.abs() > f64::EPSILON
            || shadows_tint.abs() > f64::EPSILON;
        let need_blur = {
            let sigma = (denoise * 4.0 - sharpness * 6.0).clamp(-20.0, 20.0);
            sigma.abs() > f64::EPSILON
        };
        let need_creative_blur = blur > f64::EPSILON;
        let topology_changed = if let Some(slot) =
            self.current_idx.and_then(|idx| self.slot_for_clip(idx))
        {
            (need_balance && slot.videobalance.is_none())
                || (need_coloradj && slot.coloradj_rgb.is_none() && slot.videobalance.is_none())
                || (need_3point && slot.colorbalance_3pt.is_none() && slot.videobalance.is_none())
                || (need_blur && slot.gaussianblur.is_none())
                || (need_creative_blur && slot.squareblur.is_none())
        } else {
            false
        };
        if topology_changed {
            // Update self.clips so rebuild_pipeline_at sees the new values
            // (self.clips is set at load_clips time and not updated by
            // slider callbacks, so without this the rebuild would see stale
            // defaults and take the fast reuse path instead of actually
            // creating the needed effect elements).
            if let Some(clip_idx) = self.current_idx {
                let clip = &mut self.clips[clip_idx];
                clip.brightness = brightness;
                clip.contrast = contrast;
                clip.saturation = saturation;
                clip.temperature = temperature;
                clip.tint = tint;
                clip.denoise = denoise;
                clip.sharpness = sharpness;
                clip.blur = blur;
                clip.shadows = shadows;
                clip.midtones = midtones;
                clip.highlights = highlights;
                clip.exposure = exposure;
                clip.black_point = black_point;
                clip.highlights_warmth = highlights_warmth;
                clip.highlights_tint = highlights_tint;
                clip.midtones_warmth = midtones_warmth;
                clip.midtones_tint = midtones_tint;
                clip.shadows_warmth = shadows_warmth;
                clip.shadows_tint = shadows_tint;
            }
            let pos = self.timeline_pos_ns;
            self.rebuild_pipeline_at(pos);
            return;
        }

        if let Some(slot) = self.current_idx.and_then(|idx| self.slot_for_clip(idx)) {
            let has_coloradj = slot.coloradj_rgb.is_some();
            let has_3point = slot.colorbalance_3pt.is_some();
            if let Some(ref vb) = slot.videobalance {
                let p = Self::compute_videobalance_params(
                    brightness,
                    contrast,
                    saturation,
                    temperature,
                    tint,
                    shadows,
                    midtones,
                    highlights,
                    exposure,
                    black_point,
                    highlights_warmth,
                    highlights_tint,
                    midtones_warmth,
                    midtones_tint,
                    shadows_warmth,
                    shadows_tint,
                    has_coloradj,
                    has_3point,
                );
                vb.set_property("brightness", p.brightness);
                vb.set_property("contrast", p.contrast);
                vb.set_property("saturation", p.saturation);
                vb.set_property("hue", p.hue);
            }
            if let Some(ref ca) = slot.coloradj_rgb {
                let cp = Self::compute_coloradj_params(temperature, tint);
                ca.set_property("r", cp.r);
                ca.set_property("g", cp.g);
                ca.set_property("b", cp.b);
            }
            if let Some(ref tp) = slot.colorbalance_3pt {
                let p = Self::compute_3point_params(
                    shadows,
                    midtones,
                    highlights,
                    black_point,
                    highlights_warmth,
                    highlights_tint,
                    midtones_warmth,
                    midtones_tint,
                    shadows_warmth,
                    shadows_tint,
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
            if let Some(ref gb) = slot.gaussianblur {
                let sigma = (denoise * 4.0 - sharpness * 6.0).clamp(-20.0, 20.0);
                gb.set_property("sigma", sigma);
            }
            if let Some(ref sb) = slot.squareblur {
                sb.set_property("kernel-size", blur.clamp(0.0, 1.0));
            }
        }
        // Keep self.clips in sync so future slot reuse and topology checks
        // see the current slider values (self.clips is only fully refreshed
        // by load_clips, not by slider edits).
        if let Some(clip_idx) = self.current_idx {
            let clip = &mut self.clips[clip_idx];
            clip.brightness = brightness;
            clip.contrast = contrast;
            clip.saturation = saturation;
            clip.temperature = temperature;
            clip.tint = tint;
            clip.denoise = denoise;
            clip.sharpness = sharpness;
            clip.blur = blur;
            clip.shadows = shadows;
            clip.midtones = midtones;
            clip.highlights = highlights;
            clip.exposure = exposure;
            clip.black_point = black_point;
            clip.highlights_warmth = highlights_warmth;
            clip.highlights_tint = highlights_tint;
            clip.midtones_warmth = midtones_warmth;
            clip.midtones_tint = midtones_tint;
            clip.shadows_warmth = shadows_warmth;
            clip.shadows_tint = shadows_tint;
        }
        // Force frame redraw when paused.
        if self.current_idx.is_some() && self.state != PlayerState::Playing {
            self.reseek_slot_for_current();
        }
    }

    /// Update mask data on the current slot's shared state so the pad probe
    /// picks up changes without a pipeline rebuild.
    pub fn update_current_masks(&mut self, masks: &[crate::model::clip::ClipMask]) {
        // Update all active slots — masks may be on any clip.
        for slot in &self.slots {
            let clip = &self.clips[slot.clip_idx];
            // Match by checking if this slot's clip has masks.
            // In practice, we update all slots unconditionally since the
            // shared mask_data contains only this clip's masks.
            if !clip.masks.is_empty() || !masks.is_empty() {
                if let Ok(mut guard) = slot.mask_data.lock() {
                    *guard = masks.to_vec();
                }
            }
        }
        // Force frame redraw when paused.
        if self.current_idx.is_some() && self.state != PlayerState::Playing {
            self.reseek_slot_for_current();
        }
    }

    pub fn update_masks_for_clip(&mut self, clip_id: &str, masks: &[crate::model::clip::ClipMask]) {
        let Some(clip_idx) = self.clips.iter().position(|clip| clip.id == clip_id) else {
            return;
        };

        let is_adjustment = if let Some(clip) = self.clips.get_mut(clip_idx) {
            clip.masks = masks.to_vec();
            clip.is_adjustment
        } else {
            false
        };

        if is_adjustment {
            self.rebuild_adjustment_overlays();
            if self.state != PlayerState::Playing {
                self.reseek_slot_by_clip_idx(clip_idx);
            }
            return;
        }

        if let Some(slot) = self.slot_for_clip(clip_idx) {
            if let Ok(mut guard) = slot.mask_data.lock() {
                *guard = masks.to_vec();
            }
            if self.state != PlayerState::Playing {
                self.reseek_slot_by_clip_idx(clip_idx);
            }
        }
    }

    /// Push a new HSL qualifier into the matching slot's shared state so the
    /// pad probe picks it up on the next frame without a pipeline rebuild.
    /// Accepts `None` to clear the qualifier entirely.
    pub fn update_hsl_qualifier_for_clip(
        &mut self,
        clip_id: &str,
        qualifier: Option<crate::model::clip::HslQualifier>,
    ) {
        let Some(clip_idx) = self.clips.iter().position(|clip| clip.id == clip_id) else {
            return;
        };
        if let Some(clip) = self.clips.get_mut(clip_idx) {
            clip.hsl_qualifier = qualifier.clone();
        }
        if let Some(slot) = self.slot_for_clip(clip_idx) {
            if let Ok(mut guard) = slot.hsl_data.lock() {
                *guard = qualifier;
            }
            if self.state != PlayerState::Playing {
                self.reseek_slot_by_clip_idx(clip_idx);
            }
        }
    }

    /// Apply the project-level master audio gain (from the Loudness Radar
    /// "Normalize to Target" workflow) to the post-mix bus. Values are
    /// clamped to ±24 dB and applied as a linear `volume` property on the
    /// dedicated master volume element — no pipeline rebuild required.
    ///
    /// Call this from `window.rs` whenever `project.master_gain_db` changes
    /// (load, normalize, undo/redo, reset).
    pub fn set_master_gain_db(&mut self, db: f64) {
        let clamped = db.clamp(-24.0, 24.0);
        let linear = 10.0_f64.powf(clamped / 20.0);
        if let Some(ref vol) = self.master_volume_element {
            vol.set_property("volume", linear);
        }
    }

    pub fn update_current_chroma_key(
        &mut self,
        enabled: bool,
        color: u32,
        tolerance: f32,
        softness: f32,
    ) {
        let mut is_static_image = false;
        if let Some(idx) = self.current_idx {
            if let Some(clip) = self.clips.get(idx) {
                is_static_image = Self::clip_requires_live_transform_refresh(clip);
            }
            if let Some(slot) = self.slot_for_clip(idx) {
                if let Some(ref ck) = slot.alpha_chroma_key {
                    let r = ((color >> 16) & 0xFF) as u32;
                    let g = ((color >> 8) & 0xFF) as u32;
                    let b = (color & 0xFF) as u32;
                    ck.set_property("target-r", r);
                    ck.set_property("target-g", g);
                    ck.set_property("target-b", b);
                    ck.set_property("angle", (tolerance * 90.0).clamp(0.0, 90.0));
                    ck.set_property("noise-level", (softness * 64.0).clamp(0.0, 64.0));
                    // If the alpha element is in the pipeline but now disabled, we
                    // cannot remove it without a rebuild — but the clip model
                    // controls whether build_effects_bin creates it, so a full
                    // rebuild (on_project_changed) handles enable/disable.  For
                    // live slider updates while already enabled, just update props.
                    let _ = enabled; // rebuild handles toggle; see on_project_changed
                }
            }
        }
        // Force frame redraw when paused.  Stills use the lighter compositor
        // flush instead of `reseek_slot_for_current` (which would re-seek the
        // imagefreeze decoder racily — see `flush_compositor_for_still_refresh`).
        if self.current_idx.is_some() && self.state != PlayerState::Playing {
            if is_static_image {
                self.flush_compositor_for_still_refresh();
            } else {
                self.reseek_slot_for_current();
            }
        }
    }

    /// Live-update frei0r effect parameters on the current slot.
    ///
    /// If the effect topology changed (add/remove/reorder/toggle), syncs the
    /// clip model and triggers a full pipeline rebuild. Otherwise updates
    /// GStreamer element properties in-place for zero-latency slider feedback.
    ///
    /// Returns `true` if the caller should schedule a paused-frame refresh
    /// (via `reseek_paused`). The reseek is NOT done internally so that the
    /// caller can debounce rapid slider changes and avoid blocking the GTK
    /// main loop inside a signal handler — some frei0r plugins (cairogradient)
    /// crash when a flush-seek is issued synchronously from a property-change
    /// callback.
    pub fn update_frei0r_effects(&mut self, effects: &[crate::model::clip::Frei0rEffect]) -> bool {
        let clip_idx = match self.current_idx {
            Some(i) => i,
            None => return false,
        };

        // For adjustment layers, update the clip's frei0r list and rebuild the
        // post-compositor effects chain.
        if clip_idx < self.clips.len() && self.clips[clip_idx].is_adjustment {
            self.clips[clip_idx].frei0r_effects = effects.to_vec();
            // Frei0r user effects on adjustment layers are applied on export only;
            // the preview path uses permanent videobalance/coloradj/3point elements.
            return true; // Signal caller to reseek
        }

        // Check topology: does the slot already have matching elements?
        let topology_matches = if let Some(slot) = self.slot_for_clip(clip_idx) {
            let enabled: Vec<&str> = effects
                .iter()
                .filter(|e| e.enabled)
                .map(|e| e.plugin_name.as_str())
                .collect();
            slot.frei0r_user_effects.len() == enabled.len()
                && slot
                    .frei0r_user_effects
                    .iter()
                    .zip(enabled.iter())
                    .all(|(elem, &name)| {
                        let factory_name = elem
                            .factory()
                            .map(|f| f.name().to_string())
                            .unwrap_or_default();
                        factory_name == format!("frei0r-filter-{}", name)
                    })
        } else {
            false
        };

        // Detect string-param changes BEFORE syncing, because some frei0r
        // plugins (cairogradient) SIGSEGV in f0r_set_param_value → strlen
        // when a string property is re-set on a live element. If any string
        // param changed we force a full pipeline rebuild (which creates a
        // fresh element with the new values baked in) instead of live update.
        let string_params_changed = topology_matches
            && self.clips.get(clip_idx).map_or(false, |old_clip| {
                let old = &old_clip.frei0r_effects;
                old.len() != effects.len()
                    || old
                        .iter()
                        .zip(effects.iter())
                        .any(|(o, n)| o.string_params != n.string_params)
            });

        // Always sync the clip model.
        if let Some(clip) = self.clips.get_mut(clip_idx) {
            clip.frei0r_effects = effects.to_vec();
        }

        if !topology_matches || string_params_changed {
            let pos = self.timeline_pos_ns;
            self.rebuild_pipeline_at(pos);
            return false; // rebuild already reseeks internally
        }

        // Live param update — ONLY numeric params.  String params are baked
        // into the element during pipeline construction; changing them triggers
        // a rebuild above.  Skipping them here avoids a SIGSEGV in
        // f0r_set_param_value for plugins whose C code crashes when an
        // unchanged string property is re-set on a running element.
        if let Some(slot) = self.slot_for_clip(clip_idx) {
            let enabled: Vec<&crate::model::clip::Frei0rEffect> =
                effects.iter().filter(|e| e.enabled).collect();
            for (elem, effect) in slot.frei0r_user_effects.iter().zip(enabled.iter()) {
                for (param, &val) in &effect.params {
                    if elem.has_property(param) {
                        set_frei0r_property(elem, param, val);
                    }
                }
            }
        }

        // Tell caller a paused-frame refresh is needed (but don't block here).
        self.current_idx.is_some() && self.state != PlayerState::Playing
    }

    #[allow(dead_code)]
    pub fn update_current_audio(&mut self, volume: f64, pan: f64) {
        if let Some(idx) = self.current_idx {
            if let Some(clip) = self.clips.get_mut(idx) {
                clip.volume = volume.clamp(0.0, MAX_PREVIEW_AUDIO_GAIN);
                clip.pan = pan.clamp(-1.0, 1.0);
            }
        }
        self.sync_preview_audio_levels(self.timeline_pos_ns);
    }

    pub fn update_audio_for_clip(
        &mut self,
        clip_id: &str,
        volume: f64,
        pan: f64,
        voice_isolation: f64,
    ) {
        let volume = volume.clamp(0.0, MAX_PREVIEW_AUDIO_GAIN);
        let pan = pan.clamp(-1.0, 1.0);
        let mut video_found = false;
        // Check video clips first (use audiomixer pad on compositor pipeline).
        for clip in self.clips.iter_mut().filter(|c| c.id == clip_id) {
            clip.volume = volume;
            clip.pan = pan;
            clip.voice_isolation = voice_isolation;
            video_found = true;
        }
        if video_found {
            self.apply_main_audio_slot_volumes(self.timeline_pos_ns);
        }
        // For audio-only clips, update the stored volume and, if actively playing,
        // update the dedicated volume element (avoids playbin StreamVolume crosstalk).
        // Collect matching indices first to avoid holding iter_mut borrow across &self calls.
        let matched_indices: Vec<usize> = self
            .audio_clips
            .iter()
            .enumerate()
            .filter(|(_, c)| c.id == clip_id)
            .map(|(i, _)| i)
            .collect();
        for i in matched_indices {
            self.audio_clips[i].volume = volume;
            self.audio_clips[i].pan = pan;
            self.audio_clips[i].voice_isolation = voice_isolation;
            if self.audio_current_source == Some(AudioCurrentSource::AudioClip(i)) {
                let effective = self.effective_audio_source_volume(
                    AudioCurrentSource::AudioClip(i),
                    self.timeline_pos_ns,
                );
                self.set_audio_pipeline_volume(effective);
                let effective_pan = self.effective_audio_source_pan(
                    AudioCurrentSource::AudioClip(i),
                    self.timeline_pos_ns,
                );
                self.set_audio_pipeline_pan(effective_pan);
            }
            // Live-update multi-audio pipeline mixer pads if active.
            if let Some(pad) = self.audio_multi_pads.get(&i) {
                let effective_vol = if self.master_muted {
                    0.0
                } else {
                    volume.clamp(0.0, 4.0)
                };
                pad.set_property("volume", effective_vol);
            }
            if let Some(pan_elem) = self.audio_multi_pan_elems.get(&i) {
                pan_elem.set_property("panorama", pan.clamp(-1.0, 1.0) as f32);
            }
        }
    }

    /// Update EQ band parameters for a specific clip (called from Inspector/MCP).
    pub fn update_eq_for_clip(&mut self, clip_id: &str, eq_bands: [crate::model::clip::EqBand; 3]) {
        // Update stored data on ProgramClip.
        if let Some(i) = self.clips.iter().position(|c| c.id == clip_id) {
            self.clips[i].eq_bands = eq_bands;
            // Sync live GStreamer element.
            let slot_found = self
                .slots
                .iter()
                .any(|s| s.clip_idx == i && !s.is_prerender_slot);
            if let Some(slot) = self
                .slots
                .iter()
                .find(|s| s.clip_idx == i && !s.is_prerender_slot)
            {
                if let Some(ref eq_elem) = slot.audio_equalizer {
                    for bi in 0..3u32 {
                        let b = &eq_bands[bi as usize];
                        eq_set_band(eq_elem, bi, b.freq, b.gain, b.q);
                    }
                    log::info!(
                        "update_eq_for_clip: set EQ on clip={} gains=[{:.1}, {:.1}, {:.1}]",
                        clip_id,
                        eq_bands[0].gain,
                        eq_bands[1].gain,
                        eq_bands[2].gain,
                    );
                } else {
                    log::warn!(
                        "update_eq_for_clip: slot found for clip={} but audio_equalizer is None",
                        clip_id,
                    );
                }
            } else {
                log::warn!(
                    "update_eq_for_clip: no slot found for clip={} (clip_idx={}, slots={}, slot_found={})",
                    clip_id, i, self.slots.len(), slot_found,
                );
            }
            return;
        }
        // Audio-only clips use the multi-audio pipeline.
        if let Some(i) = self.audio_clips.iter().position(|c| c.id == clip_id) {
            self.audio_clips[i].eq_bands = eq_bands;
            // If this audio clip is the currently playing source via playbin, update live.
            if self.audio_current_source == Some(AudioCurrentSource::AudioClip(i)) {
                self.set_audio_pipeline_eq(&eq_bands);
            }
            // If playing via multi pipeline, rebuild it to pick up new EQ settings.
            if self.audio_multi_pipeline.is_some() && self.audio_multi_active.contains(&i) {
                let active = self.audio_multi_active.clone();
                let pos = self.timeline_pos_ns;
                self.rebuild_audio_multi_pipeline(&active, pos);
            }
            log::info!(
                "update_eq_for_clip: stored EQ for audio-only clip={}",
                clip_id
            );
        } else {
            log::warn!(
                "update_eq_for_clip: clip={} not found in clips or audio_clips",
                clip_id
            );
        }
    }

    /// Update 7-band match EQ for a specific clip (called from Inspector/MCP).
    /// When the band count matches an existing live element, parameters are updated
    /// in-place without a rebuild. Otherwise the slot is rebuilt at the current
    /// playhead so the new band count takes effect (since `equalizer-nbands`
    /// `num-bands` is construct-only).
    pub fn update_match_eq_for_clip(
        &mut self,
        clip_id: &str,
        match_eq_bands: Vec<crate::model::clip::EqBand>,
    ) {
        let mut needs_rebuild = false;
        if let Some(i) = self.clips.iter().position(|c| c.id == clip_id) {
            let old_count = self.clips[i].match_eq_bands.len();
            self.clips[i].match_eq_bands = match_eq_bands.clone();
            // Try in-place update if a live element exists with matching band count.
            let mut updated_in_place = false;
            if let Some(slot) = self
                .slots
                .iter()
                .find(|s| s.clip_idx == i && !s.is_prerender_slot)
            {
                if let Some(ref m_eq) = slot.audio_match_equalizer {
                    if match_eq_bands.len() == old_count && !match_eq_bands.is_empty() {
                        for (bi, band) in match_eq_bands.iter().enumerate() {
                            eq_set_band(m_eq, bi as u32, band.freq, band.gain, band.q);
                        }
                        updated_in_place = true;
                        log::info!(
                            "update_match_eq_for_clip: in-place update for clip={}",
                            clip_id
                        );
                    }
                }
            }
            if !updated_in_place && match_eq_bands.len() != old_count {
                needs_rebuild = true;
            }
        }
        if let Some(i) = self.audio_clips.iter().position(|c| c.id == clip_id) {
            self.audio_clips[i].match_eq_bands = match_eq_bands.clone();
            // Force a multi-audio pipeline rebuild so the new match EQ takes effect.
            // teardown first so the should-rebuild guard inside rebuild_audio_multi_pipeline
            // sees `pipeline_exists == false` and proceeds with the rebuild.
            if self.audio_multi_pipeline.is_some() && self.audio_multi_active.contains(&i) {
                let active = self.audio_multi_active.clone();
                let pos = self.timeline_pos_ns;
                self.teardown_audio_multi_pipeline();
                self.rebuild_audio_multi_pipeline(&active, pos);
            }
        }
        if needs_rebuild {
            let pos = self.timeline_pos_ns;
            log::info!(
                "update_match_eq_for_clip: rebuilding pipeline for clip={} at pos={}ns",
                clip_id,
                pos
            );
            self.rebuild_pipeline_at(pos);
        }
    }

    pub fn set_transform(
        &self,
        crop_left: i32,
        crop_right: i32,
        crop_top: i32,
        crop_bottom: i32,
        rotate: i32,
        flip_h: bool,
        flip_v: bool,
        scale: f64,
        position_x: f64,
        position_y: f64,
    ) {
        if let Some(slot) = self.current_idx.and_then(|idx| self.slot_for_clip(idx)) {
            Self::apply_transform_to_slot(
                slot,
                crop_left,
                crop_right,
                crop_top,
                crop_bottom,
                rotate,
                flip_h,
                flip_v,
            );
            if let Some(ref pad) = slot.compositor_pad {
                let (proc_w, proc_h) = self.preview_processing_dimensions();
                let direct_translation = self
                    .current_idx
                    .and_then(|idx| self.clips.get(idx))
                    .map(Self::clip_uses_direct_canvas_translation)
                    .unwrap_or(false);
                Self::apply_zoom_to_slot(
                    slot,
                    pad,
                    scale,
                    position_x,
                    position_y,
                    direct_translation,
                    proc_w,
                    proc_h,
                );
            }
        }
    }

    pub fn set_title(&self, text: &str, font: &str, color_rgba: u32, rel_x: f64, rel_y: f64) {
        if let Some(slot) = self.current_idx.and_then(|idx| self.slot_for_clip(idx)) {
            let (pw, ph) = self.preview_processing_dimensions();
            Self::apply_title_to_slot(
                slot,
                text,
                font,
                color_rgba,
                rel_x,
                rel_y,
                self.project_height,
                pw,
                ph,
            );
        }
    }

    pub fn update_current_title(
        &self,
        text: &str,
        font: &str,
        color_rgba: u32,
        rel_x: f64,
        rel_y: f64,
    ) {
        self.set_title(text, font, color_rgba, rel_x, rel_y);
        // Don't flush here — caller schedules a debounced compositor flush
        // so rapid keystrokes / slider drags don't block the GTK main thread.
    }

    /// Update title on the slot matching `clip_id` (not just current_idx).
    /// This is needed when the selected clip differs from the highest-priority
    /// clip at the playhead (e.g. editing a lower-track clip's title overlay
    /// while a higher-track clip is the "current" composited clip).
    pub fn update_title_for_clip(
        &self,
        clip_id: &str,
        text: &str,
        font: &str,
        color_rgba: u32,
        rel_x: f64,
        rel_y: f64,
    ) {
        let idx = self.clips.iter().position(|c| c.id == clip_id);
        if let Some(slot) = idx.and_then(|i| self.slot_for_clip(i)) {
            let (pw, ph) = self.preview_processing_dimensions();
            Self::apply_title_to_slot(
                slot,
                text,
                font,
                color_rgba,
                rel_x,
                rel_y,
                self.project_height,
                pw,
                ph,
            );
        }
    }

    /// Update title style on the slot matching `clip_id`.
    pub fn update_title_style_for_clip(
        &self,
        clip_id: &str,
        outline_width: f64,
        outline_color: u32,
        shadow: bool,
        bg_box: bool,
    ) {
        let idx = self.clips.iter().position(|c| c.id == clip_id);
        if let Some(slot) = idx.and_then(|i| self.slot_for_clip(i)) {
            if let Some(ref to) = slot.textoverlay {
                to.set_property("draw-outline", outline_width > 0.0);
                if outline_width > 0.0 {
                    let (r, g, b, a) = crate::ui::colors::rgba_u32_to_u8(outline_color);
                    let argb: u32 =
                        ((a as u32) << 24) | ((r as u32) << 16) | ((g as u32) << 8) | b as u32;
                    to.set_property("outline-color", argb);
                }
                to.set_property("draw-shadow", shadow);
                to.set_property("shaded-background", bg_box);
            }
        }
    }

    /// Apply extended title styling (outline, shadow, bg box) from individual
    /// fields to the current slot's textoverlay element.  No flush here —
    /// caller schedules a debounced compositor flush.
    pub fn update_current_title_style(
        &self,
        outline_width: f64,
        outline_color: u32,
        shadow: bool,
        bg_box: bool,
    ) {
        if let Some(slot) = self.current_idx.and_then(|idx| self.slot_for_clip(idx)) {
            if let Some(ref to) = slot.textoverlay {
                to.set_property("draw-outline", outline_width > 0.0);
                if outline_width > 0.0 {
                    let (r, g, b, a) = crate::ui::colors::rgba_u32_to_u8(outline_color);
                    let argb: u32 =
                        ((a as u32) << 24) | ((r as u32) << 16) | ((g as u32) << 8) | b as u32;
                    to.set_property("outline-color", argb);
                }
                to.set_property("draw-shadow", shadow);
                to.set_property("shaded-background", bg_box);
            }
        }
        // Don't flush here — caller schedules a debounced compositor flush.
    }

    /// Apply procedural title animations (Typewriter / Fade / Pop) to every
    /// slot whose clip has a non-None `title_animation`. Called from the
    /// ~30 FPS program-monitor tick with the current timeline position.
    ///
    /// Typewriter: sets `textoverlay.text` to a prefix of `title_text` based
    ///   on progress (0..=1 over `title_animation_duration_ns`).
    /// Fade: scales the compositor pad's alpha from 0→1. Combines
    ///   multiplicatively with any clip-level opacity already applied.
    /// Pop: scales the compositor pad's width/height from 0→native around
    ///   the pad's current centre.
    pub fn apply_title_animations(&self, timeline_pos_ns: u64) {
        use crate::model::clip::TitleAnimation;
        if self.clips.is_empty() || self.slots.is_empty() {
            return;
        }
        for (clip_idx, clip) in self.clips.iter().enumerate() {
            if !clip.is_title {
                continue;
            }
            if matches!(clip.title_animation, TitleAnimation::None) {
                continue;
            }
            // Clip-local time (ns since the clip's timeline_start).
            let local_ns = timeline_pos_ns.saturating_sub(clip.timeline_start_ns);
            let clip_end = clip.timeline_start_ns.saturating_add(
                clip.source_out_ns.saturating_sub(clip.source_in_ns),
            );
            if timeline_pos_ns < clip.timeline_start_ns || timeline_pos_ns >= clip_end {
                continue;
            }
            let progress = crate::media::drawing_render::animation_progress(
                local_ns,
                clip.title_animation_duration_ns,
            );
            let Some(slot) = self.slot_for_clip(clip_idx) else {
                continue;
            };
            match clip.title_animation {
                TitleAnimation::None => {}
                TitleAnimation::Typewriter => {
                    if let Some(ref to) = slot.textoverlay {
                        let visible =
                            crate::media::drawing_render::typewriter_visible_chars(
                                &clip.title_text,
                                progress,
                            );
                        let prefix: String = clip.title_text.chars().take(visible).collect();
                        to.set_property("silent", prefix.is_empty());
                        to.set_property("text", &prefix);
                    }
                }
                TitleAnimation::Fade => {
                    if let Some(ref pad) = slot.compositor_pad {
                        // Compose with clip-level opacity so keyframed alpha
                        // and procedural fade multiply.
                        let base = clip.opacity_at_timeline_ns(timeline_pos_ns);
                        pad.set_property("alpha", base * progress);
                    }
                }
                TitleAnimation::Pop => {
                    if let Some(ref pad) = slot.compositor_pad {
                        if pad.find_property("width").is_some()
                            && pad.find_property("height").is_some()
                        {
                            let scale_prog = progress.max(0.01);
                            let base_scale = clip.scale_at_timeline_ns(timeline_pos_ns);
                            let effective = base_scale * scale_prog;
                            let pw = self.project_width as f64;
                            let ph = self.project_height as f64;
                            let sw = (pw * effective).round().max(1.0) as i32;
                            let sh = (ph * effective).round().max(1.0) as i32;
                            pad.set_property("width", sw);
                            pad.set_property("height", sh);
                            if pad.find_property("xpos").is_some()
                                && pad.find_property("ypos").is_some()
                            {
                                let xpos = ((pw - sw as f64) * 0.5).round() as i32;
                                let ypos = ((ph - sh as f64) * 0.5).round() as i32;
                                pad.set_property("xpos", xpos);
                                pad.set_property("ypos", ypos);
                            }
                        }
                    }
                }
            }
        }
    }

    /// Fire-and-forget compositor flush.  Sends a FLUSH seek so the
    /// compositor re-aggregates a frame using each pad's currently-cached
    /// buffer, picking up any pad property updates (transform/crop, title
    /// text/style, …) without involving upstream decoders.  Does NOT block
    /// — the new frame arrives asynchronously via the gtk4paintablesink.
    /// Much cheaper than `reseek_slot_for_current()` because the decoders
    /// are already at the correct position.  Returns silently if there are
    /// no slots or the pipeline is playing (live frames are already flowing).
    fn flush_compositor_for_property_update(&self) {
        if self.slots.is_empty() {
            return;
        }
        if self.state == PlayerState::Playing {
            // During playback, new frames flow continuously — no flush needed.
            return;
        }
        let _ = self
            .compositor
            .seek_simple(gst::SeekFlags::FLUSH, gst::ClockTime::ZERO);
    }

    /// Public entry point used by title text/style updates.  See
    /// `flush_compositor_for_property_update`.
    pub fn flush_compositor_for_title_update(&self) {
        self.flush_compositor_for_property_update();
    }

    /// Public entry point used by still-image transform/crop edits.
    ///
    /// A FLUSH seek on the compositor clears every sink pad's queued
    /// buffer.  For a *bare* still that would be enough — the still's
    /// `imagefreeze` element keeps a cached buffer and re-emits it on
    /// the next aggregate cycle.  But the same flush also strips the
    /// underlying video tracks of their parked frame, and those
    /// decoders are paused so they will not push a fresh buffer until
    /// somebody explicitly seeks them.  If we just flush, the next
    /// aggregate runs against empty video pads → underlying video
    /// blanks until the user nudges the playhead.
    ///
    /// So: flush the compositor, then re-seek every *non-still*
    /// decoder back to the current playhead so the underlying video
    /// tracks re-prerolll and push fresh frames.  The still's own
    /// decoder is intentionally **not** re-seeked — it has already
    /// EOSed into imagefreeze, and a re-seek there would either fail
    /// silently or race with the imagefreeze src loop (which is the
    /// race the original `reseek_slot_by_clip_idx` was hitting).
    /// Other stills on the timeline are skipped for the same reason.
    ///
    /// This is fire-and-forget: there is **no** blocking
    /// `wait_for_compositor_arrivals` call, so the GTK drag handler
    /// returns immediately and the new aggregate cycle picks up the
    /// fresh frames asynchronously via the gtk4paintablesink.
    pub fn flush_compositor_for_still_refresh(&self) {
        if self.slots.is_empty() {
            return;
        }
        if self.state == PlayerState::Playing {
            return;
        }
        let _ = self
            .compositor
            .seek_simple(gst::SeekFlags::FLUSH, gst::ClockTime::ZERO);
        for slot in &self.slots {
            let clip = match self.clips.get(slot.clip_idx) {
                Some(c) => c,
                None => continue,
            };
            // Skip stills (the source decoder is parked at EOS into
            // imagefreeze; the imagefreeze cache survives the flush
            // and re-emits on the next aggregate cycle).
            if clip.is_image && !clip.animated_svg {
                continue;
            }
            let _ = Self::seek_slot_decoder_paused_with_retry(slot, clip, self.timeline_pos_ns);
        }
    }

    pub fn update_current_transform(
        &mut self,
        crop_left: i32,
        crop_right: i32,
        crop_top: i32,
        crop_bottom: i32,
        rotate: i32,
        flip_h: bool,
        flip_v: bool,
        scale: f64,
        position_x: f64,
        position_y: f64,
    ) {
        let mut is_adjustment = false;
        if let Some(idx) = self.current_idx {
            if let Some(clip) = self.clips.get_mut(idx) {
                is_adjustment = clip.is_adjustment;
                clip.crop_left = crop_left;
                clip.crop_right = crop_right;
                clip.crop_top = crop_top;
                clip.crop_bottom = crop_bottom;
                clip.rotate = rotate;
                clip.flip_h = flip_h;
                clip.flip_v = flip_v;
                clip.scale = scale;
                clip.position_x = position_x;
                clip.position_y = position_y;
            }
        }
        if is_adjustment {
            self.rebuild_adjustment_overlays();
            if self.state != PlayerState::Playing {
                self.reseek_slot_for_current();
            }
            return;
        }
        self.set_transform(
            crop_left,
            crop_right,
            crop_top,
            crop_bottom,
            rotate,
            flip_h,
            flip_v,
            scale,
            position_x,
            position_y,
        );
        if self.current_idx.is_some() && self.state != PlayerState::Playing {
            self.reseek_slot_for_current();
        }
    }

    pub fn update_transform_for_clip(
        &mut self,
        clip_id: &str,
        crop_left: i32,
        crop_right: i32,
        crop_top: i32,
        crop_bottom: i32,
        rotate: i32,
        flip_h: bool,
        flip_v: bool,
        scale: f64,
        position_x: f64,
        position_y: f64,
    ) {
        let idx = self.clips.iter().position(|c| c.id == clip_id);
        if let Some(i) = idx {
            let mut is_adjustment = false;
            let mut direct_translation = false;
            let mut is_static_image = false;
            if let Some(clip) = self.clips.get_mut(i) {
                is_adjustment = clip.is_adjustment;
                clip.crop_left = crop_left;
                clip.crop_right = crop_right;
                clip.crop_top = crop_top;
                clip.crop_bottom = crop_bottom;
                clip.rotate = rotate;
                clip.flip_h = flip_h;
                clip.flip_v = flip_v;
                clip.scale = scale;
                clip.position_x = position_x;
                clip.position_y = position_y;
                direct_translation = Self::clip_uses_direct_canvas_translation(clip);
                is_static_image = Self::clip_requires_live_transform_refresh(clip);
            }
            if is_adjustment {
                self.rebuild_adjustment_overlays();
                if self.state != PlayerState::Playing {
                    self.reseek_slot_by_clip_idx(i);
                }
                return;
            }
            // Apply to the slot for this clip (any track, not just top).
            if let Some(slot) = self.slot_for_clip(i) {
                Self::apply_transform_to_slot(
                    slot,
                    crop_left,
                    crop_right,
                    crop_top,
                    crop_bottom,
                    rotate,
                    flip_h,
                    flip_v,
                );
                if let Some(ref pad) = slot.compositor_pad {
                    let (proc_w, proc_h) = self.preview_processing_dimensions();
                    Self::apply_zoom_to_slot(
                        slot,
                        pad,
                        scale,
                        position_x,
                        position_y,
                        direct_translation,
                        proc_w,
                        proc_h,
                    );
                }
            }
            if self.state != PlayerState::Playing {
                if is_static_image {
                    // Stills: never per-decoder reseek.  The transform was
                    // applied via compositor pad properties (xpos/ypos/
                    // width/height) and the alpha-crop pad probe — none of
                    // which need fresh upstream data.  A non-blocking
                    // compositor flush is enough to make the next aggregate
                    // cycle pick up the new properties using the buffer
                    // imagefreeze already has parked on its src pad.
                    self.flush_compositor_for_still_refresh();
                } else {
                    self.reseek_slot_by_clip_idx(i);
                }
            }
        }
    }

    /// Update transform properties on GStreamer elements without triggering a
    /// blocking reseek.  Used during interactive drag so the GTK main thread
    /// is never blocked.  The caller should call `reseek_paused()` or
    /// `exit_transform_live_mode()` when the drag ends.
    pub fn set_transform_properties_only(
        &mut self,
        clip_id: Option<&str>,
        crop_left: i32,
        crop_right: i32,
        crop_top: i32,
        crop_bottom: i32,
        rotate: i32,
        flip_h: bool,
        flip_v: bool,
        scale: f64,
        position_x: f64,
        position_y: f64,
    ) {
        let idx = match clip_id {
            Some(id) => self.clips.iter().position(|c| c.id == id),
            None => self.current_idx,
        };
        if let Some(i) = idx {
            let mut is_adjustment = false;
            let mut direct_translation = false;
            let mut needs_live_refresh = false;
            if let Some(clip) = self.clips.get_mut(i) {
                is_adjustment = clip.is_adjustment;
                clip.crop_left = crop_left;
                clip.crop_right = crop_right;
                clip.crop_top = crop_top;
                clip.crop_bottom = crop_bottom;
                clip.rotate = rotate;
                clip.flip_h = flip_h;
                clip.flip_v = flip_v;
                clip.scale = scale;
                clip.position_x = position_x;
                clip.position_y = position_y;
                direct_translation = Self::clip_uses_direct_canvas_translation(clip);
                needs_live_refresh = Self::clip_requires_live_transform_refresh(clip);
            }
            if is_adjustment {
                self.rebuild_adjustment_overlays();
                return;
            }
            if let Some(slot) = self.slot_for_clip(i) {
                Self::apply_transform_to_slot(
                    slot,
                    crop_left,
                    crop_right,
                    crop_top,
                    crop_bottom,
                    rotate,
                    flip_h,
                    flip_v,
                );
                if let Some(ref pad) = slot.compositor_pad {
                    let (proc_w, proc_h) = self.preview_processing_dimensions();
                    Self::apply_zoom_to_slot(
                        slot,
                        pad,
                        scale,
                        position_x,
                        position_y,
                        direct_translation,
                        proc_w,
                        proc_h,
                    );
                }
            }
            if needs_live_refresh && self.state != PlayerState::Playing {
                // `needs_live_refresh` is `clip.is_image && !clip.animated_svg`,
                // so this branch is the still-image path.  Use the lightweight
                // compositor flush instead of `reseek_slot_by_clip_idx`: the
                // pad properties just changed, the buffer is parked on
                // imagefreeze, and a per-decoder reseek would block on
                // `wait_for_compositor_arrivals` and race with the imagefreeze
                // src loop.
                self.flush_compositor_for_still_refresh();
            }
        }
    }

    /// Switch the pipeline into live mode for interactive transform preview.
    ///
    /// Sets `is-live=true` on the background source so the compositor enters
    /// live aggregation (clock-paced ~30fps output), makes the display queue
    /// leaky so the compositor never blocks on backpressure, and keeps the
    /// pipeline paused so transform interaction does not advance playback.
    ///
    /// Call `exit_transform_live_mode()` when the drag ends.
    pub fn enter_transform_live_mode(&mut self) {
        if self.transform_live {
            return;
        }
        if self.state == PlayerState::Playing {
            self.pause();
        }
        log::info!("enter_transform_live_mode: slots={}", self.slots.len());
        self.background_src.set_property("is-live", true);
        if let Some(ref q) = self.display_queue {
            q.set_property_from_str("leaky", "downstream");
            q.set_property("max-size-buffers", 2u32);
        }
        // Mute audio output without blocking the audio pipeline.
        // Using set_locked_state blocks the audiomixer's downstream push,
        // which can prevent some decoders (especially audio-less clips) from
        // delivering video frames to the compositor.
        if self.audio_sink.find_property("mute").is_some() {
            self.audio_sink.set_property("mute", true);
        } else {
            self.audio_sink.set_locked_state(true);
        }
        let _ = self.pipeline.set_state(gst::State::Paused);
        self.transform_live = true;
        self.update_slot_queue_policy();
    }

    /// Exit live transform mode and restore the pipeline to normal paused state.
    /// Does a final reseek so the displayed frame accurately reflects the
    /// last transform parameters.
    pub fn exit_transform_live_mode(&mut self) {
        if !self.transform_live {
            return;
        }
        log::info!("exit_transform_live_mode");
        let _ = self.pipeline.set_state(gst::State::Paused);
        // Restore audio output.
        if self.audio_sink.find_property("mute").is_some() {
            self.audio_sink.set_property("mute", false);
        } else if self.audio_sink.is_locked_state() {
            self.audio_sink.set_locked_state(false);
            let _ = self.audio_sink.sync_state_with_parent();
        }
        self.background_src.set_property("is-live", false);
        if let Some(ref q) = self.display_queue {
            q.set_property_from_str("leaky", "no");
            q.set_property("max-size-buffers", 3u32);
        }
        self.transform_live = false;
        self.update_slot_queue_policy();
        self.reseek_slot_for_current();
    }

    pub fn update_current_opacity(&mut self, opacity: f64) {
        let opacity = opacity.clamp(0.0, 1.0);
        if let Some(idx) = self.current_idx {
            if let Some(clip) = self.clips.get_mut(idx) {
                clip.opacity = opacity;
                if clip.is_adjustment {
                    self.rebuild_adjustment_overlays();
                    self.rebuild_adjustment_effects_chain();
                    if self.state != PlayerState::Playing {
                        self.reseek_slot_for_current();
                    }
                    return;
                }
            }
            // Update compositor pad alpha for this slot.
            if let Some(slot) = self.slot_for_clip(idx) {
                if slot.is_blend_mode {
                    if let Ok(mut a) = slot.blend_alpha.lock() {
                        *a = opacity;
                    }
                } else if let Some(ref pad) = slot.compositor_pad {
                    pad.set_property("alpha", opacity);
                }
            }
        }
        if self.current_idx.is_some() && self.state != PlayerState::Playing {
            self.reseek_slot_for_current();
        }
    }

    pub fn update_opacity_for_clip(&mut self, clip_id: &str, opacity: f64) {
        let opacity = opacity.clamp(0.0, 1.0);
        if let Some(i) = self.clips.iter().position(|c| c.id == clip_id) {
            let mut is_adjustment = false;
            if let Some(clip) = self.clips.get_mut(i) {
                is_adjustment = clip.is_adjustment;
                clip.opacity = opacity;
            }
            if is_adjustment {
                self.rebuild_adjustment_overlays();
                if self.current_idx == Some(i) && self.state != PlayerState::Playing {
                    self.reseek_slot_for_current();
                }
                return;
            }
            if let Some(slot) = self.slot_for_clip(i) {
                if slot.is_blend_mode {
                    if let Ok(mut a) = slot.blend_alpha.lock() {
                        *a = opacity;
                    }
                } else if let Some(ref pad) = slot.compositor_pad {
                    pad.set_property("alpha", opacity);
                }
            }
            if self.current_idx == Some(i) && self.state != PlayerState::Playing {
                self.reseek_slot_for_current();
            }
        }
    }

    /// Update speed keyframes on a clip without a full pipeline rebuild.
    /// Updates source_out to match the new speed curve.
    ///
    /// No decoder reseek is performed here — the flush seek required to
    /// refresh the preview frame races with qtdemux's streaming thread and
    /// causes a SIGSEGV (NULL stream dereference in `gst_qtdemux_push_buffer`).
    /// The updated mapping takes effect on the next natural seek (playhead
    /// scrub, play/pause, or timeline redraw).
    pub fn update_speed_keyframes_for_clip(
        &mut self,
        clip_id: &str,
        speed: f64,
        speed_keyframes: Vec<crate::model::clip::NumericKeyframe>,
    ) {
        let find_clip = |clips: &mut Vec<ProgramClip>| -> Option<usize> {
            clips.iter().position(|c| c.id == clip_id)
        };
        if let Some(i) = find_clip(&mut self.clips) {
            let clip = &mut self.clips[i];
            clip.speed = speed;
            clip.speed_keyframes = speed_keyframes.clone();
            // Recalculate source_out based on new speed curve.
            let timeline_dur_ns = clip.duration_ns();
            let source_dur = clip.integrated_source_distance_for_local_timeline_ns(timeline_dur_ns);
            clip.source_out_ns = clip.source_in_ns.saturating_add(source_dur as u64);
            // No reseek — see doc comment above.
        }
        if let Some(i) = self.audio_clips.iter().position(|c| c.id == clip_id) {
            let clip = &mut self.audio_clips[i];
            clip.speed = speed;
            clip.speed_keyframes = speed_keyframes;
        }
    }

    // ── Private helpers ────────────────────────────────────────────────────

    fn clip_has_keyframed_masks(clip: &ProgramClip) -> bool {
        clip.masks.iter().any(|mask| {
            !mask.center_x_keyframes.is_empty()
                || !mask.center_y_keyframes.is_empty()
                || !mask.width_keyframes.is_empty()
                || !mask.height_keyframes.is_empty()
                || !mask.rotation_keyframes.is_empty()
                || !mask.feather_keyframes.is_empty()
                || !mask.expansion_keyframes.is_empty()
        })
    }

    fn clip_has_phase1_keyframes(clip: &ProgramClip) -> bool {
        !clip.scale_keyframes.is_empty()
            || !clip.opacity_keyframes.is_empty()
            || !clip.brightness_keyframes.is_empty()
            || !clip.contrast_keyframes.is_empty()
            || !clip.saturation_keyframes.is_empty()
            || !clip.temperature_keyframes.is_empty()
            || !clip.tint_keyframes.is_empty()
            || !clip.blur_keyframes.is_empty()
            || !clip.position_x_keyframes.is_empty()
            || !clip.position_y_keyframes.is_empty()
            || !clip.volume_keyframes.is_empty()
            || !clip.pan_keyframes.is_empty()
            || !clip.eq_low_gain_keyframes.is_empty()
            || !clip.eq_mid_gain_keyframes.is_empty()
            || !clip.eq_high_gain_keyframes.is_empty()
            || !clip.speed_keyframes.is_empty()
            || !clip.rotate_keyframes.is_empty()
            || !clip.crop_left_keyframes.is_empty()
            || !clip.crop_right_keyframes.is_empty()
            || !clip.crop_top_keyframes.is_empty()
            || !clip.crop_bottom_keyframes.is_empty()
            || Self::clip_has_keyframed_masks(clip)
    }

    fn clip_has_unsupported_background_prerender_keyframes(clip: &ProgramClip) -> bool {
        // Audio-side keyframes are not honored by prerender (audio is baked
        // at current levels at job time and not invalidated by audio edits).
        let unsupported_audio = !clip.volume_keyframes.is_empty()
            || !clip.pan_keyframes.is_empty()
            || !clip.eq_low_gain_keyframes.is_empty()
            || !clip.eq_mid_gain_keyframes.is_empty()
            || !clip.eq_high_gain_keyframes.is_empty();
        // Speed keyframes are not yet plumbed through the prerender pre-chain.
        let unsupported_speed = !clip.speed_keyframes.is_empty();
        // Creative blur lane (gaussian blur strength keyframes) is not yet
        // emitted as a per-frame expression in prerender.
        let unsupported_blur = !clip.blur_keyframes.is_empty();
        // Animated masks are not yet supported in prerender.
        let unsupported_mask_keyframes = Self::clip_has_keyframed_masks(clip);
        // Transform keyframes (scale, position, rotate, crop, opacity) ARE
        // supported by the new keyframed-overlay chain — except when the
        // clip also has any non-animated mask, since the mask + per-frame
        // overlay interaction is not yet handled. In that case the clip
        // falls back to the live (non-prerendered) path.
        let has_transform_keyframes = !clip.scale_keyframes.is_empty()
            || !clip.opacity_keyframes.is_empty()
            || !clip.position_x_keyframes.is_empty()
            || !clip.position_y_keyframes.is_empty()
            || !clip.rotate_keyframes.is_empty()
            || !clip.crop_left_keyframes.is_empty()
            || !clip.crop_right_keyframes.is_empty()
            || !clip.crop_top_keyframes.is_empty()
            || !clip.crop_bottom_keyframes.is_empty();
        let transform_blocked_by_mask =
            has_transform_keyframes && clip.masks.iter().any(|m| m.enabled);

        unsupported_audio
            || unsupported_speed
            || unsupported_blur
            || unsupported_mask_keyframes
            || transform_blocked_by_mask
    }

    fn clip_has_unsupported_background_prerender_audio_effects(clip: &ProgramClip) -> bool {
        // Audio effects no longer block prerendering.  The prerender bakes
        // audio at current levels; audio property changes are excluded from
        // the signature so they don't invalidate cached segments.
        let _ = clip;
        false
    }

    fn clip_has_unsupported_background_prerender_features(clip: &ProgramClip) -> bool {
        Self::clip_has_unsupported_background_prerender_keyframes(clip)
            || clip.is_freeze_frame()
            || clip.reverse
            || (clip.speed - 1.0).abs() > 0.001
    }

    fn active_has_unsupported_background_prerender_features(&self, active: &[usize]) -> bool {
        active.iter().any(|&idx| {
            self.clips
                .get(idx)
                .map(Self::clip_has_unsupported_background_prerender_features)
                .unwrap_or(true)
        })
    }

    /// Return the highest-track-index clip active at this position.
    fn clip_at(&self, timeline_pos_ns: u64) -> Option<usize> {
        let exact = self
            .clips
            .iter()
            .enumerate()
            .filter(|(_, c)| {
                timeline_pos_ns >= c.timeline_start_ns && timeline_pos_ns < c.timeline_end_ns()
            })
            .max_by_key(|(_, c)| c.track_index)
            .map(|(i, _)| i);
        if exact.is_some() {
            return exact;
        }
        const GAP_NS: u64 = 100_000_000;
        let next_start = self
            .clips
            .iter()
            .filter(|c| {
                c.timeline_start_ns > timeline_pos_ns
                    && c.timeline_start_ns <= timeline_pos_ns + GAP_NS
            })
            .map(|c| c.timeline_start_ns)
            .min()?;
        self.clips
            .iter()
            .enumerate()
            .filter(|(_, c)| c.timeline_start_ns == next_start)
            .max_by_key(|(_, c)| c.track_index)
            .map(|(i, _)| i)
    }

    /// Return ALL clip indices active at the given timeline position, sorted by track_index.
    /// Includes clips that enter early or linger after the cut because of transition
    /// overlap placement.
    fn clips_active_at(&self, timeline_pos_ns: u64) -> Vec<usize> {
        let mut active: Vec<usize> = self
            .clips
            .iter()
            .enumerate()
            .filter(|(idx, _)| {
                Self::clip_active_window(&self.clips, *idx)
                    .map(|(start_ns, end_ns)| {
                        timeline_pos_ns >= start_ns && timeline_pos_ns < end_ns
                    })
                    .unwrap_or(false)
            })
            .map(|(i, _)| i)
            .collect();
        if active.is_empty() {
            const GAP_NS: u64 = 100_000_000;
            if let Some(next_start) = self
                .clips
                .iter()
                .enumerate()
                .filter_map(|(idx, _)| {
                    Self::clip_active_window(&self.clips, idx).map(|(start_ns, _)| start_ns)
                })
                .filter(|start_ns| {
                    *start_ns > timeline_pos_ns && *start_ns <= timeline_pos_ns + GAP_NS
                })
                .min()
            {
                active = self
                    .clips
                    .iter()
                    .enumerate()
                    .filter(|(idx, _)| {
                        Self::clip_active_window(&self.clips, *idx)
                            .map(|(start_ns, _)| start_ns == next_start)
                            .unwrap_or(false)
                    })
                    .map(|(i, _)| i)
                    .collect();
            }
        }
        active.sort_by_key(|&i| self.clips[i].track_index);
        active
    }

    /// Compute the transition state for a clip at the given timeline position.
    /// Returns `None` if the clip is not currently in a transition region.
    fn compute_transition_state(
        &self,
        clip_idx: usize,
        timeline_pos_ns: u64,
    ) -> Option<TransitionState> {
        let clip = &self.clips[clip_idx];
        // Check if this clip is the OUTGOING clip (has transition_after set).
        if let Some(window) = Self::outgoing_transition_window_for_clip(clip) {
            if let Some(progress) = window.progress_at(timeline_pos_ns) {
                return Some(TransitionState {
                    kind: canonicalize_transition_kind(&clip.transition_after),
                    progress,
                    role: TransitionRole::Outgoing,
                });
            }
        }
        // Check if this clip is the INCOMING clip (preceding same-track clip has transition).
        if let Some((prev_idx, window)) =
            Self::incoming_transition_window_for_clip(&self.clips, clip_idx)
        {
            if let Some(progress) = window.progress_at(timeline_pos_ns) {
                return Some(TransitionState {
                    kind: canonicalize_transition_kind(&self.clips[prev_idx].transition_after),
                    progress,
                    role: TransitionRole::Incoming,
                });
            }
        }
        None
    }

    /// Find the before-cut overlap amount for a clip entering via a transition.
    /// Returns the preceding clip's before-cut split if present, otherwise 0.
    fn transition_enter_offset_for_clip(&self, clip_idx: usize) -> u64 {
        Self::incoming_transition_window_for_clip(&self.clips, clip_idx)
            .map(|(_, window)| window.before_cut_ns)
            .unwrap_or(0)
    }

    fn apply_keyframed_video_slot_properties(&self, timeline_pos: u64) {
        let (proc_w, proc_h) = self.preview_processing_dimensions();
        for slot in &self.slots {
            if slot.hidden || slot.is_prerender_slot {
                continue;
            }
            let Some(clip) = self.clips.get(slot.clip_idx) else {
                continue;
            };
            let scale = clip.scale_at_timeline_ns(timeline_pos);
            let pos_x = clip.position_x_at_timeline_ns(timeline_pos);
            let pos_y = clip.position_y_at_timeline_ns(timeline_pos);
            let crop_left = clip.crop_left_at_timeline_ns(timeline_pos);
            let crop_right = clip.crop_right_at_timeline_ns(timeline_pos);
            let crop_top = clip.crop_top_at_timeline_ns(timeline_pos);
            let crop_bottom = clip.crop_bottom_at_timeline_ns(timeline_pos);
            let rotate = clip.rotate_at_timeline_ns(timeline_pos);
            let brightness = clip.brightness_at_timeline_ns(timeline_pos);
            let contrast = clip.contrast_at_timeline_ns(timeline_pos);
            let saturation = clip.saturation_at_timeline_ns(timeline_pos);
            let temperature = clip.temperature_at_timeline_ns(timeline_pos);
            let tint = clip.tint_at_timeline_ns(timeline_pos);
            Self::apply_transform_to_slot(
                slot,
                crop_left,
                crop_right,
                crop_top,
                crop_bottom,
                rotate,
                clip.flip_h,
                clip.flip_v,
            );
            let has_coloradj = slot.coloradj_rgb.is_some();
            let has_3point = slot.colorbalance_3pt.is_some();
            if let Some(ref vb) = slot.videobalance {
                let p = Self::compute_videobalance_params(
                    brightness,
                    contrast,
                    saturation,
                    temperature,
                    tint,
                    clip.shadows,
                    clip.midtones,
                    clip.highlights,
                    clip.exposure,
                    clip.black_point,
                    clip.highlights_warmth,
                    clip.highlights_tint,
                    clip.midtones_warmth,
                    clip.midtones_tint,
                    clip.shadows_warmth,
                    clip.shadows_tint,
                    has_coloradj,
                    has_3point,
                );
                vb.set_property("brightness", p.brightness);
                vb.set_property("contrast", p.contrast);
                vb.set_property("saturation", p.saturation);
                vb.set_property("hue", p.hue);
            }
            if let Some(ref ca) = slot.coloradj_rgb {
                let cp = Self::compute_coloradj_params(temperature, tint);
                ca.set_property("r", cp.r);
                ca.set_property("g", cp.g);
                ca.set_property("b", cp.b);
            }
            if let Some(ref pad) = slot.compositor_pad {
                let alpha = clip.opacity_at_timeline_ns(timeline_pos).clamp(0.0, 1.0);
                if slot.is_blend_mode {
                    // Blend-mode clips are hidden from compositor (alpha=0).
                    // Store intended alpha for the blend probe to use.
                    if let Ok(mut a) = slot.blend_alpha.lock() {
                        *a = alpha;
                    }
                } else {
                    pad.set_property("alpha", alpha);
                }
                Self::apply_zoom_to_slot(
                    slot,
                    pad,
                    scale,
                    pos_x,
                    pos_y,
                    Self::clip_uses_direct_canvas_translation(clip),
                    proc_w,
                    proc_h,
                );
            }
        }
    }

    /// Apply transition visual effects (alpha, crop, motion) to all visible slots
    /// based on the current timeline position.
    fn apply_transition_effects(&self, timeline_pos: u64) {
        let (proc_w, proc_h) = self.preview_processing_dimensions();
        let active = self.clips_active_at(timeline_pos);
        let transition_states: Vec<Option<TransitionState>> = self
            .slots
            .iter()
            .map(|slot| {
                if slot.hidden || slot.is_prerender_slot {
                    None
                } else {
                    self.compute_transition_state(slot.clip_idx, timeline_pos)
                }
            })
            .collect();
        let mut slot_zorders: HashMap<usize, u32> = active
            .iter()
            .enumerate()
            .map(|(idx, &clip_idx)| (clip_idx, (idx + 1) as u32))
            .collect();
        for (slot, tstate) in self.slots.iter().zip(transition_states.iter()) {
            let Some(ts) = tstate.as_ref() else {
                continue;
            };
            if ts.role != TransitionRole::Outgoing
                || !transition_preview_outgoing_should_draw_on_top(ts.kind.as_str())
            {
                continue;
            }
            let Some(incoming_idx) = Self::adjacent_next_same_track(&self.clips, slot.clip_idx)
            else {
                continue;
            };
            let Some(outgoing_zorder) = slot_zorders.get(&slot.clip_idx).copied() else {
                continue;
            };
            let Some(incoming_zorder) = slot_zorders.get(&incoming_idx).copied() else {
                continue;
            };
            slot_zorders.insert(slot.clip_idx, incoming_zorder);
            slot_zorders.insert(incoming_idx, outgoing_zorder);
        }
        self.background_src.set_property_from_str(
            "pattern",
            transition_preview_background_pattern(&transition_states),
        );
        for (slot, tstate) in self.slots.iter().zip(transition_states.iter()) {
            if slot.hidden || slot.is_prerender_slot {
                continue;
            }
            let clip = &self.clips[slot.clip_idx];
            if let Ok(mut guard) = slot.mask_data.lock() {
                *guard = merged_transition_preview_masks(&clip.masks, tstate.as_ref());
            }
            if let Some(ref pad) = slot.compositor_pad {
                if let Some(zorder) = slot_zorders.get(&slot.clip_idx).copied() {
                    pad.set_property("zorder", zorder);
                }
                let base_alpha = clip.opacity_at_timeline_ns(timeline_pos).clamp(0.0, 1.0);
                let scale = clip.scale_at_timeline_ns(timeline_pos);
                let pos_x = clip.position_x_at_timeline_ns(timeline_pos);
                let pos_y = clip.position_y_at_timeline_ns(timeline_pos);
                let effective_alpha = match tstate.as_ref() {
                    Some(ts) => {
                        let t = ts.progress;
                        match (ts.kind.as_str(), ts.role) {
                            ("cross_dissolve", TransitionRole::Outgoing) => base_alpha * (1.0 - t),
                            ("cross_dissolve", TransitionRole::Incoming) => base_alpha * t,
                            ("fade_to_black" | "fade_to_white", TransitionRole::Outgoing) => {
                                base_alpha * (1.0 - 2.0 * t).max(0.0)
                            }
                            ("fade_to_black" | "fade_to_white", TransitionRole::Incoming) => {
                                base_alpha * (2.0 * t - 1.0).max(0.0)
                            }
                            _ if transition_preserves_slot_alpha(ts.kind.as_str()) => base_alpha,
                            _ => match ts.role {
                                TransitionRole::Outgoing => base_alpha * (1.0 - t),
                                TransitionRole::Incoming => base_alpha * t,
                            },
                        }
                    }
                    None => base_alpha,
                };
                if slot.is_blend_mode {
                    if let Ok(mut a) = slot.blend_alpha.lock() {
                        *a = effective_alpha;
                    }
                } else {
                    pad.set_property("alpha", effective_alpha);
                }
                if let Some((canvas_x, canvas_y)) = tstate.as_ref().and_then(|ts| {
                    transition_preview_canvas_offset(ts.kind.as_str(), ts.role, ts.progress)
                }) {
                    Self::apply_zoom_to_slot_with_canvas_offset(
                        slot,
                        pad,
                        scale,
                        pos_x,
                        pos_y,
                        Self::clip_uses_direct_canvas_translation(clip),
                        proc_w,
                        proc_h,
                        (canvas_x * proc_w as f64).round() as i32,
                        (canvas_y * proc_h as f64).round() as i32,
                    );
                }
            }
            // Wipe transitions: animate videocrop on incoming clip to progressively reveal.
            if let Some(ts) = tstate.as_ref() {
                if transition_uses_wipe_crop(ts.kind.as_str())
                    && ts.role == TransitionRole::Incoming
                {
                    let t = ts.progress;
                    if let Some(ref vc) = slot.videocrop {
                        let user_cl = clip.crop_left_at_timeline_ns(timeline_pos).max(0);
                        let user_cr = clip.crop_right_at_timeline_ns(timeline_pos).max(0);
                        let user_ct = clip.crop_top_at_timeline_ns(timeline_pos).max(0);
                        let user_cb = clip.crop_bottom_at_timeline_ns(timeline_pos).max(0);
                        if ts.kind == "wipe_right" {
                            let crop_px = ((1.0 - t) * proc_w as f64).round() as i32;
                            // Reveal from left to right: crop the right side.
                            vc.set_property("left", user_cl);
                            vc.set_property("right", user_cr + crop_px);
                            vc.set_property("top", user_ct);
                            vc.set_property("bottom", user_cb);
                        } else if ts.kind == "wipe_left" {
                            let crop_px = ((1.0 - t) * proc_w as f64).round() as i32;
                            // Reveal from right to left: crop the left side.
                            vc.set_property("left", user_cl + crop_px);
                            vc.set_property("right", user_cr);
                            vc.set_property("top", user_ct);
                            vc.set_property("bottom", user_cb);
                        } else if ts.kind == "wipeup" {
                            let crop_px = ((1.0 - t) * proc_h as f64).round() as i32;
                            // Reveal from bottom to top: crop the top side.
                            vc.set_property("left", user_cl);
                            vc.set_property("right", user_cr);
                            vc.set_property("top", user_ct + crop_px);
                            vc.set_property("bottom", user_cb);
                        } else {
                            let crop_px = ((1.0 - t) * proc_h as f64).round() as i32;
                            // Reveal from top to bottom: crop the bottom side.
                            vc.set_property("left", user_cl);
                            vc.set_property("right", user_cr);
                            vc.set_property("top", user_ct);
                            vc.set_property("bottom", user_cb + crop_px);
                        }
                    }
                    // Re-pad with transparent borders to maintain frame dimensions.
                    if let Some(ref vb) = slot.videobox_crop_alpha {
                        let user_cl = clip.crop_left_at_timeline_ns(timeline_pos).max(0);
                        let user_cr = clip.crop_right_at_timeline_ns(timeline_pos).max(0);
                        let user_ct = clip.crop_top_at_timeline_ns(timeline_pos).max(0);
                        let user_cb = clip.crop_bottom_at_timeline_ns(timeline_pos).max(0);
                        if ts.kind == "wipe_right" {
                            let crop_px = ((1.0 - t) * proc_w as f64).round() as i32;
                            vb.set_property("left", -user_cl);
                            vb.set_property("right", -(user_cr + crop_px));
                            vb.set_property("top", -user_ct);
                            vb.set_property("bottom", -user_cb);
                        } else if ts.kind == "wipe_left" {
                            let crop_px = ((1.0 - t) * proc_w as f64).round() as i32;
                            vb.set_property("left", -(user_cl + crop_px));
                            vb.set_property("right", -user_cr);
                            vb.set_property("top", -user_ct);
                            vb.set_property("bottom", -user_cb);
                        } else if ts.kind == "wipeup" {
                            let crop_px = ((1.0 - t) * proc_h as f64).round() as i32;
                            vb.set_property("left", -user_cl);
                            vb.set_property("right", -user_cr);
                            vb.set_property("top", -(user_ct + crop_px));
                            vb.set_property("bottom", -user_cb);
                        } else {
                            let crop_px = ((1.0 - t) * proc_h as f64).round() as i32;
                            vb.set_property("left", -user_cl);
                            vb.set_property("right", -user_cr);
                            vb.set_property("top", -user_ct);
                            vb.set_property("bottom", -(user_cb + crop_px));
                        }
                        vb.set_property("border-alpha", 0.0_f64);
                    }
                }
            }
        }
    }

    fn effective_source_path_for_clip(&self, clip: &ProgramClip) -> String {
        let (path, _, _, _) = self.resolve_source_path_for_clip(clip);
        path
    }

    fn animated_svg_render_key_for_clip(&self, clip: &ProgramClip) -> Option<String> {
        clip.animated_svg.then(|| {
            crate::media::animated_svg::animated_svg_render_key(
                &clip.source_path,
                clip.source_in_ns,
                clip.source_out_ns,
                clip.media_duration_ns,
                self.frame_rate_num,
                self.frame_rate_den,
            )
        })
    }

    fn resolve_source_path_for_clip(&self, clip: &ProgramClip) -> (String, bool, String, bool) {
        let resolve_ready_proxy = |key: &String| {
            self.proxy_paths.get(key).and_then(|p| {
                std::fs::metadata(p)
                    .ok()
                    .filter(|m| m.len() > 0)
                    .map(|_| p.clone())
            })
        };

        if let Some(key) = self.animated_svg_render_key_for_clip(clip) {
            if let Some(path) = self.animated_svg_paths.get(&key).and_then(|p| {
                std::fs::metadata(p)
                    .ok()
                    .filter(|m| m.len() > 0)
                    .map(|_| p.clone())
            }) {
                return (path, false, key, true);
            }
            return (clip.source_path.clone(), false, key, false);
        }

        // AI frame-interpolation sidecar takes priority over the original
        // source so playback shows the same interpolated frames as export.
        // The sidecar has the same wall-clock duration as the source so
        // existing rate-seek and source_in math are unchanged.
        if clip.slow_motion_interp == crate::model::clip::SlowMotionInterp::Ai {
            if let Some(interp_path) = self.frame_interp_paths.get(&clip.id) {
                if std::fs::metadata(interp_path)
                    .ok()
                    .filter(|m| m.len() > 0)
                    .is_some()
                {
                    return (interp_path.clone(), false, String::new(), false);
                }
            }
        }

        // Voice-enhance prerender: when the toggle is on, the cache
        // produces a media file with the original video stream-copied
        // and the audio re-encoded through the same chain that the
        // export side uses. Swapping the source path here is what
        // makes preview audio match export — without trying to do
        // any GStreamer-side audio processing.
        //
        // The cache is keyed by `(source_path, strength)`, so different
        // strengths for the same source map to different files and
        // dropping the strength back to a previously-rendered value is
        // an instant cache hit.
        if clip.voice_enhance && !self.voice_enhance_paths.is_empty() {
            let key = crate::media::voice_enhance_cache::cache_key(
                &clip.source_path,
                clip.voice_enhance_strength,
            );
            if let Some(ve_path) = self.voice_enhance_paths.get(&key) {
                if std::fs::metadata(ve_path)
                    .ok()
                    .filter(|m| m.len() > 0)
                    .is_some()
                {
                    return (ve_path.clone(), false, String::new(), false);
                }
            }
        }

        // Check for bg-removed version first (takes priority — includes alpha channel).
        if clip.bg_removal_enabled {
            if let Some(bg_path) = self.bg_removal_paths.get(&clip.source_path) {
                if std::fs::metadata(bg_path)
                    .ok()
                    .filter(|m| m.len() > 0)
                    .is_some()
                {
                    return (bg_path.clone(), false, String::new(), false);
                }
            }
        }

        let lut_composite = if clip.lut_paths.is_empty() {
            None
        } else {
            Some(clip.lut_paths.join("|"))
        };
        if self.proxy_enabled {
            let key = crate::media::proxy_cache::proxy_key_with_vidstab(
                &clip.source_path,
                lut_composite.as_deref(),
                clip.vidstab_enabled,
                clip.vidstab_smoothing,
            );
            resolve_ready_proxy(&key)
                .map(|p| (p, true, key.clone(), false))
                .unwrap_or_else(|| (clip.source_path.clone(), false, key, false))
        } else if self.preview_luts && !clip.lut_paths.is_empty() {
            let key = crate::media::proxy_cache::proxy_key_with_vidstab(
                &clip.source_path,
                lut_composite.as_deref(),
                clip.vidstab_enabled,
                clip.vidstab_smoothing,
            );
            resolve_ready_proxy(&key)
                .map(|p| (p, true, key.clone(), false))
                .unwrap_or_else(|| (clip.source_path.clone(), false, key, false))
        } else {
            (clip.source_path.clone(), false, String::new(), false)
        }
    }

    fn prerender_manifest_inputs_for_active(
        &self,
        active: &[usize],
    ) -> Option<Vec<PrerenderManifestInput>> {
        let mut signatures_by_path: HashMap<String, PrerenderSourceSignature> = HashMap::new();
        for &idx in active {
            let clip = self.clips.get(idx)?;
            if clip.is_adjustment || clip.is_title {
                continue;
            }
            let (path, _, _, _) = self.resolve_source_path_for_clip(clip);
            let signature = prerender_source_signature_for_path(Path::new(&path))?;
            signatures_by_path.entry(path).or_insert(signature);
        }
        let mut inputs: Vec<PrerenderManifestInput> = signatures_by_path
            .into_iter()
            .map(|(path, signature)| PrerenderManifestInput { path, signature })
            .collect();
        inputs.sort_by(|a, b| a.path.cmp(&b.path));
        Some(inputs)
    }

    fn build_prerender_manifest_for_segment(
        &self,
        key: &str,
        signature: u64,
        start_ns: u64,
        end_ns: u64,
        active: &[usize],
    ) -> Option<PrerenderSegmentManifest> {
        Some(PrerenderSegmentManifest {
            key: key.to_string(),
            signature,
            start_ns,
            end_ns,
            inputs: self.prerender_manifest_inputs_for_active(active)?,
        })
    }

    fn cached_prerender_segment_matches_manifest(
        &self,
        path: &Path,
        key: &str,
        signature: u64,
        start_ns: u64,
        end_ns: u64,
        expected_inputs: &[PrerenderManifestInput],
    ) -> bool {
        let Some(manifest) = Self::load_prerender_manifest_for_path(path) else {
            return false;
        };
        manifest.key == key
            && manifest.signature == signature
            && manifest.start_ns == start_ns
            && manifest.end_ns == end_ns
            && manifest.inputs == expected_inputs
            && Self::prerender_manifest_inputs_are_fresh(&manifest.inputs)
    }

    fn discover_prerender_segment_for(
        &mut self,
        timeline_pos: u64,
        signature: u64,
        expected_inputs: &[PrerenderManifestInput],
    ) -> Option<PrerenderSegment> {
        let best_match = Self::discover_prerender_segment_in_root(
            &self.prerender_cache_root,
            timeline_pos,
            signature,
            expected_inputs,
        );
        if let Some(segment) = best_match.clone() {
            self.prerender_runtime_files.insert(segment.path.clone());
            self.prerender_segments
                .insert(segment.key.clone(), segment.clone());
            self.prune_prerender_segment_cache();
        }
        best_match
    }

    fn discover_prerender_segment_in_root(
        root: &Path,
        timeline_pos: u64,
        signature: u64,
        expected_inputs: &[PrerenderManifestInput],
    ) -> Option<PrerenderSegment> {
        let mut best_match: Option<PrerenderSegment> = None;
        if let Ok(entries) = std::fs::read_dir(root) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.extension().and_then(|s| s.to_str()) != Some("mp4") {
                    continue;
                }
                let managed_segment = is_managed_prerender_segment_path(&path);
                let Some(manifest) = Self::load_prerender_manifest_for_path(&path) else {
                    if managed_segment {
                        remove_prerender_segment_files(&path);
                    }
                    continue;
                };
                if !Self::prerender_manifest_inputs_are_fresh(&manifest.inputs) {
                    if managed_segment {
                        remove_prerender_segment_files(&path);
                    }
                    continue;
                }
                if manifest.signature != signature || manifest.inputs != expected_inputs {
                    // Signature or input mismatch — this segment was rendered
                    // with different clip state and will never match again.
                    // Remove it to free disk space and avoid repeated scans.
                    if managed_segment {
                        remove_prerender_segment_files(&path);
                    }
                    continue;
                }
                if timeline_pos < manifest.start_ns || timeline_pos >= manifest.end_ns {
                    continue;
                }
                let segment = PrerenderSegment {
                    key: manifest.key,
                    path: path.to_string_lossy().to_string(),
                    start_ns: manifest.start_ns,
                    end_ns: manifest.end_ns,
                    signature: manifest.signature,
                };
                if best_match
                    .as_ref()
                    .map(|current| segment.start_ns > current.start_ns)
                    .unwrap_or(true)
                {
                    best_match = Some(segment);
                }
            }
        }
        best_match
    }

    /// Get or parse a `.cube` LUT file, caching the result for reuse.
    fn get_or_parse_lut(&mut self, path: &str) -> Option<Arc<CubeLut>> {
        if let Some(cached) = self.lut_cache.get(path) {
            return Some(cached.clone());
        }
        match CubeLut::from_file(std::path::Path::new(path)) {
            Ok(lut) => {
                let shared = Arc::new(lut);
                self.lut_cache.insert(path.to_string(), shared.clone());
                log::info!("CubeLut: parsed {} (size={})", path, shared.size);
                Some(shared)
            }
            Err(e) => {
                log::warn!("CubeLut: failed to parse {}: {}", path, e);
                None
            }
        }
    }

    fn next_video_boundary_after(&self, timeline_pos_ns: u64) -> Option<u64> {
        let mut next: Option<u64> = None;
        for clip_idx in 0..self.clips.len() {
            let Some((active_start_ns, active_end_ns)) =
                Self::clip_active_window(&self.clips, clip_idx)
            else {
                continue;
            };
            for candidate in [active_start_ns, active_end_ns] {
                if candidate > timeline_pos_ns {
                    next = Some(
                        next.map(|current| current.min(candidate))
                            .unwrap_or(candidate),
                    );
                }
            }
        }
        next
    }

    fn supported_transition_prerender_kind(kind: &str) -> Option<&'static str> {
        transition_xfade_name_for_kind(kind)
    }

    fn transition_kind_for_prerender_metric(xfade_transition: &str) -> &'static str {
        transition_kind_from_xfade_name(xfade_transition).unwrap_or("unknown")
    }

    fn record_transition_prerender_metric(&mut self, transition_kind: &str, hit: bool) {
        let total = {
            let entry = self
                .transition_prerender_metrics
                .entry(transition_kind.to_string())
                .or_insert((0, 0));
            if hit {
                entry.0 = entry.0.saturating_add(1);
            } else {
                entry.1 = entry.1.saturating_add(1);
            }
            entry.0.saturating_add(entry.1)
        };
        if total == 1 || total % 10 == 0 {
            if let Some((hit_count, miss_count)) =
                self.transition_prerender_metrics.get(transition_kind)
            {
                let hit_rate = if total > 0 {
                    *hit_count as f64 * 100.0 / total as f64
                } else {
                    0.0
                };
                log::info!(
                    "transition_prerender_metrics: kind={} hit={} miss={} hit_rate={:.1}%",
                    transition_kind,
                    hit_count,
                    miss_count,
                    hit_rate
                );
            }
        }
        self.maybe_decay_transition_prerender_metrics();
    }

    fn decay_transition_metric_count(value: u64) -> u64 {
        if value <= 1 {
            value
        } else {
            (value.saturating_add(1)) / 2
        }
    }

    fn apply_transition_metric_decay(metrics: &mut HashMap<String, (u64, u64)>) -> (u64, u64) {
        let mut hits = 0_u64;
        let mut misses = 0_u64;
        for (hit, miss) in metrics.values_mut() {
            *hit = Self::decay_transition_metric_count(*hit);
            *miss = Self::decay_transition_metric_count(*miss);
            hits = hits.saturating_add(*hit);
            misses = misses.saturating_add(*miss);
        }
        (hits, misses)
    }

    fn maybe_decay_transition_prerender_metrics(&mut self) {
        const DECAY_INTERVAL_SAMPLES: u64 = 64;
        let (hits_before, misses_before) = self.transition_prerender_hit_miss_totals();
        let total_before = hits_before.saturating_add(misses_before);
        if total_before == 0 || total_before % DECAY_INTERVAL_SAMPLES != 0 {
            return;
        }
        let (hits_after, misses_after) =
            Self::apply_transition_metric_decay(&mut self.transition_prerender_metrics);
        log::info!(
            "transition_prerender_metrics: decay applied total={}=>{} (hit={}=>{}, miss={}=>{})",
            total_before,
            hits_after.saturating_add(misses_after),
            hits_before,
            hits_after,
            misses_before,
            misses_after
        );
        if hits_after.saturating_add(misses_after) == 0 {
            log::info!(
                "transition_prerender_metrics: decay cleared counters; resetting transition metrics"
            );
            self.transition_prerender_metrics.clear();
        }
    }

    fn transition_prerender_hit_miss_totals(&self) -> (u64, u64) {
        let mut hits = 0_u64;
        let mut misses = 0_u64;
        for (h, m) in self.transition_prerender_metrics.values() {
            hits = hits.saturating_add(*h);
            misses = misses.saturating_add(*m);
        }
        (hits, misses)
    }

    fn prerender_cache_hit_rate_percent(&self) -> f64 {
        let total = self
            .prerender_cache_hits
            .saturating_add(self.prerender_cache_misses);
        if total == 0 {
            0.0
        } else {
            self.prerender_cache_hits as f64 * 100.0 / total as f64
        }
    }

    fn record_prerender_cache_lookup(&mut self, hit: bool) {
        if hit {
            self.prerender_cache_hits = self.prerender_cache_hits.saturating_add(1);
        } else {
            self.prerender_cache_misses = self.prerender_cache_misses.saturating_add(1);
        }
        let total = self
            .prerender_cache_hits
            .saturating_add(self.prerender_cache_misses);
        if total > 0 && total % 25 == 0 {
            log::info!(
                "prerender_cache_metrics: hit={} miss={} hit_rate={:.1}%",
                self.prerender_cache_hits,
                self.prerender_cache_misses,
                self.prerender_cache_hit_rate_percent()
            );
        }
    }

    fn transition_low_hitrate_pressure(&self) -> bool {
        const LOW_HITRATE_MIN_SAMPLES: u64 = 6;
        const LOW_HITRATE_PERCENT: u64 = 60;
        if !matches!(self.playback_priority, PlaybackPriority::Smooth)
            || self.prerender_pending.len() >= 4
        {
            return false;
        }
        let (hits, misses) = self.transition_prerender_hit_miss_totals();
        let total_samples = hits.saturating_add(misses);
        total_samples >= LOW_HITRATE_MIN_SAMPLES
            && hits.saturating_mul(100) < LOW_HITRATE_PERCENT.saturating_mul(total_samples)
    }

    fn transition_prerender_priority_score_at(&self, boundary_ns: u64, active: &[usize]) -> u64 {
        let Some(spec) = self.transition_overlap_prerender_spec_at(boundary_ns, active) else {
            return 0;
        };
        let kind = Self::transition_kind_for_prerender_metric(&spec.xfade_transition);
        let Some((hit, miss)) = self.transition_prerender_metrics.get(kind) else {
            // Unseen transition kinds should still be sampled proactively.
            return 550;
        };
        let total = hit.saturating_add(*miss);
        if total == 0 {
            return 550;
        }
        let miss_rate_per_mille = miss.saturating_mul(1000) / total;
        // Prioritize confident poor performers first while still allowing low-sample
        // transitions to receive attention.
        let confidence_bonus = total.min(50);
        miss_rate_per_mille.saturating_mul(10) + confidence_bonus
    }

    fn transition_prerender_effective_priority_score(
        base_priority: u64,
        timeline_pos_ns: u64,
        boundary_ns: u64,
        lookahead_ns: u64,
    ) -> u64 {
        const PROXIMITY_BONUS_SCALE: u64 = 2;
        if lookahead_ns == 0 {
            return base_priority;
        }
        let distance_ns = boundary_ns
            .saturating_sub(timeline_pos_ns)
            .min(lookahead_ns);
        let proximity_per_mille = lookahead_ns
            .saturating_sub(distance_ns)
            .saturating_mul(1000)
            / lookahead_ns;
        base_priority.saturating_add(proximity_per_mille.saturating_mul(PROXIMITY_BONUS_SCALE))
    }

    fn sort_prerender_candidates(
        candidates: &mut [(u64, Vec<usize>, u64)],
        queue_budget_tight: bool,
        timeline_pos_ns: u64,
        lookahead_ns: u64,
    ) {
        if queue_budget_tight {
            candidates.sort_by(|a, b| {
                let score_a = Self::transition_prerender_effective_priority_score(
                    a.2,
                    timeline_pos_ns,
                    a.0,
                    lookahead_ns,
                );
                let score_b = Self::transition_prerender_effective_priority_score(
                    b.2,
                    timeline_pos_ns,
                    b.0,
                    lookahead_ns,
                );
                score_b.cmp(&score_a).then_with(|| a.0.cmp(&b.0))
            });
        } else {
            candidates.sort_by_key(|entry| entry.0);
        }
    }

    fn collect_upcoming_prerender_boundaries(
        &self,
        timeline_pos_ns: u64,
        lookahead_ns: u64,
        max_candidates: usize,
    ) -> Vec<(u64, Vec<usize>, u64)> {
        let mut candidates = Vec::new();
        let mut cursor = timeline_pos_ns;
        for _ in 0..max_candidates {
            let Some(boundary_ns) = self.next_video_boundary_after(cursor) else {
                break;
            };
            if boundary_ns.saturating_sub(timeline_pos_ns) > lookahead_ns {
                break;
            }
            let active = self.clips_active_at(boundary_ns);
            let priority = self.transition_prerender_priority_score_at(boundary_ns, &active);
            candidates.push((boundary_ns, active, priority));
            cursor = boundary_ns.saturating_add(self.frame_duration_ns.max(1));
        }
        candidates
    }

    fn prerender_request_priority_score(&self, boundary_ns: u64, active: &[usize]) -> u64 {
        const REQUEST_PRIORITY_LOOKAHEAD_NS: u64 = 16_000_000_000;
        const NON_TRANSITION_BASE_PRIORITY: u64 = 300;
        const TRANSITION_PRIORITY_OFFSET: u64 = 2_000;
        let transition_priority = self.transition_prerender_priority_score_at(boundary_ns, active);
        let base = if transition_priority > 0 {
            TRANSITION_PRIORITY_OFFSET.saturating_add(transition_priority)
        } else {
            let overlap_bonus = (active.len().saturating_sub(2) as u64).saturating_mul(75);
            NON_TRANSITION_BASE_PRIORITY.saturating_add(overlap_bonus)
        };
        Self::transition_prerender_effective_priority_score(
            base,
            self.timeline_pos_ns,
            boundary_ns,
            REQUEST_PRIORITY_LOOKAHEAD_NS,
        )
    }

    fn prerender_queue_allows_request(
        pending_count: usize,
        lowest_pending_priority: Option<u64>,
        request_priority: u64,
    ) -> bool {
        const MAX_PENDING_PRERENDER_JOBS: usize = 6;
        const MAX_PENDING_OVERFLOW_JOBS: usize = 2;
        const OVERFLOW_PRIORITY_MARGIN: u64 = 200;
        if pending_count < MAX_PENDING_PRERENDER_JOBS {
            return true;
        }
        if pending_count >= MAX_PENDING_PRERENDER_JOBS + MAX_PENDING_OVERFLOW_JOBS {
            return false;
        }
        let Some(lowest) = lowest_pending_priority else {
            return false;
        };
        request_priority > lowest.saturating_add(OVERFLOW_PRIORITY_MARGIN)
    }

    fn ordered_rebuild_history_ms(&self) -> Vec<u64> {
        let n = self.rebuild_history_count;
        if n == 0 {
            return Vec::new();
        }
        let start = if n < self.rebuild_history_ms.len() {
            0
        } else {
            self.rebuild_history_cursor % self.rebuild_history_ms.len()
        };
        let mut ordered = Vec::with_capacity(n);
        for i in 0..n {
            ordered.push(self.rebuild_history_ms[(start + i) % self.rebuild_history_ms.len()]);
        }
        ordered
    }

    pub fn performance_snapshot(&self) -> ProgramPerformanceSnapshot {
        let mut transition_metrics: Vec<TransitionPrerenderMetricPoint> = self
            .transition_prerender_metrics
            .iter()
            .map(|(kind, (hit, miss))| {
                let total = hit.saturating_add(*miss);
                let hit_rate_percent = if total > 0 {
                    *hit as f64 * 100.0 / total as f64
                } else {
                    0.0
                };
                TransitionPrerenderMetricPoint {
                    kind: kind.clone(),
                    hit: *hit,
                    miss: *miss,
                    hit_rate_percent,
                }
            })
            .collect();
        transition_metrics.sort_by(|a, b| a.kind.cmp(&b.kind));

        let (transition_hits_total, transition_misses_total) =
            self.transition_prerender_hit_miss_totals();
        let transition_total = transition_hits_total.saturating_add(transition_misses_total);
        let transition_hit_rate_percent = if transition_total > 0 {
            transition_hits_total as f64 * 100.0 / transition_total as f64
        } else {
            0.0
        };

        let rebuild_history_recent_ms = self.ordered_rebuild_history_ms();
        let rebuild_history_samples = rebuild_history_recent_ms.len();
        let rebuild_latest_ms = rebuild_history_recent_ms.last().copied();
        let (rebuild_p50_ms, rebuild_p75_ms) = if rebuild_history_samples > 0 {
            let mut sorted = rebuild_history_recent_ms.clone();
            sorted.sort_unstable();
            let p50 = sorted[(rebuild_history_samples / 2).min(rebuild_history_samples - 1)];
            let p75 = sorted[(rebuild_history_samples * 3 / 4).min(rebuild_history_samples - 1)];
            (Some(p50), Some(p75))
        } else {
            (None, None)
        };

        ProgramPerformanceSnapshot {
            player_state: match self.state {
                PlayerState::Playing => "playing",
                PlayerState::Paused => "paused",
                PlayerState::Stopped => "stopped",
            }
            .to_string(),
            playback_priority: self.playback_priority.as_str().to_string(),
            timeline_pos_ns: self.timeline_pos_ns,
            background_prerender_enabled: self.background_prerender,
            prerender_total_requested: self.prerender_total_requested,
            prerender_pending: self.prerender_pending.len(),
            prerender_ready: self.prerender_segments.len(),
            prerender_failed: self.prerender_failed.len(),
            prerender_cache_hits: self.prerender_cache_hits,
            prerender_cache_misses: self.prerender_cache_misses,
            prerender_cache_hit_rate_percent: self.prerender_cache_hit_rate_percent(),
            prewarmed_boundary_ns: self.prewarmed_boundary_ns,
            active_prerender_segment_key: self.current_prerender_segment_key.clone(),
            rebuild_history_samples,
            rebuild_history_recent_ms,
            rebuild_latest_ms,
            rebuild_p50_ms,
            rebuild_p75_ms,
            transition_hits_total,
            transition_misses_total,
            transition_hit_rate_percent,
            transition_low_hitrate_pressure: self.transition_low_hitrate_pressure(),
            prerender_queue_pressure: self.prerender_pending.len() >= 4,
            transition_metrics,
        }
    }

    fn transition_overlap_prerender_spec_at(
        &self,
        timeline_pos_ns: u64,
        active: &[usize],
    ) -> Option<TransitionPrerenderSpec> {
        if active.len() != 2 {
            return None;
        }
        for &outgoing_idx in active {
            let outgoing = self.clips.get(outgoing_idx)?;
            let Some(xfade_transition) =
                Self::supported_transition_prerender_kind(outgoing.transition_after.as_str())
            else {
                continue;
            };
            let Some(window) = Self::outgoing_transition_window_for_clip(outgoing) else {
                continue;
            };
            if !window.contains(timeline_pos_ns) {
                continue;
            }
            let incoming_idx = Self::adjacent_next_same_track(&self.clips, outgoing_idx)
                .filter(|idx| active.contains(idx))?;
            let incoming = self.clips.get(incoming_idx)?;
            if outgoing.chroma_key_enabled || incoming.chroma_key_enabled {
                continue;
            }
            let outgoing_input = active.iter().position(|&idx| idx == outgoing_idx)?;
            let incoming_input = active.iter().position(|&idx| idx == incoming_idx)?;
            return Some(TransitionPrerenderSpec {
                outgoing_input,
                incoming_input,
                xfade_transition: xfade_transition.to_string(),
                duration_ns: outgoing.transition_after_ns,
                before_cut_ns: window.before_cut_ns,
                after_cut_ns: window.after_cut_ns,
            });
        }
        None
    }

    fn active_supports_background_prerender_at(
        &self,
        timeline_pos_ns: u64,
        active: &[usize],
    ) -> bool {
        let has_scoped_adjustment = self.clips.iter().any(|clip| {
            clip.is_adjustment
                && timeline_pos_ns >= clip.timeline_start_ns
                && timeline_pos_ns < clip.timeline_end_ns()
                && (clip.masks.iter().any(|mask| mask.enabled)
                    || !AdjustmentScopeShape::from_transform(
                        self.project_width,
                        self.project_height,
                        clip.scale_at_timeline_ns(timeline_pos_ns),
                        clip.position_x_at_timeline_ns(timeline_pos_ns),
                        clip.position_y_at_timeline_ns(timeline_pos_ns),
                        clip.rotate_at_timeline_ns(timeline_pos_ns) as f64,
                        clip.crop_left_at_timeline_ns(timeline_pos_ns),
                        clip.crop_right_at_timeline_ns(timeline_pos_ns),
                        clip.crop_top_at_timeline_ns(timeline_pos_ns),
                        clip.crop_bottom_at_timeline_ns(timeline_pos_ns),
                    )
                    .is_full_frame(self.project_width, self.project_height))
        });
        if has_scoped_adjustment {
            return false;
        }
        if self.active_has_unsupported_background_prerender_features(active) {
            return false;
        }
        if active.len() >= 3 {
            return true;
        }
        matches!(self.playback_priority, PlaybackPriority::Smooth)
            && self
                .transition_overlap_prerender_spec_at(timeline_pos_ns, active)
                .is_some()
    }

    fn prewarm_incoming_clip_resources(&mut self, clip: &ProgramClip, timeline_pos: u64) {
        let (effective_path, _, _, animated_svg_rendered) = self.resolve_source_path_for_clip(clip);
        let uri = format!("file://{}", effective_path);
        // Build a lightweight sidecar pipeline that actually decodes the
        // first frame at the clip's source position.  This warms the OS
        // page cache, codec initialisation, and container demux state.
        // The pipeline runs asynchronously — we set it to Paused and seek,
        // then store it; teardown happens at rebuild or load.
        let sidecar = gst::Pipeline::new();
        let Ok(decoder) = gst::ElementFactory::make("uridecodebin")
            .property("uri", &uri)
            .build()
        else {
            return;
        };
        let Ok(fakesink) = gst::ElementFactory::make("fakesink")
            .property("sync", false)
            .build()
        else {
            return;
        };
        if sidecar.add_many([&decoder, &fakesink]).is_err() {
            return;
        }
        // Dynamic pad linking: connect first video pad to fakesink.
        let fs = fakesink.clone();
        decoder.connect_pad_added(move |_dec, pad| {
            let caps = pad.current_caps().or_else(|| Some(pad.query_caps(None)));
            if let Some(caps) = caps {
                if let Some(s) = caps.structure(0) {
                    if s.name().starts_with("video/") {
                        if let Some(sink_pad) = fs.static_pad("sink") {
                            if !sink_pad.is_linked() {
                                let _ = pad.link(&sink_pad);
                            }
                        }
                    }
                }
            }
        });
        let _ = sidecar.set_state(gst::State::Paused);
        // Seek to the clip's source position so the decoder decodes from
        // the right keyframe, not from position 0.
        let source_ns =
            Self::effective_source_pos_ns_for_clip(clip, timeline_pos, animated_svg_rendered);
        let _ = decoder.seek(
            1.0,
            gst::SeekFlags::FLUSH | gst::SeekFlags::ACCURATE,
            gst::SeekType::Set,
            gst::ClockTime::from_nseconds(source_ns),
            gst::SeekType::None,
            gst::ClockTime::NONE,
        );
        // Also warm the effects-bin construction path.
        let (proc_w, proc_h) = self.preview_processing_dimensions();
        let (effects_bin, ..) = Self::build_effects_bin(
            clip,
            animated_svg_rendered,
            proc_w,
            proc_h,
            self.project_width,
            self.project_height,
            None,
            Arc::new(Mutex::new(Vec::new())),
            Arc::new(Mutex::new(None)),
        );
        let _ = effects_bin.set_state(gst::State::Null);
        const MAX_PREPREROLL_SIDECARS: usize = 8;
        if self.prepreroll_sidecars.len() >= MAX_PREPREROLL_SIDECARS {
            if let Some(old) = self.prepreroll_sidecars.get(0) {
                let _ = old.set_state(gst::State::Null);
            }
            self.prepreroll_sidecars.remove(0);
        }
        self.prepreroll_sidecars.push(sidecar);
    }

    /// Tear down all pre-preroll sidecar pipelines.
    fn teardown_prepreroll_sidecars(&mut self) {
        for sidecar in self.prepreroll_sidecars.drain(..) {
            let _ = sidecar.set_state(gst::State::Null);
        }
    }

    fn poll_background_prerender_results(&mut self) {
        while let Ok(result) = self.prerender_result_rx.try_recv() {
            if result.generation != self.prerender_generation {
                if (!result.cache_persistent || !self.background_prerender)
                    && Path::new(&result.path).exists()
                {
                    remove_prerender_segment_files(Path::new(&result.path));
                }
                continue;
            }
            self.prerender_pending.remove(&result.key);
            self.prerender_pending_priority.remove(&result.key);
            // Discard results for superseded jobs — their output is stale.
            if self.prerender_superseded.remove(&result.key) {
                log::debug!(
                    "background_prerender: discarding superseded key={} path={}",
                    result.key,
                    result.path
                );
                if Path::new(&result.path).exists() {
                    remove_prerender_segment_files(Path::new(&result.path));
                }
                continue;
            }
            if !self.background_prerender {
                if Path::new(&result.path).exists() {
                    remove_prerender_segment_files(Path::new(&result.path));
                }
                continue;
            }
            if result.success && Path::new(&result.path).exists() {
                log::info!(
                    "background_prerender: ready key={} range={}..{} path={}",
                    result.key,
                    result.start_ns,
                    result.end_ns,
                    result.path
                );
                self.prerender_runtime_files.insert(result.path.clone());
                self.prerender_segments.insert(
                    result.key.clone(),
                    PrerenderSegment {
                        key: result.key.clone(),
                        path: result.path.clone(),
                        start_ns: result.start_ns,
                        end_ns: result.end_ns,
                        signature: result.signature,
                    },
                );
                self.prune_prerender_segment_cache();
                self.prune_prerender_cache_root_files();
                if self.state == PlayerState::Playing
                    && !self.slots.iter().any(|s| s.is_prerender_slot)
                {
                    let active = self.clips_active_at(self.timeline_pos_ns);
                    if self.active_supports_background_prerender_at(self.timeline_pos_ns, &active) {
                        let current_sig = self.prerender_signature_for_active(&active);
                        if current_sig == result.signature
                            && self.timeline_pos_ns >= result.start_ns
                            && self.timeline_pos_ns < result.end_ns
                        {
                            self.pending_prerender_promote = true;
                            log::info!(
                                "background_prerender: promote requested at timeline_pos={} key={}",
                                self.timeline_pos_ns,
                                result.key
                            );
                        }
                    }
                }
            } else {
                log::warn!(
                    "background_prerender: failed key={} range={}..{} path={}",
                    result.key,
                    result.start_ns,
                    result.end_ns,
                    result.path
                );
                self.prerender_failed.insert(result.key);
            }
        }
    }

    fn hash_prerender_keyframes(hasher: &mut DefaultHasher, keyframes: &[NumericKeyframe]) {
        keyframes.len().hash(hasher);
        for keyframe in keyframes {
            keyframe.time_ns.hash(hasher);
            keyframe.value.to_bits().hash(hasher);
            keyframe.interpolation.hash(hasher);
        }
    }

    fn hash_transition_cache_state(
        hasher: &mut DefaultHasher,
        kind: &str,
        duration_ns: u64,
        alignment: TransitionAlignment,
    ) {
        canonicalize_transition_kind(kind).hash(hasher);
        duration_ns.hash(hasher);
        alignment.hash(hasher);
    }

    fn hash_prerender_clip_state(
        hasher: &mut DefaultHasher,
        clip: &ProgramClip,
        effective_source_path: &str,
    ) {
        clip.track_index.hash(hasher);
        clip.timeline_start_ns.hash(hasher);
        clip.timeline_end_ns().hash(hasher);
        effective_source_path.hash(hasher);
        clip.source_in_ns.hash(hasher);
        clip.source_out_ns.hash(hasher);
        clip.speed.to_bits().hash(hasher);
        Self::hash_prerender_keyframes(hasher, &clip.speed_keyframes);
        clip.reverse.hash(hasher);
        clip.freeze_frame.hash(hasher);
        clip.freeze_frame_source_ns.hash(hasher);
        clip.freeze_frame_hold_duration_ns.hash(hasher);
        clip.crop_left.hash(hasher);
        Self::hash_prerender_keyframes(hasher, &clip.crop_left_keyframes);
        clip.crop_right.hash(hasher);
        Self::hash_prerender_keyframes(hasher, &clip.crop_right_keyframes);
        clip.crop_top.hash(hasher);
        Self::hash_prerender_keyframes(hasher, &clip.crop_top_keyframes);
        clip.crop_bottom.hash(hasher);
        Self::hash_prerender_keyframes(hasher, &clip.crop_bottom_keyframes);
        clip.rotate.hash(hasher);
        Self::hash_prerender_keyframes(hasher, &clip.rotate_keyframes);
        clip.flip_h.hash(hasher);
        clip.flip_v.hash(hasher);
        clip.scale.to_bits().hash(hasher);
        Self::hash_prerender_keyframes(hasher, &clip.scale_keyframes);
        clip.position_x.to_bits().hash(hasher);
        Self::hash_prerender_keyframes(hasher, &clip.position_x_keyframes);
        clip.position_y.to_bits().hash(hasher);
        Self::hash_prerender_keyframes(hasher, &clip.position_y_keyframes);
        clip.opacity.to_bits().hash(hasher);
        Self::hash_prerender_keyframes(hasher, &clip.opacity_keyframes);
        // Transition settings must participate in prerender cache keys so
        // changing a boundary type/duration/alignment cannot reuse a stale segment.
        Self::hash_transition_cache_state(
            hasher,
            &clip.transition_after,
            clip.transition_after_ns,
            clip.transition_alignment,
        );
        // Volume, pan, and EQ are excluded from the prerender signature.
        // Audio is baked at current levels; changes don't invalidate the
        // cached segment — the next idle render cycle picks up new levels.
        clip.brightness.to_bits().hash(hasher);
        Self::hash_prerender_keyframes(hasher, &clip.brightness_keyframes);
        clip.contrast.to_bits().hash(hasher);
        Self::hash_prerender_keyframes(hasher, &clip.contrast_keyframes);
        clip.saturation.to_bits().hash(hasher);
        Self::hash_prerender_keyframes(hasher, &clip.saturation_keyframes);
        clip.temperature.to_bits().hash(hasher);
        Self::hash_prerender_keyframes(hasher, &clip.temperature_keyframes);
        clip.tint.to_bits().hash(hasher);
        Self::hash_prerender_keyframes(hasher, &clip.tint_keyframes);
        clip.denoise.to_bits().hash(hasher);
        clip.sharpness.to_bits().hash(hasher);
        clip.blur.to_bits().hash(hasher);
        Self::hash_prerender_keyframes(hasher, &clip.blur_keyframes);
        clip.shadows.to_bits().hash(hasher);
        clip.midtones.to_bits().hash(hasher);
        clip.highlights.to_bits().hash(hasher);
        clip.lut_paths.hash(hasher);
        clip.exposure.to_bits().hash(hasher);
        clip.black_point.to_bits().hash(hasher);
        clip.highlights_warmth.to_bits().hash(hasher);
        clip.highlights_tint.to_bits().hash(hasher);
        clip.midtones_warmth.to_bits().hash(hasher);
        clip.midtones_tint.to_bits().hash(hasher);
        clip.shadows_warmth.to_bits().hash(hasher);
        clip.shadows_tint.to_bits().hash(hasher);
        clip.chroma_key_enabled.hash(hasher);
        clip.chroma_key_color.hash(hasher);
        clip.chroma_key_tolerance.to_bits().hash(hasher);
        clip.chroma_key_softness.to_bits().hash(hasher);
        clip.is_title.hash(hasher);
        clip.title_text.hash(hasher);
        clip.title_font.hash(hasher);
        clip.title_color.hash(hasher);
        clip.title_x.to_bits().hash(hasher);
        clip.title_y.to_bits().hash(hasher);
        clip.title_outline_width.to_bits().hash(hasher);
        clip.title_outline_color.hash(hasher);
        clip.title_shadow.hash(hasher);
        clip.title_shadow_color.hash(hasher);
        clip.title_shadow_offset_x.to_bits().hash(hasher);
        clip.title_shadow_offset_y.to_bits().hash(hasher);
        clip.title_bg_box.hash(hasher);
        clip.title_bg_box_color.hash(hasher);
        clip.title_bg_box_padding.to_bits().hash(hasher);
        clip.title_clip_bg_color.hash(hasher);
        clip.title_secondary_text.hash(hasher);
        (clip.blend_mode as u8).hash(hasher);
        clip.frei0r_effects.len().hash(hasher);
        for effect in &clip.frei0r_effects {
            effect.id.hash(hasher);
            effect.plugin_name.hash(hasher);
            effect.enabled.hash(hasher);
            for (key, value) in &effect.params {
                key.hash(hasher);
                value.to_bits().hash(hasher);
            }
            for (key, value) in &effect.string_params {
                key.hash(hasher);
                value.hash(hasher);
            }
        }
        clip.slow_motion_interp.hash(hasher);
        clip.motion_blur_enabled.hash(hasher);
        clip.motion_blur_shutter_angle.to_bits().hash(hasher);
        clip.masks.len().hash(hasher);
        for mask in &clip.masks {
            mask.enabled.hash(hasher);
            (mask.shape as u8).hash(hasher);
            mask.center_x.to_bits().hash(hasher);
            mask.center_y.to_bits().hash(hasher);
            mask.width.to_bits().hash(hasher);
            mask.height.to_bits().hash(hasher);
            mask.rotation.to_bits().hash(hasher);
            mask.feather.to_bits().hash(hasher);
            mask.expansion.to_bits().hash(hasher);
            mask.invert.hash(hasher);
            mask.center_x_keyframes.len().hash(hasher);
            mask.center_y_keyframes.len().hash(hasher);
            mask.width_keyframes.len().hash(hasher);
            mask.height_keyframes.len().hash(hasher);
            mask.rotation_keyframes.len().hash(hasher);
            mask.feather_keyframes.len().hash(hasher);
            mask.expansion_keyframes.len().hash(hasher);
        }
    }

    fn prerender_signature_for_active(&self, active: &[usize]) -> u64 {
        let mut hasher = DefaultHasher::new();
        BACKGROUND_PRERENDER_CACHE_VERSION.hash(&mut hasher);
        self.project_width.hash(&mut hasher);
        self.project_height.hash(&mut hasher);
        self.preview_divisor.hash(&mut hasher);
        self.prerender_preset.as_str().hash(&mut hasher);
        self.prerender_crf.hash(&mut hasher);
        self.proxy_enabled.hash(&mut hasher);
        self.proxy_scale_divisor.hash(&mut hasher);
        for &idx in active {
            if let Some(c) = self.clips.get(idx) {
                Self::hash_prerender_clip_state(
                    &mut hasher,
                    c,
                    &self.effective_source_path_for_clip(c),
                );
            }
        }
        hasher.finish()
    }

    fn maybe_request_background_prerender_segment(&mut self, boundary_ns: u64, active: &[usize]) {
        const TRANSITION_OVERLAP_PAD_FRAMES: u64 = 2;
        // Suppress prerender while the user is actively dragging a transform
        // control; the intermediate states are ephemeral and would waste
        // CPU/disk.  The final state is prerendered after the drag ends.
        if self.transform_live {
            return;
        }
        let transition_spec = self.transition_overlap_prerender_spec_at(boundary_ns, active);
        let transition_prerender_allowed =
            transition_spec.is_some() && matches!(self.playback_priority, PlaybackPriority::Smooth);
        if !self.background_prerender || (active.len() < 3 && !transition_prerender_allowed) {
            return;
        }
        if self.active_has_unsupported_background_prerender_features(active) {
            return;
        }
        let next_boundary = self
            .next_video_boundary_after(boundary_ns.saturating_add(1))
            .unwrap_or_else(|| boundary_ns.saturating_add(4_000_000_000));
        let frame_pad_ns = self
            .frame_duration_ns
            .max(1)
            .saturating_mul(TRANSITION_OVERLAP_PAD_FRAMES);
        let segment_start_ns = if transition_spec.is_some() {
            boundary_ns.saturating_sub(frame_pad_ns)
        } else {
            boundary_ns
        };
        let unclamped_end_ns = if transition_spec.is_some() {
            next_boundary.saturating_add(frame_pad_ns)
        } else {
            next_boundary
        };
        let segment_end_ns = if self.timeline_dur_ns > 0 {
            unclamped_end_ns.min(self.timeline_dur_ns)
        } else {
            unclamped_end_ns
        };
        if segment_end_ns <= segment_start_ns.saturating_add(200_000_000) {
            return;
        }
        let transition_offset_ns = boundary_ns.saturating_sub(segment_start_ns);
        let signature = self.prerender_signature_for_active(active);
        let key = format!(
            "seg_v{}_{:016x}_{}_{}",
            BACKGROUND_PRERENDER_CACHE_VERSION, signature, segment_start_ns, segment_end_ns
        );
        let Some(manifest) = self.build_prerender_manifest_for_segment(
            &key,
            signature,
            segment_start_ns,
            segment_end_ns,
            active,
        ) else {
            return;
        };
        if self.prerender_segments.contains_key(&key)
            || self.prerender_pending.contains(&key)
            || self.prerender_failed.contains(&key)
        {
            return;
        }
        // Supersede any stale pending job for the same boundary region.
        // This keeps at most one in-flight job per region and frees a queue
        // slot so the fresh request passes admission.
        let boundary_region = (segment_start_ns, segment_end_ns);
        if let Some(old_key) = self
            .prerender_boundary_latest
            .insert(boundary_region, key.clone())
        {
            if old_key != key && self.prerender_pending.remove(&old_key) {
                self.prerender_pending_priority.remove(&old_key);
                self.prerender_superseded.insert(old_key.clone());
                log::debug!(
                    "background_prerender: superseded key={} for boundary {:?}",
                    old_key,
                    boundary_region
                );
            }
        }
        let path = self.prerender_cache_root.join(format!("{key}.mp4"));
        if path.exists() {
            if self.cached_prerender_segment_matches_manifest(
                &path,
                &key,
                signature,
                segment_start_ns,
                segment_end_ns,
                &manifest.inputs,
            ) {
                self.prerender_runtime_files
                    .insert(path.to_string_lossy().to_string());
                self.prerender_segments.insert(
                    key.clone(),
                    PrerenderSegment {
                        key,
                        path: path.to_string_lossy().to_string(),
                        start_ns: segment_start_ns,
                        end_ns: segment_end_ns,
                        signature,
                    },
                );
                self.prune_prerender_segment_cache();
                return;
            }
            remove_prerender_segment_files(&path);
            self.forget_prerender_segment_path(&path);
        }
        let duration_ns = segment_end_ns.saturating_sub(segment_start_ns);
        // Separate adjustment clips from regular inputs for prerender.
        let adjustment_clips_for_prerender: Vec<ProgramClip> = active
            .iter()
            .filter_map(|&idx| self.clips.get(idx))
            .filter(|c| c.is_adjustment)
            .cloned()
            .collect();
        let inputs: Vec<(ProgramClip, String, u64, bool, bool)> = active
            .iter()
            .filter_map(|&idx| {
                self.clips.get(idx).and_then(|clip| {
                    if clip.is_adjustment {
                        return None;
                    }
                    let c = clip.clone();
                    let (path, source_is_proxy, _, animated_svg_rendered) =
                        self.resolve_source_path_for_clip(&c);
                    let source_ns = Self::effective_source_pos_ns_for_clip(
                        &c,
                        segment_start_ns,
                        animated_svg_rendered,
                    );
                    let has_audio = self.has_audio_for_path_fast(&path, c.has_embedded_audio());
                    Some((c, path, source_ns, has_audio, source_is_proxy))
                })
            })
            .collect();
        if inputs.is_empty() {
            return;
        }
        let fps = if self.frame_duration_ns > 0 {
            (1_000_000_000f64 / self.frame_duration_ns as f64)
                .round()
                .max(1.0) as u32
        } else {
            24
        };
        let output_path = path.to_string_lossy().to_string();
        let key_for_job = key.clone();
        let key_for_log = key.clone();
        let input_count = inputs.len();
        let request_priority = self.prerender_request_priority_score(boundary_ns, active);
        let pending_count = self.prerender_pending.len();
        let lowest_pending_priority = self.prerender_pending_priority.values().copied().min();
        if !Self::prerender_queue_allows_request(
            pending_count,
            lowest_pending_priority,
            request_priority,
        ) {
            log::debug!(
                "background_prerender: skip queue admission key={} priority={} pending={} lowest_pending={:?}",
                key_for_log,
                request_priority,
                pending_count,
                lowest_pending_priority
            );
            return;
        }
        let tx = self.prerender_result_tx.clone();
        let prerender_divisor = if self.proxy_enabled {
            self.proxy_scale_divisor.max(1)
        } else {
            1
        };
        let out_w = (self.project_width / prerender_divisor).max(2);
        let out_h = (self.project_height / prerender_divisor).max(2);
        let transition_spec_for_job = transition_spec.clone();
        let adj_clips_for_job = adjustment_clips_for_prerender;
        let manifest_for_job = manifest;
        let cache_persistent = self.prerender_cache_persistent;
        let generation = self.prerender_generation;
        let prerender_preset = self.prerender_preset.clone();
        let prerender_crf = self.prerender_crf;
        std::thread::spawn(move || {
            let output_path_buf = PathBuf::from(&output_path);
            let mut success = Self::render_prerender_segment_video_file(
                &output_path,
                &inputs,
                &adj_clips_for_job,
                duration_ns,
                out_w,
                out_h,
                fps,
                transition_spec_for_job.as_ref(),
                transition_offset_ns,
                prerender_preset,
                prerender_crf,
            );
            if success
                && Self::write_prerender_manifest_for_path(&output_path_buf, &manifest_for_job)
                    .is_err()
            {
                success = false;
            }
            if !success {
                remove_prerender_segment_files(&output_path_buf);
            }
            let _ = tx.send(PrerenderJobResult {
                key: key_for_job,
                path: output_path,
                start_ns: segment_start_ns,
                end_ns: segment_end_ns,
                signature,
                success,
                generation,
                cache_persistent,
            });
        });
        self.prerender_pending.insert(key);
        self.prerender_pending_priority
            .insert(key_for_log.clone(), request_priority);
        self.prerender_total_requested += 1;
        log::info!(
            "background_prerender: queued key={} range={}..{} clips={} priority={} pending={}",
            key_for_log,
            segment_start_ns,
            segment_end_ns,
            input_count,
            request_priority,
            self.prerender_pending.len()
        );
    }

    fn segment_distance_to_timeline_ns(timeline_pos_ns: u64, start_ns: u64, end_ns: u64) -> u64 {
        if timeline_pos_ns < start_ns {
            start_ns.saturating_sub(timeline_pos_ns)
        } else if timeline_pos_ns >= end_ns {
            timeline_pos_ns.saturating_sub(end_ns)
        } else {
            0
        }
    }

    fn prune_prerender_segment_cache(&mut self) {
        if self.prerender_segments.len() <= MAX_READY_PRERENDER_SEGMENTS {
            return;
        }
        let protected_key = self.current_prerender_segment_key.as_deref();
        while self.prerender_segments.len() > MAX_READY_PRERENDER_SEGMENTS {
            let victim_key = self
                .prerender_segments
                .values()
                .filter(|seg| Some(seg.key.as_str()) != protected_key)
                .max_by(|a, b| {
                    let da = Self::segment_distance_to_timeline_ns(
                        self.timeline_pos_ns,
                        a.start_ns,
                        a.end_ns,
                    );
                    let db = Self::segment_distance_to_timeline_ns(
                        self.timeline_pos_ns,
                        b.start_ns,
                        b.end_ns,
                    );
                    da.cmp(&db).then_with(|| a.end_ns.cmp(&b.end_ns))
                })
                .map(|seg| seg.key.clone());
            let Some(victim_key) = victim_key else {
                break;
            };
            if let Some(victim) = self.prerender_segments.remove(&victim_key) {
                remove_prerender_segment_files(Path::new(&victim.path));
                self.prerender_runtime_files.remove(&victim.path);
                log::debug!(
                    "background_prerender: evicted key={} range={}..{} path={}",
                    victim.key,
                    victim.start_ns,
                    victim.end_ns,
                    victim.path
                );
            } else {
                break;
            }
        }
    }

    fn find_prerender_segment_for(
        &mut self,
        timeline_pos: u64,
        active: &[usize],
        signature: u64,
    ) -> Option<PrerenderSegment> {
        let cached = self
            .prerender_segments
            .values()
            .filter(|seg| {
                seg.signature == signature
                    && timeline_pos >= seg.start_ns
                    && timeline_pos < seg.end_ns
                    && Path::new(&seg.path).exists()
            })
            .max_by_key(|seg| seg.start_ns)
            .cloned();
        if cached.is_some() {
            return cached;
        }
        let expected_inputs = self.prerender_manifest_inputs_for_active(active)?;
        self.discover_prerender_segment_for(timeline_pos, signature, &expected_inputs)
    }

    fn prewarm_upcoming_boundary(&mut self, timeline_pos_ns: u64) {
        const PREWARM_WINDOW_NS: u64 = 900_000_000;
        const BACKGROUND_PREWARM_WINDOW_NS: u64 = 3_000_000_000;
        const BACKGROUND_PLAYING_LOOKAHEAD_NS: u64 = 12_000_000_000;
        const BACKGROUND_PLAYING_LOOKAHEAD_SMOOTH_NS: u64 = 16_000_000_000;
        const LOW_HITRATE_LOOKAHEAD_BONUS_NS: u64 = 2_000_000_000;
        const BACKGROUND_PLAYING_MAX_BOUNDARIES: usize = 2;
        const BACKGROUND_PLAYING_MAX_BOUNDARIES_SMOOTH: usize = 3;
        const BACKGROUND_PLAYING_MAX_BOUNDARIES_LOW_HITRATE: usize = 4;
        const TRANSITION_PRIORITY_QUEUE_TIGHT_PENDING: usize = 2;
        const TRANSITION_PRIORITY_SCAN_EXTRA: usize = 3;
        let prerender_playback_active = self.slots.iter().any(|s| s.is_prerender_slot);
        if self.state != PlayerState::Playing {
            self.prewarmed_boundary_ns = None;
            self.teardown_prepreroll_sidecars();
            return;
        }
        if !self.realtime_preview && !self.background_prerender {
            self.prewarmed_boundary_ns = None;
            self.teardown_prepreroll_sidecars();
            return;
        }
        if self.background_prerender {
            let smooth_mode = matches!(self.playback_priority, PlaybackPriority::Smooth);
            let queued_jobs = self.prerender_pending.len();
            let low_hitrate_pressure = self.transition_low_hitrate_pressure();
            let max_boundaries = if queued_jobs >= 4 {
                BACKGROUND_PLAYING_MAX_BOUNDARIES
            } else if smooth_mode {
                if low_hitrate_pressure {
                    BACKGROUND_PLAYING_MAX_BOUNDARIES_LOW_HITRATE
                } else {
                    BACKGROUND_PLAYING_MAX_BOUNDARIES_SMOOTH
                }
            } else {
                BACKGROUND_PLAYING_MAX_BOUNDARIES
            };
            let lookahead_ns = if smooth_mode {
                let base = BACKGROUND_PLAYING_LOOKAHEAD_SMOOTH_NS;
                if low_hitrate_pressure {
                    base.saturating_add(LOW_HITRATE_LOOKAHEAD_BONUS_NS)
                } else {
                    base
                }
            } else {
                BACKGROUND_PLAYING_LOOKAHEAD_NS
            };
            let mut candidates = self.collect_upcoming_prerender_boundaries(
                timeline_pos_ns,
                lookahead_ns,
                max_boundaries + TRANSITION_PRIORITY_SCAN_EXTRA,
            );
            let queue_budget_tight =
                smooth_mode && queued_jobs >= TRANSITION_PRIORITY_QUEUE_TIGHT_PENDING;
            Self::sort_prerender_candidates(
                &mut candidates,
                queue_budget_tight,
                timeline_pos_ns,
                lookahead_ns,
            );
            for (scan_boundary_ns, scan_active, _) in candidates.into_iter().take(max_boundaries) {
                self.maybe_request_background_prerender_segment(scan_boundary_ns, &scan_active);
            }
        }
        let prewarm_window_ns = if self.background_prerender {
            let scale = self.rebuild_wait_scale().max(1.0).min(1.4);
            (BACKGROUND_PREWARM_WINDOW_NS as f64 * scale) as u64
        } else {
            PREWARM_WINDOW_NS
        };
        let Some(boundary_ns) = self.next_video_boundary_after(timeline_pos_ns) else {
            self.prewarmed_boundary_ns = None;
            self.teardown_prepreroll_sidecars();
            return;
        };
        if boundary_ns.saturating_sub(timeline_pos_ns) > prewarm_window_ns
            && !prerender_playback_active
        {
            self.prewarmed_boundary_ns = None;
            if !self.background_prerender {
                self.teardown_prepreroll_sidecars();
            }
            return;
        }
        if self.prewarmed_boundary_ns == Some(boundary_ns) {
            return;
        }
        let current_active = self.clips_active_at(timeline_pos_ns);
        let boundary_active = self.clips_active_at(boundary_ns);
        self.maybe_request_background_prerender_segment(boundary_ns, &boundary_active);

        // Skip prewarming when continuing decoders will handle this boundary.
        if !prerender_playback_active
            && boundary_active.len() == self.slots.len()
            && !self.slots.is_empty()
        {
            let all_same_source =
                boundary_active
                    .iter()
                    .zip(self.slots.iter())
                    .all(|(&idx, slot)| {
                        let boundary_clip = &self.clips[idx];
                        let current_clip = &self.clips[slot.clip_idx];
                        self.effective_source_path_for_clip(boundary_clip)
                            == self.effective_source_path_for_clip(current_clip)
                    });
            if all_same_source {
                log::debug!(
                    "prewarm: skipping — continuing decoders will handle boundary at {}",
                    boundary_ns
                );
                self.prewarmed_boundary_ns = Some(boundary_ns);
                return;
            }
        }

        for idx in boundary_active {
            let clip = self.clips[idx].clone();
            let incoming = if prerender_playback_active {
                true
            } else {
                !current_active.contains(&idx)
            };
            if clip.has_embedded_audio() {
                let effective_path = self.effective_source_path_for_clip(&clip);
                let _ = self.probe_has_audio_stream_cached(&effective_path);
            }
            if incoming {
                self.prewarm_incoming_clip_resources(&clip, boundary_ns);
            }
        }
        log::debug!(
            "prewarm_upcoming_boundary: timeline_pos={} boundary={} prerender_active={}",
            timeline_pos_ns,
            boundary_ns,
            prerender_playback_active
        );
        self.prewarmed_boundary_ns = Some(boundary_ns);
    }

    fn maybe_request_idle_background_prerender(&mut self) {
        const IDLE_SCAN_INTERVAL_MS: u64 = 750;
        const IDLE_LOOKAHEAD_NS: u64 = 12_000_000_000;
        const IDLE_LOOKAHEAD_SMOOTH_NS: u64 = 16_000_000_000;
        const LOW_HITRATE_IDLE_LOOKAHEAD_BONUS_NS: u64 = 2_000_000_000;
        const IDLE_MAX_BOUNDARIES: usize = 2;
        const IDLE_MAX_BOUNDARIES_SMOOTH: usize = 3;
        const IDLE_MAX_BOUNDARIES_LOW_HITRATE: usize = 4;
        const TRANSITION_PRIORITY_QUEUE_TIGHT_PENDING: usize = 2;
        const TRANSITION_PRIORITY_SCAN_EXTRA: usize = 3;

        if !self.background_prerender {
            self.last_idle_prerender_scan_at = None;
            return;
        }
        if self.clips.is_empty() {
            return;
        }
        if let Some(last_scan) = self.last_idle_prerender_scan_at {
            if last_scan.elapsed() < Duration::from_millis(IDLE_SCAN_INTERVAL_MS) {
                return;
            }
        }
        self.last_idle_prerender_scan_at = Some(Instant::now());
        let smooth_mode = matches!(self.playback_priority, PlaybackPriority::Smooth);
        let queued_jobs = self.prerender_pending.len();
        let low_hitrate_pressure = self.transition_low_hitrate_pressure();
        let idle_max_boundaries = if queued_jobs >= 4 {
            IDLE_MAX_BOUNDARIES
        } else if smooth_mode {
            if low_hitrate_pressure {
                IDLE_MAX_BOUNDARIES_LOW_HITRATE
            } else {
                IDLE_MAX_BOUNDARIES_SMOOTH
            }
        } else {
            IDLE_MAX_BOUNDARIES
        };
        let idle_lookahead_ns = if smooth_mode {
            let base = IDLE_LOOKAHEAD_SMOOTH_NS;
            if low_hitrate_pressure {
                base.saturating_add(LOW_HITRATE_IDLE_LOOKAHEAD_BONUS_NS)
            } else {
                base
            }
        } else {
            IDLE_LOOKAHEAD_NS
        };

        let active_now = self.clips_active_at(self.timeline_pos_ns);
        self.maybe_request_background_prerender_segment(self.timeline_pos_ns, &active_now);
        let mut candidates = self.collect_upcoming_prerender_boundaries(
            self.timeline_pos_ns,
            idle_lookahead_ns,
            idle_max_boundaries + TRANSITION_PRIORITY_SCAN_EXTRA,
        );
        let queue_budget_tight =
            smooth_mode && queued_jobs >= TRANSITION_PRIORITY_QUEUE_TIGHT_PENDING;
        Self::sort_prerender_candidates(
            &mut candidates,
            queue_budget_tight,
            self.timeline_pos_ns,
            idle_lookahead_ns,
        );
        for (boundary_ns, boundary_active, _) in candidates.into_iter().take(idle_max_boundaries) {
            self.maybe_request_background_prerender_segment(boundary_ns, &boundary_active);
        }
    }

    /// Find the VideoSlot corresponding to `clip_idx`, if any.
    fn slot_for_clip(&self, clip_idx: usize) -> Option<&VideoSlot> {
        self.slots
            .iter()
            .find(|s| s.clip_idx == clip_idx && !s.is_prerender_slot)
    }

    #[allow(dead_code)]
    fn should_block_preroll(&self) -> bool {
        !matches!(self.playback_priority, PlaybackPriority::Smooth)
    }

    fn clip_seek_flags(&self) -> gst::SeekFlags {
        match self.playback_priority {
            PlaybackPriority::Accurate => gst::SeekFlags::FLUSH | gst::SeekFlags::ACCURATE,
            PlaybackPriority::Balanced | PlaybackPriority::Smooth => {
                gst::SeekFlags::FLUSH | gst::SeekFlags::KEY_UNIT
            }
        }
    }

    fn paused_seek_flags() -> gst::SeekFlags {
        gst::SeekFlags::FLUSH | gst::SeekFlags::ACCURATE
    }

    fn transition_overlap_active_now(&self) -> bool {
        if self.slots.len() < 2 {
            return false;
        }
        let timeline_pos_ns = self.timeline_pos_ns;
        self.slots.iter().any(|slot| {
            !slot.hidden
                && !slot.is_prerender_slot
                && self
                    .compute_transition_state(slot.clip_idx, timeline_pos_ns)
                    .is_some()
        })
    }

    fn should_drop_late_for_playback(&self) -> bool {
        Self::should_drop_late_for_playback_mode(
            self.state == PlayerState::Playing,
            self.transform_live,
            &self.playback_priority,
            self.slots.len(),
            self.transition_overlap_active_now(),
        )
    }

    fn should_drop_late_for_playback_mode(
        is_playing: bool,
        transform_live: bool,
        playback_priority: &PlaybackPriority,
        slot_count: usize,
        transition_overlap_active: bool,
    ) -> bool {
        if !is_playing || transform_live {
            return false;
        }
        if slot_count >= 3 {
            return true;
        }
        // Only enable drop-late for active transition overlaps in Smooth
        // mode (2 clips composited together).  Enabling it for single-slot
        // playback removes backpressure, letting the compositor spin at
        // thousands of fps — the leaky display queue and QoS sink then drop
        // nearly every frame, producing ~1-2 displayed fps instead of 30.
        matches!(playback_priority, PlaybackPriority::Smooth) && transition_overlap_active
    }

    fn update_drop_late_policy(&mut self) {
        let should_drop_late = self.should_drop_late_for_playback();
        if should_drop_late == self.playback_drop_late_active {
            return;
        }
        self.playback_drop_late_active = should_drop_late;
        if let Some(ref q) = self.display_queue {
            if should_drop_late {
                q.set_property_from_str("leaky", "downstream");
                q.set_property("max-size-buffers", 1u32);
            } else {
                q.set_property_from_str("leaky", "no");
                q.set_property("max-size-buffers", 3u32);
            }
        }
        if self.display_sink.find_property("qos").is_some() {
            self.display_sink.set_property("qos", should_drop_late);
        }
        if self.display_sink.find_property("max-lateness").is_some() {
            let max_lateness: i64 = if should_drop_late { 40_000_000 } else { -1 };
            self.display_sink.set_property("max-lateness", max_lateness);
        }
        log::info!(
            "update_drop_late_policy: active={} slots={}",
            should_drop_late,
            self.slots.len()
        );
    }

    fn update_slot_queue_policy(&mut self) {
        let should_drop_late = self.should_drop_late_for_playback();
        if should_drop_late == self.slot_queue_drop_late_active {
            return;
        }
        self.slot_queue_drop_late_active = should_drop_late;
        for slot in &self.slots {
            if let Some(ref q) = slot.slot_queue {
                if should_drop_late {
                    q.set_property_from_str("leaky", "downstream");
                } else {
                    q.set_property_from_str("leaky", "no");
                }
            }
        }
        log::info!(
            "update_slot_queue_policy: active={} slots={}",
            should_drop_late,
            self.slots.len()
        );
    }

    fn should_prioritize_ui_responsiveness(&self) -> bool {
        self.state != PlayerState::Playing
    }

    /// Returns true when the user is actively scrubbing the timeline — rapid
    /// seeks arriving faster than 300ms apart.  During scrubbing we use very
    /// tight wait budgets so the UI stays responsive, accepting that a few
    /// frames may arrive after the display updates.
    fn is_rapid_scrubbing(&self) -> bool {
        if let Some(last) = self.last_seek_instant {
            last.elapsed() < Duration::from_millis(300)
        } else {
            false
        }
    }

    fn effective_wait_timeout_ms(&self, requested_ms: u64) -> u64 {
        if self.state == PlayerState::Playing {
            // During active playback, keep waits bounded to minimise boundary stall,
            // but allow enough time for 3+ track compositor arrival so we don't
            // resume with audio-only/black-video handoff.
            let cap = if self.slots.len() >= 3 { 700 } else { 300 };
            let nominal = requested_ms.min(cap);
            let scale = self.rebuild_wait_scale();
            ((nominal as f64 * scale) as u64).max(60)
        } else if self.should_prioritize_ui_responsiveness() {
            // Paused (interactive scrubbing / editing): keep waits short so the
            // GTK main thread stays responsive.  During rapid scrubbing, use
            // very tight budgets — a briefly stale frame is much better than
            // 200ms+ of UI freeze per scrub step.
            let cap = if self.is_rapid_scrubbing() {
                if self.slots.len() >= 3 {
                    60
                } else {
                    30
                }
            } else {
                if self.slots.len() >= 3 {
                    220
                } else {
                    150
                }
            };
            requested_ms.min(cap)
        } else {
            requested_ms
        }
    }

    /// Record a completed rebuild duration into the telemetry ring buffer.
    fn record_rebuild_duration_ms(&mut self, ms: u64) {
        let idx = self.rebuild_history_cursor % self.rebuild_history_ms.len();
        self.rebuild_history_ms[idx] = ms;
        self.rebuild_history_cursor = self.rebuild_history_cursor.wrapping_add(1);
        if self.rebuild_history_count < self.rebuild_history_ms.len() {
            self.rebuild_history_count += 1;
        }
    }

    /// Compute adaptive arrival-wait budget from recent rebuild telemetry.
    /// Returns a factor in `[0.5, 1.5]` to scale nominal wait timeouts.
    /// Fast recent rebuilds yield tighter waits; slow/cold history is
    /// conservative.
    fn rebuild_wait_scale(&self) -> f64 {
        if self.rebuild_history_count < 2 {
            return 1.0; // not enough data — use nominal
        }
        let n = self.rebuild_history_count;
        let start = if n < self.rebuild_history_ms.len() {
            0
        } else {
            self.rebuild_history_cursor % self.rebuild_history_ms.len()
        };
        let mut sorted = Vec::with_capacity(n);
        for i in 0..n {
            sorted.push(self.rebuild_history_ms[(start + i) % self.rebuild_history_ms.len()]);
        }
        sorted.sort_unstable();
        // Use p75 as representative (resilient to one-off spikes).
        let p75 = sorted[(n * 3 / 4).min(n - 1)];
        // Map: ≤400ms → 0.6 (fast), 400–1200ms → linear 0.6–1.0, ≥1200ms → linear 1.0–1.5 capped.
        let scale = if p75 <= 400 {
            0.6
        } else if p75 <= 1200 {
            0.6 + 0.4 * ((p75 - 400) as f64 / 800.0)
        } else {
            (1.0 + 0.5 * ((p75 - 1200) as f64 / 2000.0)).min(1.5)
        };
        scale
    }

    /// Return an adaptive arrival-wait timeout in milliseconds for
    /// `wait_for_compositor_arrivals` during playback boundary rebuilds.
    /// Uses telemetry to shorten waits when recent rebuilds were fast.
    fn adaptive_arrival_wait_ms(&self, nominal_ms: u64) -> u64 {
        let scale = self.rebuild_wait_scale();
        let scaled = (nominal_ms as f64 * scale) as u64;
        // Enforce a minimum so we don't drop to zero.  100ms is sufficient
        // for pre-warmed sources on single-track sequential playback; the
        // previous 200ms floor added unnecessary latency at clip boundaries.
        scaled.max(100)
    }

    /// Wait for decoder slots to reach their target state (typically Paused).
    ///
    /// Only waits on **decoder** elements — NOT the full pipeline.
    /// `gtk4paintablesink` needs the GTK main context to complete Paused preroll
    /// (`gdk_paintable_invalidate_contents`); calling `pipeline.state()` from the
    /// main thread deadlocks once 3+ video tracks make decoding slow enough that
    /// the sink hasn't prerolled before we block.  Decoder-level waits avoid the
    /// deadlock while still ensuring frames are decoded and available at the
    /// compositor inputs.  The display sink completes preroll asynchronously when
    /// control returns to the GTK main loop.
    ///
    /// When 3+ slots are active, per-decoder timeouts are reduced to limit total
    /// main-thread blocking time (which prevents gtk4paintablesink from completing
    /// its preroll, causing a deadlock-like stall).
    fn wait_for_paused_preroll(&self) {
        let per_decoder_ms = if self.state == PlayerState::Playing {
            // Playback boundary rebuilds run on the GTK main thread; keep
            // per-decoder waits short to reduce handoff stutter.
            // Scale with telemetry: fast recent rebuilds get tighter waits.
            let nominal = if self.slots.len() >= 3 { 60u64 } else { 100 };
            let scale = self.rebuild_wait_scale();
            ((nominal as f64 * scale) as u64).max(30)
        } else if self.should_prioritize_ui_responsiveness() {
            // Responsiveness-first paused seeks: keep each per-decoder wait tiny
            // so the GTK main loop regains control quickly.
            (self.effective_wait_timeout_ms(220) / self.slots.len() as u64).max(45)
        } else {
            500
        };
        let timeout = gst::ClockTime::from_mseconds(per_decoder_ms);
        for slot in &self.slots {
            // Skip decoders that already reached Paused or Playing — avoids
            // redundant blocking time during boundary rebuilds.
            let (_, cur, _) = slot.decoder.state(gst::ClockTime::ZERO);
            if cur >= gst::State::Paused {
                continue;
            }
            let _ = slot.decoder.state(timeout);
        }
    }

    /// Snapshot each slot's compositor-arrival counter.  Call this BEFORE
    /// issuing per-decoder flush seeks.
    fn snapshot_arrival_seqs(&self) -> Vec<u64> {
        self.slots
            .iter()
            .map(|s| s.comp_arrival_seq.load(Ordering::Relaxed))
            .collect()
    }

    /// Spin-wait (with short sleeps) until every slot's compositor-arrival
    /// counter has advanced beyond the snapshot taken before the seek.
    /// Returns true if all slots advanced within the timeout, false otherwise.
    fn wait_for_compositor_arrivals(&self, baseline: &[u64], timeout_ms: u64) -> bool {
        let effective_timeout_ms = self.effective_wait_timeout_ms(timeout_ms);
        log::debug!(
            "wait_for_compositor_arrivals: requested={}ms effective={}ms scrub={}",
            timeout_ms,
            effective_timeout_ms,
            self.is_rapid_scrubbing()
        );
        let deadline = Instant::now() + Duration::from_millis(effective_timeout_ms);
        // Use finer sleep granularity during playback for faster response.
        let sleep_ms = if self.state == PlayerState::Playing {
            5
        } else {
            15
        };
        loop {
            let all_arrived = self.slots.iter().zip(baseline.iter()).all(|(slot, &base)| {
                // Audio-only slots have no compositor pad — always "arrived".
                slot.compositor_pad.is_none()
                    || slot.comp_arrival_seq.load(Ordering::Relaxed) > base
            });
            if all_arrived {
                log::info!(
                    "wait_for_compositor_arrivals: all {} slots arrived",
                    self.slots.len()
                );
                return true;
            }
            if Instant::now() >= deadline {
                let pending: Vec<(usize, bool)> = self
                    .slots
                    .iter()
                    .zip(baseline.iter())
                    .enumerate()
                    .filter(|(_, (slot, &base))| {
                        slot.compositor_pad.is_some()
                            && slot.comp_arrival_seq.load(Ordering::Relaxed) <= base
                    })
                    .map(|(i, (slot, _))| (i, slot.video_linked.load(Ordering::Relaxed)))
                    .collect();
                let pending_indices: Vec<usize> = pending.iter().map(|(i, _)| *i).collect();
                let all_pending_unlinked = pending.iter().all(|(_, linked)| !*linked);
                if all_pending_unlinked || effective_timeout_ms < 350 {
                    log::debug!(
                        "wait_for_compositor_arrivals: timeout {}ms (requested {}ms), pending slots={:?}{}",
                        effective_timeout_ms,
                        timeout_ms,
                        pending_indices,
                        if all_pending_unlinked {
                            " (all pending video pads unlinked yet)"
                        } else {
                            " (short timeout budget)"
                        }
                    );
                } else {
                    log::warn!(
                        "wait_for_compositor_arrivals: timeout {}ms (requested {}ms), pending slots={:?}",
                        effective_timeout_ms,
                        timeout_ms,
                        pending_indices
                    );
                }
                return false;
            }
            std::thread::sleep(Duration::from_millis(sleep_ms));
        }
    }

    /// Start phase 1 of a non-blocking Playing pulse for 3+ tracks.
    /// Locks the audio sink, sets the pipeline to Playing, and returns
    /// immediately.  The caller MUST let the GTK main loop run and then
    /// call `complete_playing_pulse()` (typically via a timeout/idle callback).
    pub fn start_playing_pulse(&self) {
        if !self.audio_sink.is_locked_state() {
            self.audio_sink.set_locked_state(true);
        }
        let _ = self.pipeline.set_state(gst::State::Playing);
        log::info!(
            "start_playing_pulse: slots={} seq={}",
            self.slots.len(),
            self.scope_frame_seq.load(Ordering::Relaxed)
        );
    }

    /// Complete phase 2 of a non-blocking Playing pulse: wait briefly for the
    /// pipeline to reach Playing (the GTK main loop has been running since
    /// `start_playing_pulse` returned), then restore Paused and unlock audio.
    pub fn complete_playing_pulse(&self) {
        // Check if the pipeline reached Playing.
        let (res, cur, pend) = self.pipeline.state(gst::ClockTime::from_mseconds(10));
        log::info!(
            "complete_playing_pulse: state_result={:?} cur={:?} pend={:?} seq={}",
            res,
            cur,
            pend,
            self.scope_frame_seq.load(Ordering::Relaxed)
        );
        let _ = self.pipeline.set_state(gst::State::Paused);
        self.wait_for_paused_preroll();
        if self.audio_sink.is_locked_state() {
            self.audio_sink.set_locked_state(false);
            let _ = self.audio_sink.sync_state_with_parent();
        }
    }

    /// Execute a Playing pulse that flushes the compositor's paused frame
    /// through to the display and scope sinks.
    ///
    /// For ≤2 tracks this works synchronously: `pipeline.state()` returns
    /// before `gtk4paintablesink` blocks.  For 3+ tracks, use
    /// `start_playing_pulse()` + `complete_playing_pulse()` instead.
    fn playing_pulse(&self) {
        // Lock the audio sink for 3+ track rebuilds so the pipeline can
        // reach Playing without waiting for PulseAudio to connect.
        let lock_audio = self.slots.len() >= 3 && !self.audio_sink.is_locked_state();
        if lock_audio {
            self.audio_sink.set_locked_state(true);
        }
        let timeout_ms = if self.slots.len() >= 3 {
            300
        } else if self.is_rapid_scrubbing() {
            30
        } else {
            150
        };
        let seq_before = self.scope_frame_seq.load(Ordering::Relaxed);
        let _ = self.pipeline.set_state(gst::State::Playing);
        let (res, cur, pend) = self
            .pipeline
            .state(gst::ClockTime::from_mseconds(timeout_ms));
        let seq_after = self.scope_frame_seq.load(Ordering::Relaxed);
        log::info!(
            "playing_pulse: slots={} state_result={:?} cur={:?} pend={:?} seq={}->{}",
            self.slots.len(),
            res,
            cur,
            pend,
            seq_before,
            seq_after
        );
        let _ = self.pipeline.set_state(gst::State::Paused);
        self.wait_for_paused_preroll();
        let seq_final = self.scope_frame_seq.load(Ordering::Relaxed);
        log::info!("playing_pulse: after_paused seq={}", seq_final);
        if lock_audio {
            self.audio_sink.set_locked_state(false);
            let _ = self.audio_sink.sync_state_with_parent();
        }
    }

    fn seek_slot_decoder(
        slot: &VideoSlot,
        clip: &ProgramClip,
        timeline_pos_ns: u64,
        seek_flags: gst::SeekFlags,
        frame_duration_ns: u64,
    ) -> bool {
        if let Some(segment_start_ns) = slot.prerender_segment_start_ns {
            let source_ns = timeline_pos_ns.saturating_sub(segment_start_ns);
            return slot
                .decoder
                .seek(
                    1.0,
                    seek_flags | gst::SeekFlags::ACCURATE,
                    gst::SeekType::Set,
                    gst::ClockTime::from_nseconds(source_ns),
                    gst::SeekType::None,
                    gst::ClockTime::NONE,
                )
                .is_ok();
        }
        // For transition-entering clips, shift timeline_pos forward so
        // source_pos_ns computes the correct source-file position within
        // the virtual overlap window.
        let effective_pos = timeline_pos_ns + slot.transition_enter_offset_ns;
        let source_ns = Self::effective_slot_source_pos_ns(slot, clip, effective_pos);
        let effective_seek_flags = Self::effective_decode_seek_flags(slot, clip, seek_flags);
        let start_ns = Self::effective_video_seek_start_ns(slot, clip, source_ns);
        let stop_ns = Self::effective_video_seek_stop_ns(slot, clip, source_ns, frame_duration_ns);
        slot.decoder
            .seek(
                clip.seek_rate(),
                effective_seek_flags,
                gst::SeekType::Set,
                gst::ClockTime::from_nseconds(start_ns),
                gst::SeekType::Set,
                gst::ClockTime::from_nseconds(stop_ns),
            )
            .is_ok()
    }

    fn effective_slot_source_pos_ns(
        slot: &VideoSlot,
        clip: &ProgramClip,
        timeline_pos_ns: u64,
    ) -> u64 {
        Self::effective_source_pos_ns_for_clip(clip, timeline_pos_ns, slot.animated_svg_rendered)
    }

    fn effective_source_pos_ns_for_clip(
        clip: &ProgramClip,
        timeline_pos_ns: u64,
        animated_svg_rendered: bool,
    ) -> u64 {
        let source_ns = clip.source_pos_ns(timeline_pos_ns);
        if animated_svg_rendered {
            source_ns.saturating_sub(clip.source_in_ns)
        } else if clip.is_image {
            clip.source_in_ns
        } else {
            source_ns
        }
    }

    fn effective_decode_seek_flags(
        slot: &VideoSlot,
        clip: &ProgramClip,
        seek_flags: gst::SeekFlags,
    ) -> gst::SeekFlags {
        if clip.reverse || clip.is_freeze_frame() || (clip.is_image && !slot.animated_svg_rendered)
        {
            (seek_flags | gst::SeekFlags::ACCURATE) & !gst::SeekFlags::KEY_UNIT
        } else {
            seek_flags
        }
    }

    fn effective_video_seek_start_ns(
        slot: &VideoSlot,
        clip: &ProgramClip,
        source_pos_ns: u64,
    ) -> u64 {
        if slot.animated_svg_rendered {
            if clip.reverse {
                0
            } else {
                source_pos_ns
            }
        } else {
            clip.seek_start_ns(source_pos_ns)
        }
    }

    fn effective_video_seek_stop_ns(
        slot: &VideoSlot,
        clip: &ProgramClip,
        source_pos_ns: u64,
        frame_duration_ns: u64,
    ) -> u64 {
        if slot.animated_svg_rendered {
            if clip.reverse {
                source_pos_ns
            } else {
                clip.source_duration_ns().max(source_pos_ns)
            }
        } else if clip.is_freeze_frame() || clip.is_image {
            source_pos_ns.saturating_add(frame_duration_ns.max(1))
        } else {
            clip.seek_stop_ns(source_pos_ns)
        }
    }

    fn seek_slot_decoder_paused(
        slot: &VideoSlot,
        clip: &ProgramClip,
        timeline_pos_ns: u64,
    ) -> bool {
        if let Some(segment_start_ns) = slot.prerender_segment_start_ns {
            let source_ns = timeline_pos_ns.saturating_sub(segment_start_ns);
            return slot
                .decoder
                .seek_simple(
                    Self::paused_seek_flags(),
                    gst::ClockTime::from_nseconds(source_ns),
                )
                .is_ok();
        }
        let effective_pos = timeline_pos_ns + slot.transition_enter_offset_ns;
        let source_ns = Self::effective_slot_source_pos_ns(slot, clip, effective_pos);
        log::info!(
            "seek_slot_decoder_paused: clip={} timeline_ns={} source_ns={} transition_offset={}",
            clip.id,
            timeline_pos_ns,
            source_ns,
            slot.transition_enter_offset_ns
        );
        slot.decoder
            .seek_simple(
                Self::paused_seek_flags(),
                gst::ClockTime::from_nseconds(source_ns),
            )
            .is_ok()
    }

    fn seek_slot_decoder_with_retry(
        slot: &VideoSlot,
        clip: &ProgramClip,
        timeline_pos_ns: u64,
        seek_flags: gst::SeekFlags,
        frame_duration_ns: u64,
    ) -> bool {
        for _ in 0..4 {
            if Self::seek_slot_decoder(slot, clip, timeline_pos_ns, seek_flags, frame_duration_ns) {
                return true;
            }
            let _ = slot.decoder.state(gst::ClockTime::from_mseconds(200));
            std::thread::sleep(Duration::from_millis(20));
        }
        log::warn!(
            "ProgramPlayer: decoder seek failed after retries (clip={}, timeline_ns={})",
            clip.id,
            timeline_pos_ns
        );
        false
    }

    fn seek_slot_decoder_paused_with_retry(
        slot: &VideoSlot,
        clip: &ProgramClip,
        timeline_pos_ns: u64,
    ) -> bool {
        for _ in 0..4 {
            if Self::seek_slot_decoder_paused(slot, clip, timeline_pos_ns) {
                return true;
            }
            let _ = slot.decoder.state(gst::ClockTime::from_mseconds(200));
            std::thread::sleep(Duration::from_millis(20));
        }
        log::warn!(
            "ProgramPlayer: paused decoder seek failed after retries (clip={}, timeline_ns={})",
            clip.id,
            timeline_pos_ns
        );
        false
    }

    /// Force a re-seek on the current clip's slot so a paused frame refreshes.
    fn reseek_slot_for_current(&self) {
        let Some(idx) = self.current_idx else { return };
        self.reseek_slot_by_clip_idx(idx);
    }

    /// Public entry point to force a paused frame refresh on the current slot.
    pub fn reseek_paused(&self) {
        if self.state != PlayerState::Playing {
            self.reseek_slot_for_current();
        }
    }

    /// Force a re-seek on a specific clip's slot so a paused frame refreshes.
    /// Flushes the compositor and reseeks ALL decoder slots so the compositor
    /// can re-aggregate a complete composited frame from every video track.
    fn reseek_slot_by_clip_idx(&self, _clip_idx: usize) {
        if self.slots.is_empty() {
            return;
        }
        let baseline = self.snapshot_arrival_seqs();
        let _ = self
            .compositor
            .seek_simple(gst::SeekFlags::FLUSH, gst::ClockTime::ZERO);
        for slot in &self.slots {
            let clip = &self.clips[slot.clip_idx];
            let _ = Self::seek_slot_decoder_paused_with_retry(slot, clip, self.timeline_pos_ns);
        }
        self.wait_for_paused_preroll();
        self.wait_for_compositor_arrivals(&baseline, 3000);
    }

    /// Re-seek ALL decoder slots so the compositor produces a fresh composited
    /// frame from every video track.  Used by `export_displayed_frame_ppm` where
    /// the full multi-track composite is needed, not just the top-priority clip.
    ///
    /// For 3+ tracks the audio sink may still be connecting to PulseAudio,
    /// preventing the pipeline from reaching Paused (and therefore blocking
    /// a Playing transition).  We temporarily lock the audio sink's state so
    /// the pipeline can proceed without waiting for it.
    ///
    /// Keeps the pipeline paused to avoid visible playback movement during still capture.
    fn reseek_all_slots_for_export(&self) -> bool {
        if self.slots.is_empty() {
            return false;
        }
        let baseline = self.snapshot_arrival_seqs();
        let _ = self
            .compositor
            .seek_simple(gst::SeekFlags::FLUSH, gst::ClockTime::ZERO);
        for slot in &self.slots {
            let clip = &self.clips[slot.clip_idx];
            let _ = Self::seek_slot_decoder_paused_with_retry(slot, clip, self.timeline_pos_ns);
        }
        self.wait_for_paused_preroll();
        self.wait_for_compositor_arrivals(&baseline, 3000);
        false
    }

    /// Restore pipeline to Paused after an async export that left it Playing.
    #[allow(dead_code)]
    pub fn set_paused_after_export(&self) {
        let _ = self.pipeline.set_state(gst::State::Paused);
        self.wait_for_paused_preroll();
        // Unlock audio sink and let it sync with the pipeline.
        if self.audio_sink.is_locked_state() {
            self.audio_sink.set_locked_state(false);
            let _ = self.audio_sink.sync_state_with_parent();
        }
    }

    /// Returns true if the clips active at `timeline_pos_ns` exactly match the
    /// decoder slots currently loaded (same indices, same order).
    fn clips_match_current_slots(&self, timeline_pos_ns: u64) -> bool {
        let desired = self.clips_active_at(timeline_pos_ns);
        let current: Vec<usize> = self.slots.iter().map(|s| s.clip_idx).collect();
        desired == current
    }

    /// Seek all currently-loaded decoder slots to `timeline_pos_ns` without
    /// rebuilding the pipeline.  Returns `true` if a non-blocking Playing pulse
    /// was started (3+ tracks); the caller must schedule `complete_playing_pulse()`.
    fn seek_slots_in_place(&mut self, timeline_pos_ns: u64, arrival_wait_ms: u64) -> bool {
        // Update transition_enter_offset_ns for each slot before seeking.
        for i in 0..self.slots.len() {
            let clip_idx = self.slots[i].clip_idx;
            self.slots[i].transition_enter_offset_ns =
                self.transition_enter_offset_for_clip(clip_idx);
        }
        // Snapshot arrival counters BEFORE seeking so we can detect fresh buffers.
        let baseline = self.snapshot_arrival_seqs();
        // Atomically flush the compositor and ALL its sink pads (including
        // downstream) by seeking the compositor itself.  This clears stale
        // preroll frames that the compositor produced before decoder buffers
        // arrived.  The per-decoder seeks below will then set each decoder
        // to the correct source position.
        let _ = self
            .compositor
            .seek_simple(gst::SeekFlags::FLUSH, gst::ClockTime::ZERO);
        // Seek every decoder to the new source position.  These FLUSH seeks
        // reset each compositor pad individually and position the decoders.
        //
        // Skip still-image clips: their source decoder has already EOSed into
        // `imagefreeze`, so a per-decoder reseek here is racy — `imagefreeze`
        // sometimes re-emits its cached buffer cleanly after the compositor
        // flush, sometimes it doesn't, leaving the still's compositor pad
        // empty until the next scrub.  Letting the imagefreeze element
        // re-push its parked buffer in response to the compositor flush
        // (without disturbing the upstream EOSed decoder) is the only path
        // that doesn't intermittently drop the still.  Animated SVGs that
        // were pre-rendered to a video file behave like normal video and
        // are reseeked.
        for slot in &self.slots {
            let clip = &self.clips[slot.clip_idx];
            if clip.is_image && !clip.animated_svg && !slot.animated_svg_rendered {
                continue;
            }
            let _ = Self::seek_slot_decoder_paused_with_retry(slot, clip, timeline_pos_ns);
        }
        // Wait for decoders to reach Paused (internal preroll).
        self.wait_for_paused_preroll();
        // Wait for post-seek buffers to actually arrive at the compositor.
        // Without this, the compositor produces stale (background-only) frames
        // because the decoder buffers are still in the effects/queue chain.
        self.wait_for_compositor_arrivals(&baseline, arrival_wait_ms.max(1));
        // Reset the pipeline's start_time so running_time begins at 0 in the
        // next Playing transition.
        self.pipeline.set_start_time(gst::ClockTime::ZERO);
        if self.slots.len() >= 3 {
            // For 3+ tracks, start a non-blocking Playing pulse so the GTK
            // main loop can service gtk4paintablesink's preroll.
            self.start_playing_pulse();
            true
        } else {
            // ≤2 tracks: synchronous pulse works fine.
            self.playing_pulse();
            false
        }
    }

    fn apply_transform_to_slot(
        slot: &VideoSlot,
        crop_left: i32,
        crop_right: i32,
        crop_top: i32,
        crop_bottom: i32,
        rotate: i32,
        flip_h: bool,
        flip_v: bool,
    ) {
        // Clamp crop values so videocrop never produces zero/negative output
        // dimensions.  The processing resolution can be smaller than the
        // slider maximum (e.g. 540px wide on a 9×16 vertical timeline at
        // half-res), so crop_left + crop_right could otherwise exceed the
        // frame width, causing caps negotiation failures in GStreamer.
        //
        // Compute letterbox offset from source dimensions vs project frame.
        // The effects_bin ghost sink pad peer carries the raw decoded caps.
        // Try multiple approaches to get source dimensions for letterbox computation:
        // 1. Ghost sink pad's peer (decoder src pad)
        // 2. Ghost sink pad's own current caps (may have source caps after preroll)
        // 3. The first element inside the effects_bin's sink pad caps
        let source_caps = slot.effects_bin.static_pad("sink").and_then(|ghost| {
            ghost
                .peer()
                .and_then(|p| p.current_caps())
                .or_else(|| ghost.current_caps())
        });
        let (lb_h, lb_v) = source_caps
            .and_then(|c| {
                c.structure(0).map(|s| {
                    let src_w = s.get::<i32>("width").unwrap_or(0);
                    let src_h = s.get::<i32>("height").unwrap_or(0);
                    if src_w <= 0 || src_h <= 0 {
                        return (0, 0);
                    }
                    // Get project resolution from effects_bin src pad
                    let (fw, fh) = slot
                        .effects_bin
                        .static_pad("src")
                        .and_then(|p| p.current_caps())
                        .and_then(|c| {
                            c.structure(0).map(|s| {
                                (
                                    s.get::<i32>("width").unwrap_or(0),
                                    s.get::<i32>("height").unwrap_or(0),
                                )
                            })
                        })
                        .unwrap_or((0, 0));
                    if fw <= 0 || fh <= 0 {
                        return (0, 0);
                    }
                    let scale = (fw as f64 / src_w as f64).min(fh as f64 / src_h as f64);
                    let scaled_w = (src_w as f64 * scale).round() as i32;
                    let scaled_h = (src_h as f64 * scale).round() as i32;
                    ((fw - scaled_w) / 2, (fh - scaled_h) / 2)
                })
            })
            .unwrap_or((0, 0));
        if crop_left > 0 || crop_right > 0 || crop_top > 0 || crop_bottom > 0 {
            log::info!(
                "ProgramPlayer: crop letterbox offset: lb_h={lb_h} lb_v={lb_v} crop=({crop_left},{crop_right},{crop_top},{crop_bottom})"
            );
        }

        // Update shared crop state — the alpha pad probe reads this on each frame.
        // Only update crop values and letterbox; proj_w/proj_h are set at
        // construction from actual project dimensions (not processing resolution).
        {
            let mut st = slot.crop_alpha_state.lock().unwrap();
            st.0 = crop_left.max(0);
            st.1 = crop_right.max(0);
            st.2 = crop_top.max(0);
            st.3 = crop_bottom.max(0);
            st.4 = lb_h;
            st.5 = lb_v;
        }
        if let Some(ref vfr) = slot.videoflip_rotate {
            if vfr.find_property("angle").is_some() {
                // UI positive rotation is counterclockwise, matching GstRotate.
                vfr.set_property("angle", (rotate as f64).to_radians());
            } else {
                let method = match rotate.rem_euclid(360) {
                    90 => "counterclockwise",
                    180 => "rotate-180",
                    270 => "clockwise",
                    _ => "none",
                };
                vfr.set_property_from_str("method", method);
            }
        }
        if let Some(ref vff) = slot.videoflip_flip {
            let method = match (flip_h, flip_v) {
                (true, true) => "rotate-180",
                (true, false) => "horizontal-flip",
                (false, true) => "vertical-flip",
                (false, false) => "none",
            };
            vff.set_property_from_str("method", method);
        }
    }

    fn apply_title_to_slot(
        slot: &VideoSlot,
        text: &str,
        font: &str,
        color_rgba: u32,
        rel_x: f64,
        rel_y: f64,
        project_h: u32,
        proc_w: u32,
        proc_h: u32,
    ) {
        const TITLE_REFERENCE_HEIGHT: f64 = 1080.0;
        // GStreamer textoverlay renders Pango text proportionally to the video
        // frame.  Measured: 1 Pango pt ≈ 0.0037 of frame height (constant
        // across 540p/1080p/2160p).  Since the fraction is resolution-independent,
        // we use project_h (not proc_h) to match the export formula:
        //   export:  fontsize_px = base_size × (4/3) × (out_h / 1080)
        //   preview: pango_pt × 0.0037 = base_size × (4/3) / 1080
        //            pango_pt = base_size × (4/3) / (0.0037 × 1080) ≈ base_size / 3
        //
        // For non-1080p projects: pango_pt = base_size × (4/3) / (0.0037 × project_h)
        //
        // The (project_h / 1080) factor in the formula below cancels out with
        // the (4/3)/3.0 constant because textoverlay proportional scaling handles
        // the resolution automatically.
        const PANGO_EXPORT_MATCH: f64 = 1.0 / 3.0;
        if let Some(ref to) = slot.textoverlay {
            let silent = text.is_empty();
            to.set_property("silent", silent);
            if !silent {
                to.set_property("text", text);
                let base_size = crate::media::title_font::parse_title_font(font).size_points();
                let scaled_size =
                    (base_size * PANGO_EXPORT_MATCH * (project_h as f64 / TITLE_REFERENCE_HEIGHT))
                        .max(4.0);
                let adjusted_font =
                    crate::media::title_font::build_preview_title_font_desc(font, scaled_size);
                to.set_property("font-desc", &adjusted_font);
                // Use center alignment + pixel deltas to match FFmpeg drawtext
                // centering semantics: drawtext places text center at
                // (rel_x × w, rel_y × h).  Using halignment/valignment=center
                // with deltax/deltay avoids text-width-dependent clipping
                // differences between preview and export resolutions.
                //
                // valignment=center places the text TOP edge (not center) at
                // the vertical center.  Compensate by adding half the text
                // height in pixels to deltay.
                to.set_property_from_str("halignment", "center");
                to.set_property_from_str("valignment", "center");
                let text_h_px = (scaled_size * 0.0037 * proc_h as f64).round();
                let dx = ((rel_x - 0.5) * proc_w as f64).round() as i32;
                let dy = ((rel_y - 0.5) * proc_h as f64 + text_h_px * 0.35).round() as i32;
                to.set_property("deltax", dx);
                to.set_property("deltay", dy);
                let (r, g, b, a) = crate::ui::colors::rgba_u32_to_u8(color_rgba);
                let argb: u32 =
                    ((a as u32) << 24) | ((r as u32) << 16) | ((g as u32) << 8) | b as u32;
                to.set_property("color", argb);
            }
        }
    }

    /// Apply extended title styling (outline, shadow, bg box) to the textoverlay.
    fn apply_title_style_to_slot(slot: &VideoSlot, clip: &ProgramClip) {
        if let Some(ref to) = slot.textoverlay {
            // Outline (GStreamer textoverlay uses draw-outline, fixed ~1px width)
            to.set_property("draw-outline", clip.title_outline_width > 0.0);
            if clip.title_outline_width > 0.0 {
                let (r, g, b, a) = crate::ui::colors::rgba_u32_to_u8(clip.title_outline_color);
                let argb: u32 =
                    ((a as u32) << 24) | ((r as u32) << 16) | ((g as u32) << 8) | b as u32;
                to.set_property("outline-color", argb);
            }
            // Shadow: GStreamer textoverlay only supports a fixed ~1px shadow
            // with no configurable offset/color.  Disable it to avoid visual
            // mismatch with the export's fully customizable drawtext shadow.
            // The shadow is rendered correctly on export.
            to.set_property("draw-shadow", false);
            // Background box (shaded background)
            to.set_property("shaded-background", clip.title_bg_box);
        }
    }

    /// Apply per-slot zoom / scale / position via compositor pad properties.
    fn apply_zoom_to_slot(
        slot: &VideoSlot,
        pad: &gst::Pad,
        scale: f64,
        position_x: f64,
        position_y: f64,
        direct_translation: bool,
        project_width: u32,
        project_height: u32,
    ) {
        Self::apply_zoom_to_slot_with_canvas_offset(
            slot,
            pad,
            scale,
            position_x,
            position_y,
            direct_translation,
            project_width,
            project_height,
            0,
            0,
        );
    }

    fn apply_zoom_to_slot_with_canvas_offset(
        slot: &VideoSlot,
        pad: &gst::Pad,
        scale: f64,
        position_x: f64,
        position_y: f64,
        direct_translation: bool,
        project_width: u32,
        project_height: u32,
        extra_x_px: i32,
        extra_y_px: i32,
    ) {
        let scale = scale.clamp(
            crate::model::transform_bounds::SCALE_MIN,
            crate::model::transform_bounds::SCALE_MAX,
        );
        let pw = project_width as f64;
        let ph = project_height as f64;
        let sw = (pw * scale).round().max(1.0) as i32;
        let sh = (ph * scale).round().max(1.0) as i32;

        // Prefer compositor sink-pad scaling/positioning when supported. This
        // avoids dynamic caps renegotiation in the per-clip zoom branch while
        // playback is running, which can trigger decodebin/qtdemux
        // not-negotiated failures on some MP4 sources.
        // For blend-mode clips, always use the effects-bin zoom path so the
        // captured buffer has scale/position baked in (the compositor pad
        // alpha is 0, so compositor-level scaling would be invisible).
        if !slot.is_blend_mode
            && pad.find_property("width").is_some()
            && pad.find_property("height").is_some()
        {
            pad.set_property("width", sw);
            pad.set_property("height", sh);
            if pad.find_property("xpos").is_some() && pad.find_property("ypos").is_some() {
                let (xpos, ypos) = if direct_translation {
                    (
                        Self::direct_canvas_origin(pw, sw as f64, position_x) + extra_x_px,
                        Self::direct_canvas_origin(ph, sh as f64, position_y) + extra_y_px,
                    )
                } else {
                    let total_x = pw * (scale - 1.0);
                    let total_y = ph * (scale - 1.0);
                    (
                        (-(total_x * (1.0 + position_x) / 2.0)).round() as i32 + extra_x_px,
                        (-(total_y * (1.0 + position_y) / 2.0)).round() as i32 + extra_y_px,
                    )
                };
                pad.set_property("xpos", xpos);
                pad.set_property("ypos", ypos);
            }
            return;
        }

        if let Some(ref cf) = slot.capsfilter_zoom {
            let caps = gst::Caps::builder("video/x-raw")
                .field("format", "RGBA")
                .field("width", sw)
                .field("height", sh)
                .field("pixel-aspect-ratio", gst::Fraction::new(1, 1))
                .build();
            cf.set_property("caps", &caps);
        }

        let (box_left, box_right, box_top, box_bottom) = if direct_translation {
            let xpos = Self::direct_canvas_origin(pw, sw as f64, position_x) + extra_x_px;
            let ypos = Self::direct_canvas_origin(ph, sh as f64, position_y) + extra_y_px;
            let (left, right) = Self::videobox_axis_from_origin(project_width as i32, sw, xpos);
            let (top, bottom) = Self::videobox_axis_from_origin(project_height as i32, sh, ypos);
            (left, right, top, bottom)
        } else {
            let pos_x = position_x;
            let pos_y = position_y;
            let total_x = pw * (scale - 1.0);
            let total_y = ph * (scale - 1.0);
            (
                (total_x * (1.0 + pos_x) / 2.0) as i32,
                (total_x * (1.0 - pos_x) / 2.0) as i32,
                (total_y * (1.0 + pos_y) / 2.0) as i32,
                (total_y * (1.0 - pos_y) / 2.0) as i32,
            )
        };

        if let Some(ref vb) = slot.videobox_zoom {
            vb.set_property("left", box_left);
            vb.set_property("right", box_right);
            vb.set_property("top", box_top);
            vb.set_property("bottom", box_bottom);
            vb.set_property("border-alpha", 0.0_f64);
        }
        // Fallback: use compositor pad xpos/ypos for simple positioning.
        let _ = pad;
    }

    fn direct_canvas_origin(axis_size: f64, scaled_axis_size: f64, position: f64) -> i32 {
        // Allow positions past ±1.0 so titles, adjustment scopes, and
        // tracker-followed clips can be moved fully off-canvas; the
        // downstream `videobox` element handles negative offsets by
        // padding/cropping past the frame edges.
        let clamped = position.clamp(
            crate::model::transform_bounds::POSITION_MIN,
            crate::model::transform_bounds::POSITION_MAX,
        );
        (((axis_size - scaled_axis_size) / 2.0) + clamped * axis_size / 2.0).round() as i32
    }

    fn videobox_axis_from_origin(canvas_size: i32, scaled_size: i32, origin: i32) -> (i32, i32) {
        let crop_start = (-origin).max(0);
        let crop_end = (origin + scaled_size - canvas_size).max(0);
        let pad_start = origin.max(0);
        let pad_end = (canvas_size - (origin + scaled_size)).max(0);
        (crop_start - pad_start, crop_end - pad_end)
    }

    fn clip_uses_direct_canvas_translation(clip: &ProgramClip) -> bool {
        clip.is_adjustment || clip.is_title || clip.tracking_binding.is_some()
    }

    fn clip_requires_live_transform_refresh(clip: &ProgramClip) -> bool {
        clip.is_image && !clip.animated_svg
    }

    #[allow(dead_code)]
    /// Reset every retained slot's compositor-arrival counter so that
    /// `wait_for_compositor_arrivals` requires a genuinely fresh buffer
    /// from each slot after the topology change + flush/seek cycle.
    #[allow(dead_code)]
    fn reset_slot_arrival_seqs(&self) {
        for slot in &self.slots {
            slot.comp_arrival_seq.store(0, Ordering::Relaxed);
        }
    }

    /// Seek all current slots using the correct flags for the context:
    /// FLUSH|ACCURATE during playback (avoids long-GOP keyframe snap),
    /// paused seek flags otherwise.  Sends EOS on failed slots.
    /// Currently unused but retained for possible future full-flush scenarios.
    #[allow(dead_code)]
    fn seek_all_slots(&self, timeline_pos: u64, was_playing: bool) {
        let seek_flags = if was_playing {
            gst::SeekFlags::FLUSH | gst::SeekFlags::ACCURATE
        } else {
            Self::paused_seek_flags()
        };
        for slot in &self.slots {
            let clip = &self.clips[slot.clip_idx];
            let ok = if was_playing {
                Self::seek_slot_decoder_with_retry(
                    slot,
                    clip,
                    timeline_pos,
                    seek_flags,
                    self.frame_duration_ns,
                )
            } else {
                Self::seek_slot_decoder_paused_with_retry(slot, clip, timeline_pos)
            };
            if !ok {
                log::warn!(
                    "incremental: seek FAILED for clip {} — sending EOS",
                    clip.id
                );
                if let Some(ref pad) = slot.compositor_pad {
                    let _ = pad.send_event(gst::event::Eos::new());
                }
                if let Some(ref pad) = slot.audio_mixer_pad {
                    let _ = pad.send_event(gst::event::Eos::new());
                }
            }
        }
    }

    /// Incremental add-only boundary update — DISABLED.
    ///
    /// This method is kept for reference but not called.  Individual per-decoder
    /// FLUSH seeks don't trigger GstVideoAggregator's coordinated seek handler,
    /// leaving the compositor's segment/position tracking stale.  And using
    /// compositor.seek_simple propagates upstream to retained decoders causing
    /// double-flush corruption.  A future approach using gst_pad_set_offset()
    /// for running-time alignment could make this viable.
    #[allow(dead_code)]
    fn try_incremental_add_only_update(
        &mut self,
        timeline_pos: u64,
        desired: &[usize],
        current: &[usize],
        was_playing: bool,
    ) -> bool {
        if desired.is_empty() || current.is_empty() || desired.len() <= current.len() {
            return false;
        }
        if !current.iter().all(|idx| desired.contains(idx)) {
            return false;
        }
        let added: Vec<usize> = desired
            .iter()
            .copied()
            .filter(|idx| !current.contains(idx))
            .collect();
        if added.is_empty() || added.len() > 2 {
            return false;
        }
        let rebuild_started = Instant::now();
        self.teardown_prepreroll_sidecars();
        log::debug!(
            "try_incremental_add_only: timeline_pos={} added={} retained={} was_playing={}",
            timeline_pos,
            added.len(),
            current.len(),
            was_playing
        );

        // 1. Pause — all decoders settle into paused preroll.
        let _ = self.pipeline.set_state(gst::State::Paused);

        // 2. Build only NEW slots.
        let mut added_ok = true;
        let mut added_clip_idxs: Vec<usize> = Vec::new();
        for (zorder_offset, clip_idx) in desired.iter().enumerate() {
            if current.contains(clip_idx) {
                continue;
            }
            // Adjustment layers have no compositor slot — skip without error.
            if *clip_idx < self.clips.len() && self.clips[*clip_idx].is_adjustment {
                continue;
            }
            if let Some(mut slot) = self.build_slot_for_clip(*clip_idx, zorder_offset, true) {
                slot.transition_enter_offset_ns = self.transition_enter_offset_for_clip(*clip_idx);
                self.slots.push(slot);
                added_clip_idxs.push(*clip_idx);
            } else {
                added_ok = false;
                break;
            }
        }
        if !added_ok {
            let mut i = 0usize;
            while i < self.slots.len() {
                if added_clip_idxs.contains(&self.slots[i].clip_idx) {
                    let slot = self.slots.remove(i);
                    self.teardown_single_slot(slot);
                } else {
                    i += 1;
                }
            }
            return false;
        }

        // 3. Wait for NEW decoders to link (discover streams + pad-added).
        let link_wait_ms = if was_playing {
            self.adaptive_arrival_wait_ms(400)
        } else {
            self.effective_wait_timeout_ms(400)
        };
        let _ = self
            .pipeline
            .state(gst::ClockTime::from_mseconds(link_wait_ms));
        for slot in &self.slots {
            if !slot.video_linked.load(Ordering::Relaxed) {
                if added_clip_idxs.contains(&slot.clip_idx) {
                    log::warn!(
                        "try_incremental_add_only: new slot clip_idx={} not linked, sending EOS",
                        slot.clip_idx
                    );
                }
                if let Some(ref pad) = slot.compositor_pad {
                    let _ = pad.send_event(gst::event::Eos::new());
                }
                if let Some(ref pad) = slot.audio_mixer_pad {
                    let _ = pad.send_event(gst::event::Eos::new());
                }
            }
        }

        // 4. Update zorder on ALL slots to match desired order.
        for (zorder_offset, clip_idx) in desired.iter().enumerate() {
            if let Some(slot) = self.slots.iter().find(|s| s.clip_idx == *clip_idx) {
                if let Some(ref pad) = slot.compositor_pad {
                    pad.set_property("zorder", (zorder_offset + 1) as u32);
                }
            }
        }

        // 5. Reset start_time so running-times align across all decoders.
        self.pipeline.set_start_time(gst::ClockTime::ZERO);

        // 6. Seek ALL decoders individually — both new and retained.
        //    Each per-decoder FLUSH propagates through its own branch only
        //    (unlike compositor.seek_simple which propagates to ALL branches).
        //    This gives every decoder a clean segment from its seek position
        //    with running_time starting from 0, aligned with the reset start_time.
        self.reset_slot_arrival_seqs();
        let baseline = self.snapshot_arrival_seqs();
        let seek_flags = if was_playing {
            gst::SeekFlags::FLUSH | gst::SeekFlags::ACCURATE
        } else {
            Self::paused_seek_flags()
        };
        for slot in &self.slots {
            let clip = &self.clips[slot.clip_idx];
            let ok = if was_playing {
                Self::seek_slot_decoder_with_retry(
                    slot,
                    clip,
                    timeline_pos,
                    seek_flags,
                    self.frame_duration_ns,
                )
            } else {
                Self::seek_slot_decoder_paused_with_retry(slot, clip, timeline_pos)
            };
            if !ok {
                log::warn!("try_incremental_add_only: seek FAILED for clip {}", clip.id);
                if let Some(ref pad) = slot.compositor_pad {
                    let _ = pad.send_event(gst::event::Eos::new());
                }
                if let Some(ref pad) = slot.audio_mixer_pad {
                    let _ = pad.send_event(gst::event::Eos::new());
                }
            }
        }

        // 7. Wait for all decoders' preroll + compositor arrivals.
        self.wait_for_paused_preroll();
        let arrival_budget = if was_playing {
            self.adaptive_arrival_wait_ms(1500)
        } else {
            1200
        };
        self.wait_for_compositor_arrivals(&baseline, arrival_budget);

        self.current_idx = desired.last().copied();
        self.slot_queue_drop_late_active = false;
        let elapsed_ms = rebuild_started.elapsed().as_millis() as u64;
        if was_playing {
            self.record_rebuild_duration_ms(elapsed_ms);
        }
        log::info!(
            "try_incremental_add_only: timeline_pos={} slots_after={} elapsed_ms={}",
            timeline_pos,
            self.slots.len(),
            elapsed_ms
        );
        true
    }

    /// Incremental remove-only boundary update — DISABLED.
    ///
    /// This method is kept for reference but not called.  After removing
    /// compositor sink pads, the aggregator's internal timing/segment state
    /// remains stale without a compositor.seek_simple reset, causing retained
    /// decoders to produce ≤1 frame/sec.  All transitions now use full rebuild.
    #[allow(dead_code)]
    fn try_incremental_remove_only_update(
        &mut self,
        timeline_pos: u64,
        desired: &[usize],
        current: &[usize],
        was_playing: bool,
    ) -> bool {
        if desired.is_empty() || current.is_empty() || desired.len() >= current.len() {
            return false;
        }
        if !desired.iter().all(|idx| current.contains(idx)) {
            return false;
        }
        let removed_count = current.len().saturating_sub(desired.len());
        if removed_count == 0 {
            return false;
        }
        let rebuild_started = Instant::now();
        self.teardown_prepreroll_sidecars();
        log::debug!(
            "try_incremental_remove_only: timeline_pos={} removed={} retained={} was_playing={}",
            timeline_pos,
            removed_count,
            desired.len(),
            was_playing
        );

        // 1. Pause pipeline — streaming threads must be quiescent before
        //    removing elements and releasing pads.
        let _ = self.pipeline.set_state(gst::State::Paused);

        // 2. Teardown removed slots.
        let mut i = 0usize;
        while i < self.slots.len() {
            if desired.contains(&self.slots[i].clip_idx) {
                i += 1;
                continue;
            }
            let slot = self.slots.remove(i);
            self.teardown_single_slot(slot);
        }
        if self.slots.is_empty() {
            return false;
        }

        // 3. Update zorder on retained slots to match desired order.
        for (zorder_offset, clip_idx) in desired.iter().enumerate() {
            if let Some(slot) = self.slots.iter().find(|s| s.clip_idx == *clip_idx) {
                if let Some(ref pad) = slot.compositor_pad {
                    pad.set_property("zorder", (zorder_offset + 1) as u32);
                }
            }
        }

        // 4. No flush, no seek, no start_time reset.  Retained decoders
        //    resume from their paused preroll position when poll() sets Playing.
        self.current_idx = desired.last().copied();
        self.slot_queue_drop_late_active = false;
        let elapsed_ms = rebuild_started.elapsed().as_millis() as u64;
        // Do NOT record this duration in the adaptive wait ring buffer.
        // Remove-only times (~100ms) are fundamentally different from full
        // rebuild times (~1300ms) and would contaminate the p75 calculation,
        // causing subsequent full rebuilds to use dangerously tight budgets.
        log::info!(
            "try_incremental_remove_only: timeline_pos={} slots_after={} elapsed_ms={}",
            timeline_pos,
            self.slots.len(),
            elapsed_ms
        );
        true
    }

    /// Real-time boundary update using pad offsets for running-time alignment.
    ///
    /// Instead of tearing down the entire pipeline, this method:
    /// 1. Hides departing clips (alpha=0, volume=0) — no pad removal.
    /// 2. Builds new slots for entering clips with `pad.set_offset()` so
    ///    post-FLUSH-seek buffers align with the compositor's running-time.
    /// 3. Tears down any previously-hidden (stale) slots to bound resource use.
    ///
    /// Returns true on success, false to fall back to full rebuild.
    fn try_realtime_boundary_update(
        &mut self,
        timeline_pos: u64,
        desired: &[usize],
        current_visible: &[usize],
    ) -> bool {
        // Fall back to full rebuild when desired is empty (gap in timeline).
        if desired.is_empty() {
            return false;
        }

        let retained: Vec<usize> = desired
            .iter()
            .copied()
            .filter(|idx| current_visible.contains(idx))
            .collect();
        let added: Vec<usize> = desired
            .iter()
            .copied()
            .filter(|idx| !current_visible.contains(idx))
            .collect();
        let removed: Vec<usize> = current_visible
            .iter()
            .copied()
            .filter(|idx| !desired.contains(idx))
            .collect();

        // Limit complexity: fall back when too many clips are changing.
        if added.len() > 3 {
            return false;
        }

        let rebuild_started = Instant::now();
        self.teardown_prepreroll_sidecars();
        log::info!(
            "try_realtime_boundary: pos={} added={:?} removed={:?} retained={:?}",
            timeline_pos,
            added,
            removed,
            retained
        );

        // 1. Garbage-collect previously-hidden slots to bound resource use.
        let mut i = 0usize;
        while i < self.slots.len() {
            if self.slots[i].hidden {
                let slot = self.slots.remove(i);
                self.teardown_single_slot(slot);
            } else {
                i += 1;
            }
        }

        // 2. Hide departing clips: zero alpha/volume, mark hidden.
        for slot in self.slots.iter_mut() {
            if removed.contains(&slot.clip_idx) {
                if let Some(ref pad) = slot.compositor_pad {
                    pad.set_property("alpha", 0.0_f64);
                }
                if let Some(ref pad) = slot.audio_mixer_pad {
                    pad.set_property("volume", 0.0_f64);
                }
                slot.hidden = true;
            }
        }

        // 3. Build new slots for entering clips.
        if !added.is_empty() {
            // Get the pipeline's current running-time for pad offset alignment.
            let pipe_running_time_ns: i64 = self
                .pipeline
                .current_running_time()
                .map(|t| t.nseconds() as i64)
                .unwrap_or(0);

            let mut added_ok = true;
            let mut added_clip_idxs: Vec<usize> = Vec::new();
            for clip_idx in &added {
                // Adjustment layers have no compositor slot — skip without error.
                if *clip_idx < self.clips.len() && self.clips[*clip_idx].is_adjustment {
                    continue;
                }
                // Use zorder 0 temporarily; corrected in step 4.
                if let Some(mut slot) = self.build_slot_for_clip(*clip_idx, 0, true) {
                    // Set pad offsets so post-seek buffers (running-time 0)
                    // align with the compositor's current running-time.
                    if let Some(ref pad) = slot.compositor_pad {
                        pad.set_offset(pipe_running_time_ns);
                    }
                    if let Some(ref pad) = slot.audio_mixer_pad {
                        pad.set_offset(pipe_running_time_ns);
                    }
                    slot.hidden = false;
                    slot.transition_enter_offset_ns =
                        self.transition_enter_offset_for_clip(*clip_idx);
                    self.slots.push(slot);
                    added_clip_idxs.push(*clip_idx);
                } else {
                    added_ok = false;
                    break;
                }
            }

            if !added_ok {
                // Roll back: remove newly-added slots, un-hide departed, bail.
                let mut j = 0usize;
                while j < self.slots.len() {
                    if added_clip_idxs.contains(&self.slots[j].clip_idx) {
                        let slot = self.slots.remove(j);
                        self.teardown_single_slot(slot);
                    } else {
                        j += 1;
                    }
                }
                for slot in self.slots.iter_mut() {
                    if removed.contains(&slot.clip_idx) {
                        slot.hidden = false;
                    }
                }
                return false;
            }

            // Wait for new decoders to link (stream discovery + pad-added).
            let link_wait_ms = self.adaptive_arrival_wait_ms(400);
            let _ = self
                .pipeline
                .state(gst::ClockTime::from_mseconds(link_wait_ms));

            // Seek new decoders to their source positions.
            let seek_flags = gst::SeekFlags::FLUSH | gst::SeekFlags::ACCURATE;
            for slot in &self.slots {
                if !added_clip_idxs.contains(&slot.clip_idx) {
                    continue;
                }
                let clip = &self.clips[slot.clip_idx];
                if !Self::seek_slot_decoder_with_retry(
                    slot,
                    clip,
                    timeline_pos,
                    seek_flags,
                    self.frame_duration_ns,
                ) {
                    log::warn!("try_realtime_boundary: seek FAILED for clip {}", clip.id);
                    if let Some(ref pad) = slot.compositor_pad {
                        let _ = pad.send_event(gst::event::Eos::new());
                    }
                    if let Some(ref pad) = slot.audio_mixer_pad {
                        let _ = pad.send_event(gst::event::Eos::new());
                    }
                }
            }

            // Brief preroll wait for new decoders only.
            self.wait_for_paused_preroll();
        }

        // 4. Update zorder on all visible slots to match desired order.
        for (zorder_offset, clip_idx) in desired.iter().enumerate() {
            if let Some(slot) = self
                .slots
                .iter()
                .find(|s| s.clip_idx == *clip_idx && !s.hidden)
            {
                if let Some(ref pad) = slot.compositor_pad {
                    pad.set_property("zorder", (zorder_offset + 1) as u32);
                }
            }
        }

        // 5. Ensure pipeline stays in Playing — no state transition needed
        //    since we never paused.
        let _ = self.pipeline.set_state(gst::State::Playing);

        self.current_idx = desired.last().copied();
        self.slot_queue_drop_late_active = false;
        self.apply_keyframed_video_slot_properties(timeline_pos);
        self.apply_transition_effects(timeline_pos);
        let elapsed_ms = rebuild_started.elapsed().as_millis() as u64;
        self.record_rebuild_duration_ms(elapsed_ms);
        log::info!(
            "try_realtime_boundary: pos={} visible={} hidden={} elapsed_ms={}",
            timeline_pos,
            self.slots.iter().filter(|s| !s.hidden).count(),
            self.slots.iter().filter(|s| s.hidden).count(),
            elapsed_ms
        );
        true
    }

    // ── Pipeline rebuild ───────────────────────────────────────────────────

    fn teardown_single_slot(&mut self, slot: VideoSlot) {
        // Remove any captured blend overlay for this slot.
        if slot.is_blend_mode {
            if let Ok(mut overlays) = self.blend_overlays.lock() {
                overlays.remove(&slot.clip_idx);
            }
        }
        if let Some(ref pad) = slot.compositor_pad {
            let _ = pad.send_event(gst::event::FlushStart::new());
        }
        if let Some(ref pad) = slot.audio_mixer_pad {
            let _ = pad.send_event(gst::event::FlushStart::new());
        }
        // 1. Detach branch elements from the pipeline.
        self.pipeline.remove(&slot.decoder).ok();
        self.pipeline
            .remove(slot.effects_bin.upcast_ref::<gst::Element>())
            .ok();
        if let Some(ref q) = slot.slot_queue {
            self.pipeline.remove(q).ok();
        }
        if let Some(ref ac) = slot.audio_conv {
            self.pipeline.remove(ac).ok();
        }
        if let Some(ref ar) = slot.audio_resample {
            self.pipeline.remove(ar).ok();
        }
        if let Some(ref cf) = slot.audio_capsfilter {
            self.pipeline.remove(cf).ok();
        }
        if let Some(ref eq_elem) = slot.audio_equalizer {
            self.pipeline.remove(eq_elem).ok();
        }
        if let Some(ref meq_elem) = slot.audio_match_equalizer {
            self.pipeline.remove(meq_elem).ok();
        }
        if let Some(ref ap) = slot.audio_panorama {
            self.pipeline.remove(ap).ok();
        }
        if let Some(ref lv) = slot.audio_level {
            self.pipeline.remove(lv).ok();
        }
        // 2. Stop any residual streaming work on removed elements.
        // Use short timeouts (10ms) — after FlushStart, Null transitions are
        // near-instant.  The previous 100ms timeout per element caused up to
        // 700ms blocking on the main thread during boundary crossings.
        let _ = slot.decoder.set_state(gst::State::Null);
        let _ = slot.decoder.state(gst::ClockTime::from_mseconds(10));
        let _ = slot.effects_bin.set_state(gst::State::Null);
        let _ = slot.effects_bin.state(gst::ClockTime::from_mseconds(10));
        if let Some(ref q) = slot.slot_queue {
            let _ = q.set_state(gst::State::Null);
            let _ = q.state(gst::ClockTime::from_mseconds(10));
        }
        if let Some(ref ac) = slot.audio_conv {
            let _ = ac.set_state(gst::State::Null);
            let _ = ac.state(gst::ClockTime::from_mseconds(10));
        }
        if let Some(ref ar) = slot.audio_resample {
            let _ = ar.set_state(gst::State::Null);
            let _ = ar.state(gst::ClockTime::from_mseconds(10));
        }
        if let Some(ref cf) = slot.audio_capsfilter {
            let _ = cf.set_state(gst::State::Null);
            let _ = cf.state(gst::ClockTime::from_mseconds(10));
        }
        if let Some(ref eq_elem) = slot.audio_equalizer {
            let _ = eq_elem.set_state(gst::State::Null);
            let _ = eq_elem.state(gst::ClockTime::from_mseconds(10));
        }
        if let Some(ref meq_elem) = slot.audio_match_equalizer {
            let _ = meq_elem.set_state(gst::State::Null);
            let _ = meq_elem.state(gst::ClockTime::from_mseconds(10));
        }
        if let Some(ref ap) = slot.audio_panorama {
            let _ = ap.set_state(gst::State::Null);
            let _ = ap.state(gst::ClockTime::from_mseconds(10));
        }
        if let Some(ref lv) = slot.audio_level {
            let _ = lv.set_state(gst::State::Null);
            let _ = lv.state(gst::ClockTime::from_mseconds(10));
        }
        // 3. Release aggregator request pads after branch shutdown.
        if let Some(ref pad) = slot.compositor_pad {
            self.compositor.release_request_pad(pad);
        }
        if let Some(ref pad) = slot.audio_mixer_pad {
            self.audiomixer.release_request_pad(pad);
        }
    }

    /// Tear down all active decoder slots (decoders, effects, pads).
    ///
    /// Order is critical to avoid both races and hangs:
    /// 0. Flush compositor/audiomixer sink pads (unblocks streaming threads).
    /// 1. Remove elements from the pipeline (pads become unlinked).
    /// 2. Transition removed elements to Null to stop residual streaming work.
    /// 3. Release compositor/audiomixer request pads after branch shutdown.
    fn teardown_slots(&mut self) {
        // Pre-flush: send FlushStart to all compositor/audiomixer sink pads
        // to unblock aggregation.  Streaming threads may be blocked in
        // downstream pushes, holding STREAM_LOCKs that set_state(Null) needs
        // for pad deactivation.  Flushing releases those locks first.
        // (We cannot simply reverse the remove/Null order — setting Null while
        // branches are still attached caused a different hang; see CHANGELOG.)
        for slot in &self.slots {
            if let Some(ref pad) = slot.compositor_pad {
                let _ = pad.send_event(gst::event::FlushStart::new());
            }
            if let Some(ref pad) = slot.audio_mixer_pad {
                let _ = pad.send_event(gst::event::FlushStart::new());
            }
        }

        let drained: Vec<VideoSlot> = self.slots.drain(..).collect();
        for slot in drained {
            self.teardown_single_slot(slot);
        }
        self.reverse_video_ducked_clip_idx = None;
        self.slot_queue_drop_late_active = false;
        self.prerender_active_clips = None;
    }

    /// Wait briefly for dynamic decode pads to link into the effects chain.
    #[allow(dead_code)]
    fn wait_for_video_links(&self) {
        if self.slots.is_empty() {
            return;
        }
        // Scale the timeout with the number of slots.  With 3+ heavy decode
        // tracks, uridecodebin may need longer to typefind and link pads.
        let timeout_ms = 1_000 + (self.slots.len() as u64) * 500;
        let deadline = Instant::now() + Duration::from_millis(timeout_ms);
        while Instant::now() < deadline {
            if self
                .slots
                .iter()
                .all(|slot| slot.video_linked.load(Ordering::Relaxed))
            {
                break;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        if !self
            .slots
            .iter()
            .all(|slot| slot.video_linked.load(Ordering::Relaxed))
        {
            for (i, slot) in self.slots.iter().enumerate() {
                let linked = slot.video_linked.load(Ordering::Relaxed);
                let clip = &self.clips[slot.clip_idx];
                log::warn!(
                    "ProgramPlayer: slot[{}] clip={} linked={} source={}",
                    i,
                    clip.id,
                    linked,
                    clip.source_path
                );
            }
            log::warn!("ProgramPlayer: timed out waiting for video pad links");
            for (i, slot) in self.slots.iter().enumerate() {
                let (_, cur, pend) = slot.decoder.state(gst::ClockTime::ZERO);
                let clip = &self.clips[slot.clip_idx];
                log::warn!(
                    "ProgramPlayer: slot[{}] decoder state {:?}/{:?} source={}",
                    i,
                    cur,
                    pend,
                    clip.source_path
                );
            }
        }
    }

    /// Build a per-slot video effects bin and return it along with effect element refs.
    ///
    /// If `lut` is `Some`, a buffer pad probe is attached to the capsfilter src pad
    /// that applies the 3D LUT to every RGBA frame. This provides real-time LUT preview
    /// without requiring LUT-baked proxy media.
    fn build_effects_bin(
        clip: &ProgramClip,
        animated_svg_rendered: bool,
        target_width: u32,
        target_height: u32,
        project_width: u32,
        project_height: u32,
        lut: Option<Arc<CubeLut>>,
        mask_shared: Arc<Mutex<Vec<crate::model::clip::ClipMask>>>,
        hsl_shared: Arc<Mutex<Option<crate::model::clip::HslQualifier>>>,
    ) -> (
        gst::Bin,
        Option<gst::Element>,                                 // videobalance
        Option<gst::Element>, // coloradj_rgb (frei0r temperature/tint)
        Option<gst::Element>, // colorbalance_3pt (frei0r shadows/midtones/highlights)
        Option<gst::Element>, // gaussianblur
        Option<gst::Element>, // squareblur (creative blur)
        Option<gst::Element>, // videocrop
        Option<gst::Element>, // videobox_crop_alpha
        Option<gst::Element>, // imagefreeze
        Option<gst::Element>, // videoflip_rotate
        Option<gst::Element>, // videoflip_flip
        Option<gst::Element>, // textoverlay
        Option<gst::Element>, // alpha_filter
        Option<gst::Element>, // alpha_chroma_key
        Option<gst::Element>, // capsfilter_zoom
        Option<gst::Element>, // videobox_zoom
        Vec<gst::Element>,    // frei0r_user_effects
        Arc<Mutex<(i32, i32, i32, i32, i32, i32, i32, i32)>>, // crop_alpha_state
    ) {
        let bin = gst::Bin::new();

        // Determine which effects are active (non-default) so we can skip
        // no-op elements and their associated videoconvert instances.  This
        // dramatically reduces per-frame CPU cost for clips without effects
        // (3 concurrent clips drops from ~51 to ~22 pipeline elements).
        let need_balance = clip.brightness != 0.0
            || clip.contrast != 1.0
            || clip.saturation != 1.0
            || clip.exposure.abs() > f64::EPSILON
            || !clip.brightness_keyframes.is_empty()
            || !clip.contrast_keyframes.is_empty()
            || !clip.saturation_keyframes.is_empty();
        let need_coloradj = (clip.temperature - 6500.0).abs() > 1.0
            || clip.tint.abs() > 0.001
            || !clip.temperature_keyframes.is_empty()
            || !clip.tint_keyframes.is_empty();
        let need_3point = clip.shadows.abs() > f64::EPSILON
            || clip.midtones.abs() > f64::EPSILON
            || clip.highlights.abs() > f64::EPSILON
            || clip.black_point.abs() > f64::EPSILON
            || clip.highlights_warmth.abs() > f64::EPSILON
            || clip.highlights_tint.abs() > f64::EPSILON
            || clip.midtones_warmth.abs() > f64::EPSILON
            || clip.midtones_tint.abs() > f64::EPSILON
            || clip.shadows_warmth.abs() > f64::EPSILON
            || clip.shadows_tint.abs() > f64::EPSILON;
        let blur_sigma = (clip.denoise * 4.0 - clip.sharpness * 6.0).clamp(-20.0, 20.0);
        let need_blur = blur_sigma.abs() > f64::EPSILON;
        let need_title = !clip.title_text.is_empty();

        // Always create videocrop + videobox_crop_alpha so live crop editing
        // works even when crop starts at zero (no pipeline rebuild needed).
        // videocrop is placed AFTER capsfilter_proj (RGBA at project resolution)
        // so cropped regions become transparent via videobox_crop_alpha.
        let videocrop = gst::ElementFactory::make("videocrop").build().ok();
        let videobox_crop_alpha = gst::ElementFactory::make("videobox").build().ok();

        // conv1 + videobalance: only if color correction is active.
        let (conv1, videobalance) = if need_balance {
            (
                gst::ElementFactory::make("videoconvert").build().ok(),
                gst::ElementFactory::make("videobalance").build().ok(),
            )
        } else {
            (None, None)
        };

        // frei0r coloradj_RGB for per-channel temperature/tint (RGBA pipeline).
        // Falls back to videobalance hue approximation if frei0r unavailable.
        let coloradj_rgb = if need_coloradj {
            gst::ElementFactory::make("frei0r-filter-coloradj-rgb")
                .build()
                .ok()
        } else {
            None
        };
        // If frei0r is unavailable for temperature/tint, ensure videobalance
        // exists so we can fall back to the hue-rotation approximation.
        let need_balance_for_temp_fallback = need_coloradj && coloradj_rgb.is_none();
        let (conv1, videobalance) = if need_balance_for_temp_fallback && videobalance.is_none() {
            (
                conv1.or_else(|| gst::ElementFactory::make("videoconvert").build().ok()),
                Some(gst::ElementFactory::make("videobalance").build().unwrap()),
            )
        } else {
            (conv1, videobalance)
        };

        // frei0r 3-point-color-balance for shadows/midtones/highlights.
        // Provides per-luminance-range control matching FFmpeg's colorbalance.
        // Falls back to videobalance polynomial approximation if unavailable.
        let colorbalance_3pt = if need_3point {
            let elem = gst::ElementFactory::make("frei0r-filter-3-point-color-balance")
                .build()
                .ok();
            if let Some(ref e) = elem {
                // Disable split-preview (default: true shows A/B comparison).
                e.set_property("split-preview", false);
            }
            elem
        } else {
            None
        };
        // If frei0r 3-point is unavailable, ensure videobalance exists so the
        // polynomial approximation for shadows/midtones/highlights is used.
        let need_balance_for_3pt_fallback = need_3point && colorbalance_3pt.is_none();
        let (conv1, videobalance) = if need_balance_for_3pt_fallback && videobalance.is_none() {
            (
                conv1.or_else(|| gst::ElementFactory::make("videoconvert").build().ok()),
                Some(gst::ElementFactory::make("videobalance").build().unwrap()),
            )
        } else {
            (conv1, videobalance)
        };

        // conv2 + gaussianblur: only if denoise/sharpen is active.
        let (conv2, gaussianblur) = if need_blur {
            (
                gst::ElementFactory::make("videoconvert").build().ok(),
                gst::ElementFactory::make("gaussianblur").build().ok(),
            )
        } else {
            (None, None)
        };

        // Separate squareblur element for creative blur (RGBA-native, no
        // format conversion needed unlike gaussianblur which requires AYUV).
        let need_creative_blur = clip.blur > f64::EPSILON;
        let squareblur = if need_creative_blur {
            gst::ElementFactory::make("frei0r-filter-squareblur")
                .build()
                .ok()
        } else {
            None
        };

        // User-applied frei0r filter effects (from clip.frei0r_effects).
        let frei0r_user_effects: Vec<gst::Element> = clip
            .frei0r_effects
            .iter()
            .filter(|e| e.enabled)
            .filter_map(|effect| {
                let gst_name = format!("frei0r-filter-{}", effect.plugin_name);
                let elem = gst::ElementFactory::make(&gst_name).build().ok()?;
                for (param, &val) in &effect.params {
                    if elem.has_property(param) {
                        set_frei0r_property(&elem, param, val);
                    }
                }
                for (param, val) in &effect.string_params {
                    if elem.has_property(param) {
                        set_frei0r_string_property(&elem, param, val);
                    }
                }
                Some(elem)
            })
            .collect();

        // Always build rotation/flip path so live inspector edits work even
        // when clips start at identity transform (no full pipeline rebuild).
        let conv3 = gst::ElementFactory::make("videoconvert").build().ok();
        let videoflip_rotate = gst::ElementFactory::make("rotate")
            .build()
            .ok()
            .or_else(|| gst::ElementFactory::make("videoflip").build().ok());

        let conv4 = gst::ElementFactory::make("videoconvert").build().ok();
        let videoflip_flip = gst::ElementFactory::make("videoflip")
            .name("videoflip_flip")
            .build()
            .ok();

        // Alpha filter: skipped — opacity is applied via compositor pad property.
        // Chroma key alpha: conditionally created when chroma key is enabled.
        //
        // The `alpha` element with `method=custom` accepts RGBA but its src
        // pad output format depends on negotiation with downstream.  The next
        // downstream cap constraint is `capsfilter_zoom` which forces RGBA at
        // the project resolution.  Without an explicit videoconvert *after*
        // `alpha`, format negotiation through the chroma-key chain can fail
        // — and on a still PNG (where `imagefreeze` only ever pushes its one
        // cached buffer) there's no recovery: the pad sits empty for the
        // entire playback and the still vanishes.  Bracketing the alpha
        // element with `conv_ck` (RGBA → alpha-friendly) and `conv_post_ck`
        // (back → RGBA) keeps the negotiation stable for both stills and
        // continuously-decoding video.
        let need_chroma_key = clip.chroma_key_enabled;
        let (conv_ck, alpha_chroma_key, conv_post_ck) = if need_chroma_key {
            (
                gst::ElementFactory::make("videoconvert").build().ok(),
                gst::ElementFactory::make("alpha")
                    .property_from_str("method", "custom")
                    .build()
                    .ok(),
                gst::ElementFactory::make("videoconvert").build().ok(),
            )
        } else {
            (None, None, None)
        };
        let alpha_filter: Option<gst::Element> = None;

        // Textoverlay: only if title text is set.
        let textoverlay = if need_title {
            gst::ElementFactory::make("textoverlay").build().ok()
        } else {
            None
        };
        // Shadow textoverlay: rendered BEFORE the main textoverlay so the shadow
        // appears behind the foreground text.  Only created when shadow is enabled.
        let shadow_textoverlay = if need_title && clip.title_shadow {
            gst::ElementFactory::make("textoverlay").build().ok()
        } else {
            None
        };
        let imagefreeze = if clip.is_freeze_frame() || (clip.is_image && !animated_svg_rendered) {
            gst::ElementFactory::make("imagefreeze").build().ok()
        } else {
            None
        };

        // Scaling chain (always needed): videoconvertscale → capsfilter
        // → videoscale → capsfilter → videobox.
        // videoconvertscale does color-convert AND scale in a single pass,
        // avoiding an intermediate full-resolution RGBA allocation. Benchmarked
        // at ~2.6× faster than separate videoconvert + videoscale for 5.3K H.265.
        //
        // On macOS, VideoToolbox (vtdec) outputs IOSurface-backed NV12 buffers.
        // If videoconvertscale uses its parallelized task runner (n-threads > 1),
        // worker threads can read from an IOSurface that has already been released,
        // causing EXC_BAD_ACCESS (SIGSEGV) in unpack_NV12.  Force single-threaded
        // conversion on macOS to prevent this race.
        let convertscale = gst::ElementFactory::make("videoconvertscale")
            .property("add-borders", true)
            .build()
            .ok();
        #[cfg(target_os = "macos")]
        if let Some(ref cs) = convertscale {
            if cs.find_property("n-threads").is_some() {
                cs.set_property("n-threads", 1u32);
            }
        }
        let capsfilter_proj = gst::ElementFactory::make("capsfilter").build().ok();
        let videoscale_zoom = gst::ElementFactory::make("videoscale").build().ok();
        let capsfilter_zoom = gst::ElementFactory::make("capsfilter").build().ok();
        let videobox_zoom = gst::ElementFactory::make("videobox").build().ok();

        // Set initial values from clip data.
        let has_coloradj = coloradj_rgb.is_some();
        let has_3point = colorbalance_3pt.is_some();
        if let Some(ref vb) = videobalance {
            let p = Self::compute_videobalance_params(
                clip.brightness,
                clip.contrast,
                clip.saturation,
                clip.temperature,
                clip.tint,
                clip.shadows,
                clip.midtones,
                clip.highlights,
                clip.exposure,
                clip.black_point,
                clip.highlights_warmth,
                clip.highlights_tint,
                clip.midtones_warmth,
                clip.midtones_tint,
                clip.shadows_warmth,
                clip.shadows_tint,
                has_coloradj,
                has_3point,
            );
            vb.set_property("brightness", p.brightness);
            vb.set_property("contrast", p.contrast);
            vb.set_property("saturation", p.saturation);
            vb.set_property("hue", p.hue);
        }
        if let Some(ref ca) = coloradj_rgb {
            // frei0r coloradj_RGB: action 0.333 = multiply mode, keep-luma off.
            ca.set_property("action", 0.333_f64);
            ca.set_property("keep-luma", false);
            let cp = Self::compute_coloradj_params(clip.temperature, clip.tint);
            ca.set_property("r", cp.r);
            ca.set_property("g", cp.g);
            ca.set_property("b", cp.b);
        }
        if let Some(ref tp) = colorbalance_3pt {
            let p = Self::compute_3point_params(
                clip.shadows,
                clip.midtones,
                clip.highlights,
                clip.black_point,
                clip.highlights_warmth,
                clip.highlights_tint,
                clip.midtones_warmth,
                clip.midtones_tint,
                clip.shadows_warmth,
                clip.shadows_tint,
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
        if let Some(ref gb) = gaussianblur {
            gb.set_property("sigma", blur_sigma);
        }
        if let Some(ref sb) = squareblur {
            sb.set_property("kernel-size", clip.blur.clamp(0.0, 1.0));
        }
        if let Some(ref a) = alpha_filter {
            a.set_property("alpha", 1.0_f64);
        }
        if let Some(ref ck) = alpha_chroma_key {
            let r = ((clip.chroma_key_color >> 16) & 0xFF) as i32;
            let g = ((clip.chroma_key_color >> 8) & 0xFF) as i32;
            let b = (clip.chroma_key_color & 0xFF) as i32;
            ck.set_property("target-r", r as u32);
            ck.set_property("target-g", g as u32);
            ck.set_property("target-b", b as u32);
            // angle: tolerance 0.0–1.0 → GStreamer 0–90 degrees
            ck.set_property(
                "angle",
                (clip.chroma_key_tolerance * 90.0).clamp(0.0, 90.0) as f32,
            );
            // noise-level: softness 0.0–1.0 → GStreamer 0–64
            ck.set_property(
                "noise-level",
                (clip.chroma_key_softness * 64.0).clamp(0.0, 64.0) as f32,
            );
        }
        if let Some(ref to) = textoverlay {
            const TITLE_REFERENCE_HEIGHT: f64 = 1080.0;
            // See apply_title_to_slot for derivation.
            const PANGO_EXPORT_MATCH: f64 = 1.0 / 3.0;
            if clip.title_text.is_empty() {
                to.set_property("silent", true);
                to.set_property("text", "");
            } else {
                to.set_property("silent", false);
                to.set_property("text", &clip.title_text);
                let base_size =
                    crate::media::title_font::parse_title_font(&clip.title_font).size_points();
                let scaled_size = (base_size
                    * PANGO_EXPORT_MATCH
                    * (project_height as f64 / TITLE_REFERENCE_HEIGHT))
                    .max(4.0);
                let adjusted_font = crate::media::title_font::build_preview_title_font_desc(
                    &clip.title_font,
                    scaled_size,
                );
                to.set_property("font-desc", &adjusted_font);
                to.set_property_from_str("halignment", "center");
                to.set_property_from_str("valignment", "center");
                let text_h_px = (scaled_size * 0.0037 * target_height as f64).round();
                let dx = ((clip.title_x - 0.5) * target_width as f64).round() as i32;
                let dy =
                    ((clip.title_y - 0.5) * target_height as f64 + text_h_px * 0.35).round() as i32;
                to.set_property("deltax", dx);
                to.set_property("deltay", dy);

                // Configure shadow textoverlay with same text/font but
                // shadow color and pixel offset.
                if let Some(ref st) = shadow_textoverlay {
                    st.set_property("silent", false);
                    st.set_property("text", &clip.title_text);
                    st.set_property("font-desc", &adjusted_font);
                    st.set_property_from_str("halignment", "center");
                    st.set_property_from_str("valignment", "center");
                    st.set_property("draw-shadow", false);
                    st.set_property("draw-outline", false);
                    let scale_factor = project_height as f64 / TITLE_REFERENCE_HEIGHT;
                    let sx = (clip.title_shadow_offset_x * scale_factor).round() as i32;
                    let sy = (clip.title_shadow_offset_y * scale_factor).round() as i32;
                    st.set_property("deltax", dx + sx);
                    st.set_property("deltay", dy + sy);
                    // Shadow color (RRGGBBAA → AARRGGBB for GStreamer)
                    let (sr, sg, sb, sa) =
                        crate::ui::colors::rgba_u32_to_u8(clip.title_shadow_color);
                    let s_argb: u32 =
                        ((sa as u32) << 24) | ((sr as u32) << 16) | ((sg as u32) << 8) | sb as u32;
                    st.set_property("color", s_argb);
                }
            }
        }

        // Set processing-resolution capsfilters.
        // capsfilter_proj constrains to RGBA at target preview processing size.
        let proj_caps = gst::Caps::builder("video/x-raw")
            .field("format", "RGBA")
            .field("width", target_width as i32)
            .field("height", target_height as i32)
            .field("pixel-aspect-ratio", gst::Fraction::new(1, 1))
            .build();
        if let Some(ref cf) = capsfilter_proj {
            cf.set_property("caps", &proj_caps);
        }
        if let Some(ref cf) = capsfilter_zoom {
            cf.set_property("caps", &proj_caps.copy());
        }

        // Resolution-only capsfilter — not currently used, left for future
        // optimization where videoscale could run in native pixel format.
        // For now, videoconvertscale handles both conversion and scaling.

        // Build chain: downscale to preview processing resolution EARLY so all effects
        // process at target size instead of source resolution (e.g. 5.3K for GoPro).
        // Order: [imagefreeze] → [capssetter] → convertscale to target res
        // → [crop + alpha repad] → [effects] → zoom/position → rotate/flip → title.
        //
        // imagefreeze MUST come first: image sources (PNG, JPEG, …) produce a
        // single decoded frame then EOS.  If imagefreeze is placed after elements
        // whose properties change at runtime (crop, zoom), a property change
        // triggers caps renegotiation that propagates upstream to the decoder,
        // but the decoder already sent EOS and cannot supply a new buffer —
        // causing "streaming stopped, reason error (-5)" in imagefreeze's src
        // loop.  By placing it first we turn the single frame into an infinite
        // stream before any mutable elements, so renegotiation always succeeds.
        let mut chain: Vec<gst::Element> = Vec::new();
        // 0. imagefreeze for still-image / freeze-frame clips (before everything).
        if let Some(ref e) = imagefreeze {
            chain.push(e.clone());
        }
        // 0a. Override pixel-aspect-ratio for anamorphic desqueeze
        if (clip.anamorphic_desqueeze - 1.0).abs() > 0.001 {
            if let Ok(cs) = gst::ElementFactory::make("capssetter")
                .property("join", true)
                .property(
                    "caps",
                    &gst::Caps::builder("video/x-raw")
                        .field(
                            "pixel-aspect-ratio",
                            gst::Fraction::new(
                                (clip.anamorphic_desqueeze * 1000.0).round() as i32,
                                1000,
                            ),
                        )
                        .build(),
                )
                .build()
            {
                chain.push(cs);
            }
        }
        // 0b. When a real-time LUT is active, override the source colorimetry
        //    to BT.709 full-range.  Many camera files (S-Log3 HEVC, etc.) carry
        //    unknown/unset colorimetry; GStreamer's default YUV→RGB conversion
        //    for unknown sources diverges from FFmpeg's swscale default, causing
        //    a systematic RGB offset (~4 RMSE) that the steep LUT amplifies into
        //    a visible pink/magenta cast.  Setting BT.709 via capssetter reduces
        //    the post-LUT RMSE from ~6.4 to ~2.8 (measured with S-Log3 33-pt LUT).
        if lut.is_some() {
            if let Some(cs) = gst::ElementFactory::make("capssetter")
                .property("join", true)
                .property(
                    "caps",
                    &gst::Caps::builder("video/x-raw")
                        .field("colorimetry", "1:3:5:1")
                        .build(),
                )
                .build()
                .ok()
            {
                chain.push(cs);
            }
        }
        // 1. Convert + downscale to project resolution in a single pass.
        if let Some(ref e) = convertscale {
            chain.push(e.clone());
        }
        if let Some(ref e) = capsfilter_proj {
            chain.push(e.clone());
        }
        // 1c. Real-time 3D LUT via buffer pad probe on capsfilter src.
        // Applied AFTER downscale (RGBA at processing resolution) so the
        // trilinear interpolation runs on the smaller preview buffer, and
        // BEFORE any color effects — matching export LUT placement order.
        if let (Some(ref cf), Some(lut_ref)) = (&capsfilter_proj, &lut) {
            let lut_clone = lut_ref.clone();
            cf.static_pad("src").unwrap().add_probe(
                gst::PadProbeType::BUFFER,
                move |_pad, info| {
                    if let Some(gst::PadProbeData::Buffer(ref mut buffer)) = info.data {
                        let buf = buffer.make_mut();
                        if let Ok(mut map) = buf.map_writable() {
                            lut_clone.apply_to_rgba_buffer(map.as_mut_slice());
                        }
                    }
                    gst::PadProbeReturn::Ok
                },
            );
        }
        // 2. Crop via alpha-channel zeroing on RGBA buffers.  Unlike videocrop,
        //    this does NOT change frame dimensions, avoiding caps renegotiation
        //    crashes through multiqueue.
        // Crop state: (left, right, top, bottom, src_width, src_height).
        // Source dimensions are used to compute letterbox offset so crop
        // targets video content, not letterbox bars.
        let crop_alpha_state: Arc<Mutex<(i32, i32, i32, i32, i32, i32, i32, i32)>> =
            Arc::new(Mutex::new((
                clip.crop_left,
                clip.crop_right,
                clip.crop_top,
                clip.crop_bottom,
                0i32,
                0i32,
                project_width as i32,
                project_height as i32,
            )));
        {
            let crop_state = crop_alpha_state.clone();
            if let Ok(identity) = gst::ElementFactory::make("identity").build() {
                identity.static_pad("src").unwrap().add_probe(
                    gst::PadProbeType::BUFFER,
                    move |_pad, info| {
                        let (cl, cr, ct, cb, lb_h, lb_v, proj_w, proj_h) =
                            *crop_state.lock().unwrap();
                        if cl == 0 && cr == 0 && ct == 0 && cb == 0 {
                            return gst::PadProbeReturn::Ok;
                        }
                        if let Some(gst::PadProbeData::Buffer(ref mut buffer)) = info.data {
                            let buf = buffer.make_mut();
                            if let Ok(mut map) = buf.map_writable() {
                                let data = map.as_mut_slice();
                                // Determine frame dimensions from buffer size (RGBA = 4 bytes/pixel)
                                // We need to figure out width from the buffer; use the pad caps.
                                let len = data.len();
                                // Assume RGBA, estimate width from stride
                                // For safety, get width from nearby pad if possible
                                // Fallback: assume square-ish, or use a reasonable width
                                let stride_guess = if len > 0 {
                                    // Typical: width * 4 bytes per pixel * height = len
                                    // We'll look for the width from the buffer's video meta
                                    // or approximate from common resolutions
                                    0usize // will be computed below
                                } else {
                                    return gst::PadProbeReturn::Ok;
                                };
                                let _ = stride_guess;
                                // Use video frame info for accurate dimensions
                                let info_ref = gstreamer_video::VideoInfo::from_caps(
                                    &_pad.current_caps().unwrap_or_else(|| {
                                        gst::Caps::builder("video/x-raw")
                                            .field("format", "RGBA")
                                            .field("width", 1920i32)
                                            .field("height", 1080i32)
                                            .build()
                                    }),
                                );
                                let (w, h, stride) = match info_ref {
                                    Ok(ref vi) => {
                                        let s = vi.stride()[0] as usize;
                                        (
                                            vi.width() as i32,
                                            vi.height() as i32,
                                            if s > 0 { s } else { vi.width() as usize * 4 },
                                        )
                                    }
                                    Err(_) => return gst::PadProbeReturn::Ok,
                                };
                                // Validate buffer size matches expected dimensions
                                let expected = stride * h as usize;
                                if data.len() < expected {
                                    return gst::PadProbeReturn::Ok;
                                }
                                // Scale crop from project pixels to content-area frame pixels,
                                // then add letterbox offset.
                                let content_w = (w as i32 - 2 * lb_h.max(0)).max(1) as f64;
                                let content_h = (h as i32 - 2 * lb_v.max(0)).max(1) as f64;
                                let scale_x = if proj_w > 0 {
                                    content_w / proj_w as f64
                                } else {
                                    1.0
                                };
                                let scale_y = if proj_h > 0 {
                                    content_h / proj_h as f64
                                } else {
                                    1.0
                                };
                                let ct = ((ct.max(0) as f64 * scale_y).round() as usize)
                                    + lb_v.max(0) as usize;
                                let cb = ((cb.max(0) as f64 * scale_y).round() as usize)
                                    + lb_v.max(0) as usize;
                                let cl = ((cl.max(0) as f64 * scale_x).round() as usize)
                                    + lb_h.max(0) as usize;
                                let cr = ((cr.max(0) as f64 * scale_x).round() as usize)
                                    + lb_h.max(0) as usize;
                                let h = h as usize;
                                let w = w as usize;
                                for row in 0..h {
                                    if row < ct || row >= h.saturating_sub(cb) {
                                        // Entire row is cropped — zero alpha
                                        for x in 0..w {
                                            let idx = row * stride + x * 4 + 3;
                                            if idx < data.len() {
                                                data[idx] = 0;
                                            }
                                        }
                                    } else {
                                        // Left crop
                                        for x in 0..cl.min(w) {
                                            let idx = row * stride + x * 4 + 3;
                                            if idx < data.len() {
                                                data[idx] = 0;
                                            }
                                        }
                                        // Right crop
                                        for x in w.saturating_sub(cr)..w {
                                            let idx = row * stride + x * 4 + 3;
                                            if idx < data.len() {
                                                data[idx] = 0;
                                            }
                                        }
                                    }
                                }
                            }
                        }
                        gst::PadProbeReturn::Ok
                    },
                );
                chain.push(identity);
            }
        }
        // videocrop/videobox_crop_alpha remain in the slot for wipe transitions
        // but are NOT added to the effects chain for user crop.
        // 2b. Shape mask alpha probe — multiplies alpha channel by mask SDF.
        //     Placed after crop (cropped regions stay transparent) and before
        //     color effects so the mask operates in pre-transform clip space.
        // Populate the shared mask data for live updates from inspector sliders.
        *mask_shared.lock().unwrap() = clip.masks.clone();
        // Always keep the live mask stage in the bin so transition previews can
        // inject temporary masks even when the clip has no authored masks.
        if let Some(mask_identity) = gst::ElementFactory::make("identity")
            .property("name", "us-mask-identity")
            .build()
            .ok()
        {
            let mask_ref = mask_shared.clone();
            mask_identity.static_pad("src").unwrap().add_probe(
                gst::PadProbeType::BUFFER,
                move |_pad, info| {
                    if let Some(gst::PadProbeData::Buffer(ref mut buffer)) = info.data {
                        let masks = mask_ref.lock().unwrap().clone();
                        if masks.iter().any(|m| m.enabled) {
                            let buf = buffer.make_mut();
                            let pts_ns = buf.pts().map(|p| p.nseconds()).unwrap_or(0);
                            if let Ok(mut map) = buf.map_writable() {
                                let data = map.as_mut_slice();
                                let total = data.len();
                                if total >= 4 {
                                    let (width, height) = if let Some(caps) = _pad.current_caps() {
                                        let s = caps.structure(0).unwrap();
                                        (
                                            s.get::<i32>("width").unwrap_or(0) as usize,
                                            s.get::<i32>("height").unwrap_or(0) as usize,
                                        )
                                    } else {
                                        let pixels = total / 4;
                                        let w = (pixels as f64).sqrt() as usize;
                                        (w, if w > 0 { pixels / w } else { 0 })
                                    };
                                    if width > 0 && height > 0 && width * height * 4 <= total {
                                        crate::media::mask_alpha::apply_masks_to_rgba_buffer(
                                            &masks, data, width, height, pts_ns,
                                        );
                                    }
                                }
                            }
                        }
                    }
                    gst::PadProbeReturn::Ok
                },
            );
            chain.push(mask_identity);
        }
        // 3. Effects at project resolution (much cheaper than source res).
        if let Some(ref e) = conv1 {
            chain.push(e.clone());
        }
        if let Some(ref e) = videobalance {
            chain.push(e.clone());
        }
        // frei0r coloradj_RGB (temperature/tint via per-channel RGB gains).
        // Operates on RGBA which is already the pipeline format after capsfilter_proj.
        if let Some(ref e) = coloradj_rgb {
            chain.push(e.clone());
        }
        // frei0r 3-point-color-balance (shadows/midtones/highlights).
        if let Some(ref e) = colorbalance_3pt {
            chain.push(e.clone());
        }
        // 3a'. HSL Qualifier pad probe — secondary color correction that
        // isolates pixels by hue/saturation/luminance and applies a follow-up
        // grade only to the matched region. Placed HERE — after the primary
        // color chain (videobalance / coloradj_RGB / 3-point balance) and
        // BEFORE denoise/sharpen/blur — because:
        //   1. videobalance + both frei0r color elements preserve RGBA, so
        //      the buffer format is guaranteed RGBA at this point (required
        //      for the pad probe's byte-addressing). `gaussianblur` runs on
        //      AYUV downstream, which would break the probe.
        //   2. Secondary color correction should not see blurred/denoised
        //      pixels or the HSL matte edges would bleed.
        *hsl_shared.lock().unwrap() = clip.hsl_qualifier.clone();
        if let Some(hsl_identity) = gst::ElementFactory::make("identity")
            .property("name", "us-hsl-identity")
            .build()
            .ok()
        {
            let hsl_ref = hsl_shared.clone();
            hsl_identity.static_pad("src").unwrap().add_probe(
                gst::PadProbeType::BUFFER,
                move |_pad, info| {
                    if let Some(gst::PadProbeData::Buffer(ref mut buffer)) = info.data {
                        let q_opt = hsl_ref.lock().unwrap().clone();
                        if let Some(q) = q_opt {
                            if !q.is_neutral() {
                                // Read format + dimensions from live caps.
                                // Bail unless the format is a 4-byte RGBA-like
                                // layout so we never scribble over AYUV/YUV
                                // buffers (safety: `gaussianblur` is AYUV but
                                // we're placed before it, so this is a belt
                                // and suspenders check).
                                let caps = _pad.current_caps();
                                let (width, height, is_rgba) = if let Some(caps) = caps {
                                    if let Some(s) = caps.structure(0) {
                                        let format = s
                                            .get::<&str>("format")
                                            .unwrap_or("")
                                            .to_string();
                                        let w = s.get::<i32>("width").unwrap_or(0) as usize;
                                        let h = s.get::<i32>("height").unwrap_or(0) as usize;
                                        let is_rgba = matches!(
                                            format.as_str(),
                                            "RGBA" | "BGRA" | "ARGB" | "ABGR"
                                        );
                                        (w, h, is_rgba)
                                    } else {
                                        (0, 0, false)
                                    }
                                } else {
                                    (0, 0, false)
                                };
                                if is_rgba && width > 0 && height > 0 {
                                    let buf = buffer.make_mut();
                                    if let Ok(mut map) = buf.map_writable() {
                                        let data = map.as_mut_slice();
                                        if width * height * 4 <= data.len() {
                                            crate::media::hsl_qualifier::apply_hsl_qualifier_to_rgba_buffer(
                                                &q, data, width, height,
                                            );
                                        }
                                    }
                                }
                            }
                        }
                    }
                    gst::PadProbeReturn::Ok
                },
            );
            chain.push(hsl_identity);
        }
        if let Some(ref e) = conv2 {
            chain.push(e.clone());
        }
        if let Some(ref e) = gaussianblur {
            chain.push(e.clone());
        }
        if let Some(ref e) = squareblur {
            chain.push(e.clone());
        }
        // 3b. User-applied frei0r filter effects (after built-in color/blur and HSL).
        for e in &frei0r_user_effects {
            chain.push(e.clone());
        }
        // 3b. Chroma key (after color/blur, before zoom).
        //
        // Bracketed by videoconvert on both sides so the format negotiation
        // through the alpha element stays stable — see the comment on
        // `let need_chroma_key = …` above for why this matters for stills.
        if let Some(ref e) = conv_ck {
            chain.push(e.clone());
        }
        if let Some(ref e) = alpha_chroma_key {
            chain.push(e.clone());
        }
        if let Some(ref e) = conv_post_ck {
            chain.push(e.clone());
        }
        if let Some(ref e) = alpha_filter {
            chain.push(e.clone());
        }
        // 4. Zoom / position adjustment.
        if let Some(ref e) = videoscale_zoom {
            chain.push(e.clone());
        }
        if let Some(ref e) = capsfilter_zoom {
            chain.push(e.clone());
        }
        if let Some(ref e) = videobox_zoom {
            chain.push(e.clone());
        }
        // 5. Rotate / flip after zoom so scaled-down clips don't get
        // prematurely clipped by rotation at full-frame size.
        if let Some(ref e) = conv3 {
            chain.push(e.clone());
        }
        if let Some(ref e) = videoflip_rotate {
            chain.push(e.clone());
        }
        if let Some(ref e) = conv4 {
            chain.push(e.clone());
        }
        if let Some(ref e) = videoflip_flip {
            chain.push(e.clone());
        }
        // Shadow textoverlay goes BEFORE the main text so it renders behind.
        if let Some(ref e) = shadow_textoverlay {
            chain.push(e.clone());
        }
        if let Some(ref e) = textoverlay {
            chain.push(e.clone());
        }

        if chain.is_empty() {
            // Fallback: just a videoconvert
            let vc = gst::ElementFactory::make("videoconvert").build().unwrap();
            bin.add(&vc).ok();
            let sink_pad = vc.static_pad("sink").unwrap();
            let src_pad = vc.static_pad("src").unwrap();
            let ghost_sink = gst::GhostPad::with_target(&sink_pad).unwrap();
            // Drop RECONFIGURE events at the bin boundary so internal caps
            // changes (e.g. videocrop dimension changes) never propagate
            // upstream to the decoder/demuxer, preventing not-negotiated
            // errors during flush-seeks.
            ghost_sink.add_probe(gst::PadProbeType::EVENT_UPSTREAM, |_pad, info| {
                if let Some(ev) = info.event() {
                    if let gst::EventView::Reconfigure(_) = ev.view() {
                        return gst::PadProbeReturn::Drop;
                    }
                }
                gst::PadProbeReturn::Ok
            });
            bin.add_pad(&ghost_sink).ok();
            bin.add_pad(&gst::GhostPad::with_target(&src_pad).unwrap())
                .ok();
        } else {
            let refs: Vec<&gst::Element> = chain.iter().collect();
            bin.add_many(&refs).ok();
            gst::Element::link_many(&refs).ok();
            let sink_pad = chain.first().unwrap().static_pad("sink").unwrap();
            let src_pad = chain.last().unwrap().static_pad("src").unwrap();
            let ghost_sink = gst::GhostPad::with_target(&sink_pad).unwrap();
            ghost_sink.add_probe(gst::PadProbeType::EVENT_UPSTREAM, |_pad, info| {
                if let Some(ev) = info.event() {
                    if let gst::EventView::Reconfigure(_) = ev.view() {
                        return gst::PadProbeReturn::Drop;
                    }
                }
                gst::PadProbeReturn::Ok
            });
            bin.add_pad(&ghost_sink).ok();
            bin.add_pad(&gst::GhostPad::with_target(&src_pad).unwrap())
                .ok();
        }

        (
            bin,
            videobalance,
            coloradj_rgb,
            colorbalance_3pt,
            gaussianblur,
            squareblur,
            videocrop,
            videobox_crop_alpha,
            imagefreeze,
            videoflip_rotate,
            videoflip_flip,
            textoverlay,
            alpha_filter,
            alpha_chroma_key,
            capsfilter_zoom,
            videobox_zoom,
            frei0r_user_effects,
            crop_alpha_state,
        )
    }

    /// Quick check whether a media file contains an audio stream.
    /// Uses GStreamer Discoverer with a short timeout. Defaults to `true` on error.
    fn probe_has_audio_stream(path: &str) -> bool {
        use gstreamer_pbutils::Discoverer;
        let uri = if path.starts_with("file://") {
            path.to_string()
        } else {
            format!("file://{}", path)
        };
        let Ok(disc) = Discoverer::new(gst::ClockTime::from_seconds(2)) else {
            return true;
        };
        match disc.discover_uri(&uri) {
            Ok(info) => {
                let has = !info.audio_streams().is_empty();
                log::info!("ProgramPlayer: probe_has_audio_stream({}) = {}", path, has);
                has
            }
            Err(_) => true,
        }
    }

    /// Return whether the media at `path` has an audio stream, using a small
    /// per-player cache to avoid repeated Discoverer work during rebuilds.
    fn probe_has_audio_stream_cached(&mut self, path: &str) -> bool {
        if let Some(&has_audio) = self.audio_stream_probe_cache.get(path) {
            return has_audio;
        }
        let has_audio = Self::probe_has_audio_stream(path);
        self.audio_stream_probe_cache
            .insert(path.to_string(), has_audio);
        has_audio
    }

    /// Fast audio-presence lookup for hot paths. Uses declared clip audio and
    /// existing cache only; defaults to true when unknown to avoid blocking
    /// hot rebuild paths with Discoverer probes.
    fn has_audio_for_path_fast(&self, path: &str, declared_has_audio: bool) -> bool {
        if !declared_has_audio {
            return false;
        }
        self.audio_stream_probe_cache
            .get(path)
            .copied()
            .unwrap_or(true)
    }

    /// Returns true if clip at `pos` in `active` is fully occluded by a clip
    /// above it (higher zorder).  Conservative: only considers full-frame
    /// opaque clips with no scale-down as occluders.
    fn is_clip_video_occluded(&self, active: &[usize], pos: usize) -> bool {
        // Any clip above `pos` that is effectively default full-frame transform
        // (opaque, centered, unrotated, unflipped, uncropped, scale>=1) is
        // treated as a true occluder.
        for j in (pos + 1)..active.len() {
            let c = &self.clips[active[j]];
            if clip_can_fully_occlude(c) {
                return true;
            }
        }
        false
    }

    /// Build an audio-only slot for a fully-occluded clip.  Skips video
    /// decode, effects, and compositor pad entirely.
    fn build_audio_only_slot_for_clip(&mut self, clip_idx: usize) -> Option<VideoSlot> {
        let clip = self.clips[clip_idx].clone();
        // For audio-only clips, always use the original source — proxy transcodes
        // are video-optimized and add unnecessary decode overhead for pure audio.
        let (effective_path, using_proxy, proxy_key, _) = if clip.is_audio_only {
            (
                clip.source_path.clone(),
                false,
                clip.source_path.clone(),
                false,
            )
        } else {
            self.resolve_source_path_for_clip(&clip)
        };
        let uri = format!("file://{}", effective_path);

        let clip_has_audio =
            self.has_audio_for_path_fast(&effective_path, clip.has_embedded_audio());
        if !clip_has_audio {
            log::info!(
                "build_audio_only_slot: clip {} has no audio, skipping entirely",
                clip.id
            );
            return None;
        }

        if self.proxy_enabled {
            log::info!(
                "build_audio_only_slot: clip={} source={} resolved={} mode={} proxy_key={} resolved_exists={} (video occluded)",
                clip.id,
                clip.source_path,
                effective_path,
                if using_proxy { "proxy" } else { "fallback-original" },
                proxy_key,
                Path::new(&effective_path).exists()
            );
            if !using_proxy {
                if self.proxy_fallback_warned_keys.insert(proxy_key.clone()) {
                    log::warn!(
                        "build_audio_only_slot: proxy enabled but no proxy path for clip={} key={} (falling back to original until proxy is ready)",
                        clip.id,
                        proxy_key
                    );
                } else {
                    log::debug!(
                        "build_audio_only_slot: proxy still unavailable for clip={} key={} (continuing original fallback)",
                        clip.id,
                        proxy_key
                    );
                }
            } else {
                self.proxy_fallback_warned_keys.remove(&proxy_key);
            }
        } else {
            log::info!(
                "build_audio_only_slot: clip={} source={} resolved={} mode=original (video occluded)",
                clip.id,
                clip.source_path,
                effective_path
            );
        }

        // Create decoder with audio-only caps to skip video decode.
        let audio_caps = gst::Caps::builder("audio/x-raw").build();
        let mut builder = gst::ElementFactory::make("uridecodebin")
            .property("uri", &uri)
            .property("caps", &audio_caps);
        // For audio-only clips, reduce buffering overhead.
        if clip.is_audio_only {
            builder = builder.property("use-buffering", false);
        }
        let decoder = builder.build().ok()?;

        if self.pipeline.add(&decoder).is_err() {
            return None;
        }

        // Audio path: audioconvert → audioresample → capsfilter(48 kHz stereo)
        // → [equalizer-nbands?] → [audiopanorama?] → [level?] → audiomixer pad.
        // For audio-only track clips, skip EQ/pan/level when they're at defaults to reduce overhead.
        let needs_eq = clip.eq_bands.iter().any(|b| b.gain.abs() > 0.001);
        let needs_pan = clip.pan.abs() > 0.001 || !clip.pan_keyframes.is_empty();
        // Skip level metering for audio-only track clips to reduce CPU — the per-track
        // meters will use the main pipeline's level elements for video-embedded audio.
        let needs_level = !clip.is_audio_only;
        // Voice enhance is built whenever the clip has the toggle on, so the
        let (
            audio_conv,
            audio_resample,
            audio_capsfilter,
            audio_match_equalizer_built,
            audio_equalizer,
            audio_panorama,
            audio_level,
            amix_pad,
        ) = {
            let mut ac = gst::ElementFactory::make("audioconvert").build().ok();
            let mut ar = None;
            let mut cf = None;
            let mut eq = if needs_eq {
                gst::ElementFactory::make("equalizer-nbands")
                    .property("num-bands", 3u32)
                    .build()
                    .ok()
            } else {
                None
            };
            // 7-band match EQ — only built when the clip has a non-empty match_eq_bands.
            let mut match_eq = if !clip.match_eq_bands.is_empty() {
                gst::ElementFactory::make("equalizer-nbands")
                    .property("num-bands", clip.match_eq_bands.len() as u32)
                    .build()
                    .ok()
            } else {
                None
            };
            let mut ap = if needs_pan {
                gst::ElementFactory::make("audiopanorama").build().ok()
            } else {
                None
            };
            let mut lv = if needs_level {
                gst::ElementFactory::make("level")
                    .property("post-messages", true)
                    .property("interval", 50_000_000u64)
                    .build()
                    .ok()
            } else {
                None
            };
            let pad = if let Some(ac_elem) = ac.clone() {
                if self.pipeline.add(&ac_elem).is_ok() {
                    let mut link_src = if let Some((resample, capsfilter, normalized_src)) =
                        attach_preview_audio_normalizer_with_channel_mode(
                            &self.pipeline,
                            &ac_elem,
                            clip.audio_channel_mode,
                            "build_audio_only_slot",
                        ) {
                        ar = Some(resample);
                        cf = Some(capsfilter);
                        Some(normalized_src)
                    } else {
                        log::warn!(
                            "build_audio_only_slot: failed to normalize preview audio for clip={}, skipping audio path",
                            clip.id
                        );
                        self.pipeline.remove(&ac_elem).ok();
                        ac = None;
                        eq = None;
                        match_eq = None;
                        ap = None;
                        lv = None;
                        None
                    };
                    if link_src.is_none() {
                        None
                    } else {
                        // Insert match EQ (7-band, before user EQ) when present.
                        if let Some(ref m_eq) = match_eq {
                            if self.pipeline.add(m_eq).is_ok() {
                                for (i, band) in clip.match_eq_bands.iter().enumerate() {
                                    eq_set_band(m_eq, i as u32, band.freq, band.gain, band.q);
                                }
                                if let (Some(prev_src), Some(meq_sink)) =
                                    (link_src.clone(), m_eq.static_pad("sink"))
                                {
                                    if prev_src.link(&meq_sink).is_ok() {
                                        link_src = m_eq.static_pad("src");
                                        log::info!(
                                            "build_audio_only_slot: match-eq ({} bands) linked OK",
                                            clip.match_eq_bands.len()
                                        );
                                    } else {
                                        self.pipeline.remove(m_eq).ok();
                                        match_eq = None;
                                    }
                                } else {
                                    self.pipeline.remove(m_eq).ok();
                                    match_eq = None;
                                }
                            } else {
                                match_eq = None;
                            }
                        }
                        // Insert equalizer between audioconvert and audiopanorama.
                        if let Some(ref equalizer) = eq {
                            if self.pipeline.add(equalizer).is_ok() {
                                // Set initial EQ band params from clip.
                                for i in 0..3u32 {
                                    let b = &clip.eq_bands[i as usize];
                                    eq_set_band(equalizer, i, b.freq, b.gain, b.q);
                                }
                                if let (Some(ac_src), Some(eq_sink)) =
                                    (link_src.clone(), equalizer.static_pad("sink"))
                                {
                                    let _ = ac_src.link(&eq_sink);
                                    link_src = equalizer.static_pad("src");
                                    log::info!("build_audio_only_slot: equalizer-nbands linked OK");
                                } else {
                                    log::warn!(
                                    "build_audio_only_slot: equalizer pad link failed, removing"
                                );
                                    self.pipeline.remove(equalizer).ok();
                                    eq = None;
                                }
                            } else {
                                log::warn!(
                                    "build_audio_only_slot: failed to add equalizer to pipeline"
                                );
                                eq = None;
                            }
                        }
                        if let Some(ref pano) = ap {
                            if self.pipeline.add(pano).is_ok() {
                                pano.set_property(
                                    "panorama",
                                    self.effective_main_clip_pan(clip_idx, self.timeline_pos_ns)
                                        as f32,
                                );
                                if let (Some(prev_src), Some(pano_sink)) =
                                    (link_src.clone(), pano.static_pad("sink"))
                                {
                                    let _ = prev_src.link(&pano_sink);
                                    link_src = pano.static_pad("src");
                                } else {
                                    self.pipeline.remove(pano).ok();
                                    ap = None;
                                }
                            } else {
                                ap = None;
                            }
                        }
                        if let Some(ref level) = lv {
                            if self.pipeline.add(level).is_ok() {
                                if let (Some(link_out), Some(level_sink)) =
                                    (link_src.clone(), level.static_pad("sink"))
                                {
                                    let _ = link_out.link(&level_sink);
                                    link_src = level.static_pad("src");
                                } else {
                                    self.pipeline.remove(level).ok();
                                    lv = None;
                                }
                            } else {
                                lv = None;
                            }
                        }
                        if let Some(mp) = self.audiomixer.request_pad_simple("sink_%u") {
                            mp.set_property(
                                "volume",
                                self.effective_main_clip_volume(clip_idx, self.timeline_pos_ns),
                            );
                            if let Some(src) = link_src {
                                let _ = src.link(&mp);
                            }
                            Some(mp)
                        } else {
                            if let Some(ref level) = lv {
                                self.pipeline.remove(level).ok();
                            }
                            lv = None;
                            None
                        }
                    }
                } else {
                    ar = None;
                    cf = None;
                    match_eq = None;
                    ap = None;
                    lv = None;
                    None
                }
            } else {
                ar = None;
                cf = None;
                eq = None;
                match_eq = None;
                ap = None;
                lv = None;
                None
            };
            (ac, ar, cf, match_eq, eq, ap, lv, pad)
        };

        // Dynamic pad-added: only link audio pads (no video sink available).
        let audio_sink = audio_conv.as_ref().and_then(|ac| ac.static_pad("sink"));
        let video_linked = Arc::new(AtomicBool::new(true)); // no video to link
        let audio_linked = Arc::new(AtomicBool::new(false));
        let audio_linked_for_cb = audio_linked.clone();
        let clip_id_for_cb = clip.id.clone();
        decoder.connect_pad_added(move |_dec, pad| {
            let caps = pad.current_caps().or_else(|| Some(pad.query_caps(None)));
            if let Some(caps) = caps {
                if let Some(s) = caps.structure(0) {
                    let name = s.name().to_string();
                    if name.starts_with("audio/") {
                        if let Some(ref sink) = audio_sink {
                            if pad.link(sink).is_ok() {
                                audio_linked_for_cb.store(true, Ordering::Relaxed);
                                log::info!(
                                    "build_audio_only_slot: audio linked clip={}",
                                    clip_id_for_cb
                                );
                            }
                        }
                    }
                }
            }
        });

        if let Some(ref ac) = audio_conv {
            let _ = ac.sync_state_with_parent();
        }
        if let Some(ref ar) = audio_resample {
            let _ = ar.sync_state_with_parent();
        }
        if let Some(ref cf) = audio_capsfilter {
            let _ = cf.sync_state_with_parent();
        }
        if let Some(ref meq_elem) = audio_match_equalizer_built {
            let _ = meq_elem.sync_state_with_parent();
        }
        if let Some(ref eq_elem) = audio_equalizer {
            let _ = eq_elem.sync_state_with_parent();
        }
        if let Some(ref ap) = audio_panorama {
            let _ = ap.sync_state_with_parent();
        }
        if let Some(ref lv) = audio_level {
            let _ = lv.sync_state_with_parent();
        }
        let _ = decoder.sync_state_with_parent();

        // Dummy effects_bin — never added to pipeline, harmless in teardown.
        let effects_bin = gst::Bin::new();

        Some(VideoSlot {
            clip_idx,
            decoder,
            video_linked,
            audio_linked,
            effects_bin,
            compositor_pad: None,
            audio_mixer_pad: amix_pad,
            audio_conv,
            audio_resample,
            audio_capsfilter,
            audio_equalizer,
            audio_match_equalizer: audio_match_equalizer_built,
            audio_panorama,
            audio_level,
            videobalance: None,
            coloradj_rgb: None,
            colorbalance_3pt: None,
            gaussianblur: None,
            squareblur: None,

            videocrop: None,
            videobox_crop_alpha: None,
            crop_alpha_state: Arc::new(Mutex::new((
                0i32, 0i32, 0i32, 0i32, 0i32, 0i32, 0i32, 0i32,
            ))),
            imagefreeze: None,
            videoflip_rotate: None,
            videoflip_flip: None,
            textoverlay: None,
            alpha_filter: None,
            alpha_chroma_key: None,
            capsfilter_zoom: None,
            videobox_zoom: None,
            frei0r_user_effects: Vec::new(),
            slot_queue: None,
            comp_arrival_seq: Arc::new(AtomicU64::new(0)),
            hidden: false,
            animated_svg_rendered: false,
            is_prerender_slot: false,
            prerender_segment_start_ns: None,
            transition_enter_offset_ns: 0,
            is_blend_mode: false,
            blend_alpha: Arc::new(Mutex::new(1.0)),
            mask_data: Arc::new(Mutex::new(Vec::new())),
            hsl_data: Arc::new(Mutex::new(None)),
        })
    }

    fn build_prerender_video_slot(
        &mut self,
        source_path: &str,
        segment_start_ns: u64,
        zorder_offset: usize,
        _tune_multiqueue: bool,
    ) -> Option<VideoSlot> {
        let uri = format!("file://{}", source_path);
        let decoder = gst::ElementFactory::make("uridecodebin")
            .property("uri", &uri)
            .build()
            .ok()?;
        if self.pipeline.add(&decoder).is_err() {
            return None;
        }
        let comp_pad = self.compositor.request_pad_simple("sink_%u")?;
        comp_pad.set_property("zorder", (zorder_offset + 1) as u32);
        comp_pad.set_property("alpha", 1.0_f64);

        let (proc_w, proc_h) = self.preview_processing_dimensions();
        let effects_bin = gst::Bin::new();
        let convertscale = gst::ElementFactory::make("videoconvertscale")
            .property("add-borders", true)
            .build()
            .ok()?;
        // On macOS, vtdec IOSurface-backed buffers must not be read by parallel
        // converter worker threads — force single-threaded conversion.
        #[cfg(target_os = "macos")]
        if convertscale.find_property("n-threads").is_some() {
            convertscale.set_property("n-threads", 1u32);
        }
        let capsfilter = gst::ElementFactory::make("capsfilter").build().ok()?;
        let proc_caps = gst::Caps::builder("video/x-raw")
            .field("width", proc_w as i32)
            .field("height", proc_h as i32)
            .field("pixel-aspect-ratio", gst::Fraction::new(1, 1))
            .build();
        capsfilter.set_property("caps", &proc_caps);
        if effects_bin.add_many([&convertscale, &capsfilter]).is_err()
            || gst::Element::link_many([&convertscale, &capsfilter]).is_err()
        {
            self.compositor.release_request_pad(&comp_pad);
            self.pipeline.remove(&decoder).ok();
            return None;
        }
        let effects_sink = convertscale.static_pad("sink")?;
        let effects_src = capsfilter.static_pad("src")?;
        if effects_bin
            .add_pad(&gst::GhostPad::with_target(&effects_sink).ok()?)
            .is_err()
            || effects_bin
                .add_pad(&gst::GhostPad::with_target(&effects_src).ok()?)
                .is_err()
            || self
                .pipeline
                .add(effects_bin.upcast_ref::<gst::Element>())
                .is_err()
        {
            self.compositor.release_request_pad(&comp_pad);
            self.pipeline.remove(&decoder).ok();
            return None;
        }

        let slot_queue = gst::ElementFactory::make("queue")
            .property("max-size-buffers", 1u32)
            .property("max-size-bytes", 0u32)
            .property("max-size-time", 0u64)
            .build()
            .ok()?;
        if self.pipeline.add(&slot_queue).is_err() {
            self.compositor.release_request_pad(&comp_pad);
            self.pipeline
                .remove(effects_bin.upcast_ref::<gst::Element>())
                .ok();
            self.pipeline.remove(&decoder).ok();
            return None;
        }
        let q_sink = slot_queue.static_pad("sink")?;
        let q_src = slot_queue.static_pad("src")?;
        let effects_src = effects_bin.static_pad("src")?;
        if effects_src.link(&q_sink).is_err() {
            self.compositor.release_request_pad(&comp_pad);
            self.pipeline.remove(&slot_queue).ok();
            self.pipeline
                .remove(effects_bin.upcast_ref::<gst::Element>())
                .ok();
            self.pipeline.remove(&decoder).ok();
            return None;
        }
        let _ = q_src.link(&comp_pad);
        let comp_arrival_seq = Arc::new(AtomicU64::new(0));
        {
            let arrival_seq = comp_arrival_seq.clone();
            q_src.add_probe(gst::PadProbeType::BUFFER, move |_pad, _info| {
                arrival_seq.fetch_add(1, Ordering::Relaxed);
                gst::PadProbeReturn::Ok
            });
        }

        let mut audio_conv = gst::ElementFactory::make("audioconvert").build().ok();
        let mut audio_resample = None;
        let mut audio_capsfilter = None;
        let mut audio_panorama = gst::ElementFactory::make("audiopanorama").build().ok();
        let mut audio_level = gst::ElementFactory::make("level")
            .property("post-messages", true)
            .property("interval", 50_000_000u64)
            .build()
            .ok();
        let mut amix_pad: Option<gst::Pad> = None;
        let mut audio_sink: Option<gst::Pad> = None;
        if let Some(ac_elem) = audio_conv.clone() {
            if self.pipeline.add(&ac_elem).is_ok() {
                audio_sink = ac_elem.static_pad("sink");
                let mut link_src = if let Some((resample, capsfilter, normalized_src)) =
                    attach_preview_audio_normalizer(
                        &self.pipeline,
                        &ac_elem,
                        "build_prerender_video_slot",
                    ) {
                    audio_resample = Some(resample);
                    audio_capsfilter = Some(capsfilter);
                    Some(normalized_src)
                } else {
                    log::warn!(
                        "build_prerender_video_slot: failed to normalize preview audio for source={}, skipping audio path",
                        source_path
                    );
                    self.pipeline.remove(&ac_elem).ok();
                    audio_conv = None;
                    audio_panorama = None;
                    audio_level = None;
                    audio_sink = None;
                    None
                };
                if link_src.is_some() {
                    if let Some(ref pano) = audio_panorama {
                        if self.pipeline.add(pano).is_ok() {
                            pano.set_property("panorama", 0.0_f32);
                            if let (Some(prev_src), Some(pano_sink)) =
                                (link_src.clone(), pano.static_pad("sink"))
                            {
                                let _ = prev_src.link(&pano_sink);
                                link_src = pano.static_pad("src");
                            } else {
                                self.pipeline.remove(pano).ok();
                                audio_panorama = None;
                            }
                        } else {
                            audio_panorama = None;
                        }
                    }
                    if let Some(ref level) = audio_level {
                        if self.pipeline.add(level).is_ok() {
                            if let (Some(link_out), Some(level_sink)) =
                                (link_src.clone(), level.static_pad("sink"))
                            {
                                let _ = link_out.link(&level_sink);
                                link_src = level.static_pad("src");
                            }
                        } else {
                            audio_level = None;
                        }
                    }
                    if let Some(mp) = self.audiomixer.request_pad_simple("sink_%u") {
                        mp.set_property("volume", 1.0_f64);
                        if let Some(src) = link_src {
                            let _ = src.link(&mp);
                        }
                        amix_pad = Some(mp);
                    }
                }
            } else {
                audio_panorama = None;
                audio_level = None;
            }
        }

        let video_linked = Arc::new(AtomicBool::new(false));
        let audio_linked = Arc::new(AtomicBool::new(amix_pad.is_none()));
        let video_linked_for_cb = video_linked.clone();
        let audio_linked_for_cb = audio_linked.clone();
        let prerender_path_for_cb = source_path.to_string();
        let effects_sink_for_cb = effects_bin.static_pad("sink")?;
        let audio_sink_for_cb = audio_sink.clone();
        decoder.connect_pad_added(move |_dec, pad| {
            let caps = pad.current_caps().or_else(|| Some(pad.query_caps(None)));
            if let Some(caps) = caps {
                if let Some(s) = caps.structure(0) {
                    let name = s.name().to_string();
                    log::info!(
                        "prerender slot pad-added path={} caps={}",
                        prerender_path_for_cb,
                        name
                    );
                    if name.starts_with("video/") && !effects_sink_for_cb.is_linked() {
                        match pad.link(&effects_sink_for_cb) {
                            Ok(_) => {
                                log::info!(
                                    "prerender slot video linked path={}",
                                    prerender_path_for_cb
                                );
                                video_linked_for_cb.store(true, Ordering::Relaxed);
                            }
                            Err(e) => {
                                log::warn!(
                                    "prerender slot video link FAILED path={} err={:?}",
                                    prerender_path_for_cb,
                                    e
                                );
                            }
                        }
                    } else if name.starts_with("audio/") {
                        if let Some(ref sink) = audio_sink_for_cb {
                            if !sink.is_linked() && pad.link(sink).is_ok() {
                                log::info!(
                                    "prerender slot audio linked path={}",
                                    prerender_path_for_cb
                                );
                                audio_linked_for_cb.store(true, Ordering::Relaxed);
                            }
                        }
                    } else {
                        log::info!(
                            "prerender slot ignored pad path={} caps={}",
                            prerender_path_for_cb,
                            name
                        );
                    }
                } else {
                    log::warn!(
                        "prerender slot pad-added without structure path={}",
                        prerender_path_for_cb
                    );
                }
            } else {
                log::warn!(
                    "prerender slot pad-added without caps path={}",
                    prerender_path_for_cb
                );
            }
        });

        let _ = slot_queue.sync_state_with_parent();
        if let Some(ref ac) = audio_conv {
            let _ = ac.sync_state_with_parent();
        }
        if let Some(ref ar) = audio_resample {
            let _ = ar.sync_state_with_parent();
        }
        if let Some(ref cf) = audio_capsfilter {
            let _ = cf.sync_state_with_parent();
        }
        if let Some(ref ap) = audio_panorama {
            let _ = ap.sync_state_with_parent();
        }
        if let Some(ref lv) = audio_level {
            let _ = lv.sync_state_with_parent();
        }
        let _ = effects_bin.sync_state_with_parent();
        let _ = decoder.sync_state_with_parent();

        Some(VideoSlot {
            clip_idx: 0,
            decoder,
            video_linked,
            audio_linked,
            effects_bin,
            compositor_pad: Some(comp_pad),
            audio_mixer_pad: amix_pad,
            audio_conv,
            audio_resample,
            audio_capsfilter,
            audio_equalizer: None,
            audio_match_equalizer: None,
            audio_panorama,
            audio_level,
            videobalance: None,
            coloradj_rgb: None,
            colorbalance_3pt: None,
            gaussianblur: None,

            squareblur: None,
            videocrop: None,
            crop_alpha_state: Arc::new(Mutex::new((
                0i32, 0i32, 0i32, 0i32, 0i32, 0i32, 0i32, 0i32,
            ))),
            videobox_crop_alpha: None,
            imagefreeze: None,
            videoflip_rotate: None,
            videoflip_flip: None,
            textoverlay: None,
            alpha_filter: None,
            alpha_chroma_key: None,
            capsfilter_zoom: None,
            videobox_zoom: None,
            frei0r_user_effects: Vec::new(),
            slot_queue: Some(slot_queue),
            comp_arrival_seq,
            hidden: false,
            animated_svg_rendered: false,
            is_prerender_slot: true,
            prerender_segment_start_ns: Some(segment_start_ns),
            transition_enter_offset_ns: 0,
            is_blend_mode: false,
            blend_alpha: Arc::new(Mutex::new(1.0)),
            mask_data: Arc::new(Mutex::new(Vec::new())),
            hsl_data: Arc::new(Mutex::new(None)),
        })
    }

    fn render_prerender_segment_video_file(
        output_path: &str,
        inputs: &[(ProgramClip, String, u64, bool, bool)],
        adjustment_clips: &[ProgramClip],
        duration_ns: u64,
        out_w: u32,
        out_h: u32,
        fps: u32,
        transition_spec: Option<&TransitionPrerenderSpec>,
        transition_offset_ns: u64,
        prerender_preset: PrerenderEncodingPreset,
        prerender_crf: u32,
    ) -> bool {
        let Ok(ffmpeg) = crate::media::export::find_ffmpeg() else {
            return false;
        };
        let color_caps = crate::media::export::detect_color_filter_capabilities(&ffmpeg);
        let output_path_buf = PathBuf::from(output_path);
        if let Some(parent) = output_path_buf.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let partial_output_path = prerender_partial_output_path(&output_path_buf);
        let _ = std::fs::remove_file(&partial_output_path);
        let duration_s = duration_ns as f64 / 1_000_000_000.0;
        let prerender_crf = clamp_prerender_crf(prerender_crf);
        let mut cmd = Command::new(&ffmpeg);
        cmd.arg("-y")
            .arg("-hide_banner")
            .arg("-loglevel")
            .arg("error")
            .arg("-nostats");
        for (clip, path, source_ns, _, _) in inputs {
            if clip.is_title {
                // Title clips use a lavfi color source instead of a file.
                let lavfi =
                    Self::prerender_title_clip_lavfi_color(clip, out_w, out_h, fps, duration_s);
                cmd.arg("-f").arg("lavfi").arg("-i").arg(lavfi);
            } else {
                let source_s = *source_ns as f64 / 1_000_000_000.0;
                let clip_max_s = clip.source_duration_ns() as f64 / 1_000_000_000.0;
                let t = duration_s.min(clip_max_s).max(0.05);
                cmd.arg("-ss")
                    .arg(format!("{source_s:.6}"))
                    .arg("-t")
                    .arg(format!("{t:.6}"))
                    .arg("-i")
                    .arg(path);
            }
        }

        let use_transition_xfade = transition_spec
            .map(|spec| inputs.len() == 2 && spec.outgoing_input < 2 && spec.incoming_input < 2)
            .unwrap_or(false);
        let mut nodes: Vec<String> = Vec::new();
        let mut _mask_temp_files: Vec<tempfile::NamedTempFile> = Vec::new();
        for (i, (clip, _, _, _, source_is_proxy)) in inputs.iter().enumerate() {
            let clip_has_mask = clip.masks.iter().any(|mask| mask.enabled);
            let lut_filter = Self::prerender_build_lut_filter(clip, *source_is_proxy);
            let anamorphic_filter = Self::prerender_build_anamorphic_filter(clip);
            let color_filter = Self::prerender_build_color_filter(clip);
            let temp_tint_filter = Self::prerender_build_temperature_tint_filter(clip, &color_caps);
            let grading_filter = Self::prerender_build_grading_filter(clip);
            let denoise_filter = Self::prerender_build_denoise_filter(clip);
            let sharpen_filter = Self::prerender_build_sharpen_filter(clip);
            let blur_filter = Self::prerender_build_blur_filter(clip);
            let frei0r_filter = Self::prerender_build_frei0r_effects_filter(clip);
            let chroma_key_filter = Self::prerender_build_chroma_key_filter(clip);
            let title_filter = Self::prerender_build_title_filter(clip, out_h);
            let minterpolate_filter = Self::prerender_build_minterpolate_filter(clip, fps);
            let motion_blur_filter = Self::prerender_build_motion_blur_filter(clip, fps);
            let flip_filter = Self::prerender_build_flip_filter(clip);
            if use_transition_xfade {
                let crop_filter = Self::prerender_build_crop_filter(clip, out_w, out_h, false);
                let scale_position_filter =
                    Self::prerender_build_scale_position_filter(clip, out_w, out_h, false);
                let rotation_filter = Self::prerender_build_rotation_filter(clip, false);
                let transition_tpad_filter = Self::prerender_build_transition_tpad_filter(
                    transition_spec,
                    transition_offset_ns,
                    i,
                );
                let use_keyframed_overlay =
                    !clip_has_mask && Self::prerender_clip_has_keyframed_overlay(clip);
                if use_keyframed_overlay {
                    let pre_label = format!("pv{i}kf");
                    nodes.push(format!(
                        "[{i}:v]setpts=PTS-STARTPTS{lut_filter}{anamorphic_filter},format=yuva420p,scale={out_w}:{out_h}:force_original_aspect_ratio=decrease,setsar=1,pad={out_w}:{out_h}:(ow-iw)/2:(oh-ih)/2:color=black@0{crop_filter}{rotation_filter}{flip_filter}{transition_tpad_filter},fps={}{color_filter}{temp_tint_filter}{grading_filter}{denoise_filter}{sharpen_filter}{blur_filter}{frei0r_filter}{chroma_key_filter}{title_filter}[{pre_label}]",
                        fps.max(1),
                    ));
                    nodes.push(Self::prerender_build_keyframed_overlay_tail(
                        clip,
                        &pre_label,
                        &format!("pv{i}"),
                        out_w,
                        out_h,
                        fps.max(1),
                        1,
                        false,
                        true,
                        &format!("{minterpolate_filter}{motion_blur_filter}"),
                    ));
                } else if clip_has_mask {
                    let pre_label = format!("pv{i}pre");
                    let masked_label = format!("pv{i}masked");
                    nodes.push(format!(
                        "[{i}:v]setpts=PTS-STARTPTS{lut_filter}{anamorphic_filter},format=yuva420p,scale={out_w}:{out_h}:force_original_aspect_ratio=decrease,setsar=1,pad={out_w}:{out_h}:(ow-iw)/2:(oh-ih)/2:color=black@0{crop_filter}{scale_position_filter}{rotation_filter}{flip_filter}{transition_tpad_filter},fps={}{color_filter}{temp_tint_filter}{grading_filter}{denoise_filter}{sharpen_filter}{blur_filter}{frei0r_filter}{chroma_key_filter}{title_filter}{minterpolate_filter}{motion_blur_filter}[{pre_label}]",
                        fps.max(1),
                    ));
                    let _ = Self::prerender_append_mask_filter(
                        &mut nodes,
                        &pre_label,
                        &masked_label,
                        clip,
                        out_w,
                        out_h,
                        &mut _mask_temp_files,
                    );
                    nodes.push(format!("[{masked_label}]format=yuv420p[pv{i}]"));
                } else if clip.chroma_key_enabled {
                    nodes.push(format!(
                        "[{i}:v]setpts=PTS-STARTPTS{lut_filter}{anamorphic_filter},format=yuva420p,scale={out_w}:{out_h}:force_original_aspect_ratio=decrease,setsar=1,pad={out_w}:{out_h}:(ow-iw)/2:(oh-ih)/2:color=black@0{crop_filter}{scale_position_filter}{rotation_filter}{flip_filter}{transition_tpad_filter},fps={}{color_filter}{temp_tint_filter}{grading_filter}{denoise_filter}{sharpen_filter}{blur_filter}{frei0r_filter}{chroma_key_filter}{title_filter}{minterpolate_filter}{motion_blur_filter},format=yuv420p[pv{i}]",
                        fps.max(1),
                    ));
                } else {
                    // Apply LUT at source resolution (before downscale) so it
                    // processes the same pixel values as the export path.
                    nodes.push(format!(
                        "[{i}:v]setpts=PTS-STARTPTS{lut_filter}{anamorphic_filter},scale={out_w}:{out_h}:force_original_aspect_ratio=decrease,setsar=1,pad={out_w}:{out_h}:(ow-iw)/2:(oh-ih)/2{crop_filter}{scale_position_filter}{rotation_filter}{flip_filter}{transition_tpad_filter},fps={},format=yuv420p{color_filter}{temp_tint_filter}{grading_filter}{denoise_filter}{sharpen_filter}{blur_filter}{frei0r_filter}{title_filter}{minterpolate_filter}{motion_blur_filter}[pv{i}]",
                        fps.max(1),
                    ));
                }
            } else if i == 0 {
                let crop_filter = Self::prerender_build_crop_filter(clip, out_w, out_h, false);
                let scale_position_filter =
                    Self::prerender_build_scale_position_filter(clip, out_w, out_h, false);
                let rotation_filter = Self::prerender_build_rotation_filter(clip, false);
                let use_keyframed_overlay =
                    !clip_has_mask && Self::prerender_clip_has_keyframed_overlay(clip);
                if use_keyframed_overlay {
                    let pre_label = format!("pv{i}kf");
                    nodes.push(format!(
                        "[{i}:v]setpts=PTS-STARTPTS{lut_filter}{anamorphic_filter},format=yuva420p,scale={out_w}:{out_h}:force_original_aspect_ratio=decrease,setsar=1,pad={out_w}:{out_h}:(ow-iw)/2:(oh-ih)/2:color=black@0{crop_filter}{rotation_filter}{flip_filter},fps={}{color_filter}{temp_tint_filter}{grading_filter}{denoise_filter}{sharpen_filter}{blur_filter}{frei0r_filter}{chroma_key_filter}{title_filter}[{pre_label}]",
                        fps.max(1),
                    ));
                    nodes.push(Self::prerender_build_keyframed_overlay_tail(
                        clip,
                        &pre_label,
                        &format!("pv{i}"),
                        out_w,
                        out_h,
                        fps.max(1),
                        1,
                        false,
                        true,
                        &format!("{minterpolate_filter}{motion_blur_filter}"),
                    ));
                } else if clip_has_mask {
                    let pre_label = format!("pv{i}pre");
                    nodes.push(format!(
                        "[{i}:v]setpts=PTS-STARTPTS{lut_filter}{anamorphic_filter},format=yuva420p,scale={out_w}:{out_h}:force_original_aspect_ratio=decrease,setsar=1,pad={out_w}:{out_h}:(ow-iw)/2:(oh-ih)/2:color=black@0{crop_filter}{scale_position_filter}{rotation_filter}{flip_filter},fps={}{color_filter}{temp_tint_filter}{grading_filter}{denoise_filter}{sharpen_filter}{blur_filter}{frei0r_filter}{chroma_key_filter}{title_filter}{minterpolate_filter}{motion_blur_filter}[{pre_label}]",
                        fps.max(1),
                    ));
                    let _ = Self::prerender_append_mask_filter(
                        &mut nodes,
                        &pre_label,
                        &format!("pv{i}"),
                        clip,
                        out_w,
                        out_h,
                        &mut _mask_temp_files,
                    );
                } else if clip.chroma_key_enabled {
                    // Chroma key needs alpha: convert early so pad fills transparent.
                    // Apply LUT at source resolution before format/scale for parity.
                    nodes.push(format!(
                        "[{i}:v]setpts=PTS-STARTPTS{lut_filter}{anamorphic_filter},format=yuva420p,scale={out_w}:{out_h}:force_original_aspect_ratio=decrease,setsar=1,pad={out_w}:{out_h}:(ow-iw)/2:(oh-ih)/2:color=black@0{crop_filter}{scale_position_filter}{rotation_filter}{flip_filter},fps={}{color_filter}{temp_tint_filter}{grading_filter}{denoise_filter}{sharpen_filter}{blur_filter}{frei0r_filter}{chroma_key_filter}{title_filter}{minterpolate_filter}{motion_blur_filter}[pv{i}]",
                        fps.max(1),
                    ));
                } else {
                    // Apply LUT at source resolution before downscale for parity.
                    nodes.push(format!(
                        "[{i}:v]setpts=PTS-STARTPTS{lut_filter}{anamorphic_filter},scale={out_w}:{out_h}:force_original_aspect_ratio=decrease,setsar=1,pad={out_w}:{out_h}:(ow-iw)/2:(oh-ih)/2{crop_filter}{scale_position_filter}{rotation_filter}{flip_filter},fps={},format=yuv420p{color_filter}{temp_tint_filter}{grading_filter}{denoise_filter}{sharpen_filter}{blur_filter}{frei0r_filter}{title_filter}{minterpolate_filter}{motion_blur_filter}[pv{i}]",
                        fps.max(1),
                    ));
                }
            } else {
                let crop_filter = Self::prerender_build_crop_filter(clip, out_w, out_h, true);
                let scale_position_filter =
                    Self::prerender_build_scale_position_filter(clip, out_w, out_h, true);
                let rotation_filter = Self::prerender_build_rotation_filter(clip, true);
                let use_keyframed_overlay =
                    !clip_has_mask && Self::prerender_clip_has_keyframed_overlay(clip);
                if use_keyframed_overlay {
                    let pre_label = format!("pv{i}kf");
                    nodes.push(format!(
                        "[{i}:v]setpts=PTS-STARTPTS{lut_filter}{anamorphic_filter},format=yuva420p,scale={out_w}:{out_h}:force_original_aspect_ratio=decrease,setsar=1,pad={out_w}:{out_h}:(ow-iw)/2:(oh-ih)/2:color=black@0{crop_filter}{rotation_filter}{flip_filter}{color_filter}{temp_tint_filter}{grading_filter}{denoise_filter}{sharpen_filter}{blur_filter}{frei0r_filter}{chroma_key_filter}{title_filter}[{pre_label}]",
                    ));
                    nodes.push(Self::prerender_build_keyframed_overlay_tail(
                        clip,
                        &pre_label,
                        &format!("pv{i}"),
                        out_w,
                        out_h,
                        fps.max(1),
                        1,
                        true,  // overlay tracks: transparent bg
                        false, // overlay tracks: keep alpha (no format=yuv420p)
                        &format!("{minterpolate_filter}{motion_blur_filter}"),
                    ));
                } else if clip_has_mask {
                    let pre_label = format!("pv{i}pre");
                    let masked_label = format!("pv{i}masked");
                    nodes.push(format!(
                        "[{i}:v]setpts=PTS-STARTPTS{lut_filter}{anamorphic_filter},format=yuva420p,scale={out_w}:{out_h}:force_original_aspect_ratio=decrease,setsar=1,pad={out_w}:{out_h}:(ow-iw)/2:(oh-ih)/2:color=black@0{crop_filter}{scale_position_filter}{rotation_filter}{flip_filter}{color_filter}{temp_tint_filter}{grading_filter}{denoise_filter}{sharpen_filter}{blur_filter}{frei0r_filter}{chroma_key_filter}{title_filter}{minterpolate_filter}{motion_blur_filter}[{pre_label}]",
                    ));
                    let masked_input = if Self::prerender_append_mask_filter(
                        &mut nodes,
                        &pre_label,
                        &masked_label,
                        clip,
                        out_w,
                        out_h,
                        &mut _mask_temp_files,
                    ) {
                        masked_label
                    } else {
                        pre_label
                    };
                    nodes.push(format!(
                        "[{masked_input}]colorchannelmixer=aa={:.4}[pv{i}]",
                        clip.opacity.clamp(0.0, 1.0),
                    ));
                } else {
                    // Overlay tracks: convert to yuva420p early so pad fills transparent.
                    // Apply LUT at source resolution before format/scale for parity.
                    nodes.push(format!(
                        "[{i}:v]setpts=PTS-STARTPTS{lut_filter}{anamorphic_filter},format=yuva420p,scale={out_w}:{out_h}:force_original_aspect_ratio=decrease,setsar=1,pad={out_w}:{out_h}:(ow-iw)/2:(oh-ih)/2:color=black@0{crop_filter}{scale_position_filter}{rotation_filter}{flip_filter}{color_filter}{temp_tint_filter}{grading_filter}{denoise_filter}{sharpen_filter}{blur_filter}{frei0r_filter}{chroma_key_filter}{title_filter}{minterpolate_filter}{motion_blur_filter},colorchannelmixer=aa={:.4}[pv{i}]",
                        clip.opacity.clamp(0.0, 1.0),
                    ));
                }
            }
        }
        let mut last_label = "pv0".to_string();
        if let Some(spec) = transition_spec.filter(|_| use_transition_xfade) {
            let out_label = format!("pv{}", spec.outgoing_input);
            let in_label = format!("pv{}", spec.incoming_input);
            let offset_s = (transition_offset_ns as f64 / 1_000_000_000.0).max(0.0);
            let duration_s = (spec.duration_ns as f64 / 1_000_000_000.0)
                .max(0.001)
                .min((duration_s - offset_s).max(0.001));
            nodes.push(format!(
                "[{out_label}][{in_label}]xfade=transition={}:duration={duration_s:.6}:offset={offset_s:.6}[vcomp1]",
                spec.xfade_transition,
            ));
            last_label = "vcomp1".to_string();
        } else {
            for i in 1..inputs.len() {
                let next = format!("vcomp{i}");
                let clip = &inputs[i].0;
                if clip.blend_mode != crate::model::clip::BlendMode::Normal {
                    let mode = clip.blend_mode.ffmpeg_mode();
                    nodes.push(format!(
                        "[{last_label}][pv{i}]blend=all_mode={mode}[{next}]"
                    ));
                } else {
                    nodes.push(format!("[{last_label}][pv{i}]overlay=x=0:y=0[{next}]"));
                }
                last_label = next;
            }
        }
        // Apply adjustment layer effects to the composited output.
        for (adj_idx, adj_clip) in adjustment_clips.iter().enumerate() {
            let adj_color = Self::prerender_build_color_filter(adj_clip);
            let adj_temp_tint =
                Self::prerender_build_temperature_tint_filter(adj_clip, &color_caps);
            let adj_grading = Self::prerender_build_grading_filter(adj_clip);
            let adj_denoise = Self::prerender_build_denoise_filter(adj_clip);
            let adj_sharpen = Self::prerender_build_sharpen_filter(adj_clip);
            let adj_blur = if adj_clip.blur > f64::EPSILON {
                let radius = (adj_clip.blur * 10.0).clamp(0.0, 10.0);
                format!(",boxblur={radius:.0}:{radius:.0}")
            } else {
                String::new()
            };
            let adj_frei0r = Self::prerender_build_frei0r_effects_filter(adj_clip);

            let mut effects = String::new();
            effects.push_str(&adj_color);
            effects.push_str(&adj_temp_tint);
            effects.push_str(&adj_grading);
            effects.push_str(&adj_denoise);
            effects.push_str(&adj_sharpen);
            effects.push_str(&adj_blur);
            effects.push_str(&adj_frei0r);

            let effects = effects.trim_matches(',').to_string();
            if !effects.is_empty() {
                let next = format!("vadj{adj_idx}");
                nodes.push(format!("[{last_label}]{effects}[{next}]"));
                last_label = next;
            }
        }

        let mut audio_labels: Vec<String> = Vec::new();
        for (i, (clip, _, _, has_audio, _)) in inputs.iter().enumerate() {
            if !*has_audio {
                continue;
            }
            let mut chain = format!("[{i}:a]asetpts=PTS-STARTPTS,aresample=async=1:first_pts=0");
            chain.push_str(&Self::prerender_build_transition_adelay_filter(
                transition_spec,
                transition_offset_ns,
                i,
            ));
            let vol = clip.volume.clamp(0.0, MAX_PREVIEW_AUDIO_GAIN);
            if (vol - 1.0).abs() > 0.001 {
                chain.push_str(&format!(",volume={vol:.4}"));
            }
            chain.push_str(&format!("[pa{i}]"));
            nodes.push(chain);
            audio_labels.push(format!("[pa{i}]"));
        }
        if !audio_labels.is_empty() {
            if audio_labels.len() == 1 {
                nodes.push(format!(
                    "{}anull,atrim=duration={duration_s:.6},asetpts=PTS-STARTPTS[pa_mix]",
                    audio_labels[0]
                ));
            } else {
                nodes.push(format!(
                    "{}amix=inputs={}:normalize=0:dropout_transition=0,atrim=duration={duration_s:.6},asetpts=PTS-STARTPTS[pa_mix]",
                    audio_labels.join(""),
                    audio_labels.len()
                ));
            }
        }
        let filter = nodes.join(";");
        cmd.arg("-filter_complex")
            .arg(filter)
            .arg("-map")
            .arg(format!("[{last_label}]"));
        if audio_labels.is_empty() {
            cmd.arg("-an");
        } else {
            cmd.arg("-map")
                .arg("[pa_mix]")
                .arg("-c:a")
                .arg("aac")
                .arg("-b:a")
                .arg("192k");
        }
        cmd.arg("-t")
            .arg(format!("{duration_s:.6}"))
            .arg("-c:v")
            .arg("libx264")
            .arg("-preset")
            .arg(prerender_preset.as_str())
            .arg("-crf")
            .arg(prerender_crf.to_string())
            .arg("-pix_fmt")
            .arg("yuv420p")
            .arg("-f")
            .arg("mp4")
            .arg("-movflags")
            .arg("+faststart")
            .arg(&partial_output_path)
            .stdout(Stdio::null())
            .stderr(Stdio::piped());
        let output = match cmd.output() {
            Ok(output) => output,
            Err(err) => {
                log::warn!(
                    "background_prerender: failed to launch ffmpeg for {}: {}",
                    output_path,
                    err
                );
                let _ = std::fs::remove_file(&partial_output_path);
                return false;
            }
        };
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
            if stderr.is_empty() {
                log::warn!(
                    "background_prerender: ffmpeg render failed for {}",
                    output_path
                );
            } else {
                log::warn!(
                    "background_prerender: ffmpeg render failed for {}: {}",
                    output_path,
                    stderr
                );
            }
            let _ = std::fs::remove_file(&partial_output_path);
            return false;
        }
        let _ = std::fs::remove_file(&output_path_buf);
        if std::fs::rename(&partial_output_path, &output_path_buf).is_err() {
            let _ = std::fs::remove_file(&partial_output_path);
            let _ = std::fs::remove_file(&output_path_buf);
            return false;
        }
        true
    }

    /// Generate the lavfi color source string for a prerendered title clip.
    ///
    /// Transparent title backgrounds must keep alpha so overlay-track prerender
    /// windows composite against lower video tracks instead of flattening to
    /// opaque black.
    fn prerender_title_clip_lavfi_color(
        clip: &ProgramClip,
        out_w: u32,
        out_h: u32,
        fps: u32,
        duration_s: f64,
    ) -> String {
        let bg = clip.title_clip_bg_color;
        let (r, g, b, a) = crate::ui::colors::rgba_u32_to_u8(bg);
        if a > 0 {
            let color_str = format!("#{r:02x}{g:02x}{b:02x}");
            format!(
                "color=c={color_str}:size={out_w}x{out_h}:r={fps}:d={duration_s:.6},format=yuv420p,trim=duration={duration_s:.6},setpts=PTS-STARTPTS"
            )
        } else {
            format!(
                "color=c=black@0.0:size={out_w}x{out_h}:r={fps}:d={duration_s:.6},format=yuva420p,trim=duration={duration_s:.6},setpts=PTS-STARTPTS"
            )
        }
    }

    fn prerender_build_transition_tpad_filter(
        transition_spec: Option<&TransitionPrerenderSpec>,
        transition_offset_ns: u64,
        input_index: usize,
    ) -> String {
        let Some(spec) = transition_spec else {
            return String::new();
        };
        let mut filter = String::new();
        let offset_s = transition_offset_ns as f64 / 1_000_000_000.0;
        if input_index == spec.incoming_input && offset_s > 0.0 {
            // Keep incoming transition source parked on its first frame until the
            // overlap boundary, so pre-padding does not advance incoming content.
            filter.push_str(&format!(
                ",tpad=start_duration={offset_s:.6}:start_mode=clone"
            ));
        }
        if input_index == spec.outgoing_input && spec.after_cut_ns > 0 {
            // For after-cut overlap, hold the outgoing clip's final frame long
            // enough for xfade to cover the tail that extends past the cut.
            filter.push_str(&format!(
                ",tpad=stop_mode=clone:stop_duration={:.6}",
                spec.after_cut_ns as f64 / 1_000_000_000.0
            ));
        }
        filter
    }

    fn prerender_build_transition_adelay_filter(
        transition_spec: Option<&TransitionPrerenderSpec>,
        transition_offset_ns: u64,
        input_index: usize,
    ) -> String {
        let Some(spec) = transition_spec else {
            return String::new();
        };
        if input_index != spec.incoming_input {
            return String::new();
        }
        if transition_offset_ns == 0 {
            return String::new();
        }
        // Keep incoming transition audio silent until overlap boundary so
        // prerender pre-padding does not introduce early incoming-audio bleed.
        let delay_ms = ((transition_offset_ns + 999_999) / 1_000_000).max(1);
        format!(",adelay={delay_ms}:all=1")
    }

    fn prerender_build_color_filter(clip: &ProgramClip) -> String {
        let has_color_keyframes = !clip.brightness_keyframes.is_empty()
            || !clip.contrast_keyframes.is_empty()
            || !clip.saturation_keyframes.is_empty();
        let has_color = clip.brightness != 0.0 || clip.contrast != 1.0 || clip.saturation != 1.0;
        let has_exposure = clip.exposure.abs() > f64::EPSILON;
        if has_color_keyframes {
            // Brightness is bounded ±1.0 (a normalized value range, not a
            // transform property — kept as a literal here because it's not
            // shared with anything else).
            let brightness_expr = crate::media::export::build_keyframed_property_expression(
                &clip.brightness_keyframes,
                clip.brightness,
                -1.0,
                1.0,
                "t",
            );
            let contrast_expr = crate::media::export::build_keyframed_property_expression(
                &clip.contrast_keyframes,
                clip.contrast,
                0.0,
                2.0,
                "t",
            );
            let saturation_expr = crate::media::export::build_keyframed_property_expression(
                &clip.saturation_keyframes,
                clip.saturation,
                0.0,
                2.0,
                "t",
            );
            let brightness_expr = if has_exposure {
                let exposure_brightness_delta = clip.exposure.clamp(-1.0, 1.0) * 0.55;
                format!("({brightness_expr})+{exposure_brightness_delta:.6}")
            } else {
                brightness_expr
            };
            let contrast_expr = if has_exposure {
                let exposure_contrast_delta = clip.exposure.clamp(-1.0, 1.0) * 0.12;
                format!("({contrast_expr})+{exposure_contrast_delta:.6}")
            } else {
                contrast_expr
            };
            format!(
                ",eq=brightness='{brightness_expr}':contrast='{contrast_expr}':saturation='{saturation_expr}':eval=frame"
            )
        } else if has_color || has_exposure {
            // Use the same calibrated videobalance mapping as export so that
            // proxy-mode preview matches the final render.
            let preview_params = Self::compute_videobalance_params(
                clip.brightness,
                clip.contrast,
                clip.saturation,
                6500.0, // temperature handled by separate filter
                0.0,    // tint handled by separate filter
                0.0,    // shadows handled by grading filter
                0.0,    // midtones handled by grading filter
                0.0,    // highlights handled by grading filter
                clip.exposure,
                0.0, // black_point handled by grading filter
                0.0, // warmth/tint handled by grading filter
                0.0,
                0.0,
                0.0,
                0.0,
                0.0,
                true,
                true,
            );
            let contrast_t = clip.contrast.clamp(0.0, 2.0);
            let contrast_delta = contrast_t - 1.0;
            let contrast_brightness_bias =
                0.26 * contrast_delta - 0.08 * contrast_delta * contrast_delta;
            format!(
                ",eq=brightness={:.4}:contrast={:.4}:saturation={:.4}",
                (preview_params.brightness + contrast_brightness_bias).clamp(-1.0, 1.0),
                preview_params.contrast,
                preview_params.saturation
            )
        } else {
            String::new()
        }
    }

    fn prerender_build_temperature_tint_filter(
        clip: &ProgramClip,
        caps: &crate::media::export::ColorFilterCapabilities,
    ) -> String {
        let has_temp = (clip.temperature - 6500.0).abs() > 1.0;
        let has_tint = clip.tint.abs() > 0.001;
        let has_temp_keyframes = !clip.temperature_keyframes.is_empty();
        let has_tint_keyframes = !clip.tint_keyframes.is_empty();
        // Use frei0r coloradj_RGB when available — same calibrated path as export.
        if caps.use_coloradj_frei0r
            && (has_temp || has_tint)
            && !has_temp_keyframes
            && !has_tint_keyframes
        {
            let cp =
                crate::media::export::compute_export_coloradj_params(clip.temperature, clip.tint);
            return format!(
                ",frei0r=filter_name=coloradj_RGB:filter_params={:.6}|{:.6}|{:.6}|0.333",
                cp.r, cp.g, cp.b
            );
        }
        // Fallback when frei0r is unavailable.
        let mut f = String::new();
        if has_temp_keyframes {
            let temp_expr = crate::media::export::build_keyframed_property_expression(
                &clip.temperature_keyframes,
                clip.temperature,
                2000.0,
                10000.0,
                "t",
            );
            f.push_str(&format!(
                ",colortemperature=temperature='{temp_expr}':eval=frame"
            ));
        } else if has_temp {
            f.push_str(&format!(
                ",colortemperature=temperature={:.0}",
                clip.temperature.clamp(2000.0, 10000.0)
            ));
        }
        if has_tint_keyframes {
            let tint_expr = crate::media::export::build_keyframed_property_expression(
                &clip.tint_keyframes,
                clip.tint,
                -1.0,
                1.0,
                "t",
            );
            let gm_expr = format!("(-({tint_expr}))*0.5");
            let rm_expr = format!("({tint_expr})*0.25");
            let bm_expr = format!("({tint_expr})*0.25");
            f.push_str(&format!(
                ",colorbalance=rm='{rm_expr}':gm='{gm_expr}':bm='{bm_expr}':eval=frame"
            ));
        } else if has_tint {
            let t = clip.tint.clamp(-1.0, 1.0);
            let gm = -t * 0.5;
            let rm = t * 0.25;
            let bm = t * 0.25;
            f.push_str(&format!(",colorbalance=rm={rm:.4}:gm={gm:.4}:bm={bm:.4}"));
        }
        f
    }

    fn prerender_build_denoise_filter(clip: &ProgramClip) -> String {
        if clip.denoise > 0.0 {
            let d = clip.denoise.clamp(0.0, 1.0);
            format!(
                ",hqdn3d={:.4}:{:.4}:{:.4}:{:.4}",
                d * 4.0,
                d * 3.0,
                d * 6.0,
                d * 4.5
            )
        } else {
            String::new()
        }
    }

    fn prerender_build_sharpen_filter(clip: &ProgramClip) -> String {
        if clip.sharpness != 0.0 {
            let la = (clip.sharpness * 3.0).clamp(-2.0, 5.0);
            format!(",unsharp=lx=5:ly=5:la={la:.4}:cx=5:cy=5:ca={la:.4}")
        } else {
            String::new()
        }
    }

    fn prerender_build_blur_filter(clip: &ProgramClip) -> String {
        if clip.blur > f64::EPSILON {
            let radius = (clip.blur * 10.0).clamp(0.0, 10.0);
            format!(",boxblur={radius:.0}:{radius:.0}")
        } else {
            String::new()
        }
    }

    fn prerender_build_anamorphic_filter(clip: &ProgramClip) -> String {
        if (clip.anamorphic_desqueeze - 1.0).abs() > 0.001 {
            // Physically desqueeze the source pixels horizontally and reset SAR to 1.
            format!(",scale=iw*{}:ih,setsar=1", clip.anamorphic_desqueeze)
        } else {
            String::new()
        }
    }

    fn prerender_build_lut_filter(clip: &ProgramClip, source_is_proxy: bool) -> String {
        if source_is_proxy {
            return String::new();
        }
        let mut result = String::new();
        for path in &clip.lut_paths {
            if !path.is_empty() && Path::new(path).exists() {
                let escaped = path.replace('\\', "\\\\").replace(':', "\\:");
                result.push_str(&format!(",lut3d={escaped}"));
            }
        }
        result
    }

    fn prerender_build_chroma_key_filter(clip: &ProgramClip) -> String {
        if clip.chroma_key_enabled {
            let color = format!(
                "0x{:02X}{:02X}{:02X}",
                (clip.chroma_key_color >> 16) & 0xFF,
                (clip.chroma_key_color >> 8) & 0xFF,
                clip.chroma_key_color & 0xFF
            );
            let similarity = (clip.chroma_key_tolerance * 0.5).clamp(0.01, 0.5);
            let blend = (clip.chroma_key_softness * 0.5).clamp(0.0, 0.5);
            format!(",colorkey={color}:{similarity:.4}:{blend:.4}")
        } else {
            String::new()
        }
    }

    fn prerender_build_grading_filter(clip: &ProgramClip) -> String {
        let has_grading = clip.shadows != 0.0
            || clip.midtones != 0.0
            || clip.highlights != 0.0
            || clip.black_point != 0.0
            || clip.highlights_warmth != 0.0
            || clip.highlights_tint != 0.0
            || clip.midtones_warmth != 0.0
            || clip.midtones_tint != 0.0
            || clip.shadows_warmth != 0.0
            || clip.shadows_tint != 0.0;
        if has_grading {
            // Use the same parabola-matched lutrgb as export for proxy parity.
            let p = Self::compute_export_3point_params(
                clip.shadows,
                clip.midtones,
                clip.highlights,
                clip.black_point,
                clip.highlights_warmth,
                clip.highlights_tint,
                clip.midtones_warmth,
                clip.midtones_tint,
                clip.shadows_warmth,
                clip.shadows_tint,
            );
            let parabola = ThreePointParabola::from_params(&p);
            parabola.to_lutrgb_filter()
        } else {
            String::new()
        }
    }

    fn prerender_build_crop_filter(
        clip: &ProgramClip,
        out_w: u32,
        out_h: u32,
        transparent_pad: bool,
    ) -> String {
        let cl = clip.crop_left.max(0) as u32;
        let cr = clip.crop_right.max(0) as u32;
        let ct = clip.crop_top.max(0) as u32;
        let cb = clip.crop_bottom.max(0) as u32;
        if cl == 0 && cr == 0 && ct == 0 && cb == 0 {
            return String::new();
        }
        let cw = out_w.saturating_sub(cl + cr).max(1);
        let ch = out_h.saturating_sub(ct + cb).max(1);
        let pad_color = if transparent_pad {
            "black@0.0"
        } else {
            "black"
        };
        format!(",crop={cw}:{ch}:{cl}:{ct},pad={out_w}:{out_h}:{cl}:{ct}:{pad_color}")
    }

    fn prerender_build_rotation_filter(clip: &ProgramClip, transparent_pad: bool) -> String {
        // Keyframed rotation: emit per-frame ffmpeg expression mirroring
        // export's `build_rotation_filter` keyframed branch.
        if !clip.rotate_keyframes.is_empty() {
            let fill = if transparent_pad { "black@0" } else { "black" };
            let angle_expr = crate::media::export::build_keyframed_property_expression(
                &clip.rotate_keyframes,
                clip.rotate as f64,
                -180.0,
                180.0,
                "t",
            );
            return format!(",rotate='-({angle_expr})*PI/180':fillcolor={fill}");
        }
        if clip.rotate == 0 {
            return String::new();
        }
        let fill = if transparent_pad { "black@0" } else { "black" };
        format!(
            ",rotate={:.10}:fillcolor={fill}",
            -(clip.rotate as f64).to_radians()
        )
    }

    /// Returns `true` when this prerender clip has any keyframe lane that
    /// affects per-frame geometry (scale, position, or crop). Rotation is
    /// handled inline by `prerender_build_rotation_filter`, so it's NOT in
    /// this gate — it doesn't need the multi-stream overlay path.
    ///
    /// When this returns `true`, the prerender format string for this clip
    /// uses the keyframed multi-stream overlay chain instead of the existing
    /// static `prerender_build_scale_position_filter`.
    fn prerender_clip_has_keyframed_overlay(clip: &ProgramClip) -> bool {
        !clip.scale_keyframes.is_empty()
            || !clip.position_x_keyframes.is_empty()
            || !clip.position_y_keyframes.is_empty()
            || !clip.crop_left_keyframes.is_empty()
            || !clip.crop_right_keyframes.is_empty()
            || !clip.crop_top_keyframes.is_empty()
            || !clip.crop_bottom_keyframes.is_empty()
            || !clip.opacity_keyframes.is_empty()
    }

    /// Build the multi-step keyframed transform tail for a prerender clip
    /// chain. Mirrors the keyframed branch in `src/media/export.rs:543-605`:
    ///
    /// 1. `,scale=w='max(1,{out_w}*({scale_expr}))':h=...:eval=frame[fg]`
    /// 2. `color=c={bg}:size={out_w}x{out_h}:r={fps}:d={dur}[bg]`
    /// 3. `[bg][fg]overlay=x='...':y='...':eval=frame,geq=...alpha*({opacity_expr})[raw]`
    /// 4. `[raw]format=yuv420p[output_label]` (or yuva420p for transparent)
    ///
    /// Returns a multi-step filter graph fragment with internal chains
    /// separated by `;`, ending in `[{output_label}]`. The caller pushes
    /// this as one entry into `nodes` (which is later joined by `;`),
    /// AFTER pushing a separate node that produces the `[{pre_chain_label}]`
    /// label containing all the static effects.
    fn prerender_build_keyframed_overlay_tail(
        clip: &ProgramClip,
        pre_chain_label: &str,
        output_label: &str,
        out_w: u32,
        out_h: u32,
        fps_n: u32,
        fps_d: u32,
        transparent_bg: bool,
        final_format_yuv420p: bool,
        post_tail: &str,
    ) -> String {
        use crate::media::export::build_keyframed_property_expression;
        use crate::model::transform_bounds::{
            POSITION_MAX, POSITION_MIN, SCALE_MAX, SCALE_MIN,
        };
        let scale_expr = build_keyframed_property_expression(
            &clip.scale_keyframes,
            clip.scale,
            SCALE_MIN,
            SCALE_MAX,
            "t",
        );
        let pos_x_expr = build_keyframed_property_expression(
            &clip.position_x_keyframes,
            clip.position_x,
            POSITION_MIN,
            POSITION_MAX,
            "t",
        );
        let pos_y_expr = build_keyframed_property_expression(
            &clip.position_y_keyframes,
            clip.position_y,
            POSITION_MIN,
            POSITION_MAX,
            "t",
        );
        let opacity_expr = build_keyframed_property_expression(
            &clip.opacity_keyframes,
            clip.opacity,
            0.0,
            1.0,
            "T",
        );
        let clip_duration_s = clip.duration_ns() as f64 / 1_000_000_000.0;
        let bg_color = if transparent_bg { "black@0" } else { "black" };
        let (overlay_x_expr, overlay_y_expr) = if Self::clip_uses_direct_canvas_translation(clip) {
            (
                format!("(W*(1+({pos_x_expr}))-w)/2"),
                format!("(H*(1+({pos_y_expr}))-h)/2"),
            )
        } else {
            (
                format!("(W-w)*(1+({pos_x_expr}))/2"),
                format!("(H-h)*(1+({pos_y_expr}))/2"),
            )
        };
        // Build the tail filter chain that consumes [raw] and produces
        // [output_label]. ffmpeg syntax requires at least one filter
        // between labels, and a leading comma after a `]` is invalid, so
        // strip any leading comma from `post_tail` and fall back to a
        // no-op `null` filter when nothing else is being applied.
        let post_tail_clean = post_tail.strip_prefix(',').unwrap_or(post_tail);
        let raw_to_output_chain = if final_format_yuv420p {
            if post_tail_clean.is_empty() {
                "format=yuv420p".to_string()
            } else {
                format!("format=yuv420p,{post_tail_clean}")
            }
        } else if post_tail_clean.is_empty() {
            "null".to_string()
        } else {
            post_tail_clean.to_string()
        };
        let fg_label = format!("{output_label}fg");
        let bg_label = format!("{output_label}bg");
        let raw_label = format!("{output_label}raw");
        format!(
            "[{pre_chain_label}]scale=w='max(1,{out_w}*({scale_expr}))':h='max(1,{out_h}*({scale_expr}))':eval=frame[{fg_label}];color=c={bg_color}:size={out_w}x{out_h}:r={fps_n}/{fps_d}:d={clip_duration_s:.6}[{bg_label}];[{bg_label}][{fg_label}]overlay=x='{overlay_x_expr}':y='{overlay_y_expr}':eval=frame,geq=lum='lum(X,Y)':cb='cb(X,Y)':cr='cr(X,Y)':a='alpha(X,Y)*({opacity_expr})'[{raw_label}];[{raw_label}]{raw_to_output_chain}[{output_label}]"
        )
    }

    fn prerender_build_flip_filter(clip: &ProgramClip) -> String {
        match (clip.flip_h, clip.flip_v) {
            (false, false) => String::new(),
            (true, false) => ",hflip".to_string(),
            (false, true) => ",vflip".to_string(),
            (true, true) => ",hflip,vflip".to_string(),
        }
    }

    fn prerender_build_scale_position_filter(
        clip: &ProgramClip,
        out_w: u32,
        out_h: u32,
        transparent_pad: bool,
    ) -> String {
        let scale = clip.scale.clamp(
            crate::model::transform_bounds::SCALE_MIN,
            crate::model::transform_bounds::SCALE_MAX,
        );
        if (scale - 1.0).abs() < 0.001
            && clip.position_x.abs() < 0.001
            && clip.position_y.abs() < 0.001
        {
            return String::new();
        }
        let pw = out_w as f64;
        let ph = out_h as f64;
        let pos_x = clip.position_x;
        let pos_y = clip.position_y;
        let sw = (pw * scale).round() as u32;
        let sh = (ph * scale).round() as u32;

        if Self::clip_uses_direct_canvas_translation(clip) {
            let raw_x = Self::direct_canvas_origin(pw, sw as f64, pos_x) as i64;
            let raw_y = Self::direct_canvas_origin(ph, sh as f64, pos_y) as i64;
            return Self::prerender_build_scale_translate_filter(
                sw,
                sh,
                raw_x,
                raw_y,
                out_w,
                out_h,
                transparent_pad,
            );
        }

        if scale >= 1.0 {
            let total_x = pw * (scale - 1.0);
            let total_y = ph * (scale - 1.0);
            let cx = (total_x * (1.0 + pos_x) / 2.0).round() as i64;
            let cy = (total_y * (1.0 + pos_y) / 2.0).round() as i64;
            format!(",scale={sw}:{sh},crop={out_w}:{out_h}:{cx}:{cy}")
        } else {
            let total_x = pw * (1.0 - scale);
            let total_y = ph * (1.0 - scale);
            let raw_pad_x = (total_x * (1.0 + pos_x) / 2.0).round() as i64;
            let raw_pad_y = (total_y * (1.0 + pos_y) / 2.0).round() as i64;
            Self::prerender_build_scale_translate_filter(
                sw,
                sh,
                raw_pad_x,
                raw_pad_y,
                out_w,
                out_h,
                transparent_pad,
            )
        }
    }

    fn prerender_build_scale_translate_filter(
        sw: u32,
        sh: u32,
        raw_x: i64,
        raw_y: i64,
        out_w: u32,
        out_h: u32,
        transparent_pad: bool,
    ) -> String {
        let crop_left = if raw_x < 0 { (-raw_x) as u32 } else { 0 };
        let crop_top = if raw_y < 0 { (-raw_y) as u32 } else { 0 };
        let crop_right = if raw_x + sw as i64 > out_w as i64 {
            (raw_x + sw as i64 - out_w as i64) as u32
        } else {
            0
        };
        let crop_bottom = if raw_y + sh as i64 > out_h as i64 {
            (raw_y + sh as i64 - out_h as i64) as u32
        } else {
            0
        };
        let pad_x = raw_x.max(0) as u32;
        let pad_y = raw_y.max(0) as u32;
        let pad_color = if transparent_pad { "black@0" } else { "black" };
        let needs_crop = crop_left > 0 || crop_top > 0 || crop_right > 0 || crop_bottom > 0;
        if needs_crop {
            let vis_w = sw.saturating_sub(crop_left + crop_right).max(1);
            let vis_h = sh.saturating_sub(crop_top + crop_bottom).max(1);
            format!(
                ",scale={sw}:{sh},crop={vis_w}:{vis_h}:{crop_left}:{crop_top},pad={out_w}:{out_h}:{pad_x}:{pad_y}:{pad_color}"
            )
        } else {
            format!(",scale={sw}:{sh},pad={out_w}:{out_h}:{pad_x}:{pad_y}:{pad_color}")
        }
    }

    /// Build a chain of ffmpeg frei0r filters for user-applied effects.
    /// Mirrors the export-pipeline `build_frei0r_effects_filter()` logic
    /// but operates on `ProgramClip` instead of `Clip`.
    fn prerender_build_frei0r_effects_filter(clip: &ProgramClip) -> String {
        use crate::media::frei0r_registry::{Frei0rNativeType, Frei0rRegistry};

        if clip.frei0r_effects.is_empty() {
            return String::new();
        }
        let mut result = String::new();
        let registry = Frei0rRegistry::get_or_discover();
        for effect in &clip.frei0r_effects {
            if !effect.enabled {
                continue;
            }
            let plugin = registry.find_by_name(&effect.plugin_name);
            let params_str = if let Some(info) = plugin {
                if !info.native_params.is_empty() {
                    info.native_params
                        .iter()
                        .map(|np| match np.native_type {
                            Frei0rNativeType::Color => {
                                let r = np
                                    .gst_properties
                                    .first()
                                    .and_then(|k| effect.params.get(k))
                                    .copied()
                                    .unwrap_or(0.0);
                                let g = np
                                    .gst_properties
                                    .get(1)
                                    .and_then(|k| effect.params.get(k))
                                    .copied()
                                    .unwrap_or(0.0);
                                let b = np
                                    .gst_properties
                                    .get(2)
                                    .and_then(|k| effect.params.get(k))
                                    .copied()
                                    .unwrap_or(0.0);
                                format!("{r:.6}/{g:.6}/{b:.6}")
                            }
                            Frei0rNativeType::Position => {
                                let x = np
                                    .gst_properties
                                    .first()
                                    .and_then(|k| effect.params.get(k))
                                    .copied()
                                    .unwrap_or(0.0);
                                let y = np
                                    .gst_properties
                                    .get(1)
                                    .and_then(|k| effect.params.get(k))
                                    .copied()
                                    .unwrap_or(0.0);
                                format!("{x:.6}/{y:.6}")
                            }
                            Frei0rNativeType::NativeString => {
                                let prop =
                                    np.gst_properties.first().map(|s| s.as_str()).unwrap_or("");
                                effect.string_params.get(prop).cloned().unwrap_or_default()
                            }
                            _ => {
                                let prop =
                                    np.gst_properties.first().map(|s| s.as_str()).unwrap_or("");
                                if np.native_type == Frei0rNativeType::Bool {
                                    let val = effect.params.get(prop).copied().unwrap_or(0.0);
                                    if val > 0.5 {
                                        "y".to_string()
                                    } else {
                                        "n".to_string()
                                    }
                                } else {
                                    let val = effect.params.get(prop).copied().unwrap_or(0.0);
                                    format!("{val:.6}")
                                }
                            }
                        })
                        .collect::<Vec<_>>()
                        .join("|")
                } else {
                    info.params
                        .iter()
                        .map(|p| {
                            if p.param_type
                                == crate::media::frei0r_registry::Frei0rParamType::String
                            {
                                effect
                                    .string_params
                                    .get(&p.name)
                                    .cloned()
                                    .or_else(|| p.default_string.clone())
                                    .unwrap_or_default()
                            } else {
                                let val = effect
                                    .params
                                    .get(&p.name)
                                    .copied()
                                    .unwrap_or(p.default_value);
                                format!("{val:.6}")
                            }
                        })
                        .collect::<Vec<_>>()
                        .join("|")
                }
            } else {
                effect
                    .params
                    .values()
                    .map(|v| format!("{v:.6}"))
                    .collect::<Vec<_>>()
                    .join("|")
            };
            let ffmpeg_name = plugin
                .map(|p| p.ffmpeg_name.as_str())
                .unwrap_or(&effect.plugin_name);
            if params_str.is_empty() {
                result.push_str(&format!(",frei0r=filter_name={ffmpeg_name}"));
            } else {
                result.push_str(&format!(
                    ",frei0r=filter_name={ffmpeg_name}:filter_params={params_str}"
                ));
            }
        }
        result
    }

    /// Build an ffmpeg `drawtext` filter for a title clip's text overlay.
    /// Mirrors the export-pipeline `build_title_filter()` logic but operates
    /// on `ProgramClip` instead of `Clip`.
    fn prerender_build_title_filter(clip: &ProgramClip, out_h: u32) -> String {
        if clip.title_text.trim().is_empty() {
            return String::new();
        }
        use crate::model::clip::TitleAnimation;
        const REF_H: f64 = 1080.0;
        let text =
            crate::media::title_font::escape_drawtext_value(&clip.title_text).replace('\n', "\\n");
        let font_size = crate::media::title_font::parse_title_font(&clip.title_font).size_points();
        let font_option = crate::media::title_font::build_drawtext_font_option(&clip.title_font);
        let rel_x = clip.title_x.clamp(0.0, 1.0);
        let rel_y = clip.title_y.clamp(0.0, 1.0);
        let scale_factor = out_h as f64 / REF_H;
        let scaled_size = font_size * (4.0 / 3.0) * scale_factor;
        let (r, g, b, a) = crate::ui::colors::rgba_u32_to_u8(clip.title_color);
        let alpha = (a as f64 / 255.0).clamp(0.0, 1.0);

        let mut style = String::new();
        if clip.title_outline_width > 0.0 {
            let bw = (clip.title_outline_width * scale_factor).max(0.5);
            let (or_, og, ob, oa) = crate::ui::colors::rgba_u32_to_u8(clip.title_outline_color);
            let o_alpha = (oa as f64 / 255.0).clamp(0.0, 1.0);
            style.push_str(&format!(
                ":borderw={bw:.1}:bordercolor={or_:02x}{og:02x}{ob:02x}@{o_alpha:.4}"
            ));
        }
        if clip.title_shadow {
            let sx = (clip.title_shadow_offset_x * scale_factor).round() as i32;
            let sy = (clip.title_shadow_offset_y * scale_factor).round() as i32;
            let (sr, sg, sb, sa) = crate::ui::colors::rgba_u32_to_u8(clip.title_shadow_color);
            let s_alpha = (sa as f64 / 255.0).clamp(0.0, 1.0);
            style.push_str(&format!(
                ":shadowx={sx}:shadowy={sy}:shadowcolor={sr:02x}{sg:02x}{sb:02x}@{s_alpha:.4}"
            ));
        }
        if clip.title_bg_box {
            let pad = (clip.title_bg_box_padding * scale_factor).round() as i32;
            let (br, bgg, bb, ba) = crate::ui::colors::rgba_u32_to_u8(clip.title_bg_box_color);
            let b_alpha = (ba as f64 / 255.0).clamp(0.0, 1.0);
            style.push_str(&format!(
                ":box=1:boxcolor={br:02x}{bgg:02x}{bb:02x}@{b_alpha:.4}:boxborderw={pad}"
            ));
        }

        let dur_s = (clip.title_animation_duration_ns as f64 / 1_000_000_000.0).max(1e-6);
        let pos_x = format!("({rel_x:.6})*w-text_w/2");
        let pos_y = format!("({rel_y:.6})*h-text_h/2");
        let base_color = format!("{r:02x}{g:02x}{b:02x}@{alpha:.4}");

        let mut filter = String::new();
        match clip.title_animation {
            TitleAnimation::None | TitleAnimation::Pop => {
                filter.push_str(&format!(
                    ",drawtext={font_option}:text='{text}':fontsize={scaled_size:.2}:fontcolor={base_color}:x='{pos_x}':y='{pos_y}'{style}"
                ));
            }
            TitleAnimation::Fade => {
                let alpha_expr = format!("min(1,max(0,t/{dur_s:.4}))*{alpha:.4}");
                filter.push_str(&format!(
                    ",drawtext={font_option}:text='{text}':fontsize={scaled_size:.2}:fontcolor={r:02x}{g:02x}{b:02x}:x='{pos_x}':y='{pos_y}':alpha='{alpha_expr}'{style}"
                ));
            }
            TitleAnimation::Typewriter => {
                let char_count = clip.title_text.chars().count();
                if char_count == 0 {
                    filter.push_str(&format!(
                        ",drawtext={font_option}:text='{text}':fontsize={scaled_size:.2}:fontcolor={base_color}:x='{pos_x}':y='{pos_y}'{style}"
                    ));
                } else {
                    let step = dur_s / char_count as f64;
                    for i in 0..char_count {
                        let prefix: String = clip.title_text.chars().take(i + 1).collect();
                        let prefix_esc =
                            crate::media::title_font::escape_drawtext_value(&prefix)
                                .replace('\n', "\\n");
                        let t0 = i as f64 * step;
                        let enable = if i + 1 == char_count {
                            format!("gte(t\\,{t0:.4})")
                        } else {
                            let t1 = (i + 1) as f64 * step;
                            format!("between(t\\,{t0:.4}\\,{t1:.4})")
                        };
                        filter.push_str(&format!(
                            ",drawtext={font_option}:text='{prefix_esc}':fontsize={scaled_size:.2}:fontcolor={base_color}:x='{pos_x}':y='{pos_y}':enable='{enable}'{style}"
                        ));
                    }
                }
            }
        }
        filter
    }

    /// Background-prerender variant of `build_motion_blur_filter` (export.rs).
    /// Uses the same math: `tmix=frames=2` at 360°, otherwise oversample by
    /// 4× via `minterpolate`, average the appropriate sub-frame count, and
    /// decimate back to project rate. Returns empty when motion blur is
    /// disabled or the clip has no per-frame motion (animated transform or
    /// speed > 1).
    fn prerender_build_motion_blur_filter(clip: &ProgramClip, fps: u32) -> String {
        if !clip.motion_blur_enabled || clip.motion_blur_shutter_angle <= 0.5 {
            return String::new();
        }
        let has_animated_transform = !clip.scale_keyframes.is_empty()
            || !clip.position_x_keyframes.is_empty()
            || !clip.position_y_keyframes.is_empty()
            || !clip.rotate_keyframes.is_empty()
            || !clip.crop_left_keyframes.is_empty()
            || !clip.crop_right_keyframes.is_empty()
            || !clip.crop_top_keyframes.is_empty()
            || !clip.crop_bottom_keyframes.is_empty();
        let has_fast_speed = clip.speed > 1.001;
        if !has_animated_transform && !has_fast_speed {
            return String::new();
        }
        let shutter = clip.motion_blur_shutter_angle.clamp(0.0, 720.0);
        if (shutter - 360.0).abs() < 0.5 {
            return ",tmix=frames=2:weights='1 1'".to_string();
        }
        const K: u32 = 4;
        let raw_frames = (K as f64 * shutter / 360.0).round() as i32;
        let frames = raw_frames.max(1).min((K * 2) as i32) as u32;
        let weights = std::iter::repeat("1")
            .take(frames as usize)
            .collect::<Vec<_>>()
            .join(" ");
        let over_fps = fps.saturating_mul(K).max(1);
        format!(
            ",minterpolate=fps={over_fps}:mi_mode=blend,tmix=frames={frames}:weights='{weights}',fps={fps}"
        )
    }

    fn prerender_build_minterpolate_filter(clip: &ProgramClip, fps: u32) -> String {
        use crate::model::clip::SlowMotionInterp;
        if clip.slow_motion_interp == SlowMotionInterp::Off {
            return String::new();
        }
        // Only apply for slow-motion clips
        let is_slow = if !clip.speed_keyframes.is_empty() {
            clip.speed_keyframes.iter().any(|kf| kf.value < 1.0)
        } else {
            clip.speed < 1.0 - 0.001
        };
        if !is_slow {
            return String::new();
        }
        let mi_mode = match clip.slow_motion_interp {
            SlowMotionInterp::Blend => "blend",
            SlowMotionInterp::OpticalFlow => "mci",
            // AI mode is realized via a precomputed sidecar consumed at the
            // input level — do not also apply ffmpeg minterpolate here.
            SlowMotionInterp::Ai => return String::new(),
            SlowMotionInterp::Off => unreachable!(),
        };
        format!(",minterpolate=fps={fps}:mi_mode={mi_mode}")
    }

    fn build_slot_for_clip(
        &mut self,
        clip_idx: usize,
        zorder_offset: usize,
        tune_multiqueue: bool,
    ) -> Option<VideoSlot> {
        let clip = self.clips[clip_idx].clone();

        // Resolve proxy path.
        let (effective_path, using_proxy, proxy_key, animated_svg_rendered) =
            self.resolve_source_path_for_clip(&clip);
        let uri = format!("file://{}", effective_path);
        if self.proxy_enabled {
            log::info!(
                "ProgramPlayer: slot={} clip={} source={} resolved={} mode={} proxy_key={} resolved_exists={} uri={}",
                zorder_offset,
                clip.id,
                clip.source_path,
                effective_path,
                if using_proxy { "proxy" } else { "fallback-original" },
                proxy_key,
                Path::new(&effective_path).exists(),
                uri
            );
            if !using_proxy {
                if self.proxy_fallback_warned_keys.insert(proxy_key.clone()) {
                    log::warn!(
                        "ProgramPlayer: proxy enabled but no proxy path for clip={} key={} (falling back to original until proxy is ready)",
                        clip.id,
                        proxy_key
                    );
                } else {
                    log::debug!(
                        "ProgramPlayer: proxy still unavailable for clip={} key={} (continuing original fallback)",
                        clip.id,
                        proxy_key
                    );
                }
            } else {
                self.proxy_fallback_warned_keys.remove(&proxy_key);
            }
        } else {
            log::info!(
                "ProgramPlayer: slot={} clip={} source={} resolved={} mode=original uri={}",
                zorder_offset,
                clip.id,
                clip.source_path,
                effective_path,
                uri
            );
        }

        // Detect whether the source file has an audio stream.
        let clip_has_audio = if !clip.has_embedded_audio() {
            false
        } else {
            self.probe_has_audio_stream_cached(&effective_path)
        };

        let (proc_w, proc_h) = self.preview_processing_dimensions();

        // Apply first LUT in realtime preview; full stack applied in prerender/export.
        let realtime_lut = if !using_proxy {
            clip.lut_paths
                .first()
                .filter(|p| !p.is_empty())
                .and_then(|p| self.get_or_parse_lut(p))
        } else {
            None
        };

        // Build per-slot effects bin.
        let slot_mask_data: Arc<Mutex<Vec<crate::model::clip::ClipMask>>> =
            Arc::new(Mutex::new(Vec::new()));
        let slot_hsl_data: Arc<Mutex<Option<crate::model::clip::HslQualifier>>> =
            Arc::new(Mutex::new(None));
        let (
            effects_bin,
            videobalance,
            coloradj_rgb,
            colorbalance_3pt,
            gaussianblur,
            squareblur,
            videocrop,
            videobox_crop_alpha,
            imagefreeze,
            videoflip_rotate,
            videoflip_flip,
            textoverlay,
            alpha_filter,
            alpha_chroma_key,
            capsfilter_zoom,
            videobox_zoom,
            frei0r_user_effects,
            crop_alpha_state,
        ) = Self::build_effects_bin(
            &clip,
            animated_svg_rendered,
            proc_w,
            proc_h,
            self.project_width,
            self.project_height,
            realtime_lut,
            slot_mask_data.clone(),
            slot_hsl_data.clone(),
        );

        // Title clips use videotestsrc (solid color) instead of uridecodebin.
        if clip.is_title {
            let (r, g, b, a) = crate::ui::colors::rgba_u32_to_u8(clip.title_clip_bg_color);
            let fg: u32 = if a > 0 {
                // RRGGBBAA → AARRGGBB for GStreamer foreground-color
                ((a as u32) << 24) | ((r as u32) << 16) | ((g as u32) << 8) | b as u32
            } else {
                0 // fully transparent black
            };

            // Use continuous videotestsrc (no imagefreeze) — title clips don't
            // go through imagefreeze because videotestsrc already produces a
            // continuous stream with proper segment events.  Using imagefreeze
            // with an external source causes "data flow before segment event"
            // errors because the ghost pad doesn't propagate segments correctly
            // from elements outside the effects_bin.
            let src = match gst::ElementFactory::make("videotestsrc")
                .property_from_str("pattern", "solid-color")
                .property("foreground-color", fg)
                .property("is-live", false)
                .build()
            {
                Ok(s) => s,
                Err(e) => {
                    log::warn!(
                        "ProgramPlayer: failed to create videotestsrc for title clip: {}",
                        e
                    );
                    return None;
                }
            };

            let caps = gst::Caps::builder("video/x-raw")
                .field("format", "RGBA")
                .field("width", proc_w as i32)
                .field("height", proc_h as i32)
                .field("framerate", gst::Fraction::new(30, 1))
                .field("pixel-aspect-ratio", gst::Fraction::new(1, 1))
                .build();
            let capsfilter = gst::ElementFactory::make("capsfilter")
                .property("caps", &caps)
                .build()
                .unwrap();

            if self.pipeline.add(&src).is_err()
                || self.pipeline.add(&capsfilter).is_err()
                || self
                    .pipeline
                    .add(effects_bin.upcast_ref::<gst::Element>())
                    .is_err()
            {
                self.pipeline.remove(&src).ok();
                self.pipeline.remove(&capsfilter).ok();
                self.pipeline
                    .remove(effects_bin.upcast_ref::<gst::Element>())
                    .ok();
                return None;
            }

            let _ = src.link(&capsfilter);

            // Link capsfilter → effects_bin sink pad.
            if let Some(sink) = effects_bin.static_pad("sink") {
                let cf_src = capsfilter.static_pad("src").unwrap();
                let _ = cf_src.link(&sink);
            }

            // Request compositor sink pad.
            let comp_pad = match self.compositor.request_pad_simple("sink_%u") {
                Some(p) => p,
                None => {
                    self.pipeline.remove(&src).ok();
                    self.pipeline.remove(&capsfilter).ok();
                    self.pipeline
                        .remove(effects_bin.upcast_ref::<gst::Element>())
                        .ok();
                    return None;
                }
            };
            comp_pad.set_property("zorder", (zorder_offset + 1) as u32);
            comp_pad.set_property(
                "alpha",
                clip.opacity_at_timeline_ns(self.timeline_pos_ns)
                    .clamp(0.0, 1.0),
            );

            // Link effects_bin → [alpha] → queue → compositor.
            let slot_queue = gst::ElementFactory::make("queue")
                .property("max-size-buffers", 1u32)
                .property("max-size-bytes", 0u32)
                .property("max-size-time", 0u64)
                .build()
                .unwrap();
            self.pipeline.add(&slot_queue).ok();

            if let Some(ebs) = effects_bin.static_pad("src") {
                let q_sink = slot_queue.static_pad("sink").unwrap();
                let _ = ebs.link(&q_sink);
            }

            let q_src = slot_queue.static_pad("src").unwrap();
            let _ = q_src.link(&comp_pad);

            let comp_arrival_seq = Arc::new(AtomicU64::new(0));
            let is_blend_mode = clip.blend_mode != crate::model::clip::BlendMode::Normal;
            let blend_alpha = Arc::new(Mutex::new(
                clip.opacity_at_timeline_ns(self.timeline_pos_ns),
            ));

            let video_linked = Arc::new(AtomicBool::new(true));
            let audio_linked = Arc::new(AtomicBool::new(false));

            let slot_ref_for_transform = VideoSlot {
                clip_idx,
                decoder: src.clone(),
                video_linked: video_linked.clone(),
                audio_linked: audio_linked.clone(),
                effects_bin: effects_bin.clone(),
                compositor_pad: Some(comp_pad.clone()),
                audio_mixer_pad: None,
                audio_conv: None,
                audio_resample: None,
                audio_capsfilter: None,
                audio_equalizer: None,
                audio_match_equalizer: None,
                audio_panorama: None,
                audio_level: None,
                videobalance: videobalance.clone(),
                coloradj_rgb: coloradj_rgb.clone(),
                colorbalance_3pt: colorbalance_3pt.clone(),

                gaussianblur: gaussianblur.clone(),
                squareblur: squareblur.clone(),
                videocrop: videocrop.clone(),
                videobox_crop_alpha: videobox_crop_alpha.clone(),
                crop_alpha_state: crop_alpha_state.clone(),
                imagefreeze: imagefreeze.clone(),
                videoflip_rotate: videoflip_rotate.clone(),
                videoflip_flip: videoflip_flip.clone(),
                textoverlay: textoverlay.clone(),
                alpha_filter: alpha_filter.clone(),
                alpha_chroma_key: alpha_chroma_key.clone(),
                capsfilter_zoom: capsfilter_zoom.clone(),
                videobox_zoom: videobox_zoom.clone(),
                frei0r_user_effects: frei0r_user_effects.clone(),
                slot_queue: Some(slot_queue.clone()),
                comp_arrival_seq: comp_arrival_seq.clone(),
                hidden: false,
                animated_svg_rendered: false,
                is_prerender_slot: false,
                prerender_segment_start_ns: None,
                transition_enter_offset_ns: 0,
                is_blend_mode,
                blend_alpha: blend_alpha.clone(),
                mask_data: slot_mask_data.clone(),
                hsl_data: slot_hsl_data.clone(),
            };
            Self::apply_transform_to_slot(
                &slot_ref_for_transform,
                clip.crop_left,
                clip.crop_right,
                clip.crop_top,
                clip.crop_bottom,
                clip.rotate,
                clip.flip_h,
                clip.flip_v,
            );
            Self::apply_title_to_slot(
                &slot_ref_for_transform,
                &clip.title_text,
                &clip.title_font,
                clip.title_color,
                clip.title_x,
                clip.title_y,
                self.project_height,
                proc_w,
                proc_h,
            );
            Self::apply_title_style_to_slot(&slot_ref_for_transform, &clip);
            Self::apply_zoom_to_slot(
                &slot_ref_for_transform,
                &comp_pad,
                clip.scale_at_timeline_ns(self.timeline_pos_ns),
                clip.position_x_at_timeline_ns(self.timeline_pos_ns),
                clip.position_y_at_timeline_ns(self.timeline_pos_ns),
                Self::clip_uses_direct_canvas_translation(&clip),
                proc_w,
                proc_h,
            );

            // Sync elements to pipeline state.
            let _ = src.sync_state_with_parent();
            let _ = capsfilter.sync_state_with_parent();
            let _ = effects_bin.sync_state_with_parent();
            let _ = slot_queue.sync_state_with_parent();

            return Some(VideoSlot {
                clip_idx,
                decoder: src,
                video_linked,
                audio_linked,
                effects_bin,
                compositor_pad: Some(comp_pad),
                audio_mixer_pad: None,
                audio_conv: None,
                audio_resample: None,
                audio_capsfilter: None,
                audio_equalizer: None,
                audio_match_equalizer: None,
                audio_panorama: None,
                audio_level: None,
                videobalance,
                coloradj_rgb,
                colorbalance_3pt,
                gaussianblur,
                squareblur,
                videocrop,
                videobox_crop_alpha,
                imagefreeze,
                videoflip_rotate,
                videoflip_flip,
                textoverlay,
                alpha_filter,
                alpha_chroma_key,
                capsfilter_zoom,
                videobox_zoom,
                crop_alpha_state,
                frei0r_user_effects,
                slot_queue: Some(slot_queue),
                comp_arrival_seq,
                hidden: false,
                animated_svg_rendered: false,
                is_prerender_slot: false,
                prerender_segment_start_ns: None,
                transition_enter_offset_ns: 0,
                is_blend_mode,
                blend_alpha,
                mask_data: slot_mask_data.clone(),
                hsl_data: slot_hsl_data.clone(),
            });
        }

        // Adjustment layers have no visual output in the compositor — their effects
        // are applied post-compositor via the adjustment probe.  Skip slot creation.
        if clip.is_adjustment {
            log::debug!(
                "ProgramPlayer: skipping slot for adjustment layer clip={}",
                clip.id
            );
            return None;
        }

        // Create uridecodebin for this clip.
        let decoder = match gst::ElementFactory::make("uridecodebin")
            .property("uri", &uri)
            .build()
        {
            Ok(d) => d,
            Err(e) => {
                log::warn!(
                    "ProgramPlayer: failed to create uridecodebin for {}: {}",
                    uri,
                    e
                );
                return None;
            }
        };

        // Add to pipeline.
        if self.pipeline.add(&decoder).is_err() {
            log::warn!("ProgramPlayer: failed to add decoder to pipeline");
            return None;
        }
        if self
            .pipeline
            .add(effects_bin.upcast_ref::<gst::Element>())
            .is_err()
        {
            self.pipeline.remove(&decoder).ok();
            log::warn!("ProgramPlayer: failed to add effects_bin to pipeline");
            return None;
        }

        // Request compositor sink pad (zorder > 0; 0 is reserved for black bg).
        let comp_pad = match self.compositor.request_pad_simple("sink_%u") {
            Some(p) => p,
            None => {
                self.pipeline.remove(&decoder).ok();
                self.pipeline
                    .remove(effects_bin.upcast_ref::<gst::Element>())
                    .ok();
                return None;
            }
        };
        comp_pad.set_property("zorder", (zorder_offset + 1) as u32);
        comp_pad.set_property(
            "alpha",
            clip.opacity_at_timeline_ns(self.timeline_pos_ns)
                .clamp(0.0, 1.0),
        );

        // Link effects_bin → queue → compositor pad.
        let slot_queue = gst::ElementFactory::make("queue")
            .property("max-size-buffers", 1u32)
            .property("max-size-bytes", 0u32)
            .property("max-size-time", 0u64)
            .build()
            .unwrap();
        self.pipeline.add(&slot_queue).ok();
        if let Some(src) = effects_bin.static_pad("src") {
            let q_sink = slot_queue.static_pad("sink").unwrap();
            let _ = src.link(&q_sink);
        }
        let q_src = slot_queue.static_pad("src").unwrap();
        let _ = q_src.link(&comp_pad);

        // Diagnostic probe: log buffers arriving at the compositor from this decoder slot.
        let comp_arrival_seq = Arc::new(AtomicU64::new(0));
        {
            let cid = clip.id.clone();
            let ci = clip_idx;
            let arrival_seq = comp_arrival_seq.clone();
            q_src.add_probe(gst::PadProbeType::BUFFER, move |_pad, info| {
                if let Some(buffer) = info.buffer() {
                    log::debug!(
                        "slot[{}] queue→comp: pts={} clip={}",
                        ci,
                        buffer.pts().map(|p| p.nseconds()).unwrap_or(u64::MAX),
                        cid
                    );
                    arrival_seq.fetch_add(1, Ordering::Relaxed);
                }
                gst::PadProbeReturn::Ok
            });
        }

        // Blend-mode capture: hide from compositor and capture overlay buffer.
        let is_blend_mode = clip.blend_mode != crate::model::clip::BlendMode::Normal;
        let blend_alpha = Arc::new(Mutex::new(
            clip.opacity_at_timeline_ns(self.timeline_pos_ns)
                .clamp(0.0, 1.0),
        ));
        if is_blend_mode {
            comp_pad.set_property("alpha", 0.0_f64);
            let blend_ov = self.blend_overlays.clone();
            let blend_mode = clip.blend_mode;
            let ci = clip_idx;
            let alpha_ref = blend_alpha.clone();
            let zorder = (zorder_offset + 1) as u32;
            q_src.add_probe(gst::PadProbeType::BUFFER, move |_pad, info| {
                if let Some(buffer) = info.buffer() {
                    if let Ok(map) = buffer.map_readable() {
                        let data = map.as_slice().to_vec();
                        let opacity = alpha_ref.try_lock().map(|g| *g).unwrap_or(1.0);
                        if let Ok(mut overlays) = blend_ov.try_lock() {
                            overlays.insert(
                                ci,
                                BlendOverlay {
                                    data,
                                    width: 0, // not used — size matches compositor output
                                    height: 0,
                                    blend_mode,
                                    opacity,
                                    zorder,
                                },
                            );
                        }
                    }
                }
                gst::PadProbeReturn::Ok
            });
        }

        // Create audio path: audioconvert → audioresample → capsfilter(48 kHz stereo)
        // → [match-equalizer-nbands] → [equalizer-nbands] → [audiopanorama]
        // → [level] → audiomixer pad. (Voice enhance is applied via the
        // prerender cache that swaps `clip.source_path` for an
        // ffmpeg-processed file in `resolve_source_path_for_clip`, so the
        // realtime audio path needs no special handling.)
        let (
            audio_conv,
            audio_resample,
            audio_capsfilter,
            audio_match_equalizer_built,
            audio_equalizer,
            audio_panorama,
            audio_level,
            amix_pad,
        ) = if clip_has_audio {
            let mut ac = gst::ElementFactory::make("audioconvert").build().ok();
            let mut ar = None;
            let mut cf = None;
            let mut eq = gst::ElementFactory::make("equalizer-nbands")
                .property("num-bands", 3u32)
                .build()
                .ok();
            // 7-band match EQ — only built when the clip has a non-empty match_eq_bands.
            let mut match_eq = if !clip.match_eq_bands.is_empty() {
                gst::ElementFactory::make("equalizer-nbands")
                    .property("num-bands", clip.match_eq_bands.len() as u32)
                    .build()
                    .ok()
            } else {
                None
            };
            let mut ap = gst::ElementFactory::make("audiopanorama").build().ok();
            let mut lv = gst::ElementFactory::make("level")
                .property("post-messages", true)
                .property("interval", 50_000_000u64)
                .build()
                .ok();
            let pad = if let Some(ac_elem) = ac.clone() {
                if self.pipeline.add(&ac_elem).is_ok() {
                    let mut link_src = if let Some((resample, capsfilter, normalized_src)) =
                        attach_preview_audio_normalizer_with_channel_mode(
                            &self.pipeline,
                            &ac_elem,
                            clip.audio_channel_mode,
                            "build_slot_for_clip",
                        ) {
                        ar = Some(resample);
                        cf = Some(capsfilter);
                        Some(normalized_src)
                    } else {
                        log::warn!(
                            "build_slot_for_clip: failed to normalize preview audio for clip_idx={}, clip={}, skipping audio path",
                            clip_idx,
                            clip.id
                        );
                        self.pipeline.remove(&ac_elem).ok();
                        ac = None;
                        eq = None;
                        match_eq = None;
                        ap = None;
                        lv = None;
                        None
                    };
                    if link_src.is_some() {
                        // Insert match EQ (7-band, before user EQ) when present.
                        if let Some(ref m_eq) = match_eq {
                            if self.pipeline.add(m_eq).is_ok() {
                                for (i, band) in clip.match_eq_bands.iter().enumerate() {
                                    eq_set_band(m_eq, i as u32, band.freq, band.gain, band.q);
                                }
                                if let (Some(prev_src), Some(meq_sink)) =
                                    (link_src.clone(), m_eq.static_pad("sink"))
                                {
                                    if prev_src.link(&meq_sink).is_ok() {
                                        link_src = m_eq.static_pad("src");
                                        log::info!(
                                            "build_slot_for_clip: match-eq ({} bands) linked OK for clip_idx={}",
                                            clip.match_eq_bands.len(), clip_idx
                                        );
                                    } else {
                                        log::warn!("build_slot_for_clip: match-eq pad link FAILED");
                                        self.pipeline.remove(m_eq).ok();
                                        match_eq = None;
                                    }
                                } else {
                                    self.pipeline.remove(m_eq).ok();
                                    match_eq = None;
                                }
                            } else {
                                log::warn!(
                                    "build_slot_for_clip: failed to add match-eq to pipeline"
                                );
                                match_eq = None;
                            }
                        }
                        // Insert equalizer between audioconvert and audiopanorama.
                        if let Some(ref equalizer) = eq {
                            if self.pipeline.add(equalizer).is_ok() {
                                for i in 0..3u32 {
                                    let b = &clip.eq_bands[i as usize];
                                    eq_set_band(equalizer, i, b.freq, b.gain, b.q);
                                }
                                if let (Some(prev_src), Some(eq_sink)) =
                                    (link_src.clone(), equalizer.static_pad("sink"))
                                {
                                    let link_result = prev_src.link(&eq_sink);
                                    if link_result.is_ok() {
                                        link_src = equalizer.static_pad("src");
                                        log::info!("build_slot_for_clip: equalizer-nbands linked OK for clip_idx={}", clip_idx);
                                    } else {
                                        log::warn!(
                                            "build_slot_for_clip: equalizer pad link FAILED: {:?}",
                                            link_result
                                        );
                                        self.pipeline.remove(equalizer).ok();
                                        eq = None;
                                    }
                                } else {
                                    log::warn!(
                                        "build_slot_for_clip: equalizer static pads not available"
                                    );
                                    self.pipeline.remove(equalizer).ok();
                                    eq = None;
                                }
                            } else {
                                log::warn!(
                                    "build_slot_for_clip: failed to add equalizer to pipeline"
                                );
                                eq = None;
                            }
                        } else {
                            log::warn!(
                                "build_slot_for_clip: equalizer-nbands element not available"
                            );
                        }
                        if let Some(ref pano) = ap {
                            if self.pipeline.add(pano).is_ok() {
                                pano.set_property(
                                    "panorama",
                                    self.effective_main_clip_pan(clip_idx, self.timeline_pos_ns)
                                        as f32,
                                );
                                if let (Some(prev_src), Some(pano_sink)) =
                                    (link_src.clone(), pano.static_pad("sink"))
                                {
                                    let _ = prev_src.link(&pano_sink);
                                    link_src = pano.static_pad("src");
                                } else {
                                    self.pipeline.remove(pano).ok();
                                    ap = None;
                                }
                            } else {
                                ap = None;
                            }
                        }
                        if let Some(ref level) = lv {
                            if self.pipeline.add(level).is_ok() {
                                if let (Some(link_out), Some(level_sink)) =
                                    (link_src.clone(), level.static_pad("sink"))
                                {
                                    let _ = link_out.link(&level_sink);
                                    link_src = level.static_pad("src");
                                } else {
                                    self.pipeline.remove(level).ok();
                                    lv = None;
                                }
                            } else {
                                lv = None;
                            }
                        }
                        if let Some(mp) = self.audiomixer.request_pad_simple("sink_%u") {
                            mp.set_property(
                                "volume",
                                self.effective_main_clip_volume(clip_idx, self.timeline_pos_ns),
                            );
                            if let Some(src) = link_src {
                                let _ = src.link(&mp);
                            }
                            Some(mp)
                        } else {
                            if let Some(ref level) = lv {
                                self.pipeline.remove(level).ok();
                            }
                            lv = None;
                            None
                        }
                    } else {
                        None
                    }
                } else {
                    ar = None;
                    cf = None;
                    eq = None;
                    match_eq = None;
                    ap = None;
                    lv = None;
                    None
                }
            } else {
                ar = None;
                cf = None;
                eq = None;
                match_eq = None;
                ap = None;
                lv = None;
                None
            };
            (ac, ar, cf, match_eq, eq, ap, lv, pad)
        } else {
            log::info!(
                "ProgramPlayer: skipping audio path for clip {} (no audio)",
                clip.id
            );
            (
                None, None, None, None, None, None, None, None,
            )
        };

        // Connect uridecodebin pad-added for dynamic linking.
        let effects_sink = effects_bin.static_pad("sink");
        let audio_sink = audio_conv.as_ref().and_then(|ac| ac.static_pad("sink"));
        let video_linked = Arc::new(AtomicBool::new(false));
        let audio_linked = Arc::new(AtomicBool::new(false));
        let video_linked_for_cb = video_linked.clone();
        let audio_linked_for_cb = audio_linked.clone();
        let clip_id_for_cb = clip.id.clone();
        decoder.connect_pad_added(move |_dec, pad| {
            let caps = pad.current_caps().or_else(|| Some(pad.query_caps(None)));
            if let Some(caps) = caps {
                if let Some(s) = caps.structure(0) {
                    let name = s.name().to_string();
                    log::info!(
                        "ProgramPlayer: pad-added clip={} caps={}",
                        clip_id_for_cb,
                        name
                    );
                    if name.starts_with("video/") {
                        if let Some(ref sink) = effects_sink {
                            match pad.link(sink) {
                                Ok(_) => {
                                    video_linked_for_cb.store(true, Ordering::Relaxed);
                                    log::info!(
                                        "ProgramPlayer: video linked clip={}",
                                        clip_id_for_cb
                                    );
                                }
                                Err(e) => {
                                    log::warn!(
                                        "ProgramPlayer: video link FAILED clip={} err={:?}",
                                        clip_id_for_cb,
                                        e
                                    );
                                }
                            }
                        }
                    } else if name.starts_with("audio/") {
                        if let Some(ref sink) = audio_sink {
                            if pad.link(sink).is_ok() {
                                audio_linked_for_cb.store(true, Ordering::Relaxed);
                            }
                        }
                    }
                }
            }
        });

        decoder.connect("deep-element-added", false, move |args| {
            // deep-element-added: args[0]=uridecodebin, args[1]=parent_bin, args[2]=element
            let child = args[2].get::<gst::Element>().ok()?;
            let factory_name = child
                .factory()
                .map(|f| f.name().to_string())
                .unwrap_or_default();
            if factory_name.starts_with("avdec_h264") || factory_name.starts_with("avdec_vp8") {
                child.set_property("max-threads", 2i32);
            } else if factory_name.starts_with("avdec_h265")
                || factory_name.starts_with("avdec_vp9")
            {
                child.set_property("max-threads", 4i32);
            } else if tune_multiqueue && factory_name == "multiqueue" {
                child.set_property("max-size-time", 0u64);
                child.set_property("max-size-bytes", 10_485_760u32);
            }
            // On macOS, vtdec (VideoToolbox) produces IOSurface-backed NV12
            // buffers.  Any videoconvertscale element created inside the decode
            // chain must use a single converter thread to avoid worker threads
            // reading from a released IOSurface, which causes SIGSEGV in
            // unpack_NV12 via gst_parallelized_task_runner_run.
            #[cfg(target_os = "macos")]
            if factory_name == "videoconvertscale" && child.find_property("n-threads").is_some() {
                child.set_property("n-threads", 1u32);
            }
            None
        });

        // Sync all slot elements to the pipeline state.
        let _ = effects_bin.sync_state_with_parent();
        let _ = slot_queue.sync_state_with_parent();
        if let Some(ref ac) = audio_conv {
            let _ = ac.sync_state_with_parent();
        }
        if let Some(ref ar) = audio_resample {
            let _ = ar.sync_state_with_parent();
        }
        if let Some(ref cf) = audio_capsfilter {
            let _ = cf.sync_state_with_parent();
        }
        if let Some(ref eq_elem) = audio_equalizer {
            let _ = eq_elem.sync_state_with_parent();
        }
        if let Some(ref ap) = audio_panorama {
            let _ = ap.sync_state_with_parent();
        }
        if let Some(ref lv) = audio_level {
            let _ = lv.sync_state_with_parent();
        }
        let _ = decoder.sync_state_with_parent();

        // Apply per-clip transform.
        let slot_ref_for_transform = VideoSlot {
            clip_idx,
            decoder: decoder.clone(),
            video_linked: video_linked.clone(),
            audio_linked: audio_linked.clone(),
            effects_bin: effects_bin.clone(),
            compositor_pad: Some(comp_pad.clone()),
            audio_mixer_pad: amix_pad.clone(),
            audio_conv: audio_conv.clone(),
            audio_resample: audio_resample.clone(),
            audio_capsfilter: audio_capsfilter.clone(),
            audio_equalizer: audio_equalizer.clone(),
            audio_match_equalizer: audio_match_equalizer_built.clone(),
            audio_panorama: audio_panorama.clone(),
            audio_level: audio_level.clone(),
            videobalance: videobalance.clone(),
            coloradj_rgb: coloradj_rgb.clone(),

            colorbalance_3pt: colorbalance_3pt.clone(),
            gaussianblur: gaussianblur.clone(),
            squareblur: squareblur.clone(),
            videocrop: videocrop.clone(),
            videobox_crop_alpha: videobox_crop_alpha.clone(),
            imagefreeze: imagefreeze.clone(),
            crop_alpha_state: crop_alpha_state.clone(),
            videoflip_rotate: videoflip_rotate.clone(),
            videoflip_flip: videoflip_flip.clone(),
            textoverlay: textoverlay.clone(),
            alpha_filter: alpha_filter.clone(),
            alpha_chroma_key: alpha_chroma_key.clone(),
            capsfilter_zoom: capsfilter_zoom.clone(),
            videobox_zoom: videobox_zoom.clone(),
            frei0r_user_effects: frei0r_user_effects.clone(),
            slot_queue: Some(slot_queue.clone()),
            comp_arrival_seq: comp_arrival_seq.clone(),
            hidden: false,
            animated_svg_rendered,
            is_prerender_slot: false,
            prerender_segment_start_ns: None,
            transition_enter_offset_ns: 0,
            is_blend_mode,
            blend_alpha: blend_alpha.clone(),
            mask_data: slot_mask_data.clone(),
            hsl_data: slot_hsl_data.clone(),
        };
        Self::apply_transform_to_slot(
            &slot_ref_for_transform,
            clip.crop_left,
            clip.crop_right,
            clip.crop_top,
            clip.crop_bottom,
            clip.rotate,
            clip.flip_h,
            clip.flip_v,
        );
        let (pw, ph) = self.preview_processing_dimensions();
        Self::apply_title_to_slot(
            &slot_ref_for_transform,
            &clip.title_text,
            &clip.title_font,
            clip.title_color,
            clip.title_x,
            clip.title_y,
            self.project_height,
            pw,
            ph,
        );
        Self::apply_title_style_to_slot(&slot_ref_for_transform, &clip);
        Self::apply_zoom_to_slot(
            &slot_ref_for_transform,
            &comp_pad,
            clip.scale_at_timeline_ns(self.timeline_pos_ns),
            clip.position_x_at_timeline_ns(self.timeline_pos_ns),
            clip.position_y_at_timeline_ns(self.timeline_pos_ns),
            Self::clip_uses_direct_canvas_translation(&clip),
            proc_w,
            proc_h,
        );

        Some(VideoSlot {
            clip_idx,
            decoder,
            video_linked,
            audio_linked,
            effects_bin,
            compositor_pad: Some(comp_pad),
            audio_mixer_pad: amix_pad,
            audio_conv,
            audio_resample,
            audio_capsfilter,
            audio_equalizer,
            audio_match_equalizer: None,
            audio_panorama,
            audio_level,
            videobalance,
            coloradj_rgb,
            colorbalance_3pt,
            gaussianblur,
            squareblur,
            videocrop,
            videobox_crop_alpha,
            imagefreeze,
            videoflip_rotate,
            videoflip_flip,
            textoverlay,
            alpha_filter,
            alpha_chroma_key,
            capsfilter_zoom,
            crop_alpha_state,
            videobox_zoom,
            frei0r_user_effects,
            slot_queue: Some(slot_queue),
            comp_arrival_seq,
            hidden: false,
            animated_svg_rendered,
            is_prerender_slot: false,
            prerender_segment_start_ns: None,
            transition_enter_offset_ns: 0,
            is_blend_mode,
            blend_alpha,
            mask_data: slot_mask_data,
            hsl_data: slot_hsl_data,
        })
    }

    // ── Continuing decoders (fast-path boundary crossing) ────────────────

    // ── Calibrated videobalance mapping ──────────────────────────────────
    //
    // GStreamer `videobalance` operates in RGB/HSV with 4 knobs (brightness,
    // contrast, saturation, hue) while the FFmpeg export pipeline uses
    // dedicated per-domain filters (eq, colortemperature, colorbalance,
    // hqdn3d, unsharp).  These polynomial coefficients were derived from
    // empirical calibration (tools/calibrate_color.py) by sweeping each
    // slider across its range, generating FFmpeg reference frames, and using
    // L-BFGS-B optimisation to find the videobalance params that minimise
    // per-pixel RMSE against the FFmpeg output.

    /// Evaluate degree-4 polynomial: c₀ + c₁t + c₂t² + c₃t³ + c₄t⁴
    #[inline]
    fn poly4(t: f64, c: &[f64; 5]) -> f64 {
        c[0] + t * (c[1] + t * (c[2] + t * (c[3] + t * c[4])))
    }

    /// Compute calibrated videobalance parameters that best approximate the
    /// FFmpeg export filter chain.  Each slider contributes a *delta* from
    /// neutral (0, 1, 1, 0) based on fitted polynomials.  When multiple
    /// sliders are active, deltas are summed (linear superposition — an
    /// approximation that works well for moderate adjustments).
    ///
    /// When `has_coloradj` is true, temperature/tint are handled by the
    /// frei0r `coloradj_RGB` element and excluded from videobalance.
    /// When `has_3point` is true, shadows/midtones/highlights are handled
    /// by the frei0r `3-point-color-balance` element.
    pub(crate) fn compute_videobalance_params(
        brightness: f64,
        contrast: f64,
        saturation: f64,
        temperature: f64,
        tint: f64,
        shadows: f64,
        midtones: f64,
        highlights: f64,
        exposure: f64,
        black_point: f64,
        highlights_warmth: f64,
        highlights_tint: f64,
        midtones_warmth: f64,
        midtones_tint: f64,
        shadows_warmth: f64,
        shadows_tint: f64,
        has_coloradj: bool,
        has_3point: bool,
    ) -> VBParams {
        let mut b = 0.0_f64;
        let mut c = 1.0_f64;
        let mut s = 1.0_f64;
        let mut h = 0.0_f64;

        // ── Brightness (default 0.0) ────────────────────────────────────
        // Calibration: FFmpeg eq brightness (YUV additive) maps to ~1.15×
        // GStreamer videobalance brightness (RGB additive) with a slight
        // contrast compensation.
        {
            const B_B: [f64; 5] = [0.005156, 1.260891, 0.002010, -0.217226, -0.009772];
            const B_C: [f64; 5] = [1.025037, 0.023633, -0.205914, -0.132415, 0.120400];
            // B_S omitted: high residual (0.48), noisy at extremes
            let t = brightness;
            b += Self::poly4(t, &B_B) - B_B[0]; // delta from neutral (poly(0) = c[0])
            c += Self::poly4(t, &B_C) - B_C[0];
        }

        // ── Exposure (default 0.0, range −1..1) ─────────────────────────
        // Approximated as a brightness lift with contrast compensation.
        // Exposure is fundamentally multiplicative (gamma-like) but
        // videobalance only has additive brightness; this linear
        // approximation matches well for |exposure| ≤ 0.5 and degrades
        // gracefully at extremes.  FFmpeg export uses `eq=gamma=` for
        // a more accurate curve.
        {
            let e = exposure.clamp(-1.0, 1.0);
            b += e * 0.55;
            c += e * 0.12;
        }

        // ── Contrast (default 1.0) ──────────────────────────────────────
        // Calibrated contrast + saturation compensation (YUV↔RGB).
        // Brightness cross-effect removed: RMSE-optimal but perceptually
        // jarring (Δb ≈ +0.5 at contrast=0).
        {
            const C_C: [f64; 5] = [0.294574, 0.666291, 0.242201, -0.290659, 0.071964];
            const C_S: [f64; 5] = [2.423564, -3.689165, 3.643393, -1.624246, 0.272677];
            let t = contrast;
            let t0 = 1.0;
            c += Self::poly4(t, &C_C) - Self::poly4(t0, &C_C);
            s += Self::poly4(t, &C_S) - Self::poly4(t0, &C_S);
        }

        // ── Saturation (default 1.0) ────────────────────────────────────
        // Primary saturation polynomial only.  Brightness and contrast
        // cross-effects removed: optimizer found them RMSE-helpful but
        // users expect the saturation slider to change color, not luminance.
        {
            const S_S: [f64; 5] = [0.364002, 1.024258, -0.843067, 0.625649, -0.135496];
            let t = saturation;
            let t0 = 1.0;
            s += Self::poly4(t, &S_S) - Self::poly4(t0, &S_S);
        }

        // ── Temperature (default 6500K, normalised to [-1, 0.78]) ───────
        // When frei0r coloradj_RGB is available, temperature is handled
        // there via per-channel RGB gains.  Otherwise, fall back to a
        // hue-rotation approximation.
        if !has_coloradj {
            let t_norm = (temperature - 6500.0) / 6500.0;
            h += (t_norm * -0.15).clamp(-0.25, 0.25);
        }

        // ── Tint (default 0.0) ──────────────────────────────────────────
        // When frei0r coloradj_RGB is available, tint is handled there.
        // Otherwise, keep the perceptual hue shift.
        if !has_coloradj {
            h += (tint * 0.08).clamp(-0.15, 0.15);
        }

        // ── Shadows (default 0.0) ───────────────────────────────────────
        // When frei0r 3-point-color-balance is available, shadows/midtones/
        // highlights are handled there with proper per-luminance-range control.
        // Otherwise, fall back to polynomial approximation via videobalance.
        if !has_3point {
            // Calibration: 74-94% RMSE improvement over current coefficients.
            // FFmpeg colorbalance shadows apply per-luminance-range shifts that
            // are poorly approximated by global brightness alone; adding
            // contrast and saturation compensation dramatically improves match.
            const SH_B: [f64; 5] = [-0.004654, 0.217915, 0.503318, 0.095557, -0.208157];
            const SH_C: [f64; 5] = [0.967806, -0.569223, -0.773125, 0.208067, 0.503863];
            let t = shadows;
            b += Self::poly4(t, &SH_B) - SH_B[0];
            c += Self::poly4(t, &SH_C) - SH_C[0];
        }

        // ── Midtones (default 0.0) ──────────────────────────────────────
        if !has_3point {
            // Calibration: moderate improvement; brightness weight close to
            // original (0.13 vs 0.20) but contrast compensation is significant.
            const M_B: [f64; 5] = [-0.009292, 0.130927, -0.050035, -0.039406, 0.044492];
            const M_C: [f64; 5] = [1.049000, -0.302024, 0.407251, 0.214717, -0.319686];
            let t = midtones;
            b += Self::poly4(t, &M_B) - M_B[0];
            c += Self::poly4(t, &M_C) - M_C[0];
        }

        // ── Highlights (default 0.0) ────────────────────────────────────
        if !has_3point {
            // Calibration: 78-88% RMSE improvement.  Current coefficients
            // (b×0.15, c×0.15) dramatically undershoot; optimal mapping uses
            // ~3× stronger brightness and aggressive contrast boost.
            const H_B: [f64; 5] = [-0.002545, 0.500927, -0.437295, -0.060255, 0.152945];
            const H_C: [f64; 5] = [0.918940, 1.420073, 1.848961, -0.254895, -0.846390];
            const H_S: [f64; 5] = [1.031708, -1.145010, 0.783173, 0.699344, -0.956279];
            let t = highlights;
            b += Self::poly4(t, &H_B) - H_B[0];
            c += Self::poly4(t, &H_C) - H_C[0];
            s += Self::poly4(t, &H_S) - H_S[0];
        }

        if !has_3point {
            // Fallback approximation when frei0r 3-point-color-balance is
            // unavailable.  videobalance cannot target luminance zones, so we
            // map these controls to a small global lift/hue response.
            let bp = black_point.clamp(-1.0, 1.0);
            b += bp * 0.18;
            c += bp * 0.10;

            let warmth_avg =
                (highlights_warmth + midtones_warmth + shadows_warmth).clamp(-3.0, 3.0) / 3.0;
            let tint_avg = (highlights_tint + midtones_tint + shadows_tint).clamp(-3.0, 3.0) / 3.0;
            h += (tint_avg * 0.08 - warmth_avg * 0.06).clamp(-0.20, 0.20);
        }

        VBParams {
            brightness: b.clamp(-1.0, 1.0),
            contrast: c.clamp(0.0, 2.0),
            saturation: s.clamp(0.0, 2.0),
            hue: h.clamp(-1.0, 1.0),
        }
    }

    /// Compute frei0r `coloradj_RGB` parameters for temperature and tint.
    ///
    /// Uses the Tanner Helland algorithm (same as FFmpeg's `colortemperature`
    /// filter) to convert a Kelvin value into per-channel RGB gains, then
    /// maps those gains into frei0r's [0,1] parameter space where 0.5 is
    /// neutral (gain 1.0).
    ///
    /// Tint is applied as a green-channel shift with complementary R/B
    /// boost, matching FFmpeg's `colorbalance` midtones approach.
    pub(crate) fn compute_coloradj_params(temperature: f64, tint: f64) -> ColorAdjRGBParams {
        // Calibrated polynomial mapping: frei0r coloradj_RGB params that best
        // approximate the FFmpeg colortemperature + colorbalance tint export.
        // Coefficients derived from empirical calibration (tools/calibrate_frei0r.py).
        //
        // Temperature is normalised: t = (K − 6500) / 4500.
        // Tint is passed directly (range −1..1).
        // Each polynomial gives the absolute param value; tint contributes a
        // delta from its neutral (poly(0) = coeffs[0]).

        // ── Temperature (normalised Kelvin) ─────────────────────────────
        const TEMP_R: [f64; 5] = [0.484425, -0.096376, -0.113860, 0.040888, 0.074672];
        const TEMP_G: [f64; 5] = [0.477346, 0.003212, -0.205499, 0.074971, 0.075272];
        const TEMP_B: [f64; 5] = [0.481896, 0.123688, -0.212512, 0.107891, -0.001538];

        // ── Tint ────────────────────────────────────────────────────────
        const TINT_R: [f64; 5] = [0.501907, 0.059414, 0.031477, 0.020843, -0.014643];
        const TINT_G: [f64; 5] = [0.493519, -0.073231, 0.227645, -0.178444, -0.126107];
        const TINT_B: [f64; 5] = [0.505304, 0.055597, -0.003110, 0.025954, -0.016120];

        let t_temp = ((temperature.clamp(1000.0, 15000.0) - 6500.0) / 4500.0).clamp(-1.0, 1.0);
        let t_tint = tint.clamp(-1.0, 1.0);

        let mut r = Self::poly4(t_temp, &TEMP_R);
        let mut g = Self::poly4(t_temp, &TEMP_G);
        let mut b = Self::poly4(t_temp, &TEMP_B);

        // Tint: add delta from neutral.
        r += Self::poly4(t_tint, &TINT_R) - TINT_R[0];
        g += Self::poly4(t_tint, &TINT_G) - TINT_G[0];
        b += Self::poly4(t_tint, &TINT_B) - TINT_B[0];

        ColorAdjRGBParams {
            r: r.clamp(0.0, 1.0),
            g: g.clamp(0.0, 1.0),
            b: b.clamp(0.0, 1.0),
        }
    }

    /// Non-linear response used by tonal warmth/tint controls.
    ///
    /// Keeps precision near 0.0 while boosting creative range near slider ends.
    pub(crate) fn compute_tonal_axis_response(value: f64) -> f64 {
        let v = value.clamp(-1.0, 1.0);
        v * (1.0 + 0.35 * v * v)
    }

    /// Compute frei0r `3-point-color-balance` parameters from
    /// shadows/midtones/highlights sliders.
    ///
    /// The 3-point element fits a parabola through three control points
    /// (black, 0), (gray, 0.5), (white, 1.0) per channel and applies it
    /// as the transfer curve.  Coefficients derived from empirical
    /// calibration against FFmpeg's `colorbalance` export filter
    /// (tools/calibrate_frei0r.py).
    ///
    /// Each slider contributes a delta from neutral via fitted polynomials.
    pub(crate) fn compute_3point_params(
        shadows: f64,
        midtones: f64,
        highlights: f64,
        black_point: f64,
        highlights_warmth: f64,
        highlights_tint: f64,
        midtones_warmth: f64,
        midtones_tint: f64,
        shadows_warmth: f64,
        shadows_tint: f64,
    ) -> ThreePointParams {
        // ── Shadows ─────────────────────────────────────────────────────
        const SH_BLACK: [f64; 5] = [0.065817, 0.157626, 0.002710, -0.194674, -0.055835];
        const SH_GRAY: [f64; 5] = [0.513596, 0.081446, -0.041960, 0.017848, 0.117598];
        const SH_WHITE: [f64; 5] = [0.999118, -0.037826, -0.087347, -0.046390, -0.004455];

        // ── Midtones ────────────────────────────────────────────────────
        const M_BLACK: [f64; 5] = [0.006185, -0.024554, -0.070816, 0.093034, 0.159694];
        const M_GRAY: [f64; 5] = [0.499145, -0.111600, -0.219452, 0.010107, 0.285960];
        const M_WHITE: [f64; 5] = [0.988180, -0.000025, 0.125044, 0.017842, -0.230026];

        // ── Highlights ──────────────────────────────────────────────────
        const H_BLACK: [f64; 5] = [0.039231, 0.056431, -0.027366, 0.018788, 0.094133];
        const H_GRAY: [f64; 5] = [0.519445, -0.494215, 0.275317, 0.483889, -0.277072];
        const H_WHITE: [f64; 5] = [0.957496, -0.356602, -0.543984, 0.462982, 0.266763];

        let sh = shadows.clamp(-1.0, 1.0);
        let mid = midtones.clamp(-1.0, 1.0);
        let hi = highlights.clamp(-1.0, 1.0);

        // Start from neutral, accumulate deltas from each slider.
        let mut black = 0.0_f64;
        let mut gray = 0.5_f64;
        let mut white = 1.0_f64;

        black += Self::poly4(sh, &SH_BLACK) - SH_BLACK[0];
        gray += Self::poly4(sh, &SH_GRAY) - SH_GRAY[0];
        white += Self::poly4(sh, &SH_WHITE) - SH_WHITE[0];

        black += Self::poly4(mid, &M_BLACK) - M_BLACK[0];
        gray += Self::poly4(mid, &M_GRAY) - M_GRAY[0];
        white += Self::poly4(mid, &M_WHITE) - M_WHITE[0];

        black += Self::poly4(hi, &H_BLACK) - H_BLACK[0];
        gray += Self::poly4(hi, &H_GRAY) - H_GRAY[0];
        white += Self::poly4(hi, &H_WHITE) - H_WHITE[0];

        // Clamp to valid frei0r ranges and ensure ordering.
        let black = black.clamp(0.0, 0.95);
        let white = white.clamp(0.05, 1.0);
        let gray = gray.clamp(black + 0.01, white - 0.01);

        // ── Black point (default 0.0, range −1..1) ─────────────────────
        // Positive lifts blacks (raises floor), negative crushes them.
        // Applied as uniform shift to the black reference point.
        let bp = black_point.clamp(-1.0, 1.0) * 0.15;

        // ── Per-tone warmth/tint → per-channel RGB offsets ──────────────
        // Warmth: positive = warm (red boost, blue cut), negative = cool.
        // Tint: positive = magenta (green cut, R+B boost), negative = green.
        // Scale factor chosen for visible creative shifts at slider extremes
        // while keeping the 3-point curve within usable bounds.
        // At ±1.0 slider: response=1.35, shift=±0.47 warmth / ±0.38 tint.
        let warmth_scale = 0.35;
        let tint_scale = 0.28;
        let shadows_endpoint_boost = 1.30;

        // Warmth positive = warm (red boost, blue cut in 3-point curve space).
        // Tint positive = magenta (green cut); negated because 3-point
        // adds tint directly to green channel.
        let sw = Self::compute_tonal_axis_response(shadows_warmth)
            * warmth_scale
            * shadows_endpoint_boost;
        let st =
            -Self::compute_tonal_axis_response(shadows_tint) * tint_scale * shadows_endpoint_boost;
        let mw = Self::compute_tonal_axis_response(midtones_warmth) * warmth_scale;
        let mt = -Self::compute_tonal_axis_response(midtones_tint) * tint_scale;
        let hw = Self::compute_tonal_axis_response(highlights_warmth) * warmth_scale;
        let ht = -Self::compute_tonal_axis_response(highlights_tint) * tint_scale;

        // Per-channel: warmth shifts R↔B, tint shifts G↔(R+B).
        // In frei0r 3-point space a LOWER control point value means
        // BRIGHTER channel output (the curve shifts left), so we
        // subtract warmth from red and add it to blue for a warm look.
        let black_r = (black + bp - sw + st * 0.5).clamp(0.0, 0.95);
        let black_g = (black + bp - st).clamp(0.0, 0.95);
        let black_b = (black + bp + sw + st * 0.5).clamp(0.0, 0.95);

        let gray_r = (gray - mw + mt * 0.5).clamp(0.01, 0.99);
        let gray_g = (gray - mt).clamp(0.01, 0.99);
        let gray_b = (gray + mw + mt * 0.5).clamp(0.01, 0.99);

        let white_r = (white - hw + ht * 0.5).clamp(0.05, 1.0);
        let white_g = (white - ht).clamp(0.05, 1.0);
        let white_b = (white + hw + ht * 0.5).clamp(0.05, 1.0);

        ThreePointParams {
            black_r,
            black_g,
            black_b,
            gray_r,
            gray_g,
            gray_b,
            white_r,
            white_g,
            white_b,
        }
    }

    /// Compute export-focused 3-point parameters with small luma harmonization
    /// terms tuned to reduce known preview/export endpoint drift.
    pub(crate) fn compute_export_3point_params(
        shadows: f64,
        midtones: f64,
        highlights: f64,
        black_point: f64,
        highlights_warmth: f64,
        highlights_tint: f64,
        midtones_warmth: f64,
        midtones_tint: f64,
        shadows_warmth: f64,
        shadows_tint: f64,
    ) -> ThreePointParams {
        let (shadows, midtones, highlights) =
            Self::export_tonal_parity_inputs(shadows, midtones, highlights);
        let mut p = Self::compute_3point_params(
            shadows,
            midtones,
            highlights,
            black_point,
            highlights_warmth,
            highlights_tint,
            midtones_warmth,
            midtones_tint,
            shadows_warmth,
            shadows_tint,
        );
        p
    }

    /// Per-zone additive corrections for the export frei0r 3-point-color-balance
    /// path.  Reserved for future per-zone compensation once a reliable model
    /// is found.  Previous polynomial offsets regressed midtones parity
    /// (3-point curve reshaping has undesirable cross-zone side effects),
    /// so the body is intentionally empty.
    pub(crate) fn apply_export_3point_parity_offsets(
        _p: &mut ThreePointParams,
        _shadows: f64,
        _midtones: f64,
        _highlights: f64,
    ) {
        // Intentionally empty — tonal 3-point offsets reverted after
        // MCP cross-runtime validation showed midtones regression.
    }

    /// Cool-side export temperature gain (env-overridable for parity fitting).
    /// Uses a piecewise curve in cool range with unity at/above 6500K.
    pub(crate) fn export_temperature_parity_gain(temperature: f64) -> f64 {
        let legacy_gain = Self::export_gain_env("US_EXPORT_COOL_TEMP_GAIN", 1.0);
        let far_gain = Self::export_gain_env("US_EXPORT_COOL_TEMP_GAIN_FAR", legacy_gain);
        let near_gain = Self::export_gain_env("US_EXPORT_COOL_TEMP_GAIN_NEAR", legacy_gain);
        Self::piecewise_cool_temperature_gain(temperature, far_gain, near_gain)
    }

    pub(crate) fn piecewise_cool_temperature_gain(
        temperature: f64,
        far_gain: f64,
        near_gain: f64,
    ) -> f64 {
        const FAR_K: f64 = 2000.0;
        const NEAR_K: f64 = 5000.0;
        const NEUTRAL_K: f64 = 6500.0;

        if temperature >= NEUTRAL_K {
            return 1.0;
        }
        if temperature <= FAR_K {
            return far_gain;
        }
        if temperature <= NEAR_K {
            let t = (temperature - FAR_K) / (NEAR_K - FAR_K);
            return far_gain + (near_gain - far_gain) * t;
        }
        let t = (temperature - NEAR_K) / (NEUTRAL_K - NEAR_K);
        near_gain + (1.0 - near_gain) * t
    }

    fn export_gain_env(key: &str, default: f64) -> f64 {
        std::env::var(key)
            .ok()
            .and_then(|raw| raw.trim().parse::<f64>().ok())
            .filter(|v| *v >= 0.80 && *v <= 1.20)
            .unwrap_or(default)
    }

    /// Per-channel additive offsets for the export coloradj temperature path.
    /// Compensates for FFmpeg/GStreamer frei0r bridge differences in color
    /// temperature rendering.  All offsets taper to zero at neutral (6500K).
    pub(crate) fn export_temperature_channel_offsets(temperature: f64) -> (f64, f64, f64) {
        let deviation = (temperature - 6500.0) / 4500.0;
        if deviation.abs() < 0.01 {
            return (0.0, 0.0, 0.0);
        }
        let abs_dev = deviation.abs().min(1.0);

        if deviation < 0.0 {
            // Below neutral (warm effect: low Kelvin = orange).
            // Warm-side offsets intentionally zeroed — chart showed small
            // improvement but natural footage showed small regression,
            // indicating content-dependent behaviour.
            (0.0, 0.0, 0.0)
        } else {
            // Above neutral (cool effect: high Kelvin = blue).
            // FFmpeg bridge doesn't cool enough → excess B and moderate R.
            (-abs_dev * 0.012, -abs_dev * 0.008, -abs_dev * 0.022)
        }
    }

    pub(crate) fn export_tonal_parity_inputs(
        shadows: f64,
        midtones: f64,
        highlights: f64,
    ) -> (f64, f64, f64) {
        let shadows_pos_gain = Self::export_gain_env("US_EXPORT_SHADOWS_POS_GAIN", 1.0);
        let midtones_neg_gain = Self::export_gain_env("US_EXPORT_MIDTONES_NEG_GAIN", 1.0);
        let highlights_neg_gain = Self::export_gain_env("US_EXPORT_HIGHLIGHTS_NEG_GAIN", 1.0);
        Self::apply_export_tonal_parity_gains(
            shadows,
            midtones,
            highlights,
            shadows_pos_gain,
            midtones_neg_gain,
            highlights_neg_gain,
        )
    }

    fn prerender_build_mask_alpha(
        clip: &ProgramClip,
        out_w: u32,
        out_h: u32,
    ) -> Option<crate::media::mask_alpha::FfmpegMaskAlphaResult> {
        crate::media::mask_alpha::build_combined_mask_ffmpeg_alpha(
            &clip.masks,
            out_w,
            out_h,
            0,
            clip.scale,
            clip.position_x,
            clip.position_y,
        )
    }

    fn prerender_append_mask_filter(
        nodes: &mut Vec<String>,
        input_label: &str,
        output_label: &str,
        clip: &ProgramClip,
        out_w: u32,
        out_h: u32,
        mask_temp_files: &mut Vec<tempfile::NamedTempFile>,
    ) -> bool {
        match Self::prerender_build_mask_alpha(clip, out_w, out_h) {
            Some(crate::media::mask_alpha::FfmpegMaskAlphaResult::GeqExpression(expr)) => {
                nodes.push(format!(
                    "[{input_label}]geq=lum='lum(X,Y)':cb='cb(X,Y)':cr='cr(X,Y)':a='alpha(X,Y)*({expr})'[{output_label}]"
                ));
                true
            }
            Some(crate::media::mask_alpha::FfmpegMaskAlphaResult::RasterFile(mask_file)) => {
                let mask_path = mask_file
                    .path()
                    .display()
                    .to_string()
                    .replace('\\', "\\\\")
                    .replace(':', "\\:");
                let mask_label = format!("{output_label}_mask");
                nodes.push(format!(
                    "movie='{mask_path}',format=gray,scale={out_w}:{out_h}[{mask_label}]"
                ));
                nodes.push(format!(
                    "[{input_label}][{mask_label}]alphamerge[{output_label}]"
                ));
                mask_temp_files.push(mask_file);
                true
            }
            None => false,
        }
    }

    pub(crate) fn apply_export_tonal_parity_gains(
        shadows: f64,
        midtones: f64,
        highlights: f64,
        shadows_pos_gain: f64,
        midtones_neg_gain: f64,
        highlights_neg_gain: f64,
    ) -> (f64, f64, f64) {
        let sh = shadows.clamp(-1.0, 1.0);
        let mid = midtones.clamp(-1.0, 1.0);
        let hi = highlights.clamp(-1.0, 1.0);

        let sh = if sh > 0.0 {
            (sh * shadows_pos_gain).clamp(-1.0, 1.0)
        } else {
            sh
        };
        let mid = if mid < 0.0 {
            (mid * midtones_neg_gain).clamp(-1.0, 1.0)
        } else {
            mid
        };
        let hi = if hi < 0.0 {
            (hi * highlights_neg_gain).clamp(-1.0, 1.0)
        } else {
            hi
        };
        (sh, mid, hi)
    }

    /// Returns true if two clips would produce effects bins with the same
    /// element topology (same set of active effect elements).  When true,
    /// a reused slot's effects bin can be updated via property sets alone,
    /// without adding/removing GStreamer elements.
    fn effects_topology_matches(a: &ProgramClip, b: &ProgramClip) -> bool {
        let need_balance = |c: &ProgramClip| {
            c.brightness != 0.0
                || c.contrast != 1.0
                || c.saturation != 1.0
                || c.exposure.abs() > f64::EPSILON
                || !c.brightness_keyframes.is_empty()
                || !c.contrast_keyframes.is_empty()
                || !c.saturation_keyframes.is_empty()
        };
        let need_coloradj = |c: &ProgramClip| {
            (c.temperature - 6500.0).abs() > 1.0
                || c.tint.abs() > 0.001
                || !c.temperature_keyframes.is_empty()
                || !c.tint_keyframes.is_empty()
        };
        let need_3point = |c: &ProgramClip| {
            c.shadows.abs() > f64::EPSILON
                || c.midtones.abs() > f64::EPSILON
                || c.highlights.abs() > f64::EPSILON
                || c.black_point.abs() > f64::EPSILON
                || c.highlights_warmth.abs() > f64::EPSILON
                || c.highlights_tint.abs() > f64::EPSILON
                || c.midtones_warmth.abs() > f64::EPSILON
                || c.midtones_tint.abs() > f64::EPSILON
                || c.shadows_warmth.abs() > f64::EPSILON
                || c.shadows_tint.abs() > f64::EPSILON
        };
        let need_blur = |c: &ProgramClip| {
            let sigma = (c.denoise * 4.0 - c.sharpness * 6.0).clamp(-20.0, 20.0);
            sigma.abs() > f64::EPSILON
        };
        let need_creative_blur = |c: &ProgramClip| c.blur > f64::EPSILON;
        let need_rotate = |c: &ProgramClip| c.rotate.rem_euclid(360) != 0;
        let need_flip = |c: &ProgramClip| c.flip_h || c.flip_v;
        let need_title = |c: &ProgramClip| !c.title_text.is_empty();
        let need_chroma_key = |c: &ProgramClip| c.chroma_key_enabled;

        need_balance(a) == need_balance(b)
            && need_coloradj(a) == need_coloradj(b)
            && need_3point(a) == need_3point(b)
            && need_blur(a) == need_blur(b)
            && need_creative_blur(a) == need_creative_blur(b)
            && need_rotate(a) == need_rotate(b)
            && need_flip(a) == need_flip(b)
            && need_title(a) == need_title(b)
            && need_chroma_key(a) == need_chroma_key(b)
            && a.is_freeze_frame() == b.is_freeze_frame()
    }

    /// Verify a slot actually has the GStreamer elements required by the desired
    /// clip.  This guards against stale `self.clips` data: when slider callbacks
    /// update `self.clips` in-place before a rebuild, `effects_topology_matches`
    /// can return true (comparing a clip against its own updated entry) even
    /// though the slot was built for a different topology.
    fn slot_satisfies_clip(slot: &VideoSlot, clip: &ProgramClip) -> bool {
        let need_balance = clip.brightness != 0.0
            || clip.contrast != 1.0
            || clip.saturation != 1.0
            || clip.exposure.abs() > f64::EPSILON
            || !clip.brightness_keyframes.is_empty()
            || !clip.contrast_keyframes.is_empty()
            || !clip.saturation_keyframes.is_empty();
        let need_coloradj = (clip.temperature - 6500.0).abs() > 1.0
            || clip.tint.abs() > 0.001
            || !clip.temperature_keyframes.is_empty()
            || !clip.tint_keyframes.is_empty();
        let need_3point = clip.shadows.abs() > f64::EPSILON
            || clip.midtones.abs() > f64::EPSILON
            || clip.highlights.abs() > f64::EPSILON
            || clip.black_point.abs() > f64::EPSILON
            || clip.highlights_warmth.abs() > f64::EPSILON
            || clip.highlights_tint.abs() > f64::EPSILON
            || clip.midtones_warmth.abs() > f64::EPSILON
            || clip.midtones_tint.abs() > f64::EPSILON
            || clip.shadows_warmth.abs() > f64::EPSILON
            || clip.shadows_tint.abs() > f64::EPSILON;
        let need_blur = {
            let sigma = (clip.denoise * 4.0 - clip.sharpness * 6.0).clamp(-20.0, 20.0);
            sigma.abs() > f64::EPSILON
        };
        let need_creative_blur = clip.blur > f64::EPSILON;
        let need_rotate = clip.rotate.rem_euclid(360) != 0;
        let need_flip = clip.flip_h || clip.flip_v;
        let need_title = !clip.title_text.is_empty();
        let need_chroma_key = clip.chroma_key_enabled;
        let need_freeze_hold =
            clip.is_freeze_frame() || (clip.is_image && !slot.animated_svg_rendered);

        // User-applied frei0r effects: topology must match (same count + same
        // plugin names in the same order).
        let enabled_effects: Vec<&str> = clip
            .frei0r_effects
            .iter()
            .filter(|e| e.enabled)
            .map(|e| e.plugin_name.as_str())
            .collect();
        let frei0r_ok = slot.frei0r_user_effects.len() == enabled_effects.len()
            && slot
                .frei0r_user_effects
                .iter()
                .zip(enabled_effects.iter())
                .all(|(elem, &name)| {
                    let factory_name = elem
                        .factory()
                        .map(|f| f.name().to_string())
                        .unwrap_or_default();
                    factory_name == format!("frei0r-filter-{}", name)
                });

        // Temperature/tint needs either coloradj_rgb (preferred) or
        // videobalance (hue-rotation fallback).
        let coloradj_ok =
            !need_coloradj || slot.coloradj_rgb.is_some() || slot.videobalance.is_some();

        // Shadows/midtones/highlights need either colorbalance_3pt (preferred)
        // or videobalance (polynomial fallback).
        let threepoint_ok =
            !need_3point || slot.colorbalance_3pt.is_some() || slot.videobalance.is_some();

        (!need_balance || slot.videobalance.is_some())
            && coloradj_ok
            && threepoint_ok
            && (!need_blur || slot.gaussianblur.is_some())
            && (!need_creative_blur || slot.squareblur.is_some())
            && (!need_rotate || slot.videoflip_rotate.is_some())
            && (!need_flip || slot.videoflip_flip.is_some())
            && (!need_title || slot.textoverlay.is_some())
            && (!need_chroma_key || slot.alpha_chroma_key.is_some())
            && (!need_freeze_hold || slot.imagefreeze.is_some())
            && frei0r_ok
    }

    /// Determine whether all current slots can be reused for the desired
    /// clip set by matching slots to desired clips by compatible source/effects
    /// topology (not strictly by positional index).
    fn compute_reuse_plan(&mut self, desired: &[usize]) -> SlotReusePlan {
        let fail = SlotReusePlan {
            all_reusable: false,
            mappings: vec![],
        };

        // Must have same count — otherwise topology changes.
        if desired.len() != self.slots.len() || desired.is_empty() {
            return fail;
        }

        let mut mappings = Vec::with_capacity(desired.len());
        let mut unmatched_slots: Vec<usize> = (0..self.slots.len()).collect();

        for &desired_clip_idx in desired {
            let desired_clip = &self.clips[desired_clip_idx];
            let desired_path = self.effective_source_path_for_clip(desired_clip);
            let desired_has_audio = self.has_audio_for_path_fast(
                &desired_path,
                self.clips[desired_clip_idx].has_embedded_audio(),
            );

            let mut matched: Option<(usize, usize)> = None; // (unmatched index, slot_idx)
            for (unmatched_idx, &slot_idx) in unmatched_slots.iter().enumerate() {
                let current_clip_idx = self.slots[slot_idx].clip_idx;
                let current_clip = &self.clips[current_clip_idx];
                let current_path = self.effective_source_path_for_clip(current_clip);
                if desired_path != current_path {
                    continue;
                }
                let current_has_audio = self.slots[slot_idx].audio_mixer_pad.is_some();
                if desired_has_audio != current_has_audio {
                    continue;
                }
                if (desired_clip.speed - current_clip.speed).abs() > f64::EPSILON
                    || desired_clip.reverse != current_clip.reverse
                {
                    continue;
                }
                if !Self::effects_topology_matches(current_clip, desired_clip) {
                    continue;
                }
                // Verify slot actually has the GStreamer elements the desired
                // clip needs (guards against in-place self.clips updates).
                if !Self::slot_satisfies_clip(&self.slots[slot_idx], desired_clip) {
                    continue;
                }
                // When the source in-point changes (e.g. silence-removal
                // sub-clips), force a full rebuild instead of reusing the
                // decoder.  Seeking a reused uridecodebin to a different
                // source range within the same file can produce stale frames
                // from the previous position on some demuxer/codec paths.
                if desired_clip.source_in_ns != current_clip.source_in_ns
                    || desired_clip.source_out_ns != current_clip.source_out_ns
                {
                    continue;
                }
                matched = Some((unmatched_idx, slot_idx));
                break;
            }

            let Some((unmatched_idx, slot_idx)) = matched else {
                return fail;
            };
            unmatched_slots.remove(unmatched_idx);
            mappings.push((slot_idx, desired_clip_idx));
        }

        SlotReusePlan {
            all_reusable: true,
            mappings,
        }
    }

    /// Update all mutable effect properties on a reused slot without
    /// rebuilding the effects bin.
    fn update_slot_effects(&self, slot_idx: usize, clip: &ProgramClip, timeline_pos: u64) {
        let slot = &self.slots[slot_idx];
        let brightness = clip.brightness_at_timeline_ns(timeline_pos);
        let contrast = clip.contrast_at_timeline_ns(timeline_pos);
        let saturation = clip.saturation_at_timeline_ns(timeline_pos);
        let temperature = clip.temperature_at_timeline_ns(timeline_pos);
        let tint = clip.tint_at_timeline_ns(timeline_pos);

        // Videobalance (calibrated preview mapping)
        let has_coloradj = slot.coloradj_rgb.is_some();
        let has_3point = slot.colorbalance_3pt.is_some();
        if let Some(ref vb) = slot.videobalance {
            let p = Self::compute_videobalance_params(
                brightness,
                contrast,
                saturation,
                temperature,
                tint,
                clip.shadows,
                clip.midtones,
                clip.highlights,
                clip.exposure,
                clip.black_point,
                clip.highlights_warmth,
                clip.highlights_tint,
                clip.midtones_warmth,
                clip.midtones_tint,
                clip.shadows_warmth,
                clip.shadows_tint,
                has_coloradj,
                has_3point,
            );
            vb.set_property("brightness", p.brightness);
            vb.set_property("contrast", p.contrast);
            vb.set_property("saturation", p.saturation);
            vb.set_property("hue", p.hue);
        }

        // frei0r coloradj_RGB (per-channel temperature/tint)
        if let Some(ref ca) = slot.coloradj_rgb {
            let cp = Self::compute_coloradj_params(temperature, tint);
            ca.set_property("r", cp.r);
            ca.set_property("g", cp.g);
            ca.set_property("b", cp.b);
        }

        // frei0r 3-point-color-balance (shadows/midtones/highlights)
        if let Some(ref tp) = slot.colorbalance_3pt {
            let p = Self::compute_3point_params(
                clip.shadows,
                clip.midtones,
                clip.highlights,
                clip.black_point,
                clip.highlights_warmth,
                clip.highlights_tint,
                clip.midtones_warmth,
                clip.midtones_tint,
                clip.shadows_warmth,
                clip.shadows_tint,
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

        // Gaussianblur (denoise/sharpness)
        if let Some(ref gb) = slot.gaussianblur {
            let sigma = (clip.denoise * 4.0 - clip.sharpness * 6.0).clamp(-20.0, 20.0);
            gb.set_property("sigma", sigma);
        }

        // Squareblur (creative blur)
        if let Some(ref sb) = slot.squareblur {
            sb.set_property("kernel-size", clip.blur.clamp(0.0, 1.0));
        }

        // Chroma key alpha element
        if let Some(ref ck) = slot.alpha_chroma_key {
            let r = ((clip.chroma_key_color >> 16) & 0xFF) as u32;
            let g = ((clip.chroma_key_color >> 8) & 0xFF) as u32;
            let b = (clip.chroma_key_color & 0xFF) as u32;
            ck.set_property("target-r", r);
            ck.set_property("target-g", g);
            ck.set_property("target-b", b);
            ck.set_property(
                "angle",
                (clip.chroma_key_tolerance * 90.0).clamp(0.0, 90.0) as f32,
            );
            ck.set_property(
                "noise-level",
                (clip.chroma_key_softness * 64.0).clamp(0.0, 64.0) as f32,
            );
        }

        // Crop, rotate, flip
        Self::apply_transform_to_slot(
            slot,
            clip.crop_left_at_timeline_ns(self.timeline_pos_ns),
            clip.crop_right_at_timeline_ns(self.timeline_pos_ns),
            clip.crop_top_at_timeline_ns(self.timeline_pos_ns),
            clip.crop_bottom_at_timeline_ns(self.timeline_pos_ns),
            clip.rotate_at_timeline_ns(self.timeline_pos_ns),
            clip.flip_h,
            clip.flip_v,
        );

        // Title overlay
        {
            let (pw, ph) = self.preview_processing_dimensions();
            Self::apply_title_to_slot(
                slot,
                &clip.title_text,
                &clip.title_font,
                clip.title_color,
                clip.title_x,
                clip.title_y,
                self.project_height,
                pw,
                ph,
            );
            Self::apply_title_style_to_slot(slot, clip);
        }

        // Compositor pad: opacity + zoom/position
        if let Some(ref pad) = slot.compositor_pad {
            pad.set_property(
                "alpha",
                clip.opacity_at_timeline_ns(self.timeline_pos_ns)
                    .clamp(0.0, 1.0),
            );
            let (proc_w, proc_h) = self.preview_processing_dimensions();
            Self::apply_zoom_to_slot(
                slot,
                pad,
                clip.scale_at_timeline_ns(self.timeline_pos_ns),
                clip.position_x_at_timeline_ns(self.timeline_pos_ns),
                clip.position_y_at_timeline_ns(self.timeline_pos_ns),
                Self::clip_uses_direct_canvas_translation(clip),
                proc_w,
                proc_h,
            );
        }

        // Audiomixer pad: volume
        if let Some(ref pad) = slot.audio_mixer_pad {
            pad.set_property(
                "volume",
                self.effective_main_clip_volume(slot.clip_idx, self.timeline_pos_ns),
            );
        }
    }

    /// Fast-path boundary crossing: reuse existing decoder slots when the
    /// incoming clips share the same source files.  Avoids tearing down
    /// and rebuilding uridecodebin, effects_bin, and compositor pads.
    fn continue_decoders_at(&mut self, timeline_pos: u64, desired: &[usize], plan: &SlotReusePlan) {
        let rebuild_started = Instant::now();
        let was_playing = self.state == PlayerState::Playing;
        self.prerender_active_clips = None;
        self.current_prerender_segment_key = None;

        log::info!(
            "continue_decoders_at: START timeline_pos={}ns slots={} was_playing={}",
            timeline_pos,
            self.slots.len(),
            was_playing
        );

        // Tear down prewarming sidecars (no longer needed).
        self.teardown_prepreroll_sidecars();

        // 1. Update slot metadata: point each slot at its new clip.
        for &(slot_idx, desired_clip_idx) in &plan.mappings {
            self.slots[slot_idx].clip_idx = desired_clip_idx;
            self.slots[slot_idx].transition_enter_offset_ns =
                self.transition_enter_offset_for_clip(desired_clip_idx);
        }

        // Keep compositor z-order aligned to desired track order.
        let slot_for_clip: HashMap<usize, usize> = plan
            .mappings
            .iter()
            .map(|(slot_idx, clip_idx)| (*clip_idx, *slot_idx))
            .collect();
        for (zorder_offset, &desired_clip_idx) in desired.iter().enumerate() {
            if let Some(&slot_idx) = slot_for_clip.get(&desired_clip_idx) {
                if let Some(ref pad) = self.slots[slot_idx].compositor_pad {
                    pad.set_property("zorder", (zorder_offset + 1) as u32);
                }
            }
        }

        // 2. Update effects properties on each reused slot.
        for &(slot_idx, desired_clip_idx) in &plan.mappings {
            let clip = &self.clips[desired_clip_idx];
            self.update_slot_effects(slot_idx, clip, timeline_pos);
        }

        // 3. Update current_idx to highest-priority clip.
        self.current_idx = desired.last().copied();

        // 4. Reset start_time for running-time alignment.
        self.pipeline.set_start_time(gst::ClockTime::ZERO);

        // 5. Snapshot arrival counters, then flush compositor + audiomixer.
        let baseline = self.snapshot_arrival_seqs();
        let _ = self
            .compositor
            .seek_simple(gst::SeekFlags::FLUSH, gst::ClockTime::ZERO);
        let _ = self
            .audiomixer
            .seek_simple(gst::SeekFlags::FLUSH, gst::ClockTime::ZERO);

        // 6. Seek each decoder to its new source position.
        for slot in &self.slots {
            let clip = &self.clips[slot.clip_idx];
            if was_playing {
                let _ = Self::seek_slot_decoder_with_retry(
                    slot,
                    clip,
                    timeline_pos,
                    self.clip_seek_flags(),
                    self.frame_duration_ns,
                );
            } else {
                let _ = Self::seek_slot_decoder_paused_with_retry(slot, clip, timeline_pos);
            }
        }

        // 7. Wait for preroll + compositor arrivals.
        //    Tighter budget than full rebuild (no codec init needed).
        self.wait_for_paused_preroll();
        self.wait_for_compositor_arrivals(&baseline, 1500);

        // 8. Restore playback state.
        if was_playing {
            let _ = self.pipeline.set_state(gst::State::Playing);
        }

        self.apply_transition_effects(timeline_pos);

        let elapsed = rebuild_started.elapsed().as_millis();
        self.record_rebuild_duration_ms(elapsed as u64);
        log::info!(
            "continue_decoders_at: END timeline_pos={}ns elapsed_ms={}",
            timeline_pos,
            elapsed
        );
    }

    /// Remove excess slots for clips that are no longer active, keeping
    /// slots for clips that are still playing.  This avoids a full
    /// teardown+rebuild cycle which can corrupt GstVideoAggregator segment
    /// state when compositor pads are released and immediately re-requested.
    ///
    /// The key difference from the disabled incremental path: we flush the
    /// compositor (seek_simple) AFTER removing pads, then re-seek remaining
    /// decoders so they emit fresh segments.  This resets the aggregator's
    /// internal timing/segment state that otherwise causes ≤1 fps or a
    /// gst_segment_to_stream_time assertion crash.
    fn shrink_slots_to_active(&mut self, timeline_pos: u64, desired: &[usize], was_playing: bool) {
        let started = Instant::now();
        let desired_set: HashSet<usize> = desired.iter().copied().collect();
        log::info!(
            "shrink_slots_to_active: START timeline_pos={}ns slots={} -> {} was_playing={}",
            timeline_pos,
            self.slots.len(),
            desired.len(),
            was_playing
        );

        // 1. Flush pads being removed to unblock aggregation.
        for slot in &self.slots {
            if !desired_set.contains(&slot.clip_idx) {
                if let Some(ref pad) = slot.compositor_pad {
                    let _ = pad.send_event(gst::event::FlushStart::new());
                }
                if let Some(ref pad) = slot.audio_mixer_pad {
                    let _ = pad.send_event(gst::event::FlushStart::new());
                }
            }
        }

        // 2. Drain all slots, partition into keep vs remove, then tear down
        //    the expired ones individually.
        let (keep, remove): (Vec<VideoSlot>, Vec<VideoSlot>) = self
            .slots
            .drain(..)
            .partition(|s| desired_set.contains(&s.clip_idx));
        self.slots = keep;
        for slot in remove {
            self.teardown_single_slot(slot);
        }

        // 3. Update bookkeeping.
        self.prerender_active_clips = None;
        self.current_prerender_segment_key = None;
        self.current_idx = desired.last().copied();
        self.pipeline.set_start_time(gst::ClockTime::ZERO);
        self.teardown_prepreroll_sidecars();

        // 4. Align compositor z-order with desired clip order.
        let slot_for_clip: HashMap<usize, usize> = self
            .slots
            .iter()
            .enumerate()
            .map(|(i, s)| (s.clip_idx, i))
            .collect();
        for (zorder_offset, &clip_idx) in desired.iter().enumerate() {
            if let Some(&slot_idx) = slot_for_clip.get(&clip_idx) {
                if let Some(ref pad) = self.slots[slot_idx].compositor_pad {
                    pad.set_property("zorder", (zorder_offset + 1) as u32);
                }
            }
        }

        // 5. Update effects properties on retained slots.
        for slot_idx in 0..self.slots.len() {
            let clip_idx = self.slots[slot_idx].clip_idx;
            let clip = &self.clips[clip_idx];
            self.update_slot_effects(slot_idx, clip, timeline_pos);
        }

        // 6. Flush compositor + audiomixer to reset aggregator timing after
        //    the topology change (pad removal).  This is the critical step
        //    that prevents the segment-format-mismatch crash.
        let baseline = self.snapshot_arrival_seqs();
        let _ = self
            .compositor
            .seek_simple(gst::SeekFlags::FLUSH, gst::ClockTime::ZERO);
        let _ = self
            .audiomixer
            .seek_simple(gst::SeekFlags::FLUSH, gst::ClockTime::ZERO);

        // 7. Re-seek remaining decoders so they emit fresh segments aligned
        //    with the compositor's reset timing.
        let seek_flags = if was_playing {
            gst::SeekFlags::FLUSH | gst::SeekFlags::ACCURATE
        } else {
            self.clip_seek_flags()
        };
        for slot in &self.slots {
            let clip = &self.clips[slot.clip_idx];
            if was_playing {
                let ok = Self::seek_slot_decoder_with_retry(
                    slot,
                    clip,
                    timeline_pos,
                    seek_flags,
                    self.frame_duration_ns,
                );
                if !ok {
                    log::warn!("shrink_slots_to_active: seek FAILED for clip {}", clip.id);
                    if let Some(ref pad) = slot.compositor_pad {
                        let _ = pad.send_event(gst::event::Eos::new());
                    }
                    if let Some(ref pad) = slot.audio_mixer_pad {
                        let _ = pad.send_event(gst::event::Eos::new());
                    }
                }
            } else {
                let _ = Self::seek_slot_decoder_paused_with_retry(slot, clip, timeline_pos);
            }
        }

        // 8. Wait for preroll and compositor arrivals.
        self.wait_for_paused_preroll();
        let budget = if was_playing {
            self.adaptive_arrival_wait_ms(1500)
        } else {
            1200
        };
        self.wait_for_compositor_arrivals(&baseline, budget);

        // 9. Restore playback state.
        if was_playing {
            let _ = self.pipeline.set_state(gst::State::Playing);
        }
        self.apply_transition_effects(timeline_pos);

        if was_playing {
            self.record_rebuild_duration_ms(started.elapsed().as_millis() as u64);
        }
        log::info!(
            "shrink_slots_to_active: END timeline_pos={}ns slots={} elapsed_ms={}",
            timeline_pos,
            self.slots.len(),
            started.elapsed().as_millis()
        );
    }

    fn try_use_background_prerender_slots(
        &mut self,
        timeline_pos: u64,
        active: &[usize],
        was_playing: bool,
    ) -> bool {
        if !self.background_prerender
            || !self.active_supports_background_prerender_at(timeline_pos, active)
        {
            return false;
        }
        let transition_spec = self.transition_overlap_prerender_spec_at(timeline_pos, active);
        let transition_metric_kind = transition_spec
            .as_ref()
            .map(|spec| Self::transition_kind_for_prerender_metric(&spec.xfade_transition));
        if self.active_has_unsupported_background_prerender_features(active) {
            return false;
        }
        self.poll_background_prerender_results();
        let signature = self.prerender_signature_for_active(active);
        let segment = self.find_prerender_segment_for(timeline_pos, active, signature);
        self.record_prerender_cache_lookup(segment.is_some());
        let Some(segment) = segment else {
            let sig_hex = format!("{signature:016x}");
            let same_sig_cached = self
                .prerender_segments
                .values()
                .filter(|seg| seg.signature == signature)
                .count();
            let same_sig_pending = self
                .prerender_pending
                .iter()
                .filter(|k| k.contains(&sig_hex))
                .count();
            log::info!(
                "rebuild_pipeline_at: prerender unavailable at timeline_pos={} sig={} cached={} pending={} failed_total={}",
                timeline_pos,
                sig_hex,
                same_sig_cached,
                same_sig_pending,
                self.prerender_failed.len()
            );
            if let Some(kind) = transition_metric_kind {
                self.record_transition_prerender_metric(kind, false);
            }
            return false;
        };
        let Some(video_slot) =
            self.build_prerender_video_slot(&segment.path, segment.start_ns, 0, was_playing)
        else {
            if let Some(kind) = transition_metric_kind {
                self.record_transition_prerender_metric(kind, false);
            }
            return false;
        };
        self.slots.push(video_slot);
        self.prerender_active_clips = Some(active.to_vec());
        self.current_prerender_segment_key = Some(segment.key.clone());
        log::info!(
            "rebuild_pipeline_at: using background prerender segment key={} range={}..{} path={}",
            segment.key,
            segment.start_ns,
            segment.end_ns,
            segment.path
        );
        if let Some(kind) = transition_metric_kind {
            self.record_transition_prerender_metric(kind, true);
        }
        true
    }
    fn build_live_video_slots_for_active(&mut self, active: &[usize], was_playing: bool) {
        self.prerender_active_clips = None;
        self.current_prerender_segment_key = None;
        for (zorder_offset, &clip_idx) in active.iter().enumerate() {
            // Adjustment layers have no compositor slot — effects are applied
            // by the post-compositor probe.
            if clip_idx < self.clips.len() && self.clips[clip_idx].is_adjustment {
                continue;
            }
            // Audio-only clips are handled by the separate audio pipeline,
            // not the main compositor pipeline (to avoid clock interference).
            // Compute transition-enter offset for clips entering via transition overlap.
            let trans_offset = self.transition_enter_offset_for_clip(clip_idx);
            // When experimental preview optimizations are enabled, use lightweight
            // audio-only decoder slots for clips that are fully occluded by an
            // opaque full-frame clip above them.
            if self.experimental_preview_optimizations
                && self.is_clip_video_occluded(active, zorder_offset)
            {
                if let Some(slot) = self.build_audio_only_slot_for_clip(clip_idx) {
                    self.slots.push(slot);
                } else if let Some(mut slot) =
                    self.build_slot_for_clip(clip_idx, zorder_offset, was_playing)
                {
                    log::warn!(
                        "rebuild_pipeline_at: audio-only occlusion slot failed for clip {}, falling back to full slot",
                        self.clips[clip_idx].id
                    );
                    slot.transition_enter_offset_ns = trans_offset;
                    self.slots.push(slot);
                }
            } else if let Some(mut slot) =
                self.build_slot_for_clip(clip_idx, zorder_offset, was_playing)
            {
                slot.transition_enter_offset_ns = trans_offset;
                self.slots.push(slot);
            }
        }
    }

    /// Core method: tear down all slots and rebuild for clips active at `timeline_pos`.
    fn rebuild_pipeline_at(&mut self, timeline_pos: u64) {
        self.timeline_pos_ns = timeline_pos;
        self.sync_adjustment_timeline_pos();
        self.rebuild_adjustment_overlays();
        let rebuild_started = Instant::now();
        let was_playing = self.state == PlayerState::Playing;
        let had_existing_slots = !self.slots.is_empty();
        self.poll_background_prerender_results();
        log::debug!(
            "rebuild_pipeline_at: START was_playing={} timeline_pos={}ns slots={}",
            was_playing,
            timeline_pos,
            self.slots.len()
        );

        // ── Continuing decoders fast path ──────────────────────────────
        // When all incoming clips share the same source files as the
        // current slots, we can skip teardown/rebuild entirely and just
        // seek the existing decoders.  This avoids the GstVideoAggregator
        // topology-change issue because no compositor pads are added or
        // removed.
        if had_existing_slots {
            let desired = self.clips_active_at(timeline_pos);
            let bypass_continue_for_prerender = if self.background_prerender
                && self.active_supports_background_prerender_at(timeline_pos, &desired)
                && !self.active_has_unsupported_background_prerender_features(&desired)
                && !self.slots.iter().any(|s| s.is_prerender_slot)
            {
                let signature = self.prerender_signature_for_active(&desired);
                self.find_prerender_segment_for(timeline_pos, &desired, signature)
                    .is_some()
            } else {
                false
            };
            if bypass_continue_for_prerender {
                log::info!(
                    "rebuild_pipeline_at: prerender ready at timeline_pos={}, bypassing continue-decoder fast path",
                    timeline_pos
                );
            }
            let plan = self.compute_reuse_plan(&desired);
            if plan.all_reusable && !bypass_continue_for_prerender {
                self.continue_decoders_at(timeline_pos, &desired, &plan);
                return;
            }

            // Shrink path: when desired clips are a strict subset of the
            // current slots (some clips ended), remove excess slots instead
            // of a full teardown+rebuild.  A full rebuild releases ALL
            // compositor pads and re-requests them, which can corrupt the
            // GstVideoAggregator's internal segment state and cause a
            // gst_segment_to_stream_time assertion crash.
            //
            // The shrink path flushes the compositor AFTER removing pads,
            // then re-seeks retained decoders — this properly resets the
            // aggregator timing without tearing down slots that are still
            // producing valid frames.
            if !bypass_continue_for_prerender
                && desired.len() < self.slots.len()
                && !desired.is_empty()
                && desired
                    .iter()
                    .all(|&ci| self.slots.iter().any(|s| s.clip_idx == ci))
            {
                self.shrink_slots_to_active(timeline_pos, &desired, was_playing);
                return;
            }
        }

        // The add-only incremental path is still disabled: adding compositor
        // sink pads mid-stream without a full rebuild causes the aggregator
        // to freeze.  The full-rebuild path handles add-only transitions.
        #[allow(unused)]
        const INCREMENTAL_BOUNDARY: bool = false;

        // Full rebuild: tear down existing slots FIRST — each decoder is set
        // to Null individually, which avoids a pipeline-wide state change on
        // elements that may be mid-transition (causing a main-thread
        // deadlock when gtk4paintablesink needs the main loop to complete
        // its transition).
        self.teardown_slots();
        // Tear down pre-preroll sidecars now that the real rebuild is starting.
        // The OS file cache and codec state benefits persist after Null.
        self.teardown_prepreroll_sidecars();
        let t_teardown = rebuild_started.elapsed().as_millis();

        // Avoid pipeline-wide Ready transitions here: in some media/layout
        // combinations this can deadlock in gst_pad_set_active while pads are
        // being reconfigured. Reset start_time instead so the next Playing
        // pulse starts from a clean running-time baseline.
        self.pipeline.set_start_time(gst::ClockTime::ZERO);

        let active = self.clips_active_at(timeline_pos);
        if active.is_empty() {
            self.current_idx = None;
            // Move back to Paused so the background sources are ready.
            let _ = self.pipeline.set_state(gst::State::Paused);
            if was_playing {
                let _ = self.pipeline.set_state(gst::State::Playing);
            }
            log::info!(
                "rebuild_pipeline_at: END timeline_pos={}ns slots=0 elapsed_ms={}",
                timeline_pos,
                rebuild_started.elapsed().as_millis()
            );
            return;
        }

        // Update current_idx to highest-priority clip.
        self.current_idx = active.last().copied();

        if log::log_enabled!(log::Level::Info) {
            let resolved: Vec<String> = active
                .iter()
                .map(|&idx| {
                    let clip = &self.clips[idx];
                    let (path, using_proxy, key, _) = self.resolve_source_path_for_clip(clip);
                    if self.proxy_enabled {
                        format!(
                            "clip={} track={} mode={} key={} path={}",
                            clip.id,
                            clip.track_index,
                            if using_proxy {
                                "proxy"
                            } else {
                                "fallback-original"
                            },
                            key,
                            path
                        )
                    } else {
                        format!(
                            "clip={} track={} mode=original path={}",
                            clip.id, clip.track_index, path
                        )
                    }
                })
                .collect();
            log::info!(
                "rebuild_pipeline_at: timeline_pos={}ns active_resolved_sources=[{}]",
                timeline_pos,
                resolved.join(" | ")
            );
        }

        let used_prerender =
            self.try_use_background_prerender_slots(timeline_pos, &active, was_playing);
        if !used_prerender {
            self.build_live_video_slots_for_active(&active, was_playing);
        }
        // Rebuild the post-compositor adjustment effects chain if needed.
        self.rebuild_adjustment_effects_chain();
        let t_build = rebuild_started.elapsed().as_millis();

        // Transition pipeline to Paused so decoders preroll and can accept seeks.
        log::debug!(
            "rebuild_pipeline_at: setting Paused for preroll (was_playing={})",
            was_playing
        );
        let _ = self.pipeline.set_state(gst::State::Paused);
        log::debug!("rebuild_pipeline_at: waiting for paused preroll...");
        // Give decoders time to discover streams and link pads (up to 5s).
        // We cannot do a full wait_for_paused_preroll here because the
        // compositor might be waiting on an unlinked pad.  Use a short
        // timeout to let decoders link, then EOS any that didn't.
        let mut link_wait_ms = if was_playing {
            self.adaptive_arrival_wait_ms(400)
        } else {
            self.effective_wait_timeout_ms(400)
        };
        if used_prerender {
            link_wait_ms = link_wait_ms.max(3000);
        }
        let _ = self
            .pipeline
            .state(gst::ClockTime::from_mseconds(link_wait_ms));

        if used_prerender {
            let deadline = Instant::now() + Duration::from_millis(250);
            while Instant::now() < deadline {
                let all_prerender_linked = self
                    .slots
                    .iter()
                    .filter(|slot| slot.is_prerender_slot)
                    .all(|slot| slot.video_linked.load(Ordering::Relaxed));
                if all_prerender_linked {
                    break;
                }
                std::thread::sleep(Duration::from_millis(5));
            }
        }
        let prerender_unlinked = used_prerender
            && self
                .slots
                .iter()
                .any(|slot| slot.is_prerender_slot && !slot.video_linked.load(Ordering::Relaxed));
        if prerender_unlinked {
            log::warn!(
                "rebuild_pipeline_at: prerender slot not linked within initial window (budget={}ms), forcing live fallback rebuild",
                link_wait_ms
            );
            self.teardown_slots();
            self.prerender_active_clips = None;
            if let Some(key) = self.current_prerender_segment_key.take() {
                self.prerender_segments.remove(&key);
                self.prerender_failed.insert(key.clone());
                log::warn!(
                    "rebuild_pipeline_at: marked prerender segment failed after link-time fallback key={}",
                    key
                );
            }
            let prev = self.background_prerender;
            self.background_prerender = false;
            self.rebuild_pipeline_at(timeline_pos);
            self.background_prerender = prev;
            return;
        }
        let t_link_wait = rebuild_started.elapsed().as_millis();

        self.wait_for_paused_preroll();
        let t_preroll = rebuild_started.elapsed().as_millis();
        log::debug!("rebuild_pipeline_at: paused preroll done");

        // Link detection can legitimately lag the initial link-wait window.
        // Do not force EOS on "unlinked" slots before preroll settles:
        // those slots frequently link a few milliseconds later, and injecting
        // EOS early can poison decode negotiation (qtdemux not-negotiated).
        for slot in &self.slots {
            if !slot.video_linked.load(Ordering::Relaxed) {
                let audio_linked = slot.audio_linked.load(Ordering::Relaxed);
                log::warn!(
                    "rebuild_pipeline_at: clip_idx={} still has unlinked video after paused preroll (audio_linked={})",
                    slot.clip_idx,
                    audio_linked
                );
            }
        }

        // Atomically flush the compositor and audiomixer plus ALL their
        // downstream elements (tee, video sink, audio convert, audio sink).
        // Both aggregators must be flushed so their output segments stay in
        // sync after the decoder seeks that follow.  Without the audiomixer
        // flush the audio sink's running-time drifts from the video path,
        // causing the audiomixer to drop audio buffers as "late".
        let baseline = self.snapshot_arrival_seqs();
        log::debug!("rebuild_pipeline_at: compositor+audiomixer flush...");
        let _ = self
            .compositor
            .seek_simple(gst::SeekFlags::FLUSH, gst::ClockTime::ZERO);
        let _ = self
            .audiomixer
            .seek_simple(gst::SeekFlags::FLUSH, gst::ClockTime::ZERO);
        log::debug!("rebuild_pipeline_at: compositor+audiomixer flush done");

        // Seek each decoder to its source position with stop boundary.
        //
        // During active playback rebuilds we must use ACCURATE seeks here.
        // KEY_UNIT seeks can snap long-GOP proxy media back to the nearest
        // keyframe (often 0s), which looks like lower-track clips restarting
        // when another track enters/exits and triggers a rebuild.
        log::debug!(
            "rebuild_pipeline_at: seeking {} decoders (was_playing={})",
            self.slots.len(),
            was_playing
        );
        let seek_flags = if was_playing {
            gst::SeekFlags::FLUSH | gst::SeekFlags::ACCURATE
        } else {
            self.clip_seek_flags()
        };
        for slot in &self.slots {
            let Some(clip) = self.clips.get(slot.clip_idx) else {
                continue;
            };
            if was_playing {
                let ok = Self::seek_slot_decoder_with_retry(
                    slot,
                    clip,
                    timeline_pos,
                    seek_flags,
                    self.frame_duration_ns,
                );
                if !ok {
                    log::warn!("rebuild_pipeline_at: seek FAILED for clip {}", clip.id);
                    if let Some(ref pad) = slot.compositor_pad {
                        let _ = pad.send_event(gst::event::Eos::new());
                    }
                    if let Some(ref pad) = slot.audio_mixer_pad {
                        let _ = pad.send_event(gst::event::Eos::new());
                    }
                }
            } else {
                let _ = Self::seek_slot_decoder_paused_with_retry(slot, clip, timeline_pos);
            }
        }
        log::debug!("rebuild_pipeline_at: decoder seeks done");
        let t_seeks = rebuild_started.elapsed().as_millis();

        // Post-seek settle
        let mut arrivals_ok = true;
        if was_playing {
            self.wait_for_paused_preroll();
            // When the boundary was prewarmed via a sidecar pipeline, the OS
            // file cache is warm and decoders settle faster.  Use a tighter
            // arrival budget to reduce the visible/audible blip.
            let prewarmed = self.prewarmed_boundary_ns == Some(timeline_pos);
            let arrival_nominal = if prewarmed { 900 } else { 1500 };
            let arrival_budget = self.adaptive_arrival_wait_ms(arrival_nominal);
            log::debug!(
                "rebuild_pipeline_at: post-seek wait_for_compositor_arrivals ({}ms, scale={:.2}, prewarmed={})",
                arrival_budget,
                self.rebuild_wait_scale(),
                prewarmed
            );
            arrivals_ok = self.wait_for_compositor_arrivals(&baseline, arrival_budget);
        }

        // In paused mode, first try a single settle pass. Fall back to a
        // second reseek pass only when links/arrivals are incomplete.
        if !was_playing {
            self.wait_for_paused_preroll();
            let all_video_linked = self
                .slots
                .iter()
                .all(|slot| slot.video_linked.load(Ordering::Relaxed));
            let first_pass_arrived = self.wait_for_compositor_arrivals(&baseline, 1200);
            let mut paused_arrived = first_pass_arrived;

            if !all_video_linked || !first_pass_arrived {
                if self.should_prioritize_ui_responsiveness() {
                    log::warn!(
                        "rebuild_pipeline_at: skipping second paused settle pass for responsiveness (linked={} arrived={})",
                        all_video_linked,
                        first_pass_arrived
                    );
                } else {
                    log::debug!(
                        "rebuild_pipeline_at: running second paused settle pass (linked={} arrived={})",
                        all_video_linked,
                        first_pass_arrived
                    );
                    let baseline = self.snapshot_arrival_seqs();
                    // Flush both compositor and audiomixer atomically before
                    // per-decoder seeks to clear stale preroll state.
                    let _ = self
                        .compositor
                        .seek_simple(gst::SeekFlags::FLUSH, gst::ClockTime::ZERO);
                    let _ = self
                        .audiomixer
                        .seek_simple(gst::SeekFlags::FLUSH, gst::ClockTime::ZERO);
                    for slot in &self.slots {
                        let clip = &self.clips[slot.clip_idx];
                        let _ = Self::seek_slot_decoder_paused_with_retry(slot, clip, timeline_pos);
                    }
                    let _ = self.pipeline.set_state(gst::State::Paused);
                    self.wait_for_paused_preroll();
                    paused_arrived = self.wait_for_compositor_arrivals(&baseline, 3000);
                }
            } else {
                log::debug!("rebuild_pipeline_at: skipping second paused settle pass");
            }
            arrivals_ok = paused_arrived;
            let (_, pipe_cur, pipe_pend) = self.pipeline.state(gst::ClockTime::ZERO);
            log::info!(
                "rebuild_pipeline_at: after_settle slots={} pipe={:?}/{:?} seq={}",
                self.slots.len(),
                pipe_cur,
                pipe_pend,
                self.scope_frame_seq.load(Ordering::Relaxed)
            );
        }

        if used_prerender && !arrivals_ok {
            log::warn!(
                "rebuild_pipeline_at: prerender path had no compositor arrivals at timeline_pos={}, forcing live fallback rebuild",
                timeline_pos
            );
            self.teardown_slots();
            self.prerender_active_clips = None;
            if let Some(key) = self.current_prerender_segment_key.take() {
                self.prerender_segments.remove(&key);
                self.prerender_failed.insert(key.clone());
                log::warn!(
                    "rebuild_pipeline_at: marked prerender segment failed after no-arrival fallback key={}",
                    key
                );
            }
            let prev = self.background_prerender;
            self.background_prerender = false;
            self.rebuild_pipeline_at(timeline_pos);
            self.background_prerender = prev;
            return;
        }

        // Restore pipeline state.
        if was_playing {
            let _ = self.pipeline.set_state(gst::State::Playing);
        }
        let rebuild_elapsed_ms = rebuild_started.elapsed().as_millis() as u64;
        if was_playing {
            self.record_rebuild_duration_ms(rebuild_elapsed_ms);
        }
        log::info!(
            "rebuild_pipeline_at: END timeline_pos={}ns slots={} elapsed_ms={} scale={:.2} phases=teardown:{}|build:{}|link:{}|preroll:{}|seek:{}",
            timeline_pos,
            self.slots.len(),
            rebuild_elapsed_ms,
            self.rebuild_wait_scale(),
            t_teardown, t_build, t_link_wait, t_preroll, t_seeks
        );
    }

    // ── Bus / metering ─────────────────────────────────────────────────────

    fn should_recover_not_negotiated(error: &str, debug: Option<&str>) -> bool {
        let err_lower = error.to_ascii_lowercase();
        if err_lower.contains("not-negotiated") {
            return true;
        }
        debug
            .map(|d| d.to_ascii_lowercase().contains("not-negotiated"))
            .unwrap_or(false)
    }

    fn recover_main_pipeline_not_negotiated(&mut self) {
        const RECOVERY_DEBOUNCE_MS: u64 = 250;
        let now = Instant::now();
        if self
            .last_not_negotiated_recover_at
            .map(|last| {
                now.saturating_duration_since(last) < Duration::from_millis(RECOVERY_DEBOUNCE_MS)
            })
            .unwrap_or(false)
        {
            log::warn!("poll_bus: skipping not-negotiated recovery (debounced)");
            return;
        }
        self.last_not_negotiated_recover_at = Some(now);
        let target_pos = self.timeline_pos_ns.min(self.timeline_dur_ns);
        let was_playing = self.state == PlayerState::Playing;
        log::warn!(
            "poll_bus: recovering from main-pipeline not-negotiated at timeline_pos={} was_playing={}",
            target_pos,
            was_playing
        );
        self.teardown_slots();
        self.rebuild_pipeline_at(target_pos);
        self.teardown_audio_multi_pipeline();
        let _ = self.audio_pipeline.set_state(gst::State::Ready);
        self.audio_current_source = None;
        self.apply_reverse_video_main_audio_ducking(None);
        self.force_sync_audio_to(target_pos);
        if was_playing {
            let _ = self.pipeline.set_state(gst::State::Playing);
            self.resume_synced_audio_playback();
            self.base_timeline_ns = target_pos;
            self.play_start = Some(Instant::now());
        }
    }

    fn poll_bus(&mut self) -> bool {
        let mut eos = false;
        let mut main_not_negotiated = false;
        if let Some(bus) = self.pipeline.bus() {
            while let Some(msg) = bus.pop() {
                match msg.view() {
                    gstreamer::MessageView::Eos(_) => {
                        log::warn!("poll_bus: main pipeline EOS");
                        eos = true;
                    }
                    gstreamer::MessageView::Element(e) => {
                        if let Some(s) = e.structure() {
                            if s.name() == "level" {
                                if let Ok(peak) = s.get::<glib::ValueArray>("peak") {
                                    let vals = peak.as_slice();
                                    let l = vals
                                        .first()
                                        .and_then(|v| v.get::<f64>().ok())
                                        .unwrap_or(-60.0);
                                    let r =
                                        vals.get(1).and_then(|v| v.get::<f64>().ok()).unwrap_or(l);
                                    let src_name = msg
                                        .src()
                                        .and_then(|src| src.clone().downcast::<gst::Element>().ok())
                                        .map(|elem| elem.name().to_string());
                                    let mut handled = false;
                                    if let (Some(ref main_level), Some(ref name)) =
                                        (&self.level_element, &src_name)
                                    {
                                        if main_level.name().as_str() == name.as_str() {
                                            self.push_master_peak(l, r);
                                            handled = true;
                                        }
                                    }
                                    if !handled {
                                        if let Some(ref name) = src_name {
                                            if let Some((is_prerender_slot, clip_idx)) =
                                                self.slots.iter().find_map(|slot| {
                                                    let level = slot.audio_level.as_ref()?;
                                                    (level.name().as_str() == name.as_str())
                                                        .then_some((
                                                            slot.is_prerender_slot,
                                                            slot.clip_idx,
                                                        ))
                                                })
                                            {
                                                if is_prerender_slot {
                                                    let track_indices =
                                                        self.prerender_meter_track_indices();
                                                    if track_indices.is_empty() {
                                                        if let Some(track_index) = self
                                                            .clips
                                                            .get(clip_idx)
                                                            .map(|c| c.track_index)
                                                        {
                                                            self.push_track_peak(track_index, l, r);
                                                            handled = true;
                                                        }
                                                    } else {
                                                        for track_index in track_indices {
                                                            self.push_track_peak(track_index, l, r);
                                                        }
                                                        handled = true;
                                                    }
                                                } else if let Some(track_index) =
                                                    self.clips.get(clip_idx).map(|c| c.track_index)
                                                {
                                                    self.push_track_peak(track_index, l, r);
                                                    handled = true;
                                                }
                                            }
                                        }
                                    }
                                    if !handled {
                                        self.push_master_peak(l, r);
                                    }
                                }
                            }
                        }
                    }
                    gstreamer::MessageView::Error(e) => {
                        let err = e.error().to_string();
                        let debug = e.debug();
                        log::error!("poll_bus: main pipeline error: {} ({:?})", err, debug);
                        if Self::should_recover_not_negotiated(&err, debug.as_deref()) {
                            main_not_negotiated = true;
                        }
                    }
                    _ => {}
                }
            }
        }
        if main_not_negotiated {
            self.recover_main_pipeline_not_negotiated();
        }
        if let Some(abus) = self.audio_pipeline.bus() {
            while let Some(msg) = abus.pop() {
                if let gstreamer::MessageView::Element(e) = msg.view() {
                    if let Some(s) = e.structure() {
                        if s.name() == "level" {
                            if let Ok(peak) = s.get::<glib::ValueArray>("peak") {
                                let vals = peak.as_slice();
                                let l = vals
                                    .first()
                                    .and_then(|v| v.get::<f64>().ok())
                                    .unwrap_or(-60.0);
                                let r = vals.get(1).and_then(|v| v.get::<f64>().ok()).unwrap_or(l);
                                self.push_master_peak(l, r);
                                if let Some(track_index) = self.active_audio_track_index() {
                                    self.push_track_peak(track_index, l, r);
                                }
                            }
                        }
                    }
                }
            }
        }
        // Poll multi-audio pipeline bus for level messages.
        if let Some(ref mp) = self.audio_multi_pipeline {
            if let Some(bus) = mp.bus() {
                while let Some(msg) = bus.pop() {
                    if let gstreamer::MessageView::Element(e) = msg.view() {
                        if let Some(s) = e.structure() {
                            if s.name() == "level" {
                                if let Ok(peak) = s.get::<glib::ValueArray>("peak") {
                                    let vals = peak.as_slice();
                                    let l = vals
                                        .first()
                                        .and_then(|v| v.get::<f64>().ok())
                                        .unwrap_or(-60.0);
                                    let r =
                                        vals.get(1).and_then(|v| v.get::<f64>().ok()).unwrap_or(l);
                                    self.push_master_peak(l, r);
                                    // Route level to the specific track using the
                                    // encoded track index in the element name.
                                    if let Some(src) = msg.src() {
                                        let name = src.name().to_string();
                                        if let Some(ti) =
                                            Self::track_index_from_audio_level_element_name(&name)
                                        {
                                            self.push_track_peak(ti, l, r);
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
        eos
    }

    // ── Audio-only pipeline ────────────────────────────────────────────────

    fn should_rebuild_audio_multi_pipeline(
        active: &[usize],
        current_active: &[usize],
        pipeline_exists: bool,
    ) -> bool {
        active != current_active || !pipeline_exists
    }

    fn should_reseek_audio_multi_pipeline(
        active: &[usize],
        current_active: &[usize],
        pipeline_exists: bool,
        force_reseek: bool,
    ) -> bool {
        force_reseek && active == current_active && pipeline_exists
    }

    fn audio_multi_source_position_ns(clip: &ProgramClip, timeline_pos_ns: u64) -> u64 {
        if timeline_pos_ns >= clip.timeline_start_ns {
            clip.source_in_ns
                + (timeline_pos_ns - clip.timeline_start_ns)
                    .min(clip.source_out_ns.saturating_sub(clip.source_in_ns))
        } else {
            clip.source_in_ns
        }
    }

    fn audio_level_element_name(audio_clip_idx: usize, track_index: usize) -> String {
        format!("audiolevel_clip{audio_clip_idx}_track{track_index}")
    }

    fn track_index_from_audio_level_element_name(name: &str) -> Option<usize> {
        if let Some(idx_str) = name.strip_prefix("audiolevel_track") {
            return idx_str.parse::<usize>().ok();
        }
        name.rsplit_once("_track")
            .and_then(|(_, idx_str)| idx_str.parse::<usize>().ok())
    }

    fn reseek_audio_multi_pipeline(
        &mut self,
        active: &[usize],
        timeline_pos_ns: u64,
        force_reseek: bool,
    ) -> bool {
        if !Self::should_reseek_audio_multi_pipeline(
            active,
            &self.audio_multi_active,
            self.audio_multi_pipeline.is_some(),
            force_reseek,
        ) {
            return false;
        }
        let Some(ref pipeline) = self.audio_multi_pipeline else {
            return false;
        };
        let was_playing = self.state == PlayerState::Playing;
        let _ = pipeline.set_state(gst::State::Paused);
        let _ = pipeline.state(Some(gst::ClockTime::from_seconds(1)));
        // Reset the separate audio pipeline's segment/running-time before the
        // per-decoder reseeks. Without this flush, the audiomixer can keep the
        // old running-time and drop freshly reseeked external-audio buffers as
        // "late" until the next full audio boundary rebuild.
        let _ = pipeline.seek_simple(gst::SeekFlags::FLUSH, gst::ClockTime::ZERO);
        for &aidx in active {
            let Some(decoder) = self.audio_multi_decoders.get(&aidx) else {
                return false;
            };
            let Some(clip) = self.audio_clips.get(aidx) else {
                return false;
            };
            let source_pos_ns = Self::audio_multi_source_position_ns(clip, timeline_pos_ns);
            let _ = decoder.seek(
                1.0,
                gst::SeekFlags::FLUSH | gst::SeekFlags::ACCURATE,
                gst::SeekType::Set,
                gst::ClockTime::from_nseconds(source_pos_ns),
                gst::SeekType::Set,
                gst::ClockTime::from_nseconds(clip.source_out_ns),
            );
        }
        if was_playing {
            if let Some(base) = self.pipeline.base_time() {
                pipeline.set_base_time(base);
            }
            let _ = pipeline.set_state(gst::State::Playing);
        }
        true
    }

    /// Build a separate pipeline that mixes multiple audio-track clips.
    /// This pipeline is independent from the main compositor pipeline to
    /// avoid clock interference that causes video stuttering.
    fn rebuild_audio_multi_pipeline(&mut self, active: &[usize], timeline_pos_ns: u64) {
        if !Self::should_rebuild_audio_multi_pipeline(
            active,
            &self.audio_multi_active,
            self.audio_multi_pipeline.is_some(),
        ) {
            return;
        }
        self.teardown_audio_multi_pipeline();

        // Build pipeline string: multiple uridecodebin sources → audiomixer → autoaudiosink
        let pipeline = gst::Pipeline::builder().name("audio-multi").build();
        let mixer = match gst::ElementFactory::make("audiomixer")
            .property("ignore-inactive-pads", true)
            .build()
        {
            Ok(m) => m,
            Err(_) => return,
        };
        let conv_out = match gst::ElementFactory::make("audioconvert").build() {
            Ok(c) => c,
            Err(_) => return,
        };
        let sink = match gst::ElementFactory::make("autoaudiosink").build() {
            Ok(s) => s,
            Err(_) => return,
        };
        if pipeline.add_many([&mixer, &conv_out, &sink]).is_err() {
            return;
        }
        if gst::Element::link_many([&mixer, &conv_out, &sink]).is_err() {
            return;
        }

        // Collect per-clip info before building (to avoid borrow issues).
        struct AudioBranch {
            source_path: String,
            source_in_ns: u64,
            source_out_ns: u64,
            timeline_start_ns: u64,
            volume: f64,
            pan: f64,
            track_index: usize,
            audio_clip_idx: usize,
        }
        let branches: Vec<AudioBranch> = active
            .iter()
            .filter_map(|&aidx| {
                let clip = self.audio_clips.get(aidx)?;
                if !std::path::Path::new(&clip.source_path).exists() {
                    return None;
                }
                let vol = if self.master_muted {
                    0.0
                } else {
                    clip.volume_at_timeline_ns(timeline_pos_ns).clamp(0.0, 4.0)
                };
                let pan = clip.pan_at_timeline_ns(timeline_pos_ns);
                Some(AudioBranch {
                    source_path: clip.source_path.clone(),
                    source_in_ns: clip.source_in_ns,
                    source_out_ns: clip.source_out_ns,
                    timeline_start_ns: clip.timeline_start_ns,
                    volume: vol,
                    pan,
                    track_index: clip.track_index,
                    audio_clip_idx: aidx,
                })
            })
            .collect();

        let mut decoders: Vec<(gst::Element, AudioBranch)> = Vec::new();

        for branch in branches {
            let uri = format!("file://{}", branch.source_path);
            let audio_caps = gst::Caps::builder("audio/x-raw").build();
            let decoder = match gst::ElementFactory::make("uridecodebin")
                .property("uri", &uri)
                .property("caps", &audio_caps)
                .property("use-buffering", false)
                .build()
            {
                Ok(d) => d,
                Err(_) => continue,
            };
            let ac = match gst::ElementFactory::make("audioconvert").build() {
                Ok(c) => c,
                Err(_) => continue,
            };
            let mut audio_resample = None;
            let mut audio_capsfilter = None;
            // Optional EQ for this branch (skip when flat).
            let clip_ref = self.audio_clips.get(active[decoders.len()]);
            let needs_eq = clip_ref
                .map(|c| c.eq_bands.iter().any(|b| b.gain.abs() > 0.001))
                .unwrap_or(false);
            let eq_elem = if needs_eq {
                gst::ElementFactory::make("equalizer-nbands")
                    .property("num-bands", 3u32)
                    .build()
                    .ok()
            } else {
                None
            };
            if let (Some(ref eq), Some(clip)) = (&eq_elem, clip_ref) {
                for i in 0..3u32 {
                    let b = &clip.eq_bands[i as usize];
                    eq_set_band(eq, i, b.freq, b.gain, b.q);
                }
            }
            // Optional 7-band match EQ for this branch (skip when empty).
            let needs_match_eq = clip_ref
                .map(|c| !c.match_eq_bands.is_empty())
                .unwrap_or(false);
            let match_eq_elem = if needs_match_eq {
                let band_count = clip_ref.map(|c| c.match_eq_bands.len()).unwrap_or(0);
                gst::ElementFactory::make("equalizer-nbands")
                    .property("num-bands", band_count as u32)
                    .build()
                    .ok()
            } else {
                None
            };
            if let (Some(ref m_eq), Some(clip)) = (&match_eq_elem, clip_ref) {
                for (i, band) in clip.match_eq_bands.iter().enumerate() {
                    eq_set_band(m_eq, i as u32, band.freq, band.gain, band.q);
                }
            }
            // Optional Rubberband pitch shifter (LADSPA).
            let needs_pitch = clip_ref
                .map(|c| c.pitch_shift_semitones.abs() > 0.001)
                .unwrap_or(false);
            let rb_elem = if needs_pitch {
                gst::ElementFactory::make(
                    "ladspa-ladspa-rubberband-so-rubberband-pitchshifter-stereo",
                )
                .build()
                .ok()
                .or_else(|| {
                    // Fallback: try mono variant if stereo isn't available.
                    gst::ElementFactory::make(
                        "ladspa-ladspa-rubberband-so-rubberband-pitchshifter-mono",
                    )
                    .build()
                    .ok()
                })
            } else {
                None
            };
            if let (Some(ref rb), Some(clip)) = (&rb_elem, clip_ref) {
                let semitones = clip.pitch_shift_semitones.clamp(-12.0, 12.0);
                let whole = semitones.trunc() as i32;
                let cents = ((semitones - semitones.trunc()) * 100.0) as f32;
                rb.set_property("semitones", whole);
                rb.set_property("cents", cents);
            }

            // Name the level element uniquely per audio clip while preserving
            // the originating track index for per-track meter routing.
            let level_name =
                Self::audio_level_element_name(branch.audio_clip_idx, branch.track_index);
            let lv = gst::ElementFactory::make("level")
                .name(level_name.as_str())
                .property("post-messages", true)
                .property("interval", 50_000_000u64)
                .build()
                .ok();
            // Create LADSPA effect elements for this clip.
            let ladspa_elems: Vec<gst::Element> = clip_ref
                .map(|c| &c.ladspa_effects)
                .into_iter()
                .flatten()
                .filter(|e| e.enabled)
                .filter_map(|effect| {
                    let elem = gst::ElementFactory::make(&effect.gst_element_name)
                        .build()
                        .ok()?;
                    for (param, &val) in &effect.params {
                        if elem.find_property(param).is_some() {
                            elem.set_property_from_str(param, &val.to_string());
                        }
                    }
                    Some(elem)
                })
                .collect();

            if pipeline.add_many([&decoder, &ac]).is_err() {
                continue;
            }
            let ch_mode = clip_ref.map(|c| c.audio_channel_mode).unwrap_or_default();
            let mut link_src_pad = if let Some((resample, capsfilter, normalized_src)) =
                attach_preview_audio_normalizer_with_channel_mode(
                    &pipeline,
                    &ac,
                    ch_mode,
                    "audio_multi_pipeline",
                ) {
                audio_resample = Some(resample);
                audio_capsfilter = Some(capsfilter);
                Some(normalized_src)
            } else {
                log::warn!(
                    "audio_multi_pipeline: failed to normalize preview audio for clip_idx={} path={}, skipping branch",
                    branch.audio_clip_idx,
                    branch.source_path
                );
                pipeline.remove(&ac).ok();
                pipeline.remove(&decoder).ok();
                continue;
            };
            let mut elems: Vec<&gst::Element> = Vec::new();
            if let Some(ref m_eq) = match_eq_elem {
                elems.push(m_eq);
            }
            if let Some(ref eq) = eq_elem {
                elems.push(eq);
            }
            if let Some(ref rb) = rb_elem {
                elems.push(rb);
            }
            for le in &ladspa_elems {
                elems.push(le);
            }
            if let Some(ref l) = lv {
                elems.push(l);
            }
            if !elems.is_empty() && pipeline.add_many(elems.iter().copied()).is_err() {
                if let Some(ref l) = lv {
                    pipeline.remove(l).ok();
                }
                for le in &ladspa_elems {
                    pipeline.remove(le).ok();
                }
                if let Some(ref rb) = rb_elem {
                    pipeline.remove(rb).ok();
                }
                if let Some(ref eq) = eq_elem {
                    pipeline.remove(eq).ok();
                }
                if let Some(ref cf) = audio_capsfilter {
                    pipeline.remove(cf).ok();
                }
                if let Some(ref ar) = audio_resample {
                    pipeline.remove(ar).ok();
                }
                pipeline.remove(&ac).ok();
                pipeline.remove(&decoder).ok();
                continue;
            }
            // Audiopanorama for pan control (skip when centered).
            let pan_elem = if branch.pan.abs() > 0.001 {
                gst::ElementFactory::make("audiopanorama").build().ok()
            } else {
                None
            };
            if let Some(ref p) = pan_elem {
                elems.push(p);
                // Re-add to pipeline (elems already added above won't duplicate).
                let _ = pipeline.add(p);
            }

            // Link: audioconvert → [equalizer] → [rubberband] → [audiopanorama] → [level] → audiomixer pad.
            if let Some(ref eq) = eq_elem {
                if let (Some(prev), Some(eq_sink)) = (link_src_pad.clone(), eq.static_pad("sink")) {
                    let _ = prev.link(&eq_sink);
                    link_src_pad = eq.static_pad("src");
                }
            }
            if let Some(ref rb) = rb_elem {
                if let (Some(prev), Some(rb_sink)) = (link_src_pad.clone(), rb.static_pad("sink")) {
                    let _ = prev.link(&rb_sink);
                    link_src_pad = rb.static_pad("src");
                }
            }
            for le in &ladspa_elems {
                if let (Some(prev), Some(le_sink)) = (link_src_pad.clone(), le.static_pad("sink")) {
                    let _ = prev.link(&le_sink);
                    link_src_pad = le.static_pad("src");
                }
            }
            if let Some(ref p) = pan_elem {
                p.set_property("panorama", branch.pan as f32);
                if let (Some(prev), Some(p_sink)) = (link_src_pad.clone(), p.static_pad("sink")) {
                    let _ = prev.link(&p_sink);
                    link_src_pad = p.static_pad("src");
                }
            }
            if let Some(ref l) = lv {
                if let (Some(prev), Some(lv_sink)) = (link_src_pad.clone(), l.static_pad("sink")) {
                    let _ = prev.link(&lv_sink);
                    link_src_pad = l.static_pad("src");
                }
            }

            if let Some(pad) = mixer.request_pad_simple("sink_%u") {
                pad.set_property("volume", branch.volume);
                if let Some(src) = link_src_pad {
                    let _ = src.link(&pad);
                }
                // Store pad for live volume updates during playback.
                self.audio_multi_pads.insert(branch.audio_clip_idx, pad);
            }
            // Store pan element for live pan updates.
            if let Some(p) = pan_elem.clone() {
                self.audio_multi_pan_elems.insert(branch.audio_clip_idx, p);
            }

            let ac_for_cb = ac.clone();
            decoder.connect_pad_added(move |_, pad| {
                let caps = pad.current_caps().or_else(|| Some(pad.query_caps(None)));
                if let Some(caps) = caps {
                    if let Some(s) = caps.structure(0) {
                        if s.name().starts_with("audio/") {
                            if let Some(sink) = ac_for_cb.static_pad("sink") {
                                if !sink.is_linked() {
                                    let _ = pad.link(&sink);
                                }
                            }
                        }
                    }
                }
            });

            let _ = ac.sync_state_with_parent();
            if let Some(ref ar) = audio_resample {
                let _ = ar.sync_state_with_parent();
            }
            if let Some(ref cf) = audio_capsfilter {
                let _ = cf.sync_state_with_parent();
            }
            if let Some(ref eq) = eq_elem {
                let _ = eq.sync_state_with_parent();
            }
            if let Some(ref rb) = rb_elem {
                let _ = rb.sync_state_with_parent();
            }
            for le in &ladspa_elems {
                let _ = le.sync_state_with_parent();
            }
            if let Some(ref p) = pan_elem {
                let _ = p.sync_state_with_parent();
            }
            if let Some(ref l) = lv {
                let _ = l.sync_state_with_parent();
            }
            let _ = decoder.sync_state_with_parent();
            self.audio_multi_decoders
                .insert(branch.audio_clip_idx, decoder.clone());
            decoders.push((decoder, branch));
        }

        // Preroll the pipeline, then seek each decoder to its correct source position.
        // Slave the multi-audio pipeline to the main pipeline's clock so
        // audio and video stay in sync over long playback sessions.
        if let Some(clock) = self.pipeline.clock() {
            pipeline.use_clock(Some(&clock));
        }
        if let Some(base) = self.pipeline.base_time() {
            pipeline.set_base_time(base);
        }

        let _ = pipeline.set_state(gst::State::Paused);
        let _ = pipeline.state(Some(gst::ClockTime::from_seconds(2)));

        for (decoder, branch) in &decoders {
            // Compute the source position this clip should play from.
            let source_pos_ns = if let Some(clip) = self.audio_clips.get(branch.audio_clip_idx) {
                Self::audio_multi_source_position_ns(clip, timeline_pos_ns)
            } else {
                branch.source_in_ns
            };
            let _ = decoder.seek(
                1.0,
                gst::SeekFlags::FLUSH | gst::SeekFlags::ACCURATE,
                gst::SeekType::Set,
                gst::ClockTime::from_nseconds(source_pos_ns),
                gst::SeekType::Set,
                gst::ClockTime::from_nseconds(branch.source_out_ns),
            );
        }

        // Only start playing if the program player is actually in playback mode.
        if self.state == PlayerState::Playing {
            let _ = pipeline.set_state(gst::State::Playing);
        }

        log::info!(
            "audio_multi_pipeline: built with {} sources at pos={:.2}s",
            active.len(),
            timeline_pos_ns as f64 / 1e9
        );

        self.audio_multi_active = active.to_vec();
        self.audio_multi_pipeline = Some(pipeline);
    }

    fn teardown_audio_multi_pipeline(&mut self) {
        if let Some(ref pipeline) = self.audio_multi_pipeline {
            let _ = pipeline.set_state(gst::State::Null);
            let _ = pipeline.state(Some(gst::ClockTime::from_seconds(1)));
        }
        self.audio_multi_pipeline = None;
        self.audio_multi_active.clear();
        self.audio_multi_pads.clear();
        self.audio_multi_pan_elems.clear();
        self.audio_multi_decoders.clear();
    }

    fn audio_clip_at(&self, timeline_pos_ns: u64) -> Option<usize> {
        // Return the highest-track-index active clip (topmost audio track wins
        // when the single-playbin pipeline can only play one source at a time).
        self.audio_clips
            .iter()
            .enumerate()
            .filter(|(_, c)| {
                timeline_pos_ns >= c.timeline_start_ns && timeline_pos_ns < c.timeline_end_ns()
            })
            .max_by_key(|(_, c)| c.track_index)
            .map(|(i, _)| i)
    }

    /// Return all audio clip indices active at the given position.
    fn audio_clips_active_at(&self, timeline_pos_ns: u64) -> Vec<usize> {
        self.audio_clips
            .iter()
            .enumerate()
            .filter(|(_, c)| {
                timeline_pos_ns >= c.timeline_start_ns && timeline_pos_ns < c.timeline_end_ns()
            })
            .map(|(i, _)| i)
            .collect()
    }

    fn load_audio_clip_idx(&mut self, idx: usize, timeline_pos_ns: u64) {
        let clip = &self.audio_clips[idx];
        let source_seek_ns = clip.source_pos_ns(timeline_pos_ns);
        let vol =
            self.effective_audio_source_volume(AudioCurrentSource::AudioClip(idx), timeline_pos_ns);
        let pan =
            self.effective_audio_source_pan(AudioCurrentSource::AudioClip(idx), timeline_pos_ns);
        self.set_audio_pipeline_volume(vol);
        self.set_audio_pipeline_pan(pan);
        if self.audio_current_source == Some(AudioCurrentSource::AudioClip(idx)) {
            let _ = self.audio_pipeline.seek(
                clip.seek_rate(),
                clip.audio_seek_flags(),
                gst::SeekType::Set,
                gst::ClockTime::from_nseconds(clip.seek_start_ns(source_seek_ns)),
                gst::SeekType::Set,
                gst::ClockTime::from_nseconds(clip.seek_stop_ns(source_seek_ns)),
            );
        } else {
            let uri = format!("file://{}", clip.source_path);
            let _ = self.audio_pipeline.set_state(gst::State::Ready);
            self.audio_pipeline.set_property("uri", &uri);
            let _ = self.audio_pipeline.set_state(gst::State::Paused);
            // Wait for Paused so pads are linked before seeking.
            let _ = self
                .audio_pipeline
                .state(Some(gst::ClockTime::from_seconds(2)));
            let _ = self.audio_pipeline.seek(
                clip.seek_rate(),
                clip.audio_seek_flags(),
                gst::SeekType::Set,
                gst::ClockTime::from_nseconds(clip.seek_start_ns(source_seek_ns)),
                gst::SeekType::Set,
                gst::ClockTime::from_nseconds(clip.seek_stop_ns(source_seek_ns)),
            );
        }
        self.audio_current_source = Some(AudioCurrentSource::AudioClip(idx));
    }

    fn reverse_video_clip_at_for_audio(&mut self, timeline_pos_ns: u64) -> Option<usize> {
        let idx = self.clip_at(timeline_pos_ns)?;
        let clip = self.clips.get(idx)?.clone();
        if clip.is_audio_only || !clip.reverse || !clip.has_embedded_audio() {
            return None;
        }
        let effective_path = self.effective_source_path_for_clip(&clip);
        if self.probe_has_audio_stream_cached(&effective_path) {
            Some(idx)
        } else {
            None
        }
    }

    fn load_reverse_video_audio_clip_idx(&mut self, idx: usize, timeline_pos_ns: u64) {
        let clip = self.clips[idx].clone();
        let effective_path = self.effective_source_path_for_clip(&clip);
        let uri = format!("file://{}", effective_path);
        let source_seek_ns = clip.source_pos_ns(timeline_pos_ns);
        let vol = self.effective_audio_source_volume(
            AudioCurrentSource::ReverseVideoClip(idx),
            timeline_pos_ns,
        );
        let pan = self
            .effective_audio_source_pan(AudioCurrentSource::ReverseVideoClip(idx), timeline_pos_ns);
        self.set_audio_pipeline_volume(vol);
        self.set_audio_pipeline_pan(pan);
        if self.audio_current_source == Some(AudioCurrentSource::ReverseVideoClip(idx)) {
            let _ = self.audio_pipeline.seek(
                clip.seek_rate(),
                clip.audio_seek_flags(),
                gst::SeekType::Set,
                gst::ClockTime::from_nseconds(clip.seek_start_ns(source_seek_ns)),
                gst::SeekType::Set,
                gst::ClockTime::from_nseconds(clip.seek_stop_ns(source_seek_ns)),
            );
        } else {
            let _ = self.audio_pipeline.set_state(gst::State::Ready);
            self.audio_pipeline.set_property("uri", &uri);
            let _ = self.audio_pipeline.set_state(gst::State::Paused);
            let _ = self.audio_pipeline.seek(
                clip.seek_rate(),
                clip.audio_seek_flags(),
                gst::SeekType::Set,
                gst::ClockTime::from_nseconds(clip.seek_start_ns(source_seek_ns)),
                gst::SeekType::Set,
                gst::ClockTime::from_nseconds(clip.seek_stop_ns(source_seek_ns)),
            );
        }
        self.audio_current_source = Some(AudioCurrentSource::ReverseVideoClip(idx));
    }

    fn apply_reverse_video_main_audio_ducking(&mut self, reverse_video_audio_idx: Option<usize>) {
        self.reverse_video_ducked_clip_idx = reverse_video_audio_idx;
        self.apply_main_audio_slot_volumes(self.timeline_pos_ns);
    }

    fn force_sync_audio_to(&mut self, timeline_pos_ns: u64) {
        self.sync_audio_to_with_options(timeline_pos_ns, true);
    }

    fn sync_audio_to(&mut self, timeline_pos_ns: u64) {
        self.sync_audio_to_with_options(timeline_pos_ns, false);
    }

    fn sync_audio_to_with_options(&mut self, timeline_pos_ns: u64, force_multi_rebuild: bool) {
        let reverse_video_audio = self.reverse_video_clip_at_for_audio(timeline_pos_ns);
        self.apply_reverse_video_main_audio_ducking(reverse_video_audio);
        if let Some(idx) = reverse_video_audio {
            self.teardown_audio_multi_pipeline();
            self.load_reverse_video_audio_clip_idx(idx, timeline_pos_ns);
        } else {
            let active = self.audio_clips_active_at(timeline_pos_ns);
            if !active.is_empty() {
                // Use the multi-clip mixer for all audio-track playback (1 or more clips).
                let _ = self.audio_pipeline.set_state(gst::State::Ready);
                self.audio_current_source = None;
                if !self.reseek_audio_multi_pipeline(&active, timeline_pos_ns, force_multi_rebuild)
                {
                    if force_multi_rebuild {
                        self.teardown_audio_multi_pipeline();
                    }
                    self.rebuild_audio_multi_pipeline(&active, timeline_pos_ns);
                }
            } else {
                self.teardown_audio_multi_pipeline();
                let _ = self.audio_pipeline.set_state(gst::State::Ready);
                self.audio_current_source = None;
            }
        }
        self.sync_preview_audio_levels(timeline_pos_ns);
    }

    fn poll_audio(&mut self, timeline_pos_ns: u64) {
        // Check for reverse-video audio first (special case using the playbin).
        let reverse_idx = self.reverse_video_clip_at_for_audio(timeline_pos_ns);
        self.apply_reverse_video_main_audio_ducking(reverse_idx);
        if let Some(idx) = reverse_idx {
            // Reverse video audio uses the playbin path.
            if self.audio_multi_pipeline.is_some() {
                self.teardown_audio_multi_pipeline();
            }
            let wanted = AudioCurrentSource::ReverseVideoClip(idx);
            if self.audio_current_source != Some(wanted) {
                self.load_reverse_video_audio_clip_idx(idx, timeline_pos_ns);
                let _ = self.audio_pipeline.set_state(gst::State::Playing);
            }
            self.sync_preview_audio_levels(timeline_pos_ns);
            return;
        }

        // All audio-track clips use the multi-clip mixer pipeline.
        let active = self.audio_clips_active_at(timeline_pos_ns);
        if !active.is_empty() {
            // Ensure the playbin isn't playing stale audio.
            if self.audio_current_source.is_some() {
                let _ = self.audio_pipeline.set_state(gst::State::Ready);
                self.audio_current_source = None;
            }
            if active != self.audio_multi_active {
                self.rebuild_audio_multi_pipeline(&active, timeline_pos_ns);
            }
            self.sync_preview_audio_levels(timeline_pos_ns);
            return;
        }

        // No audio clips active — tear down multi pipeline if present.
        if self.audio_multi_pipeline.is_some() {
            self.teardown_audio_multi_pipeline();
        }
        if self.audio_current_source.is_some() {
            let _ = self.audio_pipeline.set_state(gst::State::Ready);
            self.audio_current_source = None;
        }
        self.sync_preview_audio_levels(timeline_pos_ns);

        // Legacy: keep the volume sync for any leftover audio state.
        if let Some(source) = self.audio_current_source {
            if self.master_muted {
                self.set_audio_pipeline_volume(0.0);
            } else {
                let volume = self.effective_audio_source_volume(source, timeline_pos_ns);
                self.set_audio_pipeline_volume(volume);
            }
        }
        self.sync_preview_audio_levels(timeline_pos_ns);
    }
}

/// Set a frei0r element property with correct GLib type conversion.
///
/// Frei0r parameters are stored as f64 in our model, but the actual GStreamer
/// property may be `gdouble`, `gboolean`, or `gchararray`. Setting a `gdouble`
/// on a `gboolean` property panics, so we inspect the property type first.
fn set_frei0r_property(elem: &gst::Element, param: &str, val: f64) {
    // Skip NaN/Inf — setting non-finite values panics in GStreamer.
    if !val.is_finite() {
        return;
    }
    let Some(pspec) = elem.find_property(param) else {
        return;
    };
    let vtype = pspec.value_type();
    // Wrap in catch_unwind: some frei0r plugins (e.g. cairogradient) have C-level
    // property setters that can panic or trigger GLib assertions for edge-case
    // values. Catching here prevents a single bad property from crashing the app.
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        if vtype == glib::Type::BOOL {
            elem.set_property(param, val > 0.5);
        } else if vtype == glib::Type::F64 {
            // Clamp to the property spec's actual range to avoid assertion failures.
            let clamped = if let Some(ps) = pspec.downcast_ref::<glib::ParamSpecDouble>() {
                let pmin = ps.minimum();
                let pmax = ps.maximum();
                if pmin.is_finite() && pmax.is_finite() && pmin < pmax {
                    val.clamp(pmin, pmax)
                } else {
                    val
                }
            } else {
                val
            };
            elem.set_property(param, clamped);
        } else if vtype == glib::Type::F32 {
            let clamped = if let Some(ps) = pspec.downcast_ref::<glib::ParamSpecFloat>() {
                let pmin = ps.minimum();
                let pmax = ps.maximum();
                if pmin.is_finite() && pmax.is_finite() && pmin < pmax {
                    (val as f32).clamp(pmin, pmax)
                } else {
                    val as f32
                }
            } else {
                val as f32
            };
            elem.set_property(param, clamped);
        } else if vtype == glib::Type::STRING {
            // String params are handled by set_frei0r_string_property.
        } else {
            // Unknown type (Object, etc.) — skip silently.
        }
    }));
    if let Err(e) = result {
        log::warn!(
            "set_frei0r_property: panic setting '{param}' = {val} — {:?}",
            e.downcast_ref::<String>()
                .map(|s| s.as_str())
                .unwrap_or("unknown")
        );
    }
}

fn set_frei0r_string_property(elem: &gst::Element, param: &str, val: &str) {
    let Some(pspec) = elem.find_property(param) else {
        return;
    };
    if pspec.value_type() == glib::Type::STRING {
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            elem.set_property(param, val);
        }));
        if let Err(e) = result {
            log::warn!(
                "set_frei0r_string_property: panic setting '{param}' = '{val}' — {:?}",
                e.downcast_ref::<String>()
                    .map(|s| s.as_str())
                    .unwrap_or("unknown")
            );
        }
    }
}

fn clip_can_fully_occlude(clip: &ProgramClip) -> bool {
    // Title clips use videotestsrc with a potentially transparent background
    // (default bg alpha=0) — they should never occlude lower tracks.
    !clip.is_title
        && clip.opacity >= 0.999
        && clip.opacity_keyframes.is_empty()
        && clip.scale >= 1.0
        && clip.scale_keyframes.is_empty()
        && clip.crop_left == 0
        && clip.crop_right == 0
        && clip.crop_top == 0
        && clip.crop_bottom == 0
        && clip.crop_left_keyframes.is_empty()
        && clip.crop_right_keyframes.is_empty()
        && clip.crop_top_keyframes.is_empty()
        && clip.crop_bottom_keyframes.is_empty()
        && clip.rotate.rem_euclid(360) == 0
        && clip.rotate_keyframes.is_empty()
        && !clip.flip_h
        && !clip.flip_v
        && clip.position_x_keyframes.is_empty()
        && clip.position_y_keyframes.is_empty()
        && clip.position_x.abs() < 0.000_001
        && clip.position_y.abs() < 0.000_001
}

#[cfg(test)]
mod tests {
    use super::{
        apply_adjustment_overlays_rgba, clip_can_fully_occlude, AdjustmentOverlay,
        CachedPlayheadFrame, ProgramClip, ProgramPlayer, ScopeFrame, ShortFrameCache,
        ThreePointParabola, TransitionPrerenderSpec, TransitionRole, VBParams, VideoSlot,
    };
    use crate::media::adjustment_scope::AdjustmentScopeShape;
    use crate::model::clip::{KeyframeInterpolation, MaskShape, NumericKeyframe};
    use crate::model::transition::TransitionAlignment;
    use crate::ui_state::{PrerenderEncodingPreset, DEFAULT_PRERENDER_CRF};
    use gstreamer as gst;
    use std::collections::hash_map::DefaultHasher;
    use std::collections::HashMap;
    use std::hash::Hasher;
    use std::sync::atomic::{AtomicBool, AtomicU64};
    use std::sync::Arc;
    use std::sync::Mutex;
    use std::time::Duration;

    #[test]
    fn test_equalizer_nbands_child_proxy_ffi() {
        use gst::prelude::*;
        gst::init().unwrap();
        let eq = gst::ElementFactory::make("equalizer-nbands")
            .property("num-bands", 3u32)
            .build()
            .expect("equalizer-nbands element should be available");
        assert_eq!(eq.property::<u32>("num-bands"), 3);
        // Verify child_proxy_get_child works
        for i in 0..3u32 {
            let band = super::child_proxy_get_child(&eq, i)
                .unwrap_or_else(|| panic!("child_proxy band {i} should not be null"));
            band.set_property("freq", 1000.0_f64);
            band.set_property("gain", 6.0_f64);
            band.set_property("bandwidth", 500.0_f64);
            let gain: f64 = band.property("gain");
            assert!((gain - 6.0).abs() < 0.001, "gain should be 6.0, got {gain}");
        }
        // Verify eq_set_band helper
        let bands = crate::model::clip::default_eq_bands();
        for (i, b) in bands.iter().enumerate() {
            super::eq_set_band(&eq, i as u32, b.freq, b.gain, b.q);
        }
        // Set a non-zero gain via helper and read it back
        super::eq_set_band(&eq, 0, 200.0, 10.0, 1.0);
        let band0 = super::child_proxy_get_child(&eq, 0).unwrap();
        let g: f64 = band0.property("gain");
        let f: f64 = band0.property("freq");
        assert!(
            (g - 10.0).abs() < 0.001,
            "band0 gain should be 10.0, got {g}"
        );
        assert!(
            (f - 200.0).abs() < 0.001,
            "band0 freq should be 200.0, got {f}"
        );
        // Test eq_set_band_gain
        super::eq_set_band_gain(&eq, 0, -12.0);
        let g2: f64 = band0.property("gain");
        assert!(
            (g2 - (-12.0)).abs() < 0.001,
            "band0 gain should be -12.0, got {g2}"
        );
    }

    #[test]
    fn content_inset_from_dimensions_reflects_selected_clip_aspect() {
        let base = ProgramPlayer::content_inset_from_dimensions(5320, 2280, 1920, 1080);
        let png = ProgramPlayer::content_inset_from_dimensions(1600, 844, 1920, 1080);

        assert_eq!(base.0, 0.0);
        assert_eq!(png.0, 0.0);
        assert!((base.1 - 0.119_047_619).abs() < 1e-6);
        assert!((png.1 - 0.031_111_111).abs() < 1e-6);
        assert!(
            png.1 < base.1,
            "selected PNG overlay should use its own inset, not the wider base clip's inset"
        );
    }

    #[test]
    fn smooth_single_slot_no_transition_preserves_backpressure() {
        // Single-slot playback must NOT enable drop-late: the leaky queue
        // removes backpressure, letting the compositor spin at thousands of
        // fps — QoS then drops nearly every frame, producing ~1-2 displayed
        // fps.  Backpressure naturally throttles the compositor to 30 fps.
        assert!(!ProgramPlayer::should_drop_late_for_playback_mode(
            true,
            false,
            &crate::ui_state::PlaybackPriority::Smooth,
            1,
            false,
        ));
    }

    #[test]
    fn smooth_transition_overlap_enables_drop_late() {
        assert!(ProgramPlayer::should_drop_late_for_playback_mode(
            true,
            false,
            &crate::ui_state::PlaybackPriority::Smooth,
            2,
            true,
        ));
    }

    #[test]
    fn balanced_single_slot_does_not_drop_late() {
        assert!(!ProgramPlayer::should_drop_late_for_playback_mode(
            true,
            false,
            &crate::ui_state::PlaybackPriority::Balanced,
            1,
            false,
        ));
    }

    #[test]
    fn accurate_three_slot_playback_still_drops_late() {
        assert!(ProgramPlayer::should_drop_late_for_playback_mode(
            true,
            false,
            &crate::ui_state::PlaybackPriority::Accurate,
            3,
            false,
        ));
    }

    #[test]
    fn explicit_audio_sync_reseeks_multi_pipeline_when_active_set_matches() {
        assert!(ProgramPlayer::should_reseek_audio_multi_pipeline(
            &[2],
            &[2],
            true,
            true,
        ));
        assert!(!ProgramPlayer::should_rebuild_audio_multi_pipeline(
            &[2],
            &[2],
            true,
        ));
    }

    #[test]
    fn poll_audio_reuses_multi_pipeline_when_active_set_matches() {
        assert!(!ProgramPlayer::should_rebuild_audio_multi_pipeline(
            &[2],
            &[2],
            true,
        ));
        assert!(!ProgramPlayer::should_reseek_audio_multi_pipeline(
            &[2],
            &[2],
            true,
            false,
        ));
    }

    #[test]
    fn poll_audio_rebuilds_multi_pipeline_when_active_set_changes() {
        assert!(ProgramPlayer::should_rebuild_audio_multi_pipeline(
            &[2],
            &[2, 3],
            true,
        ));
    }

    #[test]
    fn audio_level_element_names_are_unique_per_clip_and_parse_track_index() {
        let first = ProgramPlayer::audio_level_element_name(0, 1);
        let second = ProgramPlayer::audio_level_element_name(1, 1);

        assert_ne!(first, second);
        assert_eq!(
            ProgramPlayer::track_index_from_audio_level_element_name(&first),
            Some(1)
        );
        assert_eq!(
            ProgramPlayer::track_index_from_audio_level_element_name(&second),
            Some(1)
        );
        assert_eq!(
            ProgramPlayer::track_index_from_audio_level_element_name("audiolevel_track7"),
            Some(7)
        );
    }

    fn make_clip() -> ProgramClip {
        ProgramClip {
            id: "c1".to_string(),
            source_path: "/tmp/c1.mp4".to_string(),
            source_in_ns: 0,
            source_out_ns: 1_000_000_000,
            timeline_start_ns: 0,
            brightness: 0.0,
            contrast: 1.0,
            saturation: 1.0,
            temperature: 6500.0,
            tint: 0.0,
            brightness_keyframes: Vec::new(),
            contrast_keyframes: Vec::new(),
            saturation_keyframes: Vec::new(),
            temperature_keyframes: Vec::new(),
            tint_keyframes: Vec::new(),
            denoise: 0.0,
            sharpness: 0.0,
            blur: 0.0,
            blur_keyframes: Vec::new(),
            vidstab_enabled: false,
            vidstab_smoothing: 0.5,
            volume: 1.0,
            voice_isolation: 0.0,
            voice_enhance: false,
            voice_enhance_strength: 0.5,
            voice_isolation_pad_ns: 80_000_000,
            voice_isolation_fade_ns: 25_000_000,
            voice_isolation_floor: 0.0,
            volume_keyframes: Vec::new(),
            voice_isolation_merged_intervals_ns: Vec::new(),
            pan: 0.0,
            pan_keyframes: Vec::new(),
            audio_channel_mode: crate::model::clip::AudioChannelMode::default(),
            eq_bands: crate::model::clip::default_eq_bands(),
            eq_low_gain_keyframes: Vec::new(),
            eq_mid_gain_keyframes: Vec::new(),
            eq_high_gain_keyframes: Vec::new(),
            match_eq_bands: Vec::new(),
            rotate_keyframes: Vec::new(),
            crop_left_keyframes: Vec::new(),
            crop_right_keyframes: Vec::new(),
            crop_top_keyframes: Vec::new(),
            crop_bottom_keyframes: Vec::new(),
            crop_left: 0,
            crop_right: 0,
            crop_top: 0,
            crop_bottom: 0,
            rotate: 0,
            flip_h: false,
            flip_v: false,
            motion_blur_enabled: false,
            motion_blur_shutter_angle: 180.0,
            title_text: String::new(),
            title_font: String::new(),
            title_color: 0,
            title_x: 0.0,
            title_y: 0.0,
            speed: 1.0,
            speed_keyframes: Vec::new(),
            slow_motion_interp: crate::model::clip::SlowMotionInterp::Off,
            reverse: false,
            freeze_frame: false,
            freeze_frame_source_ns: None,
            freeze_frame_hold_duration_ns: None,
            is_audio_only: false,
            duck: false,
            duck_amount_db: -6.0,
            ladspa_effects: Vec::new(),
            pitch_shift_semitones: 0.0,
            pitch_preserve: false,
            track_index: 0,
            transition_after: String::new(),
            transition_after_ns: 0,
            transition_alignment: TransitionAlignment::EndOnCut,
            lut_paths: Vec::new(),
            scale: 1.0,
            scale_keyframes: Vec::new(),
            opacity: 1.0,
            opacity_keyframes: Vec::new(),
            blend_mode: crate::model::clip::BlendMode::Normal,
            position_x: 0.0,
            position_x_keyframes: Vec::new(),
            position_y: 0.0,
            position_y_keyframes: Vec::new(),
            shadows: 0.0,
            midtones: 0.0,
            highlights: 0.0,
            exposure: 0.0,
            black_point: 0.0,
            highlights_warmth: 0.0,
            highlights_tint: 0.0,
            midtones_warmth: 0.0,
            midtones_tint: 0.0,
            shadows_warmth: 0.0,
            shadows_tint: 0.0,
            has_audio: true,
            is_image: false,
            animated_svg: false,
            media_duration_ns: None,
            is_adjustment: false,
            chroma_key_enabled: false,
            chroma_key_color: 0x00FF00,
            chroma_key_tolerance: 0.3,
            chroma_key_softness: 0.1,
            bg_removal_enabled: false,
            bg_removal_threshold: 0.5,
            frei0r_effects: Vec::new(),
            tracking_binding: None,
            title_outline_color: 0x000000FF,
            title_outline_width: 0.0,
            title_shadow: false,
            title_shadow_color: 0x000000AA,
            title_shadow_offset_x: 2.0,
            title_shadow_offset_y: 2.0,
            title_bg_box: false,
            title_bg_box_color: 0x00000088,
            title_bg_box_padding: 8.0,
            title_clip_bg_color: 0,
            title_secondary_text: String::new(),
            is_title: false,
            anamorphic_desqueeze: 1.0,
            masks: Vec::new(),
            hsl_qualifier: None,
            title_animation: crate::model::clip::TitleAnimation::None,
            title_animation_duration_ns: 1_000_000_000,
            drawing_items: Vec::new(),
        }
    }

    fn make_scope_frame(seed: u8) -> ScopeFrame {
        ScopeFrame {
            data: vec![seed; 2 * 2 * 4],
            width: 2,
            height: 2,
        }
    }

    #[test]
    fn adjustment_overlay_masks_limit_preview_effect_area() {
        let width = 8usize;
        let height = 8usize;
        let mut data = vec![0u8; width * height * 4];
        for px in data.chunks_exact_mut(4) {
            px[3] = 255;
        }

        let mut mask = crate::model::clip::ClipMask::new(MaskShape::Rectangle);
        mask.enabled = true;
        mask.width = 0.2;
        mask.height = 0.2;

        let overlay = AdjustmentOverlay {
            track_index: 0,
            scope: AdjustmentScopeShape::from_transform(
                width as u32,
                height as u32,
                1.0,
                0.0,
                0.0,
                0.0,
                0,
                0,
                0,
                0,
            ),
            mask: crate::media::mask_alpha::prepare_adjustment_canvas_masks(
                &[mask],
                0,
                1.0,
                0.0,
                0.0,
                0.0,
            ),
            opacity: 1.0,
            vb_params: Some(VBParams {
                brightness: 0.25,
                contrast: 1.0,
                saturation: 1.0,
                hue: 0.0,
            }),
            coloradj_params: None,
            three_point: None,
            lut: None,
        };

        apply_adjustment_overlays_rgba(&mut data, width, height, &[overlay]);

        let center_idx = (4 * width + 4) * 4;
        let corner_idx = 0;
        assert!(
            data[center_idx] > 0,
            "masked center pixel should be adjusted"
        );
        assert_eq!(
            data[corner_idx], 0,
            "pixels outside the mask should stay unchanged"
        );
    }

    #[test]
    fn clip_active_window_extends_for_centered_transition_alignment() {
        let mut outgoing = make_clip();
        outgoing.id = "out".to_string();
        outgoing.source_out_ns = 5_000_000_000;
        outgoing.transition_after = "cross_dissolve".to_string();
        outgoing.transition_after_ns = 1_000_000_000;
        outgoing.transition_alignment = TransitionAlignment::CenterOnCut;

        let mut incoming = make_clip();
        incoming.id = "in".to_string();
        incoming.source_out_ns = 5_000_000_000;
        incoming.timeline_start_ns = 5_000_000_000;

        let clips = vec![outgoing, incoming];
        assert_eq!(
            ProgramPlayer::clip_active_window(&clips, 0),
            Some((0, 5_500_000_000))
        );
        assert_eq!(
            ProgramPlayer::clip_active_window(&clips, 1),
            Some((4_500_000_000, 10_000_000_000))
        );
        assert_eq!(
            ProgramPlayer::incoming_transition_window_for_clip(&clips, 1)
                .map(|(_, window)| window.before_cut_ns),
            Some(500_000_000)
        );
    }

    #[test]
    fn prerender_transition_tpad_filter_holds_outgoing_after_cut() {
        let spec = TransitionPrerenderSpec {
            outgoing_input: 0,
            incoming_input: 1,
            xfade_transition: "fade".to_string(),
            duration_ns: 1_000_000_000,
            before_cut_ns: 500_000_000,
            after_cut_ns: 500_000_000,
        };
        assert_eq!(
            ProgramPlayer::prerender_build_transition_tpad_filter(Some(&spec), 83_333_333, 0),
            ",tpad=stop_mode=clone:stop_duration=0.500000"
        );
        assert_eq!(
            ProgramPlayer::prerender_build_transition_tpad_filter(Some(&spec), 83_333_333, 1),
            ",tpad=start_duration=0.083333:start_mode=clone"
        );
    }

    #[test]
    fn not_negotiated_recovery_detector_matches_error_or_debug_text() {
        assert!(ProgramPlayer::should_recover_not_negotiated(
            "streaming stopped, reason not-negotiated (-4)",
            None
        ));
        assert!(ProgramPlayer::should_recover_not_negotiated(
            "Internal data stream error.",
            Some("...reason not-negotiated (-4)...")
        ));
        assert!(!ProgramPlayer::should_recover_not_negotiated(
            "Internal data stream error.",
            Some("...reason eos...")
        ));
    }

    #[test]
    fn transition_audio_delay_filter_applies_to_incoming_input() {
        let spec = TransitionPrerenderSpec {
            outgoing_input: 0,
            incoming_input: 1,
            xfade_transition: "fade".to_string(),
            duration_ns: 500_000_000,
            before_cut_ns: 500_000_000,
            after_cut_ns: 0,
        };
        let f = ProgramPlayer::prerender_build_transition_adelay_filter(Some(&spec), 83_333_333, 1);
        assert_eq!(f, ",adelay=84:all=1");
    }

    #[test]
    fn transition_audio_delay_filter_skips_non_incoming_or_zero_offset() {
        let spec = TransitionPrerenderSpec {
            outgoing_input: 0,
            incoming_input: 1,
            xfade_transition: "fade".to_string(),
            duration_ns: 500_000_000,
            before_cut_ns: 500_000_000,
            after_cut_ns: 0,
        };
        assert_eq!(
            ProgramPlayer::prerender_build_transition_adelay_filter(Some(&spec), 0, 1),
            ""
        );
        assert_eq!(
            ProgramPlayer::prerender_build_transition_adelay_filter(Some(&spec), 83_333_333, 0),
            ""
        );
    }

    #[test]
    fn prerender_title_lavfi_color_keeps_alpha_for_transparent_backgrounds() {
        let mut clip = make_clip();
        clip.is_title = true;
        clip.title_clip_bg_color = 0x00000000;

        let filter = ProgramPlayer::prerender_title_clip_lavfi_color(&clip, 1920, 1080, 24, 2.5);

        assert!(filter.contains("color=c=black@0.0"));
        assert!(filter.contains("format=yuva420p"));
    }

    #[test]
    fn prerender_title_lavfi_color_uses_opaque_format_for_filled_backgrounds() {
        let mut clip = make_clip();
        clip.is_title = true;
        clip.title_clip_bg_color = 0x112233CC;

        let filter = ProgramPlayer::prerender_title_clip_lavfi_color(&clip, 1920, 1080, 24, 2.5);

        assert!(filter.contains("color=c=#112233"));
        assert!(filter.contains("format=yuv420p"));
        assert!(!filter.contains("black@0.0"));
    }

    #[test]
    fn prerender_scale_position_filter_moves_title_at_unit_scale() {
        let mut clip = make_clip();
        clip.is_title = true;
        clip.position_x = 0.5;

        let filter = ProgramPlayer::prerender_build_scale_position_filter(&clip, 1920, 1080, true);

        assert!(filter.contains("scale=1920:1080"));
        assert!(filter.contains("crop=1440:1080:0:0"));
        assert!(filter.contains("pad=1920:1080:480:0:black@0"));
    }

    #[test]
    fn direct_canvas_translation_skips_untracked_images() {
        let mut clip = make_clip();
        clip.is_image = true;
        assert!(!ProgramPlayer::clip_uses_direct_canvas_translation(&clip));

        clip.tracking_binding = Some(crate::model::clip::TrackingBinding::new(
            "source-clip",
            "tracker-1",
        ));
        assert!(ProgramPlayer::clip_uses_direct_canvas_translation(&clip));
    }

    #[test]
    fn live_transform_refresh_targets_static_images_only() {
        let mut clip = make_clip();
        clip.is_image = true;
        assert!(ProgramPlayer::clip_requires_live_transform_refresh(&clip));

        clip.animated_svg = true;
        assert!(!ProgramPlayer::clip_requires_live_transform_refresh(&clip));

        clip.is_image = false;
        clip.is_title = true;
        clip.animated_svg = false;
        assert!(!ProgramPlayer::clip_requires_live_transform_refresh(&clip));
    }

    #[test]
    fn prerender_scale_position_filter_moves_tracked_clip_at_unit_scale() {
        let mut clip = make_clip();
        clip.position_x = 0.5;
        clip.tracking_binding = Some(crate::model::clip::TrackingBinding::new(
            "source-clip",
            "tracker-1",
        ));

        let filter = ProgramPlayer::prerender_build_scale_position_filter(&clip, 1920, 1080, false);

        assert!(filter.contains("scale=1920:1080"));
        assert!(filter.contains("crop=1440:1080:0:0"));
        assert!(filter.contains("pad=1920:1080:480:0:black"));
    }

    #[test]
    fn prerender_title_filter_uses_fontconfig_selector_for_bold_italic_fonts() {
        let mut clip = make_clip();
        clip.title_text = "Preview".to_string();
        clip.title_font = "Sans Bold Italic 36".to_string();

        let filter = ProgramPlayer::prerender_build_title_filter(&clip, 1080);

        assert!(
            filter.contains("drawtext=fontfile='")
                || filter.contains("font='Sans\\:weight=bold\\:slant=italic'")
        );
        assert!(filter.contains("fontsize=48.00"));
    }

    #[test]
    fn prerender_build_blur_filter_matches_export_mapping() {
        let mut clip = make_clip();
        clip.blur = 0.35;

        assert_eq!(
            ProgramPlayer::prerender_build_blur_filter(&clip),
            ",boxblur=4:4"
        );
    }

    #[test]
    fn prerender_build_color_filter_uses_eval_frame_when_keyframed() {
        let mut clip = make_clip();
        clip.timeline_start_ns = 2_000_000_000;
        clip.brightness_keyframes = vec![
            NumericKeyframe {
                time_ns: 0,
                value: -0.25,
                interpolation: KeyframeInterpolation::Linear,
                bezier_controls: None,
            },
            NumericKeyframe {
                time_ns: 1_000_000_000,
                value: 0.5,
                interpolation: KeyframeInterpolation::Linear,
                bezier_controls: None,
            },
        ];

        let filter = ProgramPlayer::prerender_build_color_filter(&clip);

        assert!(filter.contains("eq=brightness='if(lt(t,0.000000000),"));
        assert!(filter.contains("lt(t,1.000000000)"));
        assert!(filter.contains(":eval=frame"));
        assert!(!filter.contains("3.000000000"));
    }

    #[test]
    fn prerender_build_temperature_tint_filter_uses_eval_frame_when_keyframed() {
        let mut clip = make_clip();
        clip.temperature_keyframes = vec![
            NumericKeyframe {
                time_ns: 0,
                value: 3200.0,
                interpolation: KeyframeInterpolation::Linear,
                bezier_controls: None,
            },
            NumericKeyframe {
                time_ns: 1_000_000_000,
                value: 7800.0,
                interpolation: KeyframeInterpolation::Linear,
                bezier_controls: None,
            },
        ];
        clip.tint_keyframes = vec![
            NumericKeyframe {
                time_ns: 0,
                value: -0.5,
                interpolation: KeyframeInterpolation::Linear,
                bezier_controls: None,
            },
            NumericKeyframe {
                time_ns: 1_000_000_000,
                value: 0.5,
                interpolation: KeyframeInterpolation::Linear,
                bezier_controls: None,
            },
        ];

        let caps = crate::media::export::ColorFilterCapabilities {
            use_coloradj_frei0r: true,
            ..Default::default()
        };
        let filter = ProgramPlayer::prerender_build_temperature_tint_filter(&clip, &caps);

        assert!(filter.contains("colortemperature=temperature='if(lt(t,0.000000000),"));
        assert!(filter.contains("lt(t,1.000000000)"));
        assert!(filter.contains(",colorbalance=rm='("));
        assert!(filter.contains(":eval=frame"));
        assert!(!filter.contains("frei0r=filter_name=coloradj_RGB"));
    }

    #[test]
    fn prerender_build_flip_filter_handles_all_flip_modes() {
        let mut clip = make_clip();
        assert_eq!(ProgramPlayer::prerender_build_flip_filter(&clip), "");

        clip.flip_h = true;
        assert_eq!(ProgramPlayer::prerender_build_flip_filter(&clip), ",hflip");

        clip.flip_h = false;
        clip.flip_v = true;
        assert_eq!(ProgramPlayer::prerender_build_flip_filter(&clip), ",vflip");

        clip.flip_h = true;
        assert_eq!(
            ProgramPlayer::prerender_build_flip_filter(&clip),
            ",hflip,vflip"
        );
    }

    #[test]
    fn prerender_build_mask_alpha_uses_geq_for_shape_masks() {
        let mut clip = make_clip();
        let mut mask = crate::model::clip::ClipMask::new(crate::model::clip::MaskShape::Rectangle);
        mask.width = 0.2;
        mask.height = 0.15;
        clip.masks.push(mask);

        let mask_alpha = ProgramPlayer::prerender_build_mask_alpha(&clip, 1920, 1080);

        match mask_alpha {
            Some(crate::media::mask_alpha::FfmpegMaskAlphaResult::GeqExpression(expr)) => {
                assert!(expr.contains("between(") || expr.contains("clip("));
            }
            other => panic!("expected geq expression mask, got {other:?}"),
        }
    }

    #[test]
    fn prerender_build_mask_alpha_rasterizes_path_masks() {
        let mut clip = make_clip();
        clip.masks.push(crate::model::clip::ClipMask::new_path(vec![
            crate::model::clip::BezierPoint {
                x: 0.5,
                y: 0.2,
                handle_in_x: 0.0,
                handle_in_y: 0.0,
                handle_out_x: 0.0,
                handle_out_y: 0.0,
            },
            crate::model::clip::BezierPoint {
                x: 0.8,
                y: 0.5,
                handle_in_x: 0.0,
                handle_in_y: 0.0,
                handle_out_x: 0.0,
                handle_out_y: 0.0,
            },
            crate::model::clip::BezierPoint {
                x: 0.5,
                y: 0.8,
                handle_in_x: 0.0,
                handle_in_y: 0.0,
                handle_out_x: 0.0,
                handle_out_y: 0.0,
            },
            crate::model::clip::BezierPoint {
                x: 0.2,
                y: 0.5,
                handle_in_x: 0.0,
                handle_in_y: 0.0,
                handle_out_x: 0.0,
                handle_out_y: 0.0,
            },
        ]));

        let mask_alpha = ProgramPlayer::prerender_build_mask_alpha(&clip, 640, 360);

        match mask_alpha {
            Some(crate::media::mask_alpha::FfmpegMaskAlphaResult::RasterFile(file)) => {
                assert!(file.path().exists());
            }
            other => panic!("expected rasterized mask file, got {other:?}"),
        }
    }

    #[test]
    fn clip_has_phase1_keyframes_detects_mask_animation() {
        let mut clip = make_clip();
        let mut mask = crate::model::clip::ClipMask::new(crate::model::clip::MaskShape::Rectangle);
        mask.center_x_keyframes
            .push(crate::model::clip::NumericKeyframe {
                time_ns: 0,
                value: 0.4,
                interpolation: crate::model::clip::KeyframeInterpolation::Linear,
                bezier_controls: None,
            });
        clip.masks.push(mask);

        assert!(ProgramPlayer::clip_has_phase1_keyframes(&clip));
    }

    #[test]
    fn clip_has_unsupported_background_prerender_features_detects_speed() {
        let mut clip = make_clip();
        assert!(!ProgramPlayer::clip_has_unsupported_background_prerender_features(&clip));

        clip.speed = 1.25;
        assert!(ProgramPlayer::clip_has_unsupported_background_prerender_features(&clip));

        // Audio effects no longer block prerendering — audio is baked at
        // current levels and excluded from the signature.
        clip.speed = 1.0;
        clip.pan = 0.4;
        assert!(!ProgramPlayer::clip_has_unsupported_background_prerender_features(&clip));
    }

    #[test]
    fn clip_has_unsupported_background_prerender_features_allows_color_keyframes() {
        let mut clip = make_clip();
        clip.brightness_keyframes.push(NumericKeyframe {
            time_ns: 0,
            value: 0.25,
            interpolation: KeyframeInterpolation::Linear,
            bezier_controls: None,
        });
        clip.temperature_keyframes.push(NumericKeyframe {
            time_ns: 0,
            value: 7200.0,
            interpolation: KeyframeInterpolation::Linear,
            bezier_controls: None,
        });

        assert!(!ProgramPlayer::clip_has_unsupported_background_prerender_features(&clip));
    }

    #[test]
    fn clip_has_unsupported_background_prerender_features_allows_transform_keyframes() {
        // Phase 2 of background prerender preview: keyframed transforms
        // are now rendered via the multi-stream overlay chain in
        // `prerender_build_keyframed_overlay_tail`, so they no longer
        // disqualify a clip from prerender.
        let mut clip = make_clip();
        clip.position_x_keyframes.push(NumericKeyframe {
            time_ns: 0,
            value: 0.25,
            interpolation: KeyframeInterpolation::Linear,
            bezier_controls: None,
        });
        assert!(!ProgramPlayer::clip_has_unsupported_background_prerender_features(&clip));

        clip.position_x_keyframes.clear();
        clip.scale_keyframes.push(NumericKeyframe {
            time_ns: 0,
            value: 1.5,
            interpolation: KeyframeInterpolation::Linear,
            bezier_controls: None,
        });
        assert!(!ProgramPlayer::clip_has_unsupported_background_prerender_features(&clip));

        clip.scale_keyframes.clear();
        clip.rotate_keyframes.push(NumericKeyframe {
            time_ns: 0,
            value: 30.0,
            interpolation: KeyframeInterpolation::Linear,
            bezier_controls: None,
        });
        assert!(!ProgramPlayer::clip_has_unsupported_background_prerender_features(&clip));

        clip.rotate_keyframes.clear();
        clip.opacity_keyframes.push(NumericKeyframe {
            time_ns: 0,
            value: 0.5,
            interpolation: KeyframeInterpolation::Linear,
            bezier_controls: None,
        });
        assert!(!ProgramPlayer::clip_has_unsupported_background_prerender_features(&clip));
    }

    #[test]
    fn clip_has_unsupported_background_prerender_features_rejects_transform_keyframes_with_mask() {
        // Phase 2 limitation: animated transforms combined with a shape
        // mask are not yet handled by the keyframed-overlay chain, so
        // these clips fall back to the live path.
        let mut clip = make_clip();
        clip.position_x_keyframes.push(NumericKeyframe {
            time_ns: 0,
            value: 0.25,
            interpolation: KeyframeInterpolation::Linear,
            bezier_controls: None,
        });
        let mut mask =
            crate::model::clip::ClipMask::new(crate::model::clip::MaskShape::Rectangle);
        mask.enabled = true;
        clip.masks.push(mask);

        assert!(ProgramPlayer::clip_has_unsupported_background_prerender_features(&clip));
    }

    #[test]
    fn clip_has_unsupported_background_prerender_features_still_rejects_audio_keyframes() {
        // Audio keyframes are still unsupported in prerender — audio is
        // baked at current levels and audio-property changes do not
        // invalidate cached segments, so animated audio properties have
        // no place in the cache.
        let mut clip = make_clip();
        clip.volume_keyframes.push(NumericKeyframe {
            time_ns: 0,
            value: 0.5,
            interpolation: KeyframeInterpolation::Linear,
            bezier_controls: None,
        });
        assert!(ProgramPlayer::clip_has_unsupported_background_prerender_features(&clip));
    }

    #[test]
    fn prerender_clip_has_keyframed_overlay_detects_transform_lanes() {
        let mut clip = make_clip();
        assert!(!ProgramPlayer::prerender_clip_has_keyframed_overlay(&clip));

        clip.scale_keyframes.push(NumericKeyframe {
            time_ns: 0,
            value: 1.0,
            interpolation: KeyframeInterpolation::Linear,
            bezier_controls: None,
        });
        assert!(ProgramPlayer::prerender_clip_has_keyframed_overlay(&clip));

        clip.scale_keyframes.clear();
        clip.position_x_keyframes.push(NumericKeyframe {
            time_ns: 0,
            value: 0.0,
            interpolation: KeyframeInterpolation::Linear,
            bezier_controls: None,
        });
        assert!(ProgramPlayer::prerender_clip_has_keyframed_overlay(&clip));

        clip.position_x_keyframes.clear();
        clip.opacity_keyframes.push(NumericKeyframe {
            time_ns: 0,
            value: 1.0,
            interpolation: KeyframeInterpolation::Linear,
            bezier_controls: None,
        });
        assert!(ProgramPlayer::prerender_clip_has_keyframed_overlay(&clip));

        clip.opacity_keyframes.clear();
        // Rotate keyframes are handled by `prerender_build_rotation_filter`
        // directly (inline expression), not by the multi-stream overlay
        // chain, so they're NOT in the overlay-detection helper.
        clip.rotate_keyframes.push(NumericKeyframe {
            time_ns: 0,
            value: 10.0,
            interpolation: KeyframeInterpolation::Linear,
            bezier_controls: None,
        });
        assert!(!ProgramPlayer::prerender_clip_has_keyframed_overlay(&clip));
    }

    #[test]
    fn prerender_build_keyframed_overlay_tail_emits_eval_frame_chain() {
        let mut clip = make_clip();
        clip.scale_keyframes.push(NumericKeyframe {
            time_ns: 0,
            value: 1.0,
            interpolation: KeyframeInterpolation::Linear,
            bezier_controls: None,
        });
        clip.scale_keyframes.push(NumericKeyframe {
            time_ns: 1_000_000_000,
            value: 1.5,
            interpolation: KeyframeInterpolation::Linear,
            bezier_controls: None,
        });
        clip.position_x_keyframes.push(NumericKeyframe {
            time_ns: 0,
            value: 0.0,
            interpolation: KeyframeInterpolation::Linear,
            bezier_controls: None,
        });
        clip.position_x_keyframes.push(NumericKeyframe {
            time_ns: 1_000_000_000,
            value: 0.5,
            interpolation: KeyframeInterpolation::Linear,
            bezier_controls: None,
        });

        let tail = ProgramPlayer::prerender_build_keyframed_overlay_tail(
            &clip, "pv0kf", "pv0", 1920, 1080, 30, 1, false, true, "",
        );
        assert!(tail.contains("[pv0kf]scale=w='max(1,1920*("), "got: {tail}");
        // Intermediate fg/bg/raw labels are derived from the output label.
        assert!(tail.contains(":eval=frame[pv0fg]"), "got: {tail}");
        assert!(
            tail.contains("color=c=black:size=1920x1080:r=30/1"),
            "opaque bg expected for primary track: {tail}"
        );
        assert!(tail.contains("[pv0bg][pv0fg]overlay="));
        assert!(tail.contains("eval=frame,geq="));
        assert!(tail.contains("[pv0raw]"));
        // Format conversion is applied via a `format=yuv420p` filter
        // (not a leading comma after `]raw`, which would be invalid).
        assert!(tail.contains("[pv0raw]format=yuv420p[pv0]"));
        assert!(tail.ends_with("[pv0]"), "should end with output label: {tail}");
    }

    #[test]
    fn prerender_build_keyframed_overlay_tail_strips_post_tail_leading_comma() {
        // When post_tail starts with `,` (because motion_blur_filter or
        // minterpolate_filter were spliced in), it must be stripped so the
        // resulting chain doesn't have an invalid `[label],filter` sequence.
        let mut clip = make_clip();
        clip.scale_keyframes.push(NumericKeyframe {
            time_ns: 0,
            value: 1.0,
            interpolation: KeyframeInterpolation::Linear,
            bezier_controls: None,
        });
        let tail = ProgramPlayer::prerender_build_keyframed_overlay_tail(
            &clip,
            "pv0kf",
            "pv0",
            1920,
            1080,
            30,
            1,
            false,
            true,
            ",tmix=frames=2:weights='1 1'",
        );
        assert!(
            tail.contains("[pv0raw]format=yuv420p,tmix=frames=2:weights='1 1'[pv0]"),
            "got: {tail}"
        );
        assert!(!tail.contains("[pv0raw],format"), "leading comma not stripped: {tail}");
    }

    #[test]
    fn prerender_build_keyframed_overlay_tail_uses_transparent_bg_for_overlays() {
        let mut clip = make_clip();
        clip.position_y_keyframes.push(NumericKeyframe {
            time_ns: 0,
            value: 0.0,
            interpolation: KeyframeInterpolation::Linear,
            bezier_controls: None,
        });
        let tail = ProgramPlayer::prerender_build_keyframed_overlay_tail(
            &clip, "pv1kf", "pv1", 1920, 1080, 30, 1, true, false, "",
        );
        assert!(
            tail.contains("color=c=black@0:size=1920x1080:r=30/1"),
            "transparent bg expected for overlay track: {tail}"
        );
        assert!(!tail.contains("format=yuv420p"));
    }

    #[test]
    fn transition_effective_priority_prefers_nearer_boundary_for_equal_risk() {
        let lookahead_ns = 10_000_000_000;
        let near = ProgramPlayer::transition_prerender_effective_priority_score(
            1500,
            5_000_000_000,
            6_000_000_000,
            lookahead_ns,
        );
        let far = ProgramPlayer::transition_prerender_effective_priority_score(
            1500,
            5_000_000_000,
            14_000_000_000,
            lookahead_ns,
        );
        assert!(near > far);
    }

    #[test]
    fn transition_effective_priority_still_respects_large_risk_gap() {
        let lookahead_ns = 10_000_000_000;
        let low_risk_near = ProgramPlayer::transition_prerender_effective_priority_score(
            300,
            5_000_000_000,
            5_500_000_000,
            lookahead_ns,
        );
        let high_risk_far = ProgramPlayer::transition_prerender_effective_priority_score(
            7000,
            5_000_000_000,
            14_500_000_000,
            lookahead_ns,
        );
        assert!(high_risk_far > low_risk_near);
    }

    #[test]
    fn transition_metric_decay_halves_counts_rounding_up() {
        let mut metrics: HashMap<String, (u64, u64)> = HashMap::new();
        metrics.insert("cross_dissolve".to_string(), (11, 4));
        metrics.insert("fade_to_black".to_string(), (2, 1));
        let (hits, misses) = ProgramPlayer::apply_transition_metric_decay(&mut metrics);
        assert_eq!(metrics.get("cross_dissolve"), Some(&(6, 2)));
        assert_eq!(metrics.get("fade_to_black"), Some(&(1, 1)));
        assert_eq!(hits, 7);
        assert_eq!(misses, 3);
    }

    #[test]
    fn transition_metric_decay_preserves_singleton_counts() {
        assert_eq!(ProgramPlayer::decay_transition_metric_count(0), 0);
        assert_eq!(ProgramPlayer::decay_transition_metric_count(1), 1);
    }

    #[test]
    fn transition_preview_mask_expands_circle_open() {
        let mask = super::transition_preview_mask("circle_open", TransitionRole::Incoming, 0.5)
            .expect("circle_open should build a preview mask");
        assert_eq!(mask.shape, MaskShape::Ellipse);
        assert!(!mask.invert);
        assert!((mask.width - mask.height).abs() < 1e-9);
        assert!(
            (mask.width - super::TRANSITION_FULL_FRAME_CIRCLE_RADIUS * 0.5).abs() < 1e-9,
            "unexpected circle_open radius: {}",
            mask.width
        );
    }

    #[test]
    fn transition_preview_mask_inverts_circle_close() {
        let mask = super::transition_preview_mask("circle_close", TransitionRole::Incoming, 0.25)
            .expect("circle_close should build a preview mask");
        assert_eq!(mask.shape, MaskShape::Ellipse);
        assert!(mask.invert);
        assert!(
            (mask.width - super::TRANSITION_FULL_FRAME_CIRCLE_RADIUS * 0.75).abs() < 1e-9,
            "unexpected circle_close radius: {}",
            mask.width
        );
    }

    #[test]
    fn transition_preview_canvas_offset_moves_cover_left_incoming_from_right() {
        let (x, y) =
            super::transition_preview_canvas_offset("cover_left", TransitionRole::Incoming, 0.25)
                .expect("cover_left should offset the incoming slot");
        assert!(
            (x - 0.75).abs() < 1e-9,
            "unexpected cover_left x offset: {x}"
        );
        assert!(
            (y - 0.0).abs() < 1e-9,
            "unexpected cover_left y offset: {y}"
        );
    }

    #[test]
    fn transition_preview_canvas_offset_moves_reveal_down_outgoing_downward() {
        let (x, y) =
            super::transition_preview_canvas_offset("reveal_down", TransitionRole::Outgoing, 0.5)
                .expect("reveal_down should offset the outgoing slot");
        assert!(
            (x - 0.0).abs() < 1e-9,
            "unexpected reveal_down x offset: {x}"
        );
        assert!(
            (y - 0.5).abs() < 1e-9,
            "unexpected reveal_down y offset: {y}"
        );
    }

    #[test]
    fn transition_preview_canvas_offset_moves_slide_up_both_slots() {
        let (incoming_x, incoming_y) =
            super::transition_preview_canvas_offset("slide_up", TransitionRole::Incoming, 0.4)
                .expect("slide_up incoming should offset");
        let (outgoing_x, outgoing_y) =
            super::transition_preview_canvas_offset("slide_up", TransitionRole::Outgoing, 0.4)
                .expect("slide_up outgoing should offset");
        assert!(
            (incoming_x - 0.0).abs() < 1e-9 && (incoming_y - 0.6).abs() < 1e-9,
            "unexpected slide_up incoming offset: ({incoming_x}, {incoming_y})"
        );
        assert!(
            (outgoing_x - 0.0).abs() < 1e-9 && (outgoing_y + 0.4).abs() < 1e-9,
            "unexpected slide_up outgoing offset: ({outgoing_x}, {outgoing_y})"
        );
    }

    #[test]
    fn transition_preview_canvas_offset_ignores_wipes() {
        assert!(super::transition_preview_canvas_offset(
            "wipe_right",
            TransitionRole::Incoming,
            0.5
        )
        .is_none());
    }

    #[test]
    fn build_effects_bin_keeps_live_mask_stage_without_authored_masks() {
        use gst::prelude::*;

        let _ = gst::init();
        let clip = make_clip();
        let mask_shared = Arc::new(Mutex::new(Vec::new()));
        let hsl_shared = Arc::new(Mutex::new(None));
        let (effects_bin, ..) = ProgramPlayer::build_effects_bin(
            &clip, false, 640, 360, 640, 360, None, mask_shared, hsl_shared,
        );
        assert!(
            effects_bin.by_name("us-mask-identity").is_some(),
            "effects bin should keep the live mask stage even when clip.masks is empty"
        );
    }

    #[test]
    fn fade_to_white_switches_preview_background() {
        let states = vec![
            Some(super::TransitionState {
                kind: "cross_dissolve".to_string(),
                progress: 0.25,
                role: TransitionRole::Outgoing,
            }),
            Some(super::TransitionState {
                kind: "fade_to_white".to_string(),
                progress: 0.5,
                role: TransitionRole::Incoming,
            }),
        ];
        assert_eq!(
            super::transition_preview_background_pattern(&states),
            "white"
        );
        assert_eq!(
            super::transition_preview_background_pattern(&[None]),
            "black"
        );
    }

    #[test]
    fn prerender_queue_admits_low_load_requests() {
        assert!(ProgramPlayer::prerender_queue_allows_request(
            3,
            Some(400),
            350
        ));
    }

    #[test]
    fn prerender_queue_rejects_full_queue_without_clear_priority_gain() {
        assert!(!ProgramPlayer::prerender_queue_allows_request(
            6,
            Some(900),
            1_050
        ));
        assert!(!ProgramPlayer::prerender_queue_allows_request(
            8,
            Some(100),
            10_000
        ));
    }

    #[test]
    fn prerender_queue_allows_limited_overflow_for_high_priority_requests() {
        assert!(ProgramPlayer::prerender_queue_allows_request(
            6,
            Some(700),
            1_100
        ));
        assert!(ProgramPlayer::prerender_queue_allows_request(
            7,
            Some(700),
            1_100
        ));
    }

    #[test]
    fn segment_distance_is_zero_inside_segment() {
        assert_eq!(
            ProgramPlayer::segment_distance_to_timeline_ns(2_500, 2_000, 3_000),
            0
        );
    }

    #[test]
    fn segment_distance_handles_before_and_after_positions() {
        assert_eq!(
            ProgramPlayer::segment_distance_to_timeline_ns(1_500, 2_000, 3_000),
            500
        );
        assert_eq!(
            ProgramPlayer::segment_distance_to_timeline_ns(3_500, 2_000, 3_000),
            500
        );
    }

    #[test]
    fn prerender_lut_filter_skips_when_source_is_proxy() {
        let mut clip = make_clip();
        let lut_path = format!(
            "/tmp/ultimateslice-prerender-lut-{}-{}.cube",
            std::process::id(),
            clip.id
        );
        std::fs::write(&lut_path, "LUT_3D_SIZE 2\n0 0 0\n1 1 1\n").expect("write LUT test file");
        clip.lut_paths = vec![lut_path.clone()];
        let with_original = ProgramPlayer::prerender_build_lut_filter(&clip, false);
        let with_proxy = ProgramPlayer::prerender_build_lut_filter(&clip, true);
        assert!(with_original.contains("lut3d="));
        assert_eq!(with_proxy, "");
        let _ = std::fs::remove_file(lut_path);
    }

    #[test]
    fn prerender_meter_track_indices_returns_unique_track_order() {
        let mut c1 = make_clip();
        c1.id = "a".to_string();
        c1.track_index = 0;
        let mut c2 = make_clip();
        c2.id = "b".to_string();
        c2.track_index = 2;
        let mut c3 = make_clip();
        c3.id = "c".to_string();
        c3.track_index = 2;
        let clips = vec![c1, c2, c3];
        assert_eq!(
            ProgramPlayer::prerender_meter_track_indices_for(&clips, &[0, 1, 2]),
            vec![0, 2]
        );
    }

    fn cache_entry(
        frame_pos_ns: u64,
        signature: u64,
        scope_seq: u64,
        seed: u8,
    ) -> CachedPlayheadFrame {
        CachedPlayheadFrame {
            frame_pos_ns,
            signature,
            scope_seq,
            frame: make_scope_frame(seed),
        }
    }

    #[test]
    fn short_frame_cache_tracks_previous_current_next_neighbors() {
        let mut cache = ShortFrameCache::default();
        cache.store_current(cache_entry(100, 1, 1, 10));
        assert_eq!(cache.previous.as_ref().map(|e| e.frame_pos_ns), None);
        assert_eq!(cache.current.as_ref().map(|e| e.frame_pos_ns), Some(100));
        assert_eq!(cache.next.as_ref().map(|e| e.frame_pos_ns), None);

        cache.store_current(cache_entry(120, 2, 2, 11));
        assert_eq!(cache.previous.as_ref().map(|e| e.frame_pos_ns), Some(100));
        assert_eq!(cache.current.as_ref().map(|e| e.frame_pos_ns), Some(120));
        assert_eq!(cache.next.as_ref().map(|e| e.frame_pos_ns), None);

        cache.store_current(cache_entry(110, 3, 3, 12));
        assert_eq!(cache.previous.as_ref().map(|e| e.frame_pos_ns), Some(100));
        assert_eq!(cache.current.as_ref().map(|e| e.frame_pos_ns), Some(110));
        assert_eq!(cache.next.as_ref().map(|e| e.frame_pos_ns), Some(120));
    }

    #[test]
    fn short_frame_cache_lookup_respects_signature() {
        let mut cache = ShortFrameCache::default();
        cache.store_current(cache_entry(200, 5, 7, 20));
        assert!(cache.lookup(200, 5));
        assert!(!cache.lookup(200, 6));
        assert_eq!(cache.hits, 1);
        assert_eq!(cache.misses, 1);
    }

    #[test]
    fn short_frame_cache_clear_tracks_invalidations() {
        let mut cache = ShortFrameCache::default();
        cache.store_current(cache_entry(250, 9, 11, 30));
        assert!(cache.clear());
        assert_eq!(cache.invalidations, 1);
        assert!(!cache.clear());
        assert_eq!(cache.invalidations, 1);
    }

    #[test]
    fn clip_with_default_full_frame_transform_can_occlude() {
        let clip = make_clip();
        assert!(clip_can_fully_occlude(&clip));
    }

    #[test]
    fn clip_with_non_center_position_cannot_occlude() {
        let mut clip = make_clip();
        clip.position_x = 0.2;
        assert!(!clip_can_fully_occlude(&clip));
    }

    #[test]
    fn clip_with_rotation_or_flip_cannot_occlude() {
        let mut clip = make_clip();
        clip.rotate = 90;
        assert!(!clip_can_fully_occlude(&clip));
        clip.rotate = 0;
        clip.flip_h = true;
        assert!(!clip_can_fully_occlude(&clip));
    }

    #[test]
    fn clip_with_keyframed_position_cannot_occlude() {
        let mut clip = make_clip();
        clip.position_x_keyframes.push(NumericKeyframe {
            time_ns: 0,
            value: 0.0,
            interpolation: KeyframeInterpolation::Linear,
            bezier_controls: None,
        });
        clip.position_x_keyframes.push(NumericKeyframe {
            time_ns: 1_000_000_000,
            value: 0.5,
            interpolation: KeyframeInterpolation::Linear,
            bezier_controls: None,
        });
        assert!(!clip_can_fully_occlude(&clip));
    }

    #[test]
    fn program_clip_keyframed_volume_uses_clip_timeline_space() {
        let mut clip = make_clip();
        clip.timeline_start_ns = 2_000_000_000;
        clip.volume_keyframes = vec![
            NumericKeyframe {
                time_ns: 0,
                value: 0.0,
                interpolation: KeyframeInterpolation::Linear,
                bezier_controls: None,
            },
            NumericKeyframe {
                time_ns: 1_000_000_000,
                value: 1.0,
                interpolation: KeyframeInterpolation::Linear,
                bezier_controls: None,
            },
        ];
        let vol = clip.volume_at_timeline_ns(2_500_000_000);
        assert!((vol - 0.5).abs() < 1e-9);
    }

    #[test]
    fn program_clip_keyframed_temperature_uses_clip_timeline_space() {
        let mut clip = make_clip();
        clip.timeline_start_ns = 2_000_000_000;
        clip.temperature_keyframes = vec![
            NumericKeyframe {
                time_ns: 0,
                value: 4000.0,
                interpolation: KeyframeInterpolation::Linear,
                bezier_controls: None,
            },
            NumericKeyframe {
                time_ns: 1_000_000_000,
                value: 8000.0,
                interpolation: KeyframeInterpolation::Linear,
                bezier_controls: None,
            },
        ];
        let temp = clip.temperature_at_timeline_ns(2_500_000_000);
        assert!((temp - 6000.0).abs() < 1e-9);
    }

    #[test]
    fn three_keyframe_position_x_interpolates_all_segments() {
        // Matches project.uspxml C0378 clip: offset=8s, duration=6.25s,
        // 3 position_x keyframes at 0.618s, 2.130s, 4.709s local time.
        let mut clip = make_clip();
        clip.source_in_ns = 0;
        clip.source_out_ns = 6_250_000_000;
        clip.timeline_start_ns = 8_000_000_000;
        clip.position_x = 0.0;
        clip.position_x_keyframes = vec![
            NumericKeyframe {
                time_ns: 617_642_015,
                value: -0.82,
                interpolation: KeyframeInterpolation::Linear,
                bezier_controls: None,
            },
            NumericKeyframe {
                time_ns: 2_129_974_732,
                value: -0.82,
                interpolation: KeyframeInterpolation::Linear,
                bezier_controls: None,
            },
            NumericKeyframe {
                time_ns: 4_709_284_968,
                value: 0.67,
                interpolation: KeyframeInterpolation::Linear,
                bezier_controls: None,
            },
        ];

        // Before first keyframe: hold first keyframe value
        let v = clip.position_x_at_timeline_ns(8_000_000_000); // local=0
        assert!((v - (-0.82)).abs() < 1e-6, "before first kf: {v}");

        // At first keyframe
        let v = clip.position_x_at_timeline_ns(8_617_642_015); // local=0.618s
        assert!((v - (-0.82)).abs() < 1e-6, "at kf1: {v}");

        // Between kf1 and kf2 (same value, should stay at -0.82)
        let v = clip.position_x_at_timeline_ns(9_500_000_000); // local=1.5s
        assert!((v - (-0.82)).abs() < 1e-6, "between kf1-kf2: {v}");

        // Between kf2 and kf3 (should interpolate -0.82 → 0.67)
        let mid_local = (2_129_974_732 + 4_709_284_968) / 2; // ~3.42s
        let v = clip.position_x_at_timeline_ns(8_000_000_000 + mid_local);
        let expected = (-0.82 + 0.67) / 2.0;
        assert!(
            (v - expected).abs() < 0.01,
            "midpoint kf2-kf3: got {v}, expected ~{expected}"
        );

        // At third keyframe
        let v = clip.position_x_at_timeline_ns(8_000_000_000 + 4_709_284_968);
        assert!((v - 0.67).abs() < 1e-6, "at kf3: {v}");

        // After last keyframe: hold last value
        let v = clip.position_x_at_timeline_ns(14_000_000_000); // local=6s
        assert!((v - 0.67).abs() < 1e-6, "after kf3: {v}");
    }

    #[test]
    fn freeze_frame_holds_source_position_for_preview_timing() {
        let mut clip = make_clip();
        clip.source_in_ns = 2_000_000_000;
        clip.source_out_ns = 6_000_000_000;
        clip.timeline_start_ns = 1_000_000_000;
        clip.speed = 2.0;
        clip.freeze_frame = true;
        clip.freeze_frame_source_ns = Some(4_200_000_000);
        clip.freeze_frame_hold_duration_ns = Some(5_000_000_000);

        assert_eq!(clip.duration_ns(), 5_000_000_000);
        assert_eq!(clip.timeline_end_ns(), 6_000_000_000);
        assert_eq!(clip.source_pos_ns(1_000_000_000), 4_200_000_000);
        assert_eq!(clip.source_pos_ns(5_999_000_000), 4_200_000_000);
    }

    #[test]
    fn freeze_frame_seek_uses_single_frame_window() {
        let mut clip = make_clip();
        clip.source_in_ns = 100;
        clip.source_out_ns = 1_000;
        clip.freeze_frame = true;
        clip.freeze_frame_source_ns = Some(2_000);

        let source = clip.source_pos_ns(0);
        assert_eq!(source, 999);
        assert_eq!(clip.seek_rate(), 1.0);
        assert_eq!(clip.seek_start_ns(source), source);
        assert_eq!(clip.seek_stop_ns(source), source + 1);
    }

    #[test]
    fn freeze_frame_preview_seek_stop_uses_frame_duration_window() {
        let mut clip = make_clip();
        clip.source_in_ns = 100;
        clip.source_out_ns = 1_000;
        clip.freeze_frame = true;
        clip.freeze_frame_source_ns = Some(2_000);
        let slot = make_test_slot(false, false, false, false, false, false);

        let source = clip.source_pos_ns(0);
        let stop = ProgramPlayer::effective_video_seek_stop_ns(&slot, &clip, source, 41_666_667);
        assert_eq!(stop, source + 41_666_667);
    }

    #[test]
    fn freeze_frame_preview_seek_stop_clamps_to_minimum_window() {
        let mut clip = make_clip();
        clip.freeze_frame = true;
        let slot = make_test_slot(false, false, false, false, false, false);
        let source = clip.source_pos_ns(0);
        let stop = ProgramPlayer::effective_video_seek_stop_ns(&slot, &clip, source, 0);
        assert_eq!(stop, source + 1);
    }

    #[test]
    fn effective_source_pos_pins_static_images_to_source_in() {
        let mut clip = make_clip();
        clip.is_image = true;
        clip.source_in_ns = 123_000_000;
        clip.source_out_ns = 4_123_000_000;

        let slot = make_test_slot_ext(false, false, false, false, false, false, false, false, true);

        assert_eq!(
            ProgramPlayer::effective_slot_source_pos_ns(&slot, &clip, 2_000_000_000),
            123_000_000
        );
    }

    #[test]
    fn effective_source_pos_keeps_animated_svg_clip_local_timing() {
        let mut clip = make_clip();
        clip.is_image = true;
        clip.animated_svg = true;
        clip.source_in_ns = 500_000_000;
        clip.source_out_ns = 4_500_000_000;

        let mut slot =
            make_test_slot_ext(false, false, false, false, false, false, false, false, true);
        slot.animated_svg_rendered = true;

        assert_eq!(
            ProgramPlayer::effective_slot_source_pos_ns(&slot, &clip, 2_000_000_000),
            2_000_000_000
        );
    }

    #[test]
    fn freeze_frame_decode_seek_forces_accurate_non_key_unit() {
        let mut clip = make_clip();
        clip.freeze_frame = true;
        let slot = make_test_slot(false, false, false, false, false, false);
        let requested = gst::SeekFlags::FLUSH | gst::SeekFlags::KEY_UNIT;
        let effective = ProgramPlayer::effective_decode_seek_flags(&slot, &clip, requested);
        assert!(effective.contains(gst::SeekFlags::ACCURATE));
        assert!(!effective.contains(gst::SeekFlags::KEY_UNIT));
        assert!(effective.contains(gst::SeekFlags::FLUSH));
    }

    #[test]
    fn non_freeze_forward_decode_seek_preserves_key_unit_choice() {
        let clip = make_clip();
        let slot = make_test_slot(false, false, false, false, false, false);
        let requested = gst::SeekFlags::FLUSH | gst::SeekFlags::KEY_UNIT;
        let effective = ProgramPlayer::effective_decode_seek_flags(&slot, &clip, requested);
        assert!(effective.contains(gst::SeekFlags::KEY_UNIT));
        assert!(!effective.contains(gst::SeekFlags::ACCURATE));
    }

    #[test]
    fn freeze_frame_embedded_audio_is_suppressed() {
        let mut clip = make_clip();
        clip.has_audio = true;
        clip.freeze_frame = true;
        assert!(!clip.has_embedded_audio());
    }

    #[test]
    fn experimental_preview_optimizations_disabled_by_default() {
        // The feature defaults to off; users opt in via Preferences.
        let prefs = crate::ui_state::PreferencesState::default();
        assert!(!prefs.experimental_preview_optimizations);
    }

    #[test]
    fn background_prerender_disabled_by_default() {
        // Feature defaults to off for behavior-safe rollout.
        let prefs = crate::ui_state::PreferencesState::default();
        assert!(!prefs.background_prerender);
    }

    // ── effects_topology_matches tests ────────────────────────────────

    #[test]
    fn topology_matches_identical_default_clips() {
        let a = make_clip();
        let b = make_clip();
        assert!(ProgramPlayer::effects_topology_matches(&a, &b));
    }

    #[test]
    fn topology_matches_both_need_balance() {
        let mut a = make_clip();
        let mut b = make_clip();
        a.brightness = -0.5;
        b.brightness = 0.3;
        assert!(ProgramPlayer::effects_topology_matches(&a, &b));
    }

    #[test]
    fn topology_mismatch_one_needs_balance() {
        let a = make_clip();
        let mut b = make_clip();
        b.brightness = -0.5;
        assert!(!ProgramPlayer::effects_topology_matches(&a, &b));
    }

    #[test]
    fn topology_mismatch_temperature_triggers_coloradj() {
        let a = make_clip();
        let mut b = make_clip();
        b.temperature = 3000.0;
        assert!(!ProgramPlayer::effects_topology_matches(&a, &b));
    }

    #[test]
    fn topology_mismatch_tint_triggers_coloradj() {
        let a = make_clip();
        let mut b = make_clip();
        b.tint = 0.5;
        assert!(!ProgramPlayer::effects_topology_matches(&a, &b));
    }

    #[test]
    fn topology_mismatch_shadows_triggers_3point() {
        let a = make_clip();
        let mut b = make_clip();
        b.shadows = 0.3;
        assert!(!ProgramPlayer::effects_topology_matches(&a, &b));
    }

    #[test]
    fn topology_mismatch_midtones_triggers_3point() {
        let a = make_clip();
        let mut b = make_clip();
        b.midtones = -0.2;
        assert!(!ProgramPlayer::effects_topology_matches(&a, &b));
    }

    #[test]
    fn topology_mismatch_highlights_triggers_3point() {
        let a = make_clip();
        let mut b = make_clip();
        b.highlights = 0.4;
        assert!(!ProgramPlayer::effects_topology_matches(&a, &b));
    }

    #[test]
    fn topology_mismatch_blur() {
        let a = make_clip();
        let mut b = make_clip();
        b.denoise = 0.5;
        assert!(!ProgramPlayer::effects_topology_matches(&a, &b));
    }

    #[test]
    fn topology_mismatch_chroma_key() {
        let a = make_clip();
        let mut b = make_clip();
        b.chroma_key_enabled = true;
        assert!(!ProgramPlayer::effects_topology_matches(&a, &b));
    }

    #[test]
    fn topology_mismatch_freeze_hold() {
        let a = make_clip();
        let mut b = make_clip();
        b.freeze_frame = true;
        assert!(!ProgramPlayer::effects_topology_matches(&a, &b));
    }

    // ── slot_satisfies_clip tests ─────────────────────────────────────

    /// Create a minimal VideoSlot for testing.  Requires `gst::init()`.
    fn make_test_slot(
        has_videobalance: bool,
        has_gaussianblur: bool,
        has_rotate: bool,
        has_flip: bool,
        has_textoverlay: bool,
        has_chroma_key: bool,
    ) -> VideoSlot {
        make_test_slot_ext(
            has_videobalance,
            false,
            false,
            has_gaussianblur,
            has_rotate,
            has_flip,
            has_textoverlay,
            has_chroma_key,
            false,
        )
    }

    fn make_test_slot_ext(
        has_videobalance: bool,
        has_coloradj_rgb: bool,
        has_colorbalance_3pt: bool,
        has_gaussianblur: bool,
        has_rotate: bool,
        has_flip: bool,
        has_textoverlay: bool,
        has_chroma_key: bool,
        has_imagefreeze: bool,
    ) -> VideoSlot {
        let _ = gst::init();
        let identity = || gst::ElementFactory::make("identity").build().ok();
        VideoSlot {
            clip_idx: 0,
            decoder: gst::ElementFactory::make("fakesrc")
                .build()
                .unwrap_or_else(|_| gst::ElementFactory::make("identity").build().unwrap()),
            video_linked: Arc::new(AtomicBool::new(false)),
            audio_linked: Arc::new(AtomicBool::new(false)),
            effects_bin: gst::Bin::new(),
            compositor_pad: None,
            audio_mixer_pad: None,
            audio_conv: None,
            audio_resample: None,
            audio_capsfilter: None,
            audio_equalizer: None,
            audio_match_equalizer: None,
            audio_panorama: None,
            audio_level: None,
            videobalance: if has_videobalance { identity() } else { None },
            coloradj_rgb: if has_coloradj_rgb { identity() } else { None },
            colorbalance_3pt: if has_colorbalance_3pt {
                identity()
            } else {
                None
            },
            gaussianblur: if has_gaussianblur { identity() } else { None },
            squareblur: None, // no squareblur in test slots
            crop_alpha_state: Arc::new(Mutex::new((
                0i32, 0i32, 0i32, 0i32, 0i32, 0i32, 0i32, 0i32,
            ))),
            videocrop: None,
            videobox_crop_alpha: None,
            imagefreeze: if has_imagefreeze { identity() } else { None },
            videoflip_rotate: if has_rotate { identity() } else { None },
            videoflip_flip: if has_flip { identity() } else { None },
            textoverlay: if has_textoverlay { identity() } else { None },
            alpha_filter: None,
            alpha_chroma_key: if has_chroma_key { identity() } else { None },
            capsfilter_zoom: None,
            videobox_zoom: None,
            frei0r_user_effects: Vec::new(),
            slot_queue: None,
            comp_arrival_seq: Arc::new(AtomicU64::new(0)),
            hidden: false,
            animated_svg_rendered: false,
            is_prerender_slot: false,
            prerender_segment_start_ns: None,
            transition_enter_offset_ns: 0,
            is_blend_mode: false,
            blend_alpha: Arc::new(Mutex::new(1.0)),
            mask_data: Arc::new(Mutex::new(Vec::new())),
            hsl_data: Arc::new(Mutex::new(None)),
        }
    }

    #[test]
    fn prerender_signature_ignores_runtime_clip_id() {
        let mut first_hasher = DefaultHasher::new();
        let mut second_hasher = DefaultHasher::new();
        let first_clip = make_clip();
        let mut second_clip = make_clip();
        second_clip.id = "different-runtime-id".to_string();
        ProgramPlayer::hash_prerender_clip_state(
            &mut first_hasher,
            &first_clip,
            &first_clip.source_path,
        );
        ProgramPlayer::hash_prerender_clip_state(
            &mut second_hasher,
            &second_clip,
            &second_clip.source_path,
        );
        let first = first_hasher.finish();
        let second = second_hasher.finish();
        assert_eq!(first, second);
    }

    #[test]
    fn project_prerender_cache_root_uses_sidecar_directory() {
        let (root, persistent) =
            super::prerender_cache_root_for_project_path(Some("/tmp/My Project.uspxml"), true);
        assert!(persistent);
        assert!(root.to_string_lossy().contains("UltimateSlice.cache"));
        assert!(root.to_string_lossy().contains(&format!(
            "prerender-v{}",
            super::BACKGROUND_PRERENDER_CACHE_VERSION
        )));
        let leaf = root.file_name().and_then(|s| s.to_str()).unwrap_or("");
        assert!(leaf.starts_with("My_Project-p"));
    }

    #[test]
    fn project_prerender_cache_root_can_use_temporary_cache_only() {
        let (root, persistent) =
            super::prerender_cache_root_for_project_path(Some("/tmp/My Project.uspxml"), false);
        assert!(!persistent);
        assert_eq!(root, super::default_prerender_cache_root());
        assert!(!root.to_string_lossy().contains("UltimateSlice.cache"));
    }

    #[test]
    fn prerender_manifest_detects_source_changes() {
        let temp = tempfile::tempdir().expect("tempdir");
        let source_path = temp.path().join("clip.mp4");
        let segment_path = temp.path().join("seg.mp4");
        std::fs::write(&source_path, b"first-version").expect("write source");
        std::fs::write(&segment_path, b"segment").expect("write segment");
        let manifest = super::PrerenderSegmentManifest {
            key: "seg".to_string(),
            signature: 42,
            start_ns: 0,
            end_ns: 1_000_000_000,
            inputs: vec![super::PrerenderManifestInput {
                path: source_path.to_string_lossy().to_string(),
                signature: super::prerender_source_signature_for_path(&source_path)
                    .expect("source signature"),
            }],
        };
        ProgramPlayer::write_prerender_manifest_for_path(&segment_path, &manifest)
            .expect("write manifest");
        assert!(ProgramPlayer::prerender_manifest_inputs_are_fresh(
            &manifest.inputs
        ));
        std::thread::sleep(Duration::from_millis(5));
        std::fs::write(&source_path, b"second-version-with-new-size").expect("rewrite source");
        assert!(!ProgramPlayer::prerender_manifest_inputs_are_fresh(
            &manifest.inputs
        ));
    }

    #[test]
    fn prerender_cache_cleanup_can_preserve_or_purge_project_segments() {
        let temp = tempfile::tempdir().expect("tempdir");
        let source_path = temp.path().join("clip.mp4");
        std::fs::write(&source_path, b"clip").expect("write source");
        let segment_path = temp.path().join("seg.mp4");
        std::fs::write(&segment_path, b"segment").expect("write segment");
        let manifest = super::PrerenderSegmentManifest {
            key: "seg".to_string(),
            signature: 7,
            start_ns: 0,
            end_ns: 1_000_000_000,
            inputs: vec![super::PrerenderManifestInput {
                path: source_path.to_string_lossy().to_string(),
                signature: super::prerender_source_signature_for_path(&source_path)
                    .expect("source signature"),
            }],
        };
        ProgramPlayer::write_prerender_manifest_for_path(&segment_path, &manifest)
            .expect("write manifest");
        assert!(segment_path.exists());
        assert!(super::prerender_manifest_path(&segment_path).exists());
        ProgramPlayer::clear_prerender_cache_root(temp.path());
        assert!(!segment_path.exists());
        assert!(!super::prerender_manifest_path(&segment_path).exists());
    }

    #[test]
    fn persistent_prerender_segment_is_discovered_from_disk() {
        let temp = tempfile::tempdir().expect("tempdir");
        let source_path = temp.path().join("clip.mp4");
        std::fs::write(&source_path, b"clip").expect("write source");
        let mut clip = make_clip();
        clip.source_path = source_path.to_string_lossy().to_string();
        let inputs = vec![super::PrerenderManifestInput {
            path: clip.source_path.clone(),
            signature: super::prerender_source_signature_for_path(&source_path)
                .expect("source signature"),
        }];
        let mut signature_hasher = DefaultHasher::new();
        ProgramPlayer::hash_prerender_clip_state(&mut signature_hasher, &clip, &clip.source_path);
        let signature = signature_hasher.finish();
        let key = format!(
            "seg_v{}_{:016x}_{}_{}",
            super::BACKGROUND_PRERENDER_CACHE_VERSION,
            signature,
            0,
            1_000_000_000
        );
        let segment_path = temp.path().join(format!("{key}.mp4"));
        std::fs::write(&segment_path, b"segment").expect("write segment");
        let manifest = super::PrerenderSegmentManifest {
            key: key.clone(),
            signature,
            start_ns: 0,
            end_ns: 1_000_000_000,
            inputs: inputs.clone(),
        };
        ProgramPlayer::write_prerender_manifest_for_path(&segment_path, &manifest)
            .expect("write manifest");

        let discovered = ProgramPlayer::discover_prerender_segment_in_root(
            temp.path(),
            100_000_000,
            signature,
            &inputs,
        )
        .expect("discover prerender");
        assert_eq!(discovered.key, key);
        assert_eq!(discovered.path, segment_path.to_string_lossy().to_string());
        assert!(source_path.exists());
    }

    #[test]
    fn prerender_signature_changes_when_quality_changes() {
        let _ = gst::init();
        let result = ProgramPlayer::new();
        if result.is_err() {
            // gtk4paintablesink not available (headless CI) — skip
            return;
        }
        let (mut player, _paintable, _paintable2) = result.unwrap();
        let mut clip = make_clip();
        clip.source_path = "/tmp/clip.mp4".to_string();
        player.clips = vec![clip];

        let before = player.prerender_signature_for_active(&[0]);
        player.set_prerender_quality(PrerenderEncodingPreset::Medium, 18);
        let after = player.prerender_signature_for_active(&[0]);

        assert_ne!(before, after);
    }

    #[test]
    fn prerender_signature_changes_when_transition_changes() {
        let mut first_hasher = DefaultHasher::new();
        let mut second_hasher = DefaultHasher::new();
        let mut first_clip = make_clip();
        let mut second_clip = make_clip();
        first_clip.transition_after = "cross_dissolve".to_string();
        first_clip.transition_after_ns = 500_000_000;
        second_clip.transition_after = "circle_open".to_string();
        second_clip.transition_after_ns = 500_000_000;
        ProgramPlayer::hash_prerender_clip_state(
            &mut first_hasher,
            &first_clip,
            &first_clip.source_path,
        );
        ProgramPlayer::hash_prerender_clip_state(
            &mut second_hasher,
            &second_clip,
            &second_clip.source_path,
        );
        assert_ne!(first_hasher.finish(), second_hasher.finish());
    }

    #[test]
    fn prerender_signature_canonicalizes_transition_aliases() {
        let mut first_hasher = DefaultHasher::new();
        let mut second_hasher = DefaultHasher::new();
        let mut first_clip = make_clip();
        let mut second_clip = make_clip();
        first_clip.transition_after = "circleopen".to_string();
        first_clip.transition_after_ns = 500_000_000;
        second_clip.transition_after = "circle_open".to_string();
        second_clip.transition_after_ns = 500_000_000;
        ProgramPlayer::hash_prerender_clip_state(
            &mut first_hasher,
            &first_clip,
            &first_clip.source_path,
        );
        ProgramPlayer::hash_prerender_clip_state(
            &mut second_hasher,
            &second_clip,
            &second_clip.source_path,
        );
        assert_eq!(first_hasher.finish(), second_hasher.finish());
    }

    #[test]
    fn prerender_render_writes_mp4_even_with_partial_filename() {
        let mut title = make_clip();
        title.id = "title".to_string();
        title.source_path.clear();
        title.source_out_ns = 1_000_000_000;
        title.has_audio = false;
        title.is_title = true;
        title.title_clip_bg_color = 0x112233FF;

        let temp = tempfile::tempdir().expect("tempdir");
        let output_path = temp.path().join("prerender-title.mp4");
        let inputs = vec![(title, String::new(), 0, false, false)];

        let ok = ProgramPlayer::render_prerender_segment_video_file(
            output_path.to_str().expect("utf8 output path"),
            &inputs,
            &[],
            500_000_000,
            320,
            180,
            24,
            None,
            0,
            PrerenderEncodingPreset::Veryfast,
            DEFAULT_PRERENDER_CRF,
        );

        assert!(ok, "expected prerender render to succeed");
        assert!(output_path.exists());
        assert!(!super::prerender_partial_output_path(&output_path).exists());
    }

    #[test]
    fn preview_audio_normalizer_targets_fixed_mix_caps() {
        let _ = gst::init();
        use gst::prelude::*;
        let pipeline = gst::Pipeline::new();
        let audio_conv = gst::ElementFactory::make("audioconvert")
            .build()
            .expect("audioconvert");
        pipeline.add(&audio_conv).expect("add audioconvert");

        let (_resample, capsfilter, _src_pad) =
            super::attach_preview_audio_normalizer(&pipeline, &audio_conv, "test")
                .expect("attach preview audio normalizer");
        let caps = capsfilter.property::<gst::Caps>("caps");
        let s = caps.structure(0).expect("audio caps structure");

        assert_eq!(s.name(), "audio/x-raw");
        assert_eq!(
            s.get::<i32>("rate").expect("audio rate"),
            super::PROGRAM_PREVIEW_AUDIO_RATE
        );
        assert_eq!(
            s.get::<i32>("channels").expect("audio channels"),
            super::PROGRAM_PREVIEW_AUDIO_CHANNELS
        );
    }

    #[test]
    fn slot_satisfies_default_clip_without_elements() {
        // Default clip needs nothing — empty slot is fine.
        let slot = make_test_slot(false, false, false, false, false, false);
        let clip = make_clip();
        assert!(ProgramPlayer::slot_satisfies_clip(&slot, &clip));
    }

    #[test]
    fn slot_without_videobalance_rejects_brightness() {
        let slot = make_test_slot(false, false, false, false, false, false);
        let mut clip = make_clip();
        clip.brightness = -0.5;
        assert!(!ProgramPlayer::slot_satisfies_clip(&slot, &clip));
    }

    #[test]
    fn slot_with_videobalance_accepts_brightness() {
        let slot = make_test_slot(true, false, false, false, false, false);
        let mut clip = make_clip();
        clip.brightness = -0.5;
        assert!(ProgramPlayer::slot_satisfies_clip(&slot, &clip));
    }

    #[test]
    fn slot_without_videobalance_rejects_temperature() {
        let slot = make_test_slot(false, false, false, false, false, false);
        let mut clip = make_clip();
        clip.temperature = 3000.0;
        assert!(!ProgramPlayer::slot_satisfies_clip(&slot, &clip));
    }

    #[test]
    fn slot_without_videobalance_rejects_tint() {
        let slot = make_test_slot(false, false, false, false, false, false);
        let mut clip = make_clip();
        clip.tint = 0.5;
        assert!(!ProgramPlayer::slot_satisfies_clip(&slot, &clip));
    }

    #[test]
    fn slot_without_videobalance_rejects_shadows() {
        let slot = make_test_slot(false, false, false, false, false, false);
        let mut clip = make_clip();
        clip.shadows = 0.3;
        assert!(!ProgramPlayer::slot_satisfies_clip(&slot, &clip));
    }

    #[test]
    fn slot_without_videobalance_rejects_midtones() {
        let slot = make_test_slot(false, false, false, false, false, false);
        let mut clip = make_clip();
        clip.midtones = -0.2;
        assert!(!ProgramPlayer::slot_satisfies_clip(&slot, &clip));
    }

    #[test]
    fn slot_without_videobalance_rejects_highlights() {
        let slot = make_test_slot(false, false, false, false, false, false);
        let mut clip = make_clip();
        clip.highlights = 0.4;
        assert!(!ProgramPlayer::slot_satisfies_clip(&slot, &clip));
    }

    #[test]
    fn slot_without_gaussianblur_rejects_denoise() {
        let slot = make_test_slot(false, false, false, false, false, false);
        let mut clip = make_clip();
        clip.denoise = 0.5;
        assert!(!ProgramPlayer::slot_satisfies_clip(&slot, &clip));
    }

    #[test]
    fn slot_with_gaussianblur_accepts_denoise() {
        let slot = make_test_slot(false, true, false, false, false, false);
        let mut clip = make_clip();
        clip.denoise = 0.5;
        assert!(ProgramPlayer::slot_satisfies_clip(&slot, &clip));
    }

    #[test]
    fn slot_without_chroma_key_rejects_chroma_enabled() {
        let slot = make_test_slot(false, false, false, false, false, false);
        let mut clip = make_clip();
        clip.chroma_key_enabled = true;
        assert!(!ProgramPlayer::slot_satisfies_clip(&slot, &clip));
    }

    #[test]
    fn slot_with_chroma_key_accepts_chroma_enabled() {
        let slot = make_test_slot(false, false, false, false, false, true);
        let mut clip = make_clip();
        clip.chroma_key_enabled = true;
        assert!(ProgramPlayer::slot_satisfies_clip(&slot, &clip));
    }

    #[test]
    fn slot_without_imagefreeze_rejects_freeze_frame() {
        let slot = make_test_slot(false, false, false, false, false, false);
        let mut clip = make_clip();
        clip.freeze_frame = true;
        assert!(!ProgramPlayer::slot_satisfies_clip(&slot, &clip));
    }

    #[test]
    fn slot_with_imagefreeze_accepts_freeze_frame() {
        let slot = make_test_slot_ext(false, false, false, false, false, false, false, false, true);
        let mut clip = make_clip();
        clip.freeze_frame = true;
        assert!(ProgramPlayer::slot_satisfies_clip(&slot, &clip));
    }

    /// Regression test for the in-place self.clips update bug:
    /// When a clip is compared against itself after an in-place update,
    /// effects_topology_matches returns true even though the slot was
    /// built without the needed elements.  slot_satisfies_clip catches this.
    #[test]
    fn inplace_update_regression_topology_matches_but_slot_lacks_elements() {
        // Simulate: clip starts at defaults, user moves brightness slider.
        // After in-place update, both "old" and "new" clip have brightness=-0.5.
        let mut clip = make_clip();
        clip.brightness = -0.5;
        // effects_topology_matches compares clip against itself → true
        assert!(ProgramPlayer::effects_topology_matches(&clip, &clip));
        // But the slot was built without videobalance → slot_satisfies_clip → false
        let slot = make_test_slot(false, false, false, false, false, false);
        assert!(!ProgramPlayer::slot_satisfies_clip(&slot, &clip));
    }

    // ── compute_videobalance_params tests ────────────────────────────────

    fn vb(
        brightness: f64,
        contrast: f64,
        saturation: f64,
        temperature: f64,
        tint: f64,
        shadows: f64,
        midtones: f64,
        highlights: f64,
    ) -> super::VBParams {
        // Default: assume both frei0r elements available (no fallbacks).
        ProgramPlayer::compute_videobalance_params(
            brightness,
            contrast,
            saturation,
            temperature,
            tint,
            shadows,
            midtones,
            highlights,
            0.0,
            0.0,
            0.0,
            0.0,
            0.0,
            0.0,
            0.0,
            0.0,
            true,
            true,
        )
    }

    fn vb_no_coloradj(
        brightness: f64,
        contrast: f64,
        saturation: f64,
        temperature: f64,
        tint: f64,
        shadows: f64,
        midtones: f64,
        highlights: f64,
    ) -> super::VBParams {
        ProgramPlayer::compute_videobalance_params(
            brightness,
            contrast,
            saturation,
            temperature,
            tint,
            shadows,
            midtones,
            highlights,
            0.0,
            0.0,
            0.0,
            0.0,
            0.0,
            0.0,
            0.0,
            0.0,
            false,
            true,
        )
    }

    fn vb_no_3point(
        brightness: f64,
        contrast: f64,
        saturation: f64,
        temperature: f64,
        tint: f64,
        shadows: f64,
        midtones: f64,
        highlights: f64,
    ) -> super::VBParams {
        ProgramPlayer::compute_videobalance_params(
            brightness,
            contrast,
            saturation,
            temperature,
            tint,
            shadows,
            midtones,
            highlights,
            0.0,
            0.0,
            0.0,
            0.0,
            0.0,
            0.0,
            0.0,
            0.0,
            true,
            false,
        )
    }

    #[test]
    fn vb_neutral_returns_defaults() {
        let p = vb(0.0, 1.0, 1.0, 6500.0, 0.0, 0.0, 0.0, 0.0);
        assert!((p.brightness).abs() < 0.05, "b={}", p.brightness);
        assert!((p.contrast - 1.0).abs() < 0.05, "c={}", p.contrast);
        assert!((p.saturation - 1.0).abs() < 0.05, "s={}", p.saturation);
        assert!((p.hue).abs() < 0.05, "h={}", p.hue);
    }

    #[test]
    fn vb_brightness_positive_increases() {
        let p = vb(0.5, 1.0, 1.0, 6500.0, 0.0, 0.0, 0.0, 0.0);
        assert!(
            p.brightness > 0.3,
            "positive brightness should increase: b={}",
            p.brightness
        );
    }

    #[test]
    fn vb_brightness_negative_decreases() {
        let p = vb(-0.5, 1.0, 1.0, 6500.0, 0.0, 0.0, 0.0, 0.0);
        assert!(
            p.brightness < -0.3,
            "negative brightness should decrease: b={}",
            p.brightness
        );
    }

    #[test]
    fn vb_contrast_high_increases_contrast() {
        let p = vb(0.0, 1.8, 1.0, 6500.0, 0.0, 0.0, 0.0, 0.0);
        assert!(
            p.contrast > 1.1,
            "high contrast slider should raise contrast: c={}",
            p.contrast
        );
    }

    #[test]
    fn vb_contrast_low_decreases_contrast() {
        let p = vb(0.0, 0.3, 1.0, 6500.0, 0.0, 0.0, 0.0, 0.0);
        assert!(
            p.contrast < 0.9,
            "low contrast slider should lower contrast: c={}",
            p.contrast
        );
    }

    #[test]
    fn vb_saturation_zero_reduces_saturation() {
        let p = vb(0.0, 1.0, 0.0, 6500.0, 0.0, 0.0, 0.0, 0.0);
        assert!(
            p.saturation < 0.5,
            "zero saturation should reduce: s={}",
            p.saturation
        );
    }

    #[test]
    fn vb_temperature_warm_shifts_hue_without_coloradj() {
        let p = vb_no_coloradj(0.0, 1.0, 1.0, 3000.0, 0.0, 0.0, 0.0, 0.0);
        assert!(
            p.hue > 0.01,
            "warm temperature should shift hue positively (fallback): h={}",
            p.hue
        );
    }

    #[test]
    fn vb_temperature_cool_shifts_hue_without_coloradj() {
        let p = vb_no_coloradj(0.0, 1.0, 1.0, 9000.0, 0.0, 0.0, 0.0, 0.0);
        assert!(
            p.hue < -0.01,
            "cool temperature should shift hue negatively (fallback): h={}",
            p.hue
        );
    }

    #[test]
    fn vb_temperature_no_hue_with_coloradj() {
        let p = vb(0.0, 1.0, 1.0, 3000.0, 0.0, 0.0, 0.0, 0.0);
        assert!(
            (p.hue).abs() < 0.01,
            "with coloradj, temp should not shift hue: h={}",
            p.hue
        );
    }

    #[test]
    fn vb_temperature_warm_preserves_brightness_and_contrast() {
        let p = vb(0.0, 1.0, 1.0, 3000.0, 0.0, 0.0, 0.0, 0.0);
        assert!(
            (p.brightness).abs() < 0.05,
            "warm temp should not change brightness: b={}",
            p.brightness
        );
        assert!(
            (p.contrast - 1.0).abs() < 0.05,
            "warm temp should not change contrast: c={}",
            p.contrast
        );
    }

    #[test]
    fn vb_saturation_preserves_brightness() {
        let low = vb(0.0, 1.0, 0.2, 6500.0, 0.0, 0.0, 0.0, 0.0);
        let high = vb(0.0, 1.0, 1.8, 6500.0, 0.0, 0.0, 0.0, 0.0);
        assert!(
            (low.brightness).abs() < 0.05,
            "low saturation should not change brightness: b={}",
            low.brightness
        );
        assert!(
            (high.brightness).abs() < 0.05,
            "high saturation should not change brightness: b={}",
            high.brightness
        );
    }

    #[test]
    fn vb_tint_shifts_hue_without_coloradj() {
        let pos = vb_no_coloradj(0.0, 1.0, 1.0, 6500.0, 0.5, 0.0, 0.0, 0.0);
        let neg = vb_no_coloradj(0.0, 1.0, 1.0, 6500.0, -0.5, 0.0, 0.0, 0.0);
        assert!(
            pos.hue > neg.hue,
            "positive tint should shift hue more positive (fallback)"
        );
    }

    #[test]
    fn vb_tint_no_hue_with_coloradj() {
        let pos = vb(0.0, 1.0, 1.0, 6500.0, 0.5, 0.0, 0.0, 0.0);
        let neg = vb(0.0, 1.0, 1.0, 6500.0, -0.5, 0.0, 0.0, 0.0);
        assert!(
            (pos.hue - neg.hue).abs() < 0.01,
            "with coloradj, tint should not shift hue"
        );
    }

    #[test]
    fn vb_shadows_positive_lifts_brightness() {
        let p = vb_no_3point(0.0, 1.0, 1.0, 6500.0, 0.0, 0.5, 0.0, 0.0);
        assert!(
            p.brightness > 0.05,
            "lifting shadows should raise brightness: b={}",
            p.brightness
        );
    }

    #[test]
    fn vb_shadows_positive_reduces_contrast() {
        let p = vb_no_3point(0.0, 1.0, 1.0, 6500.0, 0.0, 0.5, 0.0, 0.0);
        assert!(
            p.contrast < 0.95,
            "lifting shadows should reduce contrast: c={}",
            p.contrast
        );
    }

    #[test]
    fn vb_highlights_positive_boosts_contrast() {
        let p = vb_no_3point(0.0, 1.0, 1.0, 6500.0, 0.0, 0.0, 0.0, 0.5);
        assert!(
            p.contrast > 1.3,
            "boosting highlights should raise contrast: c={}",
            p.contrast
        );
    }

    #[test]
    fn vb_highlights_positive_increases_brightness() {
        let p = vb_no_3point(0.0, 1.0, 1.0, 6500.0, 0.0, 0.0, 0.0, 0.5);
        assert!(
            p.brightness > 0.05,
            "boosting highlights should increase brightness: b={}",
            p.brightness
        );
    }

    #[test]
    fn vb_midtones_positive_lifts_brightness() {
        let p = vb_no_3point(0.0, 1.0, 1.0, 6500.0, 0.0, 0.0, 0.5, 0.0);
        assert!(
            p.brightness > 0.03,
            "midtones lift should increase brightness: b={}",
            p.brightness
        );
    }

    #[test]
    fn vb_black_point_fallback_affects_brightness_without_3point() {
        let base = vb_no_3point(0.0, 1.0, 1.0, 6500.0, 0.0, 0.0, 0.0, 0.0);
        let adjusted = ProgramPlayer::compute_videobalance_params(
            0.0, 1.0, 1.0, 6500.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.8, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0,
            true, false,
        );
        assert!(
            adjusted.brightness > base.brightness + 0.05,
            "positive black point should brighten fallback preview: base={} adjusted={}",
            base.brightness,
            adjusted.brightness
        );
    }

    #[test]
    fn vb_warmth_tint_fallback_shifts_hue_without_3point() {
        let warm = ProgramPlayer::compute_videobalance_params(
            0.0, 1.0, 1.0, 6500.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.8, 0.0, 0.8, 0.0, 0.8, 0.0,
            true, false,
        );
        let cool = ProgramPlayer::compute_videobalance_params(
            0.0, 1.0, 1.0, 6500.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, -0.8, 0.0, -0.8, 0.0, -0.8, 0.0,
            true, false,
        );
        let magenta = ProgramPlayer::compute_videobalance_params(
            0.0, 1.0, 1.0, 6500.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.8, 0.0, 0.8, 0.0, 0.8,
            true, false,
        );
        let green = ProgramPlayer::compute_videobalance_params(
            0.0, 1.0, 1.0, 6500.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, -0.8, 0.0, -0.8, 0.0, -0.8,
            true, false,
        );
        assert!(
            warm.hue < cool.hue,
            "warm/cool should diverge in fallback hue"
        );
        assert!(
            magenta.hue > green.hue,
            "magenta/green tint should diverge in fallback hue"
        );
    }

    #[test]
    fn vb_warmth_fallback_keeps_direction_for_each_tonal_slider() {
        let warm_shadows = ProgramPlayer::compute_videobalance_params(
            0.0, 1.0, 1.0, 6500.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.8, 0.0, 0.0, 0.0, 0.0, 0.0,
            true, false,
        );
        let cool_shadows = ProgramPlayer::compute_videobalance_params(
            0.0, 1.0, 1.0, 6500.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, -0.8, 0.0, 0.0, 0.0, 0.0, 0.0,
            true, false,
        );
        assert!(warm_shadows.hue < cool_shadows.hue);

        let warm_midtones = ProgramPlayer::compute_videobalance_params(
            0.0, 1.0, 1.0, 6500.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.8, 0.0, 0.0, 0.0,
            true, false,
        );
        let cool_midtones = ProgramPlayer::compute_videobalance_params(
            0.0, 1.0, 1.0, 6500.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, -0.8, 0.0, 0.0, 0.0,
            true, false,
        );
        assert!(warm_midtones.hue < cool_midtones.hue);

        let warm_highlights = ProgramPlayer::compute_videobalance_params(
            0.0, 1.0, 1.0, 6500.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.8, 0.0,
            true, false,
        );
        let cool_highlights = ProgramPlayer::compute_videobalance_params(
            0.0, 1.0, 1.0, 6500.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, -0.8, 0.0,
            true, false,
        );
        assert!(warm_highlights.hue < cool_highlights.hue);
    }

    #[test]
    fn vb_tint_fallback_keeps_direction_for_each_tonal_slider() {
        let magenta_shadows = ProgramPlayer::compute_videobalance_params(
            0.0, 1.0, 1.0, 6500.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.8, 0.0, 0.0, 0.0, 0.0,
            true, false,
        );
        let green_shadows = ProgramPlayer::compute_videobalance_params(
            0.0, 1.0, 1.0, 6500.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, -0.8, 0.0, 0.0, 0.0, 0.0,
            true, false,
        );
        assert!(magenta_shadows.hue > green_shadows.hue);

        let magenta_midtones = ProgramPlayer::compute_videobalance_params(
            0.0, 1.0, 1.0, 6500.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.8, 0.0, 0.0,
            true, false,
        );
        let green_midtones = ProgramPlayer::compute_videobalance_params(
            0.0, 1.0, 1.0, 6500.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, -0.8, 0.0, 0.0,
            true, false,
        );
        assert!(magenta_midtones.hue > green_midtones.hue);

        let magenta_highlights = ProgramPlayer::compute_videobalance_params(
            0.0, 1.0, 1.0, 6500.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.8,
            true, false,
        );
        let green_highlights = ProgramPlayer::compute_videobalance_params(
            0.0, 1.0, 1.0, 6500.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, -0.8,
            true, false,
        );
        assert!(magenta_highlights.hue > green_highlights.hue);
    }

    #[test]
    fn vb_shadows_no_effect_with_3point() {
        let p = vb(0.0, 1.0, 1.0, 6500.0, 0.0, 0.8, 0.0, 0.0);
        assert!(
            (p.brightness).abs() < 0.05,
            "with 3-point, shadows should not affect vb brightness: b={}",
            p.brightness
        );
    }

    #[test]
    fn vb_highlights_no_effect_with_3point() {
        let p = vb(0.0, 1.0, 1.0, 6500.0, 0.0, 0.0, 0.0, 0.8);
        assert!(
            (p.brightness).abs() < 0.05,
            "with 3-point, highlights should not affect vb brightness: b={}",
            p.brightness
        );
        assert!(
            (p.contrast - 1.0).abs() < 0.05,
            "with 3-point, highlights should not affect vb contrast: c={}",
            p.contrast
        );
    }

    #[test]
    fn vb_all_params_clamped() {
        // Extreme values: everything at max
        let p = vb(1.0, 2.0, 2.0, 10000.0, 1.0, 1.0, 1.0, 1.0);
        assert!(
            p.brightness >= -1.0 && p.brightness <= 1.0,
            "b clamped: {}",
            p.brightness
        );
        assert!(
            p.contrast >= 0.0 && p.contrast <= 2.0,
            "c clamped: {}",
            p.contrast
        );
        assert!(
            p.saturation >= 0.0 && p.saturation <= 2.0,
            "s clamped: {}",
            p.saturation
        );
        assert!(p.hue >= -1.0 && p.hue <= 1.0, "h clamped: {}", p.hue);
        // Everything at min
        let p2 = vb(-1.0, 0.0, 0.0, 2000.0, -1.0, -1.0, -1.0, -1.0);
        assert!(
            p2.brightness >= -1.0 && p2.brightness <= 1.0,
            "b clamped min: {}",
            p2.brightness
        );
        assert!(
            p2.contrast >= 0.0 && p2.contrast <= 2.0,
            "c clamped min: {}",
            p2.contrast
        );
        assert!(
            p2.saturation >= 0.0 && p2.saturation <= 2.0,
            "s clamped min: {}",
            p2.saturation
        );
        assert!(p2.hue >= -1.0 && p2.hue <= 1.0, "h clamped min: {}", p2.hue);
    }

    #[test]
    fn vb_poly4_evaluates_correctly() {
        let c = [1.0, 2.0, 3.0, 4.0, 5.0];
        // poly4(2.0) = 1 + 2*2 + 3*4 + 4*8 + 5*16 = 1 + 4 + 12 + 32 + 80 = 129
        let v = ProgramPlayer::poly4(2.0, &c);
        assert!((v - 129.0).abs() < 1e-9, "poly4(2.0) = {}", v);
        // poly4(0.0) = 1.0
        let v0 = ProgramPlayer::poly4(0.0, &c);
        assert!((v0 - 1.0).abs() < 1e-9, "poly4(0.0) = {}", v0);
    }

    // ── compute_coloradj_params tests ────────────────────────────────

    #[test]
    fn coloradj_neutral_at_6500k() {
        let cp = ProgramPlayer::compute_coloradj_params(6500.0, 0.0);
        assert!((cp.r - 0.5).abs() < 0.02, "neutral r={}", cp.r);
        assert!((cp.g - 0.5).abs() < 0.03, "neutral g={}", cp.g);
        assert!((cp.b - 0.5).abs() < 0.02, "neutral b={}", cp.b);
    }

    #[test]
    fn coloradj_warm_boosts_red_cuts_blue() {
        let cp = ProgramPlayer::compute_coloradj_params(3000.0, 0.0);
        assert!(
            cp.r > cp.b,
            "warm should boost R over B: r={} b={}",
            cp.r,
            cp.b
        );
        assert!(cp.b < 0.4, "warm should cut blue: b={}", cp.b);
    }

    #[test]
    fn coloradj_cool_boosts_blue_cuts_red() {
        let cp = ProgramPlayer::compute_coloradj_params(10000.0, 0.0);
        assert!(
            cp.b > cp.r,
            "cool should boost B over R: r={} b={}",
            cp.r,
            cp.b
        );
        assert!(cp.r < 0.45, "cool should cut red: r={}", cp.r);
    }

    #[test]
    fn coloradj_tint_shifts_green() {
        let pos = ProgramPlayer::compute_coloradj_params(6500.0, 0.5);
        let neg = ProgramPlayer::compute_coloradj_params(6500.0, -0.5);
        // Positive tint = cut green (lower param), boost R/B
        assert!(
            pos.g < neg.g,
            "positive tint cuts green: pos_g={} neg_g={}",
            pos.g,
            neg.g
        );
        assert!(
            pos.r > neg.r,
            "positive tint boosts red: pos_r={} neg_r={}",
            pos.r,
            neg.r
        );
    }

    #[test]
    fn coloradj_params_clamped() {
        let cp = ProgramPlayer::compute_coloradj_params(1000.0, 1.0);
        assert!(cp.r >= 0.0 && cp.r <= 1.0, "r clamped: {}", cp.r);
        assert!(cp.g >= 0.0 && cp.g <= 1.0, "g clamped: {}", cp.g);
        assert!(cp.b >= 0.0 && cp.b <= 1.0, "b clamped: {}", cp.b);
    }

    // ── slot_satisfies_clip with coloradj ────────────────────────────

    #[test]
    fn slot_with_coloradj_accepts_temperature() {
        let slot = make_test_slot_ext(false, true, false, false, false, false, false, false, false);
        let mut clip = make_clip();
        clip.temperature = 3000.0;
        assert!(ProgramPlayer::slot_satisfies_clip(&slot, &clip));
    }

    #[test]
    fn slot_with_videobalance_accepts_temperature_fallback() {
        let slot = make_test_slot(true, false, false, false, false, false);
        let mut clip = make_clip();
        clip.temperature = 3000.0;
        assert!(ProgramPlayer::slot_satisfies_clip(&slot, &clip));
    }

    #[test]
    fn slot_with_coloradj_accepts_tint() {
        let slot = make_test_slot_ext(false, true, false, false, false, false, false, false, false);
        let mut clip = make_clip();
        clip.tint = 0.5;
        assert!(ProgramPlayer::slot_satisfies_clip(&slot, &clip));
    }

    // ── slot_satisfies_clip with colorbalance_3pt ─────────────────────

    #[test]
    fn slot_with_3point_accepts_shadows() {
        let slot = make_test_slot_ext(false, false, true, false, false, false, false, false, false);
        let mut clip = make_clip();
        clip.shadows = 0.5;
        assert!(ProgramPlayer::slot_satisfies_clip(&slot, &clip));
    }

    #[test]
    fn slot_with_3point_accepts_midtones() {
        let slot = make_test_slot_ext(false, false, true, false, false, false, false, false, false);
        let mut clip = make_clip();
        clip.midtones = -0.3;
        assert!(ProgramPlayer::slot_satisfies_clip(&slot, &clip));
    }

    #[test]
    fn slot_with_3point_accepts_highlights() {
        let slot = make_test_slot_ext(false, false, true, false, false, false, false, false, false);
        let mut clip = make_clip();
        clip.highlights = 0.7;
        assert!(ProgramPlayer::slot_satisfies_clip(&slot, &clip));
    }

    #[test]
    fn slot_with_videobalance_accepts_shadows_fallback() {
        let slot = make_test_slot(true, false, false, false, false, false);
        let mut clip = make_clip();
        clip.shadows = 0.5;
        assert!(ProgramPlayer::slot_satisfies_clip(&slot, &clip));
    }

    // ── compute_3point_params tests ──────────────────────────────────

    #[test]
    fn threepoint_neutral_at_defaults() {
        let p =
            ProgramPlayer::compute_3point_params(0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0);
        assert!((p.black_r).abs() < 0.01, "neutral black_r={}", p.black_r);
        assert!((p.gray_r - 0.5).abs() < 0.01, "neutral gray_r={}", p.gray_r);
        assert!(
            (p.white_r - 1.0).abs() < 0.01,
            "neutral white_r={}",
            p.white_r
        );
    }

    #[test]
    fn threepoint_positive_shadows_lifts_black() {
        // Calibrated: positive shadows shift the curve to brighten shadow region.
        // Black point may stay near 0 while gray/white adjust.
        let neutral =
            ProgramPlayer::compute_3point_params(0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0);
        let p =
            ProgramPlayer::compute_3point_params(0.8, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0);
        let changed = (p.black_r - neutral.black_r).abs() > 0.001
            || (p.gray_r - neutral.gray_r).abs() > 0.001
            || (p.white_r - neutral.white_r).abs() > 0.001;
        assert!(changed, "positive shadows should alter the curve");
    }

    #[test]
    fn threepoint_negative_shadows_lowers_gray() {
        let neutral =
            ProgramPlayer::compute_3point_params(0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0);
        let neg =
            ProgramPlayer::compute_3point_params(-0.8, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0);
        assert!(
            neg.gray_r < neutral.gray_r,
            "negative shadows should lower gray: gray_r={}",
            neg.gray_r
        );
        assert!(
            (neg.black_r).abs() < 0.01,
            "negative shadows should keep black at 0: black_r={}",
            neg.black_r
        );
    }

    #[test]
    fn threepoint_positive_midtones_raises_gray() {
        // In frei0r 3-point, a LOWER gray value brightens midtones
        // (the parabola's midpoint shifts left). Calibrated positive
        // midtones → lower gray to match FFmpeg's brighten behavior.
        let neutral =
            ProgramPlayer::compute_3point_params(0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0);
        let p =
            ProgramPlayer::compute_3point_params(0.0, 0.7, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0);
        assert!(
            p.gray_r < neutral.gray_r,
            "positive midtones should lower gray (brighten): gray_r={}",
            p.gray_r
        );
    }

    #[test]
    fn threepoint_negative_highlights_pulls_white() {
        let p =
            ProgramPlayer::compute_3point_params(0.0, 0.0, -0.8, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0);
        assert!(
            p.white_r < 1.0,
            "negative highlights should pull down white: white_r={}",
            p.white_r
        );
    }

    #[test]
    fn threepoint_positive_highlights_raises_gray() {
        // Calibrated: positive highlights shift gray/white to brighten
        // the highlight region. Gray may move lower (frei0r parabola semantics).
        let neutral =
            ProgramPlayer::compute_3point_params(0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0);
        let pos =
            ProgramPlayer::compute_3point_params(0.0, 0.0, 0.8, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0);
        let changed = (pos.gray_r - neutral.gray_r).abs() > 0.01
            || (pos.white_r - neutral.white_r).abs() > 0.01;
        assert!(changed, "positive highlights should alter the curve");
    }

    #[test]
    fn threepoint_all_channels_equal() {
        let p =
            ProgramPlayer::compute_3point_params(0.5, -0.3, 0.7, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0);
        assert!((p.black_r - p.black_g).abs() < 1e-10, "R==G for black");
        assert!((p.black_r - p.black_b).abs() < 1e-10, "R==B for black");
        assert!((p.gray_r - p.gray_g).abs() < 1e-10, "R==G for gray");
        assert!((p.white_r - p.white_g).abs() < 1e-10, "R==G for white");
    }

    #[test]
    fn threepoint_params_clamped() {
        let p =
            ProgramPlayer::compute_3point_params(1.0, 1.0, 1.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0);
        assert!(
            p.black_r >= 0.0 && p.black_r <= 0.8,
            "black clamped: {}",
            p.black_r
        );
        assert!(
            p.gray_r >= 0.05 && p.gray_r <= 0.95,
            "gray clamped: {}",
            p.gray_r
        );
        assert!(
            p.white_r >= 0.2 && p.white_r <= 1.0,
            "white clamped: {}",
            p.white_r
        );
        let p2 = ProgramPlayer::compute_3point_params(
            -1.0, -1.0, -1.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0,
        );
        assert!(
            p2.black_r >= 0.0 && p2.black_r <= 0.8,
            "black clamped min: {}",
            p2.black_r
        );
        assert!(
            p2.gray_r >= 0.05 && p2.gray_r <= 0.95,
            "gray clamped min: {}",
            p2.gray_r
        );
        assert!(
            p2.white_r >= 0.2 && p2.white_r <= 1.0,
            "white clamped min: {}",
            p2.white_r
        );
    }

    #[test]
    fn apply_export_tonal_parity_gains_with_unity_preserves_inputs() {
        let (sh, mid, hi) =
            ProgramPlayer::apply_export_tonal_parity_gains(0.7, -0.4, -0.6, 1.0, 1.0, 1.0);
        assert!((sh - 0.7).abs() < 1e-9);
        assert!((mid + 0.4).abs() < 1e-9);
        assert!((hi + 0.6).abs() < 1e-9);
    }

    #[test]
    fn apply_export_tonal_parity_gains_is_side_selective() {
        let (sh, mid, hi) =
            ProgramPlayer::apply_export_tonal_parity_gains(0.8, -0.5, -0.4, 0.9, 0.92, 0.95);
        assert!((sh - 0.72).abs() < 1e-9);
        assert!((mid + 0.46).abs() < 1e-9);
        assert!((hi + 0.38).abs() < 1e-9);

        let (sh_pos, mid_pos, hi_pos) =
            ProgramPlayer::apply_export_tonal_parity_gains(-0.8, 0.5, 0.4, 0.9, 0.92, 0.95);
        assert!(
            (sh_pos + 0.8).abs() < 1e-9,
            "negative shadows must be unchanged"
        );
        assert!(
            (mid_pos - 0.5).abs() < 1e-9,
            "positive midtones must be unchanged"
        );
        assert!(
            (hi_pos - 0.4).abs() < 1e-9,
            "positive highlights must be unchanged"
        );
    }

    #[test]
    fn export_temperature_parity_gain_is_cool_side_only() {
        assert!((ProgramPlayer::export_temperature_parity_gain(6500.0) - 1.0).abs() < 1e-9);
        assert!((ProgramPlayer::export_temperature_parity_gain(10000.0) - 1.0).abs() < 1e-9);
        assert!((ProgramPlayer::export_temperature_parity_gain(2000.0) - 1.0).abs() < 1e-9);
    }

    #[test]
    fn piecewise_cool_temperature_gain_interpolates_segments() {
        let far = 0.94;
        let near = 0.98;
        assert!(
            (ProgramPlayer::piecewise_cool_temperature_gain(2000.0, far, near) - far).abs() < 1e-9
        );
        assert!(
            (ProgramPlayer::piecewise_cool_temperature_gain(3500.0, far, near) - 0.96).abs() < 1e-9
        );
        assert!(
            (ProgramPlayer::piecewise_cool_temperature_gain(5000.0, far, near) - near).abs() < 1e-9
        );
        assert!(
            (ProgramPlayer::piecewise_cool_temperature_gain(5750.0, far, near) - 0.99).abs() < 1e-9
        );
        assert!(
            (ProgramPlayer::piecewise_cool_temperature_gain(6500.0, far, near) - 1.0).abs() < 1e-9
        );
        assert!(
            (ProgramPlayer::piecewise_cool_temperature_gain(10000.0, far, near) - 1.0).abs() < 1e-9
        );
    }

    #[test]
    fn export_temperature_channel_offsets_zero_at_neutral() {
        let (r, g, b) = ProgramPlayer::export_temperature_channel_offsets(6500.0);
        assert!(r.abs() < 1e-9, "r should be zero at neutral: {r}");
        assert!(g.abs() < 1e-9, "g should be zero at neutral: {g}");
        assert!(b.abs() < 1e-9, "b should be zero at neutral: {b}");
    }

    #[test]
    fn export_temperature_channel_offsets_at_extremes() {
        // Warm side (2000K) — offsets are zeroed (content-dependent regression).
        let (r2k, g2k, b2k) = ProgramPlayer::export_temperature_channel_offsets(2000.0);
        assert!((r2k).abs() < 1e-9, "2000K: R should be zero: {r2k}");
        assert!((g2k).abs() < 1e-9, "2000K: G should be zero: {g2k}");
        assert!((b2k).abs() < 1e-9, "2000K: B should be zero: {b2k}");

        // Cool side (10000K) — offsets are negative (FFmpeg doesn't cool enough).
        let (r10k, g10k, b10k) = ProgramPlayer::export_temperature_channel_offsets(10000.0);
        assert!(
            b10k < -0.01,
            "10000K: B should be strongly negative: {b10k}"
        );
        assert!(r10k < 0.0, "10000K: R should be negative: {r10k}");
        assert!(
            b10k < r10k,
            "10000K: B offset should be larger than R: b={b10k} r={r10k}"
        );
    }

    #[test]
    fn export_3point_parity_offsets_shadows_positive_is_noop() {
        // Tonal 3-point offsets are intentionally disabled (no-op).
        let base =
            ProgramPlayer::compute_3point_params(0.5, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0);
        let mut adjusted = base.clone();
        ProgramPlayer::apply_export_3point_parity_offsets(&mut adjusted, 0.5, 0.0, 0.0);
        assert!(
            (adjusted.black_r - base.black_r).abs() < 1e-9,
            "no-op: black_r unchanged"
        );
        assert!(
            (adjusted.gray_r - base.gray_r).abs() < 1e-9,
            "no-op: gray_r unchanged"
        );
        assert!(
            (adjusted.white_r - base.white_r).abs() < 1e-9,
            "no-op: white_r unchanged"
        );
    }

    #[test]
    fn export_3point_parity_offsets_midtones_negative_is_noop() {
        let base =
            ProgramPlayer::compute_3point_params(0.0, -1.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0);
        let mut adjusted = base.clone();
        ProgramPlayer::apply_export_3point_parity_offsets(&mut adjusted, 0.0, -1.0, 0.0);
        assert!(
            (adjusted.gray_r - base.gray_r).abs() < 1e-9,
            "no-op: gray_r unchanged"
        );
    }

    #[test]
    fn export_3point_parity_offsets_highlights_is_noop() {
        let base =
            ProgramPlayer::compute_3point_params(0.0, 0.0, -1.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0);
        let mut adjusted = base.clone();
        ProgramPlayer::apply_export_3point_parity_offsets(&mut adjusted, 0.0, 0.0, -1.0);
        assert!(
            (adjusted.white_r - base.white_r).abs() < 1e-9,
            "no-op: white_r unchanged"
        );

        let base_pos =
            ProgramPlayer::compute_3point_params(0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0);
        let mut adjusted_pos = base_pos.clone();
        ProgramPlayer::apply_export_3point_parity_offsets(&mut adjusted_pos, 0.0, 0.0, 1.0);
        assert!(
            (adjusted_pos.white_r - base_pos.white_r).abs() < 1e-9,
            "no-op: white_r unchanged"
        );
    }

    #[test]
    fn export_3point_parity_offsets_neutral_is_passthrough() {
        let base =
            ProgramPlayer::compute_3point_params(0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0);
        let mut adjusted = base.clone();
        ProgramPlayer::apply_export_3point_parity_offsets(&mut adjusted, 0.0, 0.0, 0.0);
        assert!((adjusted.black_r - base.black_r).abs() < 1e-9);
        assert!((adjusted.gray_r - base.gray_r).abs() < 1e-9);
        assert!((adjusted.white_r - base.white_r).abs() < 1e-9);
    }

    #[test]
    fn tonal_axis_response_boosts_slider_ends_without_destabilizing_center() {
        let center = ProgramPlayer::compute_tonal_axis_response(0.2);
        let end = ProgramPlayer::compute_tonal_axis_response(1.0);

        assert!(
            (center - 0.2).abs() < 0.01,
            "center should remain precise: {}",
            center
        );
        assert!(
            end > 1.3,
            "ends should be substantially stronger than linear: {}",
            end
        );
    }

    #[test]
    fn threepoint_positive_shadows_warmth_produces_warm_red() {
        // Positive warmth = warm (red boost in shadows).
        // In 3-point space: lower R control point = brighter red output,
        // higher B control point = darker blue output.
        let p =
            ProgramPlayer::compute_3point_params(0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0);
        assert!(
            p.black_r < p.black_b,
            "positive shadows warmth: red control should be lower than blue: black_r={}, black_b={}",
            p.black_r,
            p.black_b
        );
    }

    #[test]
    fn threepoint_negative_shadows_warmth_produces_cool_blue() {
        // Negative warmth = cool (blue boost in shadows).
        // In 3-point space: lower B control = brighter blue, higher R control = darker red.
        let p =
            ProgramPlayer::compute_3point_params(0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, -1.0, 0.0);
        assert!(
            p.black_b < p.black_r,
            "negative shadows warmth: blue control should be lower than red: black_r={}, black_b={}",
            p.black_r,
            p.black_b
        );
    }

    #[test]
    fn threepoint_shadows_tint_changes_curve_space_channels() {
        // Positive tint = magenta (green cut): black_g should be HIGHER
        // (higher control point = darker/less green output).
        // Negative tint = green (green boost): black_g should be LOWER.
        let magenta =
            ProgramPlayer::compute_3point_params(0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 1.0);
        let green =
            ProgramPlayer::compute_3point_params(0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, -1.0);
        assert!(
            magenta.black_g > green.black_g,
            "magenta tint (+1) green control should be higher (less green): magenta_g={}, green_g={}",
            magenta.black_g,
            green.black_g
        );
        assert!(
            magenta.black_r < green.black_r && magenta.black_b < green.black_b,
            "magenta tint should have lower R+B controls (more R+B output) than green tint"
        );
    }

    #[test]
    fn threepoint_parabola_identity_at_neutral() {
        // At neutral params (black=0, gray=0.5, white=1.0), the parabola
        // should be identity: y = x.
        let p =
            ProgramPlayer::compute_3point_params(0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0);
        let para = ThreePointParabola::from_params(&p);
        // Check a few sample values.
        for &x in &[0.0, 0.25, 0.5, 0.75, 1.0] {
            let yr = para.r.a * x * x + para.r.b * x + para.r.c;
            assert!(
                (yr - x).abs() < 0.05,
                "neutral red parabola at x={}: expected ~{}, got {}",
                x,
                x,
                yr
            );
        }
    }

    #[test]
    fn threepoint_parabola_matches_frei0r_at_midgray() {
        // The frei0r plugin maps gray_c → 0.5.  Verify our parabola does too.
        let p =
            ProgramPlayer::compute_3point_params(0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0);
        let para = ThreePointParabola::from_params(&p);
        let x = p.gray_r;
        let y = para.r.a * x * x + para.r.b * x + para.r.c;
        assert!(
            (y - 0.5).abs() < 0.001,
            "gray_r={}: parabola at gray should be 0.5, got {}",
            x,
            y
        );
    }

    #[test]
    fn crossfade_curve_gain_clamps_progress_bounds() {
        assert_eq!(
            ProgramPlayer::crossfade_curve_gain(
                &crate::ui_state::CrossfadeCurve::Linear,
                -0.2,
                true
            ),
            0.0
        );
        assert_eq!(
            ProgramPlayer::crossfade_curve_gain(
                &crate::ui_state::CrossfadeCurve::Linear,
                -0.2,
                false
            ),
            1.0
        );
        assert_eq!(
            ProgramPlayer::crossfade_curve_gain(
                &crate::ui_state::CrossfadeCurve::Linear,
                1.2,
                true
            ),
            1.0
        );
        assert_eq!(
            ProgramPlayer::crossfade_curve_gain(
                &crate::ui_state::CrossfadeCurve::Linear,
                1.2,
                false
            ),
            0.0
        );
        let eq_in = ProgramPlayer::crossfade_curve_gain(
            &crate::ui_state::CrossfadeCurve::EqualPower,
            0.5,
            true,
        );
        let eq_out = ProgramPlayer::crossfade_curve_gain(
            &crate::ui_state::CrossfadeCurve::EqualPower,
            0.5,
            false,
        );
        assert!((eq_in - std::f64::consts::FRAC_1_SQRT_2).abs() < 1e-12);
        assert!((eq_out - std::f64::consts::FRAC_1_SQRT_2).abs() < 1e-12);
    }

    #[test]
    fn crossfade_duration_clamps_to_half_clip_duration() {
        let mut a = make_clip();
        a.source_out_ns = 2_000_000_000;
        let mut b = make_clip();
        b.source_out_ns = 1_000_000_000;
        assert_eq!(
            ProgramPlayer::clamped_crossfade_duration_ns(900_000_000, &a, &b),
            500_000_000
        );
    }

    #[test]
    fn compute_clip_crossfade_gain_applies_linear_fades_for_adjacent_clips() {
        let mut a = make_clip();
        a.id = "a".into();
        a.timeline_start_ns = 0;
        a.source_out_ns = 1_000_000_000;

        let mut b = make_clip();
        b.id = "b".into();
        b.timeline_start_ns = 1_000_000_000;
        b.source_out_ns = 1_000_000_000;

        let clips = vec![a, b];
        let out_gain = ProgramPlayer::compute_clip_crossfade_gain(
            true,
            &crate::ui_state::CrossfadeCurve::Linear,
            400_000_000,
            &clips,
            0,
            900_000_000,
        );
        let in_gain = ProgramPlayer::compute_clip_crossfade_gain(
            true,
            &crate::ui_state::CrossfadeCurve::Linear,
            400_000_000,
            &clips,
            1,
            1_100_000_000,
        );
        assert!((out_gain - 0.25).abs() < 1e-6, "outgoing gain={out_gain}");
        assert!((in_gain - 0.25).abs() < 1e-6, "incoming gain={in_gain}");
    }
}

impl Drop for ProgramPlayer {
    fn drop(&mut self) {
        self.cleanup_background_prerender_cache(!self.should_preserve_prerender_cache_files());
        self.teardown_prepreroll_sidecars();
        self.teardown_slots();
        let _ = self.pipeline.set_state(gst::State::Null);
        let _ = self.audio_pipeline.set_state(gst::State::Null);
    }
}
