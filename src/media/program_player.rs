use crate::media::player::PlayerState;
use crate::ui_state::PlaybackPriority;
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
use std::collections::HashMap;
use std::io::Write;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const MAX_PREVIEW_AUDIO_GAIN: f64 = 3.981_071_705_5; // +12 dB

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
    /// Denoise strength: 0.0 (off) to 1.0 (heavy)
    pub denoise: f64,
    /// Sharpness: -1.0 (soften) to 1.0 (sharpen)
    pub sharpness: f64,
    /// Volume multiplier: 0.0 (silent) to 2.0 (double), default 1.0
    pub volume: f64,
    /// Audio pan: -1.0 (full left) to 1.0 (full right), default 0.0
    pub pan: f64,
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
    /// Reverse playback when true.
    pub reverse: bool,
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
    /// Opacity multiplier for compositing: 0.0 = transparent, 1.0 = opaque.
    pub opacity: f64,
    /// Horizontal position offset: −1.0 (left) to 1.0 (right). Default 0.0.
    pub position_x: f64,
    /// Vertical position offset: −1.0 (top) to 1.0 (bottom). Default 0.0.
    pub position_y: f64,
    /// Shadow grading: −1.0 (crush) to 1.0 (lift). Default 0.0.
    pub shadows: f64,
    /// Midtone grading: −1.0 (darken) to 1.0 (brighten). Default 0.0.
    pub midtones: f64,
    /// Highlight grading: −1.0 (pull down) to 1.0 (boost). Default 0.0.
    pub highlights: f64,
    /// Whether the source file contains an audio stream.
    pub has_audio: bool,
}

impl ProgramClip {
    pub fn source_duration_ns(&self) -> u64 {
        self.source_out_ns.saturating_sub(self.source_in_ns)
    }
    /// Timeline duration accounting for speed.
    pub fn duration_ns(&self) -> u64 {
        let src = self.source_duration_ns();
        if self.speed > 0.0 {
            (src as f64 / self.speed) as u64
        } else {
            src
        }
    }
    pub fn timeline_end_ns(&self) -> u64 {
        self.timeline_start_ns + self.duration_ns()
    }
    /// Convert a timeline position offset to the corresponding source file position.
    pub fn source_pos_ns(&self, timeline_pos_ns: u64) -> u64 {
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
    /// Optional per-slot `level` element for per-track metering.
    audio_level: Option<gst::Element>,
    /// Per-slot video effect elements.
    videobalance: Option<gst::Element>,
    gaussianblur: Option<gst::Element>,
    videocrop: Option<gst::Element>,
    videobox_crop_alpha: Option<gst::Element>,
    videoflip_rotate: Option<gst::Element>,
    videoflip_flip: Option<gst::Element>,
    textoverlay: Option<gst::Element>,
    alpha_filter: Option<gst::Element>,
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
}

/// Result of `compute_reuse_plan()`: whether all current decoder slots can be
/// reused for the next set of desired clips (continuing decoder fast path).
struct SlotReusePlan {
    all_reusable: bool,
    /// For each slot index, the desired clip index it should switch to.
    mappings: Vec<(usize, usize)>,
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
    audio_clips: Vec<ProgramClip>,
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
    /// Use audio-only decoder slots for fully-occluded clips (experimental).
    experimental_preview_optimizations: bool,
    /// Pre-build upcoming decoder slots for near-instant clip transitions.
    realtime_preview: bool,
    proxy_paths: HashMap<String, String>,
    /// Cache for per-path audio-stream probe results.
    audio_stream_probe_cache: HashMap<String, bool>,
    /// GStreamer `level` element on audiomixer output for metering.
    level_element: Option<gst::Element>,
    /// GStreamer `level` element on the audio-only pipeline for metering.
    level_element_audio: Option<gst::Element>,
    /// GStreamer `volume` element on the audio-only pipeline for volume control.
    /// Using a dedicated element avoids playbin's StreamVolume which can interact
    /// with PulseAudio/PipeWire flat-volume behaviour and inadvertently change
    /// the main pipeline's audio level.
    audio_volume_element: Option<gst::Element>,
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
    /// Capsfilter on compositor output for project resolution.
    comp_capsfilter: gst::Element,
    /// Capsfilter on the black background source (must match compositor output).
    black_capsfilter: gst::Element,
    /// Capsfilter on final monitor output after preview-quality downscaling.
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
    background_compositor_pad: gst::Pad,
    /// Current preview quality divisor.
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
    /// Frame duration in nanoseconds, derived from project frame rate.
    /// Used to quantize seek positions and deduplicate same-frame seeks.
    frame_duration_ns: u64,
    /// Last frame-quantized seek position (nanoseconds). When a new seek
    /// lands on the same frame, the pipeline work is skipped entirely.
    last_seeked_frame_pos: Option<u64>,
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

        // Link compositor → project caps → convert → scale → output caps → display/scope
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
        match (&audio_volume_element, &level_element_audio) {
            (Some(vol), Some(lv)) => {
                let bin = gst::Bin::builder().name("audio-filter-bin").build();
                bin.add_many([vol, lv]).unwrap();
                vol.link(lv).unwrap();
                let sink_pad = vol.static_pad("sink").unwrap();
                let src_pad = lv.static_pad("src").unwrap();
                bin.add_pad(&gst::GhostPad::with_target(&sink_pad).unwrap())
                    .unwrap();
                bin.add_pad(&gst::GhostPad::with_target(&src_pad).unwrap())
                    .unwrap();
                audio_pipeline.set_property("audio-filter", &bin.upcast::<gst::Element>());
            }
            (Some(vol), None) => {
                audio_pipeline.set_property("audio-filter", vol);
            }
            (None, Some(lv)) => {
                audio_pipeline.set_property("audio-filter", lv);
            }
            _ => {}
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
                experimental_preview_optimizations: false,
                realtime_preview: false,
                proxy_paths: HashMap::new(),
                audio_stream_probe_cache: HashMap::new(),
                level_element,
                level_element_audio,
                audio_volume_element,
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
                // Default 24 fps ≈ 41_666_666 ns per frame
                frame_duration_ns: 1_000_000_000 / 24,
                last_seeked_frame_pos: None,
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
        self.proxy_enabled = enabled;
        self.prewarmed_boundary_ns = None;
    }

    pub fn set_experimental_preview_optimizations(&mut self, enabled: bool) {
        self.experimental_preview_optimizations = enabled;
    }

    pub fn set_realtime_preview(&mut self, enabled: bool) {
        self.realtime_preview = enabled;
    }

    /// Enable or disable scope frame capture. When disabled (scopes panel hidden),
    /// the appsink callback drops frames without allocating, saving ~7MB/s at 30fps.
    pub fn set_scope_enabled(&self, enabled: bool) {
        self.scope_enabled.store(enabled, Ordering::Relaxed);
    }

    pub fn update_proxy_paths(&mut self, paths: HashMap<String, String>) {
        if self.proxy_paths != paths {
            self.proxy_paths = paths;
            self.prewarmed_boundary_ns = None;
        }
    }

    pub fn set_project_dimensions(&mut self, width: u32, height: u32) {
        self.project_width = width;
        self.project_height = height;
        self.apply_compositor_caps();
    }

    /// Update the frame duration from the project frame rate.
    /// Called alongside `set_project_dimensions` when the project changes.
    pub fn set_frame_rate(&mut self, numerator: u32, denominator: u32) {
        if numerator > 0 && denominator > 0 {
            // frame_duration = 1e9 * denominator / numerator (nanoseconds)
            self.frame_duration_ns = (1_000_000_000u64 * denominator as u64) / numerator as u64;
        }
    }

    /// Set preview quality (compositor resolution divisor). Takes effect immediately.
    pub fn set_preview_quality(&mut self, divisor: u32) {
        let new_divisor = divisor.max(1);
        if self.preview_divisor == new_divisor {
            return;
        }
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
        let comp_w = self.project_width.max(2) as i32;
        let comp_h = self.project_height.max(2) as i32;
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

    /// Returns (opacity_a, opacity_b) for the two program monitor pictures.
    /// With the compositor approach all layering is internal; picture_b is unused.
    pub fn transition_opacities(&self) -> (f64, f64) {
        (1.0, 0.0)
    }

    pub fn try_pull_scope_frame(&self) -> Option<ScopeFrame> {
        self.latest_scope_frame.lock().ok()?.clone()
    }

    /// Export the currently displayed scope-capture frame as a binary PPM image (P6).
    pub fn export_displayed_frame_ppm(&self, path: &str) -> Result<()> {
        let start_seq = self.scope_frame_seq.load(Ordering::Relaxed);
        let was_enabled = self.scope_enabled.swap(true, Ordering::Relaxed);
        log::info!(
            "export_displayed_frame: start_seq={} slots={} current_idx={:?} state={:?} timeline_ns={}",
            start_seq, self.slots.len(), self.current_idx, self.state, self.timeline_pos_ns
        );
        let result = (|| -> Result<()> {
            if self.current_idx.is_some() && self.state != PlayerState::Playing {
                self.reseek_all_slots_for_export();
            }
            let after_reseek_seq = self.scope_frame_seq.load(Ordering::Relaxed);
            let deadline = Instant::now() + Duration::from_millis(800);
            while self.scope_frame_seq.load(Ordering::Relaxed) <= start_seq
                && Instant::now() < deadline
            {
                std::thread::sleep(Duration::from_millis(10));
            }
            let end_seq = self.scope_frame_seq.load(Ordering::Relaxed);
            log::info!(
                "export_displayed_frame: after_reseek_seq={} end_seq={} seq_changed={}",
                after_reseek_seq,
                end_seq,
                end_seq > start_seq
            );
            self.write_scope_frame_ppm(path)
        })();
        if !was_enabled {
            self.scope_enabled.store(false, Ordering::Relaxed);
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
    pub fn scope_frame_seq_arc(&self) -> Arc<AtomicU64> {
        self.scope_frame_seq.clone()
    }

    /// Monotonic counter for compositor-src frame captures.
    pub fn compositor_frame_seq_arc(&self) -> Arc<AtomicU64> {
        self.compositor_frame_seq.clone()
    }

    /// Phase 2 of async export: write the latest scope frame to disk as PPM
    /// and optionally restore scope_enabled.
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
        self.teardown_prepreroll_sidecars();
    }

    // ── Transport controls ─────────────────────────────────────────────────

    /// Seek to `timeline_pos_ns`.  Returns `true` if a non-blocking Playing
    /// pulse was started for 3+ tracks — the caller **must** schedule
    /// `complete_playing_pulse()` via a GTK idle/timeout callback so the main
    /// loop can run and `gtk4paintablesink` can complete its preroll.
    pub fn seek(&mut self, timeline_pos_ns: u64) -> bool {
        let seek_started = Instant::now();
        let resume_playback = self.state == PlayerState::Playing;

        // Frame-boundary deduplication: quantize to the nearest frame and
        // skip redundant pipeline work when the playhead hasn't moved to a
        // new frame.  This eliminates unnecessary decoder seeks during slow
        // timeline scrubbing where multiple pixel-level drag events land on
        // the same video frame.
        let frame_pos = if self.frame_duration_ns > 0 {
            (timeline_pos_ns / self.frame_duration_ns) * self.frame_duration_ns
        } else {
            timeline_pos_ns
        };
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
            let needs_async = self.seek_slots_in_place(timeline_pos_ns);
            self.sync_audio_to(timeline_pos_ns);
            log::info!(
                "seek: done timeline_pos={} fast_path={} needs_async={} slots={} elapsed_ms={}",
                timeline_pos_ns,
                fast_path,
                needs_async,
                self.slots.len(),
                seek_started.elapsed().as_millis()
            );
            self.last_seeked_frame_pos = Some(frame_pos);
            return needs_async;
        }
        // Full rebuild: needed when the set of active clips has changed (e.g.
        // crossing a clip boundary), on cold start, or when resuming from playing.
        self.rebuild_pipeline_at(timeline_pos_ns);
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
        log::info!(
            "seek: done timeline_pos={} fast_path={} needs_async={} slots={} elapsed_ms={}",
            timeline_pos_ns,
            fast_path,
            needs_async,
            self.slots.len(),
            seek_started.elapsed().as_millis()
        );
        self.last_seeked_frame_pos = Some(frame_pos);
        needs_async
    }

    pub fn play(&mut self) {
        if self.clips.is_empty() && self.audio_clips.is_empty() {
            return;
        }
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
            );
        }
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

    pub fn poll(&mut self) -> bool {
        let _eos = self.poll_bus();

        if self.state == PlayerState::Playing {
            self.audio_peak_db[0] = (self.audio_peak_db[0] - 3.0).max(-60.0);
            self.audio_peak_db[1] = (self.audio_peak_db[1] - 3.0).max(-60.0);
            for peaks in &mut self.audio_track_peak_db {
                peaks[0] = (peaks[0] - 3.0).max(-60.0);
                peaks[1] = (peaks[1] - 3.0).max(-60.0);
            }
        }

        if self.state != PlayerState::Playing {
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
        let current: Vec<usize> = self
            .slots
            .iter()
            .filter(|s| !s.hidden)
            .map(|s| s.clip_idx)
            .collect();
        if desired != current {
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
            let audio_wanted = if let Some(idx) = self.reverse_video_clip_at_for_audio(new_pos) {
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
            let used_realtime = self.realtime_preview
                && self.try_realtime_boundary_update(new_pos, &desired, &current);

            if !used_realtime {
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

        // Update current_idx to highest-priority active clip.
        self.current_idx = self.clip_at(new_pos);

        // Advance audio pipeline across clip boundaries.
        self.poll_audio(new_pos);
        self.update_drop_late_policy();
        self.update_slot_queue_policy();

        changed
    }

    // ── Effects / transform updates ────────────────────────────────────────

    pub fn update_current_color(&mut self, brightness: f64, contrast: f64, saturation: f64) {
        self.update_current_effects(brightness, contrast, saturation, 0.0, 0.0, 0.0, 0.0, 0.0);
    }

    pub fn update_current_effects(
        &mut self,
        brightness: f64,
        contrast: f64,
        saturation: f64,
        denoise: f64,
        sharpness: f64,
        shadows: f64,
        midtones: f64,
        highlights: f64,
    ) {
        if let Some(slot) = self.current_idx.and_then(|idx| self.slot_for_clip(idx)) {
            if let Some(ref vb) = slot.videobalance {
                // Approximate shadows/midtones/highlights via videobalance
                // brightness and contrast offsets.  Not pixel-perfect but
                // gives real-time visual feedback.  Export uses ffmpeg
                // colorbalance for accurate per-luminance grading.
                let eff_brightness =
                    (brightness + shadows * 0.3 + midtones * 0.2 + highlights * 0.15)
                        .clamp(-1.0, 1.0);
                let eff_contrast = (contrast - shadows * 0.15 + highlights * 0.15).clamp(0.0, 2.0);
                vb.set_property("brightness", eff_brightness);
                vb.set_property("contrast", eff_contrast);
                vb.set_property("saturation", saturation.clamp(0.0, 2.0));
            }
            if let Some(ref gb) = slot.gaussianblur {
                let sigma = (denoise * 4.0 - sharpness * 6.0).clamp(-20.0, 20.0);
                gb.set_property("sigma", sigma);
            }
        }
        // Force frame redraw when paused.
        if self.current_idx.is_some() && self.state != PlayerState::Playing {
            self.reseek_slot_for_current();
        }
    }

    pub fn update_current_audio(&mut self, volume: f64, _pan: f64) {
        // Per-clip volume on the audiomixer pad.
        if let Some(slot) = self.current_idx.and_then(|idx| self.slot_for_clip(idx)) {
            if let Some(ref pad) = slot.audio_mixer_pad {
                let clipped = volume.clamp(0.0, MAX_PREVIEW_AUDIO_GAIN);
                let pad_volume = if self.reverse_video_ducked_clip_idx == self.current_idx {
                    0.0
                } else {
                    clipped
                };
                pad.set_property("volume", pad_volume);
            }
        }
    }

    pub fn update_audio_for_clip(&mut self, clip_id: &str, volume: f64, _pan: f64) {
        let volume = volume.clamp(0.0, MAX_PREVIEW_AUDIO_GAIN);
        // Check video clips first (use audiomixer pad on compositor pipeline).
        if let Some(i) = self.clips.iter().position(|c| c.id == clip_id) {
            self.clips[i].volume = volume;
            if let Some(slot) = self.slot_for_clip(i) {
                if let Some(ref pad) = slot.audio_mixer_pad {
                    let pad_volume = if self.reverse_video_ducked_clip_idx == Some(i) {
                        0.0
                    } else {
                        volume
                    };
                    pad.set_property("volume", pad_volume);
                }
            }
            return;
        }
        // For audio-only clips, update the stored volume and, if actively playing,
        // update the dedicated volume element (avoids playbin StreamVolume crosstalk).
        if let Some(i) = self.audio_clips.iter().position(|c| c.id == clip_id) {
            self.audio_clips[i].volume = volume;
            if self.audio_current_source == Some(AudioCurrentSource::AudioClip(i)) {
                if let Some(ref vol_elem) = self.audio_volume_element {
                    vol_elem.set_property("volume", volume);
                } else {
                    self.audio_pipeline.set_property("volume", volume);
                }
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
                Self::apply_zoom_to_slot(
                    slot,
                    pad,
                    scale,
                    position_x,
                    position_y,
                    self.project_width,
                    self.project_height,
                );
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
                    Self::apply_zoom_to_slot(
                        slot,
                        pad,
                        scale,
                        position_x,
                        position_y,
                        self.project_width,
                        self.project_height,
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
                    Self::apply_zoom_to_slot(
                        slot,
                        pad,
                        scale,
                        position_x,
                        position_y,
                        self.project_width,
                        self.project_height,
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

    fn effective_source_path_for_clip(&self, clip: &ProgramClip) -> String {
        if self.proxy_enabled {
            let key =
                crate::media::proxy_cache::proxy_key(&clip.source_path, clip.lut_path.as_deref());
            self.proxy_paths
                .get(&key)
                .cloned()
                .unwrap_or_else(|| clip.source_path.clone())
        } else {
            clip.source_path.clone()
        }
    }

    fn next_video_boundary_after(&self, timeline_pos_ns: u64) -> Option<u64> {
        self.clips
            .iter()
            .flat_map(|c| [c.timeline_start_ns, c.timeline_end_ns()])
            .filter(|&t| t > timeline_pos_ns)
            .min()
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
        let (effects_bin, ..) =
            Self::build_effects_bin(clip, self.project_width, self.project_height);
        let _ = effects_bin.set_state(gst::State::Null);
        self.prepreroll_sidecars.push(sidecar);
    }

    /// Tear down all pre-preroll sidecar pipelines.
    fn teardown_prepreroll_sidecars(&mut self) {
        for sidecar in self.prepreroll_sidecars.drain(..) {
            let _ = sidecar.set_state(gst::State::Null);
        }
    }

    fn prewarm_upcoming_boundary(&mut self, timeline_pos_ns: u64) {
        const PREWARM_WINDOW_NS: u64 = 1_500_000_000;
        let Some(boundary_ns) = self.next_video_boundary_after(timeline_pos_ns) else {
            self.prewarmed_boundary_ns = None;
            return;
        };
        if boundary_ns.saturating_sub(timeline_pos_ns) > PREWARM_WINDOW_NS {
            return;
        }
        if self.prewarmed_boundary_ns == Some(boundary_ns) {
            return;
        }
        let current_active = self.clips_active_at(timeline_pos_ns);
        let boundary_active = self.clips_active_at(boundary_ns);

        // Skip prewarming when continuing decoders will handle this boundary.
        if boundary_active.len() == self.slots.len() && !self.slots.is_empty() {
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
            let incoming = !current_active.contains(&idx);
            if clip.has_audio {
                let effective_path = self.effective_source_path_for_clip(&clip);
                let _ = self.probe_has_audio_stream_cached(&effective_path);
            }
            if incoming {
                self.prewarm_incoming_clip_resources(&clip, boundary_ns);
            }
        }
        log::debug!(
            "prewarm_upcoming_boundary: timeline_pos={} boundary={}",
            timeline_pos_ns,
            boundary_ns
        );
        self.prewarmed_boundary_ns = Some(boundary_ns);
    }

    /// Find the VideoSlot corresponding to `clip_idx`, if any.
    fn slot_for_clip(&self, clip_idx: usize) -> Option<&VideoSlot> {
        self.slots.iter().find(|s| s.clip_idx == clip_idx)
    }

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
        self.state != PlayerState::Playing && self.slots.len() >= 3
    }

    fn effective_wait_timeout_ms(&self, requested_ms: u64) -> u64 {
        if self.state == PlayerState::Playing {
            // During active playback, keep waits short to minimise boundary stall.
            // Scale down with slot count and telemetry.
            let cap = if self.slots.len() >= 3 { 180 } else { 300 };
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
                let pending: Vec<usize> = self
                    .slots
                    .iter()
                    .zip(baseline.iter())
                    .enumerate()
                    .filter(|(_, (slot, &base))| {
                        slot.compositor_pad.is_some()
                            && slot.comp_arrival_seq.load(Ordering::Relaxed) <= base
                    })
                    .map(|(i, _)| i)
                    .collect();
                log::warn!(
                    "wait_for_compositor_arrivals: timeout {}ms (requested {}ms), pending slots={:?}",
                    effective_timeout_ms,
                    timeout_ms,
                    pending
                );
                return false;
            }
            std::thread::sleep(Duration::from_millis(20));
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
    ) -> bool {
        let source_ns = clip.source_pos_ns(timeline_pos_ns);
        let effective_seek_flags = if clip.reverse {
            (seek_flags | gst::SeekFlags::ACCURATE) & !gst::SeekFlags::KEY_UNIT
        } else {
            seek_flags
        };
        slot.decoder
            .seek(
                clip.seek_rate(),
                effective_seek_flags,
                gst::SeekType::Set,
                gst::ClockTime::from_nseconds(clip.seek_start_ns(source_ns)),
                gst::SeekType::Set,
                gst::ClockTime::from_nseconds(clip.seek_stop_ns(source_ns)),
            )
            .is_ok()
    }

    fn seek_slot_decoder_paused(
        slot: &VideoSlot,
        clip: &ProgramClip,
        timeline_pos_ns: u64,
    ) -> bool {
        let source_ns = clip.source_pos_ns(timeline_pos_ns);
        log::info!(
            "seek_slot_decoder_paused: clip={} timeline_ns={} source_ns={}",
            clip.id,
            timeline_pos_ns,
            source_ns
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
    ) -> bool {
        for _ in 0..4 {
            if Self::seek_slot_decoder(slot, clip, timeline_pos_ns, seek_flags) {
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
    /// Returns `true` if a non-blocking Playing pulse was started (3+ tracks).
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
        self.pipeline.set_start_time(gst::ClockTime::ZERO);
        // Always use the async playing pulse path for exports.
        // gtk4paintablesink requires the GTK main loop to complete its
        // state transitions, so we MUST return to the caller (and let the
        // main loop run) before the pipeline can actually reach Playing
        // and push fresh frames through the compositor.
        self.start_playing_pulse();
        true
    }

    /// Restore pipeline to Paused after an async export that left it Playing.
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
    fn seek_slots_in_place(&mut self, timeline_pos_ns: u64) -> bool {
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
        self.wait_for_compositor_arrivals(&baseline, 3000);
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
        if let Some(ref vc) = slot.videocrop {
            vc.set_property("left", crop_left.max(0));
            vc.set_property("right", crop_right.max(0));
            vc.set_property("top", crop_top.max(0));
            vc.set_property("bottom", crop_bottom.max(0));
        }
        // Re-pad cropped edges with transparent borders so the compositor
        // reveals lower tracks through the cropped area.
        if let Some(ref vb) = slot.videobox_crop_alpha {
            vb.set_property("left", -(crop_left.max(0)));
            vb.set_property("right", -(crop_right.max(0)));
            vb.set_property("top", -(crop_top.max(0)));
            vb.set_property("bottom", -(crop_bottom.max(0)));
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

        if let Some(ref cf) = slot.capsfilter_zoom {
            let sw = (pw * scale).round() as i32;
            let sh = (ph * scale).round() as i32;
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
                Self::seek_slot_decoder_with_retry(slot, clip, timeline_pos, seek_flags)
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
            if let Some(slot) = self.build_slot_for_clip(*clip_idx, zorder_offset, true) {
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
                Self::seek_slot_decoder_with_retry(slot, clip, timeline_pos, seek_flags)
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
                if !Self::seek_slot_decoder_with_retry(slot, clip, timeline_pos, seek_flags) {
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
    }

    /// Wait briefly for dynamic decode pads to link into the effects chain.
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
        project_width: u32,
        project_height: u32,
    ) -> (
        gst::Bin,
        Option<gst::Element>, // videobalance
        Option<gst::Element>, // gaussianblur
        Option<gst::Element>, // videocrop
        Option<gst::Element>, // videobox_crop_alpha
        Option<gst::Element>, // videoflip_rotate
        Option<gst::Element>, // videoflip_flip
        Option<gst::Element>, // textoverlay
        Option<gst::Element>, // alpha_filter
        Option<gst::Element>, // capsfilter_zoom
        Option<gst::Element>,
    ) // videobox_zoom
    {
        let bin = gst::Bin::new();

        // Determine which effects are active (non-default) so we can skip
        // no-op elements and their associated videoconvert instances.  This
        // dramatically reduces per-frame CPU cost for clips without effects
        // (3 concurrent clips drops from ~51 to ~22 pipeline elements).
        let need_balance = clip.brightness != 0.0 || clip.contrast != 1.0 || clip.saturation != 1.0;
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
        let alpha_filter: Option<gst::Element> = None;

        // Textoverlay: only if title text is set.
        let textoverlay = if need_title {
            gst::ElementFactory::make("textoverlay").build().ok()
        } else {
            None
        };

        // Scaling chain (always needed): videoconvertscale → capsfilter
        // → videoscale → capsfilter → videobox.
        // videoconvertscale does color-convert AND scale in a single pass,
        // avoiding an intermediate full-resolution RGBA allocation. Benchmarked
        // at ~2.6× faster than separate videoconvert + videoscale for 5.3K H.265.
        let convertscale = gst::ElementFactory::make("videoconvertscale")
            .property("add-borders", true)
            .build()
            .ok();
        let capsfilter_proj = gst::ElementFactory::make("capsfilter").build().ok();
        let videoscale_zoom = gst::ElementFactory::make("videoscale").build().ok();
        let capsfilter_zoom = gst::ElementFactory::make("capsfilter").build().ok();
        let videobox_zoom = gst::ElementFactory::make("videobox").build().ok();

        // Set initial values from clip data.
        if let Some(ref vb) = videobalance {
            // Apply shadows/midtones/highlights approximation via videobalance.
            let eff_brightness = (clip.brightness
                + clip.shadows * 0.3
                + clip.midtones * 0.2
                + clip.highlights * 0.15)
                .clamp(-1.0, 1.0);
            let eff_contrast =
                (clip.contrast - clip.shadows * 0.15 + clip.highlights * 0.15).clamp(0.0, 2.0);
            vb.set_property("brightness", eff_brightness);
            vb.set_property("contrast", eff_contrast);
            vb.set_property("saturation", clip.saturation.clamp(0.0, 2.0));
        }
        if let Some(ref gb) = gaussianblur {
            gb.set_property("sigma", blur_sigma);
        }
        if let Some(ref a) = alpha_filter {
            a.set_property("alpha", 1.0_f64);
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

        // Set project resolution capsfilters.
        // capsfilter_proj constrains to RGBA at project resolution (for effects).
        let proj_caps = gst::Caps::builder("video/x-raw")
            .field("format", "RGBA")
            .field("width", project_width as i32)
            .field("height", project_height as i32)
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

        // Build chain: downscale to project resolution EARLY so all effects
        // process at 1080p instead of source resolution (e.g. 5.3K for GoPro).
        // Order: convertscale to project res → [crop + alpha repad] → [effects]
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
        // 3. Effects at project resolution (much cheaper than source res).
        if let Some(ref e) = conv1 {
            chain.push(e.clone());
        }
        if let Some(ref e) = videobalance {
            chain.push(e.clone());
        }
        if let Some(ref e) = conv2 {
            chain.push(e.clone());
        }
        if let Some(ref e) = gaussianblur {
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
            bin.add_pad(&gst::GhostPad::with_target(&sink_pad).unwrap())
                .ok();
            bin.add_pad(&gst::GhostPad::with_target(&src_pad).unwrap())
                .ok();
        } else {
            let refs: Vec<&gst::Element> = chain.iter().collect();
            bin.add_many(&refs).ok();
            gst::Element::link_many(&refs).ok();
            let sink_pad = chain.first().unwrap().static_pad("sink").unwrap();
            let src_pad = chain.last().unwrap().static_pad("src").unwrap();
            bin.add_pad(&gst::GhostPad::with_target(&sink_pad).unwrap())
                .ok();
            bin.add_pad(&gst::GhostPad::with_target(&src_pad).unwrap())
                .ok();
        }

        (
            bin,
            videobalance,
            gaussianblur,
            videocrop,
            videobox_crop_alpha,
            videoflip_rotate,
            videoflip_flip,
            textoverlay,
            alpha_filter,
            capsfilter_zoom,
            videobox_zoom,
        )
    }

    /// Quick check whether a media file contains an audio stream.
    /// Uses GStreamer Discoverer with a short timeout. Defaults to `true` on error.
    fn probe_has_audio_stream(path: &str) -> bool {
        use gstreamer_pbutils::prelude::*;
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
        let effective_path = self.effective_source_path_for_clip(&clip);
        let uri = format!("file://{}", effective_path);

        let clip_has_audio = if !clip.has_audio {
            false
        } else {
            self.probe_has_audio_stream_cached(&effective_path)
        };
        if !clip_has_audio {
            log::info!(
                "build_audio_only_slot: clip {} has no audio, skipping entirely",
                clip.id
            );
            return None;
        }

        log::info!(
            "build_audio_only_slot: clip {} uri={} (video occluded)",
            clip.id,
            uri
        );

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

        // Audio path: audioconvert → [level] → audiomixer pad.
        let (audio_conv, audio_level, amix_pad) = {
            let ac = gst::ElementFactory::make("audioconvert").build().ok();
            let mut lv = gst::ElementFactory::make("level")
                .property("post-messages", true)
                .property("interval", 50_000_000u64)
                .build()
                .ok();
            let pad = if let Some(ref ac) = ac {
                if self.pipeline.add(ac).is_ok() {
                    let mut link_src = ac.static_pad("src");
                    if let Some(ref level) = lv {
                        if self.pipeline.add(level).is_ok() {
                            if let (Some(ac_src), Some(level_sink)) =
                                (ac.static_pad("src"), level.static_pad("sink"))
                            {
                                let _ = ac_src.link(&level_sink);
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
                        mp.set_property("volume", clip.volume.clamp(0.0, MAX_PREVIEW_AUDIO_GAIN));
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
                    lv = None;
                    None
                }
            } else {
                lv = None;
                None
            };
            (ac, lv, pad)
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
            audio_level,
            videobalance: None,
            gaussianblur: None,
            videocrop: None,
            videobox_crop_alpha: None,
            videoflip_rotate: None,
            videoflip_flip: None,
            textoverlay: None,
            alpha_filter: None,
            capsfilter_zoom: None,
            videobox_zoom: None,
            slot_queue: None,
            comp_arrival_seq: Arc::new(AtomicU64::new(0)),
            hidden: false,
        })
    }

    fn build_slot_for_clip(
        &mut self,
        clip_idx: usize,
        zorder_offset: usize,
        tune_multiqueue: bool,
    ) -> Option<VideoSlot> {
        let clip = self.clips[clip_idx].clone();

        // Resolve proxy path.
        let effective_path = self.effective_source_path_for_clip(&clip);
        let uri = format!("file://{}", effective_path);
        log::info!("ProgramPlayer: slot {} uri={}", zorder_offset, uri);

        // Detect whether the source file has an audio stream.
        let clip_has_audio = if !clip.has_audio {
            false
        } else {
            self.probe_has_audio_stream_cached(&effective_path)
        };

        // Build per-slot effects bin.
        let (
            effects_bin,
            videobalance,
            gaussianblur,
            videocrop,
            videobox_crop_alpha,
            videoflip_rotate,
            videoflip_flip,
            textoverlay,
            alpha_filter,
            capsfilter_zoom,
            videobox_zoom,
        ) = Self::build_effects_bin(&clip, self.project_width, self.project_height);

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
        comp_pad.set_property("alpha", clip.opacity.clamp(0.0, 1.0));

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

        // Create audio path: audioconvert → [level] → audiomixer pad.
        let (audio_conv, audio_level, amix_pad) = if clip_has_audio {
            let ac = gst::ElementFactory::make("audioconvert").build().ok();
            let mut lv = gst::ElementFactory::make("level")
                .property("post-messages", true)
                .property("interval", 50_000_000u64)
                .build()
                .ok();
            let pad = if let Some(ref ac) = ac {
                if self.pipeline.add(ac).is_ok() {
                    let mut link_src = ac.static_pad("src");
                    if let Some(ref level) = lv {
                        if self.pipeline.add(level).is_ok() {
                            if let (Some(ac_src), Some(level_sink)) =
                                (ac.static_pad("src"), level.static_pad("sink"))
                            {
                                let _ = ac_src.link(&level_sink);
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
                        mp.set_property("volume", clip.volume.clamp(0.0, MAX_PREVIEW_AUDIO_GAIN));
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
                    lv = None;
                    None
                }
            } else {
                lv = None;
                None
            };
            (ac, lv, pad)
        } else {
            log::info!(
                "ProgramPlayer: skipping audio path for clip {} (no audio)",
                clip.id
            );
            (None, None, None)
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
            None
        });

        // Sync all slot elements to the pipeline state.
        let _ = effects_bin.sync_state_with_parent();
        let _ = slot_queue.sync_state_with_parent();
        if let Some(ref ac) = audio_conv {
            let _ = ac.sync_state_with_parent();
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
            audio_level: audio_level.clone(),
            videobalance: videobalance.clone(),
            gaussianblur: gaussianblur.clone(),
            videocrop: videocrop.clone(),
            videobox_crop_alpha: videobox_crop_alpha.clone(),
            videoflip_rotate: videoflip_rotate.clone(),
            videoflip_flip: videoflip_flip.clone(),
            textoverlay: textoverlay.clone(),
            alpha_filter: alpha_filter.clone(),
            capsfilter_zoom: capsfilter_zoom.clone(),
            videobox_zoom: videobox_zoom.clone(),
            slot_queue: Some(slot_queue.clone()),
            comp_arrival_seq: comp_arrival_seq.clone(),
            hidden: false,
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
            clip.scale,
            clip.position_x,
            clip.position_y,
            self.project_width,
            self.project_height,
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
            audio_level,
            videobalance,
            gaussianblur,
            videocrop,
            videobox_crop_alpha,
            videoflip_rotate,
            videoflip_flip,
            textoverlay,
            alpha_filter,
            capsfilter_zoom,
            videobox_zoom,
            slot_queue: Some(slot_queue),
            comp_arrival_seq,
            hidden: false,
        })
    }

    // ── Continuing decoders (fast-path boundary crossing) ────────────────

    /// Returns true if two clips would produce effects bins with the same
    /// element topology (same set of active effect elements).  When true,
    /// a reused slot's effects bin can be updated via property sets alone,
    /// without adding/removing GStreamer elements.
    fn effects_topology_matches(a: &ProgramClip, b: &ProgramClip) -> bool {
        let need_balance =
            |c: &ProgramClip| c.brightness != 0.0 || c.contrast != 1.0 || c.saturation != 1.0;
        let need_blur = |c: &ProgramClip| {
            let sigma = (c.denoise * 4.0 - c.sharpness * 6.0).clamp(-20.0, 20.0);
            sigma.abs() > f64::EPSILON
        };
        let need_rotate = |c: &ProgramClip| c.rotate.rem_euclid(360) != 0;
        let need_flip = |c: &ProgramClip| c.flip_h || c.flip_v;
        let need_title = |c: &ProgramClip| !c.title_text.is_empty();

        need_balance(a) == need_balance(b)
            && need_blur(a) == need_blur(b)
            && need_rotate(a) == need_rotate(b)
            && need_flip(a) == need_flip(b)
            && need_title(a) == need_title(b)
    }

    /// Determine whether all current slots can be reused for the desired
    /// clip set.  Both `desired` and current slots are ordered by z-order
    /// (track_index), so we compare positionally.
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

        for (slot_idx, &desired_clip_idx) in desired.iter().enumerate() {
            let current_clip_idx = self.slots[slot_idx].clip_idx;

            // 1. Same source file (accounts for proxy mode)
            let desired_path = self.effective_source_path_for_clip(&self.clips[desired_clip_idx]);
            let current_path = self.effective_source_path_for_clip(&self.clips[current_clip_idx]);
            if desired_path != current_path {
                return fail;
            }

            // 2. Same audio presence
            let desired_has_audio_flag = self.clips[desired_clip_idx].has_audio;
            let desired_has_audio =
                desired_has_audio_flag && self.probe_has_audio_stream_cached(&desired_path);
            let current_has_audio = self.slots[slot_idx].audio_mixer_pad.is_some();
            if desired_has_audio != current_has_audio {
                return fail;
            }

            // 3. Same speed and reverse
            let desired_speed = self.clips[desired_clip_idx].speed;
            let current_speed = self.clips[current_clip_idx].speed;
            let desired_reverse = self.clips[desired_clip_idx].reverse;
            let current_reverse = self.clips[current_clip_idx].reverse;
            if (desired_speed - current_speed).abs() > f64::EPSILON
                || desired_reverse != current_reverse
            {
                return fail;
            }

            // 4. Same effects topology
            if !Self::effects_topology_matches(
                &self.clips[current_clip_idx],
                &self.clips[desired_clip_idx],
            ) {
                return fail;
            }

            mappings.push((slot_idx, desired_clip_idx));
        }

        SlotReusePlan {
            all_reusable: true,
            mappings,
        }
    }

    /// Update all mutable effect properties on a reused slot without
    /// rebuilding the effects bin.
    fn update_slot_effects(&self, slot_idx: usize, clip: &ProgramClip) {
        let slot = &self.slots[slot_idx];

        // Videobalance (brightness/contrast/saturation + shadows/midtones/highlights)
        if let Some(ref vb) = slot.videobalance {
            let eff_brightness = (clip.brightness
                + clip.shadows * 0.3
                + clip.midtones * 0.2
                + clip.highlights * 0.15)
                .clamp(-1.0, 1.0);
            let eff_contrast =
                (clip.contrast - clip.shadows * 0.15 + clip.highlights * 0.15).clamp(0.0, 2.0);
            vb.set_property("brightness", eff_brightness);
            vb.set_property("contrast", eff_contrast);
            vb.set_property("saturation", clip.saturation.clamp(0.0, 2.0));
        }

        // Gaussianblur
        if let Some(ref gb) = slot.gaussianblur {
            let sigma = (clip.denoise * 4.0 - clip.sharpness * 6.0).clamp(-20.0, 20.0);
            gb.set_property("sigma", sigma);
        }

        // Crop, rotate, flip
        Self::apply_transform_to_slot(
            slot,
            clip.crop_left,
            clip.crop_right,
            clip.crop_top,
            clip.crop_bottom,
            clip.rotate,
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
            pad.set_property("alpha", clip.opacity.clamp(0.0, 1.0));
            Self::apply_zoom_to_slot(
                slot,
                pad,
                clip.scale,
                clip.position_x,
                clip.position_y,
                self.project_width,
                self.project_height,
            );
        }

        // Audiomixer pad: volume
        if let Some(ref pad) = slot.audio_mixer_pad {
            pad.set_property("volume", clip.volume.clamp(0.0, 10.0));
        }
    }

    /// Fast-path boundary crossing: reuse existing decoder slots when the
    /// incoming clips share the same source files.  Avoids tearing down
    /// and rebuilding uridecodebin, effects_bin, and compositor pads.
    fn continue_decoders_at(&mut self, timeline_pos: u64, desired: &[usize], plan: &SlotReusePlan) {
        let rebuild_started = Instant::now();
        let was_playing = self.state == PlayerState::Playing;

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
        }

        // 2. Update effects properties on each reused slot.
        for &(slot_idx, desired_clip_idx) in &plan.mappings {
            let clip = &self.clips[desired_clip_idx];
            self.update_slot_effects(slot_idx, clip);
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

        let elapsed = rebuild_started.elapsed().as_millis();
        self.record_rebuild_duration_ms(elapsed as u64);
        log::info!(
            "continue_decoders_at: END timeline_pos={}ns elapsed_ms={}",
            timeline_pos,
            elapsed
        );
    }

    /// Core method: tear down all slots and rebuild for clips active at `timeline_pos`.
    fn rebuild_pipeline_at(&mut self, timeline_pos: u64) {
        let rebuild_started = Instant::now();
        let was_playing = self.state == PlayerState::Playing;
        let had_existing_slots = !self.slots.is_empty();
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
            let plan = self.compute_reuse_plan(&desired);
            if plan.all_reusable {
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

        for (zorder_offset, &clip_idx) in active.iter().enumerate() {
            // When experimental preview optimizations are enabled, use lightweight
            // audio-only decoder slots for clips that are fully occluded by an
            // opaque full-frame clip above them.
            if self.experimental_preview_optimizations
                && self.is_clip_video_occluded(&active, zorder_offset)
            {
                if let Some(slot) = self.build_audio_only_slot_for_clip(clip_idx) {
                    self.slots.push(slot);
                } else if let Some(slot) =
                    self.build_slot_for_clip(clip_idx, zorder_offset, was_playing)
                {
                    log::warn!(
                        "rebuild_pipeline_at: audio-only occlusion slot failed for clip {}, falling back to full slot",
                        self.clips[clip_idx].id
                    );
                    self.slots.push(slot);
                }
            } else if let Some(slot) =
                self.build_slot_for_clip(clip_idx, zorder_offset, was_playing)
            {
                self.slots.push(slot);
            }
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
        let link_wait_ms = if was_playing {
            self.adaptive_arrival_wait_ms(400)
        } else {
            self.effective_wait_timeout_ms(400)
        };
        let _ = self
            .pipeline
            .state(gst::ClockTime::from_mseconds(link_wait_ms));
        let t_link_wait = rebuild_started.elapsed().as_millis();

        // If any decoder's video pad didn't link in time, send EOS on its
        // compositor pad so the aggregator doesn't wait indefinitely for
        // buffers that will never arrive.  Audio mixer pads are preserved
        // when audio has already linked — only EOS audio if audio is also
        // unlinked.
        for slot in &self.slots {
            if !slot.video_linked.load(Ordering::Relaxed) {
                log::warn!(
                    "rebuild_pipeline_at: slot clip_idx={} video not linked after {}ms, sending compositor EOS",
                    slot.clip_idx,
                    link_wait_ms
                );
                if let Some(ref pad) = slot.compositor_pad {
                    let _ = pad.send_event(gst::event::Eos::new());
                }
                // Only EOS the audio pad if audio is also not linked.
                if slot.audio_mixer_pad.is_some() {
                    let audio_ok = slot.audio_linked.load(Ordering::Relaxed);
                    if !audio_ok {
                        log::warn!(
                            "rebuild_pipeline_at: clip_idx={} audio also not linked, sending audio EOS",
                            slot.clip_idx
                        );
                        if let Some(ref pad) = slot.audio_mixer_pad {
                            let _ = pad.send_event(gst::event::Eos::new());
                        }
                    } else {
                        log::info!(
                            "rebuild_pipeline_at: clip_idx={} video unlinked but audio linked — preserving audio",
                            slot.clip_idx
                        );
                    }
                }
            }
        }

        self.wait_for_paused_preroll();
        let t_preroll = rebuild_started.elapsed().as_millis();
        log::debug!("rebuild_pipeline_at: paused preroll done");

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
                let ok = Self::seek_slot_decoder_with_retry(slot, clip, timeline_pos, seek_flags);
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
            self.wait_for_compositor_arrivals(&baseline, arrival_budget);
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
                    self.wait_for_compositor_arrivals(&baseline, 3000);
                }
            } else {
                log::debug!("rebuild_pipeline_at: skipping second paused settle pass");
            }
            let (_, pipe_cur, pipe_pend) = self.pipeline.state(gst::ClockTime::ZERO);
            log::info!(
                "rebuild_pipeline_at: after_settle slots={} pipe={:?}/{:?} seq={}",
                self.slots.len(),
                pipe_cur,
                pipe_pend,
                self.scope_frame_seq.load(Ordering::Relaxed)
            );
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

    fn poll_bus(&mut self) -> bool {
        let mut eos = false;
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
                        log::error!(
                            "poll_bus: main pipeline error: {} ({:?})",
                            e.error(),
                            e.debug()
                        );
                    }
                    _ => {}
                }
            }
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
        let vol = clip.volume.clamp(0.0, MAX_PREVIEW_AUDIO_GAIN);
        if let Some(ref vol_elem) = self.audio_volume_element {
            vol_elem.set_property("volume", vol);
        } else {
            self.audio_pipeline.set_property("volume", vol);
        }
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
        if clip.is_audio_only || !clip.reverse || !clip.has_audio {
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
        let vol = clip.volume.clamp(0.0, MAX_PREVIEW_AUDIO_GAIN);
        if let Some(ref vol_elem) = self.audio_volume_element {
            vol_elem.set_property("volume", vol);
        } else {
            self.audio_pipeline.set_property("volume", vol);
        }
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
        if self.reverse_video_ducked_clip_idx == reverse_video_audio_idx {
            if let Some(idx) = reverse_video_audio_idx {
                if let Some(slot) = self.slot_for_clip(idx) {
                    if let Some(ref pad) = slot.audio_mixer_pad {
                        pad.set_property("volume", 0.0_f64);
                    }
                }
            }
            return;
        }

        if let Some(prev_idx) = self.reverse_video_ducked_clip_idx.take() {
            if let Some(clip) = self.clips.get(prev_idx) {
                if let Some(slot) = self.slot_for_clip(prev_idx) {
                    if let Some(ref pad) = slot.audio_mixer_pad {
                        pad.set_property("volume", clip.volume.clamp(0.0, MAX_PREVIEW_AUDIO_GAIN));
                    }
                }
            }
        }

        if let Some(idx) = reverse_video_audio_idx {
            if let Some(slot) = self.slot_for_clip(idx) {
                if let Some(ref pad) = slot.audio_mixer_pad {
                    pad.set_property("volume", 0.0_f64);
                }
            }
            self.reverse_video_ducked_clip_idx = Some(idx);
        }
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
        }
    }
}

fn clip_can_fully_occlude(clip: &ProgramClip) -> bool {
    clip.opacity >= 0.999
        && clip.scale >= 1.0
        && clip.crop_left == 0
        && clip.crop_right == 0
        && clip.crop_top == 0
        && clip.crop_bottom == 0
        && clip.rotate.rem_euclid(360) == 0
        && !clip.flip_h
        && !clip.flip_v
        && clip.position_x.abs() < 0.000_001
        && clip.position_y.abs() < 0.000_001
}

#[cfg(test)]
mod tests {
    use super::{clip_can_fully_occlude, ProgramClip};

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
            denoise: 0.0,
            sharpness: 0.0,
            volume: 1.0,
            pan: 0.0,
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
            reverse: false,
            is_audio_only: false,
            track_index: 0,
            transition_after: String::new(),
            transition_after_ns: 0,
            lut_path: None,
            scale: 1.0,
            opacity: 1.0,
            position_x: 0.0,
            position_y: 0.0,
            shadows: 0.0,
            midtones: 0.0,
            highlights: 0.0,
            has_audio: true,
        }
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
    fn experimental_preview_optimizations_disabled_by_default() {
        // The feature defaults to off; users opt in via Preferences.
        let prefs = crate::ui_state::PreferencesState::default();
        assert!(!prefs.experimental_preview_optimizations);
    }
}

impl Drop for ProgramPlayer {
    fn drop(&mut self) {
        self.teardown_slots();
        let _ = self.pipeline.set_state(gst::State::Null);
        let _ = self.audio_pipeline.set_state(gst::State::Null);
    }
}
