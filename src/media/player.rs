use anyhow::{anyhow, Result};
use gstreamer as gst;
use gstreamer::prelude::*;
use std::sync::{Arc, Mutex};

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
    /// Kept for property storage but NOT linked in the pipeline — gaussianblur
    /// only accepts AYUV format, forcing two expensive I420↔AYUV conversions
    /// per frame even at sigma=0.  Denoise/sharpness is applied during export.
    gaussianblur: Option<gst::Element>,
    /// videocrop element for per-clip cropping
    videocrop: Option<gst::Element>,
    /// videoflip element for rotation
    videoflip_rotate: Option<gst::Element>,
    /// videoflip element for horizontal/vertical flip
    videoflip_flip: Option<gst::Element>,
    /// Prescale capsfilter — resolution updated at runtime to match widget size
    prescale_caps: Option<gst::Element>,
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

        // Always try glsinkbin for efficient GL texture upload.
        // Without it, gtk4paintablesink must upload raw CPU buffers to GPU
        // textures on every frame, which can freeze the UI with high-res
        // content (e.g. 5.3K GoPro HEVC).  Falls back to raw paintablesink
        // only if glsinkbin is unavailable (no GL support).
        let gl_video_sink = gst::ElementFactory::make("glsinkbin")
            .property("sink", &paintablesink)
            .build()
            .ok();
        let video_sink = gl_video_sink.as_ref().unwrap_or(&paintablesink);

        let pipeline = gst::ElementFactory::make("playbin")
            .property("video-sink", video_sink)
            .build()?;

        // Force software decoders so the CPU-based effects chain receives
        // plain video/x-raw buffers.  VA-API decoders output DMABuf /
        // VAMemory which cannot be CPU-mapped for videoconvertscale /
        // videobalance.  Lower VA-API decoder ranks below software
        // decoders so GStreamer auto-plugging selects avdec_* instead.
        // The prescale caps (1920×1080) keep SW decode manageable even
        // for 5.3K sources, and the leaky queue drops frames if slow.
        for name in &[
            "vah264dec",
            "vah265dec",
            "vampeg2dec",
            "vavp8dec",
            "vavp9dec",
            "vaav1dec",
        ] {
            if let Some(factory) = gst::ElementFactory::find(name) {
                factory.set_rank(gst::Rank::MARGINAL);
            }
        }

        // Build a filter bin:
        // prescale ! caps(I420,≤640×360) ! queue ! videocrop ! videobalance
        //   ! videoflip_rotate ! videoflip_flip
        // All elements accept I420 natively — no videoconvert needed.
        // gaussianblur is NOT linked: it only accepts AYUV, forcing two
        // expensive I420↔AYUV conversions per frame even at sigma=0.
        // Denoise/sharpness is applied during export instead.
        let videobalance = gst::ElementFactory::make("videobalance").build().ok();
        let gaussianblur = gst::ElementFactory::make("gaussianblur").build().ok();
        let videocrop = gst::ElementFactory::make("videocrop").build().ok();
        let videoflip_rotate = gst::ElementFactory::make("videoflip").build().ok();
        let videoflip_flip = gst::ElementFactory::make("videoflip")
            .name("videoflip_flip")
            .build()
            .ok();

        // Keep gaussianblur element for property storage (set_denoise_sharpness)
        // but don't link it in the pipeline.
        if let Some(ref gb) = gaussianblur {
            gb.set_property("sigma", 0.0_f64);
        }

        let mut prescale_caps_field: Option<gst::Element> = None;

        if let Some(ref vb) = videobalance {
            let bin = gst::Bin::new();

            // Downscale oversized sources early. Default 640×360 matches
            // the typical ~320×200 source preview widget (slight
            // supersample). Updated at runtime via set_prescale_resolution().
            let prescale = gst::ElementFactory::make("videoconvertscale")
                .build()
                .expect("videoconvertscale must be available");
            let prescale_caps = gst::ElementFactory::make("capsfilter")
                .property(
                    "caps",
                    &gst::Caps::builder("video/x-raw")
                        .field("format", "I420")
                        .field("width", gst::IntRange::new(1i32, 640))
                        .field("height", gst::IntRange::new(1i32, 360))
                        .field("pixel-aspect-ratio", gst::Fraction::new(1, 1))
                        .build(),
                )
                .build()
                .expect("capsfilter must be available");

            // Leaky queue after prescale: decouples the expensive
            // decode+prescale thread from the effects chain so that slow
            // decode at 5.3K doesn't stall the sink/main thread.
            // leaky=downstream (2) drops oldest buffers when full.
            let prescale_queue = gst::ElementFactory::make("queue")
                .property("max-size-buffers", 2u32)
                .property("max-size-bytes", 0u32)
                .property("max-size-time", 0u64)
                .property_from_str("leaky", "downstream")
                .build()
                .expect("queue must be available");

            if let (Some(ref vc), Some(ref vfr), Some(ref vff)) =
                (&videocrop, &videoflip_rotate, &videoflip_flip)
            {
                bin.add_many([&prescale, &prescale_caps, &prescale_queue, vc, vb, vfr, vff])
                    .ok();
                gst::Element::link_many([&prescale, &prescale_caps, &prescale_queue, vc, vb, vfr, vff])
                    .ok();
                let sink_pad = prescale.static_pad("sink").unwrap();
                let src_pad = vff.static_pad("src").unwrap();
                bin.add_pad(&gst::GhostPad::with_target(&sink_pad).unwrap())
                    .ok();
                bin.add_pad(&gst::GhostPad::with_target(&src_pad).unwrap())
                    .ok();
            } else {
                bin.add_many([&prescale, &prescale_caps, &prescale_queue, vb]).ok();
                gst::Element::link_many([&prescale, &prescale_caps, &prescale_queue, vb]).ok();
                let sink_pad = prescale.static_pad("sink").unwrap();
                let src_pad = vb.static_pad("src").unwrap();
                bin.add_pad(&gst::GhostPad::with_target(&sink_pad).unwrap())
                    .ok();
                bin.add_pad(&gst::GhostPad::with_target(&src_pad).unwrap())
                    .ok();
            }
            prescale_caps_field = Some(prescale_caps);
            pipeline.set_property("video-filter", &bin);
        }

        let state = Arc::new(Mutex::new(PlayerState::Stopped));
        let hardware_acceleration_enabled = Arc::new(Mutex::new(hardware_acceleration_enabled));

        Ok((
            Self {
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
                prescale_caps: prescale_caps_field,
            },
            paintable,
        ))
    }

    /// Load a URI (e.g. `file:///path/to/video.mp4`)
    pub fn load(&self, uri: &str) -> Result<()> {
        // Fully quiesce playbin before replacing URI. Ready-level reconfiguration
        // can still leave internal playsink children mid-transition under rapid
        // repeated loads, which may trip assertions in playsink element setup.
        self.pipeline.set_state(gst::State::Null)?;
        let _ = self
            .pipeline
            .state(Some(gst::ClockTime::from_mseconds(200)));
        self.pipeline.set_property("uri", uri);
        // Start async Paused preroll. Do NOT block here — gtk4paintablesink
        // needs the main loop to complete GL preroll. Blocking the main
        // thread causes a STATE_LOCK deadlock if play() is called before
        // preroll finishes.
        self.pipeline.set_state(gst::State::Paused)?;
        *self.state.lock().unwrap() = PlayerState::Paused;
        Ok(())
    }

    pub fn play(&self) -> Result<()> {
        Self::safe_set_state(&self.pipeline, gst::State::Playing);
        *self.state.lock().unwrap() = PlayerState::Playing;
        Ok(())
    }

    pub fn pause(&self) -> Result<()> {
        Self::safe_set_state(&self.pipeline, gst::State::Paused);
        *self.state.lock().unwrap() = PlayerState::Paused;
        Ok(())
    }

    pub fn stop(&self) -> Result<()> {
        Self::safe_set_state(&self.pipeline, gst::State::Ready);
        *self.state.lock().unwrap() = PlayerState::Stopped;
        Ok(())
    }

    /// Request a state change without blocking the main thread.
    ///
    /// If the pipeline has no pending async transition the change is applied
    /// immediately.  Otherwise it is deferred via a repeating 50 ms timeout
    /// so the GTK main loop can keep processing events (needed for
    /// gtk4paintablesink GL preroll).  This prevents the STATE_LOCK
    /// deadlock that occurs when `set_state()` is called while an async
    /// transition is still running.
    fn safe_set_state(pipeline: &gst::Element, target: gst::State) {
        let (_result, _state, pending) =
            pipeline.state(Some(gst::ClockTime::from_mseconds(0)));
        if pending == gst::State::VoidPending {
            let _ = pipeline.set_state(target);
            return;
        }
        let pipeline = pipeline.clone();
        glib::timeout_add_local(std::time::Duration::from_millis(50), move || {
            let (_r, _s, p) = pipeline.state(Some(gst::ClockTime::from_mseconds(0)));
            if p != gst::State::VoidPending {
                return glib::ControlFlow::Continue;
            }
            let _ = pipeline.set_state(target);
            glib::ControlFlow::Break
        });
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
        // glsinkbin is always used when available for efficient GL rendering;
        // swapping to raw paintablesink at runtime would trigger the
        // !element_bus assertion because paintablesink already has a bus
        // from glsinkbin.  Track the flag for future use (decoder hints).
        *self.hardware_acceleration_enabled.lock().unwrap() = enabled;
        Ok(())
    }

    /// Update the prescale capsfilter to match the source preview widget size.
    /// `width` and `height` should be the target rendering resolution (e.g.
    /// 2× the widget pixel size for slight supersampling).  Values are
    /// clamped to a minimum of 160×90 and maximum of 1920×1080.
    pub fn set_prescale_resolution(&self, width: i32, height: i32) {
        if let Some(ref caps_elem) = self.prescale_caps {
            let w = width.clamp(160, 1920);
            let h = height.clamp(90, 1080);
            let caps = gst::Caps::builder("video/x-raw")
                .field("format", "I420")
                .field("width", gst::IntRange::new(1i32, w))
                .field("height", gst::IntRange::new(1i32, h))
                .field("pixel-aspect-ratio", gst::Fraction::new(1, 1))
                .build();
            caps_elem.set_property("caps", &caps);
        }
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
    pub fn set_transform(
        &self,
        crop_left: i32,
        crop_right: i32,
        crop_top: i32,
        crop_bottom: i32,
        rotate: i32,
        flip_h: bool,
        flip_v: bool,
    ) {
        if let Some(ref vc) = self.videocrop {
            vc.set_property("left", crop_left.max(0));
            vc.set_property("right", crop_right.max(0));
            vc.set_property("top", crop_top.max(0));
            vc.set_property("bottom", crop_bottom.max(0));
        }
        if let Some(ref vfr) = self.videoflip_rotate {
            let method = match rotate {
                90 => "clockwise",
                180 => "rotate-180",
                270 => "counterclockwise",
                _ => "none",
            };
            vfr.set_property_from_str("method", method);
        }
        if let Some(ref vff) = self.videoflip_flip {
            let method = match (flip_h, flip_v) {
                (true, true) => "rotate-180",
                (true, false) => "horizontal-flip",
                (false, true) => "vertical-flip",
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
