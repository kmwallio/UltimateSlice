/// Colour scopes panel — waveform, histogram, RGB parade, and vectorscope.
///
/// Builds a widget with a mode-tab strip on top and a Cairo `DrawingArea` below.
/// Call `update_scope_frame()` from the 33 ms poll timer in `window.rs` to push
/// a new frame; the widget queues a redraw automatically.
use gtk4::{self as gtk, DrawingArea, Orientation, ToggleButton};
use gtk4::prelude::*;
use std::rc::Rc;
use std::cell::RefCell;

pub use crate::media::program_player::ScopeFrame;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum ScopeMode {
    Waveform,
    Histogram,
    RgbParade,
    Vectorscope,
}

pub struct ColorScopesState {
    pub frame: Option<ScopeFrame>,
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

    let btn_wave  = ToggleButton::with_label("Waveform");
    let btn_hist  = ToggleButton::with_label("Histogram");
    let btn_parade= ToggleButton::with_label("RGB Parade");
    let btn_vec   = ToggleButton::with_label("Vectorscope");

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
    da.set_height_request(200);
    root.append(&da);

    let state = Rc::new(RefCell::new(ColorScopesState {
        frame: None,
        mode: ScopeMode::Waveform,
        drawing_area: da.clone(),
    }));

    // Wire tab buttons → mode changes
    {
        let s = state.clone(); let da2 = da.clone();
        btn_wave.connect_toggled(move |b| {
            if b.is_active() { s.borrow_mut().mode = ScopeMode::Waveform; da2.queue_draw(); }
        });
    }
    {
        let s = state.clone(); let da2 = da.clone();
        btn_hist.connect_toggled(move |b| {
            if b.is_active() { s.borrow_mut().mode = ScopeMode::Histogram; da2.queue_draw(); }
        });
    }
    {
        let s = state.clone(); let da2 = da.clone();
        btn_parade.connect_toggled(move |b| {
            if b.is_active() { s.borrow_mut().mode = ScopeMode::RgbParade; da2.queue_draw(); }
        });
    }
    {
        let s = state.clone(); let da2 = da.clone();
        btn_vec.connect_toggled(move |b| {
            if b.is_active() { s.borrow_mut().mode = ScopeMode::Vectorscope; da2.queue_draw(); }
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
                    ScopeMode::Waveform   => draw_waveform(cr, frame, w, h),
                    ScopeMode::Histogram  => draw_histogram(cr, frame, w, h),
                    ScopeMode::RgbParade  => draw_rgb_parade(cr, frame, w, h),
                    ScopeMode::Vectorscope=> draw_vectorscope(cr, frame, w, h),
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
pub fn update_scope_frame(state: &Rc<RefCell<ColorScopesState>>, frame: ScopeFrame) {
    let mut st = state.borrow_mut();
    st.frame = Some(frame);
    st.queue_draw();
}

// ── Drawing helpers ────────────────────────────────────────────────────────

/// Luma waveform: for every source pixel, plot its brightness (Y) at the
/// corresponding horizontal position.  Bright pixels → top; dark → bottom.
fn draw_waveform(cr: &gtk::cairo::Context, frame: &ScopeFrame, w: i32, h: i32) {
    let fw = frame.width as f64;
    let fh = frame.height;
    let wf = w as f64;
    let hf = h as f64;

    cr.set_source_rgba(0.2, 0.9, 0.2, 0.5);
    for fy in 0..fh {
        for fx in 0..frame.width {
            let base = (fy * frame.width + fx) * 4;
            if base + 2 >= frame.data.len() { continue; }
            let r = frame.data[base]     as f64;
            let g = frame.data[base + 1] as f64;
            let b = frame.data[base + 2] as f64;
            let luma = 0.299 * r + 0.587 * g + 0.114 * b;
            let sx = (fx as f64 / fw * wf).clamp(0.0, wf - 1.0);
            let sy = ((255.0 - luma) / 255.0 * hf).clamp(0.0, hf - 1.0);
            cr.rectangle(sx, sy, 1.0, 1.0);
        }
    }
    let _ = cr.fill();

    // IRE graticule lines at 0%, 50%, 100%
    cr.set_source_rgba(0.5, 0.5, 0.5, 0.4);
    cr.set_line_width(0.5);
    for pct in [0.0_f64, 0.25, 0.5, 0.75, 1.0] {
        let y = pct * hf;
        cr.move_to(0.0, y); cr.line_to(wf, y);
        let _ = cr.stroke();
    }
}

/// Histogram: distribution of luma values across 0–255.
fn draw_histogram(cr: &gtk::cairo::Context, frame: &ScopeFrame, w: i32, h: i32) {
    let mut counts = [0u32; 256];
    let len = frame.data.len();
    let mut i = 0;
    while i + 3 < len {
        let r = frame.data[i]     as f64;
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
    let fw = frame.width as f64;
    let fh = frame.height;
    let col_w = wf / 3.0;

    let channels: &[(usize, f64, f64, f64)] = &[
        (0, 0.9, 0.2, 0.2), // R
        (1, 0.2, 0.9, 0.2), // G
        (2, 0.2, 0.4, 0.9), // B
    ];

    for &(ch, r, g, b) in channels {
        let x_offset = ch as f64 * col_w;
        cr.set_source_rgba(r, g, b, 0.5);
        for fy in 0..fh {
            for fx in 0..frame.width {
                let base = (fy * frame.width + fx) * 4;
                if base + ch >= frame.data.len() { continue; }
                let val = frame.data[base + ch] as f64;
                let sx = x_offset + fx as f64 / fw * col_w;
                let sy = ((255.0 - val) / 255.0 * hf).clamp(0.0, hf - 1.0);
                cr.rectangle(sx, sy, 1.0, 1.0);
            }
        }
        let _ = cr.fill();

        // Divider
        cr.set_source_rgba(0.3, 0.3, 0.3, 1.0);
        cr.set_line_width(1.0);
        if ch < 2 {
            let dx = (ch + 1) as f64 * col_w;
            cr.move_to(dx, 0.0); cr.line_to(dx, hf);
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
    cr.move_to(cx - 5.0, cy); cr.line_to(cx + 5.0, cy);
    cr.move_to(cx, cy - 5.0); cr.line_to(cx, cy + 5.0);
    let _ = cr.stroke();

    // Plot pixels
    cr.set_source_rgba(0.2, 0.9, 0.4, 0.4);
    let len = frame.data.len();
    let mut i = 0;
    while i + 2 < len {
        let r = frame.data[i]     as f64 / 255.0;
        let g = frame.data[i + 1] as f64 / 255.0;
        let b = frame.data[i + 2] as f64 / 255.0;
        // BT.601 YCbCr
        let cb = -0.168736 * r - 0.331264 * g + 0.5 * b; // -0.5..0.5
        let cr_val = 0.5 * r - 0.418688 * g - 0.081312 * b; // -0.5..0.5
        let sx = cx + cb * radius * 2.0;
        let sy = cy - cr_val * radius * 2.0;
        cr.rectangle(sx, sy, 1.0, 1.0);
        i += 4;
    }
    let _ = cr.fill();
}
