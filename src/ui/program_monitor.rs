use gtk4::prelude::*;
use gtk4::{self as gtk, AspectFrame, Box as GBox, Button, DrawingArea, EventControllerScroll,
           EventControllerScrollFlags, Label, Orientation, Overlay, Picture, ScrolledWindow};
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use crate::media::program_player::ProgramPlayer;

/// Transform parameters for a clip (crop, rotation, flip).
/// Kept here so other modules can reference it without a separate file.
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

/// Build the program monitor widget.
/// Returns `(widget, pos_label, speed_label, picture_a, picture_b, vu_meter, peak_cell, canvas_frame)`.
/// `picture_a` displays the primary (outgoing) clip; `picture_b` displays the incoming
/// transition clip. The caller controls cross-dissolve by setting widget opacity on
/// each picture each poll tick via `Widget::set_opacity()`.
/// `peak_cell` is updated by the caller with `[left_db, right_db]` each poll tick;
/// `vu_meter.queue_draw()` triggers a repaint.
/// `speed_label` shows the current J/K/L shuttle rate ("◀◀ 2×", "▶▶ 4×") or is hidden.
/// `canvas_frame` is an `AspectFrame` locked to the canvas ratio — update its ratio
/// via `canvas_frame.set_ratio(w as f32 / h as f32)` when project settings change.
pub fn build_program_monitor(
    program_player: Rc<RefCell<ProgramPlayer>>,
    paintable_a: gdk4::Paintable,
    paintable_b: gdk4::Paintable,
    canvas_width: u32,
    canvas_height: u32,
    on_stop: impl Fn() + 'static,
    on_play_pause: impl Fn() + 'static,
    on_toggle_popout: impl Fn() + 'static,
    transform_overlay_da: Option<DrawingArea>,
) -> (GBox, Label, Label, Picture, Picture, DrawingArea, Rc<Cell<[f64; 2]>>, AspectFrame) {
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

    // J/K/L shuttle rate indicator — shown/hidden by window.rs.
    let speed_label = Label::new(None);
    speed_label.add_css_class("timecode");
    speed_label.set_visible(false);
    title_bar.append(&speed_label);

    let pos_label = Label::new(Some("00:00:00;00"));
    pos_label.add_css_class("timecode");
    title_bar.append(&pos_label);

    let btn_popout = Button::with_label("Pop Out / Dock");
    btn_popout.connect_clicked(move |_| on_toggle_popout());
    title_bar.append(&btn_popout);

    // Zoom controls: − / zoom% label / + / Fit
    // These are appended to the title bar AFTER we build apply_zoom (below), so we
    // defer the connections and store the label in a variable.
    let zoom_out_btn = Button::with_label("−");
    zoom_out_btn.set_tooltip_text(Some("Zoom out preview (Ctrl+Scroll)"));
    let zoom_label = Label::new(Some("100%"));
    zoom_label.set_width_chars(5);
    zoom_label.add_css_class("timecode");
    let zoom_in_btn = Button::with_label("+");
    zoom_in_btn.set_tooltip_text(Some("Zoom in preview (Ctrl+Scroll)"));
    let zoom_fit_btn = Button::with_label("Fit");
    zoom_fit_btn.set_tooltip_text(Some("Reset zoom to fit"));
    title_bar.append(&zoom_out_btn);
    title_bar.append(&zoom_label);
    title_bar.append(&zoom_in_btn);
    title_bar.append(&zoom_fit_btn);

    root.append(&title_bar);

    // Video display: GtkOverlay composites picture_b as the bottom layer and
    // picture_a as the top layer. Opacities are updated each poll tick.
    let picture_a = Picture::new();
    picture_a.set_paintable(Some(&paintable_a));
    picture_a.set_hexpand(true);
    picture_a.set_vexpand(true);
    picture_a.set_halign(gtk::Align::Fill);
    picture_a.set_valign(gtk::Align::Fill);
    picture_a.set_can_shrink(true);
    picture_a.set_size_request(1, 1);
    picture_a.set_content_fit(gtk::ContentFit::Contain);
    picture_a.add_css_class("preview-video");

    let picture_b = Picture::new();
    picture_b.set_paintable(Some(&paintable_b));
    picture_b.set_hexpand(true);
    picture_b.set_vexpand(true);
    picture_b.set_halign(gtk::Align::Fill);
    picture_b.set_valign(gtk::Align::Fill);
    picture_b.set_can_shrink(true);
    picture_b.set_size_request(1, 1);
    picture_b.set_content_fit(gtk::ContentFit::Contain);
    picture_b.add_css_class("preview-video");
    picture_b.set_opacity(0.0); // hidden until a lower layer/transition is active

    // Inner overlay: composites picture_b (bottom) and picture_a (top).
    // The transform DA is NOT added here — it lives on the outer overlay so it can
    // draw handles outside the canvas boundary (e.g. when scale > 1.0).
    let overlay = Overlay::new();
    let overlay_base = GBox::new(Orientation::Vertical, 0);
    overlay_base.set_hexpand(true);
    overlay_base.set_vexpand(true);
    overlay_base.set_size_request(1, 1);
    overlay.set_child(Some(&overlay_base));
    overlay.add_overlay(&picture_b);
    overlay.add_overlay(&picture_a);
    overlay.set_measure_overlay(&picture_b, false);
    overlay.set_measure_overlay(&picture_a, false);
    overlay.set_hexpand(true);
    overlay.set_vexpand(true);

    // AspectFrame constrains the video display area to the canvas ratio (e.g. 16:9).
    // This makes ContentFit::Contain on picture_a/b letterbox/pillarbox clips whose
    // native aspect ratio differs from the canvas (e.g. a 21:9 clip on a 16:9 canvas).
    let canvas_ratio = if canvas_height > 0 {
        canvas_width as f32 / canvas_height as f32
    } else {
        16.0 / 9.0
    };
    let canvas_frame = AspectFrame::new(0.5, 0.5, canvas_ratio, false);
    canvas_frame.set_child(Some(&overlay));
    canvas_frame.set_hexpand(true);
    canvas_frame.set_vexpand(true);

    // Outer overlay: canvas_frame (primary child) + transform DA (overlay child).
    // The outer overlay fills the scroll viewport, giving the transform DA the full
    // viewport area to draw in. This means handles drawn outside the canvas boundary
    // (e.g. bounding box corners when scale > 1.0) are visible when the user zooms
    // out the program monitor. The canvas position computed by video_rect() in the
    // transform overlay matches canvas_frame's layout exactly, because AspectFrame
    // centres its child using the same letterbox geometry as video_rect().
    let outer_overlay = Overlay::new();
    outer_overlay.set_child(Some(&canvas_frame));
    outer_overlay.set_hexpand(true);
    outer_overlay.set_vexpand(true);
    // Keep a clone of the transform DA so apply_zoom can queue_draw() after each zoom.
    let transform_da_for_zoom: Option<DrawingArea> = transform_overlay_da.as_ref().cloned();
    if let Some(da) = transform_overlay_da {
        da.set_hexpand(true);
        da.set_vexpand(true);
        da.set_halign(gtk::Align::Fill);
        da.set_valign(gtk::Align::Fill);
        outer_overlay.add_overlay(&da);
    }

    let zoom_level: Rc<Cell<f64>> = Rc::new(Cell::new(1.0));
    // Natural (fit) size of the canvas_frame recorded the moment we first leave zoom=1.0.
    // At zoom=1.0 with hexpand=true the canvas_frame fills the scroll viewport exactly,
    // so canvas_frame.width()/height() at that instant is the correct "100%" baseline.
    let fit_w: Rc<Cell<i32>> = Rc::new(Cell::new(0));
    let fit_h: Rc<Cell<i32>> = Rc::new(Cell::new(0));

    let scroll = ScrolledWindow::new();
    scroll.set_policy(gtk::PolicyType::Automatic, gtk::PolicyType::Automatic);
    scroll.set_child(Some(&outer_overlay));
    scroll.set_hexpand(true);
    scroll.set_vexpand(true);

    // apply_zoom: when leaving zoom=1.0, records the natural canvas_frame size as baseline.
    // At non-1.0 zoom, disables hexpand/vexpand so the frame can grow beyond viewport.
    let zoom_levels: &[f64] = &[0.25, 0.5, 0.75, 1.0, 1.5, 2.0, 3.0, 4.0];
    let apply_zoom = {
        let canvas_frame       = canvas_frame.clone();
        let scroll             = scroll.clone();
        let zoom_level         = zoom_level.clone();
        let fit_w              = fit_w.clone();
        let fit_h              = fit_h.clone();
        let transform_da_zoom  = transform_da_for_zoom.clone();
        move |new_z: f64| {
            let z = zoom_levels.iter()
                .cloned()
                .fold(f64::INFINITY, |best, z| {
                    if (z - new_z).abs() < (best - new_z).abs() { z } else { best }
                })
                .clamp(0.25, 4.0);

            // When transitioning away from 1.0, snapshot the natural canvas_frame size.
            if (zoom_level.get() - 1.0).abs() < 0.01 && (z - 1.0).abs() > 0.01 {
                let mut fw = canvas_frame.width();
                let mut fh = canvas_frame.height();
                if fw <= 0 || fh <= 0 {
                    fw = scroll.width();
                    fh = scroll.height();
                }
                if fw <= 0 || fh <= 0 {
                    fw = canvas_width.max(1) as i32;
                    fh = canvas_height.max(1) as i32;
                }
                fit_w.set(fw);
                fit_h.set(fh);
            }
            zoom_level.set(z);

            if (z - 1.0).abs() < 0.01 {
                // Fit: let the canvas_frame expand naturally to fill the scroll viewport.
                canvas_frame.set_hexpand(true);
                canvas_frame.set_vexpand(true);
                canvas_frame.set_halign(gtk::Align::Fill);
                canvas_frame.set_valign(gtk::Align::Fill);
                canvas_frame.set_size_request(-1, -1);
            } else {
                let fw = fit_w.get();
                let fh = fit_h.get();
                if fw > 0 && fh > 0 {
                    canvas_frame.set_hexpand(false);
                    canvas_frame.set_vexpand(false);
                    canvas_frame.set_size_request((fw as f64 * z) as i32, (fh as f64 * z) as i32);
                    if z < 1.0 {
                        // Center the smaller canvas in the scroll viewport (no scrollbars).
                        canvas_frame.set_halign(gtk::Align::Center);
                        canvas_frame.set_valign(gtk::Align::Center);
                    } else {
                        // Allow content to overflow so ScrolledWindow can scroll.
                        canvas_frame.set_halign(gtk::Align::Fill);
                        canvas_frame.set_valign(gtk::Align::Fill);
                    }
                }
            }
            // Redraw the transform overlay so the canvas border/vignette
            // repositions immediately after any zoom change.
            if let Some(ref da) = transform_da_zoom {
                da.queue_draw();
            }
        }
    };
    let apply_zoom = Rc::new(apply_zoom);

    // Ctrl+Scroll adjusts zoom
    {
        let az = apply_zoom.clone();
        let zoom_level = zoom_level.clone();
        let lbl = zoom_label.clone();
        let ctrl_scroll = EventControllerScroll::new(
            EventControllerScrollFlags::VERTICAL | EventControllerScrollFlags::DISCRETE,
        );
        ctrl_scroll.connect_scroll(move |ec, _dx, dy| {
            let mods = ec.current_event_state();
            if mods.contains(gdk4::ModifierType::CONTROL_MASK) {
                let step = if dy < 0.0 { 1_isize } else { -1_isize };
                let z = zoom_level.get();
                let idx = zoom_levels.iter().position(|&l| (l - z).abs() < 0.01).unwrap_or(3);
                let new_idx = (idx as isize + step).clamp(0, zoom_levels.len() as isize - 1) as usize;
                let new_z = zoom_levels[new_idx];
                az(new_z);
                lbl.set_label(&format!("{}%", (new_z * 100.0) as u32));
                return gtk4::glib::Propagation::Stop;
            }
            gtk4::glib::Propagation::Proceed
        });
        scroll.add_controller(ctrl_scroll);
    }

    root.append(&scroll);

    // Wire zoom buttons now that apply_zoom is defined
    {
        let az = apply_zoom.clone();
        let zl = zoom_level.clone();
        let lbl = zoom_label.clone();
        let zoom_levels_v = vec![0.25_f64, 0.5, 0.75, 1.0, 1.5, 2.0, 3.0, 4.0];
        zoom_out_btn.connect_clicked(move |_| {
            let z = zl.get();
            let idx = zoom_levels_v.iter().position(|&l| (l - z).abs() < 0.01).unwrap_or(3);
            let new_idx = idx.saturating_sub(1);
            let new_z = zoom_levels_v[new_idx];
            az(new_z);
            lbl.set_label(&format!("{}%", (new_z * 100.0) as u32));
        });
    }
    {
        let az = apply_zoom.clone();
        let zl = zoom_level.clone();
        let lbl = zoom_label.clone();
        let zoom_levels_v = vec![0.25_f64, 0.5, 0.75, 1.0, 1.5, 2.0, 3.0, 4.0];
        zoom_in_btn.connect_clicked(move |_| {
            let z = zl.get();
            let idx = zoom_levels_v.iter().position(|&l| (l - z).abs() < 0.01).unwrap_or(3);
            let new_idx = (idx + 1).min(zoom_levels_v.len() - 1);
            let new_z = zoom_levels_v[new_idx];
            az(new_z);
            lbl.set_label(&format!("{}%", (new_z * 100.0) as u32));
        });
    }
    {
        let az = apply_zoom.clone();
        let lbl = zoom_label.clone();
        zoom_fit_btn.connect_clicked(move |_| {
            az(1.0);
            lbl.set_label("100%");
        });
    }
    // Update zoom label from Ctrl+Scroll via zoom_level cell poll in size-allocate
    // is not needed — buttons update it directly. For Ctrl+Scroll we update in the
    // scroll handler above by re-reading zoom_level in the label callbacks below.


    // Transport controls
    let controls = GBox::new(Orientation::Horizontal, 8);
    controls.add_css_class("transport-bar");
    controls.set_halign(gtk::Align::Center);
    controls.set_margin_top(6);
    controls.set_margin_bottom(6);

    let btn_play = Button::with_label("▶ Play");
    btn_play.connect_clicked(move |_| on_play_pause());
    controls.append(&btn_play);

    let btn_stop = Button::with_label("■ Stop");
    btn_stop.connect_clicked(move |_| on_stop());
    controls.append(&btn_stop);

    root.append(&controls);

    // VU meter: two vertical bars (L/R) showing audio peak level in dBFS.
    let (vu_meter, peak_cell) = build_vu_meter();
    let vu_bar = GBox::new(Orientation::Horizontal, 0);
    vu_bar.set_halign(gtk::Align::End);
    vu_bar.set_valign(gtk::Align::Fill);
    vu_bar.set_margin_end(4);
    vu_bar.append(&vu_meter);

    // Place VU meter at the end of the title bar (right-aligned).
    title_bar.append(&vu_bar);

    (root, pos_label, speed_label, picture_a, picture_b, vu_meter, peak_cell, canvas_frame)
}

/// Build a VU meter DrawingArea showing L/R audio peak levels in dBFS.
/// Returns `(drawing_area, peak_cell)` where the caller writes `[left_db, right_db]`
/// into `peak_cell` and calls `drawing_area.queue_draw()` each poll tick.
///
/// Scale: 0 dBFS at top, -60 dBFS at bottom.
/// Zones: green (< -18 dBFS), yellow (-18 to -6 dBFS), red (> -6 dBFS).
pub fn build_vu_meter() -> (DrawingArea, Rc<Cell<[f64; 2]>>) {
    let peak_cell: Rc<Cell<[f64; 2]>> = Rc::new(Cell::new([-60.0, -60.0]));
    let da = DrawingArea::new();
    da.set_content_width(36);
    da.set_content_height(80);
    da.set_valign(gtk::Align::Center);

    let pc = peak_cell.clone();
    da.set_draw_func(move |_da, cr, width, height| {
        let [left_db, right_db] = pc.get();
        let bar_w = (width as f64 / 2.0 - 2.0).max(4.0);
        // dBFS → fraction of bar height (0.0 = bottom, 1.0 = top).
        let db_to_frac = |db: f64| -> f64 { ((db + 60.0) / 60.0).clamp(0.0, 1.0) };
        // Draw background.
        cr.set_source_rgb(0.13, 0.13, 0.13);
        cr.rectangle(0.0, 0.0, width as f64, height as f64);
        let _ = cr.fill();

        for (ch, db) in [(0, left_db), (1, right_db)] {
            let x = ch as f64 * (bar_w + 2.0) + 1.0;
            let frac = db_to_frac(db);
            let bar_h = frac * height as f64;
            let y_top = height as f64 - bar_h;

            // Green zone: below -18 dBFS
            let green_frac = db_to_frac(-18.0);
            let green_h = (green_frac * height as f64).min(bar_h);
            if green_h > 0.0 {
                cr.set_source_rgb(0.2, 0.8, 0.2);
                cr.rectangle(x, height as f64 - green_h, bar_w, green_h);
                let _ = cr.fill();
            }

            // Yellow zone: -18 to -6 dBFS
            let yellow_frac = db_to_frac(-6.0);
            let yellow_top = green_frac * height as f64;
            let yellow_h = ((yellow_frac - green_frac) * height as f64).min((bar_h - green_h).max(0.0));
            if yellow_h > 0.0 {
                cr.set_source_rgb(0.9, 0.85, 0.1);
                cr.rectangle(x, height as f64 - yellow_top - yellow_h, bar_w, yellow_h);
                let _ = cr.fill();
            }

            // Red zone: above -6 dBFS
            let red_top = yellow_frac * height as f64;
            let red_h = (bar_h - red_top).max(0.0);
            if red_h > 0.0 {
                cr.set_source_rgb(0.9, 0.2, 0.1);
                cr.rectangle(x, y_top, bar_w, red_h);
                let _ = cr.fill();
            }

            // Zone boundary markers (thin dark lines at -18 and -6 dBFS).
            cr.set_source_rgb(0.05, 0.05, 0.05);
            cr.set_line_width(1.0);
            for marker_db in [-18.0_f64, -6.0_f64] {
                let my = height as f64 - db_to_frac(marker_db) * height as f64;
                cr.move_to(x, my);
                cr.line_to(x + bar_w, my);
                let _ = cr.stroke();
            }
            let _ = ch; // suppress unused warning
        }
    });

    (da, peak_cell)
}

pub fn format_timecode(ns: u64) -> String {
    let total_frames = ns / (1_000_000_000 / 30);
    let frames = total_frames % 30;
    let secs   = ns / 1_000_000_000;
    let s      = secs % 60;
    let m      = (secs / 60) % 60;
    let h      = secs / 3600;
    format!("{h:02}:{m:02}:{s:02};{frames:02}")
}
