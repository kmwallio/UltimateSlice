use gtk4::prelude::*;
use gtk4::{self as gtk, Box as GBox, Button, DrawingArea, EventControllerKey, GestureDrag, Label, Orientation, Picture, Separator};
use glib;
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use crate::media::player::{Player, PlayerState};
use crate::model::media_library::SourceMarks;

const NS_PER_SECOND: f64 = 1_000_000_000.0;
/// Default frame duration at 24 fps (nanoseconds)
const DEFAULT_FRAME_NS: u64 = 41_666_667;

/// Builds the source-preview panel: video display + in/out scrubber + transport.
///
/// Returns `(widget, source_marks)` — callers read `source_marks` to get the
/// current in/out selection when appending to the timeline.
/// Returns `(widget, source_marks, clip_name_label)`.
pub fn build_preview(
    player: Rc<RefCell<Player>>,
    paintable: gdk4::Paintable,
    on_append: Rc<dyn Fn()>,
) -> (GBox, Rc<RefCell<SourceMarks>>, Label) {
    let source_marks = Rc::new(RefCell::new(SourceMarks::default()));

    let vbox = GBox::new(Orientation::Vertical, 0);
    vbox.set_hexpand(true);
    vbox.set_vexpand(true);
    vbox.set_focusable(true);

    // Clip name header
    let clip_name_label = Label::new(Some("No source loaded"));
    clip_name_label.set_halign(gtk::Align::Start);
    clip_name_label.set_margin_start(8);
    clip_name_label.set_margin_top(4);
    clip_name_label.set_margin_bottom(2);
    clip_name_label.add_css_class("clip-name");
    vbox.append(&clip_name_label);

    // Video display
    let picture = Picture::new();
    picture.set_paintable(Some(&paintable));
    picture.set_vexpand(true);
    picture.set_hexpand(true);
    picture.set_content_fit(gtk::ContentFit::Contain);
    picture.add_css_class("preview-picture");
    vbox.append(&picture);

    // DragSource on the video display so users can drag the current
    // clip selection (in/out range) directly to the timeline.
    {
        let source_marks = source_marks.clone();
        let drag_src = gtk::DragSource::new();
        drag_src.set_actions(gdk4::DragAction::COPY);
        drag_src.connect_prepare({
            let source_marks = source_marks.clone();
            move |_src, _x, _y| {
                let marks = source_marks.borrow();
                if marks.path.is_empty() || marks.duration_ns == 0 {
                    return None;
                }
                let payload = format!("{}|{}", marks.path, marks.duration_ns);
                Some(gdk4::ContentProvider::for_value(&glib::Value::from(&payload)))
            }
        });
        picture.add_controller(drag_src);
    }

    // ── Source scrubber ───────────────────────────────────────────────────
    let scrubber = DrawingArea::new();
    scrubber.set_content_height(24);
    scrubber.set_hexpand(true);
    scrubber.set_margin_start(8);
    scrubber.set_margin_end(8);
    scrubber.set_margin_top(4);

    // Share the actual drawn width so click/drag handlers use the same value.
    let drawn_width: Rc<Cell<f64>> = Rc::new(Cell::new(1.0));

    {
        let source_marks = source_marks.clone();
        let drawn_width = drawn_width.clone();
        scrubber.set_draw_func(move |_area, cr, width, _height| {
            let w = width as f64;
            drawn_width.set(w.max(1.0));
            draw_scrubber(cr, w, &source_marks.borrow());
        });
    }

    // Helper: convert a raw widget-local x to a seek position in nanoseconds.
    let seek_from_x = {
        let source_marks = source_marks.clone();
        let drawn_width = drawn_width.clone();
        let player = player.clone();
        let scrubber_weak = scrubber.downgrade();
        Rc::new(move |x: f64| {
            let dur = source_marks.borrow().duration_ns;
            if dur == 0 { return; }
            // Use the drawn width recorded in draw_func; fall back to the
            // widget's allocated width if draw_func hasn't fired yet.
            let w = {
                let dw = drawn_width.get();
                if dw > 1.0 {
                    dw
                } else {
                    scrubber_weak.upgrade()
                        .map(|s| s.width() as f64)
                        .unwrap_or(1.0)
                        .max(1.0)
                }
            };
            let frac = (x / w).clamp(0.0, 1.0);
            let pos = (frac * dur as f64) as u64;
            // Record the desired position immediately so the scrubber draws
            // the correct playhead even while GStreamer is still pre-rolling.
            source_marks.borrow_mut().display_pos_ns = pos;
            let _ = player.borrow().seek(pos);
            if let Some(a) = scrubber_weak.upgrade() { a.queue_draw(); }
        })
    };

    // ── Gesture: click OR drag → seek OR drag in/out marker ──────────────
    // DragMode: 0 = seek playhead, 1 = drag In marker, 2 = drag Out marker.
    // Determined in drag_begin by hit-testing the pointer position against the
    // in/out marker positions (within ±8 px of each marker line).
    let drag_mode: Rc<Cell<u8>> = Rc::new(Cell::new(0));
    {
        let scrubber_drag = GestureDrag::new();

        // drag_begin: decide what this gesture controls
        scrubber_drag.connect_drag_begin({
            let seek_from_x = seek_from_x.clone();
            let source_marks = source_marks.clone();
            let drawn_width = drawn_width.clone();
            let drag_mode = drag_mode.clone();
            let scrubber_weak = scrubber.downgrade();
            move |_, x, _| {
                let marks = source_marks.borrow();
                let dur = marks.duration_ns;
                if dur == 0 {
                    drag_mode.set(0);
                    drop(marks);
                    seek_from_x(x);
                    return;
                }
                let w = drawn_width.get().max(1.0);
                let in_x  = (marks.in_ns  as f64 / dur as f64) * w;
                let out_x = (marks.out_ns as f64 / dur as f64) * w;
                drop(marks);
                const HIT: f64 = 8.0;
                if (x - in_x).abs() <= HIT {
                    drag_mode.set(1);
                    if let Some(a) = scrubber_weak.upgrade() { a.queue_draw(); }
                } else if (x - out_x).abs() <= HIT {
                    drag_mode.set(2);
                    if let Some(a) = scrubber_weak.upgrade() { a.queue_draw(); }
                } else {
                    drag_mode.set(0);
                    seek_from_x(x);
                }
            }
        });

        // drag_update: apply seek or marker update
        scrubber_drag.connect_drag_update({
            let seek_from_x = seek_from_x.clone();
            let source_marks = source_marks.clone();
            let drawn_width = drawn_width.clone();
            let drag_mode = drag_mode.clone();
            let scrubber_weak = scrubber.downgrade();
            move |gesture, offset_x, _| {
                let (start_x, _) = gesture.start_point().unwrap_or((0.0, 0.0));
                let x = start_x + offset_x;
                match drag_mode.get() {
                    0 => seek_from_x(x),
                    mode => {
                        let w = drawn_width.get().max(1.0);
                        let frac = (x / w).clamp(0.0, 1.0);
                        let mut marks = source_marks.borrow_mut();
                        let dur = marks.duration_ns;
                        if dur == 0 { return; }
                        let pos_ns = (frac * dur as f64) as u64;
                        if mode == 1 {
                            marks.in_ns = pos_ns.min(marks.out_ns.saturating_sub(1_000_000));
                        } else {
                            marks.out_ns = pos_ns.max(marks.in_ns + 1_000_000);
                        }
                        drop(marks);
                        if let Some(a) = scrubber_weak.upgrade() { a.queue_draw(); }
                    }
                }
            }
        });

        scrubber.add_controller(scrubber_drag);
    }

    vbox.append(&scrubber);

    // ── Dedicated Mark In/Out bar ────────────────────────────────────────
    let marks_bar = GBox::new(Orientation::Horizontal, 12);
    marks_bar.add_css_class("marks-bar");
    marks_bar.set_halign(gtk::Align::Fill);
    marks_bar.set_margin_start(8);
    marks_bar.set_margin_end(8);
    marks_bar.set_margin_top(4);

    let in_label  = Label::new(Some("In  00:00:00:00"));
    let out_label = Label::new(Some("Out 00:00:00:00"));
    let dur_label = Label::new(Some("Dur 00:00:00:00"));
    in_label.add_css_class("marks-timecode");
    in_label.add_css_class("marks-in");
    out_label.add_css_class("marks-timecode");
    out_label.add_css_class("marks-out");
    dur_label.add_css_class("marks-timecode");
    dur_label.add_css_class("marks-dur");

    let sep1 = Separator::new(Orientation::Vertical);
    let sep2 = Separator::new(Orientation::Vertical);

    marks_bar.append(&in_label);
    marks_bar.append(&sep1);
    marks_bar.append(&out_label);
    marks_bar.append(&sep2);
    marks_bar.append(&dur_label);
    vbox.append(&marks_bar);

    // ── Position / duration timecode ──────────────────────────────────────
    let timecode_label = Label::new(Some("0:00:00:00 / 0:00:00:00"));
    timecode_label.add_css_class("timecode");
    timecode_label.set_margin_top(2);
    vbox.append(&timecode_label);

    // ── Transport bar ─────────────────────────────────────────────────────
    let controls = GBox::new(Orientation::Horizontal, 4);
    controls.set_halign(gtk::Align::Center);
    controls.set_margin_top(4);
    controls.set_margin_bottom(4);

    let btn_set_in     = Button::with_label("Set In (I)");
    let btn_set_out    = Button::with_label("Set Out (O)");
    let btn_prev_frame = Button::with_label("◀▮");
    let btn_stop       = Button::with_label("⏹");
    let btn_play_pause = Button::with_label("▶");
    let btn_next_frame = Button::with_label("▮▶");
    let btn_append     = Button::with_label("⬇ Append");
    btn_prev_frame.set_tooltip_text(Some("Step back one frame (←)"));
    btn_next_frame.set_tooltip_text(Some("Step forward one frame (→)"));
    btn_append.set_tooltip_text(Some("Append selection to timeline"));
    btn_append.set_sensitive(false); // enabled once a source is loaded

    controls.append(&btn_set_in);
    controls.append(&btn_prev_frame);
    controls.append(&btn_stop);
    controls.append(&btn_play_pause);
    controls.append(&btn_next_frame);
    controls.append(&btn_set_out);
    controls.append(&btn_append);
    vbox.append(&controls);

    // Shuttle speed state for J/K/L: negative = reverse, 0 = paused, positive = forward.
    // Values: -3, -2, -1, 0, 1, 2, 3 (corresponding to 1x, 2x, 4x speeds).
    let shuttle_speed: Rc<Cell<i32>> = Rc::new(Cell::new(0));
    // Local cache of frame duration; synced from source_marks.frame_ns in the 100ms timer.
    let frame_ns: Rc<Cell<u64>> = Rc::new(Cell::new(DEFAULT_FRAME_NS));

    // Helper: update the marks bar labels from current source_marks state.
    let update_marks_bar = {
        let in_label = in_label.clone();
        let out_label = out_label.clone();
        let dur_label = dur_label.clone();
        let frame_ns = frame_ns.clone();
        Rc::new(move |marks: &SourceMarks| {
            let fns = frame_ns.get();
            in_label.set_text(&format!("In  {}", ns_to_timecode_frames(marks.in_ns, fns)));
            out_label.set_text(&format!("Out {}", ns_to_timecode_frames(marks.out_ns, fns)));
            let dur = marks.out_ns.saturating_sub(marks.in_ns);
            dur_label.set_text(&format!("Dur {}", ns_to_timecode_frames(dur, fns)));
        })
    };

    // Set In
    {
        let source_marks = source_marks.clone();
        let update_marks_bar = update_marks_bar.clone();
        let scrubber_weak = scrubber.downgrade();
        btn_set_in.connect_clicked(move |_| {
            let mut m = source_marks.borrow_mut();
            let pos = m.display_pos_ns;
            m.in_ns = pos.min(m.out_ns.saturating_sub(1_000_000));
            update_marks_bar(&m);
            drop(m);
            if let Some(a) = scrubber_weak.upgrade() { a.queue_draw(); }
        });
    }

    // Set Out
    {
        let source_marks = source_marks.clone();
        let update_marks_bar = update_marks_bar.clone();
        let scrubber_weak = scrubber.downgrade();
        btn_set_out.connect_clicked(move |_| {
            let mut m = source_marks.borrow_mut();
            let pos = m.display_pos_ns;
            m.out_ns = pos.max(m.in_ns + 1_000_000);
            update_marks_bar(&m);
            drop(m);
            if let Some(a) = scrubber_weak.upgrade() { a.queue_draw(); }
        });
    }

    // Append
    {
        btn_append.connect_clicked(move |_| {
            on_append();
        });
    }

    // Play/Pause toggle
    {
        let player = player.clone();
        let btn = btn_play_pause.clone();
        let shuttle_speed = shuttle_speed.clone();
        btn_play_pause.connect_clicked(move |_| {
            shuttle_speed.set(0);
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
        let shuttle_speed = shuttle_speed.clone();
        btn_stop.connect_clicked(move |_| {
            shuttle_speed.set(0);
            let p = player.borrow();
            let _ = p.stop();
            btn.set_label("▶");
        });
    }

    // Step backward one frame
    {
        let player = player.clone();
        let source_marks = source_marks.clone();
        let frame_ns = frame_ns.clone();
        let scrubber_weak = scrubber.downgrade();
        btn_prev_frame.connect_clicked(move |_| {
            let p = player.borrow();
            let _ = p.pause();
            if let Ok(new_pos) = p.step_backward(frame_ns.get()) {
                source_marks.borrow_mut().display_pos_ns = new_pos;
            }
            drop(p);
            if let Some(a) = scrubber_weak.upgrade() { a.queue_draw(); }
        });
    }

    // Step forward one frame
    {
        let player = player.clone();
        let source_marks = source_marks.clone();
        let frame_ns = frame_ns.clone();
        let scrubber_weak = scrubber.downgrade();
        btn_next_frame.connect_clicked(move |_| {
            let p = player.borrow();
            let _ = p.pause();
            if let Ok(new_pos) = p.step_forward(frame_ns.get()) {
                source_marks.borrow_mut().display_pos_ns = new_pos;
            }
            drop(p);
            if let Some(a) = scrubber_weak.upgrade() { a.queue_draw(); }
        });
    }

    // ── Keyboard shortcuts (I/O, Space, J/K/L, ←/→) ─────────────────────
    {
        let key_ctrl = EventControllerKey::new();
        let player = player.clone();
        let source_marks = source_marks.clone();
        let update_marks_bar = update_marks_bar.clone();
        let scrubber_weak = scrubber.downgrade();
        let btn_play_pause = btn_play_pause.clone();
        let shuttle_speed = shuttle_speed.clone();
        let frame_ns = frame_ns.clone();

        key_ctrl.connect_key_pressed(move |_, key, _, _| {
            use gtk::gdk::Key;
            match key {
                Key::i | Key::I => {
                    let mut m = source_marks.borrow_mut();
                    let pos = m.display_pos_ns;
                    m.in_ns = pos.min(m.out_ns.saturating_sub(1_000_000));
                    update_marks_bar(&m);
                    drop(m);
                    if let Some(a) = scrubber_weak.upgrade() { a.queue_draw(); }
                    glib::Propagation::Stop
                }
                Key::o | Key::O => {
                    let mut m = source_marks.borrow_mut();
                    let pos = m.display_pos_ns;
                    m.out_ns = pos.max(m.in_ns + 1_000_000);
                    update_marks_bar(&m);
                    drop(m);
                    if let Some(a) = scrubber_weak.upgrade() { a.queue_draw(); }
                    glib::Propagation::Stop
                }
                Key::space => {
                    shuttle_speed.set(0);
                    let p = player.borrow();
                    match p.state() {
                        PlayerState::Playing => { let _ = p.pause(); btn_play_pause.set_label("▶"); }
                        _ => { let _ = p.play(); btn_play_pause.set_label("⏸"); }
                    }
                    glib::Propagation::Stop
                }
                // J — shuttle reverse (increasing speed: -1x, -2x, -4x)
                Key::j | Key::J => {
                    let cur = shuttle_speed.get();
                    let new_speed = if cur > 0 { -1 } else { (cur - 1).max(-3) };
                    shuttle_speed.set(new_speed);
                    // Shuttle is implemented as frame stepping in the 100ms timer
                    let _ = player.borrow().pause();
                    btn_play_pause.set_label("◀◀");
                    glib::Propagation::Stop
                }
                // K — stop shuttle / pause
                Key::k | Key::K => {
                    shuttle_speed.set(0);
                    let _ = player.borrow().pause();
                    btn_play_pause.set_label("▶");
                    glib::Propagation::Stop
                }
                // L — shuttle forward (increasing speed: 1x, 2x, 4x)
                Key::l | Key::L => {
                    let cur = shuttle_speed.get();
                    let new_speed = if cur < 0 { 1 } else { (cur + 1).min(3) };
                    shuttle_speed.set(new_speed);
                    if new_speed == 1 {
                        let _ = player.borrow().play();
                        btn_play_pause.set_label("⏸");
                    } else {
                        // Faster speeds are implemented via frame stepping in the timer
                        let _ = player.borrow().pause();
                        btn_play_pause.set_label("▶▶");
                    }
                    glib::Propagation::Stop
                }
                // ← — step backward one frame
                Key::Left => {
                    shuttle_speed.set(0);
                    let p = player.borrow();
                    let _ = p.pause();
                    if let Ok(new_pos) = p.step_backward(frame_ns.get()) {
                        source_marks.borrow_mut().display_pos_ns = new_pos;
                    }
                    drop(p);
                    btn_play_pause.set_label("▶");
                    if let Some(a) = scrubber_weak.upgrade() { a.queue_draw(); }
                    glib::Propagation::Stop
                }
                // → — step forward one frame
                Key::Right => {
                    shuttle_speed.set(0);
                    let p = player.borrow();
                    let _ = p.pause();
                    if let Ok(new_pos) = p.step_forward(frame_ns.get()) {
                        source_marks.borrow_mut().display_pos_ns = new_pos;
                    }
                    drop(p);
                    btn_play_pause.set_label("▶");
                    if let Some(a) = scrubber_weak.upgrade() { a.queue_draw(); }
                    glib::Propagation::Stop
                }
                _ => glib::Propagation::Proceed,
            }
        });
        vbox.add_controller(key_ctrl);
    }

    // ── Update scrubber + timecode every 100ms; handle shuttle stepping ──
    {
        let player = player.clone();
        let label = timecode_label.clone();
        let scrubber_weak = scrubber.downgrade();
        let source_marks = source_marks.clone();
        let btn = btn_play_pause.clone();
        let shuttle_speed = shuttle_speed.clone();
        let frame_ns = frame_ns.clone();
        let update_marks_bar = update_marks_bar.clone();
        let btn_append = btn_append.clone();
        glib::timeout_add_local(std::time::Duration::from_millis(100), move || {
            let p = player.borrow();
            let pos = p.position();
            let dur = p.duration();

            // Sync local frame_ns cache from source_marks
            frame_ns.set(source_marks.borrow().frame_ns);

            // Handle shuttle speeds != 0 and != 1 via frame stepping
            let spd = shuttle_speed.get();
            if spd != 0 && spd != 1 {
                // Frames to step per 100ms tick at each shuttle level
                const SHUTTLE_2X_FRAMES: u64 = 2;
                const SHUTTLE_4X_FRAMES: u64 = 4;
                let step_frames: u64 = match spd.unsigned_abs() {
                    2 => SHUTTLE_2X_FRAMES,
                    3 => SHUTTLE_4X_FRAMES,
                    _ => 1,
                };
                let step_ns = step_frames * frame_ns.get();
                if spd > 0 {
                    let _ = p.step_forward(step_ns);
                } else {
                    let _ = p.step_backward(step_ns);
                }
            }

            // Sync play button label with actual state
            if spd == 0 {
                match p.state() {
                    PlayerState::Playing => btn.set_label("⏸"),
                    _ => btn.set_label("▶"),
                }
            }
            // Update display_pos_ns from the real player position once
            // GStreamer finishes pre-rolling after a seek (pos goes non-zero).
            // We skip pos==0 to avoid snapping the scrubber to the start
            // during the flush/pre-roll window that follows a seek.
            if pos > 0 {
                source_marks.borrow_mut().display_pos_ns = pos;
            }
            let fns = frame_ns.get();
            label.set_text(&format!(
                "{} / {}",
                ns_to_timecode_frames(pos, fns),
                ns_to_timecode_frames(dur, fns),
            ));
            // Keep marks bar in sync (out label resets when source is loaded)
            {
                let m = source_marks.borrow();
                // Enable append once a source is loaded
                btn_append.set_sensitive(!m.path.is_empty());
                update_marks_bar(&m);
            }
            drop(p);
            if let Some(a) = scrubber_weak.upgrade() { a.queue_draw(); }
            glib::ControlFlow::Continue
        });
    }

    (vbox, source_marks, clip_name_label)
}

fn draw_scrubber(
    cr: &gtk::cairo::Context,
    width: f64,
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

    // In marker (green line + downward triangle handle at top)
    cr.set_source_rgb(0.2, 0.9, 0.3);
    cr.set_line_width(2.0);
    cr.move_to(in_x, 0.0);
    cr.line_to(in_x, height);
    cr.stroke().ok();
    // Triangle handle (pointing down from top edge)
    cr.move_to(in_x - 5.0, 0.0);
    cr.line_to(in_x + 5.0, 0.0);
    cr.line_to(in_x, 8.0);
    cr.close_path();
    cr.fill().ok();

    // Out marker (orange line + downward triangle handle at top)
    cr.set_source_rgb(1.0, 0.6, 0.1);
    cr.set_line_width(2.0);
    cr.move_to(out_x, 0.0);
    cr.line_to(out_x, height);
    cr.stroke().ok();
    // Triangle handle (pointing down from top edge)
    cr.move_to(out_x - 5.0, 0.0);
    cr.line_to(out_x + 5.0, 0.0);
    cr.line_to(out_x, 8.0);
    cr.close_path();
    cr.fill().ok();

    // Playhead — use display_pos_ns (set immediately on seek) rather than
    // player.position(), which returns 0 while GStreamer is pre-rolling.
    let pos = marks.display_pos_ns;
    let ph_x = (pos as f64 / dur as f64) * width;
    cr.set_source_rgb(1.0, 0.3, 0.3);
    cr.set_line_width(2.0);
    cr.move_to(ph_x, 0.0);
    cr.line_to(ph_x, height);
    cr.stroke().ok();
}

/// Frame-accurate timecode: `H:MM:SS:FF` (always shows hours for consistency).
fn ns_to_timecode_frames(ns: u64, frame_ns: u64) -> String {
    let frame_ns = frame_ns.max(1); // guard against division by zero
    let total_frames = ns / frame_ns;
    // Derive fps from frame duration; clamp to at least 1 in case of rounding edge cases
    let fps = (NS_PER_SECOND / frame_ns as f64).round().max(1.0) as u64;
    let ff = total_frames % fps;
    let total_secs = total_frames / fps;
    let h = total_secs / 3600;
    let m = (total_secs % 3600) / 60;
    let s = total_secs % 60;
    format!("{h}:{m:02}:{s:02}:{ff:02}")
}
