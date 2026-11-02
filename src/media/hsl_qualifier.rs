//! HSL Qualifier — secondary color correction.
//!
//! This module provides the shared math + rendering helpers used by both the
//! Program Monitor preview (CPU pad probe on RGBA frames) and the FFmpeg
//! export / bake pipelines (a 3D LUT compiled from the same per-pixel
//! function, see `compile_hsl_qualifier_lut`). Keeping the matte math in one
//! place means preview and export stay in parity by construction.
//!
//! The workflow is:
//!
//! 1. Convert RGB → HSL.
//! 2. Run each of (hue, saturation, luminance) through a smoothstep window to
//!    get a soft [0..1] membership term.
//! 3. Multiply the three terms together (and optionally invert) to get the
//!    qualifier's alpha for this pixel.
//! 4. Apply a secondary brightness/contrast/saturation grade to the pixel and
//!    blend back toward the original by the qualifier alpha.

use crate::media::cube_lut::CubeLut;
use crate::model::clip::HslQualifier;
use std::path::{Path, PathBuf};

/// Classic `smoothstep` — 0 below edge0, 1 above edge1, cubic hermite in between.
#[inline]
fn smoothstep(edge0: f64, edge1: f64, x: f64) -> f64 {
    if edge1 <= edge0 {
        return if x >= edge0 { 1.0 } else { 0.0 };
    }
    let t = ((x - edge0) / (edge1 - edge0)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// Convert an 0..1 RGB triple to HSL.
///
/// Returns (hue in degrees 0..360, saturation 0..1, luminance 0..1).
pub fn rgb_to_hsl(r: f64, g: f64, b: f64) -> (f64, f64, f64) {
    let max = r.max(g).max(b);
    let min = r.min(g).min(b);
    let l = (max + min) * 0.5;
    let delta = max - min;

    if delta < 1e-9 {
        return (0.0, 0.0, l);
    }

    let s = if l < 0.5 {
        delta / (max + min)
    } else {
        delta / (2.0 - max - min)
    };

    let mut h = if (max - r).abs() < 1e-9 {
        // Red is max.
        ((g - b) / delta) % 6.0
    } else if (max - g).abs() < 1e-9 {
        // Green is max.
        ((b - r) / delta) + 2.0
    } else {
        // Blue is max.
        ((r - g) / delta) + 4.0
    };
    h *= 60.0;
    if h < 0.0 {
        h += 360.0;
    }
    (h, s, l)
}

/// Convert an HSL triple back to 0..1 RGB.
pub fn hsl_to_rgb(h: f64, s: f64, l: f64) -> (f64, f64, f64) {
    if s < 1e-9 {
        return (l, l, l);
    }
    let q = if l < 0.5 {
        l * (1.0 + s)
    } else {
        l + s - l * s
    };
    let p = 2.0 * l - q;
    let hk = (h / 360.0).rem_euclid(1.0);
    let mut t = [hk + 1.0 / 3.0, hk, hk - 1.0 / 3.0];
    for v in t.iter_mut() {
        if *v < 0.0 {
            *v += 1.0;
        } else if *v > 1.0 {
            *v -= 1.0;
        }
    }
    let conv = |tc: f64| -> f64 {
        if tc < 1.0 / 6.0 {
            p + (q - p) * 6.0 * tc
        } else if tc < 0.5 {
            q
        } else if tc < 2.0 / 3.0 {
            p + (q - p) * (2.0 / 3.0 - tc) * 6.0
        } else {
            p
        }
    };
    (conv(t[0]), conv(t[1]), conv(t[2]))
}

/// Soft-window membership over a linear range [min..max] with `softness`
/// widening the smoothstep band on both sides.
///
/// Both edges are inclusive (the range is `[min..max]`, not `[min..max)`).
/// When `softness == 0` the function degenerates to a hard range test so
/// pixels sitting exactly on `max` still count as inside.
#[inline]
fn range_membership(value: f64, min: f64, max: f64, softness: f64) -> f64 {
    if min > max {
        return 0.0;
    }
    let s = softness.max(0.0);
    if s < 1e-9 {
        return if value >= min && value <= max {
            1.0
        } else {
            0.0
        };
    }
    let lo = smoothstep(min - s, min + s, value);
    let hi = 1.0 - smoothstep(max - s, max + s, value);
    (lo * hi).clamp(0.0, 1.0)
}

/// Hue-specific membership that supports wrap-around ranges
/// (e.g. `hue_min = 350`, `hue_max = 30` selects reds straddling 0°).
fn hue_membership(hue_deg: f64, min: f64, max: f64, softness: f64) -> f64 {
    // Handle a fully-open range explicitly so the neutral qualifier stays neutral.
    if min <= 0.0 && max >= 360.0 {
        return 1.0;
    }
    if (min - max).abs() < 1e-9 {
        return 0.0;
    }
    if min < max {
        return range_membership(hue_deg, min, max, softness);
    }
    // Wrap-around range [min..360] ∪ [0..max]. The inner seam at 360/0 is
    // continuous, so only the *outer* edges feather: low-side at `min`,
    // high-side at `max`.
    let s = softness.max(0.0);
    if s < 1e-9 {
        return if hue_deg >= min || hue_deg <= max {
            1.0
        } else {
            0.0
        };
    }
    let lo = smoothstep(min - s, min + s, hue_deg);
    let hi = 1.0 - smoothstep(max - s, max + s, hue_deg);
    lo.max(hi).clamp(0.0, 1.0)
}

/// Compute the alpha [0..1] that the qualifier yields for a given RGB pixel.
pub fn hsl_qualifier_alpha(q: &HslQualifier, r: f64, g: f64, b: f64) -> f64 {
    let (h, s, l) = rgb_to_hsl(r, g, b);
    let m_h = hue_membership(h, q.hue_min, q.hue_max, q.hue_softness);
    let m_s = range_membership(s, q.sat_min, q.sat_max, q.sat_softness);
    let m_l = range_membership(l, q.lum_min, q.lum_max, q.lum_softness);
    let mut alpha = (m_h * m_s * m_l).clamp(0.0, 1.0);
    if q.invert {
        alpha = 1.0 - alpha;
    }
    alpha
}

/// Apply the qualifier's secondary brightness/contrast/saturation to an RGB
/// triple. All three are "always on" — neutral defaults (brightness=0,
/// contrast=1, saturation=1) produce an identity transform.
pub fn apply_secondary_grade(q: &HslQualifier, r: f64, g: f64, b: f64) -> (f64, f64, f64) {
    // Brightness is additive in 0..1 linear space.
    let mut rr = r + q.brightness;
    let mut gg = g + q.brightness;
    let mut bb = b + q.brightness;
    // Contrast around 0.5 midpoint.
    rr = ((rr - 0.5) * q.contrast + 0.5).clamp(0.0, 1.0);
    gg = ((gg - 0.5) * q.contrast + 0.5).clamp(0.0, 1.0);
    bb = ((bb - 0.5) * q.contrast + 0.5).clamp(0.0, 1.0);
    // Saturation: shift toward luma.
    let luma = 0.2126 * rr + 0.7152 * gg + 0.0722 * bb;
    rr = (luma + (rr - luma) * q.saturation).clamp(0.0, 1.0);
    gg = (luma + (gg - luma) * q.saturation).clamp(0.0, 1.0);
    bb = (luma + (bb - luma) * q.saturation).clamp(0.0, 1.0);
    (rr, gg, bb)
}

/// CPU entry point used by the Program Monitor preview pad probe.
///
/// Walks the RGBA buffer, computes the qualifier alpha for each pixel, and
/// blends the secondary-graded sample back by that alpha. When `view_mask` is
/// true the buffer is repainted with the grayscale matte instead (debug aid).
///
/// Does nothing when the qualifier is neutral (fast path).
pub fn apply_hsl_qualifier_to_rgba_buffer(
    q: &HslQualifier,
    data: &mut [u8],
    width: usize,
    height: usize,
) {
    if q.is_neutral() {
        return;
    }

    let stride = width * 4;
    // Phase 2: keyframe sampling at pts_ns would go here.
    for y in 0..height {
        let row = y * stride;
        for x in 0..width {
            let idx = row + x * 4;
            if idx + 3 >= data.len() {
                continue;
            }
            let r = data[idx] as f64 / 255.0;
            let g = data[idx + 1] as f64 / 255.0;
            let b = data[idx + 2] as f64 / 255.0;
            let alpha = hsl_qualifier_alpha(q, r, g, b);

            if q.view_mask {
                let v = (alpha * 255.0).round().clamp(0.0, 255.0) as u8;
                data[idx] = v;
                data[idx + 1] = v;
                data[idx + 2] = v;
                continue;
            }

            if alpha <= 0.0 {
                continue;
            }
            let (nr, ng, nb) = blend_graded(q, r, g, b, alpha);
            data[idx] = (nr * 255.0).round().clamp(0.0, 255.0) as u8;
            data[idx + 1] = (ng * 255.0).round().clamp(0.0, 255.0) as u8;
            data[idx + 2] = (nb * 255.0).round().clamp(0.0, 255.0) as u8;
        }
    }
}

/// Grade `(r, g, b)` and blend the result back toward the original by `alpha`.
#[inline]
fn blend_graded(q: &HslQualifier, r: f64, g: f64, b: f64, alpha: f64) -> (f64, f64, f64) {
    let (gr, gg, gb) = apply_secondary_grade(q, r, g, b);
    (
        r * (1.0 - alpha) + gr * alpha,
        g * (1.0 - alpha) + gg * alpha,
        b * (1.0 - alpha) + gb * alpha,
    )
}

/// Apply the qualifier to a single 0..1 RGB sample: matte alpha, secondary
/// grade on the matched colour, blend back by alpha.
///
/// This is the one per-pixel definition shared by the Program Monitor pad
/// probe (`apply_hsl_qualifier_to_rgba_buffer`) and the compiled LUT the
/// export / bake pipelines use (`compile_hsl_qualifier_lut`), so preview and
/// export cannot drift apart.
pub fn apply_hsl_qualifier_rgb(q: &HslQualifier, r: f64, g: f64, b: f64) -> (f64, f64, f64) {
    let alpha = hsl_qualifier_alpha(q, r, g, b);
    if alpha <= 0.0 {
        return (r, g, b);
    }
    blend_graded(q, r, g, b, alpha)
}

/// Grid size of the compiled qualifier LUT (`HSL_LUT_SIZE³` entries).
///
/// The qualifier is a pure per-pixel colour map with no spatial term and no
/// keyframes, so a 3D LUT represents it exactly at the grid points and ffmpeg
/// interpolates between them. Pixels fully inside the window (alpha 1) and
/// fully outside it (identity) reproduce the direct math to within an 8-bit
/// rounding step; the only approximation is along the window's edge, where
/// the membership ramp is interpolated across one grid cell (1/64 of the
/// channel range at this size — about 1.5° of hue), which for a zero-softness
/// window reads as a very slight feather. The `.cube` file is ~7 MB and ffmpeg
/// loads it in well under a second; 129 would halve the edge width for an
/// 8× larger file and was not worth it.
pub const HSL_LUT_SIZE: usize = 65;

/// Upper bound on compiled LUT files kept under the cache directory; the
/// oldest are removed when a new one is written past this count.
const HSL_LUT_CACHE_MAX_FILES: usize = 24;

/// Compile the qualifier into a 3D LUT by sampling `apply_hsl_qualifier_rgb`
/// on a `size³` grid (row-major, R varying fastest — the `.cube` order).
pub fn compile_hsl_qualifier_lut(q: &HslQualifier, size: usize) -> CubeLut {
    let size = size.max(2);
    let denom = (size - 1) as f64;
    let mut data = Vec::with_capacity(size * size * size);
    for bi in 0..size {
        let b = bi as f64 / denom;
        for gi in 0..size {
            let g = gi as f64 / denom;
            for ri in 0..size {
                let r = ri as f64 / denom;
                let (nr, ng, nb) = apply_hsl_qualifier_rgb(q, r, g, b);
                data.push([
                    nr.clamp(0.0, 1.0) as f32,
                    ng.clamp(0.0, 1.0) as f32,
                    nb.clamp(0.0, 1.0) as f32,
                ]);
            }
        }
    }
    CubeLut {
        size,
        domain_min: [0.0; 3],
        domain_max: [1.0; 3],
        data,
    }
}

/// Stable identity of a compiled LUT: SHA-256 over every field that changes
/// the output plus the grid size. Doubles as the cache file name, so the same
/// qualifier on any clip, in any session, resolves to the same file.
pub fn hsl_qualifier_lut_key(q: &HslQualifier, size: usize) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(
        format!(
            "hslq-v1|{size}|{:.6}|{:.6}|{:.6}|{:.6}|{:.6}|{:.6}|{:.6}|{:.6}|{:.6}|{}|{:.6}|{:.6}|{:.6}",
            q.hue_min,
            q.hue_max,
            q.hue_softness,
            q.sat_min,
            q.sat_max,
            q.sat_softness,
            q.lum_min,
            q.lum_max,
            q.lum_softness,
            q.invert as u8,
            q.brightness,
            q.contrast,
            q.saturation,
        )
        .as_bytes(),
    );
    format!("{:x}", hasher.finalize())
}

/// `$XDG_CACHE_HOME/ultimateslice/hsl_luts/` — where compiled qualifier LUTs
/// live.
pub fn hsl_qualifier_lut_cache_dir() -> PathBuf {
    crate::media::cache_support::cache_root_dir("hsl_luts")
}

/// Return the path of the compiled `.cube` for `q` in `dir`, writing it on
/// first use (`export::build_hsl_qualifier_filter` passes the cache directory
/// from `hsl_qualifier_lut_cache_dir`, with the system temp dir as fallback).
///
/// The file is written to a temporary name and renamed into place so a
/// concurrent export never reads a half-written LUT; an existing file is
/// reused untouched apart from refreshing its modification time, which is
/// what the size-capped pruning orders on.
pub fn ensure_hsl_qualifier_lut_file_in(
    dir: &Path,
    q: &HslQualifier,
    size: usize,
) -> Result<PathBuf, String> {
    let key = hsl_qualifier_lut_key(q, size);
    let path = dir.join(format!("hslq-{key}.cube"));
    if crate::media::cache_support::file_has_content(&path.to_string_lossy()) {
        if let Ok(file) = std::fs::File::options().append(true).open(&path) {
            let _ = file.set_modified(std::time::SystemTime::now());
        }
        return Ok(path);
    }
    std::fs::create_dir_all(dir).map_err(|e| format!("failed to create {}: {e}", dir.display()))?;
    let lut = compile_hsl_qualifier_lut(q, size);
    let tmp = dir.join(format!("hslq-{key}.{}.tmp", std::process::id()));
    lut.write_cube_file(&tmp)?;
    std::fs::rename(&tmp, &path).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        format!("failed to move {} into place: {e}", path.display())
    })?;
    prune_hsl_lut_cache(dir, HSL_LUT_CACHE_MAX_FILES);
    Ok(path)
}

/// Delete the oldest `hslq-*.cube` files in `dir` beyond `keep`. Best effort;
/// errors are ignored because the cache is purely an accelerator.
fn prune_hsl_lut_cache(dir: &Path, keep: usize) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let mut files: Vec<(std::time::SystemTime, PathBuf)> = entries
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| {
            p.extension().map(|x| x == "cube").unwrap_or(false)
                && p.file_name()
                    .and_then(|n| n.to_str())
                    .map(|n| n.starts_with("hslq-"))
                    .unwrap_or(false)
        })
        .filter_map(|p| {
            let modified = std::fs::metadata(&p).and_then(|m| m.modified()).ok()?;
            Some((modified, p))
        })
        .collect();
    if files.len() <= keep {
        return;
    }
    files.sort_by(|a, b| b.0.cmp(&a.0));
    for (_, path) in files.into_iter().skip(keep) {
        let _ = std::fs::remove_file(path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn approx(a: f64, b: f64, eps: f64) -> bool {
        (a - b).abs() < eps
    }

    fn sample_qualifier() -> HslQualifier {
        HslQualifier {
            enabled: true,
            hue_min: 200.0,
            hue_max: 260.0,
            hue_softness: 10.0,
            sat_min: 0.2,
            sat_max: 1.0,
            sat_softness: 0.05,
            lum_min: 0.1,
            lum_max: 0.9,
            lum_softness: 0.05,
            brightness: 0.1,
            contrast: 1.2,
            saturation: 0.6,
            ..HslQualifier::default()
        }
    }

    /// Deterministic byte stream (LCG) so the parity sweep is reproducible
    /// without pulling in a random-number crate.
    fn lcg_bytes(n: usize) -> Vec<u8> {
        let mut state: u64 = 0x9E37_79B9_7F4A_7C15;
        (0..n)
            .map(|_| {
                state = state
                    .wrapping_mul(6364136223846793005)
                    .wrapping_add(1442695040888963407);
                (state >> 56) as u8
            })
            .collect()
    }

    /// Max and mean absolute 8-bit error of the compiled LUT (trilinear, as
    /// ffmpeg's `lut3d=interp=trilinear` and `CubeLut::apply_rgb` both do)
    /// against the direct per-pixel math over a fixed pseudo-random sweep.
    fn lut_error(q: &HslQualifier, size: usize, pixels: usize) -> (i32, f64, f64) {
        let lut = compile_hsl_qualifier_lut(q, size);
        let bytes = lcg_bytes(pixels * 3);
        let mut max_err = 0i32;
        let mut sum = 0i64;
        let mut over_two = 0usize;
        let mut worst = (0u8, 0u8, 0u8, [0i32; 3], (0u8, 0u8, 0u8));
        for px in bytes.chunks(3) {
            let (r, g, b) = (px[0], px[1], px[2]);
            let (er, eg, eb) =
                apply_hsl_qualifier_rgb(q, r as f64 / 255.0, g as f64 / 255.0, b as f64 / 255.0);
            let expect = [
                (er * 255.0).round() as i32,
                (eg * 255.0).round() as i32,
                (eb * 255.0).round() as i32,
            ];
            let (lr, lg, lb) = lut.apply_rgb(r, g, b);
            let mut px_max = 0;
            for (e, l) in expect.iter().zip([lr as i32, lg as i32, lb as i32]) {
                let d = (e - l).abs();
                px_max = px_max.max(d);
                sum += d as i64;
            }
            if px_max > 2 {
                over_two += 1;
            }
            if px_max > max_err {
                max_err = px_max;
                worst = (r, g, b, expect, (lr, lg, lb));
            }
        }
        eprintln!(
            "lut@{size}: {over_two}/{pixels} samples differ by >2; worst in={:?} direct={:?} lut={:?}",
            (worst.0, worst.1, worst.2),
            worst.3,
            worst.4
        );
        (
            max_err,
            sum as f64 / (pixels * 3) as f64,
            over_two as f64 / pixels as f64,
        )
    }

    /// Exact-match check for a pixel that is well inside or well outside the
    /// qualifier window (the LUT only approximates along the edge).
    fn assert_lut_exact(q: &HslQualifier, lut: &CubeLut, rgb: (u8, u8, u8)) {
        let (r, g, b) = rgb;
        let (er, eg, eb) =
            apply_hsl_qualifier_rgb(q, r as f64 / 255.0, g as f64 / 255.0, b as f64 / 255.0);
        let expect = [
            (er * 255.0).round() as i32,
            (eg * 255.0).round() as i32,
            (eb * 255.0).round() as i32,
        ];
        let (lr, lg, lb) = lut.apply_rgb(r, g, b);
        let got = [lr as i32, lg as i32, lb as i32];
        for ch in 0..3 {
            assert!(
                (expect[ch] - got[ch]).abs() <= 1,
                "{rgb:?}: direct {expect:?} vs lut {got:?}"
            );
        }
    }

    #[test]
    fn compiled_lut_matches_direct_math_for_soft_qualifier() {
        let q = sample_qualifier();
        let lut = compile_hsl_qualifier_lut(&q, HSL_LUT_SIZE);
        assert_eq!(lut.data.len(), HSL_LUT_SIZE * HSL_LUT_SIZE * HSL_LUT_SIZE);
        let (max_err, mean_err, over_two) = lut_error(&q, HSL_LUT_SIZE, 20_000);
        eprintln!("soft qualifier @{HSL_LUT_SIZE}: max {max_err} mean {mean_err:.3}");
        assert!(
            mean_err < 0.1,
            "mean 8-bit error {mean_err:.3} (max {max_err})"
        );
        assert!(
            over_two < 0.01,
            "{:.2}% of samples off by >2",
            over_two * 100.0
        );
        // Deep inside the window (blue, saturated, mid luminance) and far
        // outside it (red, green, grey) the LUT is exact.
        for rgb in [
            (40, 80, 230),
            (20, 120, 240),
            (230, 40, 40),
            (40, 230, 40),
            (128, 128, 128),
        ] {
            assert_lut_exact(&q, &lut, rgb);
        }
    }

    #[test]
    fn compiled_lut_stays_close_for_hard_edged_qualifier() {
        // Zero softness is the worst case for any interpolated LUT: the
        // membership steps from 0 to 1 inside a single grid cell. Only pixels
        // right at the window edge can differ, and the sweep keeps the mean
        // error well below a single 8-bit step.
        let q = HslQualifier {
            hue_softness: 0.0,
            sat_softness: 0.0,
            lum_softness: 0.0,
            ..sample_qualifier()
        };
        let (max_err, mean_err, over_two) = lut_error(&q, HSL_LUT_SIZE, 20_000);
        eprintln!("hard qualifier @{HSL_LUT_SIZE}: max {max_err} mean {mean_err:.3}");
        assert!(
            mean_err < 0.3,
            "mean 8-bit error {mean_err:.3} (max {max_err})"
        );
        assert!(
            over_two < 0.03,
            "{:.2}% of samples off by >2",
            over_two * 100.0
        );
        let lut = compile_hsl_qualifier_lut(&q, HSL_LUT_SIZE);
        for rgb in [
            (40, 80, 230),
            (20, 120, 240),
            (230, 40, 40),
            (40, 230, 40),
            (128, 128, 128),
        ] {
            assert_lut_exact(&q, &lut, rgb);
        }
    }

    #[test]
    fn compiled_lut_grid_points_are_exact() {
        // At the grid points the LUT is the direct math by construction, so
        // inputs that land exactly on a node must round-trip bit-exact.
        let q = sample_qualifier();
        let size = 5;
        let lut = compile_hsl_qualifier_lut(&q, size);
        for (i, entry) in lut.data.iter().enumerate() {
            let ri = i % size;
            let gi = (i / size) % size;
            let bi = i / (size * size);
            let d = (size - 1) as f64;
            let (r, g, b) =
                apply_hsl_qualifier_rgb(&q, ri as f64 / d, gi as f64 / d, bi as f64 / d);
            assert!(
                approx(entry[0] as f64, r, 1e-6)
                    && approx(entry[1] as f64, g, 1e-6)
                    && approx(entry[2] as f64, b, 1e-6)
            );
        }
    }

    #[test]
    fn neutral_qualifier_compiles_to_identity() {
        let q = HslQualifier {
            enabled: true,
            ..HslQualifier::default()
        };
        let size = 9;
        let lut = compile_hsl_qualifier_lut(&q, size);
        let d = (size - 1) as f32;
        for (i, entry) in lut.data.iter().enumerate() {
            let ri = (i % size) as f32 / d;
            let gi = ((i / size) % size) as f32 / d;
            let bi = (i / (size * size)) as f32 / d;
            assert!(approx(entry[0] as f64, ri as f64, 1e-6), "r at {i}");
            assert!(approx(entry[1] as f64, gi as f64, 1e-6), "g at {i}");
            assert!(approx(entry[2] as f64, bi as f64, 1e-6), "b at {i}");
        }
    }

    #[test]
    fn lut_key_is_stable_and_parameter_sensitive() {
        let q = sample_qualifier();
        let a = hsl_qualifier_lut_key(&q, HSL_LUT_SIZE);
        let b = hsl_qualifier_lut_key(&q.clone(), HSL_LUT_SIZE);
        assert_eq!(a, b);
        assert_eq!(a.len(), 64);
        let mut q2 = q.clone();
        q2.saturation = 0.61;
        assert_ne!(a, hsl_qualifier_lut_key(&q2, HSL_LUT_SIZE));
        let mut q3 = q.clone();
        q3.invert = true;
        assert_ne!(a, hsl_qualifier_lut_key(&q3, HSL_LUT_SIZE));
        assert_ne!(a, hsl_qualifier_lut_key(&q, 33));
        // Preview-only debug state must not change the compiled output.
        let mut q4 = q.clone();
        q4.view_mask = true;
        assert_eq!(a, hsl_qualifier_lut_key(&q4, HSL_LUT_SIZE));
    }

    #[test]
    fn ensure_lut_file_writes_once_and_round_trips() {
        let dir = tempfile::tempdir().expect("tempdir");
        let q = sample_qualifier();
        let first = ensure_hsl_qualifier_lut_file_in(dir.path(), &q, 9).expect("write");
        assert!(first.exists());
        assert!(first.to_string_lossy().ends_with(".cube"));
        let second = ensure_hsl_qualifier_lut_file_in(dir.path(), &q, 9).expect("reuse");
        assert_eq!(first, second);
        let files: Vec<_> = std::fs::read_dir(dir.path()).unwrap().flatten().collect();
        assert_eq!(files.len(), 1, "no temp files or duplicates left behind");
        let parsed = CubeLut::from_file(&first).expect("parse back");
        assert_eq!(parsed.size, 9);
        // The .cube text carries six decimals, so compare within that.
        let expect = compile_hsl_qualifier_lut(&q, 9);
        assert_eq!(parsed.data.len(), expect.data.len());
        for (a, b) in parsed.data.iter().zip(&expect.data) {
            for ch in 0..3 {
                assert!(approx(a[ch] as f64, b[ch] as f64, 1e-6), "{a:?} vs {b:?}");
            }
        }
    }

    #[test]
    fn ensure_lut_file_fails_cleanly_when_dir_is_a_file() {
        let file = tempfile::NamedTempFile::new().expect("tempfile");
        let err = ensure_hsl_qualifier_lut_file_in(file.path(), &sample_qualifier(), 3)
            .expect_err("a file cannot be the cache directory");
        assert!(err.contains("failed to create"), "{err}");
    }

    #[test]
    fn lut_cache_is_capped() {
        let dir = tempfile::tempdir().expect("tempdir");
        for i in 0..(HSL_LUT_CACHE_MAX_FILES + 8) {
            let q = HslQualifier {
                brightness: 0.001 * i as f64,
                ..sample_qualifier()
            };
            ensure_hsl_qualifier_lut_file_in(dir.path(), &q, 3).expect("write");
        }
        let count = std::fs::read_dir(dir.path()).unwrap().flatten().count();
        assert!(
            count <= HSL_LUT_CACHE_MAX_FILES,
            "{count} files kept, cap is {HSL_LUT_CACHE_MAX_FILES}"
        );
    }

    #[test]
    fn rgb_hsl_round_trip_primary_colors() {
        let cases = [
            (1.0, 0.0, 0.0), // red
            (0.0, 1.0, 0.0), // green
            (0.0, 0.0, 1.0), // blue
            (1.0, 1.0, 0.0), // yellow
            (0.0, 1.0, 1.0), // cyan
            (1.0, 0.0, 1.0), // magenta
            (0.5, 0.25, 0.75),
            (0.2, 0.7, 0.3),
        ];
        for (r, g, b) in cases {
            let (h, s, l) = rgb_to_hsl(r, g, b);
            let (r2, g2, b2) = hsl_to_rgb(h, s, l);
            assert!(approx(r, r2, 1e-6), "r {r}→{r2} (h={h} s={s} l={l})");
            assert!(approx(g, g2, 1e-6), "g {g}→{g2}");
            assert!(approx(b, b2, 1e-6), "b {b}→{b2}");
        }
    }

    #[test]
    fn rgb_hsl_grayscale_has_zero_saturation() {
        let (_, s, l) = rgb_to_hsl(0.3, 0.3, 0.3);
        assert!(approx(s, 0.0, 1e-9));
        assert!(approx(l, 0.3, 1e-9));
    }

    #[test]
    fn red_is_hue_zero() {
        let (h, _, _) = rgb_to_hsl(1.0, 0.0, 0.0);
        assert!(approx(h, 0.0, 1e-9));
    }

    #[test]
    fn green_is_hue_120() {
        let (h, _, _) = rgb_to_hsl(0.0, 1.0, 0.0);
        assert!(approx(h, 120.0, 1e-6));
    }

    #[test]
    fn blue_is_hue_240() {
        let (h, _, _) = rgb_to_hsl(0.0, 0.0, 1.0);
        assert!(approx(h, 240.0, 1e-6));
    }

    #[test]
    fn full_open_qualifier_alpha_is_one() {
        let q = HslQualifier {
            enabled: true,
            ..HslQualifier::default()
        };
        for (r, g, b) in [
            (0.1, 0.2, 0.3),
            (0.9, 0.5, 0.1),
            (0.0, 0.0, 0.0),
            (1.0, 1.0, 1.0),
        ] {
            assert!(approx(hsl_qualifier_alpha(&q, r, g, b), 1.0, 1e-9));
        }
    }

    #[test]
    fn narrow_blue_qualifier_hits_blue_only() {
        let q = HslQualifier {
            enabled: true,
            hue_min: 200.0,
            hue_max: 260.0,
            hue_softness: 2.0,
            ..HslQualifier::default()
        };
        let blue = hsl_qualifier_alpha(&q, 0.0, 0.2, 1.0);
        let red = hsl_qualifier_alpha(&q, 1.0, 0.0, 0.0);
        let green = hsl_qualifier_alpha(&q, 0.0, 1.0, 0.0);
        assert!(blue > 0.8, "blue membership {blue}");
        assert!(red < 0.1, "red membership {red}");
        assert!(green < 0.1, "green membership {green}");
    }

    #[test]
    fn wraparound_red_qualifier_handles_hue_0() {
        let q = HslQualifier {
            enabled: true,
            hue_min: 340.0,
            hue_max: 20.0,
            hue_softness: 5.0,
            ..HslQualifier::default()
        };
        // Pure red is exactly at 0° — should match.
        let red = hsl_qualifier_alpha(&q, 1.0, 0.0, 0.0);
        // Something near 350° should also match.
        let redish = hsl_qualifier_alpha(&q, 1.0, 0.0, 0.2);
        let green = hsl_qualifier_alpha(&q, 0.0, 1.0, 0.0);
        assert!(red > 0.8, "pure red {red}");
        assert!(redish > 0.5, "redish {redish}");
        assert!(green < 0.1, "green {green}");
    }

    #[test]
    fn invert_flips_membership() {
        let q = HslQualifier {
            enabled: true,
            hue_min: 200.0,
            hue_max: 260.0,
            invert: true,
            ..HslQualifier::default()
        };
        let blue = hsl_qualifier_alpha(&q, 0.0, 0.0, 1.0);
        let red = hsl_qualifier_alpha(&q, 1.0, 0.0, 0.0);
        assert!(blue < 0.1, "inverted blue {blue}");
        assert!(red > 0.8, "inverted red {red}");
    }

    #[test]
    fn neutral_detection() {
        assert!(HslQualifier::default().is_neutral());
        let mut q = HslQualifier::default();
        q.enabled = true;
        assert!(q.is_neutral(), "full-open + neutral grade");
        q.brightness = 0.1;
        assert!(!q.is_neutral(), "non-zero brightness");
        q.brightness = 0.0;
        q.hue_max = 100.0;
        assert!(!q.is_neutral(), "narrowed hue range");
        q = HslQualifier::default();
        q.enabled = true;
        q.view_mask = true;
        assert!(!q.is_neutral(), "view_mask forces active");
    }

    #[test]
    fn secondary_grade_neutral_is_identity() {
        let q = HslQualifier {
            enabled: true,
            ..HslQualifier::default()
        };
        let (r, g, b) = apply_secondary_grade(&q, 0.3, 0.6, 0.9);
        assert!(approx(r, 0.3, 1e-9));
        assert!(approx(g, 0.6, 1e-9));
        assert!(approx(b, 0.9, 1e-9));
    }

    #[test]
    fn secondary_grade_brightness_adds() {
        let mut q = HslQualifier::default();
        q.enabled = true;
        q.brightness = 0.2;
        let (r, _, _) = apply_secondary_grade(&q, 0.3, 0.3, 0.3);
        assert!(approx(r, 0.5, 1e-9));
    }

    #[test]
    fn rgba_buffer_neutral_qualifier_is_noop() {
        let q = HslQualifier::default();
        let mut buf = vec![
            10u8, 20, 30, 255, //
            200, 100, 50, 255, //
            0, 0, 255, 255, //
            128, 64, 32, 200,
        ];
        let before = buf.clone();
        apply_hsl_qualifier_to_rgba_buffer(&q, &mut buf, 2, 2);
        assert_eq!(buf, before);
    }

    #[test]
    fn rgba_buffer_view_mask_writes_grayscale() {
        let mut q = HslQualifier::default();
        q.enabled = true;
        q.view_mask = true;
        // A 1x1 image of red.
        let mut buf = vec![255u8, 0, 0, 255];
        apply_hsl_qualifier_to_rgba_buffer(&q, &mut buf, 1, 1);
        // Full-open qualifier → alpha 1.0 → grayscale 255 written to RGB.
        assert_eq!(buf[0], 255);
        assert_eq!(buf[1], 255);
        assert_eq!(buf[2], 255);
        // Alpha channel untouched.
        assert_eq!(buf[3], 255);
    }

    #[test]
    fn rgba_buffer_blends_graded_pixel() {
        let mut q = HslQualifier::default();
        q.enabled = true;
        q.brightness = 0.5;
        let mut buf = vec![0u8, 0, 0, 255];
        apply_hsl_qualifier_to_rgba_buffer(&q, &mut buf, 1, 1);
        // Full-open qualifier with +0.5 brightness → pixel should move toward 0.5 (~128).
        assert!(buf[0] >= 120 && buf[0] <= 135, "r={}", buf[0]);
    }

    #[test]
    fn serde_round_trip_preserves_all_fields() {
        let q = HslQualifier {
            enabled: true,
            hue_min: 15.0,
            hue_max: 85.0,
            hue_softness: 3.0,
            sat_min: 0.3,
            sat_max: 0.95,
            sat_softness: 0.05,
            lum_min: 0.1,
            lum_max: 0.7,
            lum_softness: 0.02,
            invert: true,
            view_mask: true, // should be dropped on serialize
            brightness: -0.1,
            contrast: 1.25,
            saturation: 0.8,
        };
        let s = serde_json::to_string(&q).unwrap();
        assert!(!s.contains("view_mask"), "view_mask must not persist: {s}");
        let back: HslQualifier = serde_json::from_str(&s).unwrap();
        assert_eq!(back.enabled, q.enabled);
        assert!(approx(back.hue_min, q.hue_min, 1e-9));
        assert!(approx(back.hue_max, q.hue_max, 1e-9));
        assert!(approx(back.sat_min, q.sat_min, 1e-9));
        assert!(approx(back.sat_max, q.sat_max, 1e-9));
        assert!(approx(back.lum_min, q.lum_min, 1e-9));
        assert!(approx(back.lum_max, q.lum_max, 1e-9));
        assert_eq!(back.invert, q.invert);
        // view_mask is skipped on serialize and defaults to false on parse.
        assert!(!back.view_mask);
        assert!(approx(back.brightness, q.brightness, 1e-9));
        assert!(approx(back.contrast, q.contrast, 1e-9));
        assert!(approx(back.saturation, q.saturation, 1e-9));
    }
}
