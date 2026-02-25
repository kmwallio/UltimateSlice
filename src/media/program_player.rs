/// A "program monitor" player that plays the assembled timeline clip-by-clip.
///
/// It uses a standard `playbin` pipeline and advances through clips in
/// timeline order. Seeking moves to a timeline position and loads the
/// appropriate clip at the right source offset.
use anyhow::{anyhow, Result};
use gstreamer as gst;
use gstreamer::prelude::*;
use crate::media::player::PlayerState;

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
}

impl ProgramClip {
    pub fn duration_ns(&self) -> u64 {
        self.source_out_ns.saturating_sub(self.source_in_ns)
    }
    pub fn timeline_end_ns(&self) -> u64 {
        self.timeline_start_ns + self.duration_ns()
    }
}

pub struct ProgramPlayer {
    pipeline: gst::Element,
    state: PlayerState,
    pub clips: Vec<ProgramClip>,
    current_idx: Option<usize>,
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

        if videobalance.is_some() && gaussianblur.is_some() {
            let vb = videobalance.as_ref().unwrap();
            let gb = gaussianblur.as_ref().unwrap();
            gb.set_property("sigma", 0.0_f64);
            let bin = gst::Bin::new();
            let conv = gst::ElementFactory::make("videoconvert").build()
                .expect("videoconvert must be available");
            bin.add_many([vb, &conv, gb]).ok();
            gst::Element::link_many([vb, &conv, gb]).ok();
            let sink_pad = vb.static_pad("sink").unwrap();
            let src_pad = gb.static_pad("src").unwrap();
            bin.add_pad(&gst::GhostPad::with_target(&sink_pad).unwrap()).ok();
            bin.add_pad(&gst::GhostPad::with_target(&src_pad).unwrap()).ok();
            pipeline.set_property("video-filter", &bin);
        } else if let Some(ref vb) = videobalance {
            pipeline.set_property("video-filter", vb);
        }

        if let Some(ref ap) = audiopanorama {
            pipeline.set_property("audio-filter", ap);
        }

        Ok((
            Self {
                pipeline,
                state: PlayerState::Stopped,
                clips: Vec::new(),
                current_idx: None,
                timeline_pos_ns: 0,
                timeline_dur_ns: 0,
                videobalance,
                gaussianblur,
                audiopanorama,
            },
            paintable,
        ))
    }

    /// Update the clip list from the project model. Resets playback.
    pub fn load_clips(&mut self, mut clips: Vec<ProgramClip>) {
        clips.sort_by_key(|c| c.timeline_start_ns);
        self.timeline_dur_ns = clips.iter().map(|c| c.timeline_end_ns()).max().unwrap_or(0);
        self.clips = clips;
        self.current_idx = None;
        let _ = self.pipeline.set_state(gst::State::Ready);
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
        }
    }

    pub fn play(&mut self) {
        if self.current_idx.is_none() {
            if let Some(idx) = self.clip_at(self.timeline_pos_ns) {
                self.load_clip_idx(idx, self.timeline_pos_ns);
            }
        }
        let _ = self.pipeline.set_state(gst::State::Playing);
        self.state = PlayerState::Playing;
    }

    pub fn pause(&mut self) {
        let _ = self.pipeline.set_state(gst::State::Paused);
        self.state = PlayerState::Paused;
    }

    pub fn toggle_play_pause(&mut self) {
        match self.state {
            PlayerState::Playing => self.pause(),
            _ => self.play(),
        }
    }

    pub fn state(&self) -> &PlayerState {
        &self.state
    }

    /// Poll pipeline position, detect clip boundary, advance to next clip.
    /// Returns `true` if the timeline position changed (caller should redraw).
    pub fn poll(&mut self) -> bool {
        if self.state != PlayerState::Playing {
            return false;
        }
        let Some(idx) = self.current_idx else { return false };
        let clip = &self.clips[idx];

        let src_pos = self.pipeline
            .query_position::<gst::ClockTime>()
            .map(|t| t.nseconds())
            .unwrap_or(0);

        // Detect clip end (GStreamer position may slightly overshoot source_out)
        if src_pos >= clip.source_out_ns || self.is_eos() {
            // Advance to next clip
            let next_idx = idx + 1;
            if next_idx < self.clips.len() {
                let next_start = self.clips[next_idx].timeline_start_ns;
                self.load_clip_idx(next_idx, next_start);
                let _ = self.pipeline.set_state(gst::State::Playing);
            } else {
                // End of timeline
                let _ = self.pipeline.set_state(gst::State::Ready);
                self.state = PlayerState::Stopped;
                self.current_idx = None;
            }
            return true;
        }

        // Update timeline_pos from source position
        let new_pos = clip.timeline_start_ns
            + src_pos.saturating_sub(clip.source_in_ns);
        let changed = new_pos != self.timeline_pos_ns;
        self.timeline_pos_ns = new_pos;
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
        // In PAUSED state, force frame redecode at the current source position.
        if self.current_idx.is_some() && self.state != PlayerState::Playing {
            let src_pos = self.pipeline
                .query_position::<gst::ClockTime>()
                .map(|t| t.nseconds())
                .unwrap_or(0);
            let _ = self.pipeline.seek_simple(
                gst::SeekFlags::FLUSH | gst::SeekFlags::KEY_UNIT,
                gst::ClockTime::from_nseconds(src_pos),
            );
        }
    }

    /// Directly update volume and pan on the current clip.
    pub fn update_current_audio(&mut self, volume: f64, pan: f64) {
        self.pipeline.set_property("volume", volume.clamp(0.0, 2.0));
        if let Some(ref ap) = self.audiopanorama {
            ap.set_property("panorama", (pan as f32).clamp(-1.0, 1.0));
        }
    }

    // ── Private helpers ────────────────────────────────────────────────────

    fn clip_at(&self, timeline_pos_ns: u64) -> Option<usize> {
        self.clips.iter().position(|c| {
            timeline_pos_ns >= c.timeline_start_ns && timeline_pos_ns < c.timeline_end_ns()
        })
    }

    fn load_clip_idx(&mut self, idx: usize, timeline_pos_ns: u64) {
        let clip = &self.clips[idx];
        let source_seek_ns = clip.source_in_ns
            + timeline_pos_ns.saturating_sub(clip.timeline_start_ns);

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

        if self.current_idx == Some(idx) {
            // Same clip already loaded — just seek to the new position. No pipeline reset.
            let _ = self.pipeline.seek_simple(
                gst::SeekFlags::FLUSH | gst::SeekFlags::KEY_UNIT,
                gst::ClockTime::from_nseconds(source_seek_ns),
            );
        } else {
            // Different clip — reload the pipeline.
            let uri = format!("file://{}", clip.source_path);
            let _ = self.pipeline.set_state(gst::State::Ready);
            self.pipeline.set_property("uri", &uri);
            let _ = self.pipeline.set_state(gst::State::Paused);
            // Seek with FLUSH — valid during async PAUSED pre-roll for local files.
            let _ = self.pipeline.seek_simple(
                gst::SeekFlags::FLUSH | gst::SeekFlags::KEY_UNIT,
                gst::ClockTime::from_nseconds(source_seek_ns),
            );
        }

        self.current_idx = Some(idx);
        self.timeline_pos_ns = timeline_pos_ns;
    }

    fn is_eos(&self) -> bool {
        let bus = match self.pipeline.bus() { Some(b) => b, None => return false };
        while let Some(msg) = bus.pop() {
            if let gstreamer::MessageView::Eos(_) = msg.view() { return true; }
        }
        false
    }
}

impl Drop for ProgramPlayer {
    fn drop(&mut self) {
        let _ = self.pipeline.set_state(gst::State::Null);
    }
}
