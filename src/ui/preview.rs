use gtk4::prelude::*;
use gtk4::{self as gtk, Box as GBox, Button, DrawingArea, EventControllerKey, GestureClick, Label, Orientation, Picture};
use glib;
use std::cell::RefCell;
use std::rc::Rc;
use crate::media::player::{Player, PlayerState};
use crate::model::media_library::SourceMarks;

const NS_PER_SECOND: f64 = 1_000_000_000.0;

/// Builds the source-preview panel: video display + in/out scrubber + transport.
///
/// Returns `(widget, source_marks)` — callers read `source_marks` to get the
/// current in/out selection when appending to the timeline.
pub fn build_preview(
    player: Rc<RefCell<Player>>,
    paintable: gdk4::Paintable,
) -> (GBox, Rc<RefCell<SourceMarks>>) {
    let source_marks = Rc::new(RefCell::new(SourceMarks::default()));

    let vbox = GBox::new(Orientation::Vertical, 0);
    vbox.set_hexpand(true);
    vbox.set_vexpand(true);
    vbox.set_focusable(true);

    // Video display
    let picture = Picture::new();
    picture.set_paintable(Some(&paintable));
    picture.set_vexpand(true);
    picture.set_hexpand(true);
    picture.set_content_fit(gtk::ContentFit::Contain);
    picture.add_css_class("preview-picture");
    vbox.append(&picture);

    // ── Source scrubber ───────────────────────────────────────────────────
    let scrubber = DrawingArea::new();
    scrubber.set_content_height(20);
    scrubber.set_hexpand(true);
    scrubber.set_margin_start(8);
    scrubber.set_margin_end(8);
    scrubber.set_margin_top(4);
    {
        let player = player.clone();
        let source_marks = source_marks.clone();
        scrubber.set_draw_func(move |_area, cr, width, _height| {
            draw_scrubber(cr, width as f64, &player.borrow(), &source_marks.borrow());
        });
    }
    // Click on scrubber → seek
    {
        let scrubber_click = GestureClick::new();
        let player = player.clone();
        let source_marks = source_marks.clone();
        let scrubber_weak = scrubber.downgrade();
        scrubber_click.connect_pressed(move |_, _, x, _| {
            let dur = source_marks.borrow().duration_ns;
            if dur == 0 { return; }
            let w = scrubber_weak.upgrade().map(|a| a.width()).unwrap_or(1) as f64;
            let frac = (x / w).clamp(0.0, 1.0);
            let pos = (frac * dur as f64) as u64;
            let _ = player.borrow().seek(pos);
            if let Some(a) = scrubber_weak.upgrade() { a.queue_draw(); }
        });
        scrubber.add_controller(scrubber_click);
    }
    vbox.append(&scrubber);

    // ── In/out timecode row ───────────────────────────────────────────────
    let inout_row = GBox::new(Orientation::Horizontal, 8);
    inout_row.set_halign(gtk::Align::Center);
    inout_row.set_margin_top(2);

    let in_label  = Label::new(Some("In:  0:00"));
    let out_label = Label::new(Some("Out: 0:00"));
    in_label.add_css_class("clip-path");
    out_label.add_css_class("clip-path");
    inout_row.append(&in_label);
    inout_row.append(&Label::new(Some("│")));
    inout_row.append(&out_label);
    vbox.append(&inout_row);

    // ── Position / duration timecode ──────────────────────────────────────
    let timecode_label = Label::new(Some("0:00:00 / 0:00:00"));
    timecode_label.set_margin_top(2);
    vbox.append(&timecode_label);

    // ── Transport bar ─────────────────────────────────────────────────────
    let controls = GBox::new(Orientation::Horizontal, 6);
    controls.set_halign(gtk::Align::Center);
    controls.set_margin_top(4);
    controls.set_margin_bottom(4);

    let btn_set_in  = Button::with_label("Set In (I)");
    let btn_set_out = Button::with_label("Set Out (O)");
    let btn_stop       = Button::with_label("⏹");
    let btn_play_pause = Button::with_label("▶");

    controls.append(&btn_set_in);
    controls.append(&btn_stop);
    controls.append(&btn_play_pause);
    controls.append(&btn_set_out);
    vbox.append(&controls);

    // Set In
    {
        let player = player.clone();
        let source_marks = source_marks.clone();
        let in_label = in_label.clone();
        let scrubber_weak = scrubber.downgrade();
        btn_set_in.connect_clicked(move |_| {
            let pos = player.borrow().position();
            let mut m = source_marks.borrow_mut();
            m.in_ns = pos.min(m.out_ns.saturating_sub(1_000_000));
            in_label.set_text(&format!("In:  {}", ns_to_timecode(m.in_ns)));
            drop(m);
            if let Some(a) = scrubber_weak.upgrade() { a.queue_draw(); }
        });
    }

    // Set Out
    {
        let player = player.clone();
        let source_marks = source_marks.clone();
        let out_label = out_label.clone();
        let scrubber_weak = scrubber.downgrade();
        btn_set_out.connect_clicked(move |_| {
            let pos = player.borrow().position();
            let mut m = source_marks.borrow_mut();
            m.out_ns = pos.max(m.in_ns + 1_000_000);
            out_label.set_text(&format!("Out: {}", ns_to_timecode(m.out_ns)));
            drop(m);
            if let Some(a) = scrubber_weak.upgrade() { a.queue_draw(); }
        });
    }

    // Play/Pause toggle
    {
        let player = player.clone();
        let btn = btn_play_pause.clone();
        btn_play_pause.connect_clicked(move |_| {
            let p = player.borrow();
            match p.state() {
                PlayerState::Playing => { let _ = p.pause(); btn.set_label("▶"); }
                _ => { let _ = p.play(); btn.set_label("⏸"); }
            }
        });
    }

    // Stop
    {
        let player = player.clone();
        let btn = btn_play_pause.clone();
        btn_stop.connect_clicked(move |_| {
            let p = player.borrow();
            let _ = p.stop();
            btn.set_label("▶");
        });
    }

    // ── Keyboard shortcuts (I/O for in/out marks) ─────────────────────────
    {
        let key_ctrl = EventControllerKey::new();
        let player = player.clone();
        let source_marks = source_marks.clone();
        let in_label = in_label.clone();
        let out_label = out_label.clone();
        let scrubber_weak = scrubber.downgrade();
        let btn_play_pause = btn_play_pause.clone();

        key_ctrl.connect_key_pressed(move |_, key, _, _| {
            use gtk::gdk::Key;
            match key {
                Key::i | Key::I => {
                    let pos = player.borrow().position();
                    let mut m = source_marks.borrow_mut();
                    m.in_ns = pos.min(m.out_ns.saturating_sub(1_000_000));
                    in_label.set_text(&format!("In:  {}", ns_to_timecode(m.in_ns)));
                    drop(m);
                    if let Some(a) = scrubber_weak.upgrade() { a.queue_draw(); }
                    glib::Propagation::Stop
                }
                Key::o | Key::O => {
                    let pos = player.borrow().position();
                    let mut m = source_marks.borrow_mut();
                    m.out_ns = pos.max(m.in_ns + 1_000_000);
                    out_label.set_text(&format!("Out: {}", ns_to_timecode(m.out_ns)));
                    drop(m);
                    if let Some(a) = scrubber_weak.upgrade() { a.queue_draw(); }
                    glib::Propagation::Stop
                }
                Key::space => {
                    let p = player.borrow();
                    match p.state() {
                        PlayerState::Playing => { let _ = p.pause(); btn_play_pause.set_label("▶"); }
                        _ => { let _ = p.play(); btn_play_pause.set_label("⏸"); }
                    }
                    glib::Propagation::Stop
                }
                _ => glib::Propagation::Proceed,
            }
        });
        vbox.add_controller(key_ctrl);
    }

    // ── Update scrubber + timecode every 100ms ────────────────────────────
    {
        let player = player.clone();
        let label = timecode_label.clone();
        let scrubber_weak = scrubber.downgrade();
        let btn = btn_play_pause.clone();
        glib::timeout_add_local(std::time::Duration::from_millis(100), move || {
            let p = player.borrow();
            let pos = p.position();
            let dur = p.duration();
            // Sync play button label with actual state
            match p.state() {
                PlayerState::Playing => btn.set_label("⏸"),
                _ => btn.set_label("▶"),
            }
            label.set_text(&format!("{} / {}", ns_to_timecode(pos), ns_to_timecode(dur)));
            drop(p);
            if let Some(a) = scrubber_weak.upgrade() { a.queue_draw(); }
            glib::ControlFlow::Continue
        });
    }

    (vbox, source_marks)
}

fn draw_scrubber(
    cr: &gtk::cairo::Context,
    width: f64,
    player: &Player,
    marks: &SourceMarks,
) {
    let height = 20.0;
    let dur = marks.duration_ns;

    // Background
    cr.set_source_rgb(0.15, 0.15, 0.17);
    cr.rectangle(0.0, 0.0, width, height);
    cr.fill().ok();

    if dur == 0 { return; }

    // In/out selection band
    let in_x  = (marks.in_ns  as f64 / dur as f64) * width;
    let out_x = (marks.out_ns as f64 / dur as f64) * width;
    cr.set_source_rgba(0.17, 0.47, 0.85, 0.45);
    cr.rectangle(in_x, 0.0, out_x - in_x, height);
    cr.fill().ok();

    // In marker (green line)
    cr.set_source_rgb(0.2, 0.9, 0.3);
    cr.set_line_width(2.0);
    cr.move_to(in_x, 0.0);
    cr.line_to(in_x, height);
    cr.stroke().ok();

    // Out marker (orange line)
    cr.set_source_rgb(1.0, 0.6, 0.1);
    cr.set_line_width(2.0);
    cr.move_to(out_x, 0.0);
    cr.line_to(out_x, height);
    cr.stroke().ok();

    // Playhead (red)
    let pos = player.position();
    let ph_x = (pos as f64 / dur as f64) * width;
    cr.set_source_rgb(1.0, 0.3, 0.3);
    cr.set_line_width(2.0);
    cr.move_to(ph_x, 0.0);
    cr.line_to(ph_x, height);
    cr.stroke().ok();
}

fn ns_to_timecode(ns: u64) -> String {
    let total_secs = (ns as f64 / NS_PER_SECOND) as u64;
    let h = total_secs / 3600;
    let m = (total_secs % 3600) / 60;
    let s = total_secs % 60;
    if h > 0 {
        format!("{h}:{m:02}:{s:02}")
    } else {
        format!("{m}:{s:02}")
    }
}
