//! App-level hardware video decode policy.
//!
//! GStreamer picks decoders for `uridecodebin` / `playbin` by **global**
//! element-factory rank.  Both monitors used to fight over those ranks (the
//! Source Monitor demoted the VA-API decoders to MARGINAL whenever it was in
//! software mode), which silently forced the Program Monitor onto software
//! decode even when the user had hardware acceleration on.  This module owns
//! that policy in one place: ranks follow the *preference*, and each pipeline
//! expresses its own per-source software fallback locally (playbin
//! `force-sw-decoders` flag, uridecodebin `force-sw-decoders` property)
//! instead of mutating global state.

use gstreamer as gst;
use gstreamer::prelude::*;
use std::sync::OnceLock;

/// VA-API decoder factories that participate in the rank policy.
pub const VA_DECODER_NAMES: &[&str] = &[
    "vah264dec",
    "vah265dec",
    "vampeg2dec",
    "vavp8dec",
    "vavp9dec",
    "vaav1dec",
];

/// Original (plugin-registered) ranks, captured once before any policy is
/// applied so a later `enabled = true` restores exactly what the plugin
/// shipped (primary+1 on GStreamer >= 1.24).
fn original_ranks() -> &'static Vec<(String, gst::Rank)> {
    static RANKS: OnceLock<Vec<(String, gst::Rank)>> = OnceLock::new();
    RANKS.get_or_init(|| {
        // Idempotent; this policy is applied before the first pipeline is
        // built (i.e. possibly before anything else initialised GStreamer).
        let _ = gst::init();
        VA_DECODER_NAMES
            .iter()
            .filter_map(|name| {
                gst::ElementFactory::find(name).map(|f| ((*name).to_string(), f.rank()))
            })
            .collect()
    })
}

/// True when at least one VA-API decoder plugin is registered.
pub fn va_decoders_available() -> bool {
    !original_ranks().is_empty()
}

/// True when the GPU-resident preview path can actually run: a VA-API
/// decoder *and* `vapostproc` (GPU scale / colour conversion) are present.
/// Without `vapostproc` a hardware decoder's full-resolution frame would have
/// to be downloaded to the CPU, which measures slower than software decode.
pub fn gpu_decode_available() -> bool {
    let _ = gst::init();
    va_decoders_available() && gst::ElementFactory::find("vapostproc").is_some()
}

/// Apply the global decoder rank policy for the hardware-acceleration
/// preference.  `enabled = true` restores the plugin ranks so autoplugging
/// prefers VA-API decoders; `false` demotes them to MARGINAL so every
/// autoplugged pipeline in the process decodes in software.
///
/// Must be called on the main thread before pipelines are (re)built — ranks
/// are consulted at autoplug time, so already-built decode branches are
/// unaffected (callers tear those down themselves).
pub fn apply_va_decoder_rank_policy(enabled: bool) {
    let _ = gst::init();
    for (name, original_rank) in original_ranks() {
        if let Some(factory) = gst::ElementFactory::find(name) {
            let target = if enabled {
                *original_rank
            } else {
                gst::Rank::MARGINAL
            };
            if factory.rank() != target {
                factory.set_rank(target);
            }
        }
    }
    log::info!(
        "hw_decode_policy: VA-API decoder ranks {} ({} factories, vapostproc={})",
        if enabled { "restored" } else { "demoted" },
        original_ranks().len(),
        gst::ElementFactory::find("vapostproc").is_some()
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rank_policy_round_trips_without_panicking() {
        let _ = gst::init();
        apply_va_decoder_rank_policy(false);
        for (name, _) in original_ranks() {
            if let Some(f) = gst::ElementFactory::find(name) {
                assert_eq!(f.rank(), gst::Rank::MARGINAL);
            }
        }
        apply_va_decoder_rank_policy(true);
        for (name, rank) in original_ranks() {
            if let Some(f) = gst::ElementFactory::find(name) {
                assert_eq!(f.rank(), *rank);
            }
        }
        // Availability helpers must agree with each other.
        if gpu_decode_available() {
            assert!(va_decoders_available());
        }
    }
}
