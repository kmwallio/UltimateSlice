use gtk4::prelude::*;
use gtk4::{self as gtk, Box as GBox, Button, DrawingArea, Label, Orientation, Overlay, Picture};
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
/// Returns `(widget, pos_label, speed_label, picture_a, picture_b, vu_meter, peak_cell)`.
/// `picture_a` displays the primary (outgoing) clip; `picture_b` displays the incoming
/// transition clip. The caller controls cross-dissolve by setting widget opacity on
/// each picture each poll tick via `Widget::set_opacity()`.
/// `peak_cell` is updated by the caller with `[left_db, right_db]` each poll tick;
/// `vu_meter.queue_draw()` triggers a repaint.
/// `speed_label` shows the current J/K/L shuttle rate ("◀◀ 2×", "▶▶ 4×") or is hidden.
pub fn build_program_monitor(
    program_player: Rc<RefCell<ProgramPlayer>>,
    paintable_a: gdk4::Paintable,
    paintable_b: gdk4::Paintable,
    on_stop: impl Fn() + 'static,
    on_play_pause: impl Fn() + 'static,
    on_toggle_popout: impl Fn() + 'static,
    transform_overlay_da: Option<DrawingArea>,
) -> (GBox, Label, Label, Picture, Picture, DrawingArea, Rc<Cell<[f64; 2]>>) {
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

    root.append(&title_bar);

    // Video display: GtkOverlay composites picture_a (primary) with picture_b
    // (incoming transition clip) on top. Opacities are updated each poll tick to
    // create a true cross-dissolve (complementary alpha blend).
    let picture_a = Picture::new();
    picture_a.set_paintable(Some(&paintable_a));
    picture_a.set_hexpand(true);
    picture_a.set_vexpand(true);
    picture_a.set_content_fit(gtk::ContentFit::Contain);
    picture_a.add_css_class("preview-video");

    let picture_b = Picture::new();
    picture_b.set_paintable(Some(&paintable_b));
    picture_b.set_hexpand(true);
    picture_b.set_vexpand(true);
    picture_b.set_content_fit(gtk::ContentFit::Contain);
    picture_b.set_opacity(0.0); // hidden until a transition is active

    let overlay = Overlay::new();
    overlay.set_child(Some(&picture_a));
    overlay.add_overlay(&picture_b);
    if let Some(da) = transform_overlay_da {
        da.set_hexpand(true);
        da.set_vexpand(true);
        overlay.add_overlay(&da);
    }
    overlay.set_hexpand(true);
    overlay.set_vexpand(true);

    root.append(&overlay);

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

    (root, pos_label, speed_label, picture_a, picture_b, vu_meter, peak_cell)
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
