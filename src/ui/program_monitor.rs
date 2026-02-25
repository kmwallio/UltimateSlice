use gtk4::prelude::*;
use gtk4::{self as gtk, Box as GBox, Button, DrawingArea, GestureClick, GestureDrag, Label, Orientation, Picture};
use glib;
use std::cell::RefCell;
use std::rc::Rc;
use crate::media::program_player::ProgramPlayer;

/// Transform parameters for a clip (crop, rotation, flip).
#[derive(Clone, Copy, Default)]
pub struct ClipTransform {
    pub crop_left: i32,
    pub crop_right: i32,
    pub crop_top: i32,
    pub crop_bottom: i32,
    pub rotate: i32,   // 0, 90, 180, 270
    pub flip_h: bool,
    pub flip_v: bool,
}

struct OverlayState {
    transform: Option<ClipTransform>,
    drag_handle: Option<HandleKind>,
    drag_start_pos: (f64, f64),
    drag_start_transform: ClipTransform,
    on_changed: Option<Rc<dyn Fn(i32, i32, i32, i32, i32, bool, bool)>>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum HandleKind {
    Left, Right, Top, Bottom,
    TopLeft, TopRight, BottomLeft, BottomRight,
    Rotate,
}

/// Interactive transform overlay displayed over the program monitor.
pub struct TransformOverlay {
    state: Rc<RefCell<OverlayState>>,
    area: DrawingArea,
}

impl Clone for TransformOverlay {
    fn clone(&self) -> Self {
        TransformOverlay {
            state: self.state.clone(),
            area: self.area.clone(),
        }
    }
}

impl TransformOverlay {
    /// Call when the selected clip changes. Pass `None` to hide the overlay.
    pub fn set_clip(&self, transform: Option<ClipTransform>) {
        self.state.borrow_mut().transform = transform;
        self.area.queue_draw();
    }

    /// Wire up the callback called when the user drags a handle.
    pub fn set_on_changed(&self, cb: impl Fn(i32, i32, i32, i32, i32, bool, bool) + 'static) {
        self.state.borrow_mut().on_changed = Some(Rc::new(cb));
    }
}

/// Build the program monitor widget.
///
/// Returns `(widget, transform_overlay)`. The caller should call
/// `transform_overlay.set_clip(...)` whenever the selected clip changes and
/// `transform_overlay.set_on_changed(...)` to react to drag edits.
pub fn build_program_monitor(
    program_player: Rc<RefCell<ProgramPlayer>>,
    paintable: gdk4::Paintable,
) -> (GBox, TransformOverlay) {
    let root = GBox::new(Orientation::Vertical, 0);
    root.set_hexpand(true);
    root.set_vexpand(true);
    root.add_css_class("preview-panel");

    // Title bar
    let title_bar = GBox::new(Orientation::Horizontal, 8);
    title_bar.add_css_class("preview-header");
    title_bar.set_margin_start(8);
    title_bar.set_margin_end(8);
    title_bar.set_margin_top(4);
    title_bar.set_margin_bottom(4);

    let label = Label::new(Some("Program Monitor"));
    label.add_css_class("dim-label");
    title_bar.append(&label);

    let spacer = gtk::Separator::new(Orientation::Horizontal);
    spacer.set_hexpand(true);
    title_bar.append(&spacer);

    let pos_label = Label::new(Some("00:00:00;00"));
    pos_label.add_css_class("timecode");
    title_bar.append(&pos_label);

    root.append(&title_bar);

    // Video display inside a gtk4::Overlay so we can layer handles on top
    let picture = Picture::new();
    picture.set_paintable(Some(&paintable));
    picture.set_hexpand(true);
    picture.set_vexpand(true);
    picture.set_content_fit(gtk::ContentFit::Contain);
    picture.add_css_class("preview-video");

    let video_overlay = gtk4::Overlay::new();
    video_overlay.set_child(Some(&picture));
    video_overlay.set_hexpand(true);
    video_overlay.set_vexpand(true);

    let transform_area = DrawingArea::new();
    transform_area.set_hexpand(true);
    transform_area.set_vexpand(true);
    transform_area.set_can_target(true);
    video_overlay.add_overlay(&transform_area);

    root.append(&video_overlay);

    // Transport controls
    let controls = GBox::new(Orientation::Horizontal, 8);
    controls.add_css_class("transport-bar");
    controls.set_halign(gtk::Align::Center);
    controls.set_margin_top(6);
    controls.set_margin_bottom(6);

    let btn_play = Button::with_label("▶ Play");
    {
        let pp = program_player.clone();
        btn_play.connect_clicked(move |_| {
            pp.borrow_mut().toggle_play_pause();
        });
    }
    controls.append(&btn_play);

    let btn_stop = Button::with_label("■ Stop");
    {
        let pp = program_player.clone();
        btn_stop.connect_clicked(move |_| {
            pp.borrow_mut().seek(0);
        });
    }
    controls.append(&btn_stop);

    root.append(&controls);

    // 100 ms timer: poll position + update timecode label
    {
        let pp = program_player.clone();
        let pos_label = pos_label.clone();
        glib::timeout_add_local(std::time::Duration::from_millis(100), move || {
            let mut player = pp.borrow_mut();
            player.poll();
            let pos_ns = player.timeline_pos_ns;
            drop(player);
            pos_label.set_text(&format_timecode(pos_ns));
            glib::ControlFlow::Continue
        });
    }

    // ── Overlay state ──────────────────────────────────────────────────────
    let state = Rc::new(RefCell::new(OverlayState {
        transform: None,
        drag_handle: None,
        drag_start_pos: (0.0, 0.0),
        drag_start_transform: ClipTransform::default(),
        on_changed: None,
    }));

    // ── Draw function ──────────────────────────────────────────────────────
    {
        let state = state.clone();
        transform_area.set_draw_func(move |_area, cr, w, h| {
            let transform = {
                let st = state.borrow();
                match st.transform {
                    Some(t) => t,
                    None => return,
                }
            };

            let dw = w as f64;
            let dh = h as f64;
            let (bl, br, bt, bb) = crop_box(dw, dh, &transform);

            // Dashed bounding box
            cr.set_source_rgba(1.0, 1.0, 1.0, 0.85);
            cr.set_line_width(1.5);
            cr.set_dash(&[6.0, 4.0], 0.0);
            cr.rectangle(bl, bt, br - bl, bb - bt);
            cr.stroke().ok();
            cr.set_dash(&[], 0.0);

            // 8 crop handles (corners + mid-edges)
            let mid_x = (bl + br) / 2.0;
            let mid_y = (bt + bb) / 2.0;
            draw_handle(cr, bl,    mid_y); // Left
            draw_handle(cr, br,    mid_y); // Right
            draw_handle(cr, mid_x, bt);    // Top
            draw_handle(cr, mid_x, bb);    // Bottom
            draw_handle(cr, bl,    bt);    // TopLeft
            draw_handle(cr, br,    bt);    // TopRight
            draw_handle(cr, bl,    bb);    // BottomLeft
            draw_handle(cr, br,    bb);    // BottomRight

            // Rotation handle above top-center
            let rot_x = mid_x;
            let rot_y = (bt - 24.0).max(8.0);

            cr.set_source_rgba(1.0, 1.0, 1.0, 0.8);
            cr.set_line_width(1.0);
            cr.move_to(rot_x, bt);
            cr.line_to(rot_x, rot_y + 8.0);
            cr.stroke().ok();

            cr.arc(rot_x, rot_y, 8.0, 0.0, std::f64::consts::TAU);
            cr.set_source_rgba(0.2, 0.5, 1.0, 0.9);
            cr.fill().ok();
            cr.arc(rot_x, rot_y, 8.0, 0.0, std::f64::consts::TAU);
            cr.set_source_rgb(1.0, 1.0, 1.0);
            cr.set_line_width(1.0);
            cr.stroke().ok();

            cr.set_font_size(9.0);
            cr.set_source_rgb(1.0, 1.0, 1.0);
            let rot_text = format!("{}°", transform.rotate);
            cr.move_to(rot_x - 8.0, rot_y + 3.0);
            let _ = cr.show_text(&rot_text);
        });
    }

    // ── GestureClick — rotation handle ────────────────────────────────────
    let click = GestureClick::new();
    {
        let state = state.clone();
        let area_c = transform_area.clone();
        click.connect_pressed(move |gesture, _n_press, x, y| {
            let w = area_c.width() as f64;
            let h = area_c.height() as f64;
            let transform = {
                let st = state.borrow();
                match st.transform {
                    Some(t) => t,
                    None => return,
                }
            };
            if hit_handle(x, y, w, h, &transform) == Some(HandleKind::Rotate) {
                let new_rotate = (transform.rotate + 90) % 360;
                let cb = {
                    let mut st = state.borrow_mut();
                    if let Some(ref mut t) = st.transform {
                        t.rotate = new_rotate;
                    }
                    st.on_changed.clone()
                };
                let updated = state.borrow().transform.unwrap_or(transform);
                if let Some(cb) = cb {
                    cb(updated.crop_left, updated.crop_right, updated.crop_top,
                       updated.crop_bottom, updated.rotate, updated.flip_h, updated.flip_v);
                }
                area_c.queue_draw();
                gesture.set_state(gtk4::EventSequenceState::Claimed);
            }
        });
    }
    transform_area.add_controller(click);

    // ── GestureDrag — crop handles ────────────────────────────────────────
    let drag = GestureDrag::new();
    {
        let state = state.clone();
        let area_c = transform_area.clone();
        drag.connect_drag_begin(move |_gesture, x, y| {
            let w = area_c.width() as f64;
            let h = area_c.height() as f64;
            let mut st = state.borrow_mut();
            if let Some(t) = st.transform {
                // Rotate is handled by GestureClick; filter it out here.
                let kind = hit_handle(x, y, w, h, &t)
                    .filter(|k| *k != HandleKind::Rotate);
                st.drag_handle = kind;
                st.drag_start_pos = (x, y);
                st.drag_start_transform = t;
            }
        });
    }
    {
        let state = state.clone();
        let area_c = transform_area.clone();
        drag.connect_drag_update(move |_gesture, offset_x, offset_y| {
            let w = area_c.width() as f64;
            let h = area_c.height() as f64;

            let (new_t, cb) = {
                let mut st = state.borrow_mut();
                let handle = match st.drag_handle {
                    Some(hk) => hk,
                    None => return,
                };
                let base = st.drag_start_transform;
                let dx = offset_x * (1920.0 / w);
                let dy = offset_y * (1080.0 / h);
                let cl = |v: f64| v.clamp(0.0, 500.0) as i32;

                let new_t = match handle {
                    HandleKind::Left        => ClipTransform { crop_left:   cl(base.crop_left   as f64 + dx), ..base },
                    HandleKind::Right       => ClipTransform { crop_right:  cl(base.crop_right  as f64 - dx), ..base },
                    HandleKind::Top         => ClipTransform { crop_top:    cl(base.crop_top    as f64 + dy), ..base },
                    HandleKind::Bottom      => ClipTransform { crop_bottom: cl(base.crop_bottom as f64 - dy), ..base },
                    HandleKind::TopLeft     => ClipTransform { crop_left: cl(base.crop_left as f64 + dx), crop_top: cl(base.crop_top as f64 + dy), ..base },
                    HandleKind::TopRight    => ClipTransform { crop_right: cl(base.crop_right as f64 - dx), crop_top: cl(base.crop_top as f64 + dy), ..base },
                    HandleKind::BottomLeft  => ClipTransform { crop_left: cl(base.crop_left as f64 + dx), crop_bottom: cl(base.crop_bottom as f64 - dy), ..base },
                    HandleKind::BottomRight => ClipTransform { crop_right: cl(base.crop_right as f64 - dx), crop_bottom: cl(base.crop_bottom as f64 - dy), ..base },
                    HandleKind::Rotate      => return,
                };

                st.transform = Some(new_t);
                let cb = st.on_changed.clone();
                (new_t, cb)
            };

            if let Some(cb) = cb {
                cb(new_t.crop_left, new_t.crop_right, new_t.crop_top,
                   new_t.crop_bottom, new_t.rotate, new_t.flip_h, new_t.flip_v);
            }
            area_c.queue_draw();
        });
    }
    {
        let state = state.clone();
        drag.connect_drag_end(move |_gesture, _ox, _oy| {
            state.borrow_mut().drag_handle = None;
        });
    }
    transform_area.add_controller(drag);

    let transform_overlay = TransformOverlay { state, area: transform_area };
    (root, transform_overlay)
}

/// Compute the on-screen crop box (left, right, top, bottom) in DrawingArea pixels.
fn crop_box(dw: f64, dh: f64, t: &ClipTransform) -> (f64, f64, f64, f64) {
    let sx = dw / 1920.0;
    let sy = dh / 1080.0;
    let left   = t.crop_left   as f64 * sx;
    let right  = dw - t.crop_right  as f64 * sx;
    let top    = t.crop_top    as f64 * sy;
    let bottom = dh - t.crop_bottom as f64 * sy;
    (left, right, top, bottom)
}

/// Draw a single 8×8 handle square centered at (cx, cy).
fn draw_handle(cr: &gtk4::cairo::Context, cx: f64, cy: f64) {
    cr.set_source_rgb(1.0, 1.0, 1.0);
    cr.rectangle(cx - 4.0, cy - 4.0, 8.0, 8.0);
    cr.fill().ok();
    cr.set_source_rgb(0.2, 0.2, 0.2);
    cr.set_line_width(1.0);
    cr.rectangle(cx - 4.0, cy - 4.0, 8.0, 8.0);
    cr.stroke().ok();
}

/// Return the HandleKind nearest to (x, y) within 12 px, or None.
fn hit_handle(x: f64, y: f64, w: f64, h: f64, t: &ClipTransform) -> Option<HandleKind> {
    let (bl, br, bt, bb) = crop_box(w, h, t);
    let mid_x = (bl + br) / 2.0;
    let mid_y = (bt + bb) / 2.0;
    let rot_y = (bt - 24.0).max(8.0);

    let dist = |px: f64, py: f64| ((x - px).powi(2) + (y - py).powi(2)).sqrt();

    let candidates = [
        (HandleKind::Rotate,      dist(mid_x, rot_y)),
        (HandleKind::TopLeft,     dist(bl,    bt)),
        (HandleKind::TopRight,    dist(br,    bt)),
        (HandleKind::BottomLeft,  dist(bl,    bb)),
        (HandleKind::BottomRight, dist(br,    bb)),
        (HandleKind::Left,        dist(bl,    mid_y)),
        (HandleKind::Right,       dist(br,    mid_y)),
        (HandleKind::Top,         dist(mid_x, bt)),
        (HandleKind::Bottom,      dist(mid_x, bb)),
    ];

    candidates.iter()
        .filter(|(_, d)| *d <= 12.0)
        .min_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal))
        .map(|(k, _)| *k)
}

fn format_timecode(ns: u64) -> String {
    let total_frames = ns / (1_000_000_000 / 30); // 30 fps display
    let frames = total_frames % 30;
    let secs   = ns / 1_000_000_000;
    let s      = secs % 60;
    let m      = (secs / 60) % 60;
    let h      = secs / 3600;
    format!("{h:02}:{m:02}:{s:02};{frames:02}")
}
