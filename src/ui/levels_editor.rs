/// Levels editor widget for the frei0r "levels" effect.
///
/// Renders a transfer function visualization with input/output level sliders
/// and gamma adjustment. Used by the inspector to replace raw numeric sliders
/// with an intuitive levels editor.
use gtk4::prelude::*;
use gtk4::{self as gtk, DrawingArea, Label, Orientation, Scale};
use std::cell::RefCell;
use std::rc::Rc;

const VIS_MARGIN: f64 = 8.0;

struct LevelsState {
    channel: usize,
    input_black: f64,
    input_white: f64,
    gamma: f64, // display gamma 0.1–4.0 (1.0 = neutral)
    output_black: f64,
    output_white: f64,
}

const CHANNEL_LABELS: &[&str] = &["Red", "Green", "Blue", "Luma"];

/// Map dropdown index to frei0r `channel` param value.
fn channel_idx_to_frei0r(idx: usize) -> f64 {
    match idx {
        0 => 0.0,
        1 => 0.1,
        2 => 0.2,
        _ => 0.3, // Luma
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
    } else {
        3 // Luma
    }
}

/// Convert frei0r gamma (0–1, where 0.25 = neutral gamma 1.0) to display gamma.
fn frei0r_gamma_to_display(val: f64) -> f64 {
    (val * 4.0).clamp(0.1, 4.0)
}

/// Convert display gamma (0.1–4.0) back to frei0r gamma (0–1).
fn display_gamma_to_frei0r(val: f64) -> f64 {
    (val / 4.0).clamp(0.0, 1.0)
}

/// Compute the levels transfer function value at input `x` (0..1).
fn transfer(x: f64, state: &LevelsState) -> f64 {
    let ib = state.input_black;
    let iw = state.input_white;
    let gamma = state.gamma;
    let ob = state.output_black;
    let ow = state.output_white;

    if iw <= ib {
        return ob;
    }
    if x <= ib {
        return ob;
    }
    if x >= iw {
        return ow;
    }

    let normalized = (x - ib) / (iw - ib);
    let curved = if gamma > 0.001 {
        normalized.powf(1.0 / gamma)
    } else {
        0.0
    };
    ob + curved * (ow - ob)
}

/// Draw the transfer function curve with grid and input range markers.
fn draw_levels(ctx: &gtk::cairo::Context, w: f64, h: f64, state: &LevelsState) {
    let dw = w - 2.0 * VIS_MARGIN;
    let dh = h - 2.0 * VIS_MARGIN;

    // Dark background
    ctx.set_source_rgb(0.1, 0.1, 0.11);
    ctx.rectangle(0.0, 0.0, w, h);
    let _ = ctx.fill();

    // Grid at 25% intervals
    ctx.set_source_rgba(0.3, 0.3, 0.32, 0.4);
    ctx.set_line_width(0.5);
    for i in 0..=4 {
        let f = i as f64 / 4.0;
        let x = VIS_MARGIN + f * dw;
        let y = VIS_MARGIN + f * dh;
        ctx.move_to(x, VIS_MARGIN);
        ctx.line_to(x, VIS_MARGIN + dh);
        ctx.move_to(VIS_MARGIN, y);
        ctx.line_to(VIS_MARGIN + dw, y);
    }
    let _ = ctx.stroke();

    // Input range markers (vertical lines for black and white levels)
    ctx.set_source_rgba(0.6, 0.6, 0.6, 0.5);
    ctx.set_line_width(1.0);
    let bx = VIS_MARGIN + state.input_black * dw;
    ctx.move_to(bx, VIS_MARGIN);
    ctx.line_to(bx, VIS_MARGIN + dh);
    let wx = VIS_MARGIN + state.input_white * dw;
    ctx.move_to(wx, VIS_MARGIN);
    ctx.line_to(wx, VIS_MARGIN + dh);
    let _ = ctx.stroke();

    // Transfer function curve
    let channel_col = match state.channel {
        0 => (1.0, 0.3, 0.3),
        1 => (0.3, 1.0, 0.3),
        2 => (0.3, 0.5, 1.0),
        _ => (0.9, 0.9, 0.9),
    };
    ctx.set_source_rgb(channel_col.0, channel_col.1, channel_col.2);
    ctx.set_line_width(2.0);

    let steps = 120;
    for step in 0..=steps {
        let x_norm = step as f64 / steps as f64;
        let y_norm = transfer(x_norm, state);
        let sx = VIS_MARGIN + x_norm * dw;
        let sy = VIS_MARGIN + (1.0 - y_norm) * dh;
        if step == 0 {
            ctx.move_to(sx, sy);
        } else {
            ctx.line_to(sx, sy);
        }
    }
    let _ = ctx.stroke();
}

/// Build a levels editor widget for the frei0r levels effect.
///
/// * `channel_frei0r` — initial frei0r `channel` param (0.0/0.1/0.2/0.3).
/// * `input_black`, `input_white` — input range (0–1).
/// * `gamma_frei0r` — frei0r gamma (0–1, 0.25 = neutral).
/// * `output_black`, `output_white` — output range (0–1).
/// * `on_changed` — fired with `(channel, input_black, input_white, gamma, output_black, output_white)` as frei0r values.
pub fn build_levels_widget(
    channel_frei0r: f64,
    input_black: f64,
    input_white: f64,
    gamma_frei0r: f64,
    output_black: f64,
    output_white: f64,
    on_changed: impl Fn(f64, f64, f64, f64, f64, f64) + 'static,
) -> gtk::Box {
    let ch_idx = frei0r_to_channel_idx(channel_frei0r);
    let display_gamma = frei0r_gamma_to_display(gamma_frei0r);

    let state = Rc::new(RefCell::new(LevelsState {
        channel: ch_idx,
        input_black,
        input_white: if input_white <= input_black {
            1.0
        } else {
            input_white
        },
        gamma: display_gamma,
        output_black,
        output_white: if output_white <= output_black {
            1.0
        } else {
            output_white
        },
    }));
    let on_changed = Rc::new(on_changed);

    // ── Channel selector ──
    let ch_list = gtk::StringList::new(CHANNEL_LABELS);
    let ch_dd = gtk::DropDown::new(Some(ch_list), gtk4::Expression::NONE);
    ch_dd.set_selected(ch_idx as u32);
    ch_dd.set_margin_start(4);
    ch_dd.set_margin_end(4);

    // ── Transfer function visualization ──
    let da = DrawingArea::new();
    da.set_content_width(240);
    da.set_content_height(80);
    da.set_halign(gtk::Align::Center);

    {
        let st = state.clone();
        da.set_draw_func(move |_da, ctx, ww, wh| {
            draw_levels(ctx, ww as f64, wh as f64, &st.borrow());
        });
    }

    // ── Fire change helper ──
    let fire = {
        let state = state.clone();
        let on_changed = on_changed.clone();
        let da = da.clone();
        Rc::new(move || {
            let s = state.borrow();
            on_changed(
                channel_idx_to_frei0r(s.channel),
                s.input_black,
                s.input_white,
                display_gamma_to_frei0r(s.gamma),
                s.output_black,
                s.output_white,
            );
            da.queue_draw();
        })
    };

    // ── Sliders ──
    let sliders_box = gtk::Box::new(Orientation::Vertical, 2);

    // Input Black
    {
        let row = gtk::Box::new(Orientation::Horizontal, 4);
        row.set_margin_start(4);
        let label = Label::new(Some("Input Black"));
        label.add_css_class("dim-label");
        label.set_halign(gtk::Align::Start);
        label.set_width_chars(12);
        row.append(&label);
        let slider = Scale::with_range(Orientation::Horizontal, 0.0, 1.0, 0.01);
        slider.set_value(input_black);
        slider.set_draw_value(true);
        slider.set_digits(2);
        slider.set_hexpand(true);
        row.append(&slider);
        let state = state.clone();
        let fire = fire.clone();
        slider.connect_value_changed(move |s| {
            state.borrow_mut().input_black = s.value();
            fire();
        });
        sliders_box.append(&row);
    }

    // Input White
    {
        let row = gtk::Box::new(Orientation::Horizontal, 4);
        row.set_margin_start(4);
        let label = Label::new(Some("Input White"));
        label.add_css_class("dim-label");
        label.set_halign(gtk::Align::Start);
        label.set_width_chars(12);
        row.append(&label);
        let slider = Scale::with_range(Orientation::Horizontal, 0.0, 1.0, 0.01);
        slider.set_value(state.borrow().input_white);
        slider.set_draw_value(true);
        slider.set_digits(2);
        slider.set_hexpand(true);
        row.append(&slider);
        let state = state.clone();
        let fire = fire.clone();
        slider.connect_value_changed(move |s| {
            state.borrow_mut().input_white = s.value();
            fire();
        });
        sliders_box.append(&row);
    }

    // Gamma
    {
        let row = gtk::Box::new(Orientation::Horizontal, 4);
        row.set_margin_start(4);
        let label = Label::new(Some("Gamma"));
        label.add_css_class("dim-label");
        label.set_halign(gtk::Align::Start);
        label.set_width_chars(12);
        row.append(&label);
        let slider = Scale::with_range(Orientation::Horizontal, 0.1, 4.0, 0.01);
        slider.set_value(display_gamma);
        slider.set_draw_value(true);
        slider.set_digits(2);
        slider.set_hexpand(true);
        slider.add_mark(1.0, gtk::PositionType::Bottom, None);
        row.append(&slider);
        let state = state.clone();
        let fire = fire.clone();
        slider.connect_value_changed(move |s| {
            state.borrow_mut().gamma = s.value();
            fire();
        });
        sliders_box.append(&row);
    }

    // Separator between input and output
    let sep = gtk::Separator::new(Orientation::Horizontal);
    sep.set_margin_top(4);
    sep.set_margin_bottom(4);
    sliders_box.append(&sep);

    // Output Black
    {
        let row = gtk::Box::new(Orientation::Horizontal, 4);
        row.set_margin_start(4);
        let label = Label::new(Some("Output Black"));
        label.add_css_class("dim-label");
        label.set_halign(gtk::Align::Start);
        label.set_width_chars(12);
        row.append(&label);
        let slider = Scale::with_range(Orientation::Horizontal, 0.0, 1.0, 0.01);
        slider.set_value(output_black);
        slider.set_draw_value(true);
        slider.set_digits(2);
        slider.set_hexpand(true);
        row.append(&slider);
        let state = state.clone();
        let fire = fire.clone();
        slider.connect_value_changed(move |s| {
            state.borrow_mut().output_black = s.value();
            fire();
        });
        sliders_box.append(&row);
    }

    // Output White
    {
        let row = gtk::Box::new(Orientation::Horizontal, 4);
        row.set_margin_start(4);
        let label = Label::new(Some("Output White"));
        label.add_css_class("dim-label");
        label.set_halign(gtk::Align::Start);
        label.set_width_chars(12);
        row.append(&label);
        let slider = Scale::with_range(Orientation::Horizontal, 0.0, 1.0, 0.01);
        slider.set_value(state.borrow().output_white);
        slider.set_draw_value(true);
        slider.set_digits(2);
        slider.set_hexpand(true);
        row.append(&slider);
        let state = state.clone();
        let fire = fire.clone();
        slider.connect_value_changed(move |s| {
            state.borrow_mut().output_white = s.value();
            fire();
        });
        sliders_box.append(&row);
    }

    // ── Channel dropdown change ──
    {
        let state = state.clone();
        let fire = fire.clone();
        ch_dd.connect_selected_notify(move |dd| {
            state.borrow_mut().channel = dd.selected() as usize;
            fire();
        });
    }

    // ── Layout ──
    let vbox = gtk::Box::new(Orientation::Vertical, 4);
    vbox.append(&ch_dd);
    vbox.append(&da);
    vbox.append(&sliders_box);
    vbox
}
