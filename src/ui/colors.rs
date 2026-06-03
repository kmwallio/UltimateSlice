//! Color constants and unpacking helpers used by UI rendering and analysis code.
//!
//! The theme palette section below is the single source of truth for the
//! named UI colors that previously lived as inline `set_source_rgb*` tuples
//! in the timeline and program-monitor draw functions. Adding a light theme
//! later (or letting users theme these) becomes a one-place edit.

// ─── ITU-R BT.709 luma coefficients ───────────────────────────────────────
//
// Used for false-color and zebra-pattern displays in the program monitor;
// applied to gamma-corrected RGB.
pub const LUMA_R: f64 = 0.2126;
pub const LUMA_G: f64 = 0.7152;
pub const LUMA_B: f64 = 0.0722;

// ─── Theme palette (P2.5) ─────────────────────────────────────────────────
//
// Each constant is the exact tuple that was previously inline at the
// listed call site(s). When in doubt about a value: it shipped this way.
// Tuples are `(r, g, b)` for opaque colors, `(r, g, b, a)` for translucent.

// ─── Runtime accent (selection highlight) ─────────────────────────────────
//
// The timeline clip-selection highlight follows the user's chosen accent color
// (Preferences → Appearance). Unlike the CSS chrome, these are Cairo-drawn, so
// the accent is stored here as a normalized RGB triple and derived into the
// fill/border tuples on demand. Defaults to the historical selection blue
// (accent `#1565c0`, lightened) until `set_selection_accent` is called at
// startup / on preference change.

use std::cell::Cell;

thread_local! {
    /// Normalized `(r, g, b)` of the current accent color. Default = `#1565c0`.
    static SELECTION_ACCENT: Cell<(f64, f64, f64)> =
        const { Cell::new((0.0823, 0.3960, 0.7529)) };
}

/// Mix `color` toward white by `t` in `[0,1]` (0 = unchanged, 1 = white).
#[inline]
fn lighten(color: (f64, f64, f64), t: f64) -> (f64, f64, f64) {
    (
        color.0 + (1.0 - color.0) * t,
        color.1 + (1.0 - color.1) * t,
        color.2 + (1.0 - color.2) * t,
    )
}

/// Parse a `#rrggbb` (or `#rgb`) hex string into normalized `(r, g, b)` in
/// `[0,1]`. Returns `None` for malformed input.
pub fn parse_hex_rgb(hex: &str) -> Option<(f64, f64, f64)> {
    let h = hex.trim().strip_prefix('#').unwrap_or(hex.trim());
    let (r, g, b) = match h.len() {
        6 => (
            u8::from_str_radix(&h[0..2], 16).ok()?,
            u8::from_str_radix(&h[2..4], 16).ok()?,
            u8::from_str_radix(&h[4..6], 16).ok()?,
        ),
        3 => {
            let dup = |c: &str| u8::from_str_radix(&c.repeat(2), 16).ok();
            (dup(&h[0..1])?, dup(&h[1..2])?, dup(&h[2..3])?)
        }
        _ => return None,
    };
    Some((r as f64 / 255.0, g as f64 / 255.0, b as f64 / 255.0))
}

/// Set the accent color used for the timeline clip-selection highlight from a
/// `#rrggbb` hex string. No-op on malformed input.
pub fn set_selection_accent(hex: &str) {
    if let Some(rgb) = parse_hex_rgb(hex) {
        SELECTION_ACCENT.with(|a| a.set(rgb));
    }
}

/// Selection-highlight fill (used by both selected-track highlight and
/// razor-tool track highlight in the timeline). Very low alpha so the
/// underlying clips remain readable. Derived from the current accent.
pub fn selection_fill() -> (f64, f64, f64, f64) {
    let (r, g, b) = SELECTION_ACCENT.with(|a| lighten(a.get(), 0.40));
    (r, g, b, 0.08)
}

/// Selection-highlight border (drawn around the same rectangle as
/// [`selection_fill`]). Derived from the current accent.
pub fn selection_border() -> (f64, f64, f64, f64) {
    let (r, g, b) = SELECTION_ACCENT.with(|a| lighten(a.get(), 0.40));
    (r, g, b, 0.85)
}

/// Audio level meter — "safe" zone (below −18 dBFS). Used by both the
/// timeline track meters and the program-monitor master meters.
pub const COLOR_LEVEL_GOOD: (f64, f64, f64) = (0.20, 0.80, 0.20);

/// Audio level meter — "warning" zone (−18 to −6 dBFS).
pub const COLOR_LEVEL_WARN: (f64, f64, f64) = (0.90, 0.85, 0.10);

/// Audio level meter — "clipping" zone (above −6 dBFS).
pub const COLOR_LEVEL_CLIP: (f64, f64, f64) = (0.90, 0.20, 0.10);

/// Audio role label color — Dialogue.
pub const COLOR_AUDIO_DIALOGUE: (f64, f64, f64) = (0.90, 0.70, 0.30);

/// Audio role label color — Effects.
pub const COLOR_AUDIO_EFFECTS: (f64, f64, f64) = (0.30, 0.80, 0.90);

/// Audio role label color — Music.
pub const COLOR_AUDIO_MUSIC: (f64, f64, f64) = (0.40, 0.90, 0.50);

/// Audio role label color — fallback for unknown / unset roles.
pub const COLOR_AUDIO_ROLE_NONE: (f64, f64, f64) = (0.50, 0.50, 0.50);

/// Timeline canvas background (the dark area behind tracks).
pub const COLOR_TIMELINE_BG: (f64, f64, f64) = (0.13, 0.13, 0.15);

/// Track-label / left-gutter background panel.
pub const COLOR_TRACK_LABEL_BG: (f64, f64, f64) = (0.25, 0.25, 0.28);

/// Unpack a packed RGBA `u32` (`0xRRGGBBAA`) into `(r, g, b, a)` byte channels.
///
/// Use this for code that emits ASS/`drawtext` colour strings or otherwise
/// needs the raw byte values. For Cairo `set_source_rgb*` callers prefer
/// [`rgba_u32_to_f64`] which normalizes to `[0.0, 1.0]`.
#[inline]
pub fn rgba_u32_to_u8(color: u32) -> (u8, u8, u8, u8) {
    (
        ((color >> 24) & 0xFF) as u8,
        ((color >> 16) & 0xFF) as u8,
        ((color >> 8) & 0xFF) as u8,
        (color & 0xFF) as u8,
    )
}

/// Unpack a packed RGBA `u32` (`0xRRGGBBAA`) into normalized `f64` channels in
/// `[0.0, 1.0]`. Suitable for Cairo `set_source_rgb` / `set_source_rgba`.
#[inline]
pub fn rgba_u32_to_f64(color: u32) -> (f64, f64, f64, f64) {
    let (r, g, b, a) = rgba_u32_to_u8(color);
    (
        r as f64 / 255.0,
        g as f64 / 255.0,
        b as f64 / 255.0,
        a as f64 / 255.0,
    )
}

/// Unpack a packed RGBA `u32` into normalized `f32` channels in `[0.0, 1.0]`.
/// Suitable for `gdk4::RGBA::new(r, g, b, a)`.
#[inline]
pub fn rgba_u32_to_f32(color: u32) -> (f32, f32, f32, f32) {
    let (r, g, b, a) = rgba_u32_to_u8(color);
    (
        r as f32 / 255.0,
        g as f32 / 255.0,
        b as f32 / 255.0,
        a as f32 / 255.0,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rgba_u32_to_u8_unpacks_channels() {
        assert_eq!(rgba_u32_to_u8(0xFF8040C0), (0xFF, 0x80, 0x40, 0xC0));
        assert_eq!(rgba_u32_to_u8(0x00000000), (0, 0, 0, 0));
        assert_eq!(rgba_u32_to_u8(0xFFFFFFFF), (255, 255, 255, 255));
    }

    #[test]
    fn rgba_u32_to_f32_normalizes_channels() {
        let (r, g, b, a) = rgba_u32_to_f32(0xFFFFFFFF);
        assert!((r - 1.0).abs() < 1e-6);
        assert!((g - 1.0).abs() < 1e-6);
        assert!((b - 1.0).abs() < 1e-6);
        assert!((a - 1.0).abs() < 1e-6);
    }

    #[test]
    fn rgba_u32_to_f64_normalizes_channels() {
        let (r, g, b, a) = rgba_u32_to_f64(0xFFFFFFFF);
        assert!((r - 1.0).abs() < 1e-9);
        assert!((g - 1.0).abs() < 1e-9);
        assert!((b - 1.0).abs() < 1e-9);
        assert!((a - 1.0).abs() < 1e-9);
        let (r, _, _, _) = rgba_u32_to_f64(0x80000000);
        assert!((r - 128.0 / 255.0).abs() < 1e-9);
    }
}
