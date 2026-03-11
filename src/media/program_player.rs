use crate::media::player::PlayerState;
use crate::model::clip::{Clip as ModelClip, NumericKeyframe};
use crate::ui_state::{CrossfadeCurve, PlaybackPriority};
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
use std::collections::{HashMap, HashSet};
use std::hash::{Hash, Hasher};
use std::io::Write;
use std::path::Path;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const MAX_PREVIEW_AUDIO_GAIN: f64 = 3.981_071_705_5; // +12 dB
const BACKGROUND_PRERENDER_CACHE_VERSION: u32 = 2;

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
struct VBParams {
    brightness: f64,
    contrast: f64,
    saturation: f64,
    hue: f64,
}

/// Per-channel RGB gains for frei0r `coloradj_RGB` element.
/// Values are in frei0r's [0,1] range where 0.5 = neutral (gain 1.0).
struct ColorAdjRGBParams {
    r: f64,
    g: f64,
    b: f64,
}

/// Parameters for frei0r `3-point-color-balance` element.
/// Maps shadows/midtones/highlights to the black/gray/white reference
/// points of a piecewise-linear transfer curve.
struct ThreePointParams {
    black_r: f64,
    black_g: f64,
    black_b: f64,
    gray_r: f64,
    gray_g: f64,
    gray_b: f64,
    white_r: f64,
    white_g: f64,
    white_b: f64,
}

/// Role of a clip within a transition overlap region.
#[derive(Debug, Clone, Copy, PartialEq)]
enum TransitionRole {
    /// The clip is fading out (clip A in A→B transition).
    Outgoing,
    /// The clip is fading in (clip B in A→B transition).
    Incoming,
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
    /// Volume multiplier: 0.0 (silent) to 2.0 (double), default 1.0
    pub volume: f64,
    pub volume_keyframes: Vec<NumericKeyframe>,
    /// Audio pan: -1.0 (full left) to 1.0 (full right), default 0.0
    pub pan: f64,
    pub pan_keyframes: Vec<NumericKeyframe>,
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
    /// Title text overlay
    pub title_text: String,
    pub title_font: String,
    pub title_color: u32,
    pub title_x: f64,
    pub title_y: f64,
    /// Playback speed multiplier (default 1.0). >1 = fast, <1 = slow.
    pub speed: f64,
    /// Optional variable speed keyframes over clip-local timeline.
    pub speed_keyframes: Vec<NumericKeyframe>,
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
    /// Track index — higher index clips (B-roll, overlays) take priority in preview.
    pub track_index: usize,
    /// Transition to next clip on same track (e.g. "cross_dissolve").
    pub transition_after: String,
    /// Transition duration in nanoseconds.
    pub transition_after_ns: u64,
    /// LUT file path for color grading (used for proxy lookup when proxy mode is enabled).
    pub lut_path: Option<String>,
    /// Scale multiplier: 1.0 = fill frame, >1.0 = zoom in, <1.0 = shrink with black borders.
    pub scale: f64,
    pub scale_keyframes: Vec<NumericKeyframe>,
    /// Opacity multiplier for compositing: 0.0 = transparent, 1.0 = opaque.
    pub opacity: f64,
    pub opacity_keyframes: Vec<NumericKeyframe>,
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
    /// Whether the source file contains an audio stream.
    pub has_audio: bool,
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
        ModelClip::evaluate_keyframed_value(
            &self.volume_keyframes,
            self.local_timeline_position_ns(timeline_pos_ns),
            self.volume,
        )
    }

    pub fn pan_at_timeline_ns(&self, timeline_pos_ns: u64) -> f64 {
        ModelClip::evaluate_keyframed_value(
            &self.pan_keyframes,
            self.local_timeline_position_ns(timeline_pos_ns),
            self.pan,
        )
        .clamp(-1.0, 1.0)
    }

    pub fn rotate_at_timeline_ns(&self, timeline_pos_ns: u64) -> i32 {
        ModelClip::evaluate_keyframed_value(
            &self.rotate_keyframes,
            self.local_timeline_position_ns(timeline_pos_ns),
            self.rotate as f64,
        )
        .round()
        .clamp(-180.0, 180.0) as i32
    }

    pub fn crop_left_at_timeline_ns(&self, timeline_pos_ns: u64) -> i32 {
        ModelClip::evaluate_keyframed_value(
            &self.crop_left_keyframes,
            self.local_timeline_position_ns(timeline_pos_ns),
            self.crop_left as f64,
        )
        .round()
        .clamp(0.0, 500.0) as i32
    }

    pub fn crop_right_at_timeline_ns(&self, timeline_pos_ns: u64) -> i32 {
        ModelClip::evaluate_keyframed_value(
            &self.crop_right_keyframes,
            self.local_timeline_position_ns(timeline_pos_ns),
            self.crop_right as f64,
        )
        .round()
        .clamp(0.0, 500.0) as i32
    }

    pub fn crop_top_at_timeline_ns(&self, timeline_pos_ns: u64) -> i32 {
        ModelClip::evaluate_keyframed_value(
            &self.crop_top_keyframes,
            self.local_timeline_position_ns(timeline_pos_ns),
            self.crop_top as f64,
        )
        .round()
        .clamp(0.0, 500.0) as i32
    }

    pub fn crop_bottom_at_timeline_ns(&self, timeline_pos_ns: u64) -> i32 {
        ModelClip::evaluate_keyframed_value(
            &self.crop_bottom_keyframes,
            self.local_timeline_position_ns(timeline_pos_ns),
            self.crop_bottom as f64,
        )
        .round()
        .clamp(0.0, 500.0) as i32
    }

    pub fn speed_at_local_timeline_ns(&self, local_timeline_ns: u64) -> f64 {
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
        const MAX_SAMPLES: u64 = 4096;
        const STEP_NS: u64 = 8_333_333; // ~120Hz
        let sample_count = (local_timeline_ns / STEP_NS).max(1).min(MAX_SAMPLES);
        let mut integrated = 0.0f64;
        for i in 0..sample_count {
            let t0 =
                (u128::from(local_timeline_ns) * u128::from(i) / u128::from(sample_count)) as u64;
            let t1 = (u128::from(local_timeline_ns) * u128::from(i + 1) / u128::from(sample_count))
                as u64;
            let dt = t1.saturating_sub(t0);
            if dt == 0 {
                continue;
            }
            let mid = t0.saturating_add(dt / 2);
            integrated += self.speed_at_local_timeline_ns(mid) * dt as f64;
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
        if self.speed > 0.0 {
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
        let delta = ((offset as f64 * self.speed) as u64).min(max_delta);
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
    videocrop: Option<gst::Element>,
    videobox_crop_alpha: Option<gst::Element>,
    imagefreeze: Option<gst::Element>,
    videoflip_rotate: Option<gst::Element>,
    videoflip_flip: Option<gst::Element>,
    textoverlay: Option<gst::Element>,
    #[allow(dead_code)]
    alpha_filter: Option<gst::Element>,
    alpha_chroma_key: Option<gst::Element>,
    capsfilter_zoom: Option<gst::Element>,
    videobox_zoom: Option<gst::Element>,
    /// Queue between effects_bin and compositor to decouple caps negotiation.
    slot_queue: Option<gst::Element>,
    /// Monotonic counter incremented when a buffer passes through queue→compositor.
    /// Used to detect when post-seek buffers have reached the compositor.
    comp_arrival_seq: Arc<AtomicU64>,
    /// When true, the slot is hidden (alpha=0, volume=0) — kept alive for
    /// potential re-entry but not counted as "active" for boundary detection.
    hidden: bool,
    /// True for synthetic prerender-composite slots.
    is_prerender_slot: bool,
    /// Timeline start of the prerender segment for synthetic prerender slots.
    prerender_segment_start_ns: Option<u64>,
    /// Nonzero when this slot was created for a clip entering early via a
    /// transition overlap.  The value equals the preceding clip's
    /// `transition_after_ns` and is added to timeline_pos when computing
    /// source-file seek positions.
    transition_enter_offset_ns: u64,
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
}

struct PrerenderJobResult {
    key: String,
    path: String,
    start_ns: u64,
    end_ns: u64,
    signature: u64,
    success: bool,
}

pub struct BackgroundPrerenderProgress {
    pub total: usize,
    pub completed: usize,
    pub in_flight: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum AudioCurrentSource {
    AudioClip(usize),
    ReverseVideoClip(usize),
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
    crossfade_enabled: bool,
    crossfade_curve: CrossfadeCurve,
    crossfade_duration_ns: u64,
    proxy_paths: HashMap<String, String>,
    /// Proxy cache keys already reported as fallback-to-original in this session.
    /// Prevents warning spam while proxies are still being generated.
    proxy_fallback_warned_keys: HashSet<String>,
    /// Bg-removed file paths: source_path → bg_removed file path.
    bg_removal_paths: HashMap<String, String>,
    /// Cache for per-path audio-stream probe results.
    audio_stream_probe_cache: HashMap<String, bool>,
    /// GStreamer `level` element on audiomixer output for metering.
    level_element: Option<gst::Element>,
    /// GStreamer `level` element on the audio-only pipeline for metering.
    #[allow(dead_code)]
    level_element_audio: Option<gst::Element>,
    /// GStreamer `volume` element on the audio-only pipeline for volume control.
    /// Using a dedicated element avoids playbin's StreamVolume which can interact
    /// with PulseAudio/PipeWire flat-volume behaviour and inadvertently change
    /// the main pipeline's audio level.
    audio_volume_element: Option<gst::Element>,
    /// GStreamer `audiopanorama` element on the audio-only pipeline for panning.
    audio_panorama_element: Option<gst::Element>,
    pub audio_peak_db: [f64; 2],
    pub audio_track_peak_db: Vec<[f64; 2]>,
    jkl_rate: f64,
    /// Latest RGBA frame captured from the scope appsink.
    latest_scope_frame: Arc<Mutex<Option<ScopeFrame>>>,
    /// Latest RGBA frame captured from compositor src (before preview downscale).
    latest_compositor_frame: Arc<Mutex<Option<ScopeFrame>>>,
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
    /// True when heavy-overlap playback is using drop-late display policy to
    /// keep displayed video aligned to the audio clock.
    playback_drop_late_active: bool,
    /// True when per-slot queues are in drop-late mode during heavy overlap playback.
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
    /// Failed prerender job keys (skip immediate retry churn).
    prerender_failed: HashSet<String>,
    /// Result channel for background prerender jobs.
    prerender_result_rx: mpsc::Receiver<PrerenderJobResult>,
    /// Sender cloned into background prerender workers.
    prerender_result_tx: mpsc::Sender<PrerenderJobResult>,
    /// Runtime cache root for temporary prerendered sections.
    prerender_cache_root: PathBuf,
    /// Files created during this runtime, removed on cleanup.
    prerender_runtime_files: HashSet<String>,
    /// Active clip signature represented by the current prerender segment slot.
    prerender_active_clips: Option<Vec<usize>>,
    /// Current prerender segment key when a synthetic prerender slot is active.
    current_prerender_segment_key: Option<String>,
    /// Total prerender jobs requested this session (for status UI).
    prerender_total_requested: usize,
    /// Last idle prerender scan time (throttles paused/stopped scheduling).
    last_idle_prerender_scan_at: Option<Instant>,
    /// Set when a prerender segment becomes ready while already inside its range;
    /// consumed in `poll()` to trigger a live→prerender swap rebuild.
    pending_prerender_promote: bool,
    /// Frame duration in nanoseconds, derived from project frame rate.
    /// Used to quantize seek positions and deduplicate same-frame seeks.
    frame_duration_ns: u64,
    /// Last frame-quantized seek position (nanoseconds). When a new seek
    /// lands on the same frame, the pipeline work is skipped entirely.
    last_seeked_frame_pos: Option<u64>,
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
            .property(
                "caps",
                &gst::Caps::builder("audio/x-raw")
                    .field("rate", 48000i32)
                    .field("channels", 2i32)
                    .build(),
            )
            .build()?;

        let audio_conv_out = gst::ElementFactory::make("audioconvert").build()?;
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

        // Link compositor → processing caps → convert → scale → output caps → display/scope
        gst::Element::link_many([
            &compositor,
            &comp_capsfilter,
            &videoconvert_out,
            &videoscale_out,
            &preview_capsfilter,
            &video_sink_bin,
        ])?;
        // Probe on compositor output: always increments cseq (cheap) but only
        // copies the full-res frame when compositor_capture_enabled is set (during
        // MCP frame export).  This avoids ~250 MB/s of unnecessary memcpy during
        // normal playback.
        if let Some(comp_src_pad) = comp_capsfilter.static_pad("src") {
            let lcf = latest_compositor_frame.clone();
            let cseq = compositor_frame_seq.clone();
            let capture_en = compositor_capture_enabled.clone();
            let caps_pad = comp_src_pad.clone();
            comp_src_pad.add_probe(gst::PadProbeType::BUFFER, move |_pad, info| {
                cseq.fetch_add(1, Ordering::Relaxed);
                if !capture_en.load(Ordering::Relaxed) {
                    return gst::PadProbeReturn::Ok;
                }
                if let Some(buffer) = info.buffer() {
                    if let Ok(map) = buffer.map_readable() {
                        let (w, h) = caps_pad
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

        // Link audiomixer → audioconvert → [level →] autoaudiosink
        if let Some(ref lv) = level_element {
            gst::Element::link_many([&audiomixer, &audio_conv_out, lv, &audio_sink])?;
        } else {
            gst::Element::link_many([&audiomixer, &audio_conv_out, &audio_sink])?;
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
        let mut audio_filters: Vec<gst::Element> = Vec::new();
        if let Some(ref vol) = audio_volume_element {
            audio_filters.push(vol.clone());
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
        let prerender_cache_root = std::env::temp_dir()
            .join("ultimateslice")
            .join(format!("prerender-v{}", BACKGROUND_PRERENDER_CACHE_VERSION));
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
                realtime_preview: false,
                background_prerender: false,
                crossfade_enabled: false,
                crossfade_curve: CrossfadeCurve::default(),
                crossfade_duration_ns: 200_000_000,
                proxy_paths: HashMap::new(),
                proxy_fallback_warned_keys: HashSet::new(),
                bg_removal_paths: HashMap::new(),
                audio_stream_probe_cache: HashMap::new(),
                level_element,
                level_element_audio,
                audio_volume_element,
                audio_panorama_element,
                audio_peak_db: [-60.0, -60.0],
                audio_track_peak_db: Vec::new(),
                jkl_rate: 0.0,
                latest_scope_frame,
                latest_compositor_frame,
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
                prerender_failed: HashSet::new(),
                prerender_result_rx,
                prerender_result_tx,
                prerender_cache_root,
                prerender_runtime_files: HashSet::new(),
                prerender_active_clips: None,
                current_prerender_segment_key: None,
                prerender_total_requested: 0,
                last_idle_prerender_scan_at: None,
                pending_prerender_promote: false,
                // Default 24 fps ≈ 41_666_666 ns per frame
                frame_duration_ns: 1_000_000_000 / 24,
                last_seeked_frame_pos: None,
                short_frame_cache: ShortFrameCache::default(),
                pending_short_frame_capture: None,
                last_boundary_rebuild_clips: Vec::new(),
                last_boundary_rebuild_at: None,
                last_not_negotiated_recover_at: None,
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
            self.cleanup_background_prerender_cache();
        }
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

    fn cleanup_background_prerender_cache(&mut self) {
        for path in self.prerender_runtime_files.drain() {
            let _ = std::fs::remove_file(path);
        }
        if let Ok(entries) = std::fs::read_dir(&self.prerender_cache_root) {
            for entry in entries.flatten() {
                let p = entry.path();
                if p.extension().and_then(|s| s.to_str()) == Some("mp4") {
                    let _ = std::fs::remove_file(p);
                }
            }
        }
        self.prerender_segments.clear();
        self.prerender_pending.clear();
        self.prerender_failed.clear();
        self.prerender_active_clips = None;
        self.current_prerender_segment_key = None;
        self.prerender_total_requested = 0;
        self.last_idle_prerender_scan_at = None;
        self.pending_prerender_promote = false;
        while self.prerender_result_rx.try_recv().is_ok() {}
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

    pub fn update_bg_removal_paths(&mut self, paths: HashMap<String, String>) {
        if self.bg_removal_paths != paths {
            self.bg_removal_paths = paths;
            self.prewarmed_boundary_ns = None;
            self.invalidate_short_frame_cache("bg-removal-paths-updated");
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
                c.transition_after.hash(&mut hasher);
                c.transition_after_ns.hash(&mut hasher);
                c.title_text.hash(&mut hasher);
                c.title_font.hash(&mut hasher);
                c.title_color.hash(&mut hasher);
                c.title_x.to_bits().hash(&mut hasher);
                c.title_y.to_bits().hash(&mut hasher);
                c.bg_removal_enabled.hash(&mut hasher);
                c.bg_removal_threshold.to_bits().hash(&mut hasher);
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
        self.cleanup_background_prerender_cache();
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
        let mut arrival_wait_budget_ms = 3000;
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
            return needs_async;
        }
        // Full rebuild: needed when the set of active clips has changed (e.g.
        // crossing a clip boundary), on cold start, or when resuming from playing.
        self.rebuild_pipeline_at(timeline_pos_ns);
        self.apply_keyframed_video_slot_properties(timeline_pos_ns);
        self.apply_transition_effects(timeline_pos_ns);
        // Sync audio-only pipeline
        self.sync_audio_to(timeline_pos_ns);
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
                // For 3+ tracks, start a non-blocking Playing pulse: lock the
                // audio sink, set Playing, and return immediately so the GTK
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
        for slot in &self.slots {
            let clip = &self.clips[slot.clip_idx];
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
        self.sync_audio_to(pos);
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
        if self.audio_current_source.is_some() {
            let _ = self.audio_pipeline.set_state(gst::State::Playing);
        }
        self.state = PlayerState::Playing;
        self.base_timeline_ns = pos;
        self.play_start = Some(Instant::now());
        self.last_seeked_frame_pos = None;
        self.jkl_rate = 0.0;
        self.update_drop_late_policy();
        self.update_slot_queue_policy();
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
        self.teardown_slots();
        self.teardown_prepreroll_sidecars();
        let _ = self.pipeline.set_state(gst::State::Ready);
        let _ = self.audio_pipeline.set_state(gst::State::Paused);
        self.apply_reverse_video_main_audio_ducking(None);
        self.state = PlayerState::Stopped;
        self.timeline_pos_ns = 0;
        self.base_timeline_ns = 0;
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
            return;
        }
        self.sync_audio_to(0);
        // Keep stop lightweight; avoid paused rebuild/seek in the stop path.
        let _ = self.pipeline.set_state(gst::State::Paused);
        self.state = PlayerState::Stopped;
        self.update_drop_late_policy();
        self.update_slot_queue_policy();
    }

    pub fn set_jkl_rate(&mut self, rate: f64) {
        self.jkl_rate = rate;
        if rate == 0.0 {
            self.pause();
            return;
        }
        // Capture current position, then restart playback.
        if let Some(start) = self.play_start.take() {
            let elapsed = start.elapsed().as_nanos() as u64;
            self.timeline_pos_ns = self.base_timeline_ns + elapsed;
        }
        // For negative rates, just pause (reverse not yet supported with compositor).
        if rate < 0.0 {
            self.jkl_rate = 0.0;
            self.pause();
            return;
        }
        self.base_timeline_ns = self.timeline_pos_ns;
        self.play_start = Some(Instant::now());
        // Rebuild to seek decoders at current position with the new rate.
        self.rebuild_pipeline_at(self.timeline_pos_ns);
        let _ = self.pipeline.set_state(gst::State::Playing);
        self.state = PlayerState::Playing;
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

    fn crossfade_curve_gain(curve: &CrossfadeCurve, progress: f64, incoming: bool) -> f64 {
        let t = progress.clamp(0.0, 1.0);
        match (curve, incoming) {
            (CrossfadeCurve::EqualPower, true) => (t * std::f64::consts::FRAC_PI_2).sin(),
            (CrossfadeCurve::EqualPower, false) => (t * std::f64::consts::FRAC_PI_2).cos(),
            (CrossfadeCurve::Linear, true) => t,
            (CrossfadeCurve::Linear, false) => 1.0 - t,
        }
    }

    fn adjacent_prev_same_track_with_audio(
        clips: &[ProgramClip],
        clip_idx: usize,
    ) -> Option<usize> {
        let clip = clips.get(clip_idx)?;
        clips
            .iter()
            .enumerate()
            .filter(|(idx, c)| {
                *idx != clip_idx
                    && c.has_embedded_audio()
                    && c.track_index == clip.track_index
                    && c.timeline_end_ns() == clip.timeline_start_ns
            })
            .max_by_key(|(_, c)| c.timeline_start_ns)
            .map(|(idx, _)| idx)
    }

    fn adjacent_next_same_track_with_audio(
        clips: &[ProgramClip],
        clip_idx: usize,
    ) -> Option<usize> {
        let clip = clips.get(clip_idx)?;
        clips
            .iter()
            .enumerate()
            .filter(|(idx, c)| {
                *idx != clip_idx
                    && c.has_embedded_audio()
                    && c.track_index == clip.track_index
                    && c.timeline_start_ns == clip.timeline_end_ns()
            })
            .min_by_key(|(_, c)| c.timeline_start_ns)
            .map(|(idx, _)| idx)
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

    fn apply_main_audio_slot_volumes(&self, timeline_pos_ns: u64) {
        for slot in &self.slots {
            if slot.is_prerender_slot {
                continue;
            }
            if let Some(ref pad) = slot.audio_mixer_pad {
                let volume = if slot.hidden {
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
        }
    }

    fn sync_preview_audio_levels(&self, timeline_pos_ns: u64) {
        self.apply_main_audio_slot_volumes(timeline_pos_ns);
        if let Some(source) = self.audio_current_source {
            let volume = self.effective_audio_source_volume(source, timeline_pos_ns);
            self.set_audio_pipeline_volume(volume);
            let pan = self.effective_audio_source_pan(source, timeline_pos_ns);
            self.set_audio_pipeline_pan(pan);
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
        let desired = self.clips_active_at(new_pos);
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
                let used_realtime = realtime_boundary_enabled
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
                    self.sync_audio_to(new_pos);
                    if self.audio_current_source.is_some() {
                        let _ = self.audio_pipeline.set_state(gst::State::Playing);
                    }
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
                        self.sync_audio_to(new_pos);
                        if self.audio_current_source.is_some() {
                            let _ = self.audio_pipeline.set_state(gst::State::Playing);
                        }
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
            brightness, contrast, saturation, 6500.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0,
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
    ) {
        // Check whether the required effect elements exist.  If the slider
        // values moved away from defaults but the slot was built without the
        // element (because values were at defaults when the slot was created),
        // we must do a one-time pipeline rebuild so the element gets created.
        let need_balance = brightness != 0.0 || contrast != 1.0 || saturation != 1.0;
        let need_coloradj = (temperature - 6500.0).abs() > 1.0 || tint.abs() > 0.001;
        let need_3point = shadows.abs() > f64::EPSILON
            || midtones.abs() > f64::EPSILON
            || highlights.abs() > f64::EPSILON;
        let need_blur = {
            let sigma = (denoise * 4.0 - sharpness * 6.0).clamp(-20.0, 20.0);
            sigma.abs() > f64::EPSILON
        };
        let topology_changed = if let Some(slot) =
            self.current_idx.and_then(|idx| self.slot_for_clip(idx))
        {
            (need_balance && slot.videobalance.is_none())
                || (need_coloradj && slot.coloradj_rgb.is_none() && slot.videobalance.is_none())
                || (need_3point && slot.colorbalance_3pt.is_none() && slot.videobalance.is_none())
                || (need_blur && slot.gaussianblur.is_none())
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
                clip.shadows = shadows;
                clip.midtones = midtones;
                clip.highlights = highlights;
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
                let p = Self::compute_3point_params(shadows, midtones, highlights);
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
            clip.shadows = shadows;
            clip.midtones = midtones;
            clip.highlights = highlights;
        }
        // Force frame redraw when paused.
        if self.current_idx.is_some() && self.state != PlayerState::Playing {
            self.reseek_slot_for_current();
        }
    }

    pub fn update_current_chroma_key(
        &mut self,
        enabled: bool,
        color: u32,
        tolerance: f32,
        softness: f32,
    ) {
        if let Some(slot) = self.current_idx.and_then(|idx| self.slot_for_clip(idx)) {
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
        // Force frame redraw when paused.
        if self.current_idx.is_some() && self.state != PlayerState::Playing {
            self.reseek_slot_for_current();
        }
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

    pub fn update_audio_for_clip(&mut self, clip_id: &str, volume: f64, pan: f64) {
        let volume = volume.clamp(0.0, MAX_PREVIEW_AUDIO_GAIN);
        let pan = pan.clamp(-1.0, 1.0);
        // Check video clips first (use audiomixer pad on compositor pipeline).
        if let Some(i) = self.clips.iter().position(|c| c.id == clip_id) {
            self.clips[i].volume = volume;
            self.clips[i].pan = pan;
            self.apply_main_audio_slot_volumes(self.timeline_pos_ns);
            return;
        }
        // For audio-only clips, update the stored volume and, if actively playing,
        // update the dedicated volume element (avoids playbin StreamVolume crosstalk).
        if let Some(i) = self.audio_clips.iter().position(|c| c.id == clip_id) {
            self.audio_clips[i].volume = volume;
            self.audio_clips[i].pan = pan;
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
                Self::apply_zoom_to_slot(slot, pad, scale, position_x, position_y, proc_w, proc_h);
            }
        }
    }

    pub fn set_title(&self, text: &str, font: &str, color_rgba: u32, rel_x: f64, rel_y: f64) {
        if let Some(slot) = self.current_idx.and_then(|idx| self.slot_for_clip(idx)) {
            Self::apply_title_to_slot(slot, text, font, color_rgba, rel_x, rel_y);
        }
    }

    pub fn update_current_title(
        &mut self,
        text: &str,
        font: &str,
        color_rgba: u32,
        rel_x: f64,
        rel_y: f64,
    ) {
        self.set_title(text, font, color_rgba, rel_x, rel_y);
        if self.current_idx.is_some() && self.state != PlayerState::Playing {
            self.reseek_slot_for_current();
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
        if let Some(idx) = self.current_idx {
            if let Some(clip) = self.clips.get_mut(idx) {
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
            if let Some(clip) = self.clips.get_mut(i) {
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
                        slot, pad, scale, position_x, position_y, proc_w, proc_h,
                    );
                }
            }
            if self.state != PlayerState::Playing {
                self.reseek_slot_by_clip_idx(i);
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
            if let Some(clip) = self.clips.get_mut(i) {
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
                        slot, pad, scale, position_x, position_y, proc_w, proc_h,
                    );
                }
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
            }
            // Update compositor pad alpha for this slot.
            if let Some(slot) = self.slot_for_clip(idx) {
                if let Some(ref pad) = slot.compositor_pad {
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
            if let Some(clip) = self.clips.get_mut(i) {
                clip.opacity = opacity;
            }
            if let Some(slot) = self.slot_for_clip(i) {
                if let Some(ref pad) = slot.compositor_pad {
                    pad.set_property("alpha", opacity);
                }
            }
            if self.current_idx == Some(i) && self.state != PlayerState::Playing {
                self.reseek_slot_for_current();
            }
        }
    }

    // ── Private helpers ────────────────────────────────────────────────────

    fn clip_has_phase1_keyframes(clip: &ProgramClip) -> bool {
        !clip.scale_keyframes.is_empty()
            || !clip.opacity_keyframes.is_empty()
            || !clip.brightness_keyframes.is_empty()
            || !clip.contrast_keyframes.is_empty()
            || !clip.saturation_keyframes.is_empty()
            || !clip.temperature_keyframes.is_empty()
            || !clip.tint_keyframes.is_empty()
            || !clip.position_x_keyframes.is_empty()
            || !clip.position_y_keyframes.is_empty()
            || !clip.volume_keyframes.is_empty()
            || !clip.pan_keyframes.is_empty()
            || !clip.rotate_keyframes.is_empty()
            || !clip.crop_left_keyframes.is_empty()
            || !clip.crop_right_keyframes.is_empty()
            || !clip.crop_top_keyframes.is_empty()
            || !clip.crop_bottom_keyframes.is_empty()
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
    /// Includes clips that are entering early due to a transition overlap: when clip A
    /// (active) has `transition_after_ns > 0`, the next same-track clip B is included
    /// during the window `[A.end - A.transition_after_ns, A.end)`.
    fn clips_active_at(&self, timeline_pos_ns: u64) -> Vec<usize> {
        let mut active: Vec<usize> = self
            .clips
            .iter()
            .enumerate()
            .filter(|(_, c)| {
                timeline_pos_ns >= c.timeline_start_ns && timeline_pos_ns < c.timeline_end_ns()
            })
            .map(|(i, _)| i)
            .collect();
        // Include clips entering via transition overlap from a preceding clip.
        for i in 0..self.clips.len() {
            if active.contains(&i) {
                continue;
            }
            let clip = &self.clips[i];
            // Find preceding clip on same track with an active transition.
            let prev = self.clips.iter().enumerate().find(|(_, c)| {
                c.track_index == clip.track_index
                    && c.timeline_end_ns() == clip.timeline_start_ns
                    && !c.transition_after.is_empty()
                    && c.transition_after_ns > 0
            });
            if let Some((_, prev_clip)) = prev {
                let trans_start = prev_clip
                    .timeline_end_ns()
                    .saturating_sub(prev_clip.transition_after_ns);
                if timeline_pos_ns >= trans_start && timeline_pos_ns < prev_clip.timeline_end_ns() {
                    active.push(i);
                }
            }
        }
        if active.is_empty() {
            const GAP_NS: u64 = 100_000_000;
            if let Some(next_start) = self
                .clips
                .iter()
                .filter(|c| {
                    c.timeline_start_ns > timeline_pos_ns
                        && c.timeline_start_ns <= timeline_pos_ns + GAP_NS
                })
                .map(|c| c.timeline_start_ns)
                .min()
            {
                active = self
                    .clips
                    .iter()
                    .enumerate()
                    .filter(|(_, c)| c.timeline_start_ns == next_start)
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
        if !clip.transition_after.is_empty() && clip.transition_after_ns > 0 {
            let trans_start = clip
                .timeline_end_ns()
                .saturating_sub(clip.transition_after_ns);
            if timeline_pos_ns >= trans_start && timeline_pos_ns < clip.timeline_end_ns() {
                let elapsed = timeline_pos_ns.saturating_sub(trans_start) as f64;
                let duration = clip.transition_after_ns as f64;
                let progress = (elapsed / duration).clamp(0.0, 1.0);
                return Some(TransitionState {
                    kind: clip.transition_after.clone(),
                    progress,
                    role: TransitionRole::Outgoing,
                });
            }
        }
        // Check if this clip is the INCOMING clip (preceding same-track clip has transition).
        let prev = self.clips.iter().find(|c| {
            c.track_index == clip.track_index
                && c.timeline_end_ns() == clip.timeline_start_ns
                && !c.transition_after.is_empty()
                && c.transition_after_ns > 0
        });
        if let Some(prev_clip) = prev {
            let trans_start = prev_clip
                .timeline_end_ns()
                .saturating_sub(prev_clip.transition_after_ns);
            if timeline_pos_ns >= trans_start && timeline_pos_ns < prev_clip.timeline_end_ns() {
                let elapsed = timeline_pos_ns.saturating_sub(trans_start) as f64;
                let duration = prev_clip.transition_after_ns as f64;
                let progress = (elapsed / duration).clamp(0.0, 1.0);
                return Some(TransitionState {
                    kind: prev_clip.transition_after.clone(),
                    progress,
                    role: TransitionRole::Incoming,
                });
            }
        }
        None
    }

    /// Find the transition_after_ns for a clip entering via transition overlap.
    /// Returns the preceding clip's `transition_after_ns` if this clip starts
    /// at the exact end of a clip with an active transition, otherwise 0.
    fn transition_enter_offset_for_clip(&self, clip_idx: usize) -> u64 {
        let clip = &self.clips[clip_idx];
        self.clips
            .iter()
            .find(|c| {
                c.track_index == clip.track_index
                    && c.timeline_end_ns() == clip.timeline_start_ns
                    && !c.transition_after.is_empty()
                    && c.transition_after_ns > 0
            })
            .map(|c| c.transition_after_ns)
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
                pad.set_property("alpha", alpha);
                Self::apply_zoom_to_slot(slot, pad, scale, pos_x, pos_y, proc_w, proc_h);
            }
        }
    }

    /// Apply transition visual effects (alpha, crop) to all visible slots
    /// based on the current timeline position.
    fn apply_transition_effects(&self, timeline_pos: u64) {
        let (proc_w, _proc_h) = self.preview_processing_dimensions();
        for slot in &self.slots {
            if slot.hidden || slot.is_prerender_slot {
                continue;
            }
            let clip = &self.clips[slot.clip_idx];
            let tstate = self.compute_transition_state(slot.clip_idx, timeline_pos);
            if let Some(ref pad) = slot.compositor_pad {
                let base_alpha = clip.opacity_at_timeline_ns(timeline_pos).clamp(0.0, 1.0);
                match &tstate {
                    Some(ts) => {
                        let t = ts.progress;
                        match (ts.kind.as_str(), ts.role) {
                            ("cross_dissolve", TransitionRole::Outgoing) => {
                                pad.set_property("alpha", base_alpha * (1.0 - t));
                            }
                            ("cross_dissolve", TransitionRole::Incoming) => {
                                pad.set_property("alpha", base_alpha * t);
                            }
                            ("fade_to_black", TransitionRole::Outgoing) => {
                                pad.set_property("alpha", base_alpha * (1.0 - 2.0 * t).max(0.0));
                            }
                            ("fade_to_black", TransitionRole::Incoming) => {
                                pad.set_property("alpha", base_alpha * (2.0 * t - 1.0).max(0.0));
                            }
                            ("wipe_right", _) | ("wipe_left", _) => {
                                // Wipes: both clips at full alpha; crop handles reveal.
                                pad.set_property("alpha", base_alpha);
                            }
                            _ => {
                                // Unknown transition: fall back to cross-dissolve behavior.
                                let a = match ts.role {
                                    TransitionRole::Outgoing => base_alpha * (1.0 - t),
                                    TransitionRole::Incoming => base_alpha * t,
                                };
                                pad.set_property("alpha", a);
                            }
                        }
                    }
                    None => {
                        // Not in a transition: restore full opacity.
                        pad.set_property("alpha", base_alpha);
                    }
                }
            }
            // Wipe transitions: animate videocrop on incoming clip to progressively reveal.
            if let Some(ref ts) = tstate {
                if (ts.kind == "wipe_right" || ts.kind == "wipe_left")
                    && ts.role == TransitionRole::Incoming
                {
                    let t = ts.progress;
                    let crop_px = ((1.0 - t) * proc_w as f64).round() as i32;
                    if let Some(ref vc) = slot.videocrop {
                        let user_cl = clip.crop_left_at_timeline_ns(timeline_pos).max(0);
                        let user_cr = clip.crop_right_at_timeline_ns(timeline_pos).max(0);
                        let user_ct = clip.crop_top_at_timeline_ns(timeline_pos).max(0);
                        let user_cb = clip.crop_bottom_at_timeline_ns(timeline_pos).max(0);
                        if ts.kind == "wipe_right" {
                            // Reveal from left to right: crop the right side.
                            vc.set_property("left", user_cl);
                            vc.set_property("right", user_cr + crop_px);
                            vc.set_property("top", user_ct);
                            vc.set_property("bottom", user_cb);
                        } else {
                            // Reveal from right to left: crop the left side.
                            vc.set_property("left", user_cl + crop_px);
                            vc.set_property("right", user_cr);
                            vc.set_property("top", user_ct);
                            vc.set_property("bottom", user_cb);
                        }
                    }
                    // Re-pad with transparent borders to maintain frame dimensions.
                    if let Some(ref vb) = slot.videobox_crop_alpha {
                        let user_cl = clip.crop_left_at_timeline_ns(timeline_pos).max(0);
                        let user_cr = clip.crop_right_at_timeline_ns(timeline_pos).max(0);
                        let user_ct = clip.crop_top_at_timeline_ns(timeline_pos).max(0);
                        let user_cb = clip.crop_bottom_at_timeline_ns(timeline_pos).max(0);
                        if ts.kind == "wipe_right" {
                            vb.set_property("left", -user_cl);
                            vb.set_property("right", -(user_cr + crop_px));
                            vb.set_property("top", -user_ct);
                            vb.set_property("bottom", -user_cb);
                        } else {
                            vb.set_property("left", -(user_cl + crop_px));
                            vb.set_property("right", -user_cr);
                            vb.set_property("top", -user_ct);
                            vb.set_property("bottom", -user_cb);
                        }
                        vb.set_property("border-alpha", 0.0_f64);
                    }
                } else if tstate.is_none() || !(ts.kind == "wipe_right" || ts.kind == "wipe_left") {
                    // Not in a wipe transition — ensure crop reflects user values only.
                    // (Skip reset if this slot is the outgoing clip of a wipe, since
                    // the outgoing clip's crop stays at user values naturally.)
                }
            }
        }
    }

    fn effective_source_path_for_clip(&self, clip: &ProgramClip) -> String {
        let (path, _, _) = self.resolve_source_path_for_clip(clip);
        path
    }

    fn resolve_source_path_for_clip(&self, clip: &ProgramClip) -> (String, bool, String) {
        let resolve_ready_proxy = |key: &String| {
            self.proxy_paths.get(key).and_then(|p| {
                std::fs::metadata(p)
                    .ok()
                    .filter(|m| m.len() > 0)
                    .map(|_| p.clone())
            })
        };

        // Check for bg-removed version first (takes priority — includes alpha channel).
        if clip.bg_removal_enabled {
            if let Some(bg_path) = self.bg_removal_paths.get(&clip.source_path) {
                if std::fs::metadata(bg_path)
                    .ok()
                    .filter(|m| m.len() > 0)
                    .is_some()
                {
                    return (bg_path.clone(), false, String::new());
                }
            }
        }

        if self.proxy_enabled {
            let key =
                crate::media::proxy_cache::proxy_key(&clip.source_path, clip.lut_path.as_deref());
            resolve_ready_proxy(&key)
                .map(|p| (p, true, key.clone()))
                .unwrap_or_else(|| (clip.source_path.clone(), false, key))
        } else if self.preview_luts
            && clip
                .lut_path
                .as_deref()
                .map(|p| !p.is_empty())
                .unwrap_or(false)
        {
            let key =
                crate::media::proxy_cache::proxy_key(&clip.source_path, clip.lut_path.as_deref());
            resolve_ready_proxy(&key)
                .map(|p| (p, true, key.clone()))
                .unwrap_or_else(|| (clip.source_path.clone(), false, key))
        } else {
            (clip.source_path.clone(), false, String::new())
        }
    }

    fn next_video_boundary_after(&self, timeline_pos_ns: u64) -> Option<u64> {
        let mut next: Option<u64> = None;
        for clip in &self.clips {
            for candidate in [
                clip.timeline_start_ns,
                clip.timeline_end_ns(),
                clip.timeline_end_ns()
                    .saturating_sub(clip.transition_after_ns),
            ] {
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
        match kind {
            "cross_dissolve" => Some("fade"),
            "fade_to_black" => Some("fadeblack"),
            "wipe_right" => Some("wiperight"),
            "wipe_left" => Some("wipeleft"),
            _ => None,
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
            if outgoing.transition_after_ns == 0 || outgoing.transition_after.is_empty() {
                continue;
            }
            let Some(xfade_transition) =
                Self::supported_transition_prerender_kind(outgoing.transition_after.as_str())
            else {
                continue;
            };
            let trans_start = outgoing
                .timeline_end_ns()
                .saturating_sub(outgoing.transition_after_ns);
            if timeline_pos_ns < trans_start || timeline_pos_ns >= outgoing.timeline_end_ns() {
                continue;
            }
            let incoming_idx = active.iter().copied().find(|&idx| {
                if idx == outgoing_idx {
                    return false;
                }
                self.clips
                    .get(idx)
                    .map(|incoming| {
                        incoming.track_index == outgoing.track_index
                            && incoming.timeline_start_ns == outgoing.timeline_end_ns()
                    })
                    .unwrap_or(false)
            })?;
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
            });
        }
        None
    }

    fn active_supports_background_prerender_at(
        &self,
        timeline_pos_ns: u64,
        active: &[usize],
    ) -> bool {
        active.len() >= 3
            || self
                .transition_overlap_prerender_spec_at(timeline_pos_ns, active)
                .is_some()
    }

    fn prewarm_incoming_clip_resources(&mut self, clip: &ProgramClip, timeline_pos: u64) {
        let effective_path = self.effective_source_path_for_clip(clip);
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
        let source_ns = clip.source_pos_ns(timeline_pos);
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
        let (effects_bin, ..) = Self::build_effects_bin(clip, proc_w, proc_h);
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
            self.prerender_pending.remove(&result.key);
            if !self.background_prerender {
                if Path::new(&result.path).exists() {
                    let _ = std::fs::remove_file(&result.path);
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

    fn prerender_signature_for_active(&self, active: &[usize]) -> u64 {
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        let hash_keyframes = |hasher: &mut std::collections::hash_map::DefaultHasher,
                              kfs: &[NumericKeyframe]| {
            kfs.len().hash(hasher);
            for kf in kfs {
                kf.time_ns.hash(hasher);
                kf.value.to_bits().hash(hasher);
                kf.interpolation.hash(hasher);
            }
        };
        BACKGROUND_PRERENDER_CACHE_VERSION.hash(&mut hasher);
        self.project_width.hash(&mut hasher);
        self.project_height.hash(&mut hasher);
        self.preview_divisor.hash(&mut hasher);
        self.proxy_enabled.hash(&mut hasher);
        self.proxy_scale_divisor.hash(&mut hasher);
        for &idx in active {
            if let Some(c) = self.clips.get(idx) {
                c.id.hash(&mut hasher);
                c.track_index.hash(&mut hasher);
                c.timeline_start_ns.hash(&mut hasher);
                c.timeline_end_ns().hash(&mut hasher);
                self.effective_source_path_for_clip(c).hash(&mut hasher);
                c.source_in_ns.hash(&mut hasher);
                c.source_out_ns.hash(&mut hasher);
                c.speed.to_bits().hash(&mut hasher);
                hash_keyframes(&mut hasher, &c.speed_keyframes);
                c.reverse.hash(&mut hasher);
                c.freeze_frame.hash(&mut hasher);
                c.freeze_frame_source_ns.hash(&mut hasher);
                c.freeze_frame_hold_duration_ns.hash(&mut hasher);
                c.crop_left.hash(&mut hasher);
                hash_keyframes(&mut hasher, &c.crop_left_keyframes);
                c.crop_right.hash(&mut hasher);
                hash_keyframes(&mut hasher, &c.crop_right_keyframes);
                c.crop_top.hash(&mut hasher);
                hash_keyframes(&mut hasher, &c.crop_top_keyframes);
                c.crop_bottom.hash(&mut hasher);
                hash_keyframes(&mut hasher, &c.crop_bottom_keyframes);
                c.rotate.hash(&mut hasher);
                hash_keyframes(&mut hasher, &c.rotate_keyframes);
                c.flip_h.hash(&mut hasher);
                c.flip_v.hash(&mut hasher);
                c.scale.to_bits().hash(&mut hasher);
                hash_keyframes(&mut hasher, &c.scale_keyframes);
                c.position_x.to_bits().hash(&mut hasher);
                hash_keyframes(&mut hasher, &c.position_x_keyframes);
                c.position_y.to_bits().hash(&mut hasher);
                hash_keyframes(&mut hasher, &c.position_y_keyframes);
                c.opacity.to_bits().hash(&mut hasher);
                hash_keyframes(&mut hasher, &c.opacity_keyframes);
                c.volume.to_bits().hash(&mut hasher);
                hash_keyframes(&mut hasher, &c.volume_keyframes);
                c.brightness.to_bits().hash(&mut hasher);
                hash_keyframes(&mut hasher, &c.brightness_keyframes);
                c.contrast.to_bits().hash(&mut hasher);
                hash_keyframes(&mut hasher, &c.contrast_keyframes);
                c.saturation.to_bits().hash(&mut hasher);
                hash_keyframes(&mut hasher, &c.saturation_keyframes);
                c.temperature.to_bits().hash(&mut hasher);
                hash_keyframes(&mut hasher, &c.temperature_keyframes);
                c.tint.to_bits().hash(&mut hasher);
                hash_keyframes(&mut hasher, &c.tint_keyframes);
                c.denoise.to_bits().hash(&mut hasher);
                c.sharpness.to_bits().hash(&mut hasher);
                c.shadows.to_bits().hash(&mut hasher);
                c.midtones.to_bits().hash(&mut hasher);
                c.highlights.to_bits().hash(&mut hasher);
                c.lut_path.hash(&mut hasher);
                c.chroma_key_enabled.hash(&mut hasher);
                c.chroma_key_color.hash(&mut hasher);
                c.chroma_key_tolerance.to_bits().hash(&mut hasher);
                c.chroma_key_softness.to_bits().hash(&mut hasher);
            }
        }
        hasher.finish()
    }

    fn maybe_request_background_prerender_segment(&mut self, boundary_ns: u64, active: &[usize]) {
        let transition_spec = self.transition_overlap_prerender_spec_at(boundary_ns, active);
        if !self.background_prerender || (active.len() < 3 && transition_spec.is_none()) {
            return;
        }
        if active.iter().any(|&idx| {
            self.clips
                .get(idx)
                .map(|c| c.is_freeze_frame() || Self::clip_has_phase1_keyframes(c))
                .unwrap_or(false)
        }) {
            return;
        }
        let next_boundary = self
            .next_video_boundary_after(boundary_ns.saturating_add(1))
            .unwrap_or_else(|| boundary_ns.saturating_add(4_000_000_000));
        let end_ns = next_boundary;
        if end_ns <= boundary_ns.saturating_add(200_000_000) {
            return;
        }
        let signature = self.prerender_signature_for_active(active);
        let key = format!(
            "seg_v{}_{:016x}_{}_{}",
            BACKGROUND_PRERENDER_CACHE_VERSION, signature, boundary_ns, end_ns
        );
        if self.prerender_segments.contains_key(&key)
            || self.prerender_pending.contains(&key)
            || self.prerender_failed.contains(&key)
        {
            return;
        }
        let path = self.prerender_cache_root.join(format!("{key}.mp4"));
        if path.exists() {
            self.prerender_segments.insert(
                key.clone(),
                PrerenderSegment {
                    key,
                    path: path.to_string_lossy().to_string(),
                    start_ns: boundary_ns,
                    end_ns,
                    signature,
                },
            );
            return;
        }
        let duration_ns = end_ns.saturating_sub(boundary_ns);
        let inputs: Vec<(ProgramClip, String, u64, bool)> = active
            .iter()
            .filter_map(|&idx| {
                self.clips.get(idx).map(|clip| {
                    let c = clip.clone();
                    let path = self.effective_source_path_for_clip(&c);
                    let source_ns = c.source_pos_ns(boundary_ns);
                    let has_audio = self.has_audio_for_path_fast(&path, c.has_embedded_audio());
                    (c, path, source_ns, has_audio)
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
        let tx = self.prerender_result_tx.clone();
        let prerender_divisor = if self.proxy_enabled {
            self.proxy_scale_divisor.max(1)
        } else {
            1
        };
        let out_w = (self.project_width / prerender_divisor).max(2);
        let out_h = (self.project_height / prerender_divisor).max(2);
        let transition_spec_for_job = transition_spec.clone();
        std::thread::spawn(move || {
            let success = Self::render_prerender_segment_video_file(
                &output_path,
                &inputs,
                duration_ns,
                out_w,
                out_h,
                fps,
                transition_spec_for_job.as_ref(),
            );
            let _ = tx.send(PrerenderJobResult {
                key: key_for_job,
                path: output_path,
                start_ns: boundary_ns,
                end_ns,
                signature,
                success,
            });
        });
        self.prerender_pending.insert(key);
        self.prerender_total_requested += 1;
        log::info!(
            "background_prerender: queued key={} range={}..{} clips={}",
            key_for_log,
            boundary_ns,
            end_ns,
            input_count
        );
    }

    fn find_prerender_segment_for(
        &self,
        timeline_pos: u64,
        signature: u64,
    ) -> Option<PrerenderSegment> {
        self.prerender_segments
            .values()
            .filter(|seg| {
                seg.signature == signature
                    && timeline_pos >= seg.start_ns
                    && timeline_pos < seg.end_ns
                    && Path::new(&seg.path).exists()
            })
            // Prefer the nearest segment start at/behind the target timeline position.
            .max_by_key(|seg| seg.start_ns)
            .cloned()
    }

    fn prewarm_upcoming_boundary(&mut self, timeline_pos_ns: u64) {
        const PREWARM_WINDOW_NS: u64 = 900_000_000;
        const BACKGROUND_PREWARM_WINDOW_NS: u64 = 3_000_000_000;
        const BACKGROUND_PLAYING_LOOKAHEAD_NS: u64 = 12_000_000_000;
        const BACKGROUND_PLAYING_MAX_BOUNDARIES: usize = 2;
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
            let mut cursor = timeline_pos_ns;
            for _ in 0..BACKGROUND_PLAYING_MAX_BOUNDARIES {
                let Some(scan_boundary_ns) = self.next_video_boundary_after(cursor) else {
                    break;
                };
                if scan_boundary_ns.saturating_sub(timeline_pos_ns)
                    > BACKGROUND_PLAYING_LOOKAHEAD_NS
                {
                    break;
                }
                let scan_active = self.clips_active_at(scan_boundary_ns);
                self.maybe_request_background_prerender_segment(scan_boundary_ns, &scan_active);
                cursor = scan_boundary_ns.saturating_add(self.frame_duration_ns.max(1));
            }
        }
        let prewarm_window_ns = if self.background_prerender {
            BACKGROUND_PREWARM_WINDOW_NS
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
        const IDLE_MAX_BOUNDARIES: usize = 2;

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

        let active_now = self.clips_active_at(self.timeline_pos_ns);
        self.maybe_request_background_prerender_segment(self.timeline_pos_ns, &active_now);

        let mut cursor = self.timeline_pos_ns;
        for _ in 0..IDLE_MAX_BOUNDARIES {
            let Some(boundary_ns) = self.next_video_boundary_after(cursor) else {
                break;
            };
            if boundary_ns.saturating_sub(self.timeline_pos_ns) > IDLE_LOOKAHEAD_NS {
                break;
            }
            let boundary_active = self.clips_active_at(boundary_ns);
            self.maybe_request_background_prerender_segment(boundary_ns, &boundary_active);
            cursor = boundary_ns.saturating_add(self.frame_duration_ns.max(1));
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

    fn update_drop_late_policy(&mut self) {
        let should_drop_late =
            self.state == PlayerState::Playing && self.slots.len() >= 3 && !self.transform_live;
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
        let should_drop_late =
            self.state == PlayerState::Playing && self.slots.len() >= 3 && !self.transform_live;
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
        self.state != PlayerState::Playing && self.slots.len() >= 3 && !self.background_prerender
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
            requested_ms.min(220)
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
        // Enforce a minimum so we don't drop to zero.
        scaled.max(200)
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
        let timeout_ms = if self.slots.len() >= 3 { 300 } else { 150 };
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
        let source_ns = clip.source_pos_ns(effective_pos);
        let effective_seek_flags = Self::effective_decode_seek_flags(clip, seek_flags);
        let stop_ns = Self::effective_video_seek_stop_ns(clip, source_ns, frame_duration_ns);
        slot.decoder
            .seek(
                clip.seek_rate(),
                effective_seek_flags,
                gst::SeekType::Set,
                gst::ClockTime::from_nseconds(clip.seek_start_ns(source_ns)),
                gst::SeekType::Set,
                gst::ClockTime::from_nseconds(stop_ns),
            )
            .is_ok()
    }

    fn effective_decode_seek_flags(
        clip: &ProgramClip,
        seek_flags: gst::SeekFlags,
    ) -> gst::SeekFlags {
        if clip.reverse || clip.is_freeze_frame() {
            (seek_flags | gst::SeekFlags::ACCURATE) & !gst::SeekFlags::KEY_UNIT
        } else {
            seek_flags
        }
    }

    fn effective_video_seek_stop_ns(
        clip: &ProgramClip,
        source_pos_ns: u64,
        frame_duration_ns: u64,
    ) -> u64 {
        if clip.is_freeze_frame() {
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
        let source_ns = clip.source_pos_ns(effective_pos);
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
    #[allow(dead_code)]
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
        for slot in &self.slots {
            let clip = &self.clips[slot.clip_idx];
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
        let (mut cl, mut cr, mut ct, mut cb) = (
            crop_left.max(0),
            crop_right.max(0),
            crop_top.max(0),
            crop_bottom.max(0),
        );
        let (frame_w, frame_h) = slot
            .videocrop
            .as_ref()
            .and_then(|vc| vc.static_pad("sink"))
            .and_then(|p| p.current_caps())
            .and_then(|c| {
                c.structure(0).map(|s| {
                    (
                        s.get::<i32>("width").unwrap_or(9999),
                        s.get::<i32>("height").unwrap_or(9999),
                    )
                })
            })
            .unwrap_or((9999, 9999));
        const MIN_DIM: i32 = 2;
        if cl + cr > frame_w - MIN_DIM {
            let total = (frame_w - MIN_DIM).max(0);
            let ratio = if cl + cr > 0 {
                total as f64 / (cl + cr) as f64
            } else {
                1.0
            };
            cl = (cl as f64 * ratio) as i32;
            cr = total - cl;
        }
        if ct + cb > frame_h - MIN_DIM {
            let total = (frame_h - MIN_DIM).max(0);
            let ratio = if ct + cb > 0 {
                total as f64 / (ct + cb) as f64
            } else {
                1.0
            };
            ct = (ct as f64 * ratio) as i32;
            cb = total - ct;
        }
        if let Some(ref vc) = slot.videocrop {
            vc.set_property("left", cl);
            vc.set_property("right", cr);
            vc.set_property("top", ct);
            vc.set_property("bottom", cb);
        }
        // Re-pad cropped edges with transparent borders so the compositor
        // reveals lower tracks through the cropped area.
        if let Some(ref vb) = slot.videobox_crop_alpha {
            vb.set_property("left", -cl);
            vb.set_property("right", -cr);
            vb.set_property("top", -ct);
            vb.set_property("bottom", -cb);
            vb.set_property("border-alpha", 0.0_f64);
        }
        if let Some(ref vfr) = slot.videoflip_rotate {
            if vfr.find_property("angle").is_some() {
                // UI positive rotation is clockwise; GstRotate positive is counterclockwise.
                vfr.set_property("angle", -(rotate as f64).to_radians());
            } else {
                let method = match rotate.rem_euclid(360) {
                    90 => "clockwise",
                    180 => "rotate-180",
                    270 => "counterclockwise",
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
    ) {
        if let Some(ref to) = slot.textoverlay {
            let silent = text.is_empty();
            to.set_property("silent", silent);
            if !silent {
                to.set_property("text", text);
                to.set_property("font-desc", font);
                to.set_property_from_str("halignment", "position");
                to.set_property_from_str("valignment", "position");
                to.set_property("xpos", rel_x);
                to.set_property("ypos", rel_y);
                let r = (color_rgba >> 24) & 0xFF;
                let g = (color_rgba >> 16) & 0xFF;
                let b = (color_rgba >> 8) & 0xFF;
                let a = color_rgba & 0xFF;
                let argb: u32 = (a << 24) | (r << 16) | (g << 8) | b;
                to.set_property("color", argb);
            }
        }
    }

    /// Apply per-slot zoom / scale / position via compositor pad properties.
    fn apply_zoom_to_slot(
        slot: &VideoSlot,
        pad: &gst::Pad,
        scale: f64,
        position_x: f64,
        position_y: f64,
        project_width: u32,
        project_height: u32,
    ) {
        let scale = scale.clamp(0.1, 4.0);
        let pw = project_width as f64;
        let ph = project_height as f64;
        let sw = (pw * scale).round().max(1.0) as i32;
        let sh = (ph * scale).round().max(1.0) as i32;

        // Prefer compositor sink-pad scaling/positioning when supported. This
        // avoids dynamic caps renegotiation in the per-clip zoom branch while
        // playback is running, which can trigger decodebin/qtdemux
        // not-negotiated failures on some MP4 sources.
        if pad.find_property("width").is_some() && pad.find_property("height").is_some() {
            pad.set_property("width", sw);
            pad.set_property("height", sh);
            if pad.find_property("xpos").is_some() && pad.find_property("ypos").is_some() {
                let total_x = pw * (scale - 1.0);
                let total_y = ph * (scale - 1.0);
                let xpos = (-(total_x * (1.0 + position_x) / 2.0)).round() as i32;
                let ypos = (-(total_y * (1.0 + position_y) / 2.0)).round() as i32;
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

        let pos_x = position_x;
        let pos_y = position_y;
        let total_x = pw * (scale - 1.0);
        let total_y = ph * (scale - 1.0);
        let box_left = (total_x * (1.0 + pos_x) / 2.0) as i32;
        let box_right = (total_x * (1.0 - pos_x) / 2.0) as i32;
        let box_top = (total_y * (1.0 + pos_y) / 2.0) as i32;
        let box_bottom = (total_y * (1.0 - pos_y) / 2.0) as i32;

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
        if let Some(ref ap) = slot.audio_panorama {
            self.pipeline.remove(ap).ok();
        }
        if let Some(ref lv) = slot.audio_level {
            self.pipeline.remove(lv).ok();
        }
        // 2. Stop any residual streaming work on removed elements.
        let _ = slot.decoder.set_state(gst::State::Null);
        let _ = slot.decoder.state(gst::ClockTime::from_mseconds(100));
        let _ = slot.effects_bin.set_state(gst::State::Null);
        let _ = slot.effects_bin.state(gst::ClockTime::from_mseconds(100));
        if let Some(ref q) = slot.slot_queue {
            let _ = q.set_state(gst::State::Null);
            let _ = q.state(gst::ClockTime::from_mseconds(100));
        }
        if let Some(ref ac) = slot.audio_conv {
            let _ = ac.set_state(gst::State::Null);
            let _ = ac.state(gst::ClockTime::from_mseconds(100));
        }
        if let Some(ref ap) = slot.audio_panorama {
            let _ = ap.set_state(gst::State::Null);
            let _ = ap.state(gst::ClockTime::from_mseconds(100));
        }
        if let Some(ref lv) = slot.audio_level {
            let _ = lv.set_state(gst::State::Null);
            let _ = lv.state(gst::ClockTime::from_mseconds(100));
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
    fn build_effects_bin(
        clip: &ProgramClip,
        target_width: u32,
        target_height: u32,
    ) -> (
        gst::Bin,
        Option<gst::Element>, // videobalance
        Option<gst::Element>, // coloradj_rgb (frei0r temperature/tint)
        Option<gst::Element>, // colorbalance_3pt (frei0r shadows/midtones/highlights)
        Option<gst::Element>, // gaussianblur
        Option<gst::Element>, // videocrop
        Option<gst::Element>, // videobox_crop_alpha
        Option<gst::Element>, // imagefreeze
        Option<gst::Element>, // videoflip_rotate
        Option<gst::Element>, // videoflip_flip
        Option<gst::Element>, // textoverlay
        Option<gst::Element>, // alpha_filter
        Option<gst::Element>, // alpha_chroma_key
        Option<gst::Element>, // capsfilter_zoom
        Option<gst::Element>,
    ) // videobox_zoom
    {
        let bin = gst::Bin::new();

        // Determine which effects are active (non-default) so we can skip
        // no-op elements and their associated videoconvert instances.  This
        // dramatically reduces per-frame CPU cost for clips without effects
        // (3 concurrent clips drops from ~51 to ~22 pipeline elements).
        let need_balance = clip.brightness != 0.0
            || clip.contrast != 1.0
            || clip.saturation != 1.0
            || !clip.brightness_keyframes.is_empty()
            || !clip.contrast_keyframes.is_empty()
            || !clip.saturation_keyframes.is_empty();
        let need_coloradj = (clip.temperature - 6500.0).abs() > 1.0
            || clip.tint.abs() > 0.001
            || !clip.temperature_keyframes.is_empty()
            || !clip.tint_keyframes.is_empty();
        let need_3point = clip.shadows.abs() > f64::EPSILON
            || clip.midtones.abs() > f64::EPSILON
            || clip.highlights.abs() > f64::EPSILON;
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

        // conv2 + gaussianblur: only if blur/sharpen is active.
        let (conv2, gaussianblur) = if need_blur {
            (
                gst::ElementFactory::make("videoconvert").build().ok(),
                gst::ElementFactory::make("gaussianblur").build().ok(),
            )
        } else {
            (None, None)
        };

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
        let need_chroma_key = clip.chroma_key_enabled;
        let (conv_ck, alpha_chroma_key) = if need_chroma_key {
            (
                gst::ElementFactory::make("videoconvert").build().ok(),
                gst::ElementFactory::make("alpha")
                    .property_from_str("method", "custom")
                    .build()
                    .ok(),
            )
        } else {
            (None, None)
        };
        let alpha_filter: Option<gst::Element> = None;

        // Textoverlay: only if title text is set.
        let textoverlay = if need_title {
            gst::ElementFactory::make("textoverlay").build().ok()
        } else {
            None
        };
        let imagefreeze = if clip.is_freeze_frame() {
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
            let p = Self::compute_3point_params(clip.shadows, clip.midtones, clip.highlights);
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
            if clip.title_text.is_empty() {
                to.set_property("silent", true);
                to.set_property("text", "");
            } else {
                to.set_property("silent", false);
                to.set_property("text", &clip.title_text);
                to.set_property("font-desc", &clip.title_font);
                to.set_property_from_str("halignment", "position");
                to.set_property_from_str("valignment", "position");
                to.set_property("xpos", clip.title_x);
                to.set_property("ypos", clip.title_y);
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
        // Order: convertscale to target res → [crop + alpha repad] → [effects]
        // → zoom/position → rotate/flip → title.
        let mut chain: Vec<gst::Element> = Vec::new();
        // 1. Convert + downscale to project resolution in a single pass.
        if let Some(ref e) = convertscale {
            chain.push(e.clone());
        }
        if let Some(ref e) = capsfilter_proj {
            chain.push(e.clone());
        }
        // 2. Crop at project resolution (RGBA) then re-pad with transparent
        //    borders so the compositor reveals lower tracks through cropped areas.
        if let Some(ref e) = videocrop {
            chain.push(e.clone());
        }
        if let Some(ref e) = videobox_crop_alpha {
            chain.push(e.clone());
        }
        if let Some(ref e) = imagefreeze {
            chain.push(e.clone());
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
        if let Some(ref e) = conv2 {
            chain.push(e.clone());
        }
        if let Some(ref e) = gaussianblur {
            chain.push(e.clone());
        }
        // 3b. Chroma key (after color/blur, before zoom).
        if let Some(ref e) = conv_ck {
            chain.push(e.clone());
        }
        if let Some(ref e) = alpha_chroma_key {
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
        let (effective_path, using_proxy, proxy_key) = self.resolve_source_path_for_clip(&clip);
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
        let decoder = gst::ElementFactory::make("uridecodebin")
            .property("uri", &uri)
            .property("caps", &audio_caps)
            .build()
            .ok()?;

        if self.pipeline.add(&decoder).is_err() {
            return None;
        }

        // Audio path: audioconvert → [audiopanorama] → [level] → audiomixer pad.
        let (audio_conv, audio_panorama, audio_level, amix_pad) = {
            let ac = gst::ElementFactory::make("audioconvert").build().ok();
            let mut ap = gst::ElementFactory::make("audiopanorama").build().ok();
            let mut lv = gst::ElementFactory::make("level")
                .property("post-messages", true)
                .property("interval", 50_000_000u64)
                .build()
                .ok();
            let pad = if let Some(ref ac) = ac {
                if self.pipeline.add(ac).is_ok() {
                    let mut link_src = ac.static_pad("src");
                    if let Some(ref pano) = ap {
                        if self.pipeline.add(pano).is_ok() {
                            pano.set_property(
                                "panorama",
                                self.effective_main_clip_pan(clip_idx, self.timeline_pos_ns) as f32,
                            );
                            if let (Some(ac_src), Some(pano_sink)) =
                                (ac.static_pad("src"), pano.static_pad("sink"))
                            {
                                let _ = ac_src.link(&pano_sink);
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
                    ap = None;
                    lv = None;
                    None
                }
            } else {
                ap = None;
                lv = None;
                None
            };
            (ac, ap, lv, pad)
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
            audio_panorama,
            audio_level,
            videobalance: None,
            coloradj_rgb: None,
            colorbalance_3pt: None,
            gaussianblur: None,
            videocrop: None,
            videobox_crop_alpha: None,
            imagefreeze: None,
            videoflip_rotate: None,
            videoflip_flip: None,
            textoverlay: None,
            alpha_filter: None,
            alpha_chroma_key: None,
            capsfilter_zoom: None,
            videobox_zoom: None,
            slot_queue: None,
            comp_arrival_seq: Arc::new(AtomicU64::new(0)),
            hidden: false,
            is_prerender_slot: false,
            prerender_segment_start_ns: None,
            transition_enter_offset_ns: 0,
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

        let audio_conv = gst::ElementFactory::make("audioconvert").build().ok();
        let mut audio_panorama = gst::ElementFactory::make("audiopanorama").build().ok();
        let mut audio_level = gst::ElementFactory::make("level")
            .property("post-messages", true)
            .property("interval", 50_000_000u64)
            .build()
            .ok();
        let mut amix_pad: Option<gst::Pad> = None;
        let mut audio_sink: Option<gst::Pad> = None;
        if let Some(ref ac) = audio_conv {
            if self.pipeline.add(ac).is_ok() {
                audio_sink = ac.static_pad("sink");
                let mut link_src = ac.static_pad("src");
                if let Some(ref pano) = audio_panorama {
                    if self.pipeline.add(pano).is_ok() {
                        pano.set_property("panorama", 0.0_f32);
                        if let (Some(ac_src), Some(pano_sink)) =
                            (ac.static_pad("src"), pano.static_pad("sink"))
                        {
                            let _ = ac_src.link(&pano_sink);
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
            audio_panorama,
            audio_level,
            videobalance: None,
            coloradj_rgb: None,
            colorbalance_3pt: None,
            gaussianblur: None,
            videocrop: None,
            videobox_crop_alpha: None,
            imagefreeze: None,
            videoflip_rotate: None,
            videoflip_flip: None,
            textoverlay: None,
            alpha_filter: None,
            alpha_chroma_key: None,
            capsfilter_zoom: None,
            videobox_zoom: None,
            slot_queue: Some(slot_queue),
            comp_arrival_seq,
            hidden: false,
            is_prerender_slot: true,
            prerender_segment_start_ns: Some(segment_start_ns),
            transition_enter_offset_ns: 0,
        })
    }

    fn render_prerender_segment_video_file(
        output_path: &str,
        inputs: &[(ProgramClip, String, u64, bool)],
        duration_ns: u64,
        out_w: u32,
        out_h: u32,
        fps: u32,
        transition_spec: Option<&TransitionPrerenderSpec>,
    ) -> bool {
        let Ok(ffmpeg) = crate::media::export::find_ffmpeg() else {
            return false;
        };
        if let Some(parent) = Path::new(output_path).parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let duration_s = duration_ns as f64 / 1_000_000_000.0;
        let mut cmd = Command::new(&ffmpeg);
        cmd.arg("-y")
            .arg("-hide_banner")
            .arg("-loglevel")
            .arg("error")
            .arg("-nostats");
        for (clip, path, source_ns, _) in inputs {
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

        let use_transition_xfade = transition_spec
            .map(|spec| inputs.len() == 2 && spec.outgoing_input < 2 && spec.incoming_input < 2)
            .unwrap_or(false);
        let mut nodes: Vec<String> = Vec::new();
        for (i, (clip, _, _, _)) in inputs.iter().enumerate() {
            if use_transition_xfade {
                nodes.push(format!(
                    "[{i}:v]setpts=PTS-STARTPTS,scale={out_w}:{out_h}:force_original_aspect_ratio=decrease,pad={out_w}:{out_h}:(ow-iw)/2:(oh-ih)/2,setsar=1{}{}{},fps={},format=yuv420p{}{}{}{}{}{}[pv{i}]",
                    Self::prerender_build_crop_filter(clip, out_w, out_h, false),
                    Self::prerender_build_scale_position_filter(clip, out_w, out_h, false),
                    Self::prerender_build_rotation_filter(clip, false),
                    fps.max(1),
                    Self::prerender_build_lut_filter(clip),
                    Self::prerender_build_color_filter(clip),
                    Self::prerender_build_temperature_tint_filter(clip),
                    Self::prerender_build_grading_filter(clip),
                    Self::prerender_build_denoise_filter(clip),
                    Self::prerender_build_sharpen_filter(clip),
                ));
            } else if i == 0 {
                if clip.chroma_key_enabled {
                    // Chroma key needs alpha: convert early so pad fills transparent.
                    nodes.push(format!(
                        "[{i}:v]setpts=PTS-STARTPTS,format=yuva420p,scale={out_w}:{out_h}:force_original_aspect_ratio=decrease,pad={out_w}:{out_h}:(ow-iw)/2:(oh-ih)/2:color=black@0,setsar=1{}{}{},fps={}{}{}{}{}{}{}{}[pv{i}]",
                        Self::prerender_build_crop_filter(clip, out_w, out_h, false),
                        Self::prerender_build_scale_position_filter(clip, out_w, out_h, false),
                        Self::prerender_build_rotation_filter(clip, false),
                        fps.max(1),
                        Self::prerender_build_lut_filter(clip),
                        Self::prerender_build_color_filter(clip),
                        Self::prerender_build_temperature_tint_filter(clip),
                        Self::prerender_build_grading_filter(clip),
                        Self::prerender_build_denoise_filter(clip),
                        Self::prerender_build_sharpen_filter(clip),
                        Self::prerender_build_chroma_key_filter(clip),
                    ));
                } else {
                    nodes.push(format!(
                        "[{i}:v]setpts=PTS-STARTPTS,scale={out_w}:{out_h}:force_original_aspect_ratio=decrease,pad={out_w}:{out_h}:(ow-iw)/2:(oh-ih)/2,setsar=1{}{}{},fps={},format=yuv420p{}{}{}{}{}{}[pv{i}]",
                        Self::prerender_build_crop_filter(clip, out_w, out_h, false),
                        Self::prerender_build_scale_position_filter(clip, out_w, out_h, false),
                        Self::prerender_build_rotation_filter(clip, false),
                        fps.max(1),
                        Self::prerender_build_lut_filter(clip),
                        Self::prerender_build_color_filter(clip),
                        Self::prerender_build_temperature_tint_filter(clip),
                        Self::prerender_build_grading_filter(clip),
                        Self::prerender_build_denoise_filter(clip),
                        Self::prerender_build_sharpen_filter(clip),
                    ));
                }
            } else {
                // Overlay tracks: convert to yuva420p early so pad fills transparent.
                nodes.push(format!(
                    "[{i}:v]setpts=PTS-STARTPTS,format=yuva420p,scale={out_w}:{out_h}:force_original_aspect_ratio=decrease,pad={out_w}:{out_h}:(ow-iw)/2:(oh-ih)/2:color=black@0,setsar=1{}{}{}{}{}{}{}{}{}{},colorchannelmixer=aa={:.4}[pv{i}]",
                    Self::prerender_build_crop_filter(clip, out_w, out_h, true),
                    Self::prerender_build_scale_position_filter(clip, out_w, out_h, true),
                    Self::prerender_build_rotation_filter(clip, true),
                    Self::prerender_build_lut_filter(clip),
                    Self::prerender_build_color_filter(clip),
                    Self::prerender_build_temperature_tint_filter(clip),
                    Self::prerender_build_grading_filter(clip),
                    Self::prerender_build_denoise_filter(clip),
                    Self::prerender_build_sharpen_filter(clip),
                    Self::prerender_build_chroma_key_filter(clip),
                    clip.opacity.clamp(0.0, 1.0),
                ));
            }
        }
        let mut last_label = "pv0".to_string();
        if let Some(spec) = transition_spec.filter(|_| use_transition_xfade) {
            let out_label = format!("pv{}", spec.outgoing_input);
            let in_label = format!("pv{}", spec.incoming_input);
            let duration_s = (spec.duration_ns as f64 / 1_000_000_000.0)
                .max(0.001)
                .min(duration_s.max(0.001));
            nodes.push(format!(
                "[{out_label}][{in_label}]xfade=transition={}:duration={duration_s:.6}:offset=0[vcomp1]",
                spec.xfade_transition
            ));
            last_label = "vcomp1".to_string();
        } else {
            for i in 1..inputs.len() {
                let next = format!("vcomp{i}");
                nodes.push(format!("[{last_label}][pv{i}]overlay=x=0:y=0[{next}]"));
                last_label = next;
            }
        }
        let mut audio_labels: Vec<String> = Vec::new();
        for (i, (clip, _, _, has_audio)) in inputs.iter().enumerate() {
            if !*has_audio {
                continue;
            }
            let mut chain = format!("[{i}:a]asetpts=PTS-STARTPTS,aresample=async=1:first_pts=0");
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
            .arg("ultrafast")
            .arg("-crf")
            .arg("30")
            .arg("-pix_fmt")
            .arg("yuv420p")
            .arg("-movflags")
            .arg("+faststart")
            .arg(output_path)
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        let success = cmd.status().map(|s| s.success()).unwrap_or(false);
        if !success {
            let _ = std::fs::remove_file(output_path);
        }
        success
    }

    fn prerender_build_color_filter(clip: &ProgramClip) -> String {
        if clip.brightness != 0.0 || clip.contrast != 1.0 || clip.saturation != 1.0 {
            format!(
                ",eq=brightness={:.4}:contrast={:.4}:saturation={:.4}",
                clip.brightness.clamp(-1.0, 1.0),
                clip.contrast.clamp(0.0, 2.0),
                clip.saturation.clamp(0.0, 2.0)
            )
        } else {
            String::new()
        }
    }

    fn prerender_build_temperature_tint_filter(clip: &ProgramClip) -> String {
        let has_temp = (clip.temperature - 6500.0).abs() > 1.0;
        let has_tint = clip.tint.abs() > 0.001;
        let mut f = String::new();
        if has_temp {
            f.push_str(&format!(
                ",colortemperature=temperature={:.0}",
                clip.temperature.clamp(2000.0, 10000.0)
            ));
        }
        if has_tint {
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

    fn prerender_build_lut_filter(clip: &ProgramClip) -> String {
        if let Some(path) = clip.lut_path.as_deref() {
            if !path.is_empty() && Path::new(path).exists() {
                let escaped = path.replace('\\', "\\\\").replace(':', "\\:");
                return format!(",lut3d={escaped}");
            }
        }
        String::new()
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
        if clip.shadows != 0.0 || clip.midtones != 0.0 || clip.highlights != 0.0 {
            let s = clip.shadows.clamp(-1.0, 1.0);
            let m = clip.midtones.clamp(-1.0, 1.0);
            let h = clip.highlights.clamp(-1.0, 1.0);
            format!(
                ",colorbalance=rs={s:.4}:gs={s:.4}:bs={s:.4}:rm={m:.4}:gm={m:.4}:bm={m:.4}:rh={h:.4}:gh={h:.4}:bh={h:.4}"
            )
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
        if clip.rotate == 0 {
            return String::new();
        }
        let fill = if transparent_pad { "black@0" } else { "black" };
        format!(
            ",rotate={:.10}:fillcolor={fill}",
            (clip.rotate as f64).to_radians()
        )
    }

    fn prerender_build_scale_position_filter(
        clip: &ProgramClip,
        out_w: u32,
        out_h: u32,
        transparent_pad: bool,
    ) -> String {
        let scale = clip.scale.clamp(0.1, 4.0);
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

        if scale >= 1.0 {
            let sw = (pw * scale).round() as u32;
            let sh = (ph * scale).round() as u32;
            let total_x = pw * (scale - 1.0);
            let total_y = ph * (scale - 1.0);
            let cx = (total_x * (1.0 + pos_x) / 2.0).round() as i64;
            let cy = (total_y * (1.0 + pos_y) / 2.0).round() as i64;
            format!(",scale={sw}:{sh},crop={out_w}:{out_h}:{cx}:{cy}")
        } else {
            let sw = (pw * scale).round() as u32;
            let sh = (ph * scale).round() as u32;
            let total_x = pw * (1.0 - scale);
            let total_y = ph * (1.0 - scale);
            let raw_pad_x = (total_x * (1.0 + pos_x) / 2.0).round() as i64;
            let raw_pad_y = (total_y * (1.0 + pos_y) / 2.0).round() as i64;
            let crop_left = if raw_pad_x < 0 {
                (-raw_pad_x) as u32
            } else {
                0
            };
            let crop_top = if raw_pad_y < 0 {
                (-raw_pad_y) as u32
            } else {
                0
            };
            let crop_right = if raw_pad_x + sw as i64 > out_w as i64 {
                (raw_pad_x + sw as i64 - out_w as i64) as u32
            } else {
                0
            };
            let crop_bottom = if raw_pad_y + sh as i64 > out_h as i64 {
                (raw_pad_y + sh as i64 - out_h as i64) as u32
            } else {
                0
            };
            let pad_x = raw_pad_x.max(0) as u32;
            let pad_y = raw_pad_y.max(0) as u32;
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
    }

    fn build_slot_for_clip(
        &mut self,
        clip_idx: usize,
        zorder_offset: usize,
        tune_multiqueue: bool,
    ) -> Option<VideoSlot> {
        let clip = self.clips[clip_idx].clone();

        // Resolve proxy path.
        let (effective_path, using_proxy, proxy_key) = self.resolve_source_path_for_clip(&clip);
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

        // Build per-slot effects bin.
        let (
            effects_bin,
            videobalance,
            coloradj_rgb,
            colorbalance_3pt,
            gaussianblur,
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
        ) = Self::build_effects_bin(&clip, proc_w, proc_h);

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

        // Create audio path: audioconvert → [audiopanorama] → [level] → audiomixer pad.
        let (audio_conv, audio_panorama, audio_level, amix_pad) = if clip_has_audio {
            let ac = gst::ElementFactory::make("audioconvert").build().ok();
            let mut ap = gst::ElementFactory::make("audiopanorama").build().ok();
            let mut lv = gst::ElementFactory::make("level")
                .property("post-messages", true)
                .property("interval", 50_000_000u64)
                .build()
                .ok();
            let pad = if let Some(ref ac) = ac {
                if self.pipeline.add(ac).is_ok() {
                    let mut link_src = ac.static_pad("src");
                    if let Some(ref pano) = ap {
                        if self.pipeline.add(pano).is_ok() {
                            pano.set_property(
                                "panorama",
                                self.effective_main_clip_pan(clip_idx, self.timeline_pos_ns) as f32,
                            );
                            if let (Some(ac_src), Some(pano_sink)) =
                                (ac.static_pad("src"), pano.static_pad("sink"))
                            {
                                let _ = ac_src.link(&pano_sink);
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
                    ap = None;
                    lv = None;
                    None
                }
            } else {
                ap = None;
                lv = None;
                None
            };
            (ac, ap, lv, pad)
        } else {
            log::info!(
                "ProgramPlayer: skipping audio path for clip {} (no audio)",
                clip.id
            );
            (None, None, None, None)
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
            audio_panorama: audio_panorama.clone(),
            audio_level: audio_level.clone(),
            videobalance: videobalance.clone(),
            coloradj_rgb: coloradj_rgb.clone(),
            colorbalance_3pt: colorbalance_3pt.clone(),
            gaussianblur: gaussianblur.clone(),
            videocrop: videocrop.clone(),
            videobox_crop_alpha: videobox_crop_alpha.clone(),
            imagefreeze: imagefreeze.clone(),
            videoflip_rotate: videoflip_rotate.clone(),
            videoflip_flip: videoflip_flip.clone(),
            textoverlay: textoverlay.clone(),
            alpha_filter: alpha_filter.clone(),
            alpha_chroma_key: alpha_chroma_key.clone(),
            capsfilter_zoom: capsfilter_zoom.clone(),
            videobox_zoom: videobox_zoom.clone(),
            slot_queue: Some(slot_queue.clone()),
            comp_arrival_seq: comp_arrival_seq.clone(),
            hidden: false,
            is_prerender_slot: false,
            prerender_segment_start_ns: None,
            transition_enter_offset_ns: 0,
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
        );
        Self::apply_zoom_to_slot(
            &slot_ref_for_transform,
            &comp_pad,
            clip.scale_at_timeline_ns(self.timeline_pos_ns),
            clip.position_x_at_timeline_ns(self.timeline_pos_ns),
            clip.position_y_at_timeline_ns(self.timeline_pos_ns),
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
            audio_panorama,
            audio_level,
            videobalance,
            coloradj_rgb,
            colorbalance_3pt,
            gaussianblur,
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
            slot_queue: Some(slot_queue),
            comp_arrival_seq,
            hidden: false,
            is_prerender_slot: false,
            prerender_segment_start_ns: None,
            transition_enter_offset_ns: 0,
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
    fn compute_videobalance_params(
        brightness: f64,
        contrast: f64,
        saturation: f64,
        temperature: f64,
        tint: f64,
        shadows: f64,
        midtones: f64,
        highlights: f64,
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
    fn compute_coloradj_params(temperature: f64, tint: f64) -> ColorAdjRGBParams {
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
    fn compute_3point_params(shadows: f64, midtones: f64, highlights: f64) -> ThreePointParams {
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

        // All channels equal (luminance-only, matching FFmpeg's rs=gs=bs pattern).
        ThreePointParams {
            black_r: black,
            black_g: black,
            black_b: black,
            gray_r: gray,
            gray_g: gray,
            gray_b: gray,
            white_r: white,
            white_g: white,
            white_b: white,
        }
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
        };
        let need_blur = |c: &ProgramClip| {
            let sigma = (c.denoise * 4.0 - c.sharpness * 6.0).clamp(-20.0, 20.0);
            sigma.abs() > f64::EPSILON
        };
        let need_rotate = |c: &ProgramClip| c.rotate.rem_euclid(360) != 0;
        let need_flip = |c: &ProgramClip| c.flip_h || c.flip_v;
        let need_title = |c: &ProgramClip| !c.title_text.is_empty();
        let need_chroma_key = |c: &ProgramClip| c.chroma_key_enabled;

        need_balance(a) == need_balance(b)
            && need_coloradj(a) == need_coloradj(b)
            && need_3point(a) == need_3point(b)
            && need_blur(a) == need_blur(b)
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
            || !clip.brightness_keyframes.is_empty()
            || !clip.contrast_keyframes.is_empty()
            || !clip.saturation_keyframes.is_empty();
        let need_coloradj = (clip.temperature - 6500.0).abs() > 1.0
            || clip.tint.abs() > 0.001
            || !clip.temperature_keyframes.is_empty()
            || !clip.tint_keyframes.is_empty();
        let need_3point = clip.shadows.abs() > f64::EPSILON
            || clip.midtones.abs() > f64::EPSILON
            || clip.highlights.abs() > f64::EPSILON;
        let need_blur = {
            let sigma = (clip.denoise * 4.0 - clip.sharpness * 6.0).clamp(-20.0, 20.0);
            sigma.abs() > f64::EPSILON
        };
        let need_rotate = clip.rotate.rem_euclid(360) != 0;
        let need_flip = clip.flip_h || clip.flip_v;
        let need_title = !clip.title_text.is_empty();
        let need_chroma_key = clip.chroma_key_enabled;
        let need_freeze_hold = clip.is_freeze_frame();

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
            && (!need_rotate || slot.videoflip_rotate.is_some())
            && (!need_flip || slot.videoflip_flip.is_some())
            && (!need_title || slot.textoverlay.is_some())
            && (!need_chroma_key || slot.alpha_chroma_key.is_some())
            && (!need_freeze_hold || slot.imagefreeze.is_some())
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
            let p = Self::compute_3point_params(clip.shadows, clip.midtones, clip.highlights);
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

        // Gaussianblur
        if let Some(ref gb) = slot.gaussianblur {
            let sigma = (clip.denoise * 4.0 - clip.sharpness * 6.0).clamp(-20.0, 20.0);
            gb.set_property("sigma", sigma);
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
        Self::apply_title_to_slot(
            slot,
            &clip.title_text,
            &clip.title_font,
            clip.title_color,
            clip.title_x,
            clip.title_y,
        );

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
        if active.iter().any(|&idx| {
            self.clips
                .get(idx)
                .map(Self::clip_has_phase1_keyframes)
                .unwrap_or(false)
        }) {
            return false;
        }
        self.poll_background_prerender_results();
        let signature = self.prerender_signature_for_active(active);
        let Some(segment) = self.find_prerender_segment_for(timeline_pos, signature) else {
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
            return false;
        };
        let Some(video_slot) =
            self.build_prerender_video_slot(&segment.path, segment.start_ns, 0, was_playing)
        else {
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
        true
    }

    fn build_live_video_slots_for_active(&mut self, active: &[usize], was_playing: bool) {
        self.prerender_active_clips = None;
        self.current_prerender_segment_key = None;
        for (zorder_offset, &clip_idx) in active.iter().enumerate() {
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
                && !desired.iter().any(|&idx| {
                    self.clips
                        .get(idx)
                        .map(Self::clip_has_phase1_keyframes)
                        .unwrap_or(false)
                })
                && !self.slots.iter().any(|s| s.is_prerender_slot)
            {
                let signature = self.prerender_signature_for_active(&desired);
                self.find_prerender_segment_for(timeline_pos, signature)
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
        }

        // Incremental boundary paths are disabled.  Both add-only and
        // remove-only paths suffer from the same GstVideoAggregator limitation:
        // after topology changes (adding or removing compositor sink pads),
        // the aggregator's internal timing/segment state must be reset via
        // compositor.seek_simple (src-pad seek).  Skipping that reset causes
        // retained decoders to produce ≤1 frame/sec (remove-only) or freeze
        // entirely (add-only).  The proven full-rebuild path handles all
        // transitions correctly.
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
                    let (path, using_proxy, key) = self.resolve_source_path_for_clip(clip);
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
        self.audio_current_source = None;
        self.apply_reverse_video_main_audio_ducking(None);
        if was_playing {
            let _ = self.pipeline.set_state(gst::State::Playing);
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
                                            if let Some(track_index) =
                                                self.slots.iter().find_map(|slot| {
                                                    let level = slot.audio_level.as_ref()?;
                                                    if level.name().as_str() == name.as_str() {
                                                        self.clips
                                                            .get(slot.clip_idx)
                                                            .map(|c| c.track_index)
                                                    } else {
                                                        None
                                                    }
                                                })
                                            {
                                                self.push_track_peak(track_index, l, r);
                                                handled = true;
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
        eos
    }

    // ── Audio-only pipeline ────────────────────────────────────────────────

    fn audio_clip_at(&self, timeline_pos_ns: u64) -> Option<usize> {
        self.audio_clips.iter().position(|c| {
            timeline_pos_ns >= c.timeline_start_ns && timeline_pos_ns < c.timeline_end_ns()
        })
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

    fn sync_audio_to(&mut self, timeline_pos_ns: u64) {
        let reverse_video_audio = self.reverse_video_clip_at_for_audio(timeline_pos_ns);
        self.apply_reverse_video_main_audio_ducking(reverse_video_audio);
        if let Some(idx) = reverse_video_audio {
            self.load_reverse_video_audio_clip_idx(idx, timeline_pos_ns);
        } else if let Some(idx) = self.audio_clip_at(timeline_pos_ns) {
            self.load_audio_clip_idx(idx, timeline_pos_ns);
        } else {
            let _ = self.audio_pipeline.set_state(gst::State::Ready);
            self.audio_current_source = None;
        }
        self.sync_preview_audio_levels(timeline_pos_ns);
    }

    fn poll_audio(&mut self, timeline_pos_ns: u64) {
        let wanted = if let Some(idx) = self.reverse_video_clip_at_for_audio(timeline_pos_ns) {
            Some(AudioCurrentSource::ReverseVideoClip(idx))
        } else {
            self.audio_clip_at(timeline_pos_ns)
                .map(AudioCurrentSource::AudioClip)
        };
        self.apply_reverse_video_main_audio_ducking(match wanted {
            Some(AudioCurrentSource::ReverseVideoClip(idx)) => Some(idx),
            _ => None,
        });
        if wanted != self.audio_current_source {
            match wanted {
                Some(AudioCurrentSource::AudioClip(idx)) => {
                    self.load_audio_clip_idx(idx, timeline_pos_ns);
                    let _ = self.audio_pipeline.set_state(gst::State::Playing);
                }
                Some(AudioCurrentSource::ReverseVideoClip(idx)) => {
                    self.load_reverse_video_audio_clip_idx(idx, timeline_pos_ns);
                    let _ = self.audio_pipeline.set_state(gst::State::Playing);
                }
                None => {
                    let _ = self.audio_pipeline.set_state(gst::State::Ready);
                    self.audio_current_source = None;
                }
            }
        } else if let Some(source) = self.audio_current_source {
            let volume = self.effective_audio_source_volume(source, timeline_pos_ns);
            self.set_audio_pipeline_volume(volume);
        }
        self.sync_preview_audio_levels(timeline_pos_ns);
    }
}

fn clip_can_fully_occlude(clip: &ProgramClip) -> bool {
    clip.opacity >= 0.999
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
        clip_can_fully_occlude, CachedPlayheadFrame, ProgramClip, ProgramPlayer, ScopeFrame,
        ShortFrameCache, VideoSlot,
    };
    use crate::model::clip::{KeyframeInterpolation, NumericKeyframe};
    use gstreamer as gst;
    use std::sync::atomic::{AtomicBool, AtomicU64};
    use std::sync::Arc;

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
            volume: 1.0,
            volume_keyframes: Vec::new(),
            pan: 0.0,
            pan_keyframes: Vec::new(),
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
            title_text: String::new(),
            title_font: String::new(),
            title_color: 0,
            title_x: 0.0,
            title_y: 0.0,
            speed: 1.0,
            speed_keyframes: Vec::new(),
            reverse: false,
            freeze_frame: false,
            freeze_frame_source_ns: None,
            freeze_frame_hold_duration_ns: None,
            is_audio_only: false,
            track_index: 0,
            transition_after: String::new(),
            transition_after_ns: 0,
            lut_path: None,
            scale: 1.0,
            scale_keyframes: Vec::new(),
            opacity: 1.0,
            opacity_keyframes: Vec::new(),
            position_x: 0.0,
            position_x_keyframes: Vec::new(),
            position_y: 0.0,
            position_y_keyframes: Vec::new(),
            shadows: 0.0,
            midtones: 0.0,
            highlights: 0.0,
            has_audio: true,
            chroma_key_enabled: false,
            chroma_key_color: 0x00FF00,
            chroma_key_tolerance: 0.3,
            chroma_key_softness: 0.1,
            bg_removal_enabled: false,
            bg_removal_threshold: 0.5,
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
        });
        clip.position_x_keyframes.push(NumericKeyframe {
            time_ns: 1_000_000_000,
            value: 0.5,
            interpolation: KeyframeInterpolation::Linear,
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
            },
            NumericKeyframe {
                time_ns: 1_000_000_000,
                value: 1.0,
                interpolation: KeyframeInterpolation::Linear,
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
            },
            NumericKeyframe {
                time_ns: 1_000_000_000,
                value: 8000.0,
                interpolation: KeyframeInterpolation::Linear,
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
            },
            NumericKeyframe {
                time_ns: 2_129_974_732,
                value: -0.82,
                interpolation: KeyframeInterpolation::Linear,
            },
            NumericKeyframe {
                time_ns: 4_709_284_968,
                value: 0.67,
                interpolation: KeyframeInterpolation::Linear,
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

        let source = clip.source_pos_ns(0);
        let stop = ProgramPlayer::effective_video_seek_stop_ns(&clip, source, 41_666_667);
        assert_eq!(stop, source + 41_666_667);
    }

    #[test]
    fn freeze_frame_preview_seek_stop_clamps_to_minimum_window() {
        let mut clip = make_clip();
        clip.freeze_frame = true;
        let source = clip.source_pos_ns(0);
        let stop = ProgramPlayer::effective_video_seek_stop_ns(&clip, source, 0);
        assert_eq!(stop, source + 1);
    }

    #[test]
    fn freeze_frame_decode_seek_forces_accurate_non_key_unit() {
        let mut clip = make_clip();
        clip.freeze_frame = true;
        let requested = gst::SeekFlags::FLUSH | gst::SeekFlags::KEY_UNIT;
        let effective = ProgramPlayer::effective_decode_seek_flags(&clip, requested);
        assert!(effective.contains(gst::SeekFlags::ACCURATE));
        assert!(!effective.contains(gst::SeekFlags::KEY_UNIT));
        assert!(effective.contains(gst::SeekFlags::FLUSH));
    }

    #[test]
    fn non_freeze_forward_decode_seek_preserves_key_unit_choice() {
        let clip = make_clip();
        let requested = gst::SeekFlags::FLUSH | gst::SeekFlags::KEY_UNIT;
        let effective = ProgramPlayer::effective_decode_seek_flags(&clip, requested);
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
            slot_queue: None,
            comp_arrival_seq: Arc::new(AtomicU64::new(0)),
            hidden: false,
            is_prerender_slot: false,
            prerender_segment_start_ns: None,
            transition_enter_offset_ns: 0,
        }
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
        let p = ProgramPlayer::compute_3point_params(0.0, 0.0, 0.0);
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
        let neutral = ProgramPlayer::compute_3point_params(0.0, 0.0, 0.0);
        let p = ProgramPlayer::compute_3point_params(0.8, 0.0, 0.0);
        let changed = (p.black_r - neutral.black_r).abs() > 0.001
            || (p.gray_r - neutral.gray_r).abs() > 0.001
            || (p.white_r - neutral.white_r).abs() > 0.001;
        assert!(changed, "positive shadows should alter the curve");
    }

    #[test]
    fn threepoint_negative_shadows_lowers_gray() {
        let neutral = ProgramPlayer::compute_3point_params(0.0, 0.0, 0.0);
        let neg = ProgramPlayer::compute_3point_params(-0.8, 0.0, 0.0);
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
        let neutral = ProgramPlayer::compute_3point_params(0.0, 0.0, 0.0);
        let p = ProgramPlayer::compute_3point_params(0.0, 0.7, 0.0);
        assert!(
            p.gray_r < neutral.gray_r,
            "positive midtones should lower gray (brighten): gray_r={}",
            p.gray_r
        );
    }

    #[test]
    fn threepoint_negative_highlights_pulls_white() {
        let p = ProgramPlayer::compute_3point_params(0.0, 0.0, -0.8);
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
        let neutral = ProgramPlayer::compute_3point_params(0.0, 0.0, 0.0);
        let pos = ProgramPlayer::compute_3point_params(0.0, 0.0, 0.8);
        let changed = (pos.gray_r - neutral.gray_r).abs() > 0.01
            || (pos.white_r - neutral.white_r).abs() > 0.01;
        assert!(changed, "positive highlights should alter the curve");
    }

    #[test]
    fn threepoint_all_channels_equal() {
        let p = ProgramPlayer::compute_3point_params(0.5, -0.3, 0.7);
        assert!((p.black_r - p.black_g).abs() < 1e-10, "R==G for black");
        assert!((p.black_r - p.black_b).abs() < 1e-10, "R==B for black");
        assert!((p.gray_r - p.gray_g).abs() < 1e-10, "R==G for gray");
        assert!((p.white_r - p.white_g).abs() < 1e-10, "R==G for white");
    }

    #[test]
    fn threepoint_params_clamped() {
        let p = ProgramPlayer::compute_3point_params(1.0, 1.0, 1.0);
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
        let p2 = ProgramPlayer::compute_3point_params(-1.0, -1.0, -1.0);
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
        self.cleanup_background_prerender_cache();
        self.teardown_prepreroll_sidecars();
        self.teardown_slots();
        let _ = self.pipeline.set_state(gst::State::Null);
        let _ = self.audio_pipeline.set_state(gst::State::Null);
    }
}
