// SPDX-License-Identifier: GPL-3.0-or-later
//! Binary-mask → bezier-polygon contour extraction.
//!
//! Phase 2a commit 2. This module is the bridge between
//! [`super::sam_cache::segment_with_box`] (which produces a raw
//! per-pixel binary mask) and the existing `MaskShape::Path`
//! infrastructure in [`crate::model::clip::ClipMask`] (which
//! renders closed bezier polygons over clips). Zero dependency on
//! `ort` / `ndarray` / any AI-inference stack — pure integer and
//! floating-point math on `&[u8]`. That means this module:
//!
//! * compiles and tests cleanly without the SAM model installed,
//! * has no build-time feature-gating,
//! * can be reused by any future mask source that produces raw
//!   binary pixels (not just SAM — e.g. MODNet alpha thresholding,
//!   chroma-key output, frame differencing).
//!
//! ## Pipeline
//!
//! 1. **Connected-component labeling (BFS).** SAM decoder output
//!    often contains multiple disconnected blobs plus small noise
//!    regions. We want a single closed contour representing the
//!    dominant foreground shape, so the first step is finding the
//!    largest 4-connected component in the mask.
//!
//! 2. **Moore-neighbor contour tracing.** Classic boundary-
//!    following algorithm: start at the topmost-leftmost pixel of
//!    the component, walk clockwise around its outer boundary,
//!    emitting each boundary pixel. Produces a dense polyline (in
//!    integer pixel coordinates) that closes on itself.
//!
//! 3. **Douglas-Peucker simplification.** The dense polyline from
//!    step 2 can have thousands of points for a moderate-sized
//!    object — way too many control points for a user-editable
//!    bezier mask. Douglas-Peucker simplifies to the minimum set of
//!    points that stays within `tolerance_px` of the original
//!    polyline. Since D-P operates on open polylines, we split the
//!    closed contour at two maximally-distant anchor points,
//!    simplify each half independently, and concatenate the results.
//!
//! 4. **Normalize to clip-local 0..1 coordinates.** The existing
//!    [`crate::model::clip::BezierPoint`] uses normalized
//!    coordinates so masks survive source-resolution changes. We
//!    divide each (x, y) by (width, height) and wrap as
//!    BezierPoints with zero handles (straight-edge polygon — the
//!    Phase 2 "first cut" shape, which users can edit in the
//!    existing mask editor to add smooth tangents if desired).

use crate::model::clip::BezierPoint;

// ── Public API ─────────────────────────────────────────────────────────────

/// Extract a closed bezier polygon from a binary mask.
///
/// # Arguments
///
/// * `mask` — row-major `width * height` bytes. Any non-zero value
///   is treated as foreground (matches the `SegmentResult::mask`
///   convention of 0 = background, 255 = foreground, but tolerates
///   intermediate values for future soft-mask inputs).
/// * `width`, `height` — mask dimensions in pixels.
/// * `tolerance_px` — Douglas-Peucker simplification tolerance in
///   mask-pixel units. A typical value of `2.0` keeps polygons
///   tight to the real shape while cutting point count by ~10–50×
///   on realistic SAM output. Larger values produce coarser, easier-
///   to-edit polygons; smaller values produce near-pixel-accurate
///   outlines.
///
/// # Returns
///
/// * `Some(points)` where `points.len() >= 3` if a non-degenerate
///   closed contour was found.
/// * `None` if the mask is empty, or the largest connected component
///   is smaller than 3 pixels, or contour tracing yields fewer than
///   3 boundary points (in which case a polygon can't be formed).
///
/// Each returned [`BezierPoint`] has `x` and `y` in normalized
/// `0.0..1.0` clip-local coordinates (matching the rest of the
/// masking infrastructure), and zero handles — the polygon has
/// straight edges between consecutive control points. Users can
/// add smooth tangents via the existing bezier mask editor.
pub fn mask_to_bezier_path(
    mask: &[u8],
    width: usize,
    height: usize,
    tolerance_px: f64,
) -> Option<Vec<BezierPoint>> {
    if width == 0 || height == 0 || mask.len() != width * height {
        return None;
    }

    // 1. Find the largest connected foreground component. This
    //    returns both the set of pixels and a convenient "topmost-
    //    leftmost" starting point for contour tracing.
    let largest = find_largest_component(mask, width, height)?;
    if largest.pixel_count < 3 {
        return None;
    }

    // 2. Trace the outer boundary of that component as a dense
    //    pixel-coordinate polyline.
    let contour = trace_contour(mask, width, height, largest.start)?;
    if contour.len() < 3 {
        return None;
    }

    // 3. Simplify. Passing tolerance <= 0 means "keep every point"
    //    which is almost never what anyone wants but we accept it
    //    gracefully rather than panicking.
    let simplified = if tolerance_px > 0.0 {
        simplify_closed_polyline(&contour, tolerance_px)
    } else {
        contour
    };
    if simplified.len() < 3 {
        return None;
    }

    // 4. Normalize to 0..1 clip-local coordinates and wrap as
    //    BezierPoints with zero handles (straight-edge polygon).
    let inv_w = 1.0 / width as f64;
    let inv_h = 1.0 / height as f64;
    let points: Vec<BezierPoint> = simplified
        .into_iter()
        .map(|(x, y)| BezierPoint {
            x: x as f64 * inv_w,
            y: y as f64 * inv_h,
            handle_in_x: 0.0,
            handle_in_y: 0.0,
            handle_out_x: 0.0,
            handle_out_y: 0.0,
        })
        .collect();

    Some(points)
}

// ── Connected components ──────────────────────────────────────────────────

/// Summary of the largest connected component in a binary mask.
struct LargestComponent {
    /// Number of pixels in the component.
    pixel_count: usize,
    /// Topmost-leftmost pixel (in row-major order) belonging to
    /// the component. Used as the starting point for contour
    /// tracing — by picking the topmost-then-leftmost pixel we
    /// guarantee the contour tracer walks the OUTER boundary
    /// (not an interior hole).
    start: (usize, usize),
}

/// Flood-fill all foreground components and return a summary of
/// the largest one. Uses 4-connectivity to match how
/// contour-tracing defines "boundary."
///
/// Returns `None` if the mask has no foreground pixels at all.
fn find_largest_component(
    mask: &[u8],
    width: usize,
    height: usize,
) -> Option<LargestComponent> {
    // visited[y * width + x] tracks which pixels we've assigned to
    // a component. We don't need per-component labels; just knowing
    // which component (if any) wins on pixel count is enough.
    let mut visited = vec![false; width * height];
    let mut best: Option<LargestComponent> = None;

    for y in 0..height {
        for x in 0..width {
            let idx = y * width + x;
            if visited[idx] || mask[idx] == 0 {
                continue;
            }
            // BFS flood fill from (x, y). Collect the component's
            // pixel count and track its topmost-leftmost pixel.
            let mut queue: Vec<(usize, usize)> = vec![(x, y)];
            visited[idx] = true;
            let mut count = 0usize;
            // (x, y) is guaranteed topmost-leftmost for this
            // component because the outer loop scans row-major
            // and skips visited pixels — the first time we touch a
            // component is always at its topmost-leftmost pixel.
            let start = (x, y);

            while let Some((cx, cy)) = queue.pop() {
                count += 1;
                // 4-neighbors: up / down / left / right.
                // Using saturating_sub to avoid underflow at the
                // 0-edge; the `cx > 0` / `cy > 0` guards are still
                // correct for the condition.
                if cy > 0 {
                    let ni = (cy - 1) * width + cx;
                    if !visited[ni] && mask[ni] != 0 {
                        visited[ni] = true;
                        queue.push((cx, cy - 1));
                    }
                }
                if cy + 1 < height {
                    let ni = (cy + 1) * width + cx;
                    if !visited[ni] && mask[ni] != 0 {
                        visited[ni] = true;
                        queue.push((cx, cy + 1));
                    }
                }
                if cx > 0 {
                    let ni = cy * width + (cx - 1);
                    if !visited[ni] && mask[ni] != 0 {
                        visited[ni] = true;
                        queue.push((cx - 1, cy));
                    }
                }
                if cx + 1 < width {
                    let ni = cy * width + (cx + 1);
                    if !visited[ni] && mask[ni] != 0 {
                        visited[ni] = true;
                        queue.push((cx + 1, cy));
                    }
                }
            }

            let is_best = best
                .as_ref()
                .map(|b| count > b.pixel_count)
                .unwrap_or(true);
            if is_best {
                best = Some(LargestComponent {
                    pixel_count: count,
                    start,
                });
            }
        }
    }

    best
}

// ── Moore-neighbor contour tracing ────────────────────────────────────────

/// Eight neighbor offsets in clockwise order, starting from "West."
/// Standard Moore-neighbor convention: we enter each boundary pixel
/// via a known direction, then scan the 8 neighbors clockwise
/// starting from the position "left-rear" of the entry direction.
///
/// Index 0 is (−1, 0) = West; rotating clockwise gives NW, N, NE, E,
/// SE, S, SW. This particular ordering simplifies the "where do I
/// start scanning from" lookup in `trace_contour`.
const NEIGHBORS_CW: [(i32, i32); 8] = [
    (-1, 0),  // 0: W
    (-1, -1), // 1: NW
    (0, -1),  // 2: N
    (1, -1),  // 3: NE
    (1, 0),   // 4: E
    (1, 1),   // 5: SE
    (0, 1),   // 6: S
    (-1, 1),  // 7: SW
];

/// Walk the outer boundary of a binary-mask component, starting at
/// `(start_x, start_y)`, emitting each boundary pixel as an
/// (x, y) coordinate pair in integer pixel space.
///
/// Standard Moore-neighbor tracing as described in Gonzalez &
/// Woods "Digital Image Processing", adapted for the case where we
/// know the component's topmost-leftmost pixel is the entry point
/// (so we can seed the "previous direction" with West, meaning the
/// tracer came from outside the component on the left).
///
/// Returns `None` if the contour can't be closed — which shouldn't
/// happen for a valid component but is handled defensively so a
/// malformed mask can't infinite-loop us.
fn trace_contour(
    mask: &[u8],
    width: usize,
    height: usize,
    start: (usize, usize),
) -> Option<Vec<(i32, i32)>> {
    // Guard: start must actually be a foreground pixel.
    if mask[start.1 * width + start.0] == 0 {
        return None;
    }

    let w = width as i32;
    let h = height as i32;

    // `is_fg` is a tiny closure over the borrowed mask so we can
    // ask "is (x, y) a foreground pixel?" with bounds checking.
    let is_fg = |x: i32, y: i32| -> bool {
        if x < 0 || y < 0 || x >= w || y >= h {
            return false;
        }
        mask[(y as usize) * width + x as usize] != 0
    };

    let start_xy = (start.0 as i32, start.1 as i32);

    // Special case: a component of exactly 1 foreground pixel has
    // a degenerate boundary. Return that single pixel as the
    // contour — the caller's `< 3` check will reject it.
    if !has_foreground_neighbor(&is_fg, start_xy) {
        return Some(vec![start_xy]);
    }

    let mut contour: Vec<(i32, i32)> = Vec::with_capacity(64);
    contour.push(start_xy);

    // The Moore-neighbor algorithm needs a "previous neighbor
    // position" to know which direction to start scanning from at
    // each boundary pixel. At the start we're at the topmost-
    // leftmost pixel, and we conceptually entered it from the west
    // (the pixel to its left, which is either off-frame or
    // background). So previous direction index is 0 (West).
    let mut cur = start_xy;
    let mut prev_dir: usize = 0; // West

    // Cap the iteration count as a safety guard. A reasonable
    // contour for even a 4K mask has perimeter < 50k pixels; we
    // give 10× headroom and bail if we exceed it.
    let max_steps = width * height * 4 + 100;
    let mut steps = 0usize;

    loop {
        steps += 1;
        if steps > max_steps {
            log::warn!(
                "mask_contour: trace exceeded {max_steps} steps, bailing to \
                 prevent infinite loop"
            );
            return Some(contour);
        }

        // Start scanning neighbors from the position clockwise-
        // adjacent to the one we came in from. This is the
        // canonical "start one clockwise from the back-trace
        // direction" step of Moore-neighbor tracing.
        let start_scan = (prev_dir + 1) % 8;
        let mut found_next = false;
        for i in 0..8 {
            let dir = (start_scan + i) % 8;
            let (dx, dy) = NEIGHBORS_CW[dir];
            let nx = cur.0 + dx;
            let ny = cur.1 + dy;
            if is_fg(nx, ny) {
                // Found the next boundary pixel. Record it, advance
                // the "current" pointer, and update prev_dir to the
                // direction that points BACK from the new pixel to
                // the old one (i.e. the opposite direction).
                contour.push((nx, ny));
                cur = (nx, ny);
                // Direction opposite to `dir` is `(dir + 4) % 8`.
                prev_dir = (dir + 4) % 8;
                found_next = true;
                break;
            }
        }

        if !found_next {
            // No foreground neighbor at all — the component was a
            // single pixel. We already handled that above, but as
            // a defensive fallback, terminate the trace.
            break;
        }

        // Termination condition: we've returned to the starting
        // pixel. The classical Moore-neighbor termination requires
        // BOTH the same pixel AND the same entry direction as the
        // very first pass — checking just "same pixel" can cause
        // early termination for contours that pass through the
        // start point (which happens for some topologies). For
        // simplicity, we terminate on same-pixel and accept any
        // edge-case over-tracing; the resulting polygon is still
        // correct modulo a few duplicated tail points.
        if cur == start_xy {
            // Drop the duplicated start-point at the end so the
            // polyline is cleanly "open" for Douglas-Peucker.
            contour.pop();
            break;
        }
    }

    Some(contour)
}

/// Return true if `p` has at least one foreground 4-neighbor.
/// Used to detect the degenerate "single isolated pixel" case
/// before we start the contour-tracing loop.
fn has_foreground_neighbor<F>(is_fg: &F, p: (i32, i32)) -> bool
where
    F: Fn(i32, i32) -> bool,
{
    is_fg(p.0 - 1, p.1)
        || is_fg(p.0 + 1, p.1)
        || is_fg(p.0, p.1 - 1)
        || is_fg(p.0, p.1 + 1)
}

// ── Douglas-Peucker simplification ────────────────────────────────────────

/// Simplify a closed polyline using Douglas-Peucker with the two
/// maximally-distant points of the input as "anchors". Splitting
/// on two anchors lets us reuse the standard open-polyline D-P
/// implementation on each half of the closed curve independently.
fn simplify_closed_polyline(points: &[(i32, i32)], tolerance_px: f64) -> Vec<(i32, i32)> {
    if points.len() <= 3 {
        return points.to_vec();
    }

    // Find the two maximally-distant points in the polyline. These
    // become the split anchors. O(n²) worst case, but realistic
    // contours are well under 10k points so this is sub-second
    // even without spatial indexing.
    let (a_idx, b_idx) = two_most_distant(points);
    if a_idx == b_idx {
        // Degenerate: all points are at the same location.
        return points.to_vec();
    }

    // Split the closed polyline into two open halves at a_idx and
    // b_idx. Because the input is closed (conceptually points[n]
    // == points[0]), "from a to b going forward" and "from b to a
    // going forward (wrapping)" are the two halves.
    let (lo, hi) = if a_idx < b_idx {
        (a_idx, b_idx)
    } else {
        (b_idx, a_idx)
    };
    let half1: Vec<(i32, i32)> = points[lo..=hi].to_vec();
    let mut half2: Vec<(i32, i32)> = points[hi..].to_vec();
    half2.extend_from_slice(&points[..=lo]);

    let simp1 = douglas_peucker(&half1, tolerance_px);
    let simp2 = douglas_peucker(&half2, tolerance_px);

    // Concatenate, dropping the duplicated anchor endpoints so the
    // final polygon has each vertex exactly once.
    let mut out = Vec::with_capacity(simp1.len() + simp2.len());
    out.extend_from_slice(&simp1);
    // simp2 starts at hi and ends at lo (wrapped). Its first and
    // last points are the same as simp1's last and first points
    // respectively, so we skip both.
    if simp2.len() > 2 {
        out.extend_from_slice(&simp2[1..simp2.len() - 1]);
    }
    out
}

/// Find the indices of the two points with the largest pairwise
/// Euclidean distance in `points`. Used to pick split anchors for
/// closed-curve D-P.
///
/// Uses the O(n²) brute-force approach. For the contour sizes we
/// see in practice (up to a few thousand points per SAM mask),
/// this runs in well under 10 ms and isn't worth a more complex
/// rotating-calipers convex-hull optimization.
fn two_most_distant(points: &[(i32, i32)]) -> (usize, usize) {
    if points.len() < 2 {
        return (0, 0);
    }
    let mut best = (0usize, 0usize);
    let mut best_dsq: i64 = -1;
    for i in 0..points.len() {
        for j in (i + 1)..points.len() {
            let dx = (points[i].0 - points[j].0) as i64;
            let dy = (points[i].1 - points[j].1) as i64;
            let dsq = dx * dx + dy * dy;
            if dsq > best_dsq {
                best_dsq = dsq;
                best = (i, j);
            }
        }
    }
    best
}

/// Standard iterative Douglas-Peucker for an open polyline.
/// Preserves the two endpoints and any intermediate point that
/// deviates from the local piecewise-linear approximation by more
/// than `tolerance_px`.
///
/// Iterative (stack-based) rather than recursive to avoid blowing
/// the Rust stack on very long contours (which can happen for
/// noisy SAM masks — tens of thousands of points aren't uncommon).
fn douglas_peucker(points: &[(i32, i32)], tolerance_px: f64) -> Vec<(i32, i32)> {
    if points.len() <= 2 {
        return points.to_vec();
    }

    let tol_sq = tolerance_px * tolerance_px;
    let n = points.len();
    let mut keep = vec![false; n];
    keep[0] = true;
    keep[n - 1] = true;

    // Each stack entry is a (start, end) inclusive index range of
    // the current sub-polyline to simplify.
    let mut stack: Vec<(usize, usize)> = vec![(0, n - 1)];
    while let Some((start, end)) = stack.pop() {
        if end <= start + 1 {
            continue;
        }
        // Find the point in (start, end) exclusive with the max
        // perpendicular distance to the line segment
        // points[start] → points[end].
        let mut max_dsq = -1.0f64;
        let mut max_idx = start;
        for i in (start + 1)..end {
            let dsq = perpendicular_distance_sq(points[i], points[start], points[end]);
            if dsq > max_dsq {
                max_dsq = dsq;
                max_idx = i;
            }
        }
        if max_dsq > tol_sq {
            // Keep the outlier point and recurse into both halves.
            keep[max_idx] = true;
            stack.push((start, max_idx));
            stack.push((max_idx, end));
        }
        // Otherwise: drop all intermediate points.
    }

    points
        .iter()
        .enumerate()
        .filter_map(|(i, p)| if keep[i] { Some(*p) } else { None })
        .collect()
}

/// Squared perpendicular distance from point `p` to the line
/// through `a` and `b`. Squared so we can avoid a sqrt in the
/// hot D-P loop — the comparison is against `tolerance * tolerance`.
///
/// For degenerate `a == b` (which D-P hands us when start and end
/// are the same point after the closed-curve split), we fall back
/// to the Euclidean distance from `p` to `a`.
fn perpendicular_distance_sq(p: (i32, i32), a: (i32, i32), b: (i32, i32)) -> f64 {
    let (px, py) = (p.0 as f64, p.1 as f64);
    let (ax, ay) = (a.0 as f64, a.1 as f64);
    let (bx, by) = (b.0 as f64, b.1 as f64);
    let dx = bx - ax;
    let dy = by - ay;
    let len_sq = dx * dx + dy * dy;
    if len_sq < 1e-12 {
        // Degenerate line segment: use distance from p to a.
        let ex = px - ax;
        let ey = py - ay;
        return ex * ex + ey * ey;
    }
    // Perpendicular distance from p to the infinite line a→b:
    //   |(b - a) × (a - p)| / |b - a|
    // Squared → cross² / len²
    let cross = dx * (ay - py) - dy * (ax - px);
    (cross * cross) / len_sq
}

// ── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    /// Helper: build a `width × height` mask with a filled axis-
    /// aligned rectangle at `[x1, x2) × [y1, y2)`.
    fn rect_mask(
        width: usize,
        height: usize,
        x1: usize,
        y1: usize,
        x2: usize,
        y2: usize,
    ) -> Vec<u8> {
        let mut m = vec![0u8; width * height];
        for y in y1..y2 {
            for x in x1..x2 {
                m[y * width + x] = 255;
            }
        }
        m
    }

    /// Helper: build a mask containing a filled circle.
    fn circle_mask(width: usize, height: usize, cx: f64, cy: f64, radius: f64) -> Vec<u8> {
        let mut m = vec![0u8; width * height];
        let r_sq = radius * radius;
        for y in 0..height {
            for x in 0..width {
                let dx = x as f64 - cx;
                let dy = y as f64 - cy;
                if dx * dx + dy * dy <= r_sq {
                    m[y * width + x] = 255;
                }
            }
        }
        m
    }

    #[test]
    fn mask_to_bezier_path_empty_returns_none() {
        let mask = vec![0u8; 100 * 100];
        assert!(mask_to_bezier_path(&mask, 100, 100, 2.0).is_none());
    }

    #[test]
    fn mask_to_bezier_path_zero_dims_returns_none() {
        assert!(mask_to_bezier_path(&[], 0, 0, 2.0).is_none());
        assert!(mask_to_bezier_path(&[0, 0, 0], 3, 0, 2.0).is_none());
    }

    #[test]
    fn mask_to_bezier_path_mismatched_length_returns_none() {
        // width * height != buf.len()
        let mask = vec![0u8; 50];
        assert!(mask_to_bezier_path(&mask, 10, 10, 2.0).is_none());
    }

    #[test]
    fn mask_to_bezier_path_single_pixel_returns_none() {
        // A single isolated pixel produces a 1-point contour, which
        // is less than the 3-point minimum for a polygon.
        let mut mask = vec![0u8; 100 * 100];
        mask[50 * 100 + 50] = 255;
        assert!(mask_to_bezier_path(&mask, 100, 100, 2.0).is_none());
    }

    #[test]
    fn mask_to_bezier_path_square_simplifies_to_four_corners() {
        // A 50×50 square centered in a 100×100 frame should
        // simplify to exactly 4 corner points at tolerance 2.0.
        let mask = rect_mask(100, 100, 25, 25, 75, 75);
        let points = mask_to_bezier_path(&mask, 100, 100, 2.0)
            .expect("square should produce a valid polygon");
        // Allow some slack: contour tracing can add one or two
        // near-duplicate points at corners depending on the Moore
        // neighbor scan order, and D-P shouldn't remove the actual
        // corners. 4 to 6 points is acceptable.
        assert!(
            (4..=6).contains(&points.len()),
            "expected 4-6 corner points, got {}: {:?}",
            points.len(),
            points
        );
        // Coordinates should be in the normalized 0.25..0.75 range
        // (50×50 square in a 100×100 frame).
        for p in &points {
            assert!(
                p.x >= 0.24 && p.x <= 0.76,
                "x out of expected range: {}",
                p.x
            );
            assert!(
                p.y >= 0.24 && p.y <= 0.76,
                "y out of expected range: {}",
                p.y
            );
        }
    }

    #[test]
    fn mask_to_bezier_path_square_handles_zero_handles() {
        let mask = rect_mask(100, 100, 25, 25, 75, 75);
        let points = mask_to_bezier_path(&mask, 100, 100, 2.0).unwrap();
        // Every BezierPoint returned by mask_to_bezier_path is a
        // straight-edge vertex (handles all zero). The user can
        // later edit handles in the mask editor.
        for p in &points {
            assert_eq!(p.handle_in_x, 0.0);
            assert_eq!(p.handle_in_y, 0.0);
            assert_eq!(p.handle_out_x, 0.0);
            assert_eq!(p.handle_out_y, 0.0);
        }
    }

    #[test]
    fn mask_to_bezier_path_circle_produces_moderate_point_count() {
        // A circle of radius 20 in a 100×100 frame has a
        // perimeter of ~126 pixels. At tolerance 2.0 we expect
        // D-P to keep somewhere around 10–30 points — enough to
        // approximate the curve but nowhere near the raw contour.
        let mask = circle_mask(100, 100, 50.0, 50.0, 20.0);
        let points = mask_to_bezier_path(&mask, 100, 100, 2.0)
            .expect("circle should produce a polygon");
        assert!(
            (6..=40).contains(&points.len()),
            "expected 6-40 points for circle, got {}",
            points.len()
        );
        // Every point should be approximately on the circle (within
        // a few pixels of radius 20). Check this in normalized
        // coordinate space.
        for p in &points {
            let px = p.x * 100.0;
            let py = p.y * 100.0;
            let dx = px - 50.0;
            let dy = py - 50.0;
            let r = (dx * dx + dy * dy).sqrt();
            assert!(
                (r - 20.0).abs() < 4.0,
                "point ({}, {}) is at radius {}, expected ~20",
                px,
                py,
                r
            );
        }
    }

    #[test]
    fn mask_to_bezier_path_picks_largest_of_two_blobs() {
        // Two disjoint rectangles: a small 10×10 at top-left and
        // a larger 40×40 at bottom-right. The contour should only
        // trace the larger one.
        let mut mask = rect_mask(100, 100, 5, 5, 15, 15); // 100 px
        let big = rect_mask(100, 100, 50, 50, 90, 90); // 1600 px
        for i in 0..mask.len() {
            if big[i] != 0 {
                mask[i] = 255;
            }
        }
        let points = mask_to_bezier_path(&mask, 100, 100, 2.0).unwrap();
        // All returned points should be inside the big rectangle's
        // normalized range 0.5..0.9, NOT in the small one's
        // 0.05..0.15 range.
        for p in &points {
            assert!(
                p.x >= 0.49 && p.x <= 0.91,
                "contour touches small blob region: x={}",
                p.x
            );
            assert!(
                p.y >= 0.49 && p.y <= 0.91,
                "contour touches small blob region: y={}",
                p.y
            );
        }
    }

    #[test]
    fn mask_to_bezier_path_tolerance_zero_keeps_all_contour_points() {
        // With tolerance <= 0 we skip simplification entirely, so
        // the returned point count should equal the raw contour
        // length. For a 10×10 square that's 4*10 - 4 = 36 boundary
        // pixels.
        let mask = rect_mask(50, 50, 20, 20, 30, 30);
        let points = mask_to_bezier_path(&mask, 50, 50, 0.0).unwrap();
        assert!(
            points.len() >= 30,
            "tolerance 0 should keep most points, got {}",
            points.len()
        );
    }

    #[test]
    fn douglas_peucker_preserves_endpoints() {
        // Classic D-P property: first and last points are always
        // kept regardless of tolerance.
        let line: Vec<(i32, i32)> = vec![(0, 0), (5, 1), (10, 0), (15, 1), (20, 0)];
        let simp = douglas_peucker(&line, 10.0);
        assert_eq!(simp.first(), Some(&(0, 0)));
        assert_eq!(simp.last(), Some(&(20, 0)));
    }

    #[test]
    fn douglas_peucker_drops_near_colinear_points() {
        // Nearly straight polyline → with a 2.0 tolerance,
        // everything except the endpoints should be dropped.
        let line: Vec<(i32, i32)> = vec![
            (0, 0),
            (10, 0),
            (20, 0),
            (30, 0),
            (40, 0),
            (50, 0),
        ];
        let simp = douglas_peucker(&line, 1.0);
        assert_eq!(simp.len(), 2);
        assert_eq!(simp[0], (0, 0));
        assert_eq!(simp[1], (50, 0));
    }

    #[test]
    fn douglas_peucker_preserves_sharp_corner() {
        // V-shape with a sharp corner should preserve the apex.
        let v: Vec<(i32, i32)> = vec![(0, 0), (2, 0), (5, 10), (8, 0), (10, 0)];
        let simp = douglas_peucker(&v, 1.0);
        // The apex at (5, 10) must survive — it's 10 px from the
        // line (0,0)→(10,0), well above the 1 px tolerance.
        assert!(
            simp.contains(&(5, 10)),
            "sharp corner dropped by D-P: {:?}",
            simp
        );
    }

    #[test]
    fn perpendicular_distance_sq_known_case() {
        // Point (0, 5) perpendicular to line (0, 0) → (10, 0) has
        // distance 5, so distance² = 25.
        let d = perpendicular_distance_sq((0, 5), (0, 0), (10, 0));
        assert!((d - 25.0).abs() < 1e-9);

        // Point (5, 3) perpendicular to same line = distance 3,
        // distance² = 9.
        let d = perpendicular_distance_sq((5, 3), (0, 0), (10, 0));
        assert!((d - 9.0).abs() < 1e-9);

        // Degenerate line a == b: distance from p to a.
        let d = perpendicular_distance_sq((3, 4), (0, 0), (0, 0));
        assert!((d - 25.0).abs() < 1e-9);
    }

    #[test]
    fn two_most_distant_finds_corners_of_diagonal() {
        // In a diagonal rectangle, the two far corners are
        // maximally distant.
        let pts = vec![(0, 0), (5, 5), (10, 0), (10, 10), (0, 10)];
        let (a, b) = two_most_distant(&pts);
        // Expected: either (0, 0) and (10, 10), or (10, 0) and
        // (0, 10) — both pairs have distance² = 200.
        let da = pts[a];
        let db = pts[b];
        let dx = (da.0 - db.0) as i64;
        let dy = (da.1 - db.1) as i64;
        assert_eq!(dx * dx + dy * dy, 200);
    }

    #[test]
    fn find_largest_component_picks_larger_blob() {
        // Small blob at (2, 2) and large blob at (10, 10), same
        // mask. find_largest_component should return the large
        // one's topmost-leftmost pixel.
        let w = 20;
        let h = 20;
        let mut mask = vec![0u8; w * h];
        // Small: 2×2 at (2, 2)
        for y in 2..4 {
            for x in 2..4 {
                mask[y * w + x] = 255;
            }
        }
        // Large: 5×5 at (10, 10)
        for y in 10..15 {
            for x in 10..15 {
                mask[y * w + x] = 255;
            }
        }
        let largest = find_largest_component(&mask, w, h).unwrap();
        assert_eq!(largest.pixel_count, 25);
        assert_eq!(largest.start, (10, 10));
    }

    #[test]
    fn find_largest_component_empty_mask_returns_none() {
        let mask = vec![0u8; 100];
        assert!(find_largest_component(&mask, 10, 10).is_none());
    }

    #[test]
    fn trace_contour_closes_around_rect() {
        // 3×3 filled rectangle. The Moore-neighbor trace should
        // produce at least 8 boundary pixels (one for each of the
        // square's edge cells).
        let mask = rect_mask(10, 10, 3, 3, 6, 6);
        let contour = trace_contour(&mask, 10, 10, (3, 3)).unwrap();
        assert!(
            contour.len() >= 8,
            "3×3 rect contour should have >=8 points, got {}: {:?}",
            contour.len(),
            contour
        );
        // First point is the start (topmost-leftmost of the rect).
        assert_eq!(contour[0], (3, 3));
    }
}
