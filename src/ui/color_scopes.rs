use gtk4::prelude::*;
/// Colour scopes panel — waveform, histogram, RGB parade, and vectorscope.
///
/// Builds a widget with a mode-tab strip on top and a Cairo `DrawingArea` below.
/// Call `update_scope_frame()` from the 33 ms poll timer in `window.rs` to push
/// a new frame; the widget queues a redraw automatically.
use gtk4::{self as gtk, DrawingArea, Orientation, ToggleButton};
use std::cell::RefCell;
use std::rc::Rc;

pub use crate::media::program_player::ScopeFrame;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum ScopeMode {
    Waveform,
    Histogram,
    RgbParade,
    Vectorscope,
}

pub struct ColorScopesState {
    pub frame: Option<std::sync::Arc<ScopeFrame>>,
    pub mode: ScopeMode,
    drawing_area: DrawingArea,
}

impl ColorScopesState {
    pub fn queue_draw(&self) {
        self.drawing_area.queue_draw();
    }
}

/// Build the colour scopes panel.
///
/// Returns `(widget, state)`. Wire `state` into the poll timer via
/// `update_scope_frame()`.
pub fn build_color_scopes() -> (gtk::Widget, Rc<RefCell<ColorScopesState>>) {
    let root = gtk::Box::new(Orientation::Vertical, 0);
    root.add_css_class("color-scopes-panel");

    // ── Tab strip ──────────────────────────────────────────────────────────
    let tab_bar = gtk::Box::new(Orientation::Horizontal, 0);
    tab_bar.add_css_class("linked");
    tab_bar.set_margin_start(4);
    tab_bar.set_margin_top(4);
    tab_bar.set_margin_bottom(4);

    let btn_wave = ToggleButton::with_label("Waveform");
    let btn_hist = ToggleButton::with_label("Histogram");
    let btn_parade = ToggleButton::with_label("RGB Parade");
    let btn_vec = ToggleButton::with_label("Vectorscope");

    btn_wave.set_active(true);
    btn_hist.set_group(Some(&btn_wave));
    btn_parade.set_group(Some(&btn_wave));
    btn_vec.set_group(Some(&btn_wave));

    tab_bar.append(&btn_wave);
    tab_bar.append(&btn_hist);
    tab_bar.append(&btn_parade);
    tab_bar.append(&btn_vec);
    root.append(&tab_bar);

    // ── Drawing area ───────────────────────────────────────────────────────
    let da = DrawingArea::new();
    da.set_hexpand(true);
    da.set_vexpand(true);
    da.set_height_request(200);
    root.append(&da);

    let state = Rc::new(RefCell::new(ColorScopesState {
        frame: None,
        mode: ScopeMode::Waveform,
        drawing_area: da.clone(),
    }));

    // Wire tab buttons → mode changes
    {
        let s = state.clone();
        let da2 = da.clone();
        btn_wave.connect_toggled(move |b| {
            if b.is_active() {
                s.borrow_mut().mode = ScopeMode::Waveform;
                da2.queue_draw();
            }
        });
    }
    {
        let s = state.clone();
        let da2 = da.clone();
        btn_hist.connect_toggled(move |b| {
            if b.is_active() {
                s.borrow_mut().mode = ScopeMode::Histogram;
                da2.queue_draw();
            }
        });
    }
    {
        let s = state.clone();
        let da2 = da.clone();
        btn_parade.connect_toggled(move |b| {
            if b.is_active() {
                s.borrow_mut().mode = ScopeMode::RgbParade;
                da2.queue_draw();
            }
        });
    }
    {
        let s = state.clone();
        let da2 = da.clone();
        btn_vec.connect_toggled(move |b| {
            if b.is_active() {
                s.borrow_mut().mode = ScopeMode::Vectorscope;
                da2.queue_draw();
            }
        });
    }

    // Wire draw function
    {
        let s = state.clone();
        da.set_draw_func(move |_da, cr, w, h| {
            let st = s.borrow();
            // Background
            cr.set_source_rgb(0.08, 0.08, 0.08);
            cr.rectangle(0.0, 0.0, w as f64, h as f64);
            let _ = cr.fill();

            if let Some(ref frame) = st.frame {
                match st.mode {
                    ScopeMode::Waveform => draw_waveform(cr, frame.as_ref(), w, h),
                    ScopeMode::Histogram => draw_histogram(cr, frame.as_ref(), w, h),
                    ScopeMode::RgbParade => draw_rgb_parade(cr, frame.as_ref(), w, h),
                    ScopeMode::Vectorscope => draw_vectorscope(cr, frame.as_ref(), w, h),
                }
            } else {
                // No frame yet — draw label
                cr.set_source_rgb(0.35, 0.35, 0.35);
                cr.move_to(w as f64 / 2.0 - 40.0, h as f64 / 2.0);
                let _ = cr.show_text("No video");
            }
        });
    }

    (root.upcast(), state)
}

/// Push a new frame into the scopes widget and trigger a redraw.
pub fn update_scope_frame(
    state: &Rc<RefCell<ColorScopesState>>,
    frame: std::sync::Arc<ScopeFrame>,
) {
    let mut st = state.borrow_mut();
    st.frame = Some(frame);
    st.queue_draw();
}

// ── Drawing helpers ────────────────────────────────────────────────────────
//
// The waveform / parade / vectorscope plots used to emit one 1×1 Cairo
// rectangle per source pixel (57,600+ path ops per frame at 30 Hz). Each
// plot now accumulates hits into a widget-resolution density grid (pure,
// unit-tested) and paints a single ARGB surface. Density maps to opacity
// exactly as the old repeated src-over fills did (1 − (1−a)ⁿ), so heavily
// traced regions read brighter — the intensity grading real scopes have.

/// Effective opacity of `hits` repeated src-over fills at `hit_alpha`.
fn density_alpha(hits: u32, hit_alpha: f64) -> f64 {
    if hits == 0 {
        return 0.0;
    }
    // 0.5^64 is already sub-precision; avoid pointless large exponents.
    1.0 - (1.0 - hit_alpha).powi(hits.min(64) as i32)
}

/// Rasterize a density grid into a premultiplied-ARGB surface tinted
/// `(r, g, b)`. Empty cells stay transparent.
fn density_surface(
    density: &[u32],
    w: usize,
    h: usize,
    r: f64,
    g: f64,
    b: f64,
    hit_alpha: f64,
) -> Option<gtk::cairo::ImageSurface> {
    let mut surface =
        gtk::cairo::ImageSurface::create(gtk::cairo::Format::ARgb32, w as i32, h as i32).ok()?;
    let stride = surface.stride() as usize;
    {
        // ImageSurfaceData marks the surface dirty when dropped.
        let mut dest = surface.data().ok()?;
        for y in 0..h {
            for x in 0..w {
                let hits = density[y * w + x];
                if hits == 0 {
                    continue;
                }
                let a = density_alpha(hits, hit_alpha);
                let px = (((a * 255.0) as u32) << 24)
                    | (((r * a * 255.0) as u32) << 16)
                    | (((g * a * 255.0) as u32) << 8)
                    | ((b * a * 255.0) as u32);
                let o = y * stride + x * 4;
                dest[o..o + 4].copy_from_slice(&px.to_ne_bytes());
            }
        }
    }
    Some(surface)
}

/// Luma-waveform hit density over a `w`×`h` grid: every source pixel adds a
/// hit at (source x → grid x, brightness → grid y, bright at top).
fn waveform_density(frame: &ScopeFrame, w: usize, h: usize) -> Vec<u32> {
    let mut density = vec![0u32; w * h];
    let fw = frame.width as f64;
    let (wf, hf) = (w as f64, h as f64);
    for fy in 0..frame.height {
        for fx in 0..frame.width {
            let base = (fy * frame.width + fx) * 4;
            if base + 2 >= frame.data.len() {
                continue;
            }
            let r = frame.data[base] as f64;
            let g = frame.data[base + 1] as f64;
            let b = frame.data[base + 2] as f64;
            let luma = 0.299 * r + 0.587 * g + 0.114 * b;
            let sx = ((fx as f64 / fw * wf).clamp(0.0, wf - 1.0)) as usize;
            let sy = (((255.0 - luma) / 255.0 * hf).clamp(0.0, hf - 1.0)) as usize;
            density[sy.min(h - 1) * w + sx.min(w - 1)] += 1;
        }
    }
    density
}

/// Per-channel parade hit density over a `col_w`×`h` column grid.
fn parade_density(frame: &ScopeFrame, ch: usize, col_w: usize, h: usize) -> Vec<u32> {
    let mut density = vec![0u32; col_w * h];
    let fw = frame.width as f64;
    let (wf, hf) = (col_w as f64, h as f64);
    for fy in 0..frame.height {
        for fx in 0..frame.width {
            let base = (fy * frame.width + fx) * 4;
            if base + ch >= frame.data.len() {
                continue;
            }
            let val = frame.data[base + ch] as f64;
            let sx = ((fx as f64 / fw * wf).clamp(0.0, wf - 1.0)) as usize;
            let sy = (((255.0 - val) / 255.0 * hf).clamp(0.0, hf - 1.0)) as usize;
            density[sy.min(h - 1) * col_w + sx.min(col_w - 1)] += 1;
        }
    }
    density
}

/// Vectorscope (Cb vs Cr) hit density over a `w`×`h` grid centred on the
/// widget. Hits landing outside the grid are dropped, matching the old
/// behaviour of plotting off-canvas rectangles that Cairo clipped away.
fn vectorscope_density(frame: &ScopeFrame, w: usize, h: usize) -> Vec<u32> {
    let mut density = vec![0u32; w * h];
    let cx = w as f64 / 2.0;
    let cy = h as f64 / 2.0;
    let radius = cx.min(cy) * 0.9;
    let len = frame.data.len();
    let mut i = 0;
    while i + 2 < len {
        let r = frame.data[i] as f64 / 255.0;
        let g = frame.data[i + 1] as f64 / 255.0;
        let b = frame.data[i + 2] as f64 / 255.0;
        // BT.601 YCbCr
        let cb = -0.168736 * r - 0.331264 * g + 0.5 * b; // -0.5..0.5
        let cr_val = 0.5 * r - 0.418688 * g - 0.081312 * b; // -0.5..0.5
        let sx = cx + cb * radius * 2.0;
        let sy = cy - cr_val * radius * 2.0;
        if sx >= 0.0 && sy >= 0.0 && (sx as usize) < w && (sy as usize) < h {
            density[(sy as usize) * w + sx as usize] += 1;
        }
        i += 4;
    }
    density
}

/// Luma waveform: for every source pixel, plot its brightness (Y) at the
/// corresponding horizontal position.  Bright pixels → top; dark → bottom.
fn draw_waveform(cr: &gtk::cairo::Context, frame: &ScopeFrame, w: i32, h: i32) {
    let (wu, hu) = (w.max(0) as usize, h.max(0) as usize);
    if wu == 0 || hu == 0 {
        return;
    }
    let density = waveform_density(frame, wu, hu);
    if let Some(surface) = density_surface(&density, wu, hu, 0.2, 0.9, 0.2, 0.5) {
        let _ = cr.set_source_surface(&surface, 0.0, 0.0);
        let _ = cr.paint();
    }

    let wf = w as f64;
    let hf = h as f64;
    // IRE graticule lines at 0%, 50%, 100%
    cr.set_source_rgba(0.5, 0.5, 0.5, 0.4);
    cr.set_line_width(0.5);
    for pct in [0.0_f64, 0.25, 0.5, 0.75, 1.0] {
        let y = pct * hf;
        cr.move_to(0.0, y);
        cr.line_to(wf, y);
        let _ = cr.stroke();
    }
}

/// Histogram: distribution of luma values across 0–255.
fn draw_histogram(cr: &gtk::cairo::Context, frame: &ScopeFrame, w: i32, h: i32) {
    let mut counts = [0u32; 256];
    let len = frame.data.len();
    let mut i = 0;
    while i + 3 < len {
        let r = frame.data[i] as f64;
        let g = frame.data[i + 1] as f64;
        let b = frame.data[i + 2] as f64;
        let luma = (0.299 * r + 0.587 * g + 0.114 * b).clamp(0.0, 255.0) as usize;
        counts[luma] = counts[luma].saturating_add(1);
        i += 4;
    }
    let max_count = *counts.iter().max().unwrap_or(&1).max(&1);
    let wf = w as f64;
    let hf = h as f64;
    let bar_w = wf / 256.0;

    cr.set_source_rgba(0.85, 0.85, 0.85, 0.8);
    for (idx, &count) in counts.iter().enumerate() {
        let bar_h = count as f64 / max_count as f64 * hf;
        let x = idx as f64 * bar_w;
        cr.rectangle(x, hf - bar_h, bar_w.max(1.0), bar_h);
    }
    let _ = cr.fill();
}

/// RGB parade: three side-by-side waveform monitors (R, G, B channels).
fn draw_rgb_parade(cr: &gtk::cairo::Context, frame: &ScopeFrame, w: i32, h: i32) {
    let wf = w as f64;
    let hf = h as f64;
    let col_w = wf / 3.0;

    let channels: &[(usize, f64, f64, f64)] = &[
        (0, 0.9, 0.2, 0.2), // R
        (1, 0.2, 0.9, 0.2), // G
        (2, 0.2, 0.4, 0.9), // B
    ];

    let col_w_px = ((wf / 3.0) as usize).max(1);
    let hu = h.max(0) as usize;
    if hu == 0 {
        return;
    }
    for &(ch, r, g, b) in channels {
        let x_offset = ch as f64 * col_w;
        let density = parade_density(frame, ch, col_w_px, hu);
        if let Some(surface) = density_surface(&density, col_w_px, hu, r, g, b, 0.5) {
            let _ = cr.set_source_surface(&surface, x_offset, 0.0);
            let _ = cr.paint();
        }

        // Divider
        cr.set_source_rgba(0.3, 0.3, 0.3, 1.0);
        cr.set_line_width(1.0);
        if ch < 2 {
            let dx = (ch + 1) as f64 * col_w;
            cr.move_to(dx, 0.0);
            cr.line_to(dx, hf);
            let _ = cr.stroke();
        }
    }
}

/// Vectorscope: plots Cb (U) vs Cr (V) for each pixel in a circular diagram.
/// Saturation maps to distance from centre; hue maps to angle.
fn draw_vectorscope(cr: &gtk::cairo::Context, frame: &ScopeFrame, w: i32, h: i32) {
    let cx = w as f64 / 2.0;
    let cy = h as f64 / 2.0;
    let radius = cx.min(cy) * 0.9;

    // Graticule circle
    cr.set_source_rgba(0.3, 0.3, 0.3, 0.6);
    cr.set_line_width(0.5);
    cr.arc(cx, cy, radius, 0.0, std::f64::consts::TAU);
    let _ = cr.stroke();
    // Centre cross
    cr.move_to(cx - 5.0, cy);
    cr.line_to(cx + 5.0, cy);
    cr.move_to(cx, cy - 5.0);
    cr.line_to(cx, cy + 5.0);
    let _ = cr.stroke();

    // Plot pixels
    let (wu, hu) = (w.max(0) as usize, h.max(0) as usize);
    if wu == 0 || hu == 0 {
        return;
    }
    let density = vectorscope_density(frame, wu, hu);
    if let Some(surface) = density_surface(&density, wu, hu, 0.2, 0.9, 0.4, 0.4) {
        let _ = cr.set_source_surface(&surface, 0.0, 0.0);
        let _ = cr.paint();
    }
}

#[cfg(test)]
mod tests {
    use super::{density_alpha, parade_density, vectorscope_density, waveform_density, ScopeFrame};

    /// 2×1 frame: one white pixel, one black pixel (RGBA).
    fn white_black_frame() -> ScopeFrame {
        ScopeFrame {
            data: vec![255, 255, 255, 255, 0, 0, 0, 255],
            width: 2,
            height: 1,
        }
    }

    #[test]
    fn density_alpha_matches_repeated_src_over() {
        assert_eq!(density_alpha(0, 0.5), 0.0);
        assert!((density_alpha(1, 0.5) - 0.5).abs() < 1e-12);
        assert!((density_alpha(2, 0.5) - 0.75).abs() < 1e-12);
        // Saturates instead of overflowing for huge hit counts.
        assert!(density_alpha(1_000_000, 0.5) <= 1.0);
    }

    #[test]
    fn waveform_density_places_bright_top_dark_bottom() {
        let d = waveform_density(&white_black_frame(), 4, 4);
        // White pixel (source x=0) → left half, top row.
        assert_eq!(d[0], 1, "white pixel lands top-left: {d:?}");
        // Black pixel (source x=1) → right half, bottom row.
        assert_eq!(d[3 * 4 + 2], 1, "black pixel lands bottom-right: {d:?}");
        assert_eq!(d.iter().sum::<u32>(), 2);
    }

    #[test]
    fn parade_density_reads_selected_channel() {
        // One pixel: R=255, G=0, B=127.
        let frame = ScopeFrame {
            data: vec![255, 0, 127, 255],
            width: 1,
            height: 1,
        };
        let red = parade_density(&frame, 0, 2, 4);
        assert_eq!(red[0], 1, "full red → top row: {red:?}");
        let green = parade_density(&frame, 1, 2, 4);
        assert_eq!(green[3 * 2], 1, "zero green → bottom row: {green:?}");
        let blue = parade_density(&frame, 2, 2, 4);
        assert_eq!(blue[2 * 2], 1, "mid blue → lower-middle row: {blue:?}");
    }

    #[test]
    fn vectorscope_density_puts_neutral_gray_at_center() {
        let frame = ScopeFrame {
            data: vec![128, 128, 128, 255],
            width: 1,
            height: 1,
        };
        let d = vectorscope_density(&frame, 9, 9);
        assert_eq!(d[4 * 9 + 4], 1, "neutral gray → centre cell: {d:?}");
        assert_eq!(d.iter().sum::<u32>(), 1);
    }
}
