//! Centralized clamp ranges for clip transform properties.
//!
//! These constants are the **single source of truth** for the inspector
//! sliders, the model-level keyframe clamp, the runtime evaluators in
//! `media::program_player`, the export keyframe evaluators in
//! `media::export`, the transform-overlay drag handlers, the MCP
//! `set_clip_transform` handler, and the tracker resolution layer.
//!
//! When a bound changes here, **every** consumer picks it up automatically
//! — no more grepping for `4000.0` / `-3.0` / `0.1, 4.0` to find every
//! call site that silently truncates values past the new bound.
//!
//! ## Why these values
//!
//! - **Crop** (`CROP_MIN_PX`..`CROP_MAX_PX`): pixels stripped from each
//!   edge.  4000 px covers 4K projects (3840 wide) with headroom; the
//!   transform overlay also clamps each edge against the live project
//!   resolution at runtime so opposing crops can never exceed the actual
//!   frame width/height.
//!
//! - **Position** (`POSITION_MIN`..`POSITION_MAX`): normalized X/Y offset
//!   where ±1.0 puts the clip's edge at the canvas edge.  Values past
//!   ±1.0 push the clip off-canvas; the rendering pipeline (preview
//!   compositor + export ffmpeg graph) handles the overflow by
//!   cropping/padding past the frame edges.  ±3.0 covers the
//!   "swing fully off-screen" use case at typical scales.
//!
//! - **Scale** (`SCALE_MIN`..`SCALE_MAX`): zoom multiplier.  0.1 = 10 %,
//!   4.0 = 4× zoom in.
//!
//! - **Rotate** (`ROTATE_MIN_DEG`..`ROTATE_MAX_DEG`): rotation in
//!   degrees.  ±180° spans a full turn.
//!
//! - **Opacity** (`OPACITY_MIN`..`OPACITY_MAX`): linear alpha 0..1.
//!
//! ## Adjustment-scope position bounds
//!
//! Adjustment layers and adjustment-scope positions intentionally **do
//! not** use the relaxed `POSITION_MIN`/`POSITION_MAX` bounds, because
//! moving an adjustment region off-canvas has no useful semantic — the
//! region is a rectangle on the visible frame.  Those call sites still
//! clamp to the tighter `ADJUSTMENT_POSITION_MIN`/`ADJUSTMENT_POSITION_MAX`
//! pair below.

// ── Crop (project pixels) ────────────────────────────────────────────────

/// Minimum number of pixels that can be cropped from any clip edge.
pub const CROP_MIN_PX: f64 = 0.0;

/// Maximum number of pixels that can be cropped from any clip edge.
///
/// Bumped from the original 500 px ceiling so 4K projects (3840 px wide)
/// can be cropped most of the way down without hitting the slider top.
/// The transform overlay drag handler additionally clamps each crop
/// against the live project resolution at runtime, so opposing crops can
/// never exceed the actual frame width/height.
pub const CROP_MAX_PX: f64 = 4000.0;

/// Integer view of [`CROP_MAX_PX`] for `i32`-based clamps (the transform
/// overlay's `CROP_MAX` constant and the slot crop-state struct).
pub const CROP_MAX_PX_I32: i32 = CROP_MAX_PX as i32;

// ── Position (normalized) ────────────────────────────────────────────────

/// Minimum horizontal/vertical normalized position.
///
/// `−1.0` puts the clip's left/top edge at the canvas left/top edge.
/// Values past `−1.0` push the clip off-canvas; the rendering pipeline
/// (`apply_zoom_to_slot` and the export `build_scale_translate_filter`)
/// handles the overflow by cropping/padding past the frame edges.
pub const POSITION_MIN: f64 = -3.0;

/// Maximum horizontal/vertical normalized position.  See
/// [`POSITION_MIN`] for the semantics of values past ±1.0.
pub const POSITION_MAX: f64 = 3.0;

/// Tighter position bound for adjustment-scope rectangles, where moving
/// the region off-canvas has no useful semantic.
pub const ADJUSTMENT_POSITION_MIN: f64 = -1.0;

/// Tighter position bound for adjustment-scope rectangles.  See
/// [`ADJUSTMENT_POSITION_MIN`].
pub const ADJUSTMENT_POSITION_MAX: f64 = 1.0;

// ── Scale (zoom multiplier) ──────────────────────────────────────────────

/// Minimum zoom multiplier.  Values below 0.1 collapse the clip to a
/// pixel-fraction that the GStreamer caps negotiator rejects.
pub const SCALE_MIN: f64 = 0.1;

/// Maximum zoom multiplier.  4× covers most practical zoom-in use cases
/// without forcing the compositor to allocate enormous internal buffers.
pub const SCALE_MAX: f64 = 4.0;

// ── Rotate (degrees) ─────────────────────────────────────────────────────

/// Minimum rotation in degrees.
pub const ROTATE_MIN_DEG: f64 = -180.0;

/// Maximum rotation in degrees.
pub const ROTATE_MAX_DEG: f64 = 180.0;

/// Integer view of [`ROTATE_MIN_DEG`] for `i32`-based clamps.
pub const ROTATE_MIN_DEG_I32: i32 = ROTATE_MIN_DEG as i32;

/// Integer view of [`ROTATE_MAX_DEG`] for `i32`-based clamps.
pub const ROTATE_MAX_DEG_I32: i32 = ROTATE_MAX_DEG as i32;

// ── Opacity (linear alpha) ───────────────────────────────────────────────

/// Minimum opacity (fully transparent).
pub const OPACITY_MIN: f64 = 0.0;

/// Maximum opacity (fully opaque).
pub const OPACITY_MAX: f64 = 1.0;
