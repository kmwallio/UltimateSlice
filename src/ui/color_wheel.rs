/// Color wheel widget for 3-point color balance grading.
///
/// Renders an HSV color circle via Cairo `DrawingArea` with a draggable
/// indicator. Used by the inspector to replace raw R/G/B sliders with an
/// intuitive wheel for shadows, midtones, and highlights zones.

use gtk4::prelude::*;
use gtk4::{self as gtk, DrawingArea, Orientation, Scale};
use std::cell::{Cell, RefCell};
use std::f64::consts::{PI, TAU};
use std::rc::Rc;

// ── HSV ↔ RGB conversion ────────────────────────────────────────────

/// Convert HSV (h in 0..360, s/v in 0..1) to RGB (each 0..1).
fn hsv_to_rgb(h: f64, s: f64, v: f64) -> (f64, f64, f64) {
    if s <= 0.0 {
        return (v, v, v);
    }
    let hh = (h % 360.0) / 60.0;
    let i = hh.floor() as i32;
    let ff = hh - i as f64;
    let p = v * (1.0 - s);
    let q = v * (1.0 - s * ff);
    let t = v * (1.0 - s * (1.0 - ff));
    match i {
        0 => (v, t, p),
        1 => (q, v, p),
        2 => (p, v, t),
        3 => (p, q, v),
        4 => (t, p, v),
        _ => (v, p, q),
    }
}

/// Convert RGB (each 0..1) to HSV (h in 0..360, s/v in 0..1).
fn rgb_to_hsv(r: f64, g: f64, b: f64) -> (f64, f64, f64) {
    let max = r.max(g).max(b);
    let min = r.min(g).min(b);
    let delta = max - min;

    let v = max;
    let s = if max > 0.0 { delta / max } else { 0.0 };
    let h = if delta < 1e-9 {
        0.0
    } else if (max - r).abs() < 1e-9 {
        60.0 * (((g - b) / delta) % 6.0)
    } else if (max - g).abs() < 1e-9 {
        60.0 * ((b - r) / delta + 2.0)
    } else {
        60.0 * ((r - g) / delta + 4.0)
    };
    let h = if h < 0.0 { h + 360.0 } else { h };
    (h, s, v)
}

// ── Wheel position ↔ RGB mapping ─────────────────────────────────────

/// The color wheel represents a color offset from a neutral center.
/// `neutral` is the RGB value when the indicator is at the center
/// (e.g. (0,0,0) for shadows, (0.5,0.5,0.5) for midtones, (1,1,1) for highlights).
///
/// Position in the wheel:
/// - angle → hue
/// - distance from center → saturation of the color shift
/// - luminance slider → scales the neutral brightness

/// Convert a wheel (angle, radius_fraction) + luminance to an RGB triplet.
/// `angle` is in radians (0 = right, counter-clockwise), `radius_frac` in 0..1.
/// `luminance` is 0..1 controlling overall brightness.
pub fn wheel_pos_to_rgb(angle: f64, radius_frac: f64, luminance: f64) -> (f64, f64, f64) {
    let sat = radius_frac.clamp(0.0, 1.0);
    // Map angle to hue (0..360). Top = red (0°), clockwise.
    // Standard HSV: 0° = right. We rotate so top of wheel = 0° hue (red).
    let hue_deg = (90.0 - angle.to_degrees()).rem_euclid(360.0);
    let v = luminance.clamp(0.0, 1.0);
    hsv_to_rgb(hue_deg, sat, v)
}

/// Convert an RGB triplet to wheel position (angle_rad, radius_frac, luminance).
pub fn rgb_to_wheel_pos(r: f64, g: f64, b: f64) -> (f64, f64, f64) {
    let (h, s, v) = rgb_to_hsv(r, g, b);
    // Reverse the hue→angle mapping: angle = (90 - hue)° in radians.
    let angle = (90.0 - h).to_radians();
    (angle, s, v)
}

// ── Drawing helpers ──────────────────────────────────────────────────

/// Render the HSV color wheel background into the given Cairo context.
/// Draws concentric rings of hue/saturation at the given brightness `value`.
fn draw_wheel_background(
    cr: &gtk::cairo::Context,
    cx: f64,
    cy: f64,
    radius: f64,
    value: f64,
) {
    // Draw from outside in so inner pixels overwrite outer.
    let steps = (radius * 1.5).max(60.0) as i32;
    let angle_steps = 256;
    let v = value.clamp(0.05, 1.0);

    for ri in 0..steps {
        let frac = ri as f64 / steps as f64;
        let r_inner = radius * frac;
        let r_outer = radius * (frac + 1.0 / steps as f64);
        let sat = frac;

        for ai in 0..angle_steps {
            let a0 = TAU * ai as f64 / angle_steps as f64;
            let a1 = TAU * (ai as f64 + 1.2) / angle_steps as f64;
            // Hue: top = red (0°). Standard HSV 0° is right, so offset by 90°.
            let hue_deg = (90.0 - a0.to_degrees()).rem_euclid(360.0);
            let (red, grn, blu) = hsv_to_rgb(hue_deg, sat, v);
            cr.set_source_rgb(red, grn, blu);
            cr.arc(cx, cy, r_outer, a0 - PI / 2.0, a1 - PI / 2.0);
            cr.arc_negative(cx, cy, r_inner, a1 - PI / 2.0, a0 - PI / 2.0);
            cr.close_path();
            let _ = cr.fill();
        }
    }

    // Soft edge: dark ring around the wheel.
    cr.set_source_rgba(0.12, 0.12, 0.12, 0.9);
    cr.set_line_width(1.5);
    cr.arc(cx, cy, radius, 0.0, TAU);
    let _ = cr.stroke();
}

/// Draw the indicator dot at the given wheel position.
fn draw_indicator(
    cr: &gtk::cairo::Context,
    cx: f64,
    cy: f64,
    radius: f64,
    angle: f64,
    radius_frac: f64,
) {
    let dist = radius * radius_frac.clamp(0.0, 1.0);
    let ix = cx + dist * angle.cos();
    let iy = cy - dist * angle.sin();

    // Line from center to indicator.
    cr.set_source_rgba(0.0, 0.0, 0.0, 0.5);
    cr.set_line_width(1.0);
    cr.move_to(cx, cy);
    cr.line_to(ix, iy);
    let _ = cr.stroke();

    // Indicator dot — white ring with dark outline.
    cr.set_source_rgba(0.0, 0.0, 0.0, 0.7);
    cr.arc(ix, iy, 7.0, 0.0, TAU);
    let _ = cr.fill();
    cr.set_source_rgba(1.0, 1.0, 1.0, 0.95);
    cr.arc(ix, iy, 5.5, 0.0, TAU);
    let _ = cr.fill();

    // Center crosshair.
    cr.set_source_rgba(0.7, 0.7, 0.7, 0.5);
    cr.set_line_width(0.8);
    cr.move_to(cx - 4.0, cy);
    cr.line_to(cx + 4.0, cy);
    cr.move_to(cx, cy - 4.0);
    cr.line_to(cx, cy + 4.0);
    let _ = cr.stroke();
}

// ── Public widget builder ────────────────────────────────────────────

/// State shared between the drawing area, gestures, and slider.
struct WheelState {
    angle: f64,
    radius_frac: f64,
    luminance: f64,
}

/// Build a complete color wheel widget with luminance slider.
///
/// Returns a `gtk::Box` containing the wheel DrawingArea and a luminance
/// Scale beneath it. `initial_rgb` sets the starting color. `on_changed`
/// fires whenever the user adjusts the wheel or slider, passing (r, g, b).
///
/// `size` is the diameter of the wheel in pixels.
pub fn build_color_wheel(
    size: i32,
    initial_rgb: (f64, f64, f64),
    on_changed: impl Fn(f64, f64, f64) + 'static,
) -> (gtk::Box, Rc<dyn Fn(f64, f64, f64)>) {
    let (angle, sat, lum) = rgb_to_wheel_pos(initial_rgb.0, initial_rgb.1, initial_rgb.2);

    let state = Rc::new(RefCell::new(WheelState {
        angle,
        radius_frac: sat,
        luminance: lum,
    }));
    let on_changed = Rc::new(on_changed);
    let updating = Rc::new(Cell::new(false));

    // ── DrawingArea ──
    let da = DrawingArea::new();
    da.set_content_width(size);
    da.set_content_height(size);
    da.set_halign(gtk::Align::Center);

    {
        let st = state.clone();
        da.set_draw_func(move |_da, cr, ww, wh| {
            let w = ww as f64;
            let h = wh as f64;
            let cx = w / 2.0;
            let cy = h / 2.0;
            let radius = (w.min(h) / 2.0) - 4.0;

            let s = st.borrow();
            draw_wheel_background(cr, cx, cy, radius, s.luminance);
            draw_indicator(cr, cx, cy, radius, s.angle, s.radius_frac);
        });
    }

    // ── Helper: update from (x, y) hit position ──
    let fire_change = {
        let state = state.clone();
        let on_changed = on_changed.clone();
        let da = da.clone();
        Rc::new(move |x: f64, y: f64| {
            let w = da.width() as f64;
            let h = da.height() as f64;
            let cx = w / 2.0;
            let cy = h / 2.0;
            let radius = (w.min(h) / 2.0) - 4.0;

            let dx = x - cx;
            let dy = cy - y; // Y-up for math
            let dist = (dx * dx + dy * dy).sqrt();
            let frac = (dist / radius).clamp(0.0, 1.0);
            let angle = dy.atan2(dx);

            let lum;
            {
                let mut s = state.borrow_mut();
                s.angle = angle;
                s.radius_frac = frac;
                lum = s.luminance;
            }

            let (r, g, b) = wheel_pos_to_rgb(angle, frac, lum);
            on_changed(r, g, b);
            da.queue_draw();
        })
    };

    // ── GestureClick ──
    {
        let fire = fire_change.clone();
        let click = gtk::GestureClick::new();
        click.set_button(1);
        click.connect_pressed(move |_g, n_press, x, y| {
            if n_press == 2 {
                // Double-click handled below (reset).
                return;
            }
            fire(x, y);
        });
        da.add_controller(click);
    }

    // ── Double-click to reset ──
    {
        let state = state.clone();
        let on_changed = on_changed.clone();
        let da_reset = da.clone();
        let dbl = gtk::GestureClick::new();
        dbl.set_button(1);
        dbl.connect_pressed(move |_g, n_press, _x, _y| {
            if n_press < 2 {
                return;
            }
            let lum;
            {
                let mut s = state.borrow_mut();
                s.angle = 0.0;
                s.radius_frac = 0.0;
                lum = s.luminance;
            }
            let (r, g, b) = wheel_pos_to_rgb(0.0, 0.0, lum);
            on_changed(r, g, b);
            da_reset.queue_draw();
        });
        da.add_controller(dbl);
    }

    // ── GestureDrag ──
    {
        let fire = fire_change.clone();
        let drag_start = Rc::new(Cell::new((0.0_f64, 0.0_f64)));
        let drag = gtk::GestureDrag::new();
        drag.set_button(1);
        {
            let ds = drag_start.clone();
            let fire = fire.clone();
            drag.connect_drag_begin(move |_g, x, y| {
                ds.set((x, y));
                fire(x, y);
            });
        }
        {
            let ds = drag_start.clone();
            drag.connect_drag_update(move |_g, off_x, off_y| {
                let (sx, sy) = ds.get();
                fire(sx + off_x, sy + off_y);
            });
        }
        da.add_controller(drag);
    }

    // ── Luminance slider ──
    let lum_slider = Scale::with_range(Orientation::Horizontal, 0.0, 1.0, 0.01);
    lum_slider.set_value(state.borrow().luminance);
    lum_slider.set_draw_value(false);
    lum_slider.set_hexpand(true);
    lum_slider.add_mark(0.5, gtk::PositionType::Bottom, None);
    {
        let state = state.clone();
        let on_changed = on_changed.clone();
        let da_lum = da.clone();
        let updating = updating.clone();
        lum_slider.connect_value_changed(move |s| {
            if updating.get() {
                return;
            }
            let v = s.value();
            let (angle, frac);
            {
                let mut st = state.borrow_mut();
                st.luminance = v;
                angle = st.angle;
                frac = st.radius_frac;
            }
            let (r, g, b) = wheel_pos_to_rgb(angle, frac, v);
            on_changed(r, g, b);
            da_lum.queue_draw();
        });
    }

    // ── Layout ──
    let vbox = gtk::Box::new(Orientation::Vertical, 2);
    vbox.append(&da);
    vbox.append(&lum_slider);

    // ── External setter (for updating from model without firing callbacks) ──
    let set_rgb: Rc<dyn Fn(f64, f64, f64)> = {
        let state = state.clone();
        let da_ext = da.clone();
        let lum_slider = lum_slider.clone();
        let updating = updating.clone();
        Rc::new(move |r: f64, g: f64, b: f64| {
            let (a, s, v) = rgb_to_wheel_pos(r, g, b);
            {
                let mut st = state.borrow_mut();
                st.angle = a;
                st.radius_frac = s;
                st.luminance = v;
            }
            updating.set(true);
            lum_slider.set_value(v);
            updating.set(false);
            da_ext.queue_draw();
        })
    };

    (vbox, set_rgb)
}

// ── Tests ────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hsv_rgb_roundtrip() {
        let cases = [
            (0.0, 1.0, 1.0),   // pure red
            (120.0, 1.0, 1.0), // pure green
            (240.0, 1.0, 1.0), // pure blue
            (60.0, 0.5, 0.8),  // muted yellow
            (0.0, 0.0, 0.5),   // neutral gray
        ];
        for (h, s, v) in cases {
            let (r, g, b) = hsv_to_rgb(h, s, v);
            let (h2, s2, v2) = rgb_to_hsv(r, g, b);
            assert!(
                (v - v2).abs() < 0.001,
                "V mismatch: {v} vs {v2} for ({h},{s},{v})"
            );
            assert!(
                (s - s2).abs() < 0.001,
                "S mismatch: {s} vs {s2} for ({h},{s},{v})"
            );
            if s > 0.01 {
                let dh = (h - h2).abs();
                let dh = dh.min(360.0 - dh);
                assert!(dh < 0.5, "H mismatch: {h} vs {h2}");
            }
        }
    }

    #[test]
    fn wheel_pos_roundtrip() {
        let test_rgbs = [
            (0.5, 0.5, 0.5), // neutral gray
            (1.0, 0.0, 0.0), // pure red
            (0.0, 1.0, 0.0), // pure green
            (0.0, 0.0, 1.0), // pure blue
            (0.8, 0.3, 0.6), // pink-ish
        ];
        for (r, g, b) in test_rgbs {
            let (angle, frac, lum) = rgb_to_wheel_pos(r, g, b);
            let (r2, g2, b2) = wheel_pos_to_rgb(angle, frac, lum);
            assert!(
                (r - r2).abs() < 0.01 && (g - g2).abs() < 0.01 && (b - b2).abs() < 0.01,
                "Roundtrip failed: ({r},{g},{b}) → ({r2},{g2},{b2})"
            );
        }
    }

    #[test]
    fn neutral_center_is_gray() {
        // At center (radius_frac=0), output should be pure gray at the luminance.
        let (r, g, b) = wheel_pos_to_rgb(0.0, 0.0, 0.5);
        assert!((r - 0.5).abs() < 0.001);
        assert!((g - 0.5).abs() < 0.001);
        assert!((b - 0.5).abs() < 0.001);
    }
}
