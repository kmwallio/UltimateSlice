//! HSL Qualifier — secondary color correction.
//!
//! This module provides the shared math + rendering helpers used by both the
//! Program Monitor preview (CPU pad probe on RGBA frames) and the FFmpeg
//! export pipeline (`geq` alpha expression). Keeping the matte math in one
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

use crate::model::clip::HslQualifier;

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
            let (gr, gg, gb) = apply_secondary_grade(q, r, g, b);
            let nr = r * (1.0 - alpha) + gr * alpha;
            let ng = g * (1.0 - alpha) + gg * alpha;
            let nb = b * (1.0 - alpha) + gb * alpha;
            data[idx] = (nr * 255.0).round().clamp(0.0, 255.0) as u8;
            data[idx + 1] = (ng * 255.0).round().clamp(0.0, 255.0) as u8;
            data[idx + 2] = (nb * 255.0).round().clamp(0.0, 255.0) as u8;
        }
    }
}

/// Build the FFmpeg `geq` alpha expression that computes the qualifier's alpha
/// for a single pixel using `r(X,Y)` / `g(X,Y)` / `b(X,Y)` and stored-variable
/// scratch space.
///
/// The expression is entirely static per clip — bake it into the filter
/// string once and let FFmpeg's expression evaluator run it per pixel.
///
/// Output range is 0..1. Multiply by 255 on the caller side if used directly
/// as an alpha channel value.
pub fn build_hsl_qualifier_geq_alpha(q: &HslQualifier) -> String {
    // Degrees → fraction 0..1 for hue comparisons in geq (we normalize to
    // 0..1 because `st()`/`ld()` is easier to reason about than mixed units).
    let hmin = (q.hue_min / 360.0).clamp(0.0, 1.0);
    let hmax = (q.hue_max / 360.0).clamp(0.0, 1.0);
    let hsoft = (q.hue_softness / 360.0).max(0.0);
    let smin = q.sat_min.clamp(0.0, 1.0);
    let smax = q.sat_max.clamp(0.0, 1.0);
    let ssoft = q.sat_softness.max(0.0);
    let lmin = q.lum_min.clamp(0.0, 1.0);
    let lmax = q.lum_max.clamp(0.0, 1.0);
    let lsoft = q.lum_softness.max(0.0);
    let invert = q.invert;

    // In `geq`, `st(n, expr)` stores into register n and returns expr; `ld(n)`
    // reads it back. We use:
    //   0: r, 1: g, 2: b
    //   3: max(r,g,b) = V, 4: min(r,g,b)
    //   5: delta = V - min, 6: L = (V+min)/2, 7: S
    //   8: Hfrac (0..1)
    //   9: per-channel membership terms merged
    //
    // gte(a,b) returns 1 when a >= b, 0 otherwise. `if(c, t, e)` branches.
    //
    // Saturation formula:
    //   if L < 0.5: S = delta / (V + min)
    //   else:       S = delta / (2 - V - min)
    //
    // Hue formula (standard piecewise):
    //   if r == V: H = ((g - b)/delta) mod 6
    //   elif g == V: H = ((b - r)/delta) + 2
    //   else: H = ((r - g)/delta) + 4
    //   H /= 6  (to get 0..1)
    //
    // Note: ffmpeg expression parser uses `:` between params; single-quoted
    // strings survive fine, but we do NOT embed any bare apostrophes.
    //
    // Everything below uses only addition, subtraction, multiplication,
    // division, clip/clamp via `between()`, `if()`, `gte()`, `max()`, `min()`.

    // Smoothstep expression builder. edge0/edge1 are constants baked in.
    // smoothstep(e0, e1, x) = clip((x-e0)/(e1-e0), 0, 1)^2 * (3 - 2*t)
    // where t = clipped. We inline t twice; ugly but correct.
    fn ss(e0: f64, e1: f64, x_expr: &str) -> String {
        if (e1 - e0).abs() < 1e-9 {
            return format!("gte({x},{e0})", x = x_expr, e0 = e0);
        }
        // Use st(31, clip(...)) so we don't re-evaluate the inner expr three times.
        format!(
            "(if(lt({x},{e0}),0,if(gt({x},{e1}),1,st(31,({x}-{e0})/({span}))*ld(31)*(3-2*ld(31)))))",
            x = x_expr,
            e0 = e0,
            e1 = e1,
            span = (e1 - e0),
        )
    }

    // Range membership (low smoothstep × high smoothstep).
    fn range(min: f64, max: f64, soft: f64, x_expr: &str) -> String {
        if min >= max {
            return "0".to_string();
        }
        let lo = ss(min - soft, min + soft, x_expr);
        let hi = ss(max - soft, max + soft, x_expr);
        format!("({lo}*(1-{hi}))", lo = lo, hi = hi)
    }

    // Hue is either a plain range or a wraparound range.
    let hue_expr = if hmin <= 0.0 && hmax >= 1.0 {
        "1".to_string()
    } else if hmin < hmax {
        range(hmin, hmax, hsoft, "ld(8)")
    } else {
        // Wraparound.
        let a = range(hmin, 1.0, hsoft, "ld(8)");
        let b = range(0.0, hmax, hsoft, "ld(8)");
        format!("max({a},{b})", a = a, b = b)
    };
    let sat_expr = if smin <= 0.0 && smax >= 1.0 {
        "1".to_string()
    } else {
        range(smin, smax, ssoft, "ld(7)")
    };
    let lum_expr = if lmin <= 0.0 && lmax >= 1.0 {
        "1".to_string()
    } else {
        range(lmin, lmax, lsoft, "ld(6)")
    };

    // RGB → V, min, delta, L, S, H into stored vars.
    // We pack all setup into a single expression using nested st().
    let setup = concat!(
        "st(0,r(X,Y)/255)",
        "+st(1,g(X,Y)/255)",
        "+st(2,b(X,Y)/255)",
        "+st(3,max(ld(0),max(ld(1),ld(2))))",
        "+st(4,min(ld(0),min(ld(1),ld(2))))",
        "+st(5,ld(3)-ld(4))",
        "+st(6,(ld(3)+ld(4))/2)",
        "+st(7,if(lt(ld(5),0.0001),0,if(lt(ld(6),0.5),ld(5)/(ld(3)+ld(4)+0.0001),ld(5)/(2-ld(3)-ld(4)+0.0001))))",
        // Hue (0..1): branch on which channel is max.
        "+st(8,if(lt(ld(5),0.0001),0,",
        "if(gte(ld(0),ld(3)),", // r is max
        "(ld(1)-ld(2))/(ld(5)*6)+if(lt(ld(1),ld(2)),1,0),",
        "if(gte(ld(1),ld(3)),", // g is max
        "(ld(2)-ld(0))/(ld(5)*6)+2/6,",
        "(ld(0)-ld(1))/(ld(5)*6)+4/6))))",
    );

    // Final alpha = H * S * L membership (each as stored var so big expr
    // doesn't blow up). Invert handled last.
    let mut alpha_expr = format!(
        "({setup}+st(9,({h})*({s})*({l}))+0*ld(9)+ld(9))",
        setup = setup,
        h = hue_expr,
        s = sat_expr,
        l = lum_expr,
    );
    if invert {
        alpha_expr = format!("(1-{inner})", inner = alpha_expr);
    }
    alpha_expr
}

#[cfg(test)]
mod tests {
    use super::*;

    fn approx(a: f64, b: f64, eps: f64) -> bool {
        (a - b).abs() < eps
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
    fn geq_alpha_expression_is_non_empty_and_escaped() {
        let mut q = HslQualifier::default();
        q.enabled = true;
        q.hue_min = 30.0;
        q.hue_max = 90.0;
        q.hue_softness = 5.0;
        q.sat_min = 0.2;
        q.sat_max = 0.9;
        q.lum_min = 0.1;
        q.lum_max = 0.8;
        let expr = build_hsl_qualifier_geq_alpha(&q);
        assert!(!expr.is_empty());
        // No bare apostrophes — they'd break the filter string.
        assert!(!expr.contains('\''));
        // Sanity: references the HSL stored vars we set up.
        assert!(expr.contains("ld(6)"));
        assert!(expr.contains("ld(7)"));
        assert!(expr.contains("ld(8)"));
        assert!(expr.contains("r(X,Y)"));
    }

    #[test]
    fn geq_alpha_expression_invert_wraps_with_one_minus() {
        let mut q = HslQualifier::default();
        q.enabled = true;
        q.invert = true;
        let expr = build_hsl_qualifier_geq_alpha(&q);
        assert!(expr.starts_with("(1-"));
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
