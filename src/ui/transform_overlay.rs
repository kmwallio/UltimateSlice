/// Interactive transform overlay drawn over the program monitor video.
///
/// Shows a bounding box + 4 corner handles representing the clip's scale and position.
/// - Drag a **corner handle** to change the clip's zoom scale.
/// - Drag **inside** the bounding box to pan (position X/Y).
///
/// The overlay is a transparent `DrawingArea` layered on top of the GtkPicture
/// via GtkOverlay.  It calls `on_change(scale, position_x, position_y)` each
/// time the user moves the mouse.
use gtk4::prelude::*;
use gtk4::{self as gtk, DrawingArea};
use std::cell::{Cell, RefCell};
use std::rc::Rc;

/// Radius (px) of the drawn corner handle circles.
const HANDLE_R: f64 = 7.0;
/// Hit-test radius for corner handles (a bit larger than drawn for ease of use).
const HANDLE_HIT: f64 = 16.0;

#[derive(Clone, Copy, PartialEq)]
enum Handle {
    None,
    TopLeft,
    TopRight,
    BottomLeft,
    BottomRight,
    /// Drag inside the bounding box → pan.
    Pan,
}

struct DragState {
    handle:      Handle,
    start_wx:    f64,
    start_wy:    f64,
    start_scale: f64,
    start_px:    f64,
    start_py:    f64,
    /// Video rect cached at drag start.
    vx: f64, vy: f64, vw: f64, vh: f64,
}

pub struct TransformOverlay {
    pub drawing_area: DrawingArea,
    scale:      Rc<Cell<f64>>,
    position_x: Rc<Cell<f64>>,
    position_y: Rc<Cell<f64>>,
    selected:   Rc<Cell<bool>>,
    proj_w:     Rc<Cell<u32>>,
    proj_h:     Rc<Cell<u32>>,
    picture:    Rc<RefCell<Option<gtk4::Picture>>>,
}

impl TransformOverlay {
    /// Create a new overlay.  `on_change(scale, position_x, position_y)` is
    /// called whenever the user adjusts scale or position via drag.
    pub fn new(on_change: impl Fn(f64, f64, f64) + 'static) -> Self {
        let scale      = Rc::new(Cell::new(1.0_f64));
        let position_x = Rc::new(Cell::new(0.0_f64));
        let position_y = Rc::new(Cell::new(0.0_f64));
        let selected   = Rc::new(Cell::new(false));
        let proj_w     = Rc::new(Cell::new(1920_u32));
        let proj_h     = Rc::new(Cell::new(1080_u32));
        let picture: Rc<RefCell<Option<gtk4::Picture>>> = Rc::new(RefCell::new(None));

        let da = DrawingArea::new();
        da.set_hexpand(true);
        da.set_vexpand(true);
        da.set_can_target(true);
        da.set_focusable(false);

        // Draw function ----------------------------------------------------
        {
            let scale      = scale.clone();
            let position_x = position_x.clone();
            let position_y = position_y.clone();
            let selected   = selected.clone();
            let proj_w     = proj_w.clone();
            let proj_h     = proj_h.clone();
            let picture    = picture.clone();

            da.set_draw_func(move |_da, cr, ww, wh| {
                if !selected.get() { return; }
                let (pw, ph) = paintable_dims(&picture, proj_w.get(), proj_h.get());
                let (vx, vy, vw, vh) = video_rect(ww, wh, pw, ph);
                // Always draw: dark vignette + canvas border
                draw_outside_vignette(cr, ww as f64, wh as f64, vx, vy, vw, vh);
                draw_frame_border(cr, vx, vy, vw, vh);
                // Only draw clip handles when clip doesn't fill the canvas exactly
                let s  = scale.get();
                let px = position_x.get();
                let py = position_y.get();
                if (s - 1.0).abs() > 0.02 || px.abs() > 0.02 || py.abs() > 0.02 {
                    draw_overlay(cr, vx, vy, vw, vh, s, px, py);
                }
            });
        }

        // Drag gesture -----------------------------------------------------
        let drag_state: Rc<RefCell<Option<DragState>>> = Rc::new(RefCell::new(None));
        let on_change = Rc::new(on_change);

        let gesture = gtk::GestureDrag::new();
        gesture.set_button(1); // left button only

        // drag_begin: hit-test → choose handle
        {
            let scale      = scale.clone();
            let position_x = position_x.clone();
            let position_y = position_y.clone();
            let selected   = selected.clone();
            let proj_w     = proj_w.clone();
            let proj_h     = proj_h.clone();
            let picture    = picture.clone();
            let drag_state = drag_state.clone();
            let da_ref     = da.clone();

            gesture.connect_drag_begin(move |_g, sx, sy| {
                if !selected.get() { return; }
                let ww = da_ref.width();
                let wh = da_ref.height();
                let (pw, ph) = paintable_dims(&picture, proj_w.get(), proj_h.get());
                let (vx, vy, vw, vh) = video_rect(ww, wh, pw, ph);
                let s  = scale.get();
                let px = position_x.get();
                let py = position_y.get();

                // Clip bounding box in widget space
                let cx = vx + vw / 2.0 + px * vw / 2.0;
                let cy = vy + vh / 2.0 + py * vh / 2.0;
                let hw = vw * s / 2.0;
                let hh = vh * s / 2.0;

                let corners = [
                    (cx - hw, cy - hh, Handle::TopLeft),
                    (cx + hw, cy - hh, Handle::TopRight),
                    (cx - hw, cy + hh, Handle::BottomLeft),
                    (cx + hw, cy + hh, Handle::BottomRight),
                ];

                let mut handle = Handle::None;
                for (hx, hy, h) in &corners {
                    let d = ((sx - hx).powi(2) + (sy - hy).powi(2)).sqrt();
                    if d <= HANDLE_HIT {
                        handle = *h;
                        break;
                    }
                }

                if handle == Handle::None {
                    // Inside the video rect → pan
                    if sx >= vx && sx <= vx + vw && sy >= vy && sy <= vy + vh {
                        handle = Handle::Pan;
                    }
                }

                if handle != Handle::None {
                    *drag_state.borrow_mut() = Some(DragState {
                        handle,
                        start_wx: sx, start_wy: sy,
                        start_scale: s, start_px: px, start_py: py,
                        vx, vy, vw, vh,
                    });
                }
            });
        }

        // drag_update: apply delta
        {
            let scale      = scale.clone();
            let position_x = position_x.clone();
            let position_y = position_y.clone();
            let drag_state = drag_state.clone();
            let on_change  = on_change.clone();
            let da_ref     = da.clone();

            gesture.connect_drag_update(move |_g, off_x, off_y| {
                let mut ds_borrow = drag_state.borrow_mut();
                let Some(ref ds) = *ds_borrow else { return; };

                match ds.handle {
                    Handle::Pan => {
                        // One full video-rect width/height = ±1.0 in position space
                        let new_px = (ds.start_px + off_x / (ds.vw / 2.0)).clamp(-1.0, 1.0);
                        let new_py = (ds.start_py + off_y / (ds.vh / 2.0)).clamp(-1.0, 1.0);
                        position_x.set(new_px);
                        position_y.set(new_py);
                        on_change(scale.get(), new_px, new_py);
                    }
                    _ => {
                        // Scale: ratio of distance from clip centre to current vs. start
                        let clip_cx = ds.vx + ds.vw / 2.0 + ds.start_px * ds.vw / 2.0;
                        let clip_cy = ds.vy + ds.vh / 2.0 + ds.start_py * ds.vh / 2.0;
                        let orig = ((ds.start_wx - clip_cx).powi(2)
                                  + (ds.start_wy - clip_cy).powi(2)).sqrt();
                        let cur_x = ds.start_wx + off_x;
                        let cur_y = ds.start_wy + off_y;
                        let now  = ((cur_x - clip_cx).powi(2)
                                  + (cur_y - clip_cy).powi(2)).sqrt();
                        if orig > 1.0 {
                            let new_s = (ds.start_scale * now / orig).clamp(0.1, 4.0);
                            scale.set(new_s);
                            on_change(new_s, position_x.get(), position_y.get());
                        }
                    }
                    Handle::None => {}
                }
                drop(ds_borrow);
                da_ref.queue_draw();
            });
        }

        // drag_end: clear state
        {
            let drag_state = drag_state.clone();
            gesture.connect_drag_end(move |_g, _ox, _oy| {
                *drag_state.borrow_mut() = None;
            });
        }

        da.add_controller(gesture);

        TransformOverlay { drawing_area: da, scale, position_x, position_y, selected, proj_w, proj_h, picture }
    }

    /// Give the overlay access to the GtkPicture so it can query the actual
    /// paintable dimensions (used by ContentFit::Contain) for pixel-perfect alignment.
    pub fn set_picture(&self, p: gtk4::Picture) {
        *self.picture.borrow_mut() = Some(p);
    }

    /// Update the displayed transform values (e.g. when inspector sliders change).
    pub fn set_transform(&self, s: f64, px: f64, py: f64) {
        self.scale.set(s);
        self.position_x.set(px);
        self.position_y.set(py);
        self.drawing_area.queue_draw();
    }

    /// Show or hide handles (true when a clip is selected).
    pub fn set_clip_selected(&self, selected: bool) {
        self.selected.set(selected);
        self.drawing_area.queue_draw();
    }

    /// Set project resolution so the video-rect aspect ratio is correct.
    pub fn set_project_dimensions(&self, w: u32, h: u32) {
        self.proj_w.set(w);
        self.proj_h.set(h);
    }
}

// ── Helper functions ──────────────────────────────────────────────────────────

/// Query the actual paintable intrinsic dimensions from the GtkPicture.
/// Falls back to project dims if no paintable is available yet (before first frame).
/// This ensures `video_rect()` uses the same aspect ratio that GtkPicture
/// (ContentFit::Contain) uses, keeping handles pixel-perfectly aligned.
fn paintable_dims(picture: &Rc<RefCell<Option<gtk4::Picture>>>, proj_w: u32, proj_h: u32) -> (u32, u32) {
    if let Some(ref p) = *picture.borrow() {
        if let Some(paintable) = p.paintable() {
            let iw = paintable.intrinsic_width();
            let ih = paintable.intrinsic_height();
            if iw > 0 && ih > 0 {
                return (iw as u32, ih as u32);
            }
        }
    }
    (proj_w, proj_h)
}

/// Compute the video letterbox rect `(x, y, w, h)` inside a widget of size
/// `(ww × wh)` for a project of resolution `pw × ph` (ContentFit::Contain).
fn video_rect(ww: i32, wh: i32, pw: u32, ph: u32) -> (f64, f64, f64, f64) {
    let ww = ww as f64;
    let wh = wh as f64;
    if ww <= 0.0 || wh <= 0.0 || pw == 0 || ph == 0 {
        return (0.0, 0.0, ww.max(1.0), wh.max(1.0));
    }
    let vid_asp = pw as f64 / ph as f64;
    let wid_asp = ww / wh;
    let (vw, vh) = if vid_asp > wid_asp {
        (ww, ww / vid_asp)
    } else {
        (wh * vid_asp, wh)
    };
    let vx = (ww - vw) / 2.0;
    let vy = (wh - vh) / 2.0;
    (vx, vy, vw, vh)
}

/// Darken the areas outside the canvas rect so it's immediately obvious what
/// is in-frame (will be exported) vs. out-of-frame.
fn draw_outside_vignette(cr: &gtk4::cairo::Context, ww: f64, wh: f64, vx: f64, vy: f64, vw: f64, vh: f64) {
    cr.save().ok();
    cr.set_source_rgba(0.0, 0.0, 0.0, 0.35);
    // Fill the four rects surrounding the canvas rect
    // Top strip
    if vy > 0.0 {
        cr.rectangle(0.0, 0.0, ww, vy);
        cr.fill().ok();
    }
    // Bottom strip
    if vy + vh < wh {
        cr.rectangle(0.0, vy + vh, ww, wh - (vy + vh));
        cr.fill().ok();
    }
    // Left strip (between top/bottom strips)
    if vx > 0.0 {
        cr.rectangle(0.0, vy, vx, vh);
        cr.fill().ok();
    }
    // Right strip
    if vx + vw < ww {
        cr.rectangle(vx + vw, vy, ww - (vx + vw), vh);
        cr.fill().ok();
    }
    cr.restore().ok();
}

/// Draw the export frame border — a prominent solid rectangle showing exactly
/// what will be included in the exported video.  Drawn with a dark shadow line
/// and a bright accent line so it reads on both light and dark backgrounds.
fn draw_frame_border(cr: &gtk4::cairo::Context, vx: f64, vy: f64, vw: f64, vh: f64) {
    // Shadow (1 px outset, semi-transparent black)
    cr.save().ok();
    cr.set_source_rgba(0.0, 0.0, 0.0, 0.7);
    cr.set_line_width(3.0);
    cr.rectangle(vx - 0.5, vy - 0.5, vw + 1.0, vh + 1.0);
    cr.stroke().ok();
    cr.restore().ok();

    // Bright accent border
    cr.save().ok();
    cr.set_source_rgba(1.0, 0.95, 0.3, 0.95);
    cr.set_line_width(1.5);
    cr.rectangle(vx, vy, vw, vh);
    cr.stroke().ok();
    cr.restore().ok();

    // Corner tick marks (small L-shapes at each corner, 10 px long)
    let tick = 10.0_f64;
    let corners = [
        (vx, vy, tick, 0.0, 0.0, tick),          // top-left
        (vx + vw, vy, -tick, 0.0, 0.0, tick),    // top-right
        (vx, vy + vh, tick, 0.0, 0.0, -tick),    // bottom-left
        (vx + vw, vy + vh, -tick, 0.0, 0.0, -tick), // bottom-right
    ];
    cr.save().ok();
    cr.set_source_rgba(1.0, 1.0, 1.0, 1.0);
    cr.set_line_width(2.5);
    for (cx, cy, hx, _, _, vy_off) in &corners {
        // Horizontal arm
        cr.move_to(*cx, *cy);
        cr.line_to(cx + hx, *cy);
        cr.stroke().ok();
        // Vertical arm
        cr.move_to(*cx, *cy);
        cr.line_to(*cx, cy + vy_off);
        cr.stroke().ok();
    }
    cr.restore().ok();
}

/// Draw the clip bounding box, corner scale handles, center pan dot, and scale label.
fn draw_overlay(
    cr: &gtk4::cairo::Context,
    vx: f64, vy: f64, vw: f64, vh: f64,
    scale: f64, pos_x: f64, pos_y: f64,
) {
    // Clip centre and half-extents in widget coords
    let cx = vx + vw / 2.0 + pos_x * vw / 2.0;
    let cy = vy + vh / 2.0 + pos_y * vh / 2.0;
    let hw = vw * scale / 2.0;
    let hh = vh * scale / 2.0;

    let left   = cx - hw;
    let right  = cx + hw;
    let top    = cy - hh;
    let bottom = cy + hh;

    // Clip bounding box (white dashed)
    cr.save().ok();
    cr.set_source_rgba(1.0, 1.0, 1.0, 0.80);
    cr.set_line_width(1.5);
    cr.set_dash(&[6.0, 4.0], 0.0);
    cr.rectangle(left, top, right - left, bottom - top);
    cr.stroke().ok();
    cr.restore().ok();

    // Corner scale handles
    for (hx, hy) in &[(left, top), (right, top), (left, bottom), (right, bottom)] {
        cr.save().ok();
        cr.arc(*hx, *hy, HANDLE_R, 0.0, std::f64::consts::TAU);
        cr.set_source_rgba(1.0, 1.0, 1.0, 0.95);
        cr.fill_preserve().ok();
        cr.set_source_rgba(0.25, 0.55, 1.0, 1.0);
        cr.set_line_width(1.5);
        cr.stroke().ok();
        cr.restore().ok();
    }

    // Centre pan dot
    cr.save().ok();
    cr.arc(cx, cy, 4.5, 0.0, std::f64::consts::TAU);
    cr.set_source_rgba(1.0, 1.0, 1.0, 0.90);
    cr.fill_preserve().ok();
    cr.set_source_rgba(0.25, 0.55, 1.0, 1.0);
    cr.set_line_width(1.0);
    cr.stroke().ok();
    cr.restore().ok();

    // Scale label near top-right of the video frame
    cr.save().ok();
    cr.select_font_face("Sans", gtk4::cairo::FontSlant::Normal, gtk4::cairo::FontWeight::Bold);
    cr.set_font_size(11.0);
    let label = format!("{scale:.2}×");
    let te = match cr.text_extents(&label) {
        Ok(te) => te,
        Err(_) => return,
    };
    let tx = vx + vw - te.width() - 12.0;
    let ty = vy + 8.0;
    // Dark pill background
    cr.set_source_rgba(0.0, 0.0, 0.0, 0.60);
    cr.rectangle(tx - 4.0, ty - 1.0, te.width() + 8.0, te.height() + 4.0);
    cr.fill().ok();
    cr.set_source_rgba(1.0, 0.95, 0.3, 1.0);
    cr.move_to(tx, ty + te.height() + 1.0);
    cr.show_text(&label).ok();
    cr.restore().ok();
}
