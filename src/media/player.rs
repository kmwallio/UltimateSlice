use std::sync::{Arc, Mutex};
use anyhow::{anyhow, Result};
use gstreamer as gst;
use gstreamer::prelude::*;

/// Playback state
#[derive(Debug, Clone, PartialEq)]
pub enum PlayerState {
    Stopped,
    Playing,
    Paused,
}

/// Wraps a GStreamer playbin pipeline and exposes simple controls.
/// The video sink is `gtk4paintablesink`, which produces a `gdk4::Paintable`
/// that can be displayed in a `gtk4::Picture` widget.
pub struct Player {
    pipeline: gst::Element,
    state: Arc<Mutex<PlayerState>>,
    paintablesink: gst::Element,
    gl_video_sink: Option<gst::Element>,
    hardware_acceleration_enabled: Arc<Mutex<bool>>,
    /// videobalance element for color correction (brightness/contrast/saturation)
    videobalance: Option<gst::Element>,
    /// gaussianblur element for denoise (positive sigma) / sharpness (negative sigma)
    gaussianblur: Option<gst::Element>,
    /// videocrop element for per-clip cropping
    videocrop: Option<gst::Element>,
    /// videoflip element for rotation
    videoflip_rotate: Option<gst::Element>,
    /// videoflip element for horizontal/vertical flip
    videoflip_flip: Option<gst::Element>,
}

impl Player {
    /// Create a new player. Returns `(player, paintable)` — attach `paintable`
    /// to a `gtk4::Picture` to display video.
    pub fn new(hardware_acceleration_enabled: bool) -> Result<(Self, gdk4::Paintable)> {
        gst::init()?;

        let paintablesink = gst::ElementFactory::make("gtk4paintablesink")
            .build()
            .map_err(|_| anyhow!("gtk4paintablesink not available — install gst-plugins-rs"))?;

        let paintable = {
            let obj = paintablesink.property::<glib::Object>("paintable");
            obj.dynamic_cast::<gdk4::Paintable>()
                .expect("gtk4paintablesink 'paintable' property must implement gdk4::Paintable")
        };

        // Optional GL sink path for hardware-accelerated upload.
        let gl_video_sink = match gst::ElementFactory::make("glsinkbin")
            .property("sink", &paintablesink)
            .build()
        {
            Ok(s) => Some(s),
            Err(_) => None,
        };
        let video_sink = if hardware_acceleration_enabled {
            gl_video_sink.as_ref().unwrap_or(&paintablesink)
        } else {
            &paintablesink
        };

        let pipeline = gst::ElementFactory::make("playbin")
            .property("video-sink", video_sink)
            .build()?;

        // Build a filter bin:
        // videocrop ! videoconvert ! videobalance ! videoconvert ! gaussianblur
        //   ! videoconvert ! videoflip_rotate ! videoconvert ! videoflip_flip
        // Chained as playbin's video-filter for per-clip color + denoise/sharpness + transform.
        let videobalance = gst::ElementFactory::make("videobalance").build().ok();
        let gaussianblur = gst::ElementFactory::make("gaussianblur").build().ok();
        let videocrop = gst::ElementFactory::make("videocrop").build().ok();
        let videoflip_rotate = gst::ElementFactory::make("videoflip").build().ok();
        let videoflip_flip = gst::ElementFactory::make("videoflip")
            .name("videoflip_flip")
            .build()
            .ok();

        if videobalance.is_some() && gaussianblur.is_some() {
            let vb = videobalance.as_ref().unwrap();
            let gb = gaussianblur.as_ref().unwrap();
            // sigma=0 means no blur/sharpen (neutral)
            gb.set_property("sigma", 0.0_f64);

            let bin = gst::Bin::new();
            let conv1 = gst::ElementFactory::make("videoconvert").build()
                .expect("videoconvert must be available");
            let conv2 = gst::ElementFactory::make("videoconvert").build()
                .expect("videoconvert must be available");

            // Determine sink element (videocrop if available, else videobalance)
            // and src element (last flip if available, else gaussianblur)
            if let (Some(ref vc), Some(ref vfr), Some(ref vff)) =
                (&videocrop, &videoflip_rotate, &videoflip_flip)
            {
                let conv3 = gst::ElementFactory::make("videoconvert").build()
                    .expect("videoconvert must be available");
                let conv4 = gst::ElementFactory::make("videoconvert").build()
                    .expect("videoconvert must be available");
                bin.add_many([vc, &conv1, vb, &conv2, gb, &conv3, vfr, &conv4, vff]).ok();
                gst::Element::link_many([vc, &conv1, vb, &conv2, gb, &conv3, vfr, &conv4, vff]).ok();
                let sink_pad = vc.static_pad("sink").unwrap();
                let src_pad = vff.static_pad("src").unwrap();
                bin.add_pad(&gst::GhostPad::with_target(&sink_pad).unwrap()).ok();
                bin.add_pad(&gst::GhostPad::with_target(&src_pad).unwrap()).ok();
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
            // Fallback: videobalance only (no gaussianblur plugin)
            pipeline.set_property("video-filter", vb);
        }

        let state = Arc::new(Mutex::new(PlayerState::Stopped));
        let hardware_acceleration_enabled = Arc::new(Mutex::new(hardware_acceleration_enabled));

        Ok((Self {
            pipeline,
            state,
            paintablesink,
            gl_video_sink,
            hardware_acceleration_enabled,
            videobalance,
            gaussianblur,
            videocrop,
            videoflip_rotate,
            videoflip_flip,
        }, paintable))
    }

    /// Load a URI (e.g. `file:///path/to/video.mp4`)
    pub fn load(&self, uri: &str) -> Result<()> {
        self.pipeline.set_state(gst::State::Ready)?;
        self.pipeline.set_property("uri", uri);
        self.pipeline.set_state(gst::State::Paused)?;
        *self.state.lock().unwrap() = PlayerState::Paused;
        Ok(())
    }

    pub fn play(&self) -> Result<()> {
        self.pipeline.set_state(gst::State::Playing)?;
        *self.state.lock().unwrap() = PlayerState::Playing;
        Ok(())
    }

    pub fn pause(&self) -> Result<()> {
        self.pipeline.set_state(gst::State::Paused)?;
        *self.state.lock().unwrap() = PlayerState::Paused;
        Ok(())
    }

    pub fn stop(&self) -> Result<()> {
        self.pipeline.set_state(gst::State::Ready)?;
        *self.state.lock().unwrap() = PlayerState::Stopped;
        Ok(())
    }

    pub fn toggle_play_pause(&self) -> Result<()> {
        let state = self.state.lock().unwrap().clone();
        drop(state);
        match *self.state.lock().unwrap() {
            PlayerState::Playing => self.pause(),
            _ => self.play(),
        }
    }

    /// Seek to an absolute position in nanoseconds (snaps to nearest keyframe)
    pub fn seek(&self, position_ns: u64) -> Result<()> {
        self.pipeline.seek_simple(
            gst::SeekFlags::FLUSH | gst::SeekFlags::KEY_UNIT,
            gst::ClockTime::from_nseconds(position_ns),
        )?;
        Ok(())
    }

    /// Frame-accurate seek to an absolute position in nanoseconds.
    /// Slower than `seek()` but lands on the exact requested frame.
    pub fn seek_accurate(&self, position_ns: u64) -> Result<()> {
        self.pipeline.seek_simple(
            gst::SeekFlags::FLUSH | gst::SeekFlags::ACCURATE,
            gst::ClockTime::from_nseconds(position_ns),
        )?;
        Ok(())
    }

    /// Step forward by one frame (frame_duration_ns = 1e9 / fps).
    pub fn step_forward(&self, frame_duration_ns: u64) -> Result<u64> {
        let pos = self.position();
        let dur = self.duration();
        let new_pos = (pos + frame_duration_ns).min(dur);
        self.seek_accurate(new_pos)?;
        Ok(new_pos)
    }

    /// Step backward by one frame (frame_duration_ns = 1e9 / fps).
    pub fn step_backward(&self, frame_duration_ns: u64) -> Result<u64> {
        let pos = self.position();
        let new_pos = pos.saturating_sub(frame_duration_ns);
        self.seek_accurate(new_pos)?;
        Ok(new_pos)
    }

    /// Current playback position in nanoseconds, or 0 if unknown
    pub fn position(&self) -> u64 {
        self.pipeline
            .query_position::<gst::ClockTime>()
            .map(|t| t.nseconds())
            .unwrap_or(0)
    }

    /// Total duration in nanoseconds, or 0 if unknown
    pub fn duration(&self) -> u64 {
        self.pipeline
            .query_duration::<gst::ClockTime>()
            .map(|t| t.nseconds())
            .unwrap_or(0)
    }

    pub fn state(&self) -> PlayerState {
        self.state.lock().unwrap().clone()
    }

    pub fn set_hardware_acceleration(&self, enabled: bool) -> Result<()> {
        let current_enabled = *self.hardware_acceleration_enabled.lock().unwrap();
        if current_enabled == enabled {
            return Ok(());
        }

        let target_sink = if enabled {
            self.gl_video_sink.as_ref().unwrap_or(&self.paintablesink)
        } else {
            &self.paintablesink
        };

        let state_before = self.state();
        let pos_before = self.position();
        self.pipeline.set_state(gst::State::Ready)?;
        self.pipeline.set_property("video-sink", target_sink);

        match state_before {
            PlayerState::Playing => {
                self.pipeline.set_state(gst::State::Paused)?;
                let _ = self.seek(pos_before);
                self.pipeline.set_state(gst::State::Playing)?;
                *self.state.lock().unwrap() = PlayerState::Playing;
            }
            PlayerState::Paused => {
                self.pipeline.set_state(gst::State::Paused)?;
                let _ = self.seek(pos_before);
                *self.state.lock().unwrap() = PlayerState::Paused;
            }
            PlayerState::Stopped => {
                *self.state.lock().unwrap() = PlayerState::Stopped;
            }
        }

        *self.hardware_acceleration_enabled.lock().unwrap() = enabled;
        Ok(())
    }

    /// Apply color correction and denoise/sharpness to the video filter elements.
    /// - brightness: -1.0 to 1.0 (0.0 = neutral)
    /// - contrast:   0.0 to 2.0  (1.0 = neutral)
    /// - saturation: 0.0 to 2.0  (1.0 = neutral)
    /// - denoise:    0.0 to 1.0  (0.0 = off; maps to positive gaussianblur sigma)
    /// - sharpness:  -1.0 to 1.0 (0.0 = neutral; negative = soften, positive = sharpen)
    pub fn set_color(&self, brightness: f64, contrast: f64, saturation: f64) {
        if let Some(ref vb) = self.videobalance {
            vb.set_property("brightness", brightness.clamp(-1.0, 1.0));
            vb.set_property("contrast", contrast.clamp(0.0, 2.0));
            vb.set_property("saturation", saturation.clamp(0.0, 2.0));
        }
    }

    /// Apply denoise and sharpness via the gaussianblur video filter.
    /// Combined sigma = denoise * 4 − sharpness * 6 (clamped to −20..20).
    pub fn set_denoise_sharpness(&self, denoise: f64, sharpness: f64) {
        if let Some(ref gb) = self.gaussianblur {
            let sigma = (denoise * 4.0 - sharpness * 6.0).clamp(-20.0, 20.0);
            gb.set_property("sigma", sigma);
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

    /// Get the underlying GStreamer pipeline (e.g. to connect bus signals)
    pub fn pipeline(&self) -> &gst::Element {
        &self.pipeline
    }
}

impl Drop for Player {
    fn drop(&mut self) {
        let _ = self.pipeline.set_state(gst::State::Null);
    }
}
