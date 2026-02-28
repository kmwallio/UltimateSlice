pub mod player;
pub mod thumbnail;
pub mod thumb_cache;
pub mod waveform_cache;
pub mod probe_cache;
pub mod proxy_cache;
pub mod program_player;
pub mod export;

/// RAII guard that sets a GStreamer pipeline to NULL on drop.
/// Prevents "Trying to dispose element ... but it is in READY" warnings
/// when a function returns early (via `?`) without explicit cleanup.
pub(crate) struct PipelineGuard(pub gstreamer::Pipeline);

impl Drop for PipelineGuard {
    fn drop(&mut self) {
        use gstreamer::prelude::*;
        let _ = self.0.set_state(gstreamer::State::Null);
    }
}
