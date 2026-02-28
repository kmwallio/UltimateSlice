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
    /// Rotation in degrees: 0, 90, 180, 270
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
        self.source_in_ns + (offset as f64 * self.speed) as u64
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
    /// Video effect chain bin between decoder and compositor.
    effects_bin: gst::Bin,
    /// Compositor sink pad for this slot's video.
    compositor_pad: Option<gst::Pad>,
    /// Audiomixer sink pad for this slot's audio.
    audio_mixer_pad: Option<gst::Pad>,
    /// `audioconvert` element between decoder audio and audiomixer (must be cleaned up).
    audio_conv: Option<gst::Element>,
    /// Per-slot video effect elements.
    videobalance: Option<gst::Element>,
    gaussianblur: Option<gst::Element>,
    videocrop: Option<gst::Element>,
    videoflip_rotate: Option<gst::Element>,
    videoflip_flip: Option<gst::Element>,
    textoverlay: Option<gst::Element>,
    alpha_filter: Option<gst::Element>,
    capsfilter_zoom: Option<gst::Element>,
    videobox_zoom: Option<gst::Element>,
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
    audio_current_idx: Option<usize>,
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
    proxy_paths: HashMap<String, String>,
    /// GStreamer `level` element on audiomixer output for metering.
    level_element: Option<gst::Element>,
    /// GStreamer `level` element on the audio-only pipeline for metering.
    level_element_audio: Option<gst::Element>,
    pub audio_peak_db: [f64; 2],
    jkl_rate: f64,
    /// Latest RGBA frame captured from the scope appsink.
    latest_scope_frame: Arc<Mutex<Option<ScopeFrame>>>,
    /// When false, the scope appsink callback skips frame allocation (scopes panel hidden).
    scope_enabled: Arc<AtomicBool>,
    /// Monotonic counter incremented whenever a new scope frame is captured.
    scope_frame_seq: Arc<AtomicU64>,
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
    /// Current preview quality divisor.
    preview_divisor: u32,
    /// Wall-clock instant when playback last entered Playing state.
    play_start: Option<Instant>,
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

        // Shared frame store for colour scope analysis.
        let latest_scope_frame: Arc<Mutex<Option<ScopeFrame>>> = Arc::new(Mutex::new(None));
        // Flag set false when the scopes panel is hidden to skip the frame copy allocation.
        let scope_enabled: Arc<AtomicBool> = Arc::new(AtomicBool::new(true));
        let scope_frame_seq: Arc<AtomicU64> = Arc::new(AtomicU64::new(0));

        // Build tee-based sink bin: display + scope appsink (320x180 RGBA).
        let video_sink_bin: gst::Element = (|| {
            let tee = gst::ElementFactory::make("tee").build().ok()?;
            let q1 = gst::ElementFactory::make("queue").build().ok()?;
            let q2 = gst::ElementFactory::make("queue").build().ok()?;
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
                                if let Ok(map) = buffer.map_readable() {
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
                                if let Ok(map) = buffer.map_readable() {
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
            Some(bin.upcast::<gst::Element>())
        })()
        .unwrap_or(video_sink_inner);

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

        let videoconvert_out = gst::ElementFactory::make("videoconvert")
            .build()
            .map_err(|_| anyhow!("videoconvert not available"))?;
        let videoscale_out = gst::ElementFactory::make("videoscale")
            .build()
            .map_err(|_| anyhow!("videoscale not available"))?;
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
        let silence_src = gst::ElementFactory::make("audiotestsrc")
            .property_from_str("wave", "silence")
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

        // Link compositor → project caps → convert → scale/output caps → display/scope
        gst::Element::link_many([
            &compositor,
            &comp_capsfilter,
            &videoconvert_out,
            &videoscale_out,
            &preview_capsfilter,
            &video_sink_bin,
        ])?;

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
        if let Some(ref lv) = level_element_audio {
            audio_pipeline.set_property("audio-filter", lv);
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
                audio_current_idx: None,
                slots: Vec::new(),
                timeline_pos_ns: 0,
                timeline_dur_ns: 0,
                base_timeline_ns: 0,
                project_width: 1920,
                project_height: 1080,
                playback_priority: PlaybackPriority::default(),
                proxy_enabled: false,
                proxy_paths: HashMap::new(),
                level_element,
                level_element_audio,
                audio_peak_db: [-60.0, -60.0],
                jkl_rate: 0.0,
                latest_scope_frame,
                scope_enabled,
                scope_frame_seq,
                current_idx: None,
                _video_sink_bin: video_sink_bin,
                comp_capsfilter,
                black_capsfilter: black_caps,
                preview_capsfilter,
                preview_divisor: 1,
                play_start: None,
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
    }

    /// Enable or disable scope frame capture. When disabled (scopes panel hidden),
    /// the appsink callback drops frames without allocating, saving ~7MB/s at 30fps.
    pub fn set_scope_enabled(&self, enabled: bool) {
        self.scope_enabled.store(enabled, Ordering::Relaxed);
    }

    pub fn update_proxy_paths(&mut self, paths: HashMap<String, String>) {
        self.proxy_paths = paths;
    }

    pub fn set_project_dimensions(&mut self, width: u32, height: u32) {
        self.project_width = width;
        self.project_height = height;
        self.apply_compositor_caps();
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
        let result = (|| -> Result<()> {
            if self.current_idx.is_some() && self.state != PlayerState::Playing {
                self.reseek_slot_for_current();
            }
            let deadline = Instant::now() + Duration::from_millis(500);
            while self.scope_frame_seq.load(Ordering::Relaxed) <= start_seq && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(10));
            }
            let frame = self
                .try_pull_scope_frame()
                .ok_or_else(|| anyhow!("no displayed frame available yet"))?;
            let pixel_count = frame.width.saturating_mul(frame.height);
            let needed = pixel_count.saturating_mul(4);
            if frame.data.len() < needed {
                return Err(anyhow!("scope frame buffer is incomplete"));
            }
            let mut bytes = Vec::with_capacity(32 + pixel_count.saturating_mul(3));
            write!(&mut bytes, "P6\n{} {}\n255\n", frame.width, frame.height)?;
            for rgba in frame.data[..needed].chunks_exact(4) {
                bytes.extend_from_slice(&rgba[..3]);
            }
            std::fs::write(path, bytes)?;
            Ok(())
        })();
        if !was_enabled {
            self.scope_enabled.store(false, Ordering::Relaxed);
        }
        result
    }

    pub fn jkl_rate(&self) -> f64 {
        self.jkl_rate
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
        self.audio_current_idx = None;
        self.teardown_slots();
        let _ = self.pipeline.set_state(gst::State::Ready);
        let _ = self.audio_pipeline.set_state(gst::State::Null);
        self.audio_pipeline.set_property("uri", "");
        let _ = self.audio_pipeline.set_state(gst::State::Ready);
        self.state = PlayerState::Stopped;
        self.timeline_pos_ns = 0;
        self.base_timeline_ns = 0;
        self.play_start = None;
    }

    // ── Transport controls ─────────────────────────────────────────────────

    pub fn seek(&mut self, timeline_pos_ns: u64) {
        let resume_playback = self.state == PlayerState::Playing;
        self.timeline_pos_ns = timeline_pos_ns;
        self.base_timeline_ns = timeline_pos_ns;
        self.play_start = None;
        if self.clips.is_empty() && self.audio_clips.is_empty() {
            return;
        }
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
            self.current_idx = self.clip_at(timeline_pos_ns);
            self.seek_slots_in_place(timeline_pos_ns);
            self.sync_audio_to(timeline_pos_ns);
            return;
        }
        // Full rebuild: needed when the set of active clips has changed (e.g.
        // crossing a clip boundary), on cold start, or when resuming from playing.
        self.rebuild_pipeline_at(timeline_pos_ns);
        // Sync audio-only pipeline
        self.sync_audio_to(timeline_pos_ns);
        if resume_playback {
            self.play_start = Some(Instant::now());
        } else if self.current_idx.is_some() {
            // After rebuilding through Ready state, the compositor's output is
            // held back by the GStreamer PAUSED clock until Playing is entered.
            // A brief Playing pulse flushes the composited frame all the way
            // through to gtk4paintablesink so the paintable is actually updated.
            let _ = self.pipeline.set_state(gst::State::Playing);
            let _ = self.pipeline.state(gst::ClockTime::from_mseconds(150));
            let _ = self.pipeline.set_state(gst::State::Paused);
            self.wait_for_paused_preroll();
        }
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
        let _ = self.pipeline.set_state(gst::State::Playing);
        self.sync_audio_to(pos);
        if let Some(aidx) = self.audio_current_idx {
            let aclip = &self.audio_clips[aidx];
            let asrc = aclip.source_pos_ns(pos);
            let _ = self.audio_pipeline.seek(
                1.0_f64,
                gst::SeekFlags::FLUSH | gst::SeekFlags::KEY_UNIT,
                gst::SeekType::Set,
                gst::ClockTime::from_nseconds(asrc),
                gst::SeekType::Set,
                gst::ClockTime::from_nseconds(aclip.source_out_ns),
            );
            let _ = self.audio_pipeline.set_state(gst::State::Playing);
        }
        self.state = PlayerState::Playing;
        self.base_timeline_ns = pos;
        self.play_start = Some(Instant::now());
        self.jkl_rate = 0.0;
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
    }

    pub fn toggle_play_pause(&mut self) {
        match self.state {
            PlayerState::Playing => self.pause(),
            _ => self.play(),
        }
    }

    pub fn stop(&mut self) {
        self.teardown_slots();
        let _ = self.pipeline.set_state(gst::State::Ready);
        let _ = self.audio_pipeline.set_state(gst::State::Paused);
        self.state = PlayerState::Stopped;
        self.timeline_pos_ns = 0;
        self.base_timeline_ns = 0;
        self.play_start = None;
        self.current_idx = None;
        self.audio_current_idx = None;
        if self.clips.is_empty() && self.audio_clips.is_empty() {
            return;
        }
        self.sync_audio_to(0);
        // Keep stop lightweight; avoid paused rebuild/seek in the stop path.
        let _ = self.pipeline.set_state(gst::State::Paused);
        self.state = PlayerState::Stopped;
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
    }

    // ── Poll ───────────────────────────────────────────────────────────────

    pub fn poll(&mut self) -> bool {
        let _eos = self.poll_bus();

        if self.state == PlayerState::Playing {
            self.audio_peak_db[0] = (self.audio_peak_db[0] - 3.0).max(-60.0);
            self.audio_peak_db[1] = (self.audio_peak_db[1] - 3.0).max(-60.0);
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
            self.audio_current_idx = None;
            self.play_start = None;
            self.timeline_pos_ns = self.timeline_dur_ns;
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

        // Timeline end reached after update?
        if self.timeline_dur_ns > 0 && self.timeline_pos_ns >= self.timeline_dur_ns {
            self.teardown_slots();
            let _ = self.pipeline.set_state(gst::State::Ready);
            let _ = self.audio_pipeline.set_state(gst::State::Ready);
            self.state = PlayerState::Stopped;
            self.current_idx = None;
            self.audio_current_idx = None;
            self.play_start = None;
            self.timeline_pos_ns = self.timeline_dur_ns;
            return true;
        }

        // Detect clip boundary changes: have the active clips changed?
        let desired = self.clips_active_at(new_pos);
        let current: Vec<usize> = self.slots.iter().map(|s| s.clip_idx).collect();
        if desired != current {
            self.rebuild_pipeline_at(new_pos);
            let _ = self.pipeline.set_state(gst::State::Playing);
            // Reset wall-clock base after rebuild.
            self.base_timeline_ns = new_pos;
            self.play_start = Some(Instant::now());
        }

        // Update current_idx to highest-priority active clip.
        self.current_idx = self.clip_at(new_pos);

        // Advance audio pipeline across clip boundaries.
        self.poll_audio(new_pos);

        changed
    }

    // ── Effects / transform updates ────────────────────────────────────────

    pub fn update_current_color(&mut self, brightness: f64, contrast: f64, saturation: f64) {
        self.update_current_effects(brightness, contrast, saturation, 0.0, 0.0);
    }

    pub fn update_current_effects(
        &mut self,
        brightness: f64,
        contrast: f64,
        saturation: f64,
        denoise: f64,
        sharpness: f64,
    ) {
        if let Some(slot) = self.current_idx.and_then(|idx| self.slot_for_clip(idx)) {
            if let Some(ref vb) = slot.videobalance {
                vb.set_property("brightness", brightness.clamp(-1.0, 1.0));
                vb.set_property("contrast", contrast.clamp(0.0, 2.0));
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
                pad.set_property("volume", volume.clamp(0.0, 2.0));
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
        _scale: f64,
        _position_x: f64,
        _position_y: f64,
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
            if self.current_idx == Some(i) {
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
                if self.state != PlayerState::Playing {
                    self.reseek_slot_for_current();
                }
            }
        }
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
    fn wait_for_paused_preroll(&self) {
        let per_decoder_ms = if self.state == PlayerState::Playing { 150 } else { 500 };
        let timeout = gst::ClockTime::from_mseconds(per_decoder_ms);
        for slot in &self.slots {
            let _ = slot.decoder.state(timeout);
        }
    }

    fn seek_slot_decoder(
        slot: &VideoSlot,
        clip: &ProgramClip,
        timeline_pos_ns: u64,
        seek_flags: gst::SeekFlags,
    ) -> bool {
        let source_ns = clip.source_pos_ns(timeline_pos_ns);
        slot.decoder
            .seek(
            clip.speed,
            seek_flags,
            gst::SeekType::Set,
            gst::ClockTime::from_nseconds(source_ns),
            gst::SeekType::Set,
            gst::ClockTime::from_nseconds(clip.source_out_ns),
            )
            .is_ok()
    }

    fn seek_slot_decoder_paused(slot: &VideoSlot, clip: &ProgramClip, timeline_pos_ns: u64) -> bool {
        let source_ns = clip.source_pos_ns(timeline_pos_ns);
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
        let Some(slot) = self.slot_for_clip(idx) else {
            return;
        };
        let clip = &self.clips[idx];
        let _ = Self::seek_slot_decoder_paused_with_retry(slot, clip, self.timeline_pos_ns);
        self.wait_for_paused_preroll();
    }

    /// Returns true if the clips active at `timeline_pos_ns` exactly match the
    /// decoder slots currently loaded (same indices, same order).
    fn clips_match_current_slots(&self, timeline_pos_ns: u64) -> bool {
        let desired = self.clips_active_at(timeline_pos_ns);
        let current: Vec<usize> = self.slots.iter().map(|s| s.clip_idx).collect();
        desired == current
    }

    /// Seek all currently-loaded decoder slots to `timeline_pos_ns` without
    /// rebuilding the pipeline.  Used when the same clips are active at the new
    /// position — avoids the black-frame / first-frame flash caused by going
    /// through Ready state and letting decoders preroll at position 0.
    fn seek_slots_in_place(&mut self, timeline_pos_ns: u64) {
        // Seek every decoder to the new source position.
        for slot in &self.slots {
            let clip = &self.clips[slot.clip_idx];
            let _ = Self::seek_slot_decoder_paused_with_retry(slot, clip, timeline_pos_ns);
        }
        // Wait for decoders to decode the target frame and push it through
        // the effects chain to the compositor.  The flush seek clears the
        // compositor's input buffers; without this wait the Playing pulse
        // below would fire before new frames arrive, producing a black frame.
        self.wait_for_paused_preroll();
        // Playing pulse: per-decoder FLUSH events stop at the compositor's
        // sink pads and are NOT forwarded downstream.  The display sink stays
        // prerolled with its old frame.  Briefly entering Playing starts the
        // clock so the sink consumes the pending compositor buffer; the
        // subsequent Paused transition triggers a fresh preroll with the
        // latest composited frame.
        let _ = self.pipeline.set_state(gst::State::Playing);
        let _ = self.pipeline.state(gst::ClockTime::from_mseconds(150));
        let _ = self.pipeline.set_state(gst::State::Paused);
        self.wait_for_paused_preroll();
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
        if let Some(ref vfr) = slot.videoflip_rotate {
            let method = match rotate {
                90 => "clockwise",
                180 => "rotate-180",
                270 => "counterclockwise",
                _ => "none",
            };
            vfr.set_property_from_str("method", method);
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

        let pos_x = position_x.clamp(-1.0, 1.0);
        let pos_y = position_y.clamp(-1.0, 1.0);
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

    // ── Pipeline rebuild ───────────────────────────────────────────────────

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

        for slot in self.slots.drain(..) {
            // 1. Detach branch elements from the pipeline.
            self.pipeline.remove(&slot.decoder).ok();
            self.pipeline
                .remove(slot.effects_bin.upcast_ref::<gst::Element>())
                .ok();
            if let Some(ref ac) = slot.audio_conv {
                self.pipeline.remove(ac).ok();
            }
            // 2. Stop any residual streaming work on removed elements.
            let _ = slot.decoder.set_state(gst::State::Null);
            let _ = slot.decoder.state(gst::ClockTime::from_mseconds(100));
            let _ = slot.effects_bin.set_state(gst::State::Null);
            let _ = slot.effects_bin.state(gst::ClockTime::from_mseconds(100));
            if let Some(ref ac) = slot.audio_conv {
                let _ = ac.set_state(gst::State::Null);
                let _ = ac.state(gst::ClockTime::from_mseconds(100));
            }
            // 3. Release aggregator request pads after branch shutdown.
            if let Some(ref pad) = slot.compositor_pad {
                self.compositor.release_request_pad(pad);
            }
            if let Some(ref pad) = slot.audio_mixer_pad {
                self.audiomixer.release_request_pad(pad);
            }
        }
    }

    /// Wait briefly for dynamic decode pads to link into the effects chain.
    fn wait_for_video_links(&self) {
        if self.slots.is_empty() {
            return;
        }
        let deadline = Instant::now() + Duration::from_millis(1_000);
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
            log::warn!("ProgramPlayer: timed out waiting for video pad links");
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
        Option<gst::Element>, // videoflip_rotate
        Option<gst::Element>, // videoflip_flip
        Option<gst::Element>, // textoverlay
        Option<gst::Element>, // alpha_filter
        Option<gst::Element>, // capsfilter_zoom
        Option<gst::Element>,
    ) // videobox_zoom
    {
        let bin = gst::Bin::new();

        let videocrop = gst::ElementFactory::make("videocrop").build().ok();
        let conv1 = gst::ElementFactory::make("videoconvert").build().ok();
        let videobalance = gst::ElementFactory::make("videobalance").build().ok();
        let conv2 = gst::ElementFactory::make("videoconvert").build().ok();
        let gaussianblur = gst::ElementFactory::make("gaussianblur").build().ok();
        let conv3 = gst::ElementFactory::make("videoconvert").build().ok();
        let videoflip_rotate = gst::ElementFactory::make("videoflip").build().ok();
        let conv4 = gst::ElementFactory::make("videoconvert").build().ok();
        let videoflip_flip = gst::ElementFactory::make("videoflip")
            .name("videoflip_flip")
            .build()
            .ok();
        let alpha_filter = gst::ElementFactory::make("alpha").build().ok();
        let textoverlay = gst::ElementFactory::make("textoverlay").build().ok();

        let conv_zoom = gst::ElementFactory::make("videoconvert").build().ok();
        let videoscale_norm = gst::ElementFactory::make("videoscale")
            .property("add-borders", true)
            .build()
            .ok();
        let capsfilter_proj = gst::ElementFactory::make("capsfilter").build().ok();
        let videoscale_zoom = gst::ElementFactory::make("videoscale").build().ok();
        let capsfilter_zoom = gst::ElementFactory::make("capsfilter").build().ok();
        let videobox_zoom = gst::ElementFactory::make("videobox").build().ok();

        // Set initial values from clip data.
        if let Some(ref vb) = videobalance {
            vb.set_property("brightness", clip.brightness.clamp(-1.0, 1.0));
            vb.set_property("contrast", clip.contrast.clamp(0.0, 2.0));
            vb.set_property("saturation", clip.saturation.clamp(0.0, 2.0));
        }
        if let Some(ref gb) = gaussianblur {
            let sigma = (clip.denoise * 4.0 - clip.sharpness * 6.0).clamp(-20.0, 20.0);
            gb.set_property("sigma", sigma);
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

        // Build chain: collect all available elements in order.
        let mut chain: Vec<gst::Element> = Vec::new();
        if let Some(ref e) = videocrop {
            chain.push(e.clone());
        }
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
        if let Some(ref e) = alpha_filter {
            chain.push(e.clone());
        }
        if let Some(ref e) = textoverlay {
            chain.push(e.clone());
        }
        if let Some(ref e) = conv_zoom {
            chain.push(e.clone());
        }
        if let Some(ref e) = videoscale_norm {
            chain.push(e.clone());
        }
        if let Some(ref e) = capsfilter_proj {
            chain.push(e.clone());
        }
        if let Some(ref e) = videoscale_zoom {
            chain.push(e.clone());
        }
        if let Some(ref e) = capsfilter_zoom {
            chain.push(e.clone());
        }
        if let Some(ref e) = videobox_zoom {
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
            videoflip_rotate,
            videoflip_flip,
            textoverlay,
            alpha_filter,
            capsfilter_zoom,
            videobox_zoom,
        )
    }

    /// Core method: tear down all slots and rebuild for clips active at `timeline_pos`.
    fn rebuild_pipeline_at(&mut self, timeline_pos: u64) {
        let was_playing = self.state == PlayerState::Playing;

        // Tear down existing slots FIRST — each decoder is set to Null
        // individually, which avoids a pipeline-wide state change on
        // elements that may be mid-transition (causing a main-thread
        // deadlock when gtk4paintablesink needs the main loop to complete
        // its transition).
        self.teardown_slots();

        // Go through Ready to reset the pipeline's base-time / running-time.
        // Without this, the always-on videotestsrc accumulates running-time
        // while newly-created decoders start at running-time 0 after their
        // flush seek.  The compositor waits for the decoders to catch up,
        // deadlocking the pipeline and freezing the playhead.
        // Now that slots are torn down, only the lightweight background
        // sources (videotestsrc, audiotestsrc) remain — this is fast and safe.
        let _ = self.pipeline.set_state(gst::State::Ready);

        let active = self.clips_active_at(timeline_pos);
        if active.is_empty() {
            self.current_idx = None;
            // Move back to Paused so the background sources are ready.
            let _ = self.pipeline.set_state(gst::State::Paused);
            if was_playing {
                let _ = self.pipeline.set_state(gst::State::Playing);
            }
            return;
        }

        // Update current_idx to highest-priority clip.
        self.current_idx = active.last().copied();

        for (zorder_offset, &clip_idx) in active.iter().enumerate() {
            let clip = self.clips[clip_idx].clone();

            // Resolve proxy path.
            let effective_path = if self.proxy_enabled {
                let key = crate::media::proxy_cache::proxy_key(
                    &clip.source_path,
                    clip.lut_path.as_deref(),
                );
                self.proxy_paths
                    .get(&key)
                    .cloned()
                    .unwrap_or_else(|| clip.source_path.clone())
            } else {
                clip.source_path.clone()
            };
            let uri = format!("file://{}", effective_path);

            // Build per-slot effects bin.
            let (
                effects_bin,
                videobalance,
                gaussianblur,
                videocrop,
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
                    continue;
                }
            };

            // Add to pipeline.
            if self.pipeline.add(&decoder).is_err() {
                log::warn!("ProgramPlayer: failed to add decoder to pipeline");
                continue;
            }
            if self
                .pipeline
                .add(effects_bin.upcast_ref::<gst::Element>())
                .is_err()
            {
                self.pipeline.remove(&decoder).ok();
                log::warn!("ProgramPlayer: failed to add effects_bin to pipeline");
                continue;
            }

            // Request compositor sink pad (zorder > 0; 0 is reserved for black bg).
            let comp_pad = match self.compositor.request_pad_simple("sink_%u") {
                Some(p) => p,
                None => {
                    self.pipeline.remove(&decoder).ok();
                    self.pipeline
                        .remove(effects_bin.upcast_ref::<gst::Element>())
                        .ok();
                    continue;
                }
            };
            comp_pad.set_property("zorder", (zorder_offset + 1) as u32);
            comp_pad.set_property("alpha", clip.opacity.clamp(0.0, 1.0));

            // Link effects_bin src → compositor pad.
            if let Some(src) = effects_bin.static_pad("src") {
                let _ = src.link(&comp_pad);
            }

            // Create audio path: audioconvert → audiomixer pad.
            let audio_conv = gst::ElementFactory::make("audioconvert").build().ok();
            let amix_pad = if let Some(ref ac) = audio_conv {
                if self.pipeline.add(ac).is_ok() {
                    if let Some(mp) = self.audiomixer.request_pad_simple("sink_%u") {
                        mp.set_property("volume", clip.volume.clamp(0.0, 2.0));
                        if let Some(ac_src) = ac.static_pad("src") {
                            let _ = ac_src.link(&mp);
                        }
                        Some(mp)
                    } else {
                        None
                    }
                } else {
                    None
                }
            } else {
                None
            };

            // Connect uridecodebin pad-added for dynamic linking.
            let effects_sink = effects_bin.static_pad("sink");
            let audio_sink = audio_conv.as_ref().and_then(|ac| ac.static_pad("sink"));
            let video_linked = Arc::new(AtomicBool::new(false));
            let video_linked_for_cb = video_linked.clone();
            decoder.connect_pad_added(move |_dec, pad| {
                let caps = pad.current_caps().or_else(|| Some(pad.query_caps(None)));
                if let Some(caps) = caps {
                    if let Some(s) = caps.structure(0) {
                        let name = s.name().to_string();
                        if name.starts_with("video/") {
                            if let Some(ref sink) = effects_sink {
                                if pad.link(sink).is_ok() {
                                    video_linked_for_cb.store(true, Ordering::Relaxed);
                                }
                            }
                        } else if name.starts_with("audio/") {
                            if let Some(ref sink) = audio_sink {
                                let _ = pad.link(sink);
                            }
                        }
                    }
                }
            });

            // Tune inner elements created by uridecodebin for local-file playback:
            // – cap decoder thread count to avoid thread explosion across 3+ tracks
            // – keep multiqueue limits seek-safe while paused scrubbing
            let tune_playback_pipeline = was_playing;
            decoder.connect("deep-element-added", false, move |args| {
                // deep-element-added: args[0]=uridecodebin, args[1]=parent_bin, args[2]=element
                let child = args[2].get::<gst::Element>().ok()?;
                let factory_name = child
                    .factory()
                    .map(|f| f.name().to_string())
                    .unwrap_or_default();
                // Cap H.264/HEVC/VP9 decoder threads (avdec_* defaults to num_cpus).
                if tune_playback_pipeline
                    && (factory_name.starts_with("avdec_h264")
                        || factory_name.starts_with("avdec_vp8"))
                {
                    child.set_property("max-threads", 2i32);
                } else if tune_playback_pipeline
                    && (factory_name.starts_with("avdec_h265")
                        || factory_name.starts_with("avdec_vp9"))
                {
                    child.set_property("max-threads", 4i32);
                } else if tune_playback_pipeline && factory_name == "multiqueue" {
                    // Keep paused scrubbing seek-safe: ACCURATE seeks may need to decode
                    // a full GOP before reaching the target frame, so paused rebuilds keep
                    // default multiqueue behavior. During active playback, keep a 10MB
                    // cap per slot to reduce decode buffering pressure.
                    child.set_property("max-size-time", 0u64);
                    child.set_property("max-size-bytes", 10_485_760u32);
                }
                None
            });

            // Sync element states with pipeline.
            let _ = decoder.sync_state_with_parent();
            let _ = effects_bin.sync_state_with_parent();
            if let Some(ref ac) = audio_conv {
                let _ = ac.sync_state_with_parent();
            }

            // Apply per-clip transform.
            let slot_ref_for_transform = VideoSlot {
                clip_idx,
                decoder: decoder.clone(),
                video_linked: video_linked.clone(),
                effects_bin: effects_bin.clone(),
                compositor_pad: Some(comp_pad.clone()),
                audio_mixer_pad: amix_pad.clone(),
                audio_conv: audio_conv.clone(),
                videobalance: videobalance.clone(),
                gaussianblur: gaussianblur.clone(),
                videocrop: videocrop.clone(),
                videoflip_rotate: videoflip_rotate.clone(),
                videoflip_flip: videoflip_flip.clone(),
                textoverlay: textoverlay.clone(),
                alpha_filter: alpha_filter.clone(),
                capsfilter_zoom: capsfilter_zoom.clone(),
                videobox_zoom: videobox_zoom.clone(),
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

            self.slots.push(VideoSlot {
                clip_idx,
                decoder,
                video_linked,
                effects_bin,
                compositor_pad: Some(comp_pad),
                audio_mixer_pad: amix_pad,
                audio_conv,
                videobalance,
                gaussianblur,
                videocrop,
                videoflip_rotate,
                videoflip_flip,
                textoverlay,
                alpha_filter,
                capsfilter_zoom,
                videobox_zoom,
            });
        }

        // Transition to Paused after all branches are added so decoder pad-linking
        // and preroll happen in the same cycle as the seek passes below.
        let _ = self.pipeline.set_state(gst::State::Paused);
        self.wait_for_video_links();

        // Wait for pipeline to preroll so seeks are accepted.
        // When paused (scrubbing), always wait — we need a decoded frame for
        // the preview.  When playing, respect the playback priority to avoid
        // stutter during clip boundary crossings.
        if !was_playing || self.should_block_preroll() {
            self.wait_for_paused_preroll();
        }

        // Ensure decoders have reached their parent state before issuing seeks.
        for slot in &self.slots {
            let _ = slot.decoder.state(gst::ClockTime::from_mseconds(200));
        }

        // Seek each decoder to its source position with stop boundary.
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

        // In paused mode, perform a two-pass settle:
        // 1) wait once for initial decoder/linking work to settle,
        // 2) re-seek decoders after pads are likely linked,
        // 3) wait again for a fresh preroll frame.
        if !was_playing {
            self.wait_for_paused_preroll();
            for slot in &self.slots {
                let clip = &self.clips[slot.clip_idx];
                let _ = Self::seek_slot_decoder_paused_with_retry(slot, clip, timeline_pos);
            }
            let _ = self.pipeline.set_state(gst::State::Paused);
            self.wait_for_paused_preroll();
            self.reseek_slot_for_current();
        }

        // Restore pipeline state.
        if was_playing {
            let _ = self.pipeline.set_state(gst::State::Playing);
        }
    }

    // ── Bus / metering ─────────────────────────────────────────────────────

    fn poll_bus(&mut self) -> bool {
        let mut eos = false;
        if let Some(bus) = self.pipeline.bus() {
            while let Some(msg) = bus.pop() {
                match msg.view() {
                    gstreamer::MessageView::Eos(_) => eos = true,
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
                                    self.audio_peak_db[0] = self.audio_peak_db[0].max(l);
                                    self.audio_peak_db[1] = self.audio_peak_db[1].max(r);
                                }
                            }
                        }
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
                                self.audio_peak_db[0] = self.audio_peak_db[0].max(l);
                                self.audio_peak_db[1] = self.audio_peak_db[1].max(r);
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
        self.audio_pipeline
            .set_property("volume", clip.volume.clamp(0.0, 2.0));
        if self.audio_current_idx == Some(idx) {
            let _ = self.audio_pipeline.seek(
                1.0_f64,
                gst::SeekFlags::FLUSH | gst::SeekFlags::KEY_UNIT,
                gst::SeekType::Set,
                gst::ClockTime::from_nseconds(source_seek_ns),
                gst::SeekType::Set,
                gst::ClockTime::from_nseconds(clip.source_out_ns),
            );
        } else {
            let uri = format!("file://{}", clip.source_path);
            let _ = self.audio_pipeline.set_state(gst::State::Ready);
            self.audio_pipeline.set_property("uri", &uri);
            let _ = self.audio_pipeline.set_state(gst::State::Paused);
            let _ = self.audio_pipeline.seek(
                1.0_f64,
                gst::SeekFlags::FLUSH | gst::SeekFlags::KEY_UNIT,
                gst::SeekType::Set,
                gst::ClockTime::from_nseconds(source_seek_ns),
                gst::SeekType::Set,
                gst::ClockTime::from_nseconds(clip.source_out_ns),
            );
        }
        self.audio_current_idx = Some(idx);
    }

    fn sync_audio_to(&mut self, timeline_pos_ns: u64) {
        if let Some(idx) = self.audio_clip_at(timeline_pos_ns) {
            self.load_audio_clip_idx(idx, timeline_pos_ns);
        } else {
            let _ = self.audio_pipeline.set_state(gst::State::Ready);
            self.audio_current_idx = None;
        }
    }

    fn poll_audio(&mut self, timeline_pos_ns: u64) {
        let wanted = self.audio_clip_at(timeline_pos_ns);
        if wanted != self.audio_current_idx {
            match wanted {
                Some(idx) => {
                    self.load_audio_clip_idx(idx, timeline_pos_ns);
                    let _ = self.audio_pipeline.set_state(gst::State::Playing);
                }
                None => {
                    let _ = self.audio_pipeline.set_state(gst::State::Ready);
                    self.audio_current_idx = None;
                }
            }
        }
    }
}

impl Drop for ProgramPlayer {
    fn drop(&mut self) {
        self.teardown_slots();
        let _ = self.pipeline.set_state(gst::State::Null);
        let _ = self.audio_pipeline.set_state(gst::State::Null);
    }
}
