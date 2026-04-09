//! Color constants used by UI rendering and analysis code.
//!
//! Designed to grow over time. Future homes for items currently scattered
//! across the codebase:
//! - P1.6: `color_u32_to_rgba(u32) -> (f64, f64, f64, f64)` helper
//! - P2.5: named theme palette (timeline backgrounds, playhead, selection, etc.)

// ITU-R BT.709 luma coefficients. Used for false-color and zebra-pattern
// displays in the program monitor; applied to gamma-corrected RGB.
pub const LUMA_R: f64 = 0.2126;
pub const LUMA_G: f64 = 0.7152;
pub const LUMA_B: f64 = 0.0722;
