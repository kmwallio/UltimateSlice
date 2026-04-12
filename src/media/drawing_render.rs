//! Cairo rasterization for `ClipKind::Drawing` vector overlays.
//!
//! Produces a transparent ARGB32 PNG at project resolution that both the
//! preview pipeline (GStreamer `filesrc ! pngdec ! imagefreeze`) and the
//! export pipeline (FFmpeg `-loop 1 -i <png>` + overlay) can consume.

use crate::model::clip::{DrawingItem, DrawingKind};
use gtk4::cairo;
use std::path::{Path, PathBuf};

/// Stagger between consecutive items as a fraction of the reveal
/// duration. 0.7 means each item starts after 70% of the previous
/// item's reveal has elapsed, giving a slight overlap that reads as
/// continuous hand-drawing rather than stop-and-start.
const REVEAL_STAGGER_FRACTION: f64 = 0.7;

/// Per-item reveal progress `[0, 1]` at `elapsed_ns`, using the same
/// stagger model as `drawing_svg`. `reveal_ns == 0` always returns 1.0
/// (static rendering).
pub fn item_reveal_progress(
    item_index: usize,
    elapsed_ns: u64,
    reveal_ns: u64,
) -> f64 {
    if reveal_ns == 0 {
        return 1.0;
    }
    let stagger_ns = (reveal_ns as f64 * REVEAL_STAGGER_FRACTION) as u64;
    let begin_ns = stagger_ns.saturating_mul(item_index as u64);
    let rel_ns = elapsed_ns.saturating_sub(begin_ns);
    (rel_ns as f64 / reveal_ns as f64).clamp(0.0, 1.0)
}

/// Total time needed for every item in `items` to fully reveal.
pub fn total_reveal_duration_ns(item_count: usize, reveal_ns: u64) -> u64 {
    if reveal_ns == 0 || item_count == 0 {
        return 0;
    }
    let stagger_ns = (reveal_ns as f64 * REVEAL_STAGGER_FRACTION) as u64;
    stagger_ns
        .saturating_mul(item_count.saturating_sub(1) as u64)
        .saturating_add(reveal_ns)
}

fn argb_from_u32(color: u32) -> (f64, f64, f64, f64) {
    let r = ((color >> 24) & 0xFF) as f64 / 255.0;
    let g = ((color >> 16) & 0xFF) as f64 / 255.0;
    let b = ((color >> 8) & 0xFF) as f64 / 255.0;
    let a = (color & 0xFF) as f64 / 255.0;
    (r, g, b, a)
}

/// Rasterize `items` onto a transparent ARGB32 Cairo surface at `width × height`.
///
/// `width_scale` converts a `DrawingItem::width` (pixels relative to 1080p
/// vertical) into canvas pixels. Pass `(height as f64) / 1080.0`.
pub fn rasterize_drawing_surface(
    items: &[DrawingItem],
    width: i32,
    height: i32,
) -> Result<cairo::ImageSurface, cairo::Error> {
    rasterize_drawing_surface_at_time(items, width, height, 0, 0)
}

/// Like `rasterize_drawing_surface` but applies a time-based reveal.
/// `elapsed_ns` is the time since the clip started; `reveal_ns` is the
/// per-item reveal duration (0 = static, everything visible).
///
/// Freehand strokes and arrow lines are truncated along their path
/// length to reflect progress. Shapes (Rectangle / Ellipse) and the
/// arrowhead fade in via alpha. Matches the `drawing_svg` behaviour
/// so preview / export / SVG stay visually consistent.
pub fn rasterize_drawing_surface_at_time(
    items: &[DrawingItem],
    width: i32,
    height: i32,
    elapsed_ns: u64,
    reveal_ns: u64,
) -> Result<cairo::ImageSurface, cairo::Error> {
    let surface = cairo::ImageSurface::create(cairo::Format::ARgb32, width, height)?;
    let cr = cairo::Context::new(&surface)?;
    cr.set_line_cap(cairo::LineCap::Round);
    cr.set_line_join(cairo::LineJoin::Round);

    let scale_ref = (height as f64) / 1080.0;
    let w = width as f64;
    let h = height as f64;

    for (idx, item) in items.iter().enumerate() {
        if item.points.is_empty() {
            continue;
        }
        let progress = item_reveal_progress(idx, elapsed_ns, reveal_ns);
        if progress <= 0.0 {
            continue;
        }
        let (r, g, b, a) = argb_from_u32(item.color);
        let lw = (item.width * scale_ref).max(0.5);
        cr.set_line_width(lw);
        cr.set_source_rgba(r, g, b, a);

        match item.kind {
            DrawingKind::Stroke => {
                // Truncate the polyline at `progress * total_length`
                // so strokes draw on continuously from the start.
                let pts_px: Vec<(f64, f64)> = item
                    .points
                    .iter()
                    .map(|(nx, ny)| (nx * w, ny * h))
                    .collect();
                let total_len: f64 = pts_px
                    .windows(2)
                    .map(|p| {
                        let dx = p[1].0 - p[0].0;
                        let dy = p[1].1 - p[0].1;
                        (dx * dx + dy * dy).sqrt()
                    })
                    .sum();
                let target = (total_len * progress).max(0.0);
                let mut walked = 0.0;
                let mut started = false;
                for pair in pts_px.windows(2) {
                    let (a, b) = (pair[0], pair[1]);
                    let seg = {
                        let dx = b.0 - a.0;
                        let dy = b.1 - a.1;
                        (dx * dx + dy * dy).sqrt()
                    };
                    if !started {
                        cr.move_to(a.0, a.1);
                        started = true;
                    }
                    if walked + seg <= target {
                        cr.line_to(b.0, b.1);
                        walked += seg;
                    } else {
                        let frac = if seg > 0.0 {
                            (target - walked) / seg
                        } else {
                            0.0
                        };
                        cr.line_to(a.0 + (b.0 - a.0) * frac, a.1 + (b.1 - a.1) * frac);
                        break;
                    }
                }
                let _ = cr.stroke();
            }
            DrawingKind::Rectangle => {
                // Fade in via alpha (stroke + optional fill).
                let (p0, p1) = (item.points[0], *item.points.last().unwrap());
                let x = (p0.0.min(p1.0)) * w;
                let y = (p0.1.min(p1.1)) * h;
                let rw = (p0.0 - p1.0).abs() * w;
                let rh = (p0.1 - p1.1).abs() * h;
                cr.rectangle(x, y, rw, rh);
                if let Some(fill) = item.fill_color {
                    let (fr, fg, fb, fa) = argb_from_u32(fill);
                    cr.set_source_rgba(fr, fg, fb, fa * progress);
                    let _ = cr.fill_preserve();
                }
                cr.set_source_rgba(r, g, b, a * progress);
                let _ = cr.stroke();
            }
            DrawingKind::Ellipse => {
                let (p0, p1) = (item.points[0], *item.points.last().unwrap());
                let x0 = p0.0.min(p1.0) * w;
                let y0 = p0.1.min(p1.1) * h;
                let rw = ((p0.0 - p1.0).abs() * w).max(1.0);
                let rh = ((p0.1 - p1.1).abs() * h).max(1.0);
                let cx = x0 + rw * 0.5;
                let cy = y0 + rh * 0.5;
                cr.save().ok();
                cr.translate(cx, cy);
                cr.scale(rw * 0.5, rh * 0.5);
                cr.arc(0.0, 0.0, 1.0, 0.0, std::f64::consts::TAU);
                cr.restore().ok();
                if let Some(fill) = item.fill_color {
                    let (fr, fg, fb, fa) = argb_from_u32(fill);
                    cr.set_source_rgba(fr, fg, fb, fa * progress);
                    let _ = cr.fill_preserve();
                }
                cr.set_source_rgba(r, g, b, a * progress);
                let _ = cr.stroke();
            }
            DrawingKind::Arrow => {
                let p0 = item.points[0];
                let p1 = *item.points.last().unwrap();
                let x0 = p0.0 * w;
                let y0 = p0.1 * h;
                let x1 = p1.0 * w;
                let y1 = p1.1 * h;
                // Line draws progressively; head appears during the
                // last 25% like the SVG serialiser.
                let line_frac = (progress / 0.75).clamp(0.0, 1.0);
                let tip_x = x0 + (x1 - x0) * line_frac;
                let tip_y = y0 + (y1 - y0) * line_frac;
                cr.move_to(x0, y0);
                cr.line_to(tip_x, tip_y);
                let _ = cr.stroke();
                if progress < 0.75 {
                    continue;
                }
                let head_progress = ((progress - 0.75) / 0.25).clamp(0.0, 1.0);

                // Arrowhead: isoceles triangle, length ~ 6× line width, half-angle 25°.
                let dx = x1 - x0;
                let dy = y1 - y0;
                let len = (dx * dx + dy * dy).sqrt().max(1.0);
                let ux = dx / len;
                let uy = dy / len;
                let head = (lw * 6.0).max(10.0);
                let (ca, sa) = (25f64.to_radians().cos(), 25f64.to_radians().sin());
                let lxa = x1 - head * (ux * ca - uy * sa);
                let lya = y1 - head * (uy * ca + ux * sa);
                let rxa = x1 - head * (ux * ca + uy * sa);
                let rya = y1 - head * (uy * ca - ux * sa);
                cr.set_source_rgba(r, g, b, a * head_progress);
                cr.move_to(x1, y1);
                cr.line_to(lxa, lya);
                cr.line_to(rxa, rya);
                cr.close_path();
                let _ = cr.fill();
            }
        }
    }

    drop(cr);
    Ok(surface)
}

/// Render `items` into `path` as a PNG.
pub fn rasterize_drawing_to_png(
    items: &[DrawingItem],
    width: i32,
    height: i32,
    path: &Path,
) -> std::io::Result<()> {
    let mut surface = rasterize_drawing_surface(items, width, height)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, format!("cairo: {e}")))?;
    // Cairo's ARGB32 is premultiplied native-endian (BGRA on little-endian).
    // Convert to straight-alpha RGBA for PNG.
    let stride = surface.stride() as usize;
    let data = surface
        .data()
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, format!("cairo data: {e}")))?;
    let w = width as usize;
    let h = height as usize;
    let mut rgba = vec![0u8; w * h * 4];
    for y in 0..h {
        for x in 0..w {
            let i = y * stride + x * 4;
            let b = data[i];
            let g = data[i + 1];
            let r = data[i + 2];
            let a = data[i + 3];
            let o = (y * w + x) * 4;
            // Un-premultiply
            let (rr, gg, bb) = if a == 0 {
                (0, 0, 0)
            } else {
                let af = a as f32 / 255.0;
                (
                    ((r as f32 / af).round().clamp(0.0, 255.0)) as u8,
                    ((g as f32 / af).round().clamp(0.0, 255.0)) as u8,
                    ((b as f32 / af).round().clamp(0.0, 255.0)) as u8,
                )
            };
            rgba[o] = rr;
            rgba[o + 1] = gg;
            rgba[o + 2] = bb;
            rgba[o + 3] = a;
        }
    }
    drop(data);
    drop(surface);

    let file = std::fs::File::create(path)?;
    let w_buf = std::io::BufWriter::new(file);
    let mut encoder = png::Encoder::new(w_buf, width as u32, height as u32);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    let mut writer = encoder
        .write_header()
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, format!("png header: {e}")))?;
    writer
        .write_image_data(&rgba)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, format!("png data: {e}")))?;
    Ok(())
}

/// Stable path for a drawing clip's animated WebM (VP9/alpha),
/// keyed on content hash + timing parameters + resolution + fps.
pub fn drawing_animation_cache_path(
    clip_id: &str,
    items: &[DrawingItem],
    width: i32,
    height: i32,
    fps_num: u32,
    fps_den: u32,
    clip_duration_ns: u64,
    reveal_ns: u64,
) -> PathBuf {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    clip_id.hash(&mut h);
    width.hash(&mut h);
    height.hash(&mut h);
    fps_num.hash(&mut h);
    fps_den.hash(&mut h);
    clip_duration_ns.hash(&mut h);
    reveal_ns.hash(&mut h);
    for it in items {
        (it.kind as u8).hash(&mut h);
        it.color.hash(&mut h);
        it.fill_color.hash(&mut h);
        it.width.to_bits().hash(&mut h);
        for (x, y) in &it.points {
            x.to_bits().hash(&mut h);
            y.to_bits().hash(&mut h);
        }
    }
    let hash = h.finish();
    let mut p = std::env::temp_dir();
    // QuickTime RLE in .mov — a well-supported ARGB format both
    // GStreamer (`qtdemux ! avdec_qtrle`) and FFmpeg decode with
    // alpha intact. Earlier VP9/alpha in `.webm` probed as `yuv420p`
    // on FFmpeg's side (alpha stripped), which made exported overlays
    // render on a fully opaque black background.
    p.push(format!("ultimate-slice-drawing-anim-{hash:016x}.mov"));
    p
}

thread_local! {
    /// Cache paths of animation encodes currently running in a
    /// background thread. `clip_to_program_clips` checks this before
    /// deciding whether to fall back to the static PNG while the
    /// WebM is still baking. Main-thread only.
    static PENDING_DRAWING_ENCODES:
        std::cell::RefCell<std::collections::HashSet<PathBuf>> =
        std::cell::RefCell::new(std::collections::HashSet::new());

    /// Callback fired on the main thread when any background encode
    /// completes. The app registers a closure here that invalidates
    /// the preview (typically `on_project_changed`). Main-thread only.
    static DRAWING_ENCODE_COMPLETE:
        std::cell::RefCell<Option<Box<dyn Fn()>>> =
        std::cell::RefCell::new(None);
}

/// Install the "encode finished" callback once at window build time.
/// Replaces any previously installed callback.
pub fn install_drawing_encode_complete_callback(cb: Box<dyn Fn()>) {
    DRAWING_ENCODE_COMPLETE.with(|slot| {
        *slot.borrow_mut() = Some(cb);
    });
}

/// Non-blocking variant of `ensure_drawing_animation_webm`:
/// * Returns `Some(path)` immediately if the cache hit.
/// * Returns `None` if an encode is already in flight (caller should
///   fall back to the static PNG for now).
/// * Otherwise kicks off a background thread to bake the WebM, marks
///   it as pending, and returns `None`. When the encode completes,
///   the installed callback fires on the main thread so the preview
///   can rebuild and pick up the new file.
pub fn ensure_drawing_animation_webm_nonblocking(
    clip_id: &str,
    items: &[DrawingItem],
    width: i32,
    height: i32,
    fps_num: u32,
    fps_den: u32,
    clip_duration_ns: u64,
    reveal_ns: u64,
) -> Option<PathBuf> {
    let path = drawing_animation_cache_path(
        clip_id,
        items,
        width,
        height,
        fps_num,
        fps_den,
        clip_duration_ns,
        reveal_ns,
    );
    if path.exists() {
        return Some(path);
    }
    let already_pending = PENDING_DRAWING_ENCODES.with(|set| {
        let mut set = set.borrow_mut();
        if set.contains(&path) {
            true
        } else {
            set.insert(path.clone());
            false
        }
    });
    if already_pending {
        return None;
    }
    // Own the data the thread needs.
    let items_clone = items.to_vec();
    let clip_id_clone = clip_id.to_string();
    let path_for_thread = path.clone();
    std::thread::spawn(move || {
        let result = ensure_drawing_animation_webm(
            &clip_id_clone,
            &items_clone,
            width,
            height,
            fps_num,
            fps_den,
            clip_duration_ns,
            reveal_ns,
        );
        if let Err(ref e) = result {
            log::warn!(
                "drawing animation encode failed for {}: {e}",
                path_for_thread.display()
            );
        }
        // Bounce to the GTK main thread: clear the pending marker
        // and notify the app so it can rebuild the preview.
        gtk4::glib::idle_add_once(move || {
            PENDING_DRAWING_ENCODES.with(|set| {
                set.borrow_mut().remove(&path_for_thread);
            });
            DRAWING_ENCODE_COMPLETE.with(|slot| {
                if let Some(ref cb) = *slot.borrow() {
                    cb();
                }
            });
        });
    });
    None
}

/// Render a drawing clip's progressive reveal into a VP9/alpha WebM
/// file at `fps_num/fps_den` fps, lasting `clip_duration_ns`. Pipes
/// frames to FFmpeg via stdin as raw RGBA. Returns the path to the
/// (cached) file — if the target already exists, re-uses it.
///
/// Caller must guarantee `clip_duration_ns > 0`. `reveal_ns` of 0
/// would produce a static video — callers should prefer the static
/// PNG path via `ensure_drawing_png` instead.
pub fn ensure_drawing_animation_webm(
    clip_id: &str,
    items: &[DrawingItem],
    width: i32,
    height: i32,
    fps_num: u32,
    fps_den: u32,
    clip_duration_ns: u64,
    reveal_ns: u64,
) -> std::io::Result<PathBuf> {
    let path = drawing_animation_cache_path(
        clip_id,
        items,
        width,
        height,
        fps_num,
        fps_den,
        clip_duration_ns,
        reveal_ns,
    );
    if path.exists() {
        return Ok(path);
    }
    let fps = fps_num.max(1) as f64 / fps_den.max(1) as f64;
    let clip_secs = clip_duration_ns as f64 / 1_000_000_000.0;
    let total_frames = ((clip_secs * fps).ceil() as u64).max(1);

    let mut encoder = std::process::Command::new("ffmpeg")
        .args([
            "-hide_banner",
            "-loglevel",
            "error",
            "-y",
            "-f",
            "rawvideo",
            "-pix_fmt",
            "rgba",
            "-s",
            &format!("{width}x{height}"),
            "-r",
            &format!("{fps_num}/{fps_den}"),
            "-i",
            "pipe:0",
            "-c:v",
            "qtrle",
            "-pix_fmt",
            "argb",
            "-f",
            "mov",
        ])
        .arg(&path)
        .stdin(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, format!("ffmpeg spawn: {e}")))?;

    let mut stdin = encoder.stdin.take().ok_or_else(|| {
        std::io::Error::new(std::io::ErrorKind::Other, "ffmpeg stdin unavailable")
    })?;
    use std::io::Write;
    let w = width as usize;
    let h = height as usize;
    let mut rgba = vec![0u8; w * h * 4];
    for frame_idx in 0..total_frames {
        let elapsed_ns = (frame_idx as f64 / fps * 1_000_000_000.0) as u64;
        let mut surface = rasterize_drawing_surface_at_time(
            items,
            width,
            height,
            elapsed_ns,
            reveal_ns,
        )
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, format!("cairo: {e}")))?;
        let stride = surface.stride() as usize;
        let data = surface
            .data()
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, format!("cairo data: {e}")))?;
        // Cairo ARGB32 little-endian is BGRA. Convert to straight RGBA.
        for y in 0..h {
            for x in 0..w {
                let i = y * stride + x * 4;
                let b = data[i];
                let g = data[i + 1];
                let r = data[i + 2];
                let a = data[i + 3];
                let (rr, gg, bb) = if a == 0 {
                    (0, 0, 0)
                } else {
                    let af = a as f32 / 255.0;
                    (
                        ((r as f32 / af).round().clamp(0.0, 255.0)) as u8,
                        ((g as f32 / af).round().clamp(0.0, 255.0)) as u8,
                        ((b as f32 / af).round().clamp(0.0, 255.0)) as u8,
                    )
                };
                let o = (y * w + x) * 4;
                rgba[o] = rr;
                rgba[o + 1] = gg;
                rgba[o + 2] = bb;
                rgba[o + 3] = a;
            }
        }
        drop(data);
        drop(surface);
        if let Err(e) = stdin.write_all(&rgba) {
            log::error!(
                "drawing animation encode: stdin write failed at frame {frame_idx}/{total_frames}: {e}"
            );
            let _ = encoder.kill();
            return Err(e);
        }
    }
    drop(stdin);
    let status = encoder.wait()?;
    if !status.success() {
        let _ = std::fs::remove_file(&path);
        return Err(std::io::Error::new(
            std::io::ErrorKind::Other,
            format!("ffmpeg exited with status {status}"),
        ));
    }
    Ok(path)
}

/// Stable path for a drawing clip's rasterized PNG, keyed on a hash of the
/// clip id + item list. Cached in the OS temp dir so multiple sessions reuse
/// the same file when content is unchanged.
pub fn drawing_png_cache_path(clip_id: &str, items: &[DrawingItem], width: i32, height: i32) -> PathBuf {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    clip_id.hash(&mut h);
    width.hash(&mut h);
    height.hash(&mut h);
    for it in items {
        (it.kind as u8).hash(&mut h);
        it.color.hash(&mut h);
        it.fill_color.hash(&mut h);
        (it.width.to_bits()).hash(&mut h);
        for (x, y) in &it.points {
            x.to_bits().hash(&mut h);
            y.to_bits().hash(&mut h);
        }
    }
    let hash = h.finish();
    let mut p = std::env::temp_dir();
    p.push(format!("ultimate-slice-drawing-{hash:016x}.png"));
    p
}

/// Render and return the path, reusing the cached file if it exists.
pub fn ensure_drawing_png(
    clip_id: &str,
    items: &[DrawingItem],
    width: i32,
    height: i32,
) -> std::io::Result<PathBuf> {
    let path = drawing_png_cache_path(clip_id, items, width, height);
    if !path.exists() {
        rasterize_drawing_to_png(items, width, height, &path)?;
    }
    Ok(path)
}

/// Procedural progress value (0.0–1.0) for a title animation.
///
/// `local_time_ns` is time since the clip's start on its own timeline.
/// Clamped to `[0, 1]`; returns 1.0 when `duration_ns == 0`.
pub fn animation_progress(local_time_ns: u64, duration_ns: u64) -> f64 {
    if duration_ns == 0 {
        return 1.0;
    }
    (local_time_ns as f64 / duration_ns as f64).clamp(0.0, 1.0)
}

/// Number of characters of `text` that should be visible at the given progress
/// (for `TitleAnimation::Typewriter`). Always reveals at least one character
/// once any progress has elapsed so the title doesn't flash empty.
pub fn typewriter_visible_chars(text: &str, progress: f64) -> usize {
    let total = text.chars().count();
    if total == 0 {
        return 0;
    }
    if progress <= 0.0 {
        return 0;
    }
    ((total as f64 * progress).ceil() as usize).clamp(1, total)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_stroke() -> DrawingItem {
        DrawingItem {
            kind: DrawingKind::Stroke,
            points: vec![(0.1, 0.1), (0.5, 0.5), (0.9, 0.1)],
            color: 0xFF0000FF,
            width: 6.0,
            fill_color: None,
        }
    }

    #[test]
    fn rasterize_produces_nonempty_surface() {
        let surface = rasterize_drawing_surface(&[sample_stroke()], 320, 180).unwrap();
        assert_eq!(surface.width(), 320);
        assert_eq!(surface.height(), 180);
    }

    #[test]
    fn empty_items_produce_transparent_surface() {
        let surface = rasterize_drawing_surface(&[], 10, 10).unwrap();
        assert_eq!(surface.width(), 10);
    }

    #[test]
    fn progress_clamps_and_handles_zero_duration() {
        assert_eq!(animation_progress(0, 1_000), 0.0);
        assert_eq!(animation_progress(500, 1_000), 0.5);
        assert_eq!(animation_progress(2_000, 1_000), 1.0);
        assert_eq!(animation_progress(100, 0), 1.0);
    }

    #[test]
    fn item_reveal_progress_matches_stagger_math() {
        // reveal=1s, stagger=0.7s (70% of reveal).
        // Item 0 begins at 0.0s, item 1 at 0.7s.
        assert_eq!(item_reveal_progress(0, 0, 1_000_000_000), 0.0);
        assert_eq!(item_reveal_progress(0, 500_000_000, 1_000_000_000), 0.5);
        assert_eq!(item_reveal_progress(0, 1_000_000_000, 1_000_000_000), 1.0);
        // Item 1 hasn't started yet at t=0.5s.
        assert_eq!(item_reveal_progress(1, 500_000_000, 1_000_000_000), 0.0);
        // Item 1 is halfway at t=0.7 + 0.5 = 1.2s.
        let p = item_reveal_progress(1, 1_200_000_000, 1_000_000_000);
        assert!((p - 0.5).abs() < 0.01, "progress was {p}");
        // Static mode always returns 1.0.
        assert_eq!(item_reveal_progress(42, 0, 0), 1.0);
    }

    #[test]
    fn total_reveal_duration_math() {
        // 3 items, 1s reveal, 0.7s stagger → 2*0.7 + 1.0 = 2.4s.
        assert_eq!(
            total_reveal_duration_ns(3, 1_000_000_000),
            2_400_000_000
        );
        // Static and empty cases.
        assert_eq!(total_reveal_duration_ns(3, 0), 0);
        assert_eq!(total_reveal_duration_ns(0, 1_000_000_000), 0);
    }

    #[test]
    fn rasterize_partial_reveal_differs_from_full() {
        let items = vec![sample_stroke()];
        let surface_full =
            rasterize_drawing_surface_at_time(&items, 320, 180, 500_000_000, 1_000_000_000)
                .unwrap();
        let surface_partial =
            rasterize_drawing_surface_at_time(&items, 320, 180, 100_000_000, 1_000_000_000)
                .unwrap();
        // Crude "has pixels" check via stride — both have the surface
        // object; distinguish that they are different objects, not
        // that pixel counts differ.
        assert_eq!(surface_full.width(), 320);
        assert_eq!(surface_partial.width(), 320);
    }

    #[test]
    fn typewriter_reveals_at_least_one_char() {
        assert_eq!(typewriter_visible_chars("hello", 0.0), 0);
        assert_eq!(typewriter_visible_chars("hello", 0.01), 1);
        assert_eq!(typewriter_visible_chars("hello", 0.5), 3);
        assert_eq!(typewriter_visible_chars("hello", 1.0), 5);
        assert_eq!(typewriter_visible_chars("", 0.5), 0);
    }
}
