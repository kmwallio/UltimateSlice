/// Curve editor widget for the frei0r "curves" effect.
///
/// Renders a tone curve with draggable control points and smooth Catmull-Rom
/// spline interpolation via Cairo DrawingArea. Used by the inspector to replace
/// raw numeric sliders with an intuitive graphical curve editor.

use gtk4::prelude::*;
use gtk4::{self as gtk, DrawingArea, Orientation};
use std::cell::{Cell, RefCell};
use std::f64::consts::TAU;
use std::rc::Rc;

const MARGIN: f64 = 16.0;
const POINT_RADIUS: f64 = 6.0;
const HIT_RADIUS: f64 = 12.0;

struct CurveState {
    /// Control points (input, output) in 0..1 space, sorted by input.
    points: Vec<(f64, f64)>,
    /// Index of the point being dragged, if any.
    selected: Option<usize>,
    /// Channel dropdown index.
    channel: usize,
}

const CHANNEL_LABELS: &[&str] = &["Red", "Green", "Blue", "RGB", "Luma"];

/// Map dropdown index to frei0r `channel` param value.
fn channel_idx_to_frei0r(idx: usize) -> f64 {
    match idx {
        0 => 0.0,   // Red
        1 => 0.1,   // Green
        2 => 0.2,   // Blue
        3 => 0.5,   // RGB
        4 => 0.4,   // Luma
        _ => 0.5,
    }
}

/// Map frei0r `channel` param value to dropdown index.
pub fn frei0r_to_channel_idx(val: f64) -> usize {
    if (val - 0.0).abs() < 0.05 {
        0
    } else if (val - 0.1).abs() < 0.05 {
        1
    } else if (val - 0.2).abs() < 0.05 {
        2
    } else if (val - 0.5).abs() < 0.05 {
        3
    } else if (val - 0.4).abs() < 0.05 {
        4
    } else {
        3 // default RGB
    }
}

/// Convert a point from 0..1 space to screen coordinates.
fn to_screen(inp: f64, out: f64, w: f64, h: f64) -> (f64, f64) {
    (
        MARGIN + inp * (w - 2.0 * MARGIN),
        MARGIN + (1.0 - out) * (h - 2.0 * MARGIN),
    )
}

/// Convert screen coordinates to 0..1 space.
fn from_screen(sx: f64, sy: f64, w: f64, h: f64) -> (f64, f64) {
    (
        ((sx - MARGIN) / (w - 2.0 * MARGIN)).clamp(0.0, 1.0),
        (1.0 - (sy - MARGIN) / (h - 2.0 * MARGIN)).clamp(0.0, 1.0),
    )
}

/// Curve color per channel.
fn channel_color(idx: usize) -> (f64, f64, f64) {
    match idx {
        0 => (1.0, 0.3, 0.3),
        1 => (0.3, 1.0, 0.3),
        2 => (0.3, 0.5, 1.0),
        _ => (0.9, 0.9, 0.9),
    }
}

/// Find the index of the control point closest to the screen position, if within hit radius.
fn hit_test(points: &[(f64, f64)], sx: f64, sy: f64, w: f64, h: f64) -> Option<usize> {
    points.iter().enumerate().find_map(|(i, &(inp, out))| {
        let (px, py) = to_screen(inp, out, w, h);
        if ((sx - px).powi(2) + (sy - py).powi(2)).sqrt() <= HIT_RADIUS {
            Some(i)
        } else {
            None
        }
    })
}

/// Draw the curve editor: background, grid, baseline, spline, and control points.
fn draw_curves(ctx: &gtk::cairo::Context, w: f64, h: f64, state: &CurveState) {
    let dw = w - 2.0 * MARGIN;
    let dh = h - 2.0 * MARGIN;

    // Dark background
    ctx.set_source_rgb(0.1, 0.1, 0.11);
    ctx.rectangle(0.0, 0.0, w, h);
    let _ = ctx.fill();

    // Grid at 25% intervals
    ctx.set_source_rgba(0.3, 0.3, 0.32, 0.5);
    ctx.set_line_width(0.5);
    for i in 0..=4 {
        let f = i as f64 / 4.0;
        let x = MARGIN + f * dw;
        let y = MARGIN + f * dh;
        ctx.move_to(x, MARGIN);
        ctx.line_to(x, MARGIN + dh);
        ctx.move_to(MARGIN, y);
        ctx.line_to(MARGIN + dw, y);
    }
    let _ = ctx.stroke();

    // Diagonal baseline (identity line)
    ctx.set_source_rgba(0.4, 0.4, 0.42, 0.6);
    ctx.set_line_width(1.0);
    let (x0, y0) = to_screen(0.0, 0.0, w, h);
    let (x1, y1) = to_screen(1.0, 1.0, w, h);
    ctx.move_to(x0, y0);
    ctx.line_to(x1, y1);
    let _ = ctx.stroke();

    // Spline curve through control points
    let pts = &state.points;
    if pts.len() >= 2 {
        let (cc_r, cc_g, cc_b) = channel_color(state.channel);
        ctx.set_source_rgb(cc_r, cc_g, cc_b);
        ctx.set_line_width(2.0);

        // Extend from left edge to first point
        let (sx, sy) = to_screen(0.0, pts[0].1, w, h);
        ctx.move_to(sx, sy);
        if pts[0].0 > 0.001 {
            let (sx, sy) = to_screen(pts[0].0, pts[0].1, w, h);
            ctx.line_to(sx, sy);
        }

        // Catmull-Rom spline segments
        for i in 0..pts.len() - 1 {
            let p0 = if i > 0 {
                pts[i - 1]
            } else {
                (2.0 * pts[0].0 - pts[1].0, 2.0 * pts[0].1 - pts[1].1)
            };
            let p1 = pts[i];
            let p2 = pts[i + 1];
            let p3 = if i + 2 < pts.len() {
                pts[i + 2]
            } else {
                let n = pts.len() - 1;
                (2.0 * pts[n].0 - pts[n - 1].0, 2.0 * pts[n].1 - pts[n - 1].1)
            };

            // Catmull-Rom → cubic bezier control points
            let cp1x = p1.0 + (p2.0 - p0.0) / 6.0;
            let cp1y = p1.1 + (p2.1 - p0.1) / 6.0;
            let cp2x = p2.0 - (p3.0 - p1.0) / 6.0;
            let cp2y = p2.1 - (p3.1 - p1.1) / 6.0;

            let (cx1, cy1) = to_screen(cp1x, cp1y, w, h);
            let (cx2, cy2) = to_screen(cp2x, cp2y, w, h);
            let (ex, ey) = to_screen(p2.0, p2.1, w, h);
            ctx.curve_to(cx1, cy1, cx2, cy2, ex, ey);
        }

        // Extend from last point to right edge
        let last = pts.last().unwrap();
        if last.0 < 0.999 {
            let (ex, ey) = to_screen(1.0, last.1, w, h);
            ctx.line_to(ex, ey);
        }
        let _ = ctx.stroke();
    }

    // Control points
    for (i, &(inp, out)) in pts.iter().enumerate() {
        let (sx, sy) = to_screen(inp, out, w, h);
        let is_sel = state.selected == Some(i);
        let r = if is_sel { POINT_RADIUS + 2.0 } else { POINT_RADIUS };

        // Dark outline ring
        ctx.set_source_rgba(0.0, 0.0, 0.0, 0.8);
        ctx.arc(sx, sy, r + 1.5, 0.0, TAU);
        let _ = ctx.fill();

        // Inner fill
        if is_sel {
            ctx.set_source_rgb(1.0, 1.0, 1.0);
        } else {
            let (cc_r, cc_g, cc_b) = channel_color(state.channel);
            ctx.set_source_rgb(cc_r, cc_g, cc_b);
        }
        ctx.arc(sx, sy, r, 0.0, TAU);
        let _ = ctx.fill();
    }
}

/// Build a curve editor widget for the frei0r curves effect.
///
/// * `channel_frei0r` — initial frei0r `channel` param value (0.0/0.1/0.2/0.4/0.5).
/// * `initial_points` — initial (input, output) control points in 0..1 space.
/// * `on_changed` — fired with `(channel_frei0r_val, &points)` on every user change.
pub fn build_curves_widget(
    channel_frei0r: f64,
    initial_points: Vec<(f64, f64)>,
    on_changed: impl Fn(f64, &[(f64, f64)]) + 'static,
) -> gtk::Box {
    let ch_idx = frei0r_to_channel_idx(channel_frei0r);
    let mut points = initial_points;
    if points.len() < 2 {
        points = vec![(0.0, 0.0), (1.0, 1.0)];
    }
    points.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap());

    let state = Rc::new(RefCell::new(CurveState {
        points,
        selected: None,
        channel: ch_idx,
    }));
    let on_changed = Rc::new(on_changed);

    // ── Channel selector ──
    let ch_list = gtk::StringList::new(CHANNEL_LABELS);
    let ch_dd = gtk::DropDown::new(Some(ch_list), gtk4::Expression::NONE);
    ch_dd.set_selected(ch_idx as u32);
    ch_dd.set_margin_start(4);
    ch_dd.set_margin_end(4);

    // ── DrawingArea ──
    let da = DrawingArea::new();
    da.set_content_width(240);
    da.set_content_height(240);
    da.set_halign(gtk::Align::Center);

    {
        let st = state.clone();
        da.set_draw_func(move |_da, ctx, ww, wh| {
            draw_curves(ctx, ww as f64, wh as f64, &st.borrow());
        });
    }

    // ── Fire change callback ──
    let fire = {
        let state = state.clone();
        let on_changed = on_changed.clone();
        Rc::new(move || {
            let s = state.borrow();
            on_changed(channel_idx_to_frei0r(s.channel), &s.points);
        })
    };

    // ── GestureClick: select point / double-click to add or remove ──
    {
        let state = state.clone();
        let da_c = da.clone();
        let fire = fire.clone();
        let click = gtk::GestureClick::new();
        click.set_button(1);
        click.connect_pressed(move |_g, n_press, x, y| {
            let w = da_c.width() as f64;
            let h = da_c.height() as f64;

            if n_press >= 2 {
                let mut s = state.borrow_mut();
                if let Some(hit) = hit_test(&s.points, x, y, w, h) {
                    // Double-click on existing point: remove (keep at least 2)
                    if s.points.len() > 2 {
                        s.points.remove(hit);
                        s.selected = None;
                        drop(s);
                        fire();
                        da_c.queue_draw();
                    }
                } else if s.points.len() < 5 {
                    // Double-click on empty area: add new point
                    let (inp, out) = from_screen(x, y, w, h);
                    s.points.push((inp, out));
                    s.points
                        .sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap());
                    s.selected = s.points.iter().position(|&p| {
                        (p.0 - inp).abs() < 0.001 && (p.1 - out).abs() < 0.001
                    });
                    drop(s);
                    fire();
                    da_c.queue_draw();
                }
                return;
            }

            // Single click: select nearest point
            let mut s = state.borrow_mut();
            s.selected = hit_test(&s.points, x, y, w, h);
            drop(s);
            da_c.queue_draw();
        });
        da.add_controller(click);
    }

    // ── GestureDrag: move selected point ──
    {
        let state = state.clone();
        let da_d = da.clone();
        let fire = fire.clone();
        let drag_origin = Rc::new(Cell::new((0.0_f64, 0.0_f64)));
        let drag = gtk::GestureDrag::new();
        drag.set_button(1);

        {
            let state = state.clone();
            let da_db = da_d.clone();
            let ds = drag_origin.clone();
            drag.connect_drag_begin(move |_g, x, y| {
                ds.set((x, y));
                let w = da_db.width() as f64;
                let h = da_db.height() as f64;
                let mut s = state.borrow_mut();
                s.selected = hit_test(&s.points, x, y, w, h);
            });
        }

        {
            let state = state.clone();
            let da_du = da_d.clone();
            let ds = drag_origin.clone();
            let fire = fire.clone();
            drag.connect_drag_update(move |_g, off_x, off_y| {
                let w = da_du.width() as f64;
                let h = da_du.height() as f64;
                let (sx, sy) = ds.get();
                let (inp, out) = from_screen(sx + off_x, sy + off_y, w, h);

                let mut s = state.borrow_mut();
                if let Some(idx) = s.selected {
                    s.points[idx] = (inp, out);
                    let target = (inp, out);
                    s.points
                        .sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap());
                    s.selected = s.points.iter().position(|p| {
                        (p.0 - target.0).abs() < 0.0001
                            && (p.1 - target.1).abs() < 0.0001
                    });
                    drop(s);
                    fire();
                    da_du.queue_draw();
                }
            });
        }

        da.add_controller(drag);
    }

    // ── Channel dropdown change ──
    {
        let state = state.clone();
        let fire = fire.clone();
        let da_ch = da.clone();
        ch_dd.connect_selected_notify(move |dd| {
            state.borrow_mut().channel = dd.selected() as usize;
            fire();
            da_ch.queue_draw();
        });
    }

    // ── Layout ──
    let vbox = gtk::Box::new(Orientation::Vertical, 4);
    vbox.append(&ch_dd);
    vbox.append(&da);
    vbox
}
