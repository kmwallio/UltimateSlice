/// A "program monitor" player that plays the assembled timeline clip-by-clip.
///
/// It uses a standard `playbin` pipeline and advances through clips in
/// timeline order. Seeking moves to a timeline position and loads the
/// appropriate clip at the right source offset.
use anyhow::{anyhow, Result};
use gstreamer as gst;
use gstreamer::prelude::*;
use gstreamer_app::AppSink;
use glib;
use crate::media::player::PlayerState;
use crate::ui_state::PlaybackPriority;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

/// A single RGBA frame pulled from the scope appsink for colour scope analysis.
#[derive(Clone)]
pub struct ScopeFrame {
    pub data: Vec<u8>,
    pub width: usize,
    pub height: usize,
}

#[derive(Clone, Debug)]
pub struct ProgramClip {
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
}

impl ProgramClip {
    pub fn source_duration_ns(&self) -> u64 {
        self.source_out_ns.saturating_sub(self.source_in_ns)
    }
    /// Timeline duration accounting for speed.
    pub fn duration_ns(&self) -> u64 {
        let src = self.source_duration_ns();
        if self.speed > 0.0 { (src as f64 / self.speed) as u64 } else { src }
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

pub struct ProgramPlayer {
    pipeline: gst::Element,
    state: PlayerState,
    pub clips: Vec<ProgramClip>,      // video-track clips only
    audio_clips: Vec<ProgramClip>,    // audio-track clips only
    audio_pipeline: gst::Element,     // separate playbin for audio-only clips
    audio_current_idx: Option<usize>,
    current_idx: Option<usize>,
    /// Anchor pair for converting queried source position back into timeline position.
    /// Updated on every video seek/load.
    seek_anchor_timeline_ns: u64,
    seek_anchor_source_ns: u64,
    /// Whether query_position is segment-relative (true) or absolute (false)
    /// for the currently loaded video segment.
    seek_reports_relative: Option<bool>,
    /// Cached timeline position in nanoseconds (updated by `poll`)
    pub timeline_pos_ns: u64,
    /// Total timeline duration
    pub timeline_dur_ns: u64,
    /// videobalance element for per-clip color correction
    videobalance: Option<gst::Element>,
    /// gaussianblur element for per-clip denoise/sharpness
    gaussianblur: Option<gst::Element>,
    /// audiopanorama element for per-clip audio pan
    audiopanorama: Option<gst::Element>,
    /// videocrop element for per-clip cropping
    videocrop: Option<gst::Element>,
    /// videoflip element for rotation
    videoflip_rotate: Option<gst::Element>,
    /// videoflip element for horizontal/vertical flip
    videoflip_flip: Option<gst::Element>,
    /// textoverlay element for per-clip title
    textoverlay: Option<gst::Element>,
    /// alpha element for transition fade approximation in preview.
    alpha_filter: Option<gst::Element>,
    playback_priority: PlaybackPriority,
    /// Proxy mode: when enabled, load_clip_idx uses proxy paths for preview.
    proxy_enabled: bool,
    /// Map from original source path → proxy file path.
    proxy_paths: HashMap<String, String>,
    /// Second pipeline for cross-dissolve preview: plays the incoming clip while
    /// pipeline1 fades out the outgoing clip.
    pipeline2: gst::Element,
    /// Whether pipeline2 is currently active (playing the incoming transition clip).
    transition_active: bool,
    /// Index into `clips` of the incoming clip currently loaded in pipeline2.
    transition_incoming_idx: Option<usize>,
    /// Deferred seek position for pipeline2: set when activating a transition and
    /// cleared on the next poll() tick after the seek is issued. Avoids any blocking
    /// wait for pipeline2 preroll on the GTK UI thread.
    pipeline2_pending_seek_ns: Option<u64>,
    /// GStreamer `level` element for metering audio on the main pipeline.
    level_element: Option<gst::Element>,
    /// GStreamer `level` element for metering audio on the audio-only pipeline.
    level_element_audio: Option<gst::Element>,
    /// Current audio peak level in dBFS per channel [left, right].
    /// Updated each poll tick from `level` element bus messages; decays toward -60 over time.
    pub audio_peak_db: [f64; 2],
    /// J/K/L shuttle rate: 0.0 = paused, positive = forward N×, negative = reverse N×.
    jkl_rate: f64,
    /// Latest RGBA frame captured from the scope appsink, updated via GStreamer
    /// callbacks on both preroll (seek/pause) and new-sample (playback).
    /// `Arc<Mutex<>>` because callbacks fire on GStreamer threads.
    latest_scope_frame: Arc<Mutex<Option<ScopeFrame>>>,
}

impl ProgramPlayer {
    pub fn new() -> Result<(Self, gdk4::Paintable, gdk4::Paintable)> {
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

        // Shared frame store updated by the appsink callbacks (GStreamer threads → Arc<Mutex>).
        let latest_scope_frame: Arc<Mutex<Option<ScopeFrame>>> = Arc::new(Mutex::new(None));

        // Build a tee-based sink bin so frames are delivered to both the display
        // paintable and a small appsink (320×180 RGBA) for colour scope analysis.
        // The appsink uses callbacks (new_preroll + new_sample) so every decoded
        // frame — whether during playback or after a seek/pause preroll — is captured.
        let video_sink: gst::Element = (|| {
            let tee   = gst::ElementFactory::make("tee").build().ok()?;
            let q1    = gst::ElementFactory::make("queue").build().ok()?;
            let q2    = gst::ElementFactory::make("queue").build().ok()?;
            let scale = gst::ElementFactory::make("videoscale").build().ok()?;
            let conv  = gst::ElementFactory::make("videoconvert").build().ok()?;
            let sink  = AppSink::builder()
                .caps(&gst::Caps::builder("video/x-raw")
                    .field("format", "RGBA")
                    .field("width",  320i32)
                    .field("height", 180i32)
                    .build())
                .max_buffers(1u32)
                .drop(true)
                .build();

            // Wire callbacks: captures both preroll (seek/pause) and live samples (playback).
            let lsf_preroll = latest_scope_frame.clone();
            let lsf_sample  = latest_scope_frame.clone();
            sink.set_callbacks(
                gstreamer_app::AppSinkCallbacks::builder()
                    .new_preroll(move |appsink| {
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
                                        }
                                    }
                                }
                            }
                        }
                        Ok(gst::FlowSuccess::Ok)
                    })
                    .new_sample(move |appsink| {
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
                                        }
                                    }
                                }
                            }
                        }
                        Ok(gst::FlowSuccess::Ok)
                    })
                    .build()
            );

            let sink_bin = gst::Bin::new();
            sink_bin.add_many([&tee, &q1, &video_sink_inner, &q2, &scale, &conv, sink.upcast_ref::<gst::Element>()]).ok()?;

            // tee src_0 → queue1 → display sink
            let tee_src0 = tee.request_pad_simple("src_%u")?;
            tee_src0.link(&q1.static_pad("sink")?).ok()?;
            gst::Element::link_many([&q1, &video_sink_inner]).ok()?;

            // tee src_1 → queue2 → videoscale → videoconvert → appsink
            let tee_src1 = tee.request_pad_simple("src_%u")?;
            tee_src1.link(&q2.static_pad("sink")?).ok()?;
            gst::Element::link_many([&q2, &scale, &conv, sink.upcast_ref::<gst::Element>()]).ok()?;

            // Expose the tee's sink pad as the bin's ghost sink
            let ghost = gst::GhostPad::with_target(&tee.static_pad("sink")?).ok()?;
            sink_bin.add_pad(&ghost).ok()?;

            Some(sink_bin.upcast::<gst::Element>())
        })().unwrap_or(video_sink_inner);

        let pipeline = gst::ElementFactory::make("playbin")
            .property("video-sink", &video_sink)
            .build()?;

        let videobalance = gst::ElementFactory::make("videobalance").build().ok();
        let gaussianblur = gst::ElementFactory::make("gaussianblur").build().ok();
        let audiopanorama = gst::ElementFactory::make("audiopanorama").build().ok();
        let videocrop = gst::ElementFactory::make("videocrop").build().ok();
        let videoflip_rotate = gst::ElementFactory::make("videoflip").build().ok();
        let videoflip_flip = gst::ElementFactory::make("videoflip")
            .name("videoflip_flip")
            .build()
            .ok();
        let textoverlay = gst::ElementFactory::make("textoverlay").build().ok();
        let alpha_filter = gst::ElementFactory::make("alpha").build().ok();

        if videobalance.is_some() && gaussianblur.is_some() {
            let vb = videobalance.as_ref().unwrap();
            let gb = gaussianblur.as_ref().unwrap();
            gb.set_property("sigma", 0.0_f64);
            let bin = gst::Bin::new();
            let conv1 = gst::ElementFactory::make("videoconvert").build()
                .expect("videoconvert must be available");
            let conv2 = gst::ElementFactory::make("videoconvert").build()
                .expect("videoconvert must be available");

            if let (Some(ref vc), Some(ref vfr), Some(ref vff)) =
                (&videocrop, &videoflip_rotate, &videoflip_flip)
            {
                let conv3 = gst::ElementFactory::make("videoconvert").build()
                    .expect("videoconvert must be available");
                let conv4 = gst::ElementFactory::make("videoconvert").build()
                    .expect("videoconvert must be available");
                if let (Some(ref a), Some(ref to)) = (&alpha_filter, &textoverlay) {
                    a.set_property("alpha", 1.0_f64);
                    to.set_property("text", "");
                    to.set_property("silent", true);
                    bin.add_many([vc, &conv1, vb, &conv2, gb, &conv3, vfr, &conv4, vff, a, to]).ok();
                    gst::Element::link_many([vc, &conv1, vb, &conv2, gb, &conv3, vfr, &conv4, vff, a, to]).ok();
                    let sink_pad = vc.static_pad("sink").unwrap();
                    let src_pad = to.static_pad("src").unwrap();
                    bin.add_pad(&gst::GhostPad::with_target(&sink_pad).unwrap()).ok();
                    bin.add_pad(&gst::GhostPad::with_target(&src_pad).unwrap()).ok();
                } else if let Some(ref to) = textoverlay {
                    to.set_property("text", "");
                    to.set_property("silent", true);
                    bin.add_many([vc, &conv1, vb, &conv2, gb, &conv3, vfr, &conv4, vff, to]).ok();
                    gst::Element::link_many([vc, &conv1, vb, &conv2, gb, &conv3, vfr, &conv4, vff, to]).ok();
                    let sink_pad = vc.static_pad("sink").unwrap();
                    let src_pad = to.static_pad("src").unwrap();
                    bin.add_pad(&gst::GhostPad::with_target(&sink_pad).unwrap()).ok();
                    bin.add_pad(&gst::GhostPad::with_target(&src_pad).unwrap()).ok();
                } else {
                    bin.add_many([vc, &conv1, vb, &conv2, gb, &conv3, vfr, &conv4, vff]).ok();
                    gst::Element::link_many([vc, &conv1, vb, &conv2, gb, &conv3, vfr, &conv4, vff]).ok();
                    let sink_pad = vc.static_pad("sink").unwrap();
                    let src_pad = vff.static_pad("src").unwrap();
                    bin.add_pad(&gst::GhostPad::with_target(&sink_pad).unwrap()).ok();
                    bin.add_pad(&gst::GhostPad::with_target(&src_pad).unwrap()).ok();
                }
            } else {
                bin.add_many([vb, &conv1, gb]).ok();
                gst::Element::link_many([vb, &conv1, gb]).ok();
                let sink_pad = vb.static_pad("sink").unwrap();
                let src_pad = gb.static_pad("src").unwrap();
                bin.add_pad(&gst::GhostPad::with_target(&sink_pad).unwrap()).ok();
                bin.add_pad(&gst::GhostPad::with_target(&src_pad).unwrap()).ok();
            }
            pipeline.set_property("video-filter", &bin);
        } else if let Some(ref vb) = videobalance {
            pipeline.set_property("video-filter", vb);
        }

        // Build audio filter: audiopanorama → audioconvert → level (metering).
        // If level is unavailable, fall back to audiopanorama alone.
        let level_element = gst::ElementFactory::make("level")
            .property("post-messages", true)
            .property("interval", 50_000_000u64) // 50ms
            .build()
            .ok();
        if let Some(ref ap) = audiopanorama {
            if let Some(ref lv) = level_element {
                // Wrap audiopanorama + audioconvert + level into a bin for audio-filter.
                let audio_bin = gst::Bin::new();
                let aconv = gst::ElementFactory::make("audioconvert").build()
                    .expect("audioconvert must be available");
                audio_bin.add_many([ap, &aconv, lv]).ok();
                gst::Element::link_many([ap, &aconv, lv]).ok();
                let sink_pad = ap.static_pad("sink").unwrap();
                let src_pad = lv.static_pad("src").unwrap();
                audio_bin.add_pad(&gst::GhostPad::with_target(&sink_pad).unwrap()).ok();
                audio_bin.add_pad(&gst::GhostPad::with_target(&src_pad).unwrap()).ok();
                pipeline.set_property("audio-filter", &audio_bin);
            } else {
                pipeline.set_property("audio-filter", ap);
            }
        } else if let Some(ref lv) = level_element {
            pipeline.set_property("audio-filter", lv);
        }

        // Dedicated audio-only pipeline: video routed to fakesink so it plays
        // audio-track clips without interfering with the visual display.
        let fakevideo = gst::ElementFactory::make("fakesink").build()
            .unwrap_or_else(|_| gst::ElementFactory::make("autovideosink").build().unwrap());
        let audio_pipeline = gst::ElementFactory::make("playbin")
            .property("video-sink", &fakevideo)
            .build()
            .unwrap_or_else(|_| gst::ElementFactory::make("playbin").build().unwrap());

        // Add a second level element to the audio-only pipeline for metering.
        let level_element_audio = gst::ElementFactory::make("level")
            .property("post-messages", true)
            .property("interval", 50_000_000u64) // 50ms
            .build()
            .ok();
        if let Some(ref lv) = level_element_audio {
            audio_pipeline.set_property("audio-filter", lv);
        }

        // Second pipeline for cross-dissolve transition preview.
        // It is a bare playbin (no color-correction filters) used only to feed the
        // incoming clip to a second gtk4paintablesink during the transition window.
        // Audio is suppressed so only pipeline1 emits sound.
        let paintablesink2 = gst::ElementFactory::make("gtk4paintablesink")
            .build()
            .map_err(|_| anyhow!("gtk4paintablesink not available for transition pipeline"))?;
        let paintable2 = {
            let obj = paintablesink2.property::<glib::Object>("paintable");
            obj.dynamic_cast::<gdk4::Paintable>()
                .expect("gtk4paintablesink paintable must implement Paintable")
        };
        let video_sink2 = match gst::ElementFactory::make("glsinkbin")
            .property("sink", &paintablesink2)
            .build()
        {
            Ok(s) => s,
            Err(_) => paintablesink2.clone(),
        };
        let fakevideo2 = gst::ElementFactory::make("fakesink").build()
            .unwrap_or_else(|_| gst::ElementFactory::make("autovideosink").build().unwrap());
        let pipeline2 = gst::ElementFactory::make("playbin")
            .property("video-sink", &video_sink2)
            .property("audio-sink", &fakevideo2)
            .build()
            .unwrap_or_else(|_| {
                gst::ElementFactory::make("playbin").build().unwrap()
            });

        Ok((
            Self {
                pipeline,
                state: PlayerState::Stopped,
                clips: Vec::new(),
                audio_clips: Vec::new(),
                audio_pipeline,
                audio_current_idx: None,
                current_idx: None,
                seek_anchor_timeline_ns: 0,
                seek_anchor_source_ns: 0,
                seek_reports_relative: None,
                timeline_pos_ns: 0,
                timeline_dur_ns: 0,
                videobalance,
                gaussianblur,
                audiopanorama,
                videocrop,
                videoflip_rotate,
                videoflip_flip,
                textoverlay,
                alpha_filter,
                playback_priority: PlaybackPriority::default(),
                proxy_enabled: false,
                proxy_paths: HashMap::new(),
                pipeline2,
                transition_active: false,
                transition_incoming_idx: None,
                pipeline2_pending_seek_ns: None,
                level_element,
                level_element_audio,
                audio_peak_db: [-60.0, -60.0],
                latest_scope_frame,
                jkl_rate: 0.0,
            },
            paintable,
            paintable2,
        ))
    }

    pub fn set_playback_priority(&mut self, playback_priority: PlaybackPriority) {
        self.playback_priority = playback_priority;
    }

    pub fn set_proxy_enabled(&mut self, enabled: bool) {
        self.proxy_enabled = enabled;
    }

    /// Update the proxy path mapping. Called when new proxy transcodes complete.
    pub fn update_proxy_paths(&mut self, paths: HashMap<String, String>) {
        self.proxy_paths = paths;
    }

    /// Returns (opacity_a, opacity_b) for the two program monitor pictures.
    /// Outside transition windows: (1.0, 0.0). During a cross-dissolve: complementary values.
    pub fn transition_opacities(&self) -> (f64, f64) {
        // If pipeline2 is not actually running, always show only picture_a.
        if !self.transition_active { return (1.0, 0.0); }
        let Some(idx) = self.current_idx else { return (1.0, 0.0) };
        let clip = &self.clips[idx];
        if clip.transition_after != "cross_dissolve" || clip.transition_after_ns == 0 {
            return (1.0, 0.0);
        }
        let d = clip.transition_after_ns.min(clip.duration_ns());
        let start = clip.timeline_end_ns().saturating_sub(d);
        if self.timeline_pos_ns >= start && self.timeline_pos_ns < clip.timeline_end_ns() && d > 0 {
            let t = ((self.timeline_pos_ns - start) as f64 / d as f64).clamp(0.0, 1.0);
            return (1.0 - t, t);
        }
        (1.0, 0.0)
    }

    /// Returns the latest decoded scope frame (320×180 RGBA), or `None` if no
    /// frame has been captured yet. Updated by GStreamer callbacks on both preroll
    /// (every seek/pause) and new-sample (every decoded frame during playback).
    pub fn try_pull_scope_frame(&self) -> Option<ScopeFrame> {
        self.latest_scope_frame.lock().ok()?.clone()
    }

    /// Current J/K/L shuttle rate (0.0 = paused, >0 = forward, <0 = reverse).
    pub fn jkl_rate(&self) -> f64 {
        self.jkl_rate
    }

    /// Set the J/K/L shuttle rate and start/continue playback at that rate.
    ///
    /// - `rate == 0.0` — pause.
    /// - `rate > 0` — forward playback at `rate`× speed.
    /// - `rate < 0` — reverse playback at `|rate|`× speed via a negative-rate
    ///   GStreamer seek. Falls back gracefully: if the pipeline rejects the
    ///   negative seek the player stops at the current position.
    pub fn set_jkl_rate(&mut self, rate: f64) {
        self.jkl_rate = rate;

        if rate == 0.0 {
            self.pause();
            return;
        }

        let pos = self.timeline_pos_ns;

        // Ensure a clip is loaded at the current position.
        if self.current_idx.is_none() {
            if let Some(idx) = self.clip_at(pos) {
                self.load_clip_idx(idx, pos);
            } else {
                return; // no clip here — nothing to play
            }
        }

        if let Some(idx) = self.current_idx {
            let clip = &self.clips[idx];
            let source_pos = clip.source_pos_ns(pos);
            let abs_rate = rate.abs();

            if rate > 0.0 {
                // Forward: standard positive-rate seek.
                let _ = self.pipeline.seek(
                    abs_rate,
                    gst::SeekFlags::FLUSH | gst::SeekFlags::KEY_UNIT,
                    gst::SeekType::Set,
                    gst::ClockTime::from_nseconds(source_pos),
                    gst::SeekType::None,
                    gst::ClockTime::NONE,
                );
            } else {
                // Reverse: negative-rate seek with stop at the clip's in-point.
                // GStreamer's stop position for reverse is the segment start (nearest
                // keyframe ≥ start); we use clip source_in_ns as the natural stop.
                let stop_pos = clip.source_in_ns;
                let seek_ok = self.pipeline.seek(
                    -abs_rate,
                    gst::SeekFlags::FLUSH | gst::SeekFlags::KEY_UNIT,
                    gst::SeekType::Set,
                    gst::ClockTime::from_nseconds(stop_pos),
                    gst::SeekType::Set,
                    gst::ClockTime::from_nseconds(source_pos),
                ).is_ok();
                if !seek_ok {
                    // Format doesn't support reverse — silently stop.
                    self.jkl_rate = 0.0;
                    self.pause();
                    return;
                }
            }

            let _ = self.pipeline.set_state(gst::State::Playing);
            self.state = PlayerState::Playing;
            self.seek_anchor_timeline_ns = pos;
            self.seek_anchor_source_ns = source_pos;
            self.seek_reports_relative = None;
        }
    }

    /// Activate the cross-dissolve transition preview: load the incoming clip into
    /// pipeline2 and begin playing it. Called when entering the transition window.
    /// Non-blocking: pipeline2 is set to Playing immediately and the seek to
    /// `source_in_ns` is deferred to the next `poll()` tick via `pipeline2_pending_seek_ns`.
    fn activate_transition(&mut self, incoming_idx: usize) {
        if self.transition_incoming_idx == Some(incoming_idx) {
            return; // already active for this clip
        }
        let clip = &self.clips[incoming_idx];
        let source_path = if self.proxy_enabled {
            let key = crate::media::proxy_cache::proxy_key(
                &clip.source_path,
                clip.lut_path.as_deref(),
            );
            self.proxy_paths.get(&key)
                .cloned()
                .unwrap_or_else(|| clip.source_path.clone())
        } else {
            clip.source_path.clone()
        };
        let source_ns = clip.source_in_ns;
        let uri = format!("file://{source_path}");
        // Go directly to Playing with no blocking preroll wait. GStreamer will buffer
        // asynchronously. The seek to source_in_ns is deferred: poll() will issue it
        // on the next tick, by which point the pipeline is usually ready to accept it.
        let _ = self.pipeline2.set_state(gst::State::Ready);
        self.pipeline2.set_property("uri", &uri);
        let _ = self.pipeline2.set_state(gst::State::Playing);
        self.pipeline2_pending_seek_ns = Some(source_ns);
        self.transition_active = true;
        self.transition_incoming_idx = Some(incoming_idx);
    }

    /// Deactivate the cross-dissolve transition preview: stop pipeline2.
    fn deactivate_transition(&mut self) {
        if self.transition_active {
            let _ = self.pipeline2.set_state(gst::State::Ready);
            self.transition_active = false;
            self.transition_incoming_idx = None;
        }
        self.pipeline2_pending_seek_ns = None;
    }

    fn should_block_preroll(&self) -> bool {
        !matches!(self.playback_priority, PlaybackPriority::Smooth)
    }

    fn clip_seek_flags(&self) -> gst::SeekFlags {
        match self.playback_priority {
            PlaybackPriority::Accurate => gst::SeekFlags::FLUSH | gst::SeekFlags::ACCURATE,
            PlaybackPriority::Balanced | PlaybackPriority::Smooth => gst::SeekFlags::FLUSH | gst::SeekFlags::KEY_UNIT,
        }
    }

    /// Seek flags used when the player is paused/stopped (scrubbing).
    /// Always ACCURATE so the displayed frame matches the exact playhead position,
    /// regardless of keyframe boundaries. KEY_UNIT is only appropriate during
    /// active playback where smoothness beats frame precision.
    fn paused_seek_flags() -> gst::SeekFlags {
        gst::SeekFlags::FLUSH | gst::SeekFlags::ACCURATE
    }

    /// Update the clip list from the project model. Resets playback.
    pub fn load_clips(&mut self, mut clips: Vec<ProgramClip>) {
        clips.sort_by_key(|c| c.timeline_start_ns);
        // Separate audio-only clips from video clips
        let (audio, video): (Vec<_>, Vec<_>) = clips.into_iter().partition(|c| c.is_audio_only);
        self.audio_clips = audio;
        self.clips = video;
        // Timeline duration is the max across both sets
        let vdur = self.clips.iter().map(|c| c.timeline_end_ns()).max().unwrap_or(0);
        let adur = self.audio_clips.iter().map(|c| c.timeline_end_ns()).max().unwrap_or(0);
        self.timeline_dur_ns = vdur.max(adur);
        self.current_idx = None;
        self.audio_current_idx = None;
        self.seek_anchor_timeline_ns = 0;
        self.seek_anchor_source_ns = 0;
        self.seek_reports_relative = None;
        let _ = self.pipeline.set_state(gst::State::Ready);
        let _ = self.audio_pipeline.set_state(gst::State::Null);
        self.audio_pipeline.set_property("uri", "");
        let _ = self.audio_pipeline.set_state(gst::State::Ready);
        self.deactivate_transition();
        self.state = PlayerState::Stopped;
        self.timeline_pos_ns = 0;
    }

    /// Seek to a timeline position in nanoseconds.
    pub fn seek(&mut self, timeline_pos_ns: u64) {
        self.timeline_pos_ns = timeline_pos_ns;
        if let Some(idx) = self.clip_at(timeline_pos_ns) {
            self.load_clip_idx(idx, timeline_pos_ns);
        } else {
            let _ = self.pipeline.set_state(gst::State::Ready);
            self.current_idx = None;
            self.seek_anchor_timeline_ns = timeline_pos_ns;
            self.seek_anchor_source_ns = 0;
            self.seek_reports_relative = None;
        }
        // Sync audio pipeline
        self.sync_audio_to(timeline_pos_ns);
    }

    pub fn play(&mut self) {
        let pos = self.timeline_pos_ns;
        if self.current_idx.is_none() {
            if let Some(idx) = self.clip_at(pos) {
                self.load_clip_idx(idx, pos);
            }
        }
        // Block briefly until the pipeline reaches PAUSED so our seek is accepted.
        if self.should_block_preroll() {
            let _ = self.pipeline.state(gst::ClockTime::from_mseconds(100));
        }
        // Re-seek to make sure we start at the right position with the correct rate.
        if let Some(idx) = self.current_idx {
            let clip = &self.clips[idx];
            let source_seek_ns = clip.source_pos_ns(pos);
            let speed = clip.speed;
            let _ = self.pipeline.seek(
                speed,
                self.clip_seek_flags(),
                gst::SeekType::Set,
                gst::ClockTime::from_nseconds(source_seek_ns),
                gst::SeekType::None,
                gst::ClockTime::NONE,
            );
            self.seek_anchor_timeline_ns = pos;
            self.seek_anchor_source_ns = source_seek_ns;
            self.seek_reports_relative = None;
        }
        let _ = self.pipeline.set_state(gst::State::Playing);
        // Also start the audio pipeline
        self.sync_audio_to(pos);
        if self.should_block_preroll() {
            let _ = self.audio_pipeline.state(gst::ClockTime::from_mseconds(100));
        }
        if let Some(aidx) = self.audio_current_idx {
            let aclip = &self.audio_clips[aidx];
            let asrc = aclip.source_pos_ns(pos);
            let _ = self.audio_pipeline.seek(
                1.0_f64,
                gst::SeekFlags::FLUSH | gst::SeekFlags::KEY_UNIT,
                gst::SeekType::Set,
                gst::ClockTime::from_nseconds(asrc),
                gst::SeekType::None,
                gst::ClockTime::NONE,
            );
            let _ = self.audio_pipeline.set_state(gst::State::Playing);
        }
        self.state = PlayerState::Playing;
        // Normal play always resets JKL shuttle speed.
        self.jkl_rate = 0.0;
    }

    pub fn pause(&mut self) {
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

    /// Stop playback and return to position 0.
    pub fn stop(&mut self) {
        let _ = self.pipeline.set_state(gst::State::Paused);
        let _ = self.audio_pipeline.set_state(gst::State::Paused);
        self.deactivate_transition();
        self.state = PlayerState::Paused;
        self.timeline_pos_ns = 0;
        if let Some(idx) = self.clip_at(0) {
            self.load_clip_idx(idx, 0);
        } else {
            let _ = self.pipeline.set_state(gst::State::Ready);
            self.current_idx = None;
            self.seek_anchor_timeline_ns = 0;
            self.seek_anchor_source_ns = 0;
            self.seek_reports_relative = None;
        }
        self.sync_audio_to(0);
        self.state = PlayerState::Stopped;
    }

    pub fn state(&self) -> &PlayerState {
        &self.state
    }

    /// Return the transform of the clip currently loaded in the pipeline, if any.
    pub fn current_clip_transform(&self) -> Option<crate::ui::program_monitor::ClipTransform> {
        let c = self.clips.get(self.current_idx?)?;
        Some(crate::ui::program_monitor::ClipTransform {
            crop_left:   c.crop_left,
            crop_right:  c.crop_right,
            crop_top:    c.crop_top,
            crop_bottom: c.crop_bottom,
            rotate:      c.rotate,
            flip_h:      c.flip_h,
            flip_v:      c.flip_v,
        })
    }

    pub fn current_clip_idx(&self) -> Option<usize> {
        self.current_idx
    }

    pub fn is_playing(&self) -> bool {
        self.state == PlayerState::Playing
    }

    /// Poll pipeline position, detect clip boundary, advance to next clip.
    /// Returns `true` if the timeline position changed (caller should redraw).
    pub fn poll(&mut self) -> bool {
        // Always drain the bus for level messages so the VU meter updates on seeks
        // and paused state changes (the level element fires one message per preroll).
        let eos = self.poll_bus();
        // Apply decay only during active playback so the meter holds its value
        // when paused (showing the most recent preroll level reading).
        if self.state == PlayerState::Playing {
            self.audio_peak_db[0] = (self.audio_peak_db[0] - 3.0).max(-60.0);
            self.audio_peak_db[1] = (self.audio_peak_db[1] - 3.0).max(-60.0);
        }

        if self.state != PlayerState::Playing {
            return false;
        }
        if self.timeline_dur_ns > 0 && self.timeline_pos_ns >= self.timeline_dur_ns {
            let _ = self.pipeline.set_state(gst::State::Ready);
            let _ = self.audio_pipeline.set_state(gst::State::Ready);
            self.state = PlayerState::Stopped;
            self.current_idx = None;
            self.audio_current_idx = None;
            self.timeline_pos_ns = self.timeline_dur_ns;
            return true;
        }

        let Some(idx) = self.current_idx else {
            // No video clip is currently active; keep tracking audio-only tails and
            // stop cleanly at timeline end.
            if let Some(aidx) = self.audio_current_idx {
                let (start_ns, in_ns, speed) = {
                    let aclip = &self.audio_clips[aidx];
                    (aclip.timeline_start_ns, aclip.source_in_ns, aclip.speed)
                };
                let asrc_pos = self.audio_pipeline
                    .query_position::<gst::ClockTime>()
                    .map(|t| t.nseconds())
                    .unwrap_or(0);
                let aoff = asrc_pos.saturating_sub(in_ns);
                let apos = start_ns + if speed > 0.0 { (aoff as f64 / speed) as u64 } else { aoff };
                if apos > self.timeline_pos_ns {
                    self.timeline_pos_ns = apos;
                }
                self.poll_audio(self.timeline_pos_ns);
                if self.timeline_dur_ns > 0 && self.timeline_pos_ns >= self.timeline_dur_ns {
                    let _ = self.pipeline.set_state(gst::State::Ready);
                    let _ = self.audio_pipeline.set_state(gst::State::Ready);
                    self.state = PlayerState::Stopped;
                    self.current_idx = None;
                    self.audio_current_idx = None;
                    self.timeline_pos_ns = self.timeline_dur_ns;
                    return true;
                }
            }
            return false;
        };
        // Extract clip data before any mutable operations.
        let (cur_track_idx, clip_speed, clip_source_out_ns, clip_timeline_end_ns,
             clip_transition_kind, clip_transition_ns) = {
            let clip = &self.clips[idx];
            (clip.track_index, clip.speed, clip.source_out_ns, clip.timeline_end_ns(),
             clip.transition_after.clone(), clip.transition_after_ns)
        };

        // Issue the deferred pipeline2 seek (set during activate_transition to avoid
        // blocking on the UI thread). By the time the next poll() tick runs, the
        // pipeline has usually advanced far enough to accept the seek.
        if let Some(seek_ns) = self.pipeline2_pending_seek_ns.take() {
            let _ = self.pipeline2.seek_simple(
                gst::SeekFlags::FLUSH | gst::SeekFlags::KEY_UNIT,
                gst::ClockTime::from_nseconds(seek_ns),
            );
        }

        // Cross-dissolve transition management: activate pipeline2 when entering the
        // last `transition_after_ns` ns of an outgoing clip, deactivate on exit.
        if clip_transition_kind == "cross_dissolve" && clip_transition_ns > 0 {
            let d = clip_transition_ns.min(clip_timeline_end_ns.saturating_sub(
                self.clips[idx].timeline_start_ns)); // same as duration_ns() approx
            let window_start = clip_timeline_end_ns.saturating_sub(d);
            if self.timeline_pos_ns >= window_start {
                // Find the adjacent incoming clip on the same track (tolerance for
                // 1-frame FCPXML rounding gaps).
                const GAP_NS: u64 = 100_000_000;
                let incoming_idx = self.clips.iter().position(|c|
                    c.track_index == cur_track_idx
                        && c.timeline_start_ns >= clip_timeline_end_ns
                        && c.timeline_start_ns <= clip_timeline_end_ns + GAP_NS
                );
                if let Some(bidx) = incoming_idx {
                    self.activate_transition(bidx);
                }
            } else {
                self.deactivate_transition();
            }
        } else {
            self.deactivate_transition();
        }

        let src_pos = self.pipeline
            .query_position::<gst::ClockTime>()
            .map(|t| t.nseconds())
            .unwrap_or(0);

        // Update timeline_pos from source position, accounting for speed.
        // Determine once per seek whether query_position is segment-relative
        // or absolute, and keep that mode stable for the segment.
        if self.seek_reports_relative.is_none() {
            if self.seek_anchor_source_ns == 0 {
                self.seek_reports_relative = Some(false);
            } else if src_pos.saturating_add(200_000_000) < self.seek_anchor_source_ns {
                self.seek_reports_relative = Some(true);
            } else if src_pos >= self.seek_anchor_source_ns.saturating_sub(200_000_000) {
                self.seek_reports_relative = Some(false);
            }
        }
        let delta_src_ns = if self.seek_reports_relative.unwrap_or(false) {
            src_pos
        } else {
            src_pos.saturating_sub(self.seek_anchor_source_ns)
        };
        let timeline_offset = if clip_speed > 0.0 {
            (delta_src_ns as f64 / clip_speed) as u64
        } else {
            delta_src_ns
        };
        let mut new_pos = self.seek_anchor_timeline_ns + timeline_offset;
        // Prevent tiny decoder/keyframe backsteps from rewinding timeline state.
        if new_pos < self.timeline_pos_ns {
            new_pos = self.timeline_pos_ns;
        }

        // Detect clip end (GStreamer position may slightly overshoot source_out).
        // Only trust EOS when we're already near the end to avoid stale EOS
        // messages from a previous clip forcing a false stop/restart.
        let near_end = src_pos.saturating_add(50_000_000) >= clip_source_out_ns; // 50ms
        let near_timeline_end = new_pos.saturating_add(50_000_000) >= clip_timeline_end_ns;
        let at_end_pos = clip_timeline_end_ns;
        let has_next_at_boundary = self.clip_at(at_end_pos).map(|n| n != idx).unwrap_or(false);
        // Pre-emptive handoff shortly before boundary when a next clip exists.
        let early_handoff = has_next_at_boundary && src_pos.saturating_add(80_000_000) >= clip_source_out_ns; // 80ms
        if src_pos >= clip_source_out_ns || ((near_end || near_timeline_end) && eos) || early_handoff {
            // Transition is complete when the outgoing clip ends.
            self.deactivate_transition();
            // Find what should play at the current timeline position using track-priority logic.
            // This handles B-roll ending and resuming the primary clip underneath.
            match self.clip_at(at_end_pos) {
                Some(next_idx) if next_idx != idx => {
                    self.load_clip_idx(next_idx, at_end_pos);
                    let _ = self.pipeline.set_state(gst::State::Playing);
                }
                Some(_) => {
                    // Same clip selected — try advancing past it to avoid infinite loop
                    let past = at_end_pos + 1;
                    if let Some(next_idx) = self.clip_at(past) {
                        self.load_clip_idx(next_idx, past);
                        let _ = self.pipeline.set_state(gst::State::Playing);
                    } else {
                        let _ = self.pipeline.set_state(gst::State::Ready);
                        let _ = self.audio_pipeline.set_state(gst::State::Ready);
                        self.state = PlayerState::Stopped;
                        self.current_idx = None;
                        self.audio_current_idx = None;
                    }
                }
                None => {
                    // No clip active at this timeline position — end of timeline
                    let _ = self.pipeline.set_state(gst::State::Ready);
                    let _ = self.audio_pipeline.set_state(gst::State::Ready);
                    self.state = PlayerState::Stopped;
                    self.current_idx = None;
                    self.audio_current_idx = None;
                }
            }
            return true;
        }

        let changed = new_pos != self.timeline_pos_ns;
        self.timeline_pos_ns = new_pos;
        if self.timeline_dur_ns > 0 && self.timeline_pos_ns >= self.timeline_dur_ns {
            let _ = self.pipeline.set_state(gst::State::Ready);
            let _ = self.audio_pipeline.set_state(gst::State::Ready);
            self.state = PlayerState::Stopped;
            self.current_idx = None;
            self.audio_current_idx = None;
            self.timeline_pos_ns = self.timeline_dur_ns;
            return true;
        }

        // Advance audio pipeline if needed (audio clip boundary)
        self.poll_audio(new_pos);

        // If the desired clip changed (e.g. B-roll became active mid-playback), switch to it
        if let Some(wanted) = self.clip_at(new_pos) {
            if wanted != idx {
                // Only switch here when moving to a higher-priority track.
                // Lower-priority fallback (e.g. B-roll ending) is handled by end-of-clip logic.
                let wanted_track_idx = self.clips[wanted].track_index;
                if wanted_track_idx > cur_track_idx {
                    self.load_clip_idx(wanted, new_pos);
                    let _ = self.pipeline.set_state(gst::State::Playing);
                }
            }
        }

        changed
    }

    /// Directly update effects on the current clip without reloading the pipeline.
    /// Sets videobalance and gaussianblur properties then force-seeks so the PAUSED
    /// frame is redrawn with the new values.
    pub fn update_current_color(&mut self, brightness: f64, contrast: f64, saturation: f64) {
        self.update_current_effects(brightness, contrast, saturation, 0.0, 0.0);
    }

    /// Same as update_current_color but also applies denoise and sharpness.
    pub fn update_current_effects(&mut self, brightness: f64, contrast: f64, saturation: f64, denoise: f64, sharpness: f64) {
        if let Some(ref vb) = self.videobalance {
            vb.set_property("brightness", brightness.clamp(-1.0, 1.0));
            vb.set_property("contrast",   contrast.clamp(0.0, 2.0));
            vb.set_property("saturation", saturation.clamp(0.0, 2.0));
        }
        if let Some(ref gb) = self.gaussianblur {
            let sigma = (denoise * 4.0 - sharpness * 6.0).clamp(-20.0, 20.0);
            gb.set_property("sigma", sigma);
        }
        // In PAUSED state, force frame redecode using tracked timeline_pos_ns.
        if self.current_idx.is_some() && self.state != PlayerState::Playing {
            let pos = self.timeline_pos_ns;
            if let Some(idx) = self.clip_at(pos) {
                let clip = &self.clips[idx];
                let source_ns = clip.source_pos_ns(pos);
                let speed = clip.speed;
                let _ = self.pipeline.seek(speed,
                    Self::paused_seek_flags(),
                    gst::SeekType::Set, gst::ClockTime::from_nseconds(source_ns),
                    gst::SeekType::None, gst::ClockTime::NONE);
            }
        }
    }
    pub fn update_current_audio(&mut self, volume: f64, pan: f64) {
        self.pipeline.set_property("volume", volume.clamp(0.0, 2.0));
        if let Some(ref ap) = self.audiopanorama {
            ap.set_property("panorama", (pan as f32).clamp(-1.0, 1.0));
        }
        // When paused, force a re-seek to trigger a new preroll buffer through
        // the level element so the VU meter reflects the updated volume.
        if self.current_idx.is_some() && self.state != PlayerState::Playing {
            let pos = self.timeline_pos_ns;
            if let Some(idx) = self.clip_at(pos) {
                let clip = &self.clips[idx];
                let source_ns = clip.source_pos_ns(pos);
                let speed = clip.speed;
                let _ = self.pipeline.seek(speed,
                    Self::paused_seek_flags(),
                    gst::SeekType::Set, gst::ClockTime::from_nseconds(source_ns),
                    gst::SeekType::None, gst::ClockTime::NONE);
            }
        }
    }

    /// Apply crop, rotation, and flip transform to the video filter elements.
    pub fn set_transform(&self, crop_left: i32, crop_right: i32, crop_top: i32, crop_bottom: i32, rotate: i32, flip_h: bool, flip_v: bool) {
        if let Some(ref vc) = self.videocrop {
            vc.set_property("left", crop_left.max(0));
            vc.set_property("right", crop_right.max(0));
            vc.set_property("top", crop_top.max(0));
            vc.set_property("bottom", crop_bottom.max(0));
        }
        if let Some(ref vfr) = self.videoflip_rotate {
            let method = match rotate {
                90  => "clockwise",
                180 => "rotate-180",
                270 => "counterclockwise",
                _   => "none",
            };
            vfr.set_property_from_str("method", method);
        }
        if let Some(ref vff) = self.videoflip_flip {
            let method = match (flip_h, flip_v) {
                (true, true)   => "rotate-180",
                (true, false)  => "horizontal-flip",
                (false, true)  => "vertical-flip",
                (false, false) => "none",
            };
            vff.set_property_from_str("method", method);
        }
    }

    /// Sets textoverlay properties for the per-clip title.
    pub fn set_title(&self, text: &str, font: &str, color_rgba: u32, rel_x: f64, rel_y: f64) {
        if let Some(ref to) = self.textoverlay {
            let silent = text.is_empty();
            to.set_property("silent", silent);
            if !silent {
                to.set_property("text", text);
                to.set_property("font-desc", font);
                // textoverlay uses xpos/ypos as relative (0.0–1.0) when halignment/valignment = "position"
                to.set_property_from_str("halignment", "position");
                to.set_property_from_str("valignment", "position");
                to.set_property("xpos", rel_x);
                to.set_property("ypos", rel_y);
                // Convert color from 0xRRGGBBAA to argb u32
                let r = (color_rgba >> 24) & 0xFF;
                let g = (color_rgba >> 16) & 0xFF;
                let b = (color_rgba >> 8)  & 0xFF;
                let a = color_rgba & 0xFF;
                let argb: u32 = (a << 24) | (r << 16) | (g << 8) | b;
                to.set_property("color", argb);
            }
        }
    }

    /// Directly update title on the current clip without reloading the pipeline.
    pub fn update_current_title(&mut self, text: &str, font: &str, color_rgba: u32, rel_x: f64, rel_y: f64) {
        self.set_title(text, font, color_rgba, rel_x, rel_y);
        if self.current_idx.is_some() && self.state != PlayerState::Playing {
            let pos = self.timeline_pos_ns;
            if let Some(idx) = self.clip_at(pos) {
                let clip = &self.clips[idx];
                let source_ns = clip.source_pos_ns(pos);
                let speed = clip.speed;
                let _ = self.pipeline.seek(speed,
                    Self::paused_seek_flags(),
                    gst::SeekType::Set, gst::ClockTime::from_nseconds(source_ns),
                    gst::SeekType::None, gst::ClockTime::NONE);
            }
        }
    }

    /// Directly update transform on the current clip without reloading the pipeline.
    pub fn update_current_transform(&mut self, crop_left: i32, crop_right: i32, crop_top: i32, crop_bottom: i32, rotate: i32, flip_h: bool, flip_v: bool) {
        self.set_transform(crop_left, crop_right, crop_top, crop_bottom, rotate, flip_h, flip_v);
        if self.current_idx.is_some() && self.state != PlayerState::Playing {
            let pos = self.timeline_pos_ns;
            if let Some(idx) = self.clip_at(pos) {
                let clip = &self.clips[idx];
                let source_ns = clip.source_pos_ns(pos);
                let speed = clip.speed;
                let _ = self.pipeline.seek(speed,
                    Self::paused_seek_flags(),
                    gst::SeekType::Set, gst::ClockTime::from_nseconds(source_ns),
                    gst::SeekType::None, gst::ClockTime::NONE);
            }
        }
    }

    // ── Private helpers ────────────────────────────────────────────────────

    fn clip_at(&self, timeline_pos_ns: u64) -> Option<usize> {
        // Return the highest-track-index clip active at this position (B-roll beats primary).
        let exact = self.clips.iter().enumerate()
            .filter(|(_, c)| timeline_pos_ns >= c.timeline_start_ns && timeline_pos_ns < c.timeline_end_ns())
            .max_by_key(|(_, c)| c.track_index)
            .map(|(i, _)| i);
        if exact.is_some() { return exact; }
        // Fallback: bridge small rounding gaps that arise from integer frame-count
        // conversion in the FCPXML round-trip (e.g. 1 frame ≈ 41 ms at 24 fps).
        // If no clip contains the exact position, find the earliest clip that starts
        // within 100 ms ahead, then among those ties pick the highest track index.
        const GAP_NS: u64 = 100_000_000; // 100 ms
        let next_start = self.clips.iter()
            .filter(|c| c.timeline_start_ns > timeline_pos_ns
                     && c.timeline_start_ns <= timeline_pos_ns + GAP_NS)
            .map(|c| c.timeline_start_ns)
            .min()?;
        self.clips.iter().enumerate()
            .filter(|(_, c)| c.timeline_start_ns == next_start)
            .max_by_key(|(_, c)| c.track_index)
            .map(|(i, _)| i)
    }

    fn load_clip_idx(&mut self, idx: usize, timeline_pos_ns: u64) {
        let clip = &self.clips[idx];
        let source_seek_ns = clip.source_pos_ns(timeline_pos_ns);
        let speed = clip.speed;

        // Apply per-clip color correction (always, before seek so new frame uses it)
        if let Some(ref vb) = self.videobalance {
            vb.set_property("brightness", clip.brightness.clamp(-1.0, 1.0));
            vb.set_property("contrast",   clip.contrast.clamp(0.0, 2.0));
            vb.set_property("saturation", clip.saturation.clamp(0.0, 2.0));
        }
        if let Some(ref gb) = self.gaussianblur {
            let sigma = (clip.denoise * 4.0 - clip.sharpness * 6.0).clamp(-20.0, 20.0);
            gb.set_property("sigma", sigma);
        }
        // Apply per-clip audio volume and pan
        self.pipeline.set_property("volume", clip.volume.clamp(0.0, 2.0));
        if let Some(ref ap) = self.audiopanorama {
            ap.set_property("panorama", (clip.pan as f32).clamp(-1.0, 1.0));
        }
        // Apply per-clip transform (crop, rotate, flip)
        self.set_transform(clip.crop_left, clip.crop_right, clip.crop_top, clip.crop_bottom,
                           clip.rotate, clip.flip_h, clip.flip_v);
        // Apply per-clip title overlay
        self.set_title(&clip.title_text, &clip.title_font, clip.title_color, clip.title_x, clip.title_y);

        let same_source_loaded = self.current_idx
            .and_then(|cur| self.clips.get(cur))
            .map(|c| c.source_path == clip.source_path)
            .unwrap_or(false);

        if self.current_idx == Some(idx) || same_source_loaded {
            // Reuse currently loaded source when possible; just seek.
            // This avoids a full playbin reset when moving between segments
            // from the same media file.
            let seek_flags = if self.state == PlayerState::Playing {
                self.clip_seek_flags()
            } else {
                Self::paused_seek_flags()
            };
            let _ = self.pipeline.seek(
                speed,
                seek_flags,
                gst::SeekType::Set,
                gst::ClockTime::from_nseconds(source_seek_ns),
                gst::SeekType::None,
                gst::ClockTime::NONE,
            );
        } else {
            // Different clip — flush stale bus messages (e.g. EOS from previous clip)
            // then reload the pipeline.
            if let Some(bus) = self.pipeline.bus() {
                while bus.pop().is_some() {}
            }
            // Use proxy file when proxy mode is enabled and a proxy exists.
            // The proxy map is keyed by (source_path, lut_path) composite key so
            // each LUT-assigned clip gets its own baked proxy.
            let effective_path = if self.proxy_enabled {
                let key = crate::media::proxy_cache::proxy_key(
                    &clip.source_path,
                    clip.lut_path.as_deref(),
                );
                self.proxy_paths.get(&key)
                    .map(|p| p.as_str())
                    .unwrap_or(&clip.source_path)
            } else {
                &clip.source_path
            };
            let uri = format!("file://{}", effective_path);
            if self.state == PlayerState::Playing {
                // During active playback, go through Ready quickly to change
                // the URI (playbin requires Ready/Null for URI changes) but
                // transition directly to Playing to minimise the visible gap.
                let _ = self.pipeline.set_state(gst::State::Ready);
                self.pipeline.set_property("uri", &uri);
                let _ = self.pipeline.set_state(gst::State::Playing);
            } else {
                let _ = self.pipeline.set_state(gst::State::Ready);
                self.pipeline.set_property("uri", &uri);
                let _ = self.pipeline.set_state(gst::State::Paused);
                // Always wait for preroll when not playing so the seek is accepted.
                // GStreamer requires PAUSED state before it can process a seek; without
                // this wait (previously gated on should_block_preroll), Smooth mode
                // would issue the seek before PAUSED was reached and the seek would be
                // silently ignored, leaving the display at frame 0 of the new clip.
                let _ = self.pipeline.state(gst::ClockTime::from_mseconds(150));
            }
            // When not playing, use ACCURATE so the exact playhead frame is decoded.
            // During playback, use the priority-based flags for smooth transitions.
            let seek_flags = if self.state == PlayerState::Playing {
                self.clip_seek_flags()
            } else {
                Self::paused_seek_flags()
            };
            let _ = self.pipeline.seek(
                speed,
                seek_flags,
                gst::SeekType::Set,
                gst::ClockTime::from_nseconds(source_seek_ns),
                gst::SeekType::None,
                gst::ClockTime::NONE,
            );
        }

        self.current_idx = Some(idx);
        self.seek_anchor_timeline_ns = timeline_pos_ns;
        self.seek_anchor_source_ns = source_seek_ns;
        self.seek_reports_relative = None;
        self.timeline_pos_ns = timeline_pos_ns;
        // Cross-dissolve opacity is now handled by the dual-pipeline GTK overlay
        // (picture_a / picture_b set_opacity). The GStreamer alpha filter must stay
        // at 1.0 so pipeline1 is always opaque — the GTK layer blends both pictures.
        if let Some(ref a) = self.alpha_filter {
            a.set_property("alpha", 1.0_f64);
        }
    }

    /// Transition alpha approximation for preview:
    /// fade-out at end of outgoing clip + fade-in at start of incoming clip.
    fn transition_alpha(&self, idx: usize, timeline_pos_ns: u64) -> f64 {
        let Some(clip) = self.clips.get(idx) else { return 1.0 };
        let mut alpha = 1.0_f64;
        // Outgoing transition on this clip
        if clip.transition_after == "cross_dissolve" && clip.transition_after_ns > 0 {
            let d = clip.transition_after_ns.min(clip.duration_ns());
            let start = clip.timeline_end_ns().saturating_sub(d);
            if timeline_pos_ns >= start && timeline_pos_ns < clip.timeline_end_ns() && d > 0 {
                let t = (timeline_pos_ns - start) as f64 / d as f64;
                alpha = alpha.min((1.0 - t).clamp(0.0, 1.0));
            }
        }
        // Incoming transition from previous clip on same track
        if let Some(prev) = self.clips.iter().find(|c|
            c.track_index == clip.track_index
                && c.timeline_end_ns() == clip.timeline_start_ns
                && c.transition_after == "cross_dissolve"
                && c.transition_after_ns > 0
        ) {
            let d = prev.transition_after_ns.min(clip.duration_ns());
            if timeline_pos_ns >= clip.timeline_start_ns
                && timeline_pos_ns < clip.timeline_start_ns.saturating_add(d)
                && d > 0
            {
                let t = (timeline_pos_ns - clip.timeline_start_ns) as f64 / d as f64;
                alpha = alpha.min(t.clamp(0.0, 1.0));
            }
        }
        alpha
    }

    /// Drain both pipeline buses, returning `true` if an EOS was found on the main
    /// pipeline. Also extracts `level` element peak messages and updates `audio_peak_db`.
    fn poll_bus(&mut self) -> bool {
        let mut eos = false;
        // Main pipeline bus.
        if let Some(bus) = self.pipeline.bus() {
            while let Some(msg) = bus.pop() {
                match msg.view() {
                    gstreamer::MessageView::Eos(_) => eos = true,
                    gstreamer::MessageView::Element(e) => {
                        if let Some(s) = e.structure() {
                            if s.name() == "level" {
                                if let Ok(peak) = s.get::<glib::ValueArray>("peak") {
                                    let vals = peak.as_slice();
                                    let l = vals.first().and_then(|v| v.get::<f64>().ok()).unwrap_or(-60.0);
                                    let r = vals.get(1).and_then(|v| v.get::<f64>().ok()).unwrap_or(l);
                                    // Take max so the peak reading wins over decay within a single poll.
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
        // Audio-only pipeline bus (metering only; no EOS propagation needed here).
        if let Some(abus) = self.audio_pipeline.bus() {
            while let Some(msg) = abus.pop() {
                if let gstreamer::MessageView::Element(e) = msg.view() {
                    if let Some(s) = e.structure() {
                        if s.name() == "level" {
                            if let Ok(peak) = s.get::<glib::ValueArray>("peak") {
                                let vals = peak.as_slice();
                                let l = vals.first().and_then(|v| v.get::<f64>().ok()).unwrap_or(-60.0);
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

    fn audio_clip_at(&self, timeline_pos_ns: u64) -> Option<usize> {
        self.audio_clips.iter().position(|c| {
            timeline_pos_ns >= c.timeline_start_ns && timeline_pos_ns < c.timeline_end_ns()
        })
    }

    /// Load an audio clip into the audio pipeline and seek to the right source position.
    fn load_audio_clip_idx(&mut self, idx: usize, timeline_pos_ns: u64) {
        let clip = &self.audio_clips[idx];
        let source_seek_ns = clip.source_pos_ns(timeline_pos_ns);
        self.audio_pipeline.set_property("volume", clip.volume.clamp(0.0, 2.0));

        if self.audio_current_idx == Some(idx) {
            let _ = self.audio_pipeline.seek(
                1.0_f64,
                gst::SeekFlags::FLUSH | gst::SeekFlags::KEY_UNIT,
                gst::SeekType::Set,
                gst::ClockTime::from_nseconds(source_seek_ns),
                gst::SeekType::None,
                gst::ClockTime::NONE,
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
                gst::SeekType::None,
                gst::ClockTime::NONE,
            );
        }
        self.audio_current_idx = Some(idx);
    }

    /// Seek the audio pipeline to match the given timeline position.
    fn sync_audio_to(&mut self, timeline_pos_ns: u64) {
        if let Some(idx) = self.audio_clip_at(timeline_pos_ns) {
            self.load_audio_clip_idx(idx, timeline_pos_ns);
        } else {
            // No audio clip at this position — stop audio pipeline
            let _ = self.audio_pipeline.set_state(gst::State::Ready);
            self.audio_current_idx = None;
        }
    }

    /// Called each poll tick to advance the audio pipeline across clip boundaries.
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
        let _ = self.pipeline.set_state(gst::State::Null);
        let _ = self.audio_pipeline.set_state(gst::State::Null);
    }
}
