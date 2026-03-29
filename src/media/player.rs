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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SourceDecodeMode {
    SoftwareFiltered,
    HardwareFast,
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
    #[allow(dead_code)]
    videobalance: Option<gst::Element>,
    /// gaussianblur element for denoise (positive sigma) / sharpness (negative sigma)
    /// Kept for property storage but NOT linked in the pipeline — gaussianblur
    /// only accepts AYUV format, forcing two expensive I420↔AYUV conversions
    /// per frame even at sigma=0.  Denoise/sharpness is applied during export.
    #[allow(dead_code)]
    gaussianblur: Option<gst::Element>,
    /// videocrop element for per-clip cropping
    videocrop: Option<gst::Element>,
    /// videoflip element for rotation
    videoflip_rotate: Option<gst::Element>,
    /// videoflip element for horizontal/vertical flip
    videoflip_flip: Option<gst::Element>,
    /// Prescale capsfilter — resolution updated at runtime to match widget size
    prescale_caps: Option<gst::Element>,
    /// Queue after prescale, tuned dynamically for playback smoothness
    prescale_queue: Option<gst::Element>,
    /// Software filter bin used by the safe CPU decode path.
    software_video_filter: Option<gst::Element>,
    /// Whether VA-API decoder plugins are available in this runtime.
    vaapi_available: bool,
    /// Original decoder ranks for VA decoder factories (restored in HW mode).
    va_decoder_original_ranks: Vec<(String, gst::Rank)>,
    /// Original decoder ranks for Apple VideoToolbox decoders.
    apple_decoder_original_ranks: Vec<(String, gst::Rank)>,
    /// Active source decode mode.
    decode_mode: Arc<Mutex<SourceDecodeMode>>,
    /// Last URI loaded into the source player.
    current_uri: Arc<Mutex<Option<String>>>,
    /// URI that already failed in HW mode; keep software mode for this URI.
    hw_failed_uri: Arc<Mutex<Option<String>>>,
    /// Source monitor seek policy.
    source_playback_priority: Arc<Mutex<crate::ui_state::PlaybackPriority>>,
    /// Frame duration used for paused seek deduplication.
    frame_duration_ns: Arc<Mutex<u64>>,
    /// Last frame-quantized seek position.
    last_seeked_frame_pos: Arc<Mutex<Option<u64>>>,
    /// Pending seek position deferred until the pipeline finishes an async
    /// state transition.  Avoids FLUSH seeks racing with qtdemux's internal
    /// loop during preroll, which causes a NULL-dereference crash on macOS.
    pending_seek_ns: Arc<Mutex<Option<u64>>>,
    /// Whether the deferred-seek dispatch timer is already running.
    pending_seek_active: Arc<Mutex<bool>>,
    /// Whether the deferred seek should use ACCURATE flags.
    pending_seek_accurate: Arc<Mutex<bool>>,
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

        // Use paintablesink directly — do NOT wrap in glsinkbin.
        //
        // glsinkbin advertises DMABuf sink caps (video/x-raw(memory:DMABuf)).
        // VA-API decoders also output DMABuf, so GStreamer's greedy caps
        // negotiation routes: VA-API decoder → [DMABuf] → glsinkbin → sink,
        // completely BYPASSING playbin's video-filter property.  The software
        // prescale filter (≤640×360, I420) is then silently ignored and the
        // VA-API decoder outputs DMABuf frames directly to the sink at full
        // resolution, causing odd-height frames (e.g. 1156×495 for a widget
        // resized to non-even dimensions) and a torrent of per-frame
        // "not valid for dmabuf format YU12" errors.
        //
        // Without glsinkbin, gtk4paintablesink only advertises system-memory
        // caps, forcing GStreamer to insert a DMABuf→system-memory converter,
        // which makes the video-filter reliably usable in both decode modes.
        let gl_video_sink: Option<gst::Element> = None;
        let video_sink = &paintablesink;

        let pipeline = gst::ElementFactory::make("playbin")
            .property("video-sink", video_sink)
            .build()?;

        let va_decoder_names = [
            "vah264dec",
            "vah265dec",
            "vampeg2dec",
            "vavp8dec",
            "vavp9dec",
            "vaav1dec",
        ];
        let mut va_decoder_original_ranks: Vec<(String, gst::Rank)> = Vec::new();
        for name in &va_decoder_names {
            if let Some(factory) = gst::ElementFactory::find(name) {
                va_decoder_original_ranks.push(((*name).to_string(), factory.rank()));
            }
        }
        let vaapi_available = !va_decoder_original_ranks.is_empty();
        let apple_decoder_names = ["vtdec", "vtdec_hw"];
        let mut apple_decoder_original_ranks: Vec<(String, gst::Rank)> = Vec::new();
        for name in &apple_decoder_names {
            if let Some(factory) = gst::ElementFactory::find(name) {
                apple_decoder_original_ranks.push(((*name).to_string(), factory.rank()));
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
        let mut prescale_queue_field: Option<gst::Element> = None;
        let mut software_video_filter_field: Option<gst::Element> = None;

        if let Some(ref vb) = videobalance {
            let bin = gst::Bin::new();

            // Downscale oversized sources early. Default 640×360 matches
            // the typical ~320×200 source preview widget (slight
            // supersample). Updated at runtime via set_prescale_resolution().
            let prescale = gst::ElementFactory::make("videoconvertscale")
                .build()
                .expect("videoconvertscale must be available");
            // Use nearest-neighbour scaling for speed — source preview quality is
            // secondary to a smooth frame rate.  Bilinear (default) adds significant
            // cost when downscaling from 5.3K to 640×360.
            if prescale.find_property("method").is_some() {
                prescale.set_property_from_str("method", "nearest-neighbour");
            }
            // Multi-threaded prescale: each CPU tile is converted in parallel,
            // cutting the 5.3K→640×360 cost proportionally.
            // On macOS, vtdec (VideoToolbox) outputs IOSurface-backed NV12 buffers.
            // The parallelized task runner in videoconvertscale reads them on worker
            // threads after the backing memory may be released → SIGSEGV in
            // unpack_NV12.  Force single-threaded conversion to avoid this.
            #[cfg(target_os = "macos")]
            if prescale.find_property("n-threads").is_some() {
                prescale.set_property("n-threads", 1u32);
            }
            #[cfg(not(target_os = "macos"))]
            if prescale.find_property("n-threads").is_some() {
                let threads = (std::thread::available_parallelism()
                    .map(|n| n.get() as u32)
                    .unwrap_or(4)
                    / 2)
                .clamp(2, 8);
                prescale.set_property("n-threads", threads);
            }
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
                .property("max-size-buffers", 8u32)
                .property("max-size-bytes", 0u32)
                .property("max-size-time", 0u64)
                .property_from_str("leaky", "no")
                .build()
                .expect("queue must be available");

            if let (Some(ref vc), Some(ref vfr), Some(ref vff)) =
                (&videocrop, &videoflip_rotate, &videoflip_flip)
            {
                bin.add_many([&prescale, &prescale_caps, &prescale_queue, vc, vb, vfr, vff])
                    .ok();
                gst::Element::link_many([
                    &prescale,
                    &prescale_caps,
                    &prescale_queue,
                    vc,
                    vb,
                    vfr,
                    vff,
                ])
                .ok();
                let sink_pad = prescale.static_pad("sink").unwrap();
                let src_pad = vff.static_pad("src").unwrap();
                bin.add_pad(&gst::GhostPad::with_target(&sink_pad).unwrap())
                    .ok();
                bin.add_pad(&gst::GhostPad::with_target(&src_pad).unwrap())
                    .ok();
            } else {
                bin.add_many([&prescale, &prescale_caps, &prescale_queue, vb])
                    .ok();
                gst::Element::link_many([&prescale, &prescale_caps, &prescale_queue, vb]).ok();
                let sink_pad = prescale.static_pad("sink").unwrap();
                let src_pad = vb.static_pad("src").unwrap();
                bin.add_pad(&gst::GhostPad::with_target(&sink_pad).unwrap())
                    .ok();
                bin.add_pad(&gst::GhostPad::with_target(&src_pad).unwrap())
                    .ok();
            }
            prescale_caps_field = Some(prescale_caps);
            prescale_queue_field = Some(prescale_queue);
            let bin_element: gst::Element = bin.upcast();
            software_video_filter_field = Some(bin_element.clone());
            pipeline.set_property("video-filter", &bin_element);
        }

        // Enable multi-threaded decoding on software decoders and fast scaling
        // on any videoconvertscale elements that playbin auto-creates internally
        // (colour-space converters, sinks, etc.).
        //
        // On macOS: vtdec (VideoToolbox) outputs IOSurface-backed NV12 buffers.
        // The parallelized task runner in videoconvertscale reads them on worker
        // threads after the backing memory may be released → SIGSEGV in
        // unpack_NV12.  Force single-threaded conversion on macOS only.
        pipeline.connect("deep-element-added", false, |args| {
            let child = args[2].get::<gst::Element>().ok()?;
            let factory_name = child
                .factory()
                .map(|f| f.name().to_string())
                .unwrap_or_default();
            // Enable multi-threaded libav software decoders (avdec_h265, avdec_h264, …).
            // max-threads=0 means "auto" but in practice FFmpeg caps it conservatively;
            // setting it explicitly to the core count gives the full thread pool.
            if factory_name.starts_with("avdec_") {
                if child.find_property("max-threads").is_some() {
                    let threads = (std::thread::available_parallelism()
                        .map(|n| n.get() as i32)
                        .unwrap_or(4)
                        / 2)
                    .clamp(2, 8);
                    child.set_property("max-threads", threads);
                }
            }
            #[cfg(target_os = "macos")]
            if factory_name == "videoconvertscale" && child.find_property("n-threads").is_some() {
                child.set_property("n-threads", 1u32);
            }
            None
        });

        let state = Arc::new(Mutex::new(PlayerState::Stopped));
        let hardware_acceleration_enabled = Arc::new(Mutex::new(hardware_acceleration_enabled));
        // Always start with HardwareFast so apply_decode_mode() below never
        // early-returns.  The initial stored value must differ from whatever
        // we pass to apply_decode_mode() or the early-return guard fires and
        // the VA-API rank policy / software video-filter are never applied.
        let decode_mode = Arc::new(Mutex::new(SourceDecodeMode::HardwareFast));
        // Source preview always uses the software-filtered path.  VA-API decoders
        // output DMABuf frames; even with glsinkbin removed the DMABuf→system-memory
        // conversion has proven unreliable across driver/kernel combinations, and
        // the prescale filter (≤640×360) keeps CPU cost negligible for a preview
        // widget.  Hardware acceleration setting is respected for the program
        // player (export pipeline) but NOT for this source-preview player.
        let initial_mode = SourceDecodeMode::SoftwareFiltered;
        let current_uri = Arc::new(Mutex::new(None));
        let hw_failed_uri = Arc::new(Mutex::new(None));
        let source_playback_priority =
            Arc::new(Mutex::new(crate::ui_state::PlaybackPriority::Smooth));
        let frame_duration_ns = Arc::new(Mutex::new(41_666_667));
        let last_seeked_frame_pos = Arc::new(Mutex::new(None));
        let pending_seek_ns = Arc::new(Mutex::new(None));
        let pending_seek_active = Arc::new(Mutex::new(false));
        let pending_seek_accurate = Arc::new(Mutex::new(false));

        let player = Self {
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
            prescale_queue: prescale_queue_field,
            software_video_filter: software_video_filter_field,
            vaapi_available,
            va_decoder_original_ranks,
            apple_decoder_original_ranks,
            decode_mode,
            current_uri,
            hw_failed_uri,
            source_playback_priority,
            frame_duration_ns,
            last_seeked_frame_pos,
            pending_seek_ns,
            pending_seek_active,
            pending_seek_accurate,
        };

        // Apply the correct decode mode for startup.  Because decode_mode was
        // initialised to HardwareFast above, calling apply_decode_mode with
        // either value is guaranteed not to early-return, so VA-API ranks and
        // the software video-filter are always configured correctly.
        player.apply_decode_mode(initial_mode);

        Ok((player, paintable))
    }

    /// Load a URI (e.g. `file:///path/to/video.mp4`)
    pub fn load(&self, uri: &str) -> Result<()> {
        {
            let mut current = self.current_uri.lock().unwrap();
            *current = Some(uri.to_string());
        }
        *self.last_seeked_frame_pos.lock().unwrap() = None;
        self.apply_decode_mode(self.preferred_mode_for_uri(uri));
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
        self.set_playback_smoothness_policy(false);
        *self.state.lock().unwrap() = PlayerState::Paused;
        Ok(())
    }

    pub fn play(&self) -> Result<()> {
        self.set_playback_smoothness_policy(true);
        Self::safe_set_state(&self.pipeline, gst::State::Playing);
        *self.state.lock().unwrap() = PlayerState::Playing;
        Ok(())
    }

    pub fn pause(&self) -> Result<()> {
        self.set_playback_smoothness_policy(false);
        Self::safe_set_state(&self.pipeline, gst::State::Paused);
        *self.state.lock().unwrap() = PlayerState::Paused;
        Ok(())
    }

    pub fn stop(&self) -> Result<()> {
        self.set_playback_smoothness_policy(false);
        // Go to PAUSED rather than READY to preserve the gtk4paintablesink GL
        // context.  Going to READY tears down the GL texture; the subsequent
        // READY→PAUSED→PLAYING transition cannot reconstruct it without a full
        // GL preroll cycle, leaving video permanently black while audio plays.
        //
        // We intentionally do NOT seek to position 0 here: calling seek_simple()
        // in the same call-chain as safe_set_state creates an async race where
        // the deferred set_state(Paused) timer can fire during a subsequent
        // load() preroll and corrupt the playbin state machine.
        Self::safe_set_state(&self.pipeline, gst::State::Paused);
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
        let (_result, _state, pending) = pipeline.state(Some(gst::ClockTime::from_mseconds(0)));
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

    /// Start a periodic timer that waits for the pipeline to finish any async
    /// state transition, then dispatches the most-recently-requested seek.
    /// Coalesces rapid seek requests so only the latest position is applied.
    fn arm_pending_seek_dispatch(&self, _playing: bool) {
        let mut active = self.pending_seek_active.lock().unwrap();
        if *active {
            return; // timer already running — it will pick up the latest position
        }
        *active = true;
        drop(active);

        let pipeline = self.pipeline.clone();
        let pending_seek_ns = self.pending_seek_ns.clone();
        let pending_seek_active = self.pending_seek_active.clone();
        let pending_seek_accurate = self.pending_seek_accurate.clone();
        let last_seeked_frame_pos = self.last_seeked_frame_pos.clone();
        let source_playback_priority = self.source_playback_priority.clone();

        glib::timeout_add_local(std::time::Duration::from_millis(50), move || {
            let (_r, _s, p) = pipeline.state(Some(gst::ClockTime::from_mseconds(0)));
            if p != gst::State::VoidPending {
                return glib::ControlFlow::Continue;
            }
            // Pipeline is in a stable state — safe to seek.
            let pos = pending_seek_ns.lock().unwrap().take();
            *pending_seek_active.lock().unwrap() = false;
            let accurate = *pending_seek_accurate.lock().unwrap();

            if let Some(pos_ns) = pos {
                *last_seeked_frame_pos.lock().unwrap() = None;
                let flags = if accurate {
                    gst::SeekFlags::FLUSH | gst::SeekFlags::ACCURATE
                } else {
                    let prio = source_playback_priority.lock().unwrap().clone();
                    match prio {
                        crate::ui_state::PlaybackPriority::Accurate => {
                            gst::SeekFlags::FLUSH | gst::SeekFlags::ACCURATE
                        }
                        _ => gst::SeekFlags::FLUSH | gst::SeekFlags::KEY_UNIT,
                    }
                };
                let _ = pipeline.seek_simple(flags, gst::ClockTime::from_nseconds(pos_ns));
            }

            glib::ControlFlow::Break
        });
    }

    #[allow(dead_code)]
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
        let playing = *self.state.lock().unwrap() == PlayerState::Playing;
        if !playing {
            let frame_ns = (*self.frame_duration_ns.lock().unwrap()).max(1);
            let frame_pos = (position_ns / frame_ns) * frame_ns;
            let mut last = self.last_seeked_frame_pos.lock().unwrap();
            if *last == Some(frame_pos) {
                return Ok(());
            }
            *last = Some(frame_pos);
        } else {
            *self.last_seeked_frame_pos.lock().unwrap() = None;
        }
        // Defer FLUSH seeks while the pipeline has an async state transition
        // in progress (e.g. Null→Paused preroll).  Sending a FLUSH event into
        // qtdemux while its streaming loop is initializing causes a
        // NULL-dereference crash on macOS (gst_qtdemux_push_buffer + 0x50).
        let (_result, _current, pending) =
            self.pipeline.state(Some(gst::ClockTime::from_mseconds(0)));
        if pending != gst::State::VoidPending {
            *self.pending_seek_ns.lock().unwrap() = Some(position_ns);
            *self.pending_seek_accurate.lock().unwrap() = false;
            self.arm_pending_seek_dispatch(playing);
            return Ok(());
        }
        self.pipeline.seek_simple(
            self.source_seek_flags(playing),
            gst::ClockTime::from_nseconds(position_ns),
        )?;
        Ok(())
    }

    /// Frame-accurate seek to an absolute position in nanoseconds.
    /// Slower than `seek()` but lands on the exact requested frame.
    pub fn seek_accurate(&self, position_ns: u64) -> Result<()> {
        let frame_ns = (*self.frame_duration_ns.lock().unwrap()).max(1);
        let frame_pos = (position_ns / frame_ns) * frame_ns;
        *self.last_seeked_frame_pos.lock().unwrap() = Some(frame_pos);
        // Defer FLUSH seeks during async state transitions (see seek()).
        let (_result, _current, pending) =
            self.pipeline.state(Some(gst::ClockTime::from_mseconds(0)));
        if pending != gst::State::VoidPending {
            *self.pending_seek_ns.lock().unwrap() = Some(position_ns);
            *self.pending_seek_accurate.lock().unwrap() = true;
            self.arm_pending_seek_dispatch(false);
            return Ok(());
        }
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

    pub fn current_uri(&self) -> Option<String> {
        self.current_uri.lock().unwrap().clone()
    }

    pub fn set_hardware_acceleration(&self, enabled: bool) -> Result<()> {
        *self.hardware_acceleration_enabled.lock().unwrap() = enabled;
        // Source preview always stays in SoftwareFiltered mode regardless of
        // the hardware-acceleration setting — see Player::new() for rationale.
        // We still store the flag (affects preferred_mode_for_uri callers) but
        // do not trigger a decode-mode switch here.
        Ok(())
    }

    pub fn set_source_playback_priority(&self, priority: crate::ui_state::PlaybackPriority) {
        *self.source_playback_priority.lock().unwrap() = priority;
        let playing = *self.state.lock().unwrap() == PlayerState::Playing;
        self.set_playback_smoothness_policy(playing);
    }

    pub fn set_source_frame_duration(&self, frame_duration_ns: u64) {
        *self.frame_duration_ns.lock().unwrap() = frame_duration_ns.max(1);
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

    fn preferred_mode_for_uri(&self, uri: &str) -> SourceDecodeMode {
        let hw_enabled = *self.hardware_acceleration_enabled.lock().unwrap();
        if !hw_enabled || !self.vaapi_available {
            return SourceDecodeMode::SoftwareFiltered;
        }
        let hw_failed_uri = self.hw_failed_uri.lock().unwrap();
        if hw_failed_uri.as_deref() == Some(uri) {
            SourceDecodeMode::SoftwareFiltered
        } else {
            SourceDecodeMode::HardwareFast
        }
    }

    fn apply_decode_mode(&self, mode: SourceDecodeMode) {
        let current = *self.decode_mode.lock().unwrap();
        if current == mode {
            return;
        }
        // Always keep the software_video_filter active regardless of decode mode.
        // In HardwareFast mode, VA-API outputs DMA-buf frames; without the filter
        // playsink's internal videoconvertscale can produce odd-height output
        // (e.g. 1156×495) that is invalid for YU12 DMA-buf format, causing a
        // torrent of per-frame "not valid for dmabuf" errors and a black display.
        // The prescale filter (≤640×360, I420) enforces even dimensions and keeps
        // CPU load low, so removing it in HW mode offers no practical benefit for
        // a source-preview widget.
        match mode {
            SourceDecodeMode::SoftwareFiltered => {
                self.set_va_decoder_rank_policy(false);
                self.set_apple_decoder_rank_policy(false);
            }
            SourceDecodeMode::HardwareFast => {
                self.set_va_decoder_rank_policy(true);
                self.set_apple_decoder_rank_policy(true);
            }
        }
        if let Some(ref filter) = self.software_video_filter {
            self.pipeline.set_property("video-filter", filter);
        }
        *self.decode_mode.lock().unwrap() = mode;
    }

    fn set_va_decoder_rank_policy(&self, restore_original: bool) {
        for (name, original_rank) in &self.va_decoder_original_ranks {
            if let Some(factory) = gst::ElementFactory::find(name) {
                if restore_original {
                    factory.set_rank(*original_rank);
                } else {
                    factory.set_rank(gst::Rank::MARGINAL);
                }
            }
        }
    }

    fn set_apple_decoder_rank_policy(&self, restore_original: bool) {
        for (name, original_rank) in &self.apple_decoder_original_ranks {
            if let Some(factory) = gst::ElementFactory::find(name) {
                if restore_original {
                    factory.set_rank(*original_rank);
                } else {
                    factory.set_rank(gst::Rank::NONE);
                }
            }
        }
    }

    pub fn fallback_to_software_after_error(&self) -> Result<bool> {
        if *self.decode_mode.lock().unwrap() != SourceDecodeMode::HardwareFast {
            return Ok(false);
        }
        let uri = match self.current_uri.lock().unwrap().clone() {
            Some(u) => u,
            None => return Ok(false),
        };
        *self.hw_failed_uri.lock().unwrap() = Some(uri.clone());
        let was_playing = *self.state.lock().unwrap() == PlayerState::Playing;
        self.apply_decode_mode(SourceDecodeMode::SoftwareFiltered);
        self.pipeline.set_state(gst::State::Null)?;
        let _ = self
            .pipeline
            .state(Some(gst::ClockTime::from_mseconds(150)));
        self.pipeline.set_property("uri", uri.as_str());
        self.pipeline.set_state(gst::State::Paused)?;
        self.set_playback_smoothness_policy(false);
        if was_playing {
            self.play()?;
        } else {
            *self.state.lock().unwrap() = PlayerState::Paused;
        }
        Ok(true)
    }

    fn set_playback_smoothness_policy(&self, playing: bool) {
        let accurate_mode = matches!(
            *self.source_playback_priority.lock().unwrap(),
            crate::ui_state::PlaybackPriority::Accurate
        );
        if let Some(ref q) = self.prescale_queue {
            if playing {
                if accurate_mode {
                    q.set_property("max-size-buffers", 6u32);
                    q.set_property_from_str("leaky", "no");
                } else {
                    q.set_property("max-size-buffers", 2u32);
                    q.set_property_from_str("leaky", "downstream");
                }
            } else {
                q.set_property("max-size-buffers", 8u32);
                q.set_property_from_str("leaky", "no");
            }
        }
        Self::configure_sink_drop_late(&self.paintablesink, playing && !accurate_mode);
        if let Some(ref sink) = self.gl_video_sink {
            Self::configure_sink_drop_late(sink, playing && !accurate_mode);
        }
    }

    fn source_seek_flags(&self, _playing: bool) -> gst::SeekFlags {
        match *self.source_playback_priority.lock().unwrap() {
            crate::ui_state::PlaybackPriority::Accurate => {
                gst::SeekFlags::FLUSH | gst::SeekFlags::ACCURATE
            }
            crate::ui_state::PlaybackPriority::Balanced
            | crate::ui_state::PlaybackPriority::Smooth => {
                gst::SeekFlags::FLUSH | gst::SeekFlags::KEY_UNIT
            }
        }
    }

    fn configure_sink_drop_late(sink: &gst::Element, playing: bool) {
        if sink.find_property("qos").is_some() {
            sink.set_property("qos", playing);
        }
        if sink.find_property("max-lateness").is_some() {
            let max_lateness_ns: i64 = if playing { 20_000_000 } else { -1 };
            sink.set_property("max-lateness", max_lateness_ns);
        }
    }

    /// Apply color correction and denoise/sharpness to the video filter elements.
    /// - brightness: -1.0 to 1.0 (0.0 = neutral)
    /// - contrast:   0.0 to 2.0  (1.0 = neutral)
    /// - saturation: 0.0 to 2.0  (1.0 = neutral)
    /// - denoise:    0.0 to 1.0  (0.0 = off; maps to positive gaussianblur sigma)
    /// - sharpness:  -1.0 to 1.0 (0.0 = neutral; negative = soften, positive = sharpen)
    #[allow(dead_code)]
    pub fn set_color(&self, brightness: f64, contrast: f64, saturation: f64) {
        if let Some(ref vb) = self.videobalance {
            vb.set_property("brightness", brightness.clamp(-1.0, 1.0));
            vb.set_property("contrast", contrast.clamp(0.0, 2.0));
            vb.set_property("saturation", saturation.clamp(0.0, 2.0));
        }
    }

    /// Apply denoise and sharpness via the gaussianblur video filter.
    /// Combined sigma = denoise * 4 − sharpness * 6 (clamped to −20..20).
    #[allow(dead_code)]
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
                90 => "counterclockwise",
                -90 | 270 => "clockwise",
                180 | -180 => "rotate-180",
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

    pub fn vaapi_available(&self) -> bool {
        self.vaapi_available
    }

    pub fn decode_mode_name(&self) -> &'static str {
        match *self.decode_mode.lock().unwrap() {
            SourceDecodeMode::SoftwareFiltered => "software_filtered",
            SourceDecodeMode::HardwareFast => "hardware_fast",
        }
    }
}

impl Drop for Player {
    fn drop(&mut self) {
        let _ = self.pipeline.set_state(gst::State::Null);
    }
}
