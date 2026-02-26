/// A "program monitor" player that plays the assembled timeline clip-by-clip.
///
/// It uses a standard `playbin` pipeline and advances through clips in
/// timeline order. Seeking moves to a timeline position and loads the
/// appropriate clip at the right source offset.
use anyhow::{anyhow, Result};
use gstreamer as gst;
use gstreamer::prelude::*;
use crate::media::player::PlayerState;
use crate::ui_state::PlaybackPriority;
use std::collections::HashMap;

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
}

impl ProgramPlayer {
    pub fn new() -> Result<(Self, gdk4::Paintable)> {
        let paintablesink = gst::ElementFactory::make("gtk4paintablesink")
            .build()
            .map_err(|_| anyhow!("gtk4paintablesink not available"))?;

        let paintable = {
            let obj = paintablesink.property::<glib::Object>("paintable");
            obj.dynamic_cast::<gdk4::Paintable>()
                .expect("gtk4paintablesink paintable must implement Paintable")
        };

        let video_sink = match gst::ElementFactory::make("glsinkbin")
            .property("sink", &paintablesink)
            .build()
        {
            Ok(s) => s,
            Err(_) => paintablesink.clone(),
        };

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

        if let Some(ref ap) = audiopanorama {
            pipeline.set_property("audio-filter", ap);
        }

        // Dedicated audio-only pipeline: video routed to fakesink so it plays
        // audio-track clips without interfering with the visual display.
        let fakevideo = gst::ElementFactory::make("fakesink").build()
            .unwrap_or_else(|_| gst::ElementFactory::make("autovideosink").build().unwrap());
        let audio_pipeline = gst::ElementFactory::make("playbin")
            .property("video-sink", &fakevideo)
            .build()
            .unwrap_or_else(|_| gst::ElementFactory::make("playbin").build().unwrap());

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
            },
            paintable,
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

    fn should_block_preroll(&self) -> bool {
        !matches!(self.playback_priority, PlaybackPriority::Smooth)
    }

    fn clip_seek_flags(&self) -> gst::SeekFlags {
        match self.playback_priority {
            PlaybackPriority::Accurate => gst::SeekFlags::FLUSH | gst::SeekFlags::ACCURATE,
            PlaybackPriority::Balanced | PlaybackPriority::Smooth => gst::SeekFlags::FLUSH | gst::SeekFlags::KEY_UNIT,
        }
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
        let _ = self.audio_pipeline.set_state(gst::State::Ready);
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
        }
        let _ = self.audio_pipeline.set_state(gst::State::Playing);
        self.state = PlayerState::Playing;
    }

    pub fn pause(&mut self) {
        let _ = self.pipeline.set_state(gst::State::Paused);
        let _ = self.audio_pipeline.set_state(gst::State::Paused);
        self.state = PlayerState::Paused;
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
        let clip = &self.clips[idx];
        let cur_track_idx = clip.track_index;
        if let Some(ref a) = self.alpha_filter {
            let alpha = self.transition_alpha(idx, self.timeline_pos_ns);
            a.set_property("alpha", alpha.clamp(0.0, 1.0));
        }

        let src_pos = self.pipeline
            .query_position::<gst::ClockTime>()
            .map(|t| t.nseconds())
            .unwrap_or(0);
        let eos = self.is_eos();

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
        let timeline_offset = if clip.speed > 0.0 {
            (delta_src_ns as f64 / clip.speed) as u64
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
        let near_end = src_pos.saturating_add(50_000_000) >= clip.source_out_ns; // 50ms
        let near_timeline_end = new_pos.saturating_add(50_000_000) >= clip.timeline_end_ns();
        let at_end_pos = clip.timeline_end_ns();
        let has_next_at_boundary = self.clip_at(at_end_pos).map(|n| n != idx).unwrap_or(false);
        // Pre-emptive handoff shortly before boundary when a next clip exists.
        let early_handoff = has_next_at_boundary && src_pos.saturating_add(80_000_000) >= clip.source_out_ns; // 80ms
        if src_pos >= clip.source_out_ns || ((near_end || near_timeline_end) && eos) || early_handoff {
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
        if let Some(ref a) = self.alpha_filter {
            let alpha = self.transition_alpha(idx, new_pos);
            a.set_property("alpha", alpha.clamp(0.0, 1.0));
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
                    gst::SeekFlags::FLUSH | gst::SeekFlags::KEY_UNIT,
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
                    gst::SeekFlags::FLUSH | gst::SeekFlags::KEY_UNIT,
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
                    gst::SeekFlags::FLUSH | gst::SeekFlags::KEY_UNIT,
                    gst::SeekType::Set, gst::ClockTime::from_nseconds(source_ns),
                    gst::SeekType::None, gst::ClockTime::NONE);
            }
        }
    }

    // ── Private helpers ────────────────────────────────────────────────────

    fn clip_at(&self, timeline_pos_ns: u64) -> Option<usize> {
        // Return the highest-track-index clip active at this position (B-roll beats primary).
        self.clips.iter().enumerate()
            .filter(|(_, c)| timeline_pos_ns >= c.timeline_start_ns && timeline_pos_ns < c.timeline_end_ns())
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
            let _ = self.pipeline.seek(
                speed,
                self.clip_seek_flags(),
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
            let effective_path = if self.proxy_enabled {
                self.proxy_paths.get(&clip.source_path)
                    .map(|p| p.as_str())
                    .unwrap_or(&clip.source_path)
            } else {
                &clip.source_path
            };
            let uri = format!("file://{}", effective_path);
            if self.state == PlayerState::Playing {
                // During active playback, avoid dropping to Ready state which
                // tears down the video sink and causes a visible black flash.
                // Instead pause briefly, swap URI, and seek — the sink stays
                // alive so the last frame remains on screen until the new
                // source delivers its first decoded frame.
                let _ = self.pipeline.set_state(gst::State::Paused);
                self.pipeline.set_property("uri", &uri);
            } else {
                let _ = self.pipeline.set_state(gst::State::Ready);
                self.pipeline.set_property("uri", &uri);
                let _ = self.pipeline.set_state(gst::State::Paused);
                if self.should_block_preroll() {
                    let _ = self.pipeline.state(gst::ClockTime::from_mseconds(120));
                }
            }
            // Seek with FLUSH and the clip's speed as rate.
            let _ = self.pipeline.seek(
                speed,
                self.clip_seek_flags(),
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
        if let Some(ref a) = self.alpha_filter {
            let alpha = self.transition_alpha(idx, timeline_pos_ns);
            a.set_property("alpha", alpha.clamp(0.0, 1.0));
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

    fn is_eos(&self) -> bool {
        let bus = match self.pipeline.bus() { Some(b) => b, None => return false };
        while let Some(msg) = bus.pop() {
            if let gstreamer::MessageView::Eos(_) = msg.view() { return true; }
        }
        false
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
