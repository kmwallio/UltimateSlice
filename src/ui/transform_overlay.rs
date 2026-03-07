use gtk4::glib;
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
/// Inspector crop slider max (pixels).
const CROP_MAX: i32 = 500;

#[derive(Clone, Copy, PartialEq)]
enum Handle {
    None,
    Rotate,
    TopLeft,
    TopRight,
    BottomLeft,
    BottomRight,
    CropLeft,
    CropRight,
    CropTop,
    CropBottom,
    /// Drag inside the bounding box → pan.
    Pan,
}

struct DragState {
    handle: Handle,
    start_wx: f64,
    start_wy: f64,
    start_scale: f64,
    start_px: f64,
    start_py: f64,
    start_crop_left: i32,
    start_crop_right: i32,
    start_crop_top: i32,
    start_crop_bottom: i32,
    proj_w: u32,
    proj_h: u32,
    /// Video rect cached at drag start.
    vx: f64,
    vy: f64,
    vw: f64,
    vh: f64,
}

pub struct TransformOverlay {
    pub drawing_area: DrawingArea,
    scale: Rc<Cell<f64>>,
    position_x: Rc<Cell<f64>>,
    position_y: Rc<Cell<f64>>,
    rotation: Rc<Cell<f64>>,
    crop_left: Rc<Cell<i32>>,
    crop_right: Rc<Cell<i32>>,
    crop_top: Rc<Cell<i32>>,
    crop_bottom: Rc<Cell<i32>>,
    selected: Rc<Cell<bool>>,
    proj_w: Rc<Cell<u32>>,
    proj_h: Rc<Cell<u32>>,
    picture: Rc<RefCell<Option<gtk4::Picture>>>,
    /// The AspectFrame widget that constrains the canvas area.
    /// When set, the draw/drag functions query its bounds in the DA's coordinate
    /// space via `Widget::compute_bounds()` instead of calling `video_rect()` on
    /// the full DA size.  This gives correct results when the DA is larger than the
    /// canvas (e.g. when the outer overlay covers the full scroll viewport and the
    /// canvas_frame is smaller due to zoom < 1.0).
    canvas_widget: Rc<RefCell<Option<gtk4::Widget>>>,
}

impl TransformOverlay {
    /// Create a new overlay.  `on_change(scale, position_x, position_y)` is
    /// called whenever the user adjusts scale or position via drag.
    pub fn new(
        on_change: impl Fn(f64, f64, f64) + 'static,
        on_rotate_change: impl Fn(i32) + 'static,
        on_crop_change: impl Fn(i32, i32, i32, i32) + 'static,
        on_drag_begin: impl Fn() + 'static,
        on_drag_end: impl Fn() + 'static,
    ) -> Self {
        let scale = Rc::new(Cell::new(1.0_f64));
        let position_x = Rc::new(Cell::new(0.0_f64));
        let position_y = Rc::new(Cell::new(0.0_f64));
        let rotation = Rc::new(Cell::new(0.0_f64));
        let crop_left = Rc::new(Cell::new(0_i32));
        let crop_right = Rc::new(Cell::new(0_i32));
        let crop_top = Rc::new(Cell::new(0_i32));
        let crop_bottom = Rc::new(Cell::new(0_i32));
        let selected = Rc::new(Cell::new(false));
        let proj_w = Rc::new(Cell::new(1920_u32));
        let proj_h = Rc::new(Cell::new(1080_u32));
        let picture: Rc<RefCell<Option<gtk4::Picture>>> = Rc::new(RefCell::new(None));
        let canvas_widget: Rc<RefCell<Option<gtk4::Widget>>> = Rc::new(RefCell::new(None));

        let da = DrawingArea::new();
        da.set_hexpand(true);
        da.set_vexpand(true);
        da.set_can_target(true);
        da.set_focusable(true);

        // Draw function ----------------------------------------------------
        {
            let scale = scale.clone();
            let position_x = position_x.clone();
            let position_y = position_y.clone();
            let rotation = rotation.clone();
            let crop_left = crop_left.clone();
            let crop_right = crop_right.clone();
            let crop_top = crop_top.clone();
            let crop_bottom = crop_bottom.clone();
            let selected = selected.clone();
            let proj_w = proj_w.clone();
            let proj_h = proj_h.clone();
            let picture = picture.clone();
            let canvas_widget = canvas_widget.clone();

            da.set_draw_func(move |da, cr, ww, wh| {
                if !selected.get() {
                    return;
                }
                // Always use project dimensions for the canvas boundary.
                // The canvas border represents what will be exported, not the clip's native size.
                let _ = &picture; // kept for potential future use
                let (vx, vy, vw, vh) =
                    canvas_video_rect(da, &canvas_widget, ww, wh, proj_w.get(), proj_h.get());
                // Always draw: dark vignette + canvas border
                draw_outside_vignette(cr, ww as f64, wh as f64, vx, vy, vw, vh);
                draw_frame_border(cr, vx, vy, vw, vh);
                // Only draw clip handles when clip doesn't fill the canvas exactly
                let s = scale.get();
                let px = position_x.get();
                let py = position_y.get();
                draw_overlay(
                    cr,
                    vx,
                    vy,
                    vw,
                    vh,
                    s,
                    px,
                    py,
                    rotation.get(),
                    crop_left.get(),
                    crop_right.get(),
                    crop_top.get(),
                    crop_bottom.get(),
                    proj_w.get(),
                    proj_h.get(),
                );
            });
        }

        // Drag gesture -----------------------------------------------------
        let drag_state: Rc<RefCell<Option<DragState>>> = Rc::new(RefCell::new(None));
        let on_change = Rc::new(on_change);
        let on_rotate_change = Rc::new(on_rotate_change);
        let on_crop_change = Rc::new(on_crop_change);
        let on_drag_begin = Rc::new(on_drag_begin);

        let gesture = gtk::GestureDrag::new();
        gesture.set_button(1); // left button only

        // drag_begin: hit-test → choose handle
        {
            let scale = scale.clone();
            let position_x = position_x.clone();
            let position_y = position_y.clone();
            let rotation = rotation.clone();
            let crop_left = crop_left.clone();
            let crop_right = crop_right.clone();
            let crop_top = crop_top.clone();
            let crop_bottom = crop_bottom.clone();
            let selected = selected.clone();
            let proj_w = proj_w.clone();
            let proj_h = proj_h.clone();
            let picture = picture.clone();
            let drag_state = drag_state.clone();
            let on_drag_begin = on_drag_begin.clone();
            let da_ref = da.clone();
            let canvas_widget = canvas_widget.clone();

            gesture.connect_drag_begin(move |_g, sx, sy| {
                if !selected.get() {
                    return;
                }
                da_ref.grab_focus();
                let ww = da_ref.width();
                let wh = da_ref.height();
                let _ = &picture; // kept for potential future use
                let (vx, vy, vw, vh) =
                    canvas_video_rect(&da_ref, &canvas_widget, ww, wh, proj_w.get(), proj_h.get());
                let s = scale.get();
                let px = position_x.get();
                let py = position_y.get();
                let rot_rad = rotation.get().to_radians();

                // Clip bounding box in widget space (same formula as draw_overlay)
                let cx = vx + vw / 2.0 + px * vw * (1.0 - s) / 2.0;
                let cy = vy + vh / 2.0 + py * vh * (1.0 - s) / 2.0;
                let hw = vw * s / 2.0;
                let hh = vh * s / 2.0;
                let left = cx - hw;
                let right = cx + hw;
                let top = cy - hh;
                let bottom = cy + hh;
                let (crop_l_px, crop_r_px, crop_t_px, crop_b_px) = crop_insets_to_overlay_px(
                    crop_left.get(),
                    crop_right.get(),
                    crop_top.get(),
                    crop_bottom.get(),
                    proj_w.get(),
                    proj_h.get(),
                    right - left,
                    bottom - top,
                );
                let crop_left_x = left + crop_l_px;
                let crop_right_x = right - crop_r_px;
                let crop_top_y = top + crop_t_px;
                let crop_bottom_y = bottom - crop_b_px;

                let to_world = |lx: f64, ly: f64| -> (f64, f64) {
                    rotate_point_about(cx + lx, cy + ly, cx, cy, rot_rad)
                };
                let corners = [
                    {
                        let (x, y) = to_world(-hw, -hh);
                        (x, y, Handle::TopLeft)
                    },
                    {
                        let (x, y) = to_world(hw, -hh);
                        (x, y, Handle::TopRight)
                    },
                    {
                        let (x, y) = to_world(-hw, hh);
                        (x, y, Handle::BottomLeft)
                    },
                    {
                        let (x, y) = to_world(hw, hh);
                        (x, y, Handle::BottomRight)
                    },
                ];
                let rotate_handle = {
                    let (x, y) = to_world(0.0, -hh - 24.0);
                    (x, y, Handle::Rotate)
                };
                let crop_edges = [
                    (
                        {
                            let (x, _) = to_world(
                                ((crop_left_x + crop_right_x) / 2.0) - cx,
                                crop_top_y - cy,
                            );
                            x
                        },
                        {
                            let (_, y) = to_world(
                                ((crop_left_x + crop_right_x) / 2.0) - cx,
                                crop_top_y - cy,
                            );
                            y
                        },
                        Handle::CropTop,
                    ),
                    (
                        {
                            let (x, _) = to_world(
                                ((crop_left_x + crop_right_x) / 2.0) - cx,
                                crop_bottom_y - cy,
                            );
                            x
                        },
                        {
                            let (_, y) = to_world(
                                ((crop_left_x + crop_right_x) / 2.0) - cx,
                                crop_bottom_y - cy,
                            );
                            y
                        },
                        Handle::CropBottom,
                    ),
                    (
                        {
                            let (x, _) = to_world(
                                crop_left_x - cx,
                                ((crop_top_y + crop_bottom_y) / 2.0) - cy,
                            );
                            x
                        },
                        {
                            let (_, y) = to_world(
                                crop_left_x - cx,
                                ((crop_top_y + crop_bottom_y) / 2.0) - cy,
                            );
                            y
                        },
                        Handle::CropLeft,
                    ),
                    (
                        {
                            let (x, _) = to_world(
                                crop_right_x - cx,
                                ((crop_top_y + crop_bottom_y) / 2.0) - cy,
                            );
                            x
                        },
                        {
                            let (_, y) = to_world(
                                crop_right_x - cx,
                                ((crop_top_y + crop_bottom_y) / 2.0) - cy,
                            );
                            y
                        },
                        Handle::CropRight,
                    ),
                ];

                let mut handle = Handle::None;
                {
                    let d =
                        ((sx - rotate_handle.0).powi(2) + (sy - rotate_handle.1).powi(2)).sqrt();
                    if d <= HANDLE_HIT {
                        handle = rotate_handle.2;
                    }
                }
                if handle == Handle::None {
                    for (hx, hy, h) in &corners {
                        let d = ((sx - hx).powi(2) + (sy - hy).powi(2)).sqrt();
                        if d <= HANDLE_HIT {
                            handle = *h;
                            break;
                        }
                    }
                }
                if handle == Handle::None {
                    for (hx, hy, h) in &crop_edges {
                        let d = ((sx - hx).powi(2) + (sy - hy).powi(2)).sqrt();
                        if d <= HANDLE_HIT {
                            handle = *h;
                            break;
                        }
                    }
                }

                if handle == Handle::None {
                    // Inside the clip bounds → pan
                    let (lx, ly) = unrotate_point_about(sx, sy, cx, cy, rot_rad);
                    if lx >= left && lx <= right && ly >= top && ly <= bottom {
                        handle = Handle::Pan;
                    }
                }

                if handle != Handle::None {
                    on_drag_begin();
                    *drag_state.borrow_mut() = Some(DragState {
                        handle,
                        start_wx: sx,
                        start_wy: sy,
                        start_scale: s,
                        start_px: px,
                        start_py: py,
                        start_crop_left: crop_left.get(),
                        start_crop_right: crop_right.get(),
                        start_crop_top: crop_top.get(),
                        start_crop_bottom: crop_bottom.get(),
                        proj_w: proj_w.get(),
                        proj_h: proj_h.get(),
                        vx,
                        vy,
                        vw,
                        vh,
                    });
                }
            });
        }

        // drag_update: apply delta
        {
            let scale = scale.clone();
            let position_x = position_x.clone();
            let position_y = position_y.clone();
            let rotation_for_drag = rotation.clone();
            let crop_left = crop_left.clone();
            let crop_right = crop_right.clone();
            let crop_top = crop_top.clone();
            let crop_bottom = crop_bottom.clone();
            let drag_state = drag_state.clone();
            let on_change = on_change.clone();
            let on_rotate_change = on_rotate_change.clone();
            let on_crop_change = on_crop_change.clone();
            let da_ref = da.clone();

            gesture.connect_drag_update(move |g, off_x, off_y| {
                let mut ds_borrow = drag_state.borrow_mut();
                let Some(ref ds) = *ds_borrow else {
                    return;
                };
                let rot_rad = rotation_for_drag.get().to_radians();
                let local_dx = off_x * rot_rad.cos() + off_y * rot_rad.sin();
                let local_dy = -off_x * rot_rad.sin() + off_y * rot_rad.cos();

                match ds.handle {
                    Handle::Rotate => {
                        let clip_cx = ds.vx
                            + ds.vw / 2.0
                            + ds.start_px * ds.vw * (1.0 - ds.start_scale) / 2.0;
                        let clip_cy = ds.vy
                            + ds.vh / 2.0
                            + ds.start_py * ds.vh * (1.0 - ds.start_scale) / 2.0;
                        let cur_x = ds.start_wx + off_x;
                        let cur_y = ds.start_wy + off_y;
                        let mut deg = ((cur_y - clip_cy).atan2(cur_x - clip_cx).to_degrees()
                            + 90.0)
                            .rem_euclid(360.0);
                        if deg > 180.0 {
                            deg -= 360.0;
                        }
                        let deg = deg.round().clamp(-180.0, 180.0);
                        rotation_for_drag.set(deg);
                        on_rotate_change(deg as i32);
                    }
                    Handle::Pan => {
                        // Position sensitivity: d(pos_x) = off_x / (vw*(1-scale)/2)
                        // This gives 1:1 pixel movement of the clip centre in canvas space.
                        // For scale>1 the denominator is negative, so dragging right decreases
                        // pos_x — which is correct because at scale>1, pos_x controls which
                        // part of the clip is visible (higher pos_x = viewport panned right
                        // = clip appears shifted left).
                        let h_range = ds.vw * (1.0 - ds.start_scale) / 2.0;
                        let v_range = ds.vh * (1.0 - ds.start_scale) / 2.0;
                        let new_px = if h_range.abs() > 0.5 {
                            (ds.start_px + off_x / h_range).clamp(-1.0, 1.0)
                        } else {
                            ds.start_px // scale≈1.0: position has no effect
                        };
                        let new_py = if v_range.abs() > 0.5 {
                            (ds.start_py + off_y / v_range).clamp(-1.0, 1.0)
                        } else {
                            ds.start_py
                        };
                        position_x.set(new_px);
                        position_y.set(new_py);
                        on_change(scale.get(), new_px, new_py);
                    }
                    Handle::TopLeft
                    | Handle::TopRight
                    | Handle::BottomLeft
                    | Handle::BottomRight => {
                        // Scale: ratio of distance from clip centre to current vs. start.
                        // Holding Shift uses constrained scaling (same X/Y scale factor).
                        let clip_cx = ds.vx
                            + ds.vw / 2.0
                            + ds.start_px * ds.vw * (1.0 - ds.start_scale) / 2.0;
                        let clip_cy = ds.vy
                            + ds.vh / 2.0
                            + ds.start_py * ds.vh * (1.0 - ds.start_scale) / 2.0;
                        let cur_x = ds.start_wx + off_x;
                        let cur_y = ds.start_wy + off_y;
                        let orig_dx = (ds.start_wx - clip_cx).abs();
                        let orig_dy = (ds.start_wy - clip_cy).abs();
                        let now_dx = (cur_x - clip_cx).abs();
                        let now_dy = (cur_y - clip_cy).abs();
                        if orig_dx > 1.0 || orig_dy > 1.0 {
                            let sx = if orig_dx > 1.0 { now_dx / orig_dx } else { 1.0 };
                            let sy = if orig_dy > 1.0 { now_dy / orig_dy } else { 1.0 };
                            let shift = g
                                .current_event_state()
                                .contains(gtk::gdk::ModifierType::SHIFT_MASK);
                            let factor = if shift { sx.max(sy) } else { (sx + sy) * 0.5 };
                            let new_s = (ds.start_scale * factor).clamp(0.1, 4.0);
                            scale.set(new_s);
                            on_change(new_s, position_x.get(), position_y.get());
                        }
                    }
                    Handle::CropLeft => {
                        let clip_w = (ds.vw * ds.start_scale).max(1.0);
                        let delta = (local_dx * ds.proj_w as f64 / clip_w).round() as i32;
                        let mut new_left = ds.start_crop_left + delta;
                        let max_left = (ds.proj_w as i32 - 2 - crop_right.get()).clamp(0, CROP_MAX);
                        new_left = new_left.clamp(0, max_left);
                        crop_left.set(new_left);
                        on_crop_change(
                            new_left,
                            crop_right.get(),
                            crop_top.get(),
                            crop_bottom.get(),
                        );
                    }
                    Handle::CropRight => {
                        let clip_w = (ds.vw * ds.start_scale).max(1.0);
                        let delta = (-local_dx * ds.proj_w as f64 / clip_w).round() as i32;
                        let mut new_right = ds.start_crop_right + delta;
                        let max_right = (ds.proj_w as i32 - 2 - crop_left.get()).clamp(0, CROP_MAX);
                        new_right = new_right.clamp(0, max_right);
                        crop_right.set(new_right);
                        on_crop_change(
                            crop_left.get(),
                            new_right,
                            crop_top.get(),
                            crop_bottom.get(),
                        );
                    }
                    Handle::CropTop => {
                        let clip_h = (ds.vh * ds.start_scale).max(1.0);
                        let delta = (local_dy * ds.proj_h as f64 / clip_h).round() as i32;
                        let mut new_top = ds.start_crop_top + delta;
                        let max_top = (ds.proj_h as i32 - 2 - crop_bottom.get()).clamp(0, CROP_MAX);
                        new_top = new_top.clamp(0, max_top);
                        crop_top.set(new_top);
                        on_crop_change(
                            crop_left.get(),
                            crop_right.get(),
                            new_top,
                            crop_bottom.get(),
                        );
                    }
                    Handle::CropBottom => {
                        let clip_h = (ds.vh * ds.start_scale).max(1.0);
                        let delta = (-local_dy * ds.proj_h as f64 / clip_h).round() as i32;
                        let mut new_bottom = ds.start_crop_bottom + delta;
                        let max_bottom = (ds.proj_h as i32 - 2 - crop_top.get()).clamp(0, CROP_MAX);
                        new_bottom = new_bottom.clamp(0, max_bottom);
                        crop_bottom.set(new_bottom);
                        on_crop_change(
                            crop_left.get(),
                            crop_right.get(),
                            crop_top.get(),
                            new_bottom,
                        );
                    }
                    Handle::None => {}
                }
                drop(ds_borrow);
                da_ref.queue_draw();
            });
        }

        // drag_end: clear state and notify for a final preview refresh.
        {
            let drag_state = drag_state.clone();
            let on_drag_end = Rc::new(on_drag_end);
            gesture.connect_drag_end(move |_g, _ox, _oy| {
                *drag_state.borrow_mut() = None;
                on_drag_end();
            });
        }

        da.add_controller(gesture);
        {
            let scale = scale.clone();
            let position_x = position_x.clone();
            let position_y = position_y.clone();
            let selected = selected.clone();
            let on_change = on_change.clone();
            let da_ref = da.clone();
            let key_ctrl = gtk::EventControllerKey::new();
            key_ctrl.connect_key_pressed(move |_, key, _, mods| {
                use gtk::gdk::{Key, ModifierType};
                if !selected.get() {
                    return glib::Propagation::Proceed;
                }
                let shift = mods.contains(ModifierType::SHIFT_MASK);
                let mut handled = false;
                match key {
                    Key::Left => {
                        position_x.set(
                            (position_x.get() - if shift { 0.1 } else { 0.01 }).clamp(-1.0, 1.0),
                        );
                        handled = true;
                    }
                    Key::Right => {
                        position_x.set(
                            (position_x.get() + if shift { 0.1 } else { 0.01 }).clamp(-1.0, 1.0),
                        );
                        handled = true;
                    }
                    Key::Up => {
                        position_y.set(
                            (position_y.get() - if shift { 0.1 } else { 0.01 }).clamp(-1.0, 1.0),
                        );
                        handled = true;
                    }
                    Key::Down => {
                        position_y.set(
                            (position_y.get() + if shift { 0.1 } else { 0.01 }).clamp(-1.0, 1.0),
                        );
                        handled = true;
                    }
                    Key::plus | Key::equal | Key::KP_Add => {
                        scale.set((scale.get() + if shift { 0.10 } else { 0.05 }).clamp(0.1, 4.0));
                        handled = true;
                    }
                    Key::minus | Key::underscore | Key::KP_Subtract => {
                        scale.set((scale.get() - if shift { 0.10 } else { 0.05 }).clamp(0.1, 4.0));
                        handled = true;
                    }
                    _ => {}
                }
                if handled {
                    on_change(scale.get(), position_x.get(), position_y.get());
                    da_ref.queue_draw();
                    glib::Propagation::Stop
                } else {
                    glib::Propagation::Proceed
                }
            });
            da.add_controller(key_ctrl);
        }

        TransformOverlay {
            drawing_area: da,
            scale,
            position_x,
            position_y,
            rotation,
            crop_left,
            crop_right,
            crop_top,
            crop_bottom,
            selected,
            proj_w,
            proj_h,
            picture,
            canvas_widget,
        }
    }

    /// Give the overlay access to the AspectFrame that constrains the canvas area.
    /// When set, canvas rect computation uses `Widget::compute_bounds()` for pixel-
    /// perfect alignment at all program monitor zoom levels.
    pub fn set_canvas_widget(&self, w: gtk4::Widget) {
        *self.canvas_widget.borrow_mut() = Some(w);
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

    /// Update displayed rotation value in degrees.
    pub fn set_rotation(&self, rot: i32) {
        self.rotation.set(rot as f64);
        self.drawing_area.queue_draw();
    }

    /// Update overlay crop values (in source pixels).
    pub fn set_crop(&self, cl: i32, cr: i32, ct: i32, cb: i32) {
        self.crop_left.set(cl.clamp(0, CROP_MAX));
        self.crop_right.set(cr.clamp(0, CROP_MAX));
        self.crop_top.set(ct.clamp(0, CROP_MAX));
        self.crop_bottom.set(cb.clamp(0, CROP_MAX));
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
/// Kept for future use if re-alignment with the paintable is needed.
#[allow(dead_code)]
fn paintable_dims(
    picture: &Rc<RefCell<Option<gtk4::Picture>>>,
    proj_w: u32,
    proj_h: u32,
) -> (u32, u32) {
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

/// Compute the canvas video rect in the DrawingArea's coordinate space.
///
/// If a `canvas_widget` is set (the AspectFrame that constrains the canvas),
/// queries its actual bounds in the DA's coordinate space via `compute_bounds()`
/// and then letterboxes the canvas ratio within those bounds.  This gives correct
/// results when the DA is larger than the canvas_widget (e.g. when the outer overlay
/// fills the full scroll viewport and the canvas_frame is smaller due to zoom < 1.0).
///
/// Falls back to `video_rect()` on the full DA if compute_bounds() fails.
fn canvas_video_rect(
    da: &DrawingArea,
    canvas_widget: &Rc<RefCell<Option<gtk4::Widget>>>,
    ww: i32,
    wh: i32,
    proj_w: u32,
    proj_h: u32,
) -> (f64, f64, f64, f64) {
    if let Some(ref cw) = *canvas_widget.borrow() {
        if let Some(bounds) = cw.compute_bounds(da) {
            let cfx = bounds.x() as f64;
            let cfy = bounds.y() as f64;
            let cfw = bounds.width() as f64;
            let cfh = bounds.height() as f64;
            if cfw > 0.0 && cfh > 0.0 {
                return video_rect_within(cfx, cfy, cfw, cfh, proj_w, proj_h);
            }
        }
    }
    video_rect(ww, wh, proj_w, proj_h)
}

/// Letterbox `proj_w × proj_h` within the box `(bx, by, bw, bh)`.
/// Returns `(vx, vy, vw, vh)` — the canvas rect in the outer DA coordinate space.
fn video_rect_within(bx: f64, by: f64, bw: f64, bh: f64, pw: u32, ph: u32) -> (f64, f64, f64, f64) {
    if bw <= 0.0 || bh <= 0.0 || pw == 0 || ph == 0 {
        return (bx, by, bw.max(1.0), bh.max(1.0));
    }
    let vid_asp = pw as f64 / ph as f64;
    let box_asp = bw / bh;
    let (vw, vh) = if vid_asp > box_asp {
        (bw, bw / vid_asp)
    } else {
        (bh * vid_asp, bh)
    };
    let vx = bx + (bw - vw) / 2.0;
    let vy = by + (bh - vh) / 2.0;
    (vx, vy, vw, vh)
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
fn draw_outside_vignette(
    cr: &gtk4::cairo::Context,
    ww: f64,
    wh: f64,
    vx: f64,
    vy: f64,
    vw: f64,
    vh: f64,
) {
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
        (vx, vy, tick, 0.0, 0.0, tick),             // top-left
        (vx + vw, vy, -tick, 0.0, 0.0, tick),       // top-right
        (vx, vy + vh, tick, 0.0, 0.0, -tick),       // bottom-left
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
fn crop_insets_to_overlay_px(
    crop_left: i32,
    crop_right: i32,
    crop_top: i32,
    crop_bottom: i32,
    proj_w: u32,
    proj_h: u32,
    clip_w: f64,
    clip_h: f64,
) -> (f64, f64, f64, f64) {
    let pw = proj_w.max(1) as f64;
    let ph = proj_h.max(1) as f64;
    let left = (crop_left.max(0) as f64 / pw) * clip_w;
    let right = (crop_right.max(0) as f64 / pw) * clip_w;
    let top = (crop_top.max(0) as f64 / ph) * clip_h;
    let bottom = (crop_bottom.max(0) as f64 / ph) * clip_h;
    (left, right, top, bottom)
}

fn rotate_point_about(x: f64, y: f64, cx: f64, cy: f64, rad: f64) -> (f64, f64) {
    let dx = x - cx;
    let dy = y - cy;
    let xr = dx * rad.cos() - dy * rad.sin();
    let yr = dx * rad.sin() + dy * rad.cos();
    (cx + xr, cy + yr)
}

fn unrotate_point_about(x: f64, y: f64, cx: f64, cy: f64, rad: f64) -> (f64, f64) {
    rotate_point_about(x, y, cx, cy, -rad)
}

fn draw_overlay(
    cr: &gtk4::cairo::Context,
    vx: f64,
    vy: f64,
    vw: f64,
    vh: f64,
    scale: f64,
    pos_x: f64,
    pos_y: f64,
    rotation_deg: f64,
    crop_left: i32,
    crop_right: i32,
    crop_top: i32,
    crop_bottom: i32,
    proj_w: u32,
    proj_h: u32,
) {
    // Clip centre and half-extents in widget coords.
    // GStreamer's videobox pads/crops (1-scale)*pw*(1+pos_x)/2 on the left, so the
    // clip centre = canvas_centre + pos_x * canvas_half * (1-scale).
    // This formula is valid for both zoom-in (scale>1) and zoom-out (scale<1).
    let cx = vx + vw / 2.0 + pos_x * vw * (1.0 - scale) / 2.0;
    let cy = vy + vh / 2.0 + pos_y * vh * (1.0 - scale) / 2.0;
    let hw = vw * scale / 2.0;
    let hh = vh * scale / 2.0;

    let left = cx - hw;
    let right = cx + hw;
    let top = cy - hh;
    let bottom = cy + hh;
    let rot_rad = rotation_deg.to_radians();
    let (crop_l_px, crop_r_px, crop_t_px, crop_b_px) = crop_insets_to_overlay_px(
        crop_left,
        crop_right,
        crop_top,
        crop_bottom,
        proj_w,
        proj_h,
        right - left,
        bottom - top,
    );
    let crop_left_x = left + crop_l_px;
    let crop_right_x = right - crop_r_px;
    let crop_top_y = top + crop_t_px;
    let crop_bottom_y = bottom - crop_b_px;
    let to_world = |x: f64, y: f64| -> (f64, f64) { rotate_point_about(x, y, cx, cy, rot_rad) };

    // Clip bounding box (white dashed)
    cr.save().ok();
    cr.set_source_rgba(1.0, 1.0, 1.0, 0.80);
    cr.set_line_width(1.5);
    cr.set_dash(&[6.0, 4.0], 0.0);
    let (tlx, tly) = to_world(left, top);
    let (trx, try_) = to_world(right, top);
    let (brx, bry) = to_world(right, bottom);
    let (blx, bly) = to_world(left, bottom);
    cr.move_to(tlx, tly);
    cr.line_to(trx, try_);
    cr.line_to(brx, bry);
    cr.line_to(blx, bly);
    cr.close_path();
    cr.stroke().ok();
    cr.restore().ok();

    // Corner scale handles
    for (hx, hy) in &[(tlx, tly), (trx, try_), (blx, bly), (brx, bry)] {
        cr.save().ok();
        cr.arc(*hx, *hy, HANDLE_R, 0.0, std::f64::consts::TAU);
        cr.set_source_rgba(1.0, 1.0, 1.0, 0.95);
        cr.fill_preserve().ok();
        cr.set_source_rgba(0.25, 0.55, 1.0, 1.0);
        cr.set_line_width(1.5);
        cr.stroke().ok();
        cr.restore().ok();
    }

    // Rotation handle (top-center) connected to clip box
    let (rot_hx, rot_hy) = to_world(cx, top - 24.0);
    let (rot_ax, rot_ay) = to_world(cx, top);
    cr.save().ok();
    cr.move_to(rot_ax, rot_ay);
    cr.line_to(rot_hx, rot_hy);
    cr.set_source_rgba(1.0, 0.65, 0.2, 0.95);
    cr.set_line_width(1.5);
    cr.stroke().ok();
    cr.arc(rot_hx, rot_hy, HANDLE_R, 0.0, std::f64::consts::TAU);
    cr.set_source_rgba(1.0, 0.75, 0.2, 0.98);
    cr.fill_preserve().ok();
    cr.set_source_rgba(0.55, 0.25, 0.0, 1.0);
    cr.set_line_width(1.2);
    cr.stroke().ok();
    cr.restore().ok();

    // Crop rectangle and edge midpoint handles
    cr.save().ok();
    cr.set_source_rgba(0.3, 0.95, 0.45, 0.95);
    cr.set_line_width(1.5);
    cr.set_dash(&[5.0, 3.0], 0.0);
    let (ctlx, ctly) = to_world(crop_left_x, crop_top_y);
    let (ctrx, ctry) = to_world(crop_right_x, crop_top_y);
    let (cbrx, cbry) = to_world(crop_right_x, crop_bottom_y);
    let (cblx, cbly) = to_world(crop_left_x, crop_bottom_y);
    cr.move_to(ctlx, ctly);
    cr.line_to(ctrx, ctry);
    cr.line_to(cbrx, cbry);
    cr.line_to(cblx, cbly);
    cr.close_path();
    cr.stroke().ok();
    cr.restore().ok();
    for (hx, hy) in &[
        to_world((crop_left_x + crop_right_x) / 2.0, crop_top_y),
        to_world((crop_left_x + crop_right_x) / 2.0, crop_bottom_y),
        to_world(crop_left_x, (crop_top_y + crop_bottom_y) / 2.0),
        to_world(crop_right_x, (crop_top_y + crop_bottom_y) / 2.0),
    ] {
        cr.save().ok();
        cr.rectangle(*hx - 6.0, *hy - 6.0, 12.0, 12.0);
        cr.set_source_rgba(0.3, 0.95, 0.45, 0.95);
        cr.fill_preserve().ok();
        cr.set_source_rgba(0.0, 0.35, 0.1, 1.0);
        cr.set_line_width(1.2);
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
    cr.select_font_face(
        "Sans",
        gtk4::cairo::FontSlant::Normal,
        gtk4::cairo::FontWeight::Bold,
    );
    cr.set_font_size(11.0);
    let label = format!("{scale:.2}×  {rotation_deg:.0}°");
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
