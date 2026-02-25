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
    /// videocrop element for per-clip cropping
    videocrop: Option<gst::Element>,
    /// videoflip element for rotation
    videoflip_rotate: Option<gst::Element>,
    /// videoflip element for horizontal/vertical flip
    videoflip_flip: Option<gst::Element>,
    /// textoverlay element for per-clip title
    textoverlay: Option<gst::Element>,
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
                if let Some(ref to) = textoverlay {
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
                videocrop,
                videoflip_rotate,
                videoflip_flip,
                textoverlay,
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

    /// Stop playback and return to position 0.
    pub fn stop(&mut self) {
        let _ = self.pipeline.set_state(gst::State::Paused);
        self.state = PlayerState::Paused;
        self.timeline_pos_ns = 0;
        if let Some(idx) = self.clip_at(0) {
            self.load_clip_idx(idx, 0);
        } else {
            let _ = self.pipeline.set_state(gst::State::Ready);
            self.current_idx = None;
        }
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
        // In PAUSED state, force frame redecode using tracked timeline_pos_ns.
        if self.current_idx.is_some() && self.state != PlayerState::Playing {
            let pos = self.timeline_pos_ns;
            if let Some(idx) = self.clip_at(pos) {
                let clip = &self.clips[idx];
                let source_ns = clip.source_in_ns + pos.saturating_sub(clip.timeline_start_ns);
                let _ = self.pipeline.seek_simple(
                    gst::SeekFlags::FLUSH | gst::SeekFlags::KEY_UNIT,
                    gst::ClockTime::from_nseconds(source_ns),
                );
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
                let source_ns = clip.source_in_ns + pos.saturating_sub(clip.timeline_start_ns);
                let _ = self.pipeline.seek_simple(
                    gst::SeekFlags::FLUSH | gst::SeekFlags::KEY_UNIT,
                    gst::ClockTime::from_nseconds(source_ns),
                );
            }
        }
    }

    /// Directly update transform on the current clip without reloading the pipeline.
    pub fn update_current_transform(&mut self, crop_left: i32, crop_right: i32, crop_top: i32, crop_bottom: i32, rotate: i32, flip_h: bool, flip_v: bool) {
        self.set_transform(crop_left, crop_right, crop_top, crop_bottom, rotate, flip_h, flip_v);
        // Force the current frame to redecode with the new transform by seeking to the
        // current timeline position. Use timeline_pos_ns (not query_position, which can
        // return 0 during pre-roll and would jump the playhead to the start).
        if self.current_idx.is_some() && self.state != PlayerState::Playing {
            let pos = self.timeline_pos_ns;
            if let Some(idx) = self.clip_at(pos) {
                let clip = &self.clips[idx];
                let source_ns = clip.source_in_ns + pos.saturating_sub(clip.timeline_start_ns);
                let _ = self.pipeline.seek_simple(
                    gst::SeekFlags::FLUSH | gst::SeekFlags::KEY_UNIT,
                    gst::ClockTime::from_nseconds(source_ns),
                );
            }
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
        // Apply per-clip transform (crop, rotate, flip)
        self.set_transform(clip.crop_left, clip.crop_right, clip.crop_top, clip.crop_bottom,
                           clip.rotate, clip.flip_h, clip.flip_v);
        // Apply per-clip title overlay
        self.set_title(&clip.title_text, &clip.title_font, clip.title_color, clip.title_x, clip.title_y);

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
