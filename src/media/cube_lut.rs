//! Parser and applicator for .cube 3D LUT files.
//!
//! Supports the standard Adobe/Resolve .cube format:
//! - `TITLE` (ignored)
//! - `DOMAIN_MIN` / `DOMAIN_MAX` (defaults 0.0 / 1.0)
//! - `LUT_3D_SIZE` N (required; N×N×N entries)
//! - N³ lines of `R G B` float triplets in [0,1]
//!
//! The lookup table is stored as a flat `Vec<[f32; 3]>` in row-major order
//! (R varies fastest, then G, then B). Trilinear interpolation is used for
//! sub-grid lookups when applying the LUT to 8-bit RGBA pixel buffers.

use std::fs;
use std::path::Path;

/// A parsed 3D LUT.
#[derive(Debug, Clone)]
pub struct CubeLut {
    /// Grid size per axis (the LUT has `size³` entries).
    pub size: usize,
    /// Domain minimum per channel (usually [0, 0, 0]).
    pub domain_min: [f32; 3],
    /// Domain maximum per channel (usually [1, 1, 1]).
    pub domain_max: [f32; 3],
    /// Flat row-major table: index = r + g*size + b*size*size.
    pub data: Vec<[f32; 3]>,
}

impl CubeLut {
    /// Parse a `.cube` file from disk.
    pub fn from_file(path: &Path) -> Result<Self, String> {
        let contents = fs::read_to_string(path)
            .map_err(|e| format!("Failed to read {}: {}", path.display(), e))?;
        Self::parse(&contents)
    }

    /// Parse `.cube` text content.
    pub fn parse(text: &str) -> Result<Self, String> {
        let mut size: Option<usize> = None;
        let mut domain_min = [0.0_f32; 3];
        let mut domain_max = [1.0_f32; 3];
        let mut data: Vec<[f32; 3]> = Vec::new();

        for line in text.lines() {
            let trimmed = line.trim();
            // Skip empty lines and comments.
            if trimmed.is_empty() || trimmed.starts_with('#') {
                continue;
            }
            if trimmed.starts_with("TITLE") {
                continue;
            }
            if let Some(rest) = trimmed.strip_prefix("DOMAIN_MIN") {
                let vals: Vec<f32> = rest
                    .split_whitespace()
                    .filter_map(|s| s.parse().ok())
                    .collect();
                if vals.len() == 3 {
                    domain_min = [vals[0], vals[1], vals[2]];
                }
                continue;
            }
            if let Some(rest) = trimmed.strip_prefix("DOMAIN_MAX") {
                let vals: Vec<f32> = rest
                    .split_whitespace()
                    .filter_map(|s| s.parse().ok())
                    .collect();
                if vals.len() == 3 {
                    domain_max = [vals[0], vals[1], vals[2]];
                }
                continue;
            }
            if let Some(rest) = trimmed.strip_prefix("LUT_3D_SIZE") {
                size = rest.trim().parse().ok();
                if let Some(s) = size {
                    data.reserve(s * s * s);
                }
                continue;
            }
            // Skip any other keyword lines (e.g. LUT_1D_SIZE, LUT_1D_INPUT_RANGE).
            if trimmed
                .chars()
                .next()
                .map_or(true, |c| c.is_ascii_alphabetic())
            {
                // If we haven't started reading data yet, treat as keyword.
                if data.is_empty() {
                    continue;
                }
            }
            // Data line: three floats.
            let parts: Vec<&str> = trimmed.split_whitespace().collect();
            if parts.len() >= 3 {
                if let (Ok(r), Ok(g), Ok(b)) = (
                    parts[0].parse::<f32>(),
                    parts[1].parse::<f32>(),
                    parts[2].parse::<f32>(),
                ) {
                    data.push([r, g, b]);
                }
            }
        }

        let size = size.ok_or_else(|| "LUT_3D_SIZE not found in .cube file".to_string())?;
        let expected = size * size * size;
        if data.len() != expected {
            return Err(format!(
                "Expected {} LUT entries for size {}, got {}",
                expected,
                size,
                data.len()
            ));
        }
        Ok(CubeLut {
            size,
            domain_min,
            domain_max,
            data,
        })
    }

    /// Look up an index in the flat table: `data[r + g*size + b*size*size]`.
    #[inline(always)]
    fn lookup(&self, ri: usize, gi: usize, bi: usize) -> [f32; 3] {
        self.data[ri + gi * self.size + bi * self.size * self.size]
    }

    /// Trilinear interpolation for a single pixel (0–255 per channel).
    /// Returns clamped 0–255 output.
    #[inline]
    fn interpolate(&self, r: u8, g: u8, b: u8) -> (u8, u8, u8) {
        let n = self.size as f32 - 1.0;
        // Normalise 0–255 → domain range → 0–(size-1) grid coords.
        let fr = ((r as f32 / 255.0 - self.domain_min[0])
            / (self.domain_max[0] - self.domain_min[0]))
            .clamp(0.0, 1.0)
            * n;
        let fg = ((g as f32 / 255.0 - self.domain_min[1])
            / (self.domain_max[1] - self.domain_min[1]))
            .clamp(0.0, 1.0)
            * n;
        let fb = ((b as f32 / 255.0 - self.domain_min[2])
            / (self.domain_max[2] - self.domain_min[2]))
            .clamp(0.0, 1.0)
            * n;

        let r0 = (fr as usize).min(self.size - 2);
        let g0 = (fg as usize).min(self.size - 2);
        let b0 = (fb as usize).min(self.size - 2);
        let r1 = r0 + 1;
        let g1 = g0 + 1;
        let b1 = b0 + 1;

        let dr = fr - r0 as f32;
        let dg = fg - g0 as f32;
        let db = fb - b0 as f32;

        // Trilinear: 8 corner lookups.
        let c000 = self.lookup(r0, g0, b0);
        let c100 = self.lookup(r1, g0, b0);
        let c010 = self.lookup(r0, g1, b0);
        let c110 = self.lookup(r1, g1, b0);
        let c001 = self.lookup(r0, g0, b1);
        let c101 = self.lookup(r1, g0, b1);
        let c011 = self.lookup(r0, g1, b1);
        let c111 = self.lookup(r1, g1, b1);

        let mut out = [0.0_f32; 3];
        for ch in 0..3 {
            let c00 = c000[ch] * (1.0 - dr) + c100[ch] * dr;
            let c01 = c001[ch] * (1.0 - dr) + c101[ch] * dr;
            let c10 = c010[ch] * (1.0 - dr) + c110[ch] * dr;
            let c11 = c011[ch] * (1.0 - dr) + c111[ch] * dr;
            let c0 = c00 * (1.0 - dg) + c10 * dg;
            let c1 = c01 * (1.0 - dg) + c11 * dg;
            out[ch] = c0 * (1.0 - db) + c1 * db;
        }

        (
            (out[0] * 255.0 + 0.5).clamp(0.0, 255.0) as u8,
            (out[1] * 255.0 + 0.5).clamp(0.0, 255.0) as u8,
            (out[2] * 255.0 + 0.5).clamp(0.0, 255.0) as u8,
        )
    }

    /// Apply this LUT to an RGBA pixel buffer in-place.
    /// `data` must have length divisible by 4 (R, G, B, A bytes).
    /// Alpha channel is preserved unchanged.
    pub fn apply_to_rgba_buffer(&self, data: &mut [u8]) {
        debug_assert!(
            data.len() % 4 == 0,
            "RGBA buffer length must be multiple of 4"
        );
        for pixel in data.chunks_exact_mut(4) {
            let (r_out, g_out, b_out) = self.interpolate(pixel[0], pixel[1], pixel[2]);
            pixel[0] = r_out;
            pixel[1] = g_out;
            pixel[2] = b_out;
            // pixel[3] (alpha) unchanged.
        }
    }

    /// Apply this LUT to a single RGB pixel and return the transformed values.
    pub fn apply_rgb(&self, r: u8, g: u8, b: u8) -> (u8, u8, u8) {
        self.interpolate(r, g, b)
    }

    /// Write this LUT to a `.cube` file at the given path.
    pub fn write_cube_file(&self, path: &Path) -> Result<(), String> {
        use std::fmt::Write as FmtWrite;

        let n = self.size;
        let mut buf = String::with_capacity(n * n * n * 30 + 128);
        writeln!(buf, "# Generated by UltimateSlice color match").unwrap();
        writeln!(buf, "LUT_3D_SIZE {n}").unwrap();
        if self.domain_min != [0.0, 0.0, 0.0] {
            writeln!(
                buf,
                "DOMAIN_MIN {:.6} {:.6} {:.6}",
                self.domain_min[0], self.domain_min[1], self.domain_min[2]
            )
            .unwrap();
        }
        if self.domain_max != [1.0, 1.0, 1.0] {
            writeln!(
                buf,
                "DOMAIN_MAX {:.6} {:.6} {:.6}",
                self.domain_max[0], self.domain_max[1], self.domain_max[2]
            )
            .unwrap();
        }
        for entry in &self.data {
            writeln!(buf, "{:.6} {:.6} {:.6}", entry[0], entry[1], entry[2]).unwrap();
        }
        fs::write(path, buf).map_err(|e| format!("failed to write {}: {e}", path.display()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn identity_cube(size: usize) -> String {
        let mut lines = format!("LUT_3D_SIZE {}\n", size);
        let n = size as f32 - 1.0;
        for b in 0..size {
            for g in 0..size {
                for r in 0..size {
                    lines.push_str(&format!(
                        "{:.6} {:.6} {:.6}\n",
                        r as f32 / n,
                        g as f32 / n,
                        b as f32 / n
                    ));
                }
            }
        }
        lines
    }

    #[test]
    fn parse_identity_lut() {
        let text = identity_cube(17);
        let lut = CubeLut::parse(&text).unwrap();
        assert_eq!(lut.size, 17);
        assert_eq!(lut.data.len(), 17 * 17 * 17);
        assert_eq!(lut.domain_min, [0.0, 0.0, 0.0]);
        assert_eq!(lut.domain_max, [1.0, 1.0, 1.0]);
    }

    #[test]
    fn parse_with_comments_and_title() {
        let text = "# Comment line\nTITLE \"My Cool LUT\"\nLUT_3D_SIZE 2\n\
                     0.0 0.0 0.0\n1.0 0.0 0.0\n0.0 1.0 0.0\n1.0 1.0 0.0\n\
                     0.0 0.0 1.0\n1.0 0.0 1.0\n0.0 1.0 1.0\n1.0 1.0 1.0\n";
        let lut = CubeLut::parse(text).unwrap();
        assert_eq!(lut.size, 2);
        assert_eq!(lut.data.len(), 8);
    }

    #[test]
    fn parse_custom_domain() {
        let text = "LUT_3D_SIZE 2\nDOMAIN_MIN 0.0 0.0 0.0\nDOMAIN_MAX 0.5 0.5 0.5\n\
                     0.0 0.0 0.0\n0.5 0.0 0.0\n0.0 0.5 0.0\n0.5 0.5 0.0\n\
                     0.0 0.0 0.5\n0.5 0.0 0.5\n0.0 0.5 0.5\n0.5 0.5 0.5\n";
        let lut = CubeLut::parse(text).unwrap();
        assert_eq!(lut.domain_max, [0.5, 0.5, 0.5]);
    }

    #[test]
    fn parse_error_missing_size() {
        let text = "0.0 0.0 0.0\n1.0 1.0 1.0\n";
        assert!(CubeLut::parse(text).is_err());
    }

    #[test]
    fn parse_error_wrong_count() {
        let text = "LUT_3D_SIZE 2\n0.0 0.0 0.0\n1.0 0.0 0.0\n";
        assert!(CubeLut::parse(text).is_err());
    }

    #[test]
    fn identity_lut_passthrough() {
        let text = identity_cube(33);
        let lut = CubeLut::parse(&text).unwrap();
        // Test several known pixel values through the identity LUT.
        for val in [0u8, 64, 128, 192, 255] {
            let (r, g, b) = lut.interpolate(val, val, val);
            assert!(
                (r as i16 - val as i16).unsigned_abs() <= 1,
                "Identity LUT: expected ~{val}, got r={r}"
            );
            assert!(
                (g as i16 - val as i16).unsigned_abs() <= 1,
                "Identity LUT: expected ~{val}, got g={g}"
            );
            assert!(
                (b as i16 - val as i16).unsigned_abs() <= 1,
                "Identity LUT: expected ~{val}, got b={b}"
            );
        }
    }

    #[test]
    fn apply_to_rgba_preserves_alpha() {
        let text = identity_cube(17);
        let lut = CubeLut::parse(&text).unwrap();
        // 2 pixels: (100,150,200,42) and (50,60,70,255)
        let mut buf = vec![100u8, 150, 200, 42, 50, 60, 70, 255];
        lut.apply_to_rgba_buffer(&mut buf);
        // Identity LUT: RGB should be ~unchanged (±1 for rounding).
        assert!((buf[0] as i16 - 100).unsigned_abs() <= 1);
        assert!((buf[1] as i16 - 150).unsigned_abs() <= 1);
        assert!((buf[2] as i16 - 200).unsigned_abs() <= 1);
        assert_eq!(buf[3], 42, "Alpha must be preserved");
        assert!((buf[4] as i16 - 50).unsigned_abs() <= 1);
        assert!((buf[5] as i16 - 60).unsigned_abs() <= 1);
        assert!((buf[6] as i16 - 70).unsigned_abs() <= 1);
        assert_eq!(buf[7], 255, "Alpha must be preserved");
    }

    #[test]
    fn known_transform_lut() {
        // Build a simple 2×2×2 LUT that inverts all channels.
        // Input grid corners map to inverted output.
        let text = "LUT_3D_SIZE 2\n\
                     1.0 1.0 1.0\n0.0 1.0 1.0\n\
                     1.0 0.0 1.0\n0.0 0.0 1.0\n\
                     1.0 1.0 0.0\n0.0 1.0 0.0\n\
                     1.0 0.0 0.0\n0.0 0.0 0.0\n";
        let lut = CubeLut::parse(text).unwrap();
        // Black (0,0,0) → first entry (1,1,1) → (255,255,255)
        let (r, g, b) = lut.interpolate(0, 0, 0);
        assert_eq!((r, g, b), (255, 255, 255));
        // White (255,255,255) → last entry (0,0,0) → (0,0,0)
        let (r, g, b) = lut.interpolate(255, 255, 255);
        assert_eq!((r, g, b), (0, 0, 0));
        // Mid grey (128,128,128) → ~(128,128,128) due to linear inversion through midpoint
        let (r, g, b) = lut.interpolate(128, 128, 128);
        assert!(
            (r as i16 - 127).unsigned_abs() <= 1,
            "Invert LUT midpoint: got r={r}"
        );
    }

    #[test]
    fn parse_large_lut() {
        // Ensure 33×33×33 = 35937 entries parse correctly.
        let text = identity_cube(33);
        let lut = CubeLut::parse(&text).unwrap();
        assert_eq!(lut.size, 33);
        assert_eq!(lut.data.len(), 35937);
    }
}
