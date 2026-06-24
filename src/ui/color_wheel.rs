/// Color wheel widget for 3-point color balance grading.
///
/// Renders a color-balance disc via Cairo `DrawingArea` with a draggable
/// indicator. Used by the inspector to replace raw R/G/B sliders with an
/// intuitive wheel for shadows, midtones, and highlights zones.
///
/// ## Mapping to the frei0r `three_point_balance` params
///
/// The wheel drives the `black-color` / `gray-color` / `white-color` params
/// of the frei0r `three_point_balance` plugin. That plugin fits a parabola
/// per channel through `(black_c, 0) (gray_c, 0.5) (white_c, 1.0)` mapping
/// **input → output**, so each param is *the input level that becomes
/// 0 / mid / white*. The crucial consequence: **lowering a channel's param
/// brightens that channel in the output** (its target output is reached at a
/// lower input). A naïve "param = HSV color" mapping therefore (a) inverts
/// the perceived hue and (b) — because HSV at a fixed value caps every
/// channel — clips many different wheel positions to similar muddy results.
///
/// Instead, this wheel maps position to a **centered offset** around the
/// zone's neutral param: it *subtracts* a zero-sum, hue-directed vector from
/// the neutral so the output gains exactly the puck's hue, and the magnitude
/// scales with radius. The disc and the puck are painted with the resulting
/// **output tint**, so what you see in the circle is what lands on the video.
use gtk4::prelude::*;
use gtk4::{self as gtk, DrawingArea, Orientation, Scale};
use std::cell::{Cell, RefCell};
#[cfg(test)]
use std::f64::consts::PI;
use std::f64::consts::TAU;
use std::rc::Rc;

/// Maximum per-channel hue push at the rim (radius = 1), in output space.
/// Color balance is a gentle effect, so the rim tints toward (but never
/// saturates to) the chosen hue — a pure primary at the rim shifts a
/// channel by ~`MAX_PUSH` and its complements by ~`MAX_PUSH/2`.
const MAX_PUSH: f64 = 0.4;

/// Range of the master (luminance) slider as a uniform shift applied to all
/// three params. `0.5` on the slider is neutral; `1.0` brightens the zone by
/// lowering every param by `MASTER_RANGE/2`, `0.0` darkens it.
const MASTER_RANGE: f64 = 1.0;

/// Safe interior band each frei0r control-point param is clamped to, keyed on
/// the zone neutral. The `three_point_balance` parabola is fit through the
/// three control points `(black, 0) (gray, 0.5) (white, 1)`; if the moving
/// point approaches a neighbouring fixed point the parabola degenerates and
/// the channel mapping inverts — which made the master slider flip the tint
/// (e.g. blue → yellow) once it pushed a channel near 0 or 1. Empirically the
/// mapping stays monotonic and hue-faithful while the moving point keeps a
/// margin from its neighbours, so we clamp:
/// - midtones (gray, neighbours 0 / 1) → `[0.25, 0.75]`
/// - shadows  (black, neighbour gray 0.5) → `[0.0, 0.35]`
/// - highlights (white, neighbour gray 0.5) → `[0.65, 1.0]`
///
/// Pushes within the band stay faithful; at the band edge the tint washes
/// toward neutral instead of inverting.
fn safe_band(neutral: (f64, f64, f64)) -> (f64, f64) {
    let n = neutral.0;
    if n < 0.25 {
        (0.0, 0.35)
    } else if n > 0.75 {
        (0.65, 1.0)
    } else {
        (0.25, 0.75)
    }
}

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

// ── Hue direction + output tint ──────────────────────────────────────

/// Zero-sum RGB direction for a hue (`hsv(hue,1,1)` minus its mean). Adding a
/// positive multiple of this to a neutral gray tints it toward `hue_deg`
/// while keeping overall brightness roughly fixed.
fn centered_dir(hue_deg: f64) -> (f64, f64, f64) {
    let (r, g, b) = hsv_to_rgb(hue_deg, 1.0, 1.0);
    let mean = (r + g + b) / 3.0;
    (r - mean, g - mean, b - mean)
}

/// The output tint shown on the disc / puck for a given hue + radius:
/// mid-gray pushed toward the hue by `radius * MAX_PUSH`. This is exactly the
/// midtone color the grade produces, so the circle visually matches the
/// applied result regardless of which zone (shadows/mid/highlights) is being
/// edited.
fn hue_tint(hue_deg: f64, radius_frac: f64) -> (f64, f64, f64) {
    let (cr, cg, cb) = centered_dir(hue_deg);
    let m = radius_frac.clamp(0.0, 1.0) * MAX_PUSH;
    (
        (0.5 + m * cr).clamp(0.0, 1.0),
        (0.5 + m * cg).clamp(0.0, 1.0),
        (0.5 + m * cb).clamp(0.0, 1.0),
    )
}

// ── Wheel position ↔ frei0r param mapping ────────────────────────────

/// Convert a wheel position to the frei0r color param for a zone whose
/// neutral param is `neutral`. `angle` is in radians (math convention:
/// 0 = right / red, CCW), `radius_frac` in 0..1, `master` in 0..1
/// (0.5 = no brightness change).
///
/// The hue offset is *subtracted* from the neutral because lowering a
/// channel's `three_point_balance` control value brightens that channel in
/// the output — so subtracting the hue direction makes the output gain that
/// hue.
pub fn wheel_to_param(
    angle: f64,
    radius_frac: f64,
    master: f64,
    neutral: (f64, f64, f64),
) -> (f64, f64, f64) {
    let hue_deg = angle.to_degrees().rem_euclid(360.0);
    let (cr, cg, cb) = centered_dir(hue_deg);
    let m = radius_frac.clamp(0.0, 1.0) * MAX_PUSH;
    let master_sub = (master.clamp(0.0, 1.0) - 0.5) * MASTER_RANGE;
    let (lo, hi) = safe_band(neutral);
    let map = |n: f64, c: f64| (n - m * c - master_sub).clamp(lo, hi);
    (map(neutral.0, cr), map(neutral.1, cg), map(neutral.2, cb))
}

/// Invert [`wheel_to_param`]: recover `(angle_rad, radius_frac, master)` from
/// a stored frei0r param so the puck can be positioned. Exact for the
/// symmetric midtones zone (`neutral = 0.5`); approximate for shadows /
/// highlights where the `[0,1]` clamp on one side discards information.
pub fn param_to_wheel(r: f64, g: f64, b: f64, neutral: (f64, f64, f64)) -> (f64, f64, f64) {
    // offset = neutral - param = hue_offset + master_sub (hue_offset is zero-sum).
    let off = (neutral.0 - r, neutral.1 - g, neutral.2 - b);
    let master_sub = (off.0 + off.1 + off.2) / 3.0;
    let master = (master_sub / MASTER_RANGE + 0.5).clamp(0.0, 1.0);
    // Remove the common (master) component to leave the zero-sum hue vector.
    let hvec = (
        off.0 - master_sub,
        off.1 - master_sub,
        off.2 - master_sub,
    );
    let hmag = (hvec.0 * hvec.0 + hvec.1 * hvec.1 + hvec.2 * hvec.2).sqrt();
    if hmag < 1e-6 {
        return (0.0, 0.0, master);
    }
    // Hue from the offset direction: probing 0.5 + hvec keeps channels
    // positive (hvec ∈ ±MAX_PUSH) and preserves channel ordering, so the
    // recovered hue matches the push direction.
    let (hue, _, _) = rgb_to_hsv(0.5 + hvec.0, 0.5 + hvec.1, 0.5 + hvec.2);
    let (cr, cg, cb) = centered_dir(hue);
    let cn = (cr * cr + cg * cg + cb * cb).sqrt().max(1e-6);
    let radius = (hmag / (MAX_PUSH * cn)).clamp(0.0, 1.0);
    (hue.to_radians(), radius, master)
}

// ── Drawing helpers ──────────────────────────────────────────────────

/// Render the color-balance disc: each pixel shows the output tint
/// ([`hue_tint`]) for its hue (angle) and saturation (radius), so the disc
/// is a soft, gray-centered field that matches the graded result.
///
/// Convention: hue 0° (red) at 3-o'clock, increasing CCW on screen — matches
/// `wheel_to_param` where hue = degrees(math_angle).
fn draw_wheel_background(cr: &gtk::cairo::Context, cx: f64, cy: f64, radius: f64) {
    let steps = (radius * 1.5).max(60.0) as i32;
    let angle_steps = 256;
    let wedge = TAU / angle_steps as f64;

    for ri in 0..steps {
        let frac = ri as f64 / steps as f64;
        let r_inner = radius * frac;
        let r_outer = radius * (frac + 1.0 / steps as f64);

        for ai in 0..angle_steps {
            // Iterate in Cairo angle space (0 = right, increasing = CW on screen).
            let ca = wedge * ai as f64;
            let ca_next = wedge * (ai as f64 + 1.2); // slightly wide to avoid gaps

            // Cairo Y-down means CW on screen = CCW in math.
            // Math angle = -cairo_angle, hue = degrees(math_angle).
            let hue_deg = (-ca.to_degrees()).rem_euclid(360.0);
            let (red, grn, blu) = hue_tint(hue_deg, frac);

            cr.set_source_rgb(red, grn, blu);
            cr.arc(cx, cy, r_outer, ca, ca_next);
            cr.arc_negative(cx, cy, r_inner, ca_next, ca);
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

/// Draw the indicator dot at the given wheel position. `fill` is the output
/// tint at the puck, ringed in white over a dark outline so it stays legible
/// over any disc hue.
fn draw_indicator(
    cr: &gtk::cairo::Context,
    cx: f64,
    cy: f64,
    radius: f64,
    angle: f64,
    radius_frac: f64,
    fill: (f64, f64, f64),
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

    // Indicator dot — the applied tint, ringed in white over a dark outline.
    cr.set_source_rgba(0.0, 0.0, 0.0, 0.7);
    cr.arc(ix, iy, 8.0, 0.0, TAU);
    let _ = cr.fill();
    cr.set_source_rgba(1.0, 1.0, 1.0, 0.95);
    cr.arc(ix, iy, 6.5, 0.0, TAU);
    let _ = cr.fill();
    cr.set_source_rgb(
        fill.0.clamp(0.0, 1.0),
        fill.1.clamp(0.0, 1.0),
        fill.2.clamp(0.0, 1.0),
    );
    cr.arc(ix, iy, 5.0, 0.0, TAU);
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
    /// Master brightness for the zone (0..1, 0.5 = neutral).
    master: f64,
}

/// Build a complete color wheel widget with a master-brightness slider.
///
/// Returns a `gtk::Box` containing the wheel DrawingArea and a slider
/// beneath it. `initial_rgb` is the stored frei0r param; `neutral` is the
/// zone's neutral param (`(0,0,0)` shadows, `(0.5,0.5,0.5)` midtones,
/// `(1,1,1)` highlights). `on_changed` fires whenever the user adjusts the
/// wheel or slider, passing the new frei0r `(r, g, b)` param.
///
/// `size` is the diameter of the wheel in pixels.
pub fn build_color_wheel(
    size: i32,
    initial_rgb: (f64, f64, f64),
    neutral: (f64, f64, f64),
    on_changed: impl Fn(f64, f64, f64) + 'static,
) -> (gtk::Box, Rc<dyn Fn(f64, f64, f64)>) {
    let (angle, radius_frac, master) =
        param_to_wheel(initial_rgb.0, initial_rgb.1, initial_rgb.2, neutral);

    let state = Rc::new(RefCell::new(WheelState {
        angle,
        radius_frac,
        master,
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
            draw_wheel_background(cr, cx, cy, radius);
            // Puck carries the same output tint the disc shows at that spot.
            let hue_deg = s.angle.to_degrees().rem_euclid(360.0);
            let fill = hue_tint(hue_deg, s.radius_frac);
            draw_indicator(cr, cx, cy, radius, s.angle, s.radius_frac, fill);
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

            let master;
            {
                let mut s = state.borrow_mut();
                s.angle = angle;
                s.radius_frac = frac;
                master = s.master;
            }

            let (r, g, b) = wheel_to_param(angle, frac, master, neutral);
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
            {
                let mut s = state.borrow_mut();
                s.angle = 0.0;
                s.radius_frac = 0.0;
                s.master = 0.5;
            }
            let (r, g, b) = wheel_to_param(0.0, 0.0, 0.5, neutral);
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

    // ── Master brightness slider ──
    let lum_slider = Scale::with_range(Orientation::Horizontal, 0.0, 1.0, 0.01);
    lum_slider.set_value(state.borrow().master);
    lum_slider.set_draw_value(false);
    lum_slider.set_hexpand(true);
    lum_slider.set_tooltip_text(Some("Master brightness for this tonal range"));
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
                st.master = v;
                angle = st.angle;
                frac = st.radius_frac;
            }
            let (r, g, b) = wheel_to_param(angle, frac, v, neutral);
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
            let (a, frac, master) = param_to_wheel(r, g, b, neutral);
            {
                let mut st = state.borrow_mut();
                st.angle = a;
                st.radius_frac = frac;
                st.master = master;
            }
            updating.set(true);
            lum_slider.set_value(master);
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

    const MID: (f64, f64, f64) = (0.5, 0.5, 0.5);

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
            assert!((v - v2).abs() < 0.001, "V mismatch: {v} vs {v2}");
            assert!((s - s2).abs() < 0.001, "S mismatch: {s} vs {s2}");
            if s > 0.01 {
                let dh = (h - h2).abs();
                let dh = dh.min(360.0 - dh);
                assert!(dh < 0.5, "H mismatch: {h} vs {h2}");
            }
        }
    }

    #[test]
    fn neutral_center_maps_to_neutral_param() {
        // Center + neutral master must reproduce the zone neutral exactly.
        for neutral in [(0.0, 0.0, 0.0), MID, (1.0, 1.0, 1.0)] {
            let p = wheel_to_param(0.0, 0.0, 0.5, neutral);
            assert!((p.0 - neutral.0).abs() < 1e-9);
            assert!((p.1 - neutral.1).abs() < 1e-9);
            assert!((p.2 - neutral.2).abs() < 1e-9);
        }
    }

    #[test]
    fn red_push_brightens_red_output() {
        // Pushing midtones toward red (angle 0) must LOWER the red param
        // (which brightens red in the three_point_balance output) and RAISE
        // green/blue — i.e. the video gets redder, matching the puck hue.
        let (r, g, b) = wheel_to_param(0.0, 1.0, 0.5, MID);
        assert!(r < 0.5, "red param should drop below neutral, got {r}");
        assert!(g > 0.5 && b > 0.5, "g/b should rise, got {g},{b}");
        // And the displayed tint at that spot is reddish (matches the video).
        let (tr, tg, tb) = hue_tint(0.0, 1.0);
        assert!(tr > tg && tr > tb, "tint should read red: {tr},{tg},{tb}");
    }

    #[test]
    fn distinct_hues_give_distinct_params() {
        // The old HSV mapping collapsed many positions to similar colors.
        // Verify red / green / blue rim pushes are all clearly different.
        let red = wheel_to_param(0.0_f64.to_radians(), 1.0, 0.5, MID);
        let grn = wheel_to_param(120.0_f64.to_radians(), 1.0, 0.5, MID);
        let blu = wheel_to_param(240.0_f64.to_radians(), 1.0, 0.5, MID);
        let dist = |a: (f64, f64, f64), b: (f64, f64, f64)| {
            ((a.0 - b.0).powi(2) + (a.1 - b.1).powi(2) + (a.2 - b.2).powi(2)).sqrt()
        };
        assert!(dist(red, grn) > 0.3, "red vs green too close: {red:?} {grn:?}");
        assert!(dist(red, blu) > 0.3, "red vs blue too close: {red:?} {blu:?}");
        assert!(dist(grn, blu) > 0.3, "green vs blue too close: {grn:?} {blu:?}");
    }

    #[test]
    fn wheel_param_roundtrip_midtones() {
        // Midtones are symmetric (no clamp for moderate pushes), so the
        // forward/inverse mapping must round-trip closely.
        let cases = [
            (0.0_f64, 0.0_f64, 0.5_f64),     // center
            (30.0, 0.6, 0.5),                // orange-ish push
            (210.0, 0.5, 0.5),               // azure push
            (120.0, 0.4, 0.6),               // green push + brighter
            (300.0, 0.55, 0.45),             // magenta push + darker
        ];
        for (hue_deg, radius, master) in cases {
            let angle = hue_deg.to_radians();
            let (r, g, b) = wheel_to_param(angle, radius, master, MID);
            let (a2, rad2, m2) = param_to_wheel(r, g, b, MID);
            assert!((rad2 - radius).abs() < 0.05, "radius {radius} -> {rad2}");
            assert!((m2 - master).abs() < 0.02, "master {master} -> {m2}");
            if radius > 0.01 {
                let h2 = a2.to_degrees().rem_euclid(360.0);
                let dh = (h2 - hue_deg).abs();
                let dh = dh.min(360.0 - dh);
                assert!(dh < 2.0, "hue {hue_deg} -> {h2}");
            }
        }
    }

    #[test]
    fn master_slider_never_flips_hue() {
        // Regression: sweeping the master slider must never make the puck's
        // hue channel stop being the brightest output. For frei0r
        // three_point_balance the LOWEST gray-color channel is the brightest
        // output, so the puck's dominant channel must stay the minimum param
        // at every master setting (it may tie at the band edges → washes to
        // neutral, but must never become the maximum → that was the
        // blue→yellow flip).
        for hue_deg in [0.0_f64, 60.0, 120.0, 210.0, 240.0, 300.0] {
            let angle = hue_deg.to_radians();
            // Channel the hue pushes brightest = max centered component.
            let (cr, cg, cb) = centered_dir(hue_deg);
            let dom = if cr >= cg && cr >= cb {
                0
            } else if cg >= cr && cg >= cb {
                1
            } else {
                2
            };
            for step in 0..=20 {
                let master = step as f64 / 20.0;
                let p = wheel_to_param(angle, 0.9, master, MID);
                let arr = [p.0, p.1, p.2];
                let dom_val = arr[dom];
                assert!(
                    arr.iter().all(|&v| dom_val <= v + 1e-9),
                    "hue {hue_deg}° master {master}: dominant channel {dom} \
                     ({dom_val}) is not the lowest param {arr:?} — hue flipped"
                );
            }
        }
    }

    #[test]
    fn hue_tint_center_is_gray() {
        let (r, g, b) = hue_tint(123.0, 0.0);
        assert!((r - 0.5).abs() < 1e-9 && (g - 0.5).abs() < 1e-9 && (b - 0.5).abs() < 1e-9);
    }

    #[test]
    fn drawing_and_interaction_hues_match() {
        // Drawing uses hue = -degrees(cairo_angle); interaction uses
        // hue = degrees(math_angle). They must agree at each screen position.
        let positions: &[(&str, f64, f64, f64)] = &[
            ("right", 0.0, 0.0, 0.0),
            ("up", 3.0 * PI / 2.0, PI / 2.0, 90.0),
            ("left", PI, PI, 180.0),
            ("down", PI / 2.0, -PI / 2.0, 270.0),
            ("1 o'clock", 5.0 * PI / 3.0, PI / 3.0, 60.0),
        ];
        for &(label, cairo_a, math_a, expected_hue) in positions {
            let draw_hue = (-cairo_a.to_degrees()).rem_euclid(360.0);
            let interact_hue = math_a.to_degrees().rem_euclid(360.0);
            let dh1 = (draw_hue - expected_hue).abs();
            let dh1 = dh1.min(360.0 - dh1);
            assert!(dh1 < 0.1, "{label}: draw_hue={draw_hue}");
            let dh2 = (interact_hue - expected_hue).abs();
            let dh2 = dh2.min(360.0 - dh2);
            assert!(dh2 < 0.1, "{label}: interact_hue={interact_hue}");
        }
    }
}
