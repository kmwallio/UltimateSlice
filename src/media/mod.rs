pub mod audio_sync;
pub mod adjustment_scope;
pub mod ladspa_registry;
pub mod mask_alpha;
pub mod voiceover;
pub mod bg_removal_cache;
pub mod color_match;
pub mod cube_lut;
pub mod export;
pub mod frei0r_registry;
pub mod player;
pub mod probe_cache;
pub mod program_player;
pub mod proxy_cache;
pub mod thumb_cache;
pub mod thumbnail;
pub mod waveform_cache;

/// RAII guard that sets a GStreamer pipeline to NULL on drop.
/// Prevents "Trying to dispose element ... but it is in READY" warnings
/// when a function returns early (via `?`) without explicit cleanup.
pub(crate) struct PipelineGuard(pub gstreamer::Pipeline);

impl Drop for PipelineGuard {
    fn drop(&mut self) {
        use gstreamer::prelude::*;
        let _ = self.0.set_state(gstreamer::State::Null);
        // Wait for the Null transition to complete so streaming threads
        // (e.g. qtdemux's gst_qtdemux_loop) have fully stopped before the
        // pipeline and its elements are freed.  Without this, a qtdemux
        // streaming thread can dereference a freed stream struct (SIGSEGV
        // at offset 0x50 in gst_qtdemux_push_buffer).
        let _ = self
            .0
            .state(gstreamer::ClockTime::from_seconds(5));
    }
}
