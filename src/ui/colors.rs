//! Color constants and unpacking helpers used by UI rendering and analysis code.
//!
//! Designed to grow over time. Future home for the P2.5 named theme palette
//! (timeline backgrounds, playhead, selection, etc.).

// ITU-R BT.709 luma coefficients. Used for false-color and zebra-pattern
// displays in the program monitor; applied to gamma-corrected RGB.
pub const LUMA_R: f64 = 0.2126;
pub const LUMA_G: f64 = 0.7152;
pub const LUMA_B: f64 = 0.0722;

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
