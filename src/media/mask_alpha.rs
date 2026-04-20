use crate::model::clip::{ClipMask, MaskPath, MaskShape, NumericKeyframe};
use std::io::Write;
use tempfile::NamedTempFile;

/// Evaluate a keyframed f64 property at a given local time, falling back to
/// the static default when no keyframes are present.
fn interpolate_keyframed(keyframes: &[NumericKeyframe], local_time_ns: u64, default: f64) -> f64 {
    if keyframes.is_empty() {
        return default;
    }
    if keyframes.len() == 1 {
        return keyframes[0].value;
    }
    if local_time_ns <= keyframes[0].time_ns {
        return keyframes[0].value;
    }
    let last = &keyframes[keyframes.len() - 1];
    if local_time_ns >= last.time_ns {
        return last.value;
    }
    // Find surrounding pair.
    for i in 0..keyframes.len() - 1 {
        let a = &keyframes[i];
        let b = &keyframes[i + 1];
        if local_time_ns >= a.time_ns && local_time_ns <= b.time_ns {
            let span = (b.time_ns - a.time_ns) as f64;
            if span < 1.0 {
                return a.value;
            }
            let t = (local_time_ns - a.time_ns) as f64 / span;
            let (cx1, cy1, cx2, cy2) = a.segment_control_points();
            let et = cubic_bezier_ease(t, cx1, cy1, cx2, cy2);
            return a.value + (b.value - a.value) * et;
        }
    }
    default
}

/// Approximate cubic-bezier ease curve.  Control points are (cx1,cy1)→(cx2,cy2).
fn cubic_bezier_ease(t: f64, cx1: f64, cy1: f64, cx2: f64, cy2: f64) -> f64 {
    // Linear shortcut
    if (cx1 - cy1).abs() < 1e-9 && (cx2 - cy2).abs() < 1e-9 {
        return t;
    }
    // De Casteljau evaluation on the standard cubic (0,0)→(cx1,cy1)→(cx2,cy2)→(1,1)
    // We need the y for a given x=t, but for a simple approximation we evaluate y(t)
    // which is close enough for smooth easing curves.
    let mt = 1.0 - t;
    let mt2 = mt * mt;
    let t2 = t * t;
    (3.0 * mt2 * t * cy1 + 3.0 * mt * t2 * cy2 + t2 * t).clamp(0.0, 1.0)
}

/// Smoothstep: 0 when x <= edge0, 1 when x >= edge1, smooth hermite in between.
#[inline]
fn smoothstep(edge0: f64, edge1: f64, x: f64) -> f64 {
    if edge1 <= edge0 {
        return if x >= edge0 { 1.0 } else { 0.0 };
    }
    let t = ((x - edge0) / (edge1 - edge0)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// Signed distance from a point (px, py) to a rotated rectangle centered at
/// (cx, cy) with half-widths (hw, hh), rotated by `rot_rad` radians.
/// All coordinates in normalized space (0..1).
/// Returns negative inside, positive outside, zero on edge.
pub fn compute_rect_sdf(px: f64, py: f64, cx: f64, cy: f64, hw: f64, hh: f64, rot_rad: f64) -> f64 {
    // Unrotate point into rect-local space.
    let dx = px - cx;
    let dy = py - cy;
    let cos_r = rot_rad.cos();
    let sin_r = rot_rad.sin();
    let ux = dx * cos_r + dy * sin_r;
    let uy = -dx * sin_r + dy * cos_r;

    let ox = ux.abs() - hw;
    let oy = uy.abs() - hh;

    let outside_dist = (ox.max(0.0).powi(2) + oy.max(0.0).powi(2)).sqrt();
    let inside_dist = ox.max(oy).min(0.0);

    outside_dist + inside_dist
}

/// Signed distance from a point (px, py) to a rotated ellipse centered at
/// (cx, cy) with semi-axes (hw, hh), rotated by `rot_rad` radians.
/// Approximate SDF: uses normalized-radius approach.
/// Returns negative inside, positive outside (approximately), zero on boundary.
pub fn compute_ellipse_sdf(
    px: f64,
    py: f64,
    cx: f64,
    cy: f64,
    hw: f64,
    hh: f64,
    rot_rad: f64,
) -> f64 {
    let dx = px - cx;
    let dy = py - cy;
    let cos_r = rot_rad.cos();
    let sin_r = rot_rad.sin();
    let ux = dx * cos_r + dy * sin_r;
    let uy = -dx * sin_r + dy * cos_r;

    if hw < 1e-12 || hh < 1e-12 {
        return (ux * ux + uy * uy).sqrt();
    }

    // Normalized ellipse distance: (ux/hw)^2 + (uy/hh)^2 = 1 on boundary.
    let nx = ux / hw;
    let ny = uy / hh;
    let r = (nx * nx + ny * ny).sqrt();

    // The actual distance from the boundary is complex for ellipses;
    // use a linear approximation: (r - 1) * min(hw, hh).
    // This gives exact results for circles and smooth behavior for ellipses.
    (r - 1.0) * hw.min(hh)
}

// ── Bezier path SDF ──────────────────────────────────────────────────────

/// Subdivide a single cubic bezier segment into `steps` line segments.
fn subdivide_bezier_segment(
    p0: (f64, f64),
    cp1: (f64, f64),
    cp2: (f64, f64),
    p3: (f64, f64),
    steps: usize,
) -> Vec<(f64, f64)> {
    let mut pts = Vec::with_capacity(steps + 1);
    for i in 0..=steps {
        let t = i as f64 / steps as f64;
        let mt = 1.0 - t;
        let mt2 = mt * mt;
        let mt3 = mt2 * mt;
        let t2 = t * t;
        let t3 = t2 * t;
        let x = mt3 * p0.0 + 3.0 * mt2 * t * cp1.0 + 3.0 * mt * t2 * cp2.0 + t3 * p3.0;
        let y = mt3 * p0.1 + 3.0 * mt2 * t * cp1.1 + 3.0 * mt * t2 * cp2.1 + t3 * p3.1;
        pts.push((x, y));
    }
    pts
}

/// Subdivide a closed bezier path into a polyline with `steps_per_segment`
/// subdivisions per curve segment.
pub fn subdivide_path(path: &MaskPath, steps_per_segment: usize) -> Vec<(f64, f64)> {
    let n = path.points.len();
    if n < 3 {
        return Vec::new();
    }
    let steps = steps_per_segment.max(2);
    let mut polyline = Vec::with_capacity(n * steps + 1);
    for i in 0..n {
        let a = &path.points[i];
        let b = &path.points[(i + 1) % n];
        let p0 = (a.x, a.y);
        let cp1 = (a.x + a.handle_out_x, a.y + a.handle_out_y);
        let cp2 = (b.x + b.handle_in_x, b.y + b.handle_in_y);
        let p3 = (b.x, b.y);
        let seg = subdivide_bezier_segment(p0, cp1, cp2, p3, steps);
        // Skip the last point of each segment (it's the first of the next).
        let end = if i < n - 1 { seg.len() - 1 } else { seg.len() };
        polyline.extend_from_slice(&seg[..end]);
    }
    polyline
}

/// Winding number of point (px, py) with respect to a closed polyline.
/// Non-zero means inside.
fn winding_number(px: f64, py: f64, polyline: &[(f64, f64)]) -> i32 {
    let n = polyline.len();
    if n < 3 {
        return 0;
    }
    let mut wn = 0i32;
    for i in 0..n {
        let (x1, y1) = polyline[i];
        let (x2, y2) = polyline[(i + 1) % n];
        if y1 <= py {
            if y2 > py {
                // Upward crossing.
                let cross = (x2 - x1) * (py - y1) - (px - x1) * (y2 - y1);
                if cross > 0.0 {
                    wn += 1;
                }
            }
        } else if y2 <= py {
            // Downward crossing.
            let cross = (x2 - x1) * (py - y1) - (px - x1) * (y2 - y1);
            if cross < 0.0 {
                wn -= 1;
            }
        }
    }
    wn
}

/// Minimum distance from point (px, py) to any segment in the polyline.
fn distance_to_polyline(px: f64, py: f64, polyline: &[(f64, f64)]) -> f64 {
    let n = polyline.len();
    if n < 2 {
        return f64::MAX;
    }
    let mut min_d = f64::MAX;
    for i in 0..n {
        let (x1, y1) = polyline[i];
        let (x2, y2) = polyline[(i + 1) % n];
        let dx = x2 - x1;
        let dy = y2 - y1;
        let len_sq = dx * dx + dy * dy;
        let t = if len_sq < 1e-18 {
            0.0
        } else {
            ((px - x1) * dx + (py - y1) * dy) / len_sq
        }
        .clamp(0.0, 1.0);
        let proj_x = x1 + t * dx;
        let proj_y = y1 + t * dy;
        let d = ((px - proj_x).powi(2) + (py - proj_y).powi(2)).sqrt();
        if d < min_d {
            min_d = d;
        }
    }
    min_d
}

/// Compute signed distance for a point relative to a polyline (closed path).
/// Negative = inside, positive = outside.
pub fn compute_path_sdf(px: f64, py: f64, polyline: &[(f64, f64)], expansion: f64) -> f64 {
    let inside = winding_number(px, py, polyline) != 0;
    let dist = distance_to_polyline(px, py, polyline);
    let signed = if inside { -dist } else { dist };
    signed - expansion
}

/// Compute the axis-aligned bounding box of a polyline.
/// Returns (min_x, min_y, max_x, max_y).
fn polyline_aabb(polyline: &[(f64, f64)]) -> (f64, f64, f64, f64) {
    let mut min_x = f64::MAX;
    let mut min_y = f64::MAX;
    let mut max_x = f64::MIN;
    let mut max_y = f64::MIN;
    for &(x, y) in polyline {
        if x < min_x {
            min_x = x;
        }
        if y < min_y {
            min_y = y;
        }
        if x > max_x {
            max_x = x;
        }
        if y > max_y {
            max_y = y;
        }
    }
    (min_x, min_y, max_x, max_y)
}

/// Evaluate mask alpha for a path mask using a pre-computed polyline.
/// Used by the optimized `apply_masks_to_rgba_buffer` to avoid
/// re-subdividing the bezier path for every pixel.
fn path_mask_alpha(
    polyline: &[(f64, f64)],
    aabb: (f64, f64, f64, f64),
    npx: f64,
    npy: f64,
    feather: f64,
    expansion: f64,
    invert: bool,
) -> f64 {
    let (min_x, min_y, max_x, max_y) = aabb;
    let margin = feather + expansion.abs() + 0.01;
    // AABB early-out: if pixel is well outside the path bounding box, return 0 (or 1 if inverted).
    if npx < min_x - margin || npx > max_x + margin || npy < min_y - margin || npy > max_y + margin
    {
        return if invert { 1.0 } else { 0.0 };
    }
    // If pixel is well inside the contracted AABB, return 1 (or 0 if inverted).
    if npx > min_x + margin && npx < max_x - margin && npy > min_y + margin && npy < max_y - margin
    {
        // Still need to check winding number for concave shapes, so skip this optimization.
    }

    let sdf = compute_path_sdf(npx, npy, polyline, expansion);

    let alpha = if feather < 1e-9 {
        if sdf <= 0.0 {
            1.0
        } else {
            0.0
        }
    } else {
        1.0 - smoothstep(-feather, 0.0, sdf)
    };

    if invert {
        1.0 - alpha
    } else {
        alpha
    }
}

/// Rasterize combined mask alpha to a grayscale buffer (one byte per pixel).
/// Used by the export pipeline for path masks that cannot be expressed as
/// FFmpeg `geq` expressions.
pub fn rasterize_masks_to_grayscale(
    masks: &[ClipMask],
    width: usize,
    height: usize,
    local_time_ns: u64,
    clip_scale: f64,
    clip_pos_x: f64,
    clip_pos_y: f64,
) -> Vec<u8> {
    let active: Vec<&ClipMask> = masks.iter().filter(|m| m.enabled).collect();
    let mut buf = vec![255u8; width * height];
    if active.is_empty() {
        return buf;
    }

    // Pre-compute polylines for path masks.
    let polylines: Vec<Option<(Vec<(f64, f64)>, (f64, f64, f64, f64))>> = active
        .iter()
        .map(|m| {
            if m.shape == MaskShape::Path {
                if let Some(ref path) = m.path {
                    let poly = subdivide_path(path, 20);
                    let aabb = polyline_aabb(&poly);
                    return Some((poly, aabb));
                }
            }
            None
        })
        .collect();

    // The rasterized mask is used in the export `geq` context, which operates
    // on the output canvas (post-zoom/position).  Map pixel coords back to
    // clip-local normalized space, matching the preview probe coordinate system.
    let fw = width as f64;
    let fh = height as f64;
    let clip_cx_canvas = fw / 2.0 + clip_pos_x * fw * (1.0 - clip_scale) / 2.0;
    let clip_cy_canvas = fh / 2.0 + clip_pos_y * fh * (1.0 - clip_scale) / 2.0;
    let clip_w = fw * clip_scale;
    let clip_h = fh * clip_scale;
    let clip_left = clip_cx_canvas - clip_w / 2.0;
    let clip_top = clip_cy_canvas - clip_h / 2.0;

    for y in 0..height {
        for x in 0..width {
            // Map canvas pixel to clip-local normalized coords.
            let npx = (x as f64 + 0.5 - clip_left) / clip_w;
            let npy = (y as f64 + 0.5 - clip_top) / clip_h;

            let mut combined = 1.0f64;
            for (i, mask) in active.iter().enumerate() {
                let feather =
                    interpolate_keyframed(&mask.feather_keyframes, local_time_ns, mask.feather)
                        .max(0.0);
                let expansion =
                    interpolate_keyframed(&mask.expansion_keyframes, local_time_ns, mask.expansion);

                let alpha = match mask.shape {
                    MaskShape::Path => {
                        if let Some((ref poly, aabb)) = polylines[i] {
                            path_mask_alpha(poly, aabb, npx, npy, feather, expansion, mask.invert)
                        } else {
                            1.0
                        }
                    }
                    MaskShape::Rectangle => {
                        let cx = interpolate_keyframed(
                            &mask.center_x_keyframes,
                            local_time_ns,
                            mask.center_x,
                        );
                        let cy = interpolate_keyframed(
                            &mask.center_y_keyframes,
                            local_time_ns,
                            mask.center_y,
                        );
                        let hw = (interpolate_keyframed(
                            &mask.width_keyframes,
                            local_time_ns,
                            mask.width,
                        ) + expansion)
                            .max(0.0);
                        let hh = (interpolate_keyframed(
                            &mask.height_keyframes,
                            local_time_ns,
                            mask.height,
                        ) + expansion)
                            .max(0.0);
                        let rot = interpolate_keyframed(
                            &mask.rotation_keyframes,
                            local_time_ns,
                            mask.rotation,
                        )
                        .to_radians();
                        let sdf = compute_rect_sdf(npx, npy, cx, cy, hw, hh, rot);
                        let a = if feather < 1e-9 {
                            if sdf <= 0.0 {
                                1.0
                            } else {
                                0.0
                            }
                        } else {
                            1.0 - smoothstep(-feather, 0.0, sdf)
                        };
                        if mask.invert {
                            1.0 - a
                        } else {
                            a
                        }
                    }
                    MaskShape::Ellipse => {
                        let cx = interpolate_keyframed(
                            &mask.center_x_keyframes,
                            local_time_ns,
                            mask.center_x,
                        );
                        let cy = interpolate_keyframed(
                            &mask.center_y_keyframes,
                            local_time_ns,
                            mask.center_y,
                        );
                        let hw = (interpolate_keyframed(
                            &mask.width_keyframes,
                            local_time_ns,
                            mask.width,
                        ) + expansion)
                            .max(0.0);
                        let hh = (interpolate_keyframed(
                            &mask.height_keyframes,
                            local_time_ns,
                            mask.height,
                        ) + expansion)
                            .max(0.0);
                        let rot = interpolate_keyframed(
                            &mask.rotation_keyframes,
                            local_time_ns,
                            mask.rotation,
                        )
                        .to_radians();
                        let sdf = compute_ellipse_sdf(npx, npy, cx, cy, hw, hh, rot);
                        let a = if feather < 1e-9 {
                            if sdf <= 0.0 {
                                1.0
                            } else {
                                0.0
                            }
                        } else {
                            1.0 - smoothstep(-feather, 0.0, sdf)
                        };
                        if mask.invert {
                            1.0 - a
                        } else {
                            a
                        }
                    }
                };
                combined *= alpha;
            }
            buf[y * width + x] = (combined * 255.0).round().clamp(0.0, 255.0) as u8;
        }
    }
    buf
}

/// Evaluate the mask alpha for a single pixel.
/// Returns 0.0 (fully transparent) to 1.0 (fully opaque).
pub fn mask_alpha_at_pixel(
    mask: &ClipMask,
    px: usize,
    py: usize,
    frame_w: usize,
    frame_h: usize,
    local_time_ns: u64,
) -> f64 {
    if !mask.enabled || frame_w == 0 || frame_h == 0 {
        return 1.0;
    }

    let cx = interpolate_keyframed(&mask.center_x_keyframes, local_time_ns, mask.center_x);
    let cy = interpolate_keyframed(&mask.center_y_keyframes, local_time_ns, mask.center_y);
    let hw = interpolate_keyframed(&mask.width_keyframes, local_time_ns, mask.width);
    let hh = interpolate_keyframed(&mask.height_keyframes, local_time_ns, mask.height);
    let rotation = interpolate_keyframed(&mask.rotation_keyframes, local_time_ns, mask.rotation);
    let feather =
        interpolate_keyframed(&mask.feather_keyframes, local_time_ns, mask.feather).max(0.0);
    let expansion = interpolate_keyframed(&mask.expansion_keyframes, local_time_ns, mask.expansion);

    let rot_rad = rotation.to_radians();

    // Apply expansion to half-widths.
    let hw = (hw + expansion).max(0.0);
    let hh = (hh + expansion).max(0.0);

    // Normalize pixel to 0..1.
    let npx = (px as f64 + 0.5) / frame_w as f64;
    let npy = (py as f64 + 0.5) / frame_h as f64;

    let sdf = match mask.shape {
        MaskShape::Rectangle => compute_rect_sdf(npx, npy, cx, cy, hw, hh, rot_rad),
        MaskShape::Ellipse => compute_ellipse_sdf(npx, npy, cx, cy, hw, hh, rot_rad),
        MaskShape::Path => {
            // Path SDF computed via polyline approximation (see apply_masks_to_rgba_buffer
            // for the cached path). Fallback for single-pixel calls: subdivide inline.
            if let Some(ref path) = mask.path {
                if path.points.len() >= 3 {
                    let polyline = subdivide_path(path, 20);
                    compute_path_sdf(npx, npy, &polyline, expansion)
                } else {
                    return 1.0; // degenerate path
                }
            } else {
                return 1.0; // no path data
            }
        }
    };

    // sdf < 0 means inside, > 0 means outside.
    // With feathering: smooth transition from 1 (inside) to 0 (outside) over feather width.
    let alpha = if feather < 1e-9 {
        if sdf <= 0.0 {
            1.0
        } else {
            0.0
        }
    } else {
        // smoothstep from fully opaque (at -feather/2 inside) to fully transparent (at feather/2 outside).
        // Actually: 1 - smoothstep(0 - feather, 0, sdf)
        1.0 - smoothstep(-feather, 0.0, sdf)
    };

    if mask.invert {
        1.0 - alpha
    } else {
        alpha
    }
}

/// Apply all masks to an RGBA buffer (4 bytes per pixel), multiplying existing
/// alpha by the combined mask alpha.  Multiple masks combine multiplicatively
/// (intersection).
pub fn apply_masks_to_rgba_buffer(
    masks: &[ClipMask],
    data: &mut [u8],
    width: usize,
    height: usize,
    local_time_ns: u64,
) {
    let active: Vec<&ClipMask> = masks.iter().filter(|m| m.enabled).collect();
    if active.is_empty() {
        return;
    }

    // Pre-compute polylines for path masks (avoid re-subdividing per pixel).
    let polylines: Vec<Option<(Vec<(f64, f64)>, (f64, f64, f64, f64))>> = active
        .iter()
        .map(|m| {
            if m.shape == MaskShape::Path {
                if let Some(ref path) = m.path {
                    if path.points.len() >= 3 {
                        let poly = subdivide_path(path, 20);
                        let aabb = polyline_aabb(&poly);
                        return Some((poly, aabb));
                    }
                }
            }
            None
        })
        .collect();

    let stride = width * 4;
    for y in 0..height {
        let row_offset = y * stride;
        for x in 0..width {
            let npx = (x as f64 + 0.5) / width as f64;
            let npy = (y as f64 + 0.5) / height as f64;
            let mut combined_alpha = 1.0f64;
            for (i, mask) in active.iter().enumerate() {
                let alpha = if mask.shape == MaskShape::Path {
                    if let Some((ref poly, aabb)) = polylines[i] {
                        let feather = interpolate_keyframed(
                            &mask.feather_keyframes,
                            local_time_ns,
                            mask.feather,
                        )
                        .max(0.0);
                        let expansion = interpolate_keyframed(
                            &mask.expansion_keyframes,
                            local_time_ns,
                            mask.expansion,
                        );
                        path_mask_alpha(poly, aabb, npx, npy, feather, expansion, mask.invert)
                    } else {
                        1.0
                    }
                } else {
                    mask_alpha_at_pixel(mask, x, y, width, height, local_time_ns)
                };
                combined_alpha *= alpha;
            }
            if combined_alpha >= 1.0 {
                continue;
            }
            let idx = row_offset + x * 4 + 3; // Alpha channel (RGBA).
            if idx < data.len() {
                let existing = data[idx] as f64 / 255.0;
                data[idx] = (existing * combined_alpha * 255.0)
                    .round()
                    .clamp(0.0, 255.0) as u8;
            }
        }
    }
}

#[derive(Debug, Clone)]
struct ResolvedCanvasMask {
    shape: MaskShape,
    center_x: f64,
    center_y: f64,
    width: f64,
    height: f64,
    rotation_rad: f64,
    feather: f64,
    expansion: f64,
    invert: bool,
    path_polyline: Option<Vec<(f64, f64)>>,
    path_aabb: Option<(f64, f64, f64, f64)>,
}

impl ResolvedCanvasMask {
    fn alpha_at_normalized_point(&self, npx: f64, npy: f64) -> f64 {
        match self.shape {
            MaskShape::Rectangle => {
                let sdf = compute_rect_sdf(
                    npx,
                    npy,
                    self.center_x,
                    self.center_y,
                    (self.width + self.expansion).max(0.0),
                    (self.height + self.expansion).max(0.0),
                    self.rotation_rad,
                );
                alpha_from_sdf(sdf, self.feather, self.invert)
            }
            MaskShape::Ellipse => {
                let sdf = compute_ellipse_sdf(
                    npx,
                    npy,
                    self.center_x,
                    self.center_y,
                    (self.width + self.expansion).max(0.0),
                    (self.height + self.expansion).max(0.0),
                    self.rotation_rad,
                );
                alpha_from_sdf(sdf, self.feather, self.invert)
            }
            MaskShape::Path => match (&self.path_polyline, self.path_aabb) {
                (Some(polyline), Some(aabb)) => path_mask_alpha(
                    polyline,
                    aabb,
                    npx,
                    npy,
                    self.feather,
                    self.expansion,
                    self.invert,
                ),
                _ => 1.0,
            },
        }
    }

    fn build_ffmpeg_expression(&self, npx_expr: &str, npy_expr: &str) -> String {
        match self.shape {
            MaskShape::Rectangle => build_rect_geq_expr_for_coords(
                npx_expr,
                npy_expr,
                self.center_x,
                self.center_y,
                (self.width + self.expansion).max(0.0),
                (self.height + self.expansion).max(0.0),
                self.rotation_rad,
                self.feather,
                self.invert,
            ),
            MaskShape::Ellipse => build_ellipse_geq_expr_for_coords(
                npx_expr,
                npy_expr,
                self.center_x,
                self.center_y,
                (self.width + self.expansion).max(0.0),
                (self.height + self.expansion).max(0.0),
                self.rotation_rad,
                self.feather,
                self.invert,
            ),
            MaskShape::Path => "1".to_string(),
        }
    }

    fn normalized_bounds(&self) -> Option<(f64, f64, f64, f64)> {
        match self.shape {
            MaskShape::Rectangle | MaskShape::Ellipse => {
                let half_w = (self.width + self.expansion).max(0.0) + self.feather;
                let half_h = (self.height + self.expansion).max(0.0) + self.feather;
                let corners = [
                    (-half_w, -half_h),
                    (half_w, -half_h),
                    (half_w, half_h),
                    (-half_w, half_h),
                ];
                let cos_r = self.rotation_rad.cos();
                let sin_r = self.rotation_rad.sin();
                let mut min_x = f64::INFINITY;
                let mut min_y = f64::INFINITY;
                let mut max_x = f64::NEG_INFINITY;
                let mut max_y = f64::NEG_INFINITY;
                for (dx, dy) in corners {
                    let x = self.center_x + dx * cos_r - dy * sin_r;
                    let y = self.center_y + dx * sin_r + dy * cos_r;
                    min_x = min_x.min(x);
                    min_y = min_y.min(y);
                    max_x = max_x.max(x);
                    max_y = max_y.max(y);
                }
                Some((min_x, min_y, max_x, max_y))
            }
            MaskShape::Path => self.path_aabb.map(|(min_x, min_y, max_x, max_y)| {
                let margin = self.feather + self.expansion.abs();
                (
                    min_x - margin,
                    min_y - margin,
                    max_x + margin,
                    max_y + margin,
                )
            }),
        }
    }
}

#[derive(Debug, Clone)]
pub struct PreparedCanvasMasks {
    clip_scale: f64,
    clip_pos_x: f64,
    clip_pos_y: f64,
    clip_rotation_rad: f64,
    transform_space: CanvasTransformSpace,
    masks: Vec<ResolvedCanvasMask>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CanvasTransformSpace {
    ClipPlacement,
    AdjustmentLayer,
}

impl PreparedCanvasMasks {
    pub fn alpha_at_canvas_pixel(
        &self,
        x: usize,
        y: usize,
        frame_width: usize,
        frame_height: usize,
    ) -> f64 {
        if self.masks.is_empty() || frame_width == 0 || frame_height == 0 {
            return 1.0;
        }

        let (npx, npy) = self.canvas_pixel_to_normalized_coords(x, y, frame_width, frame_height);
        let mut combined = 1.0;
        for mask in &self.masks {
            combined *= mask.alpha_at_normalized_point(npx, npy);
            if combined <= f64::EPSILON {
                return 0.0;
            }
        }
        combined
    }

    pub fn rasterize_to_grayscale(&self, frame_width: usize, frame_height: usize) -> Vec<u8> {
        let mut buf = vec![255u8; frame_width.saturating_mul(frame_height)];
        for y in 0..frame_height {
            for x in 0..frame_width {
                buf[y * frame_width + x] =
                    (self.alpha_at_canvas_pixel(x, y, frame_width, frame_height) * 255.0)
                        .round()
                        .clamp(0.0, 255.0) as u8;
            }
        }
        buf
    }

    pub fn pixel_bounds(
        &self,
        frame_width: usize,
        frame_height: usize,
    ) -> Option<(usize, usize, usize, usize)> {
        if self.masks.is_empty() || frame_width == 0 || frame_height == 0 {
            return None;
        }

        let mut min_x = f64::INFINITY;
        let mut min_y = f64::INFINITY;
        let mut max_x = f64::NEG_INFINITY;
        let mut max_y = f64::NEG_INFINITY;

        for mask in &self.masks {
            let Some((mask_min_x, mask_min_y, mask_max_x, mask_max_y)) = mask.normalized_bounds()
            else {
                continue;
            };
            let corners = [
                (mask_min_x, mask_min_y),
                (mask_max_x, mask_min_y),
                (mask_max_x, mask_max_y),
                (mask_min_x, mask_max_y),
            ];
            for (npx, npy) in corners {
                let (x, y) =
                    self.normalized_point_to_canvas_coords(npx, npy, frame_width, frame_height);
                min_x = min_x.min(x);
                min_y = min_y.min(y);
                max_x = max_x.max(x);
                max_y = max_y.max(y);
            }
        }

        if !min_x.is_finite() || !min_y.is_finite() || !max_x.is_finite() || !max_y.is_finite() {
            return None;
        }

        let x0 = min_x.floor().max(0.0).min(frame_width as f64) as usize;
        let y0 = min_y.floor().max(0.0).min(frame_height as f64) as usize;
        let x1 = max_x.ceil().max(0.0).min(frame_width as f64) as usize;
        let y1 = max_y.ceil().max(0.0).min(frame_height as f64) as usize;
        if x0 >= x1 || y0 >= y1 {
            None
        } else {
            Some((x0, y0, x1, y1))
        }
    }

    fn canvas_pixel_to_normalized_coords(
        &self,
        x: usize,
        y: usize,
        frame_width: usize,
        frame_height: usize,
    ) -> (f64, f64) {
        let fw = frame_width.max(1) as f64;
        let fh = frame_height.max(1) as f64;
        let clip_scale = self.clip_scale.max(1e-6);
        let (clip_center_x, clip_center_y, clip_width, clip_height) = match self.transform_space {
            CanvasTransformSpace::ClipPlacement => (
                fw / 2.0 + self.clip_pos_x * fw * (1.0 - clip_scale) / 2.0,
                fh / 2.0 + self.clip_pos_y * fh * (1.0 - clip_scale) / 2.0,
                (fw * clip_scale).max(1e-6),
                (fh * clip_scale).max(1e-6),
            ),
            CanvasTransformSpace::AdjustmentLayer => {
                let (center_x, center_y, clip_width, clip_height) =
                    crate::media::adjustment_scope::adjustment_canvas_geometry(
                        fw,
                        fh,
                        clip_scale,
                        self.clip_pos_x,
                        self.clip_pos_y,
                    );
                (
                    center_x,
                    center_y,
                    clip_width.max(1e-6),
                    clip_height.max(1e-6),
                )
            }
        };
        let clip_left = clip_center_x - clip_width / 2.0;
        let clip_top = clip_center_y - clip_height / 2.0;
        let px = x as f64 + 0.5;
        let py = y as f64 + 0.5;
        let dx = px - clip_center_x;
        let dy = py - clip_center_y;
        let cos_r = self.clip_rotation_rad.cos();
        let sin_r = self.clip_rotation_rad.sin();
        let ux = clip_center_x + dx * cos_r - dy * sin_r;
        let uy = clip_center_y + dx * sin_r + dy * cos_r;
        ((ux - clip_left) / clip_width, (uy - clip_top) / clip_height)
    }

    fn normalized_point_to_canvas_coords(
        &self,
        npx: f64,
        npy: f64,
        frame_width: usize,
        frame_height: usize,
    ) -> (f64, f64) {
        let fw = frame_width.max(1) as f64;
        let fh = frame_height.max(1) as f64;
        let clip_scale = self.clip_scale.max(1e-6);
        let (clip_center_x, clip_center_y, clip_width, clip_height) = match self.transform_space {
            CanvasTransformSpace::ClipPlacement => (
                fw / 2.0 + self.clip_pos_x * fw * (1.0 - clip_scale) / 2.0,
                fh / 2.0 + self.clip_pos_y * fh * (1.0 - clip_scale) / 2.0,
                (fw * clip_scale).max(1e-6),
                (fh * clip_scale).max(1e-6),
            ),
            CanvasTransformSpace::AdjustmentLayer => {
                let (center_x, center_y, clip_width, clip_height) =
                    crate::media::adjustment_scope::adjustment_canvas_geometry(
                        fw,
                        fh,
                        clip_scale,
                        self.clip_pos_x,
                        self.clip_pos_y,
                    );
                (
                    center_x,
                    center_y,
                    clip_width.max(1e-6),
                    clip_height.max(1e-6),
                )
            }
        };
        let clip_left = clip_center_x - clip_width / 2.0;
        let clip_top = clip_center_y - clip_height / 2.0;
        let ux = clip_left + npx * clip_width;
        let uy = clip_top + npy * clip_height;
        let dx = ux - clip_center_x;
        let dy = uy - clip_center_y;
        let cos_r = self.clip_rotation_rad.cos();
        let sin_r = self.clip_rotation_rad.sin();
        (
            clip_center_x + dx * cos_r + dy * sin_r,
            clip_center_y - dx * sin_r + dy * cos_r,
        )
    }

    fn has_path_masks(&self) -> bool {
        self.masks.iter().any(|mask| mask.shape == MaskShape::Path)
    }
}

fn alpha_from_sdf(sdf: f64, feather: f64, invert: bool) -> f64 {
    let alpha = if feather < 1e-9 {
        if sdf <= 0.0 {
            1.0
        } else {
            0.0
        }
    } else {
        1.0 - smoothstep(-feather, 0.0, sdf)
    };
    if invert {
        1.0 - alpha
    } else {
        alpha
    }
}

pub fn prepare_canvas_masks(
    masks: &[ClipMask],
    local_time_ns: u64,
    clip_scale: f64,
    clip_pos_x: f64,
    clip_pos_y: f64,
    clip_rotation_deg: f64,
) -> Option<PreparedCanvasMasks> {
    prepare_canvas_masks_with_space(
        masks,
        local_time_ns,
        clip_scale,
        clip_pos_x,
        clip_pos_y,
        clip_rotation_deg,
        CanvasTransformSpace::ClipPlacement,
    )
}

pub fn prepare_adjustment_canvas_masks(
    masks: &[ClipMask],
    local_time_ns: u64,
    clip_scale: f64,
    clip_pos_x: f64,
    clip_pos_y: f64,
    clip_rotation_deg: f64,
) -> Option<PreparedCanvasMasks> {
    prepare_canvas_masks_with_space(
        masks,
        local_time_ns,
        clip_scale,
        clip_pos_x,
        clip_pos_y,
        clip_rotation_deg,
        CanvasTransformSpace::AdjustmentLayer,
    )
}

fn prepare_canvas_masks_with_space(
    masks: &[ClipMask],
    local_time_ns: u64,
    clip_scale: f64,
    clip_pos_x: f64,
    clip_pos_y: f64,
    clip_rotation_deg: f64,
    transform_space: CanvasTransformSpace,
) -> Option<PreparedCanvasMasks> {
    let resolved: Vec<ResolvedCanvasMask> = masks
        .iter()
        .filter(|mask| mask.enabled)
        .map(|mask| {
            let path_polyline = if mask.shape == MaskShape::Path {
                mask.path.as_ref().and_then(|path| {
                    if path.points.len() >= 3 {
                        Some(subdivide_path(path, 20))
                    } else {
                        None
                    }
                })
            } else {
                None
            };
            let path_aabb = path_polyline
                .as_ref()
                .map(|polyline| polyline_aabb(polyline));

            ResolvedCanvasMask {
                shape: mask.shape,
                center_x: interpolate_keyframed(
                    &mask.center_x_keyframes,
                    local_time_ns,
                    mask.center_x,
                ),
                center_y: interpolate_keyframed(
                    &mask.center_y_keyframes,
                    local_time_ns,
                    mask.center_y,
                ),
                width: interpolate_keyframed(&mask.width_keyframes, local_time_ns, mask.width),
                height: interpolate_keyframed(&mask.height_keyframes, local_time_ns, mask.height),
                rotation_rad: interpolate_keyframed(
                    &mask.rotation_keyframes,
                    local_time_ns,
                    mask.rotation,
                )
                .to_radians(),
                feather: interpolate_keyframed(
                    &mask.feather_keyframes,
                    local_time_ns,
                    mask.feather,
                )
                .max(0.0),
                expansion: interpolate_keyframed(
                    &mask.expansion_keyframes,
                    local_time_ns,
                    mask.expansion,
                ),
                invert: mask.invert,
                path_polyline,
                path_aabb,
            }
        })
        .collect();

    if resolved.is_empty() {
        return None;
    }

    Some(PreparedCanvasMasks {
        clip_scale,
        clip_pos_x,
        clip_pos_y,
        clip_rotation_rad: clip_rotation_deg.to_radians(),
        transform_space,
        masks: resolved,
    })
}

fn build_canvas_normalized_coord_expressions(
    out_w: u32,
    out_h: u32,
    clip_scale: f64,
    clip_pos_x: f64,
    clip_pos_y: f64,
    clip_rotation_deg: f64,
) -> (String, String) {
    let fw = out_w.max(1) as f64;
    let fh = out_h.max(1) as f64;
    let clip_scale = clip_scale.max(1e-6);
    let clip_center_x = fw / 2.0 + clip_pos_x * fw * (1.0 - clip_scale) / 2.0;
    let clip_center_y = fh / 2.0 + clip_pos_y * fh * (1.0 - clip_scale) / 2.0;
    let clip_width = (fw * clip_scale).max(1e-6);
    let clip_height = (fh * clip_scale).max(1e-6);
    let clip_left = clip_center_x - clip_width / 2.0;
    let clip_top = clip_center_y - clip_height / 2.0;
    let clip_rotation_rad = clip_rotation_deg.to_radians();
    let ux_expr = format!(
        "({clip_center_x:.10})+(X-({clip_center_x:.10}))*{cos_r:.10}-(Y-({clip_center_y:.10}))*{sin_r:.10}",
        cos_r = clip_rotation_rad.cos(),
        sin_r = clip_rotation_rad.sin(),
    );
    let uy_expr = format!(
        "({clip_center_y:.10})+(X-({clip_center_x:.10}))*{sin_r:.10}+(Y-({clip_center_y:.10}))*{cos_r:.10}",
        cos_r = clip_rotation_rad.cos(),
        sin_r = clip_rotation_rad.sin(),
    );
    (
        format!("(({ux_expr})-({clip_left:.10}))/({clip_width:.10})"),
        format!("(({uy_expr})-({clip_top:.10}))/({clip_height:.10})"),
    )
}

/// Build an FFmpeg `geq` alpha sub-expression for a single mask.
///
/// The `geq` filter runs on the composited output canvas (after the clip
/// has been scaled and positioned via the overlay step).  Mask coordinates
/// are defined in clip-local normalized space (0..1), so this function
/// maps them through the clip's scale/position transform to produce
/// pixel-space coordinates in the output canvas.
///
/// `out_w` and `out_h` are the output frame dimensions.
/// `clip_scale`, `clip_pos_x`, `clip_pos_y` are the clip's transform parameters.
/// `time_var` is the FFmpeg time variable (typically "T" in seconds).
/// Returns an expression that evaluates to 0.0–1.0 per pixel.
pub fn build_mask_ffmpeg_geq_alpha(
    mask: &ClipMask,
    out_w: u32,
    out_h: u32,
    clip_scale: f64,
    clip_pos_x: f64,
    clip_pos_y: f64,
    _time_var: &str,
) -> String {
    if !mask.enabled {
        return "1".to_string();
    }

    let cx = mask.center_x;
    let cy = mask.center_y;
    let hw = (mask.width + mask.expansion).max(0.0);
    let hh = (mask.height + mask.expansion).max(0.0);
    let feather = mask.feather.max(0.0);
    let rot_rad = mask.rotation.to_radians();

    let fw = out_w as f64;
    let fh = out_h as f64;

    // The clip occupies a region within the output canvas defined by its
    // scale and position.  Same formula as the GStreamer compositor and
    // the overlay drawing code.
    let clip_cx_canvas = fw / 2.0 + clip_pos_x * fw * (1.0 - clip_scale) / 2.0;
    let clip_cy_canvas = fh / 2.0 + clip_pos_y * fh * (1.0 - clip_scale) / 2.0;
    let clip_w = fw * clip_scale;
    let clip_h = fh * clip_scale;
    let clip_left = clip_cx_canvas - clip_w / 2.0;
    let clip_top = clip_cy_canvas - clip_h / 2.0;

    // Map mask normalized coords to canvas pixel coords within the clip region.
    let pcx = clip_left + cx * clip_w;
    let pcy = clip_top + cy * clip_h;
    let phw = hw * clip_w;
    let phh = hh * clip_h;
    let pfeather = feather * clip_w.min(clip_h);

    match mask.shape {
        MaskShape::Rectangle => {
            build_rect_geq_expr(pcx, pcy, phw, phh, rot_rad, pfeather, mask.invert)
        }
        MaskShape::Ellipse => {
            build_ellipse_geq_expr(pcx, pcy, phw, phh, rot_rad, pfeather, mask.invert)
        }
        MaskShape::Path => {
            // Path masks use rasterized PNG in the export pipeline.
            // Return "1" here; the caller detects path masks and uses the raster path.
            "1".to_string()
        }
    }
}

/// Combined mask representation for FFmpeg-based render paths.
#[derive(Debug)]
pub enum FfmpegMaskAlphaResult {
    /// All active masks can be represented inline via a `geq` alpha expression.
    GeqExpression(String),
    /// At least one active mask is a path mask, so a rasterized grayscale file is needed.
    RasterFile(NamedTempFile),
}

pub fn build_combined_transformed_mask_ffmpeg_alpha(
    masks: &[ClipMask],
    out_w: u32,
    out_h: u32,
    local_time_ns: u64,
    clip_scale: f64,
    clip_pos_x: f64,
    clip_pos_y: f64,
    clip_rotation_deg: f64,
) -> Option<FfmpegMaskAlphaResult> {
    let prepared = prepare_canvas_masks(
        masks,
        local_time_ns,
        clip_scale,
        clip_pos_x,
        clip_pos_y,
        clip_rotation_deg,
    )?;

    if prepared.has_path_masks() {
        let buf = prepared.rasterize_to_grayscale(out_w as usize, out_h as usize);
        if let Ok(mut file) = NamedTempFile::new() {
            let header = format!("P5\n{} {}\n255\n", out_w, out_h);
            if file.write_all(header.as_bytes()).is_ok() && file.write_all(&buf).is_ok() {
                return Some(FfmpegMaskAlphaResult::RasterFile(file));
            }
        }
        return Some(FfmpegMaskAlphaResult::GeqExpression("1".to_string()));
    }

    let (npx_expr, npy_expr) = build_canvas_normalized_coord_expressions(
        out_w,
        out_h,
        clip_scale,
        clip_pos_x,
        clip_pos_y,
        clip_rotation_deg,
    );
    let exprs: Vec<String> = prepared
        .masks
        .iter()
        .map(|mask| mask.build_ffmpeg_expression(&npx_expr, &npy_expr))
        .collect();
    let combined = if exprs.len() == 1 {
        exprs.into_iter().next().unwrap_or_else(|| "1".to_string())
    } else {
        exprs.join("*")
    };
    Some(FfmpegMaskAlphaResult::GeqExpression(combined))
}

/// Build a combined FFmpeg mask representation for the given clip transform.
pub fn build_combined_mask_ffmpeg_alpha(
    masks: &[ClipMask],
    out_w: u32,
    out_h: u32,
    local_time_ns: u64,
    clip_scale: f64,
    clip_pos_x: f64,
    clip_pos_y: f64,
) -> Option<FfmpegMaskAlphaResult> {
    let active: Vec<_> = masks.iter().filter(|mask| mask.enabled).collect();
    if active.is_empty() {
        return None;
    }

    let has_path = active.iter().any(|mask| mask.shape == MaskShape::Path);
    if has_path {
        let buf = rasterize_masks_to_grayscale(
            masks,
            out_w as usize,
            out_h as usize,
            local_time_ns,
            clip_scale,
            clip_pos_x,
            clip_pos_y,
        );
        if let Ok(mut file) = NamedTempFile::new() {
            let header = format!("P5\n{} {}\n255\n", out_w, out_h);
            if file.write_all(header.as_bytes()).is_ok() && file.write_all(&buf).is_ok() {
                return Some(FfmpegMaskAlphaResult::RasterFile(file));
            }
        }
        return Some(FfmpegMaskAlphaResult::GeqExpression("1".to_string()));
    }

    let exprs: Vec<String> = active
        .iter()
        .map(|mask| {
            build_mask_ffmpeg_geq_alpha(mask, out_w, out_h, clip_scale, clip_pos_x, clip_pos_y, "T")
        })
        .collect();
    let combined = if exprs.len() == 1 {
        exprs.into_iter().next().unwrap_or_else(|| "1".to_string())
    } else {
        exprs.join("*")
    };
    Some(FfmpegMaskAlphaResult::GeqExpression(combined))
}

fn build_rect_geq_expr_for_coords(
    px_expr: &str,
    py_expr: &str,
    cx: f64,
    cy: f64,
    hw: f64,
    hh: f64,
    rot_rad: f64,
    feather: f64,
    invert: bool,
) -> String {
    let cos_r = rot_rad.cos();
    let sin_r = rot_rad.sin();
    let ux = format!("((({px_expr})-{cx:.10})*{cos_r:.10})+((({py_expr})-{cy:.10})*{sin_r:.10})");
    let uy =
        format!("(-((({px_expr})-{cx:.10})*{sin_r:.10}))+((({py_expr})-{cy:.10})*{cos_r:.10})");

    if feather < 0.5 {
        let expr = format!(
            "between(abs({ux}),0,{hw:.10})*between(abs({uy}),0,{hh:.10})",
            hw = hw.max(0.0),
            hh = hh.max(0.0),
        );
        if invert {
            format!("(1-{})", expr)
        } else {
            expr
        }
    } else {
        let f2 = feather * 2.0;
        let ax = format!("clip(({hw:.10}+{feather:.10}-abs({ux}))/{f2:.10},0,1)");
        let ay = format!("clip(({hh:.10}+{feather:.10}-abs({uy}))/{f2:.10},0,1)");
        let sax = format!("({ax}*{ax}*(3-2*{ax}))");
        let say = format!("({ay}*{ay}*(3-2*{ay}))");
        let expr = format!("{sax}*{say}");
        if invert {
            format!("(1-{})", expr)
        } else {
            expr
        }
    }
}

fn build_rect_geq_expr(
    cx: f64,
    cy: f64,
    hw: f64,
    hh: f64,
    rot_rad: f64,
    feather: f64,
    invert: bool,
) -> String {
    build_rect_geq_expr_for_coords("X", "Y", cx, cy, hw, hh, rot_rad, feather, invert)
}

fn build_ellipse_geq_expr_for_coords(
    px_expr: &str,
    py_expr: &str,
    cx: f64,
    cy: f64,
    hw: f64,
    hh: f64,
    rot_rad: f64,
    feather: f64,
    invert: bool,
) -> String {
    let cos_r = rot_rad.cos();
    let sin_r = rot_rad.sin();

    let ux = format!("((({px_expr})-{cx:.10})*{cos_r:.10})+((({py_expr})-{cy:.10})*{sin_r:.10})");
    let uy =
        format!("(-((({px_expr})-{cx:.10})*{sin_r:.10}))+((({py_expr})-{cy:.10})*{cos_r:.10})");

    let hw_safe = hw.max(0.1);
    let hh_safe = hh.max(0.1);

    let r_expr = format!(
        "sqrt(({ux})*({ux})/({hw_safe:.10}*{hw_safe:.10})+({uy})*({uy})/({hh_safe:.10}*{hh_safe:.10}))"
    );

    if feather < 0.5 {
        let expr = format!("lte({r_expr},1)");
        if invert {
            format!("(1-{})", expr)
        } else {
            expr
        }
    } else {
        let min_axis = hw_safe.min(hh_safe);
        let f_norm = feather / min_axis;
        let inner = 1.0 - f_norm;
        let t_expr = format!("clip(({inner:.10}-{r_expr}+{f_norm:.10})/{f_norm:.10},0,1)");
        let expr = format!("({t_expr}*{t_expr}*(3-2*{t_expr}))");
        if invert {
            format!("(1-{})", expr)
        } else {
            expr
        }
    }
}

fn build_ellipse_geq_expr(
    cx: f64,
    cy: f64,
    hw: f64,
    hh: f64,
    rot_rad: f64,
    feather: f64,
    invert: bool,
) -> String {
    build_ellipse_geq_expr_for_coords("X", "Y", cx, cy, hw, hh, rot_rad, feather, invert)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::clip::{BezierPoint, MaskPath, MaskShape};

    fn default_rect_mask() -> ClipMask {
        ClipMask::new(MaskShape::Rectangle)
    }

    fn default_ellipse_mask() -> ClipMask {
        ClipMask::new(MaskShape::Ellipse)
    }

    #[test]
    fn rect_center_is_inside() {
        let mask = default_rect_mask();
        // Center of a 100x100 frame, mask centered at 0.5,0.5 with hw=0.25
        let alpha = mask_alpha_at_pixel(&mask, 50, 50, 100, 100, 0);
        assert!(
            (alpha - 1.0).abs() < 1e-6,
            "center should be fully opaque, got {alpha}"
        );
    }

    #[test]
    fn rect_corner_is_outside() {
        let mask = default_rect_mask();
        // Top-left corner: (0,0) normalized = (0.005, 0.005), mask covers 0.25..0.75
        let alpha = mask_alpha_at_pixel(&mask, 0, 0, 100, 100, 0);
        assert!(alpha < 0.01, "corner should be transparent, got {alpha}");
    }

    #[test]
    fn rect_invert() {
        let mut mask = default_rect_mask();
        mask.invert = true;
        let alpha_center = mask_alpha_at_pixel(&mask, 50, 50, 100, 100, 0);
        let alpha_corner = mask_alpha_at_pixel(&mask, 0, 0, 100, 100, 0);
        assert!(alpha_center < 0.01, "inverted center should be transparent");
        assert!(alpha_corner > 0.99, "inverted corner should be opaque");
    }

    #[test]
    fn ellipse_center_is_inside() {
        let mask = default_ellipse_mask();
        let alpha = mask_alpha_at_pixel(&mask, 50, 50, 100, 100, 0);
        assert!(
            (alpha - 1.0).abs() < 1e-6,
            "ellipse center should be opaque, got {alpha}"
        );
    }

    #[test]
    fn ellipse_far_outside() {
        let mask = default_ellipse_mask();
        let alpha = mask_alpha_at_pixel(&mask, 0, 0, 100, 100, 0);
        assert!(
            alpha < 0.01,
            "ellipse corner should be transparent, got {alpha}"
        );
    }

    #[test]
    fn feathered_rect_has_gradient() {
        let mut mask = default_rect_mask();
        mask.feather = 0.1;
        // Use a larger frame so the feather zone spans more pixels.
        // Mask covers x=0.25..0.75. With feather=0.1, the transition
        // extends from the edge inward. At x=0.70 (inside but near edge),
        // we should still have partial alpha.
        let w = 1000usize;
        let h = 1000usize;
        let alpha_inside = mask_alpha_at_pixel(&mask, 500, 500, w, h, 0);
        // x=720 → normalized 0.7205, well inside feather zone near edge at 0.75
        let alpha_partial = mask_alpha_at_pixel(&mask, 720, 500, w, h, 0);
        let alpha_outside = mask_alpha_at_pixel(&mask, 900, 500, w, h, 0);
        assert!(alpha_inside > 0.99, "deep inside should be opaque");
        assert!(
            alpha_partial > 0.01 && alpha_partial < 0.99,
            "near-edge should be partial: {alpha_partial}"
        );
        assert!(alpha_outside < 0.01, "far outside should be transparent");
    }

    #[test]
    fn apply_masks_modifies_alpha() {
        let mask = default_rect_mask();
        let masks = vec![mask];
        let w = 10usize;
        let h = 10usize;
        let mut buf = vec![255u8; w * h * 4]; // All white, fully opaque.
        apply_masks_to_rgba_buffer(&masks, &mut buf, w, h, 0);
        // Center pixel (5,5) should be opaque.
        let center_alpha = buf[(5 * w + 5) * 4 + 3];
        assert_eq!(center_alpha, 255, "center alpha should be 255");
        // Corner pixel (0,0) should be transparent.
        let corner_alpha = buf[(0 * w + 0) * 4 + 3];
        assert_eq!(corner_alpha, 0, "corner alpha should be 0");
    }

    #[test]
    fn serde_round_trip() {
        let mask = ClipMask::new(MaskShape::Ellipse);
        let json = serde_json::to_string(&mask).unwrap();
        let deserialized: ClipMask = serde_json::from_str(&json).unwrap();
        assert_eq!(mask.shape, deserialized.shape);
        assert!((mask.center_x - deserialized.center_x).abs() < 1e-9);
        assert_eq!(mask.enabled, deserialized.enabled);
    }

    #[test]
    fn ffmpeg_rect_expr_hard() {
        let mask = default_rect_mask();
        // scale=1.0, pos=0,0 → mask covers same region as normalized coords on full canvas
        let expr = build_mask_ffmpeg_geq_alpha(&mask, 1920, 1080, 1.0, 0.0, 0.0, "T");
        assert!(
            expr.contains("between"),
            "should use between for hard-edge rect: {expr}"
        );
    }

    #[test]
    fn ffmpeg_ellipse_expr_hard() {
        let mask = default_ellipse_mask();
        let expr = build_mask_ffmpeg_geq_alpha(&mask, 1920, 1080, 1.0, 0.0, 0.0, "T");
        assert!(
            expr.contains("lte"),
            "should use lte for hard-edge ellipse: {expr}"
        );
    }

    #[test]
    fn ffmpeg_ellipse_geq_precedence_center_inside() {
        // Verifies that the GEQ expression correctly squares ux/uy
        // (regression test for operator-precedence bug where ux*ux
        // was parsed as ux + last_term_of_ux * first_term_of_next_ux).
        let mut mask = default_ellipse_mask();
        mask.width = 0.09;
        mask.height = 0.22;
        // Build for a scaled clip — exercises the full coordinate mapping.
        let expr = build_mask_ffmpeg_geq_alpha(&mask, 1920, 1080, 2.0, 0.0, 0.0, "T");
        // The ellipse geq must produce 1 at the center and 0 far outside.
        // Centre pixel (960, 540):
        let center = eval_geq_lte(&expr, 960.0, 540.0);
        assert!(
            center > 0.99,
            "center should be inside the ellipse, got {center}"
        );
        // Far corner (0, 0):
        let corner = eval_geq_lte(&expr, 0.0, 0.0);
        assert!(
            corner < 0.01,
            "far corner should be outside the ellipse, got {corner}"
        );
        // A point just inside the horizontal edge:
        let inside_h = eval_geq_lte(&expr, 960.0 + 300.0, 540.0);
        assert!(
            inside_h > 0.99,
            "point inside horizontal extent should be opaque, got {inside_h}"
        );
        // A point well outside the horizontal edge:
        let outside_h = eval_geq_lte(&expr, 960.0 + 400.0, 540.0);
        assert!(
            outside_h < 0.01,
            "point outside horizontal extent should be transparent, got {outside_h}"
        );
    }

    /// Minimal evaluator for `lte(sqrt((...)*(...)/(...)+(...)*(...)/(...)),1)`
    /// style expressions produced by `build_ellipse_geq_expr_for_coords`.
    /// Replaces X and Y literals and evaluates using basic arithmetic.
    fn eval_geq_lte(expr: &str, x: f64, y: f64) -> f64 {
        // The expression is `lte(<sqrt_expr>, 1)` — extract the inner sqrt arg.
        let inner = expr
            .strip_prefix("lte(sqrt(")
            .and_then(|s| s.strip_suffix("),1)"))
            .expect("expected lte(sqrt(...),1) form");
        let val = eval_simple_expr(inner, x, y);
        if val.sqrt() <= 1.0 {
            1.0
        } else {
            0.0
        }
    }

    /// Evaluate a simple arithmetic expression with X and Y substituted.
    /// Supports +, -, *, / and nested parentheses.
    fn eval_simple_expr(expr: &str, x: f64, y: f64) -> f64 {
        let chars: Vec<char> = expr.chars().collect();
        let mut pos = 0;
        parse_additive(&chars, &mut pos, x, y)
    }

    fn parse_additive(chars: &[char], pos: &mut usize, x: f64, y: f64) -> f64 {
        let mut result = parse_multiplicative(chars, pos, x, y);
        while *pos < chars.len() {
            if chars[*pos] == '+' {
                *pos += 1;
                result += parse_multiplicative(chars, pos, x, y);
            } else if chars[*pos] == '-' && (*pos + 1 < chars.len()) && chars[*pos + 1] != '(' {
                // Careful: '-' could be unary or subtraction
                let saved = *pos;
                *pos += 1;
                let next = parse_multiplicative(chars, pos, x, y);
                result -= next;
                let _ = saved;
            } else {
                break;
            }
        }
        result
    }

    fn parse_multiplicative(chars: &[char], pos: &mut usize, x: f64, y: f64) -> f64 {
        let mut result = parse_unary(chars, pos, x, y);
        while *pos < chars.len() {
            if chars[*pos] == '*' {
                *pos += 1;
                result *= parse_unary(chars, pos, x, y);
            } else if chars[*pos] == '/' {
                *pos += 1;
                result /= parse_unary(chars, pos, x, y);
            } else {
                break;
            }
        }
        result
    }

    fn parse_unary(chars: &[char], pos: &mut usize, x: f64, y: f64) -> f64 {
        if *pos < chars.len() && chars[*pos] == '-' {
            *pos += 1;
            -parse_primary(chars, pos, x, y)
        } else {
            parse_primary(chars, pos, x, y)
        }
    }

    fn parse_primary(chars: &[char], pos: &mut usize, x: f64, y: f64) -> f64 {
        if *pos < chars.len() && chars[*pos] == '(' {
            *pos += 1; // skip '('
            let result = parse_additive(chars, pos, x, y);
            if *pos < chars.len() && chars[*pos] == ')' {
                *pos += 1;
            }
            return result;
        }
        if *pos < chars.len() && chars[*pos] == 'X' {
            *pos += 1;
            return x;
        }
        if *pos < chars.len() && chars[*pos] == 'Y' {
            *pos += 1;
            return y;
        }
        // Parse a number
        let start = *pos;
        while *pos < chars.len()
            && (chars[*pos].is_ascii_digit()
                || chars[*pos] == '.'
                || chars[*pos] == 'e'
                || chars[*pos] == 'E'
                || (chars[*pos] == '-'
                    && *pos > start
                    && (chars[*pos - 1] == 'e' || chars[*pos - 1] == 'E')))
        {
            *pos += 1;
        }
        let num_str: String = chars[start..*pos].iter().collect();
        num_str.parse::<f64>().unwrap_or(0.0)
    }

    // ── Path SDF tests ──────────────────────────────────────────────

    fn square_path() -> MaskPath {
        // A simple square path from (0.25,0.25) to (0.75,0.75) with straight edges.
        MaskPath {
            points: vec![
                BezierPoint {
                    x: 0.25,
                    y: 0.25,
                    handle_in_x: 0.0,
                    handle_in_y: 0.0,
                    handle_out_x: 0.0,
                    handle_out_y: 0.0,
                },
                BezierPoint {
                    x: 0.75,
                    y: 0.25,
                    handle_in_x: 0.0,
                    handle_in_y: 0.0,
                    handle_out_x: 0.0,
                    handle_out_y: 0.0,
                },
                BezierPoint {
                    x: 0.75,
                    y: 0.75,
                    handle_in_x: 0.0,
                    handle_in_y: 0.0,
                    handle_out_x: 0.0,
                    handle_out_y: 0.0,
                },
                BezierPoint {
                    x: 0.25,
                    y: 0.75,
                    handle_in_x: 0.0,
                    handle_in_y: 0.0,
                    handle_out_x: 0.0,
                    handle_out_y: 0.0,
                },
            ],
        }
    }

    #[test]
    fn subdivide_path_produces_polyline() {
        let path = square_path();
        let poly = subdivide_path(&path, 10);
        assert!(
            poly.len() >= 40,
            "expected at least 40 points, got {}",
            poly.len()
        );
    }

    #[test]
    fn winding_number_inside_square() {
        let path = square_path();
        let poly = subdivide_path(&path, 10);
        let wn = winding_number(0.5, 0.5, &poly);
        assert_ne!(wn, 0, "center of square should be inside");
    }

    #[test]
    fn winding_number_outside_square() {
        let path = square_path();
        let poly = subdivide_path(&path, 10);
        let wn = winding_number(0.1, 0.1, &poly);
        assert_eq!(wn, 0, "corner should be outside");
    }

    #[test]
    fn path_sdf_inside_negative() {
        let path = square_path();
        let poly = subdivide_path(&path, 10);
        let sdf = compute_path_sdf(0.5, 0.5, &poly, 0.0);
        assert!(sdf < 0.0, "center should have negative SDF, got {sdf}");
    }

    #[test]
    fn path_sdf_outside_positive() {
        let path = square_path();
        let poly = subdivide_path(&path, 10);
        let sdf = compute_path_sdf(0.1, 0.1, &poly, 0.0);
        assert!(sdf > 0.0, "outside should have positive SDF, got {sdf}");
    }

    #[test]
    fn path_mask_alpha_inside() {
        let mut mask = ClipMask::new_path(square_path().points);
        mask.enabled = true;
        let alpha = mask_alpha_at_pixel(&mask, 50, 50, 100, 100, 0);
        assert!(alpha > 0.99, "center should be opaque, got {alpha}");
    }

    #[test]
    fn path_mask_alpha_outside() {
        let mut mask = ClipMask::new_path(square_path().points);
        mask.enabled = true;
        let alpha = mask_alpha_at_pixel(&mask, 5, 5, 100, 100, 0);
        assert!(alpha < 0.01, "outside should be transparent, got {alpha}");
    }

    #[test]
    fn path_mask_serde_round_trip() {
        let mask = ClipMask::new_path(crate::model::clip::default_diamond_path().points);
        let json = serde_json::to_string(&mask).unwrap();
        let deserialized: ClipMask = serde_json::from_str(&json).unwrap();
        assert_eq!(mask.shape, deserialized.shape);
        assert!(deserialized.path.is_some());
        assert_eq!(deserialized.path.unwrap().points.len(), 4);
    }

    #[test]
    fn rasterize_masks_grayscale_basic() {
        let mask = ClipMask::new_path(square_path().points);
        let masks = vec![mask];
        let buf = rasterize_masks_to_grayscale(&masks, 100, 100, 0, 1.0, 0.0, 0.0);
        assert_eq!(buf.len(), 10000);
        // Center pixel should be opaque (255).
        assert_eq!(buf[50 * 100 + 50], 255);
        // Corner pixel should be transparent (0).
        assert_eq!(buf[5 * 100 + 5], 0);
    }

    #[test]
    fn prepared_canvas_masks_follow_current_frame_resolution() {
        let mut mask = default_ellipse_mask();
        mask.enabled = true;
        mask.center_x = 0.26;
        mask.center_y = 0.21;
        mask.width = 0.17;
        mask.height = 0.15;

        let prepared =
            prepare_canvas_masks(&[mask], 0, 1.0, 0.0, 0.0, 0.0).expect("prepared masks");

        assert!(
            prepared.alpha_at_canvas_pixel(26, 21, 100, 100) > 0.99,
            "100x100 center should stay inside the mask"
        );
        assert!(
            prepared.alpha_at_canvas_pixel(52, 42, 200, 200) > 0.99,
            "200x200 center should stay inside the mask"
        );
        assert!(
            prepared.alpha_at_canvas_pixel(52, 42, 100, 100) < 0.01,
            "sampling the doubled-resolution point on 100x100 should be outside"
        );
    }

    #[test]
    fn prepared_adjustment_masks_translate_even_at_full_scale() {
        let mut mask = default_ellipse_mask();
        mask.enabled = true;
        mask.center_x = 0.5;
        mask.center_y = 0.5;
        mask.width = 0.1;
        mask.height = 0.1;

        let prepared = prepare_adjustment_canvas_masks(&[mask], 0, 1.0, 0.5, 0.0, 0.0)
            .expect("prepared masks");

        assert!(
            prepared.alpha_at_canvas_pixel(75, 50, 100, 100) > 0.99,
            "translated adjustment mask should move right at full scale"
        );
        assert!(
            prepared.alpha_at_canvas_pixel(50, 50, 100, 100) < 0.01,
            "original center should be outside after translation"
        );
    }

    #[test]
    fn prepared_adjustment_masks_report_translated_bounds() {
        let mut mask = default_ellipse_mask();
        mask.enabled = true;
        mask.center_x = 0.5;
        mask.center_y = 0.5;
        mask.width = 0.1;
        mask.height = 0.1;

        let prepared = prepare_adjustment_canvas_masks(&[mask], 0, 1.0, 0.5, 0.0, 0.0)
            .expect("prepared masks");
        let bounds = prepared.pixel_bounds(100, 100).expect("mask bounds");

        assert_eq!(bounds, (65, 40, 85, 60));
    }
}
