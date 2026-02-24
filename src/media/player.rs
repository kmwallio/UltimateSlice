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
}

impl Player {
    /// Create a new player. Returns `(player, paintable)` — attach `paintable`
    /// to a `gtk4::Picture` to display video.
    pub fn new() -> Result<(Self, gdk4::Paintable)> {
        gst::init()?;

        let paintablesink = gst::ElementFactory::make("gtk4paintablesink")
            .build()
            .map_err(|_| anyhow!("gtk4paintablesink not available — install gst-plugins-rs"))?;

        let paintable = {
            let obj = paintablesink.property::<glib::Object>("paintable");
            obj.dynamic_cast::<gdk4::Paintable>()
                .expect("gtk4paintablesink 'paintable' property must implement gdk4::Paintable")
        };

        // Use glsinkbin to wrap the paintablesink for GPU-accelerated upload
        let video_sink = match gst::ElementFactory::make("glsinkbin")
            .property("sink", &paintablesink)
            .build()
        {
            Ok(s) => s,
            Err(_) => {
                // Fallback: use paintablesink directly
                paintablesink.clone()
            }
        };

        let pipeline = gst::ElementFactory::make("playbin")
            .property("video-sink", &video_sink)
            .build()?;

        let state = Arc::new(Mutex::new(PlayerState::Stopped));

        Ok((Self { pipeline, state }, paintable))
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

    /// Seek to an absolute position in nanoseconds
    pub fn seek(&self, position_ns: u64) -> Result<()> {
        self.pipeline.seek_simple(
            gst::SeekFlags::FLUSH | gst::SeekFlags::KEY_UNIT,
            gst::ClockTime::from_nseconds(position_ns),
        )?;
        Ok(())
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
