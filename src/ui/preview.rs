use crate::media::player::{Player, PlayerState};
use crate::model::media_library::SourceMarks;
use glib;
use gtk4::prelude::*;
use gtk4::{
    self as gtk, Box as GBox, Button, DrawingArea, EventControllerKey, GestureDrag, Label,
    Orientation, Picture, Popover, Separator, Stack,
};
use std::cell::{Cell, RefCell};
use std::rc::Rc;

use crate::units::NS_PER_SECOND_F as NS_PER_SECOND;
/// Default frame duration at 24 fps (nanoseconds)
const DEFAULT_FRAME_NS: u64 = 41_666_667;

/// Which "add to timeline" action the split button currently performs.
#[derive(Clone, Copy, PartialEq)]
enum AddMode {
    Append,
    Insert,
    Overwrite,
}

/// Returns `(widget, source_marks, clip_name_label, set_audio_only)`.
/// `set_audio_only(true)` shows the audio-only banner in place of the video display.
pub fn build_preview(
    player: Rc<RefCell<Player>>,
    paintable: gdk4::Paintable,
    on_append: Rc<dyn Fn()>,
    on_insert: Rc<dyn Fn()>,
    on_overwrite: Rc<dyn Fn()>,
    on_close_preview: Rc<dyn Fn()>,
) -> (GBox, Rc<RefCell<SourceMarks>>, Label, Rc<dyn Fn(bool)>) {
    let source_marks = Rc::new(RefCell::new(SourceMarks::default()));

    let vbox = GBox::new(Orientation::Vertical, 0);
    vbox.set_hexpand(true);
    vbox.set_vexpand(true);
    vbox.set_focusable(true);

    // Clip name header + close button
    let header_row = GBox::new(Orientation::Horizontal, 4);
    header_row.set_margin_start(8);
    header_row.set_margin_end(8);
    header_row.set_margin_top(4);
    header_row.set_margin_bottom(2);

    let clip_name_label = Label::new(Some("No source loaded"));
    clip_name_label.set_halign(gtk::Align::Start);
    clip_name_label.set_hexpand(true);
    clip_name_label.set_xalign(0.0);
    clip_name_label.add_css_class("clip-name");
    header_row.append(&clip_name_label);

    let btn_close_preview = Button::with_label("✕");
    btn_close_preview.set_tooltip_text(Some("Close source preview"));
    btn_close_preview.add_css_class("flat");
    btn_close_preview.set_sensitive(false);
    {
        let on_close_preview = on_close_preview.clone();
        btn_close_preview.connect_clicked(move |_| {
            on_close_preview();
        });
    }
    header_row.append(&btn_close_preview);
    vbox.append(&header_row);

    // Video display
    let picture = Picture::new();
    picture.set_paintable(Some(&paintable));
    picture.set_vexpand(true);
    picture.set_hexpand(true);
    picture.set_content_fit(gtk::ContentFit::Contain);
    picture.add_css_class("preview-picture");

    // Audio-only banner page: shown when selected clip has no video stream.
    let audio_banner = GBox::new(Orientation::Vertical, 8);
    audio_banner.set_vexpand(true);
    audio_banner.set_hexpand(true);
    audio_banner.set_valign(gtk::Align::Center);
    audio_banner.set_halign(gtk::Align::Center);
    audio_banner.add_css_class("audio-only-banner");
    let note_label = Label::new(Some("♪"));
    note_label.add_css_class("audio-only-note");
    audio_banner.append(&note_label);
    let audio_label = Label::new(Some("Audio only"));
    audio_label.add_css_class("audio-only-subtitle");
    audio_banner.append(&audio_label);

    // Stack: "video" page is the Picture; "audio" page is the banner.
    let preview_stack = Stack::new();
    preview_stack.set_vexpand(true);
    preview_stack.set_hexpand(true);
    preview_stack.set_transition_type(gtk::StackTransitionType::Crossfade);
    preview_stack.add_named(&picture, Some("video"));
    preview_stack.add_named(&audio_banner, Some("audio"));
    preview_stack.set_visible_child_name("video");
    vbox.append(&preview_stack);

    // DragSource on the video display so users can drag the current
    // clip selection (in/out range) directly to the timeline.
    {
        let source_marks = source_marks.clone();
        let player_for_drag = player.clone();
        let preview_drag_was_playing: Rc<Cell<bool>> = Rc::new(Cell::new(false));
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
                Some(gdk4::ContentProvider::for_value(&glib::Value::from(
                    &payload,
                )))
            }
        });
        drag_src.connect_drag_begin({
            let player_for_drag = player_for_drag.clone();
            let preview_drag_was_playing = preview_drag_was_playing.clone();
            move |_, _| {
                let was_playing = player_for_drag.borrow().state() == PlayerState::Playing;
                preview_drag_was_playing.set(was_playing);
                if was_playing {
                    let _ = player_for_drag.borrow().pause();
                }
            }
        });
        drag_src.connect_drag_end({
            let player_for_drag = player_for_drag.clone();
            let preview_drag_was_playing = preview_drag_was_playing.clone();
            move |_, _, _| {
                if preview_drag_was_playing.get() {
                    let _ = player_for_drag.borrow().play();
                }
                preview_drag_was_playing.set(false);
            }
        });
        picture.add_controller(drag_src);

        // Swallow internal source-clip payload drops on the source picture so
        // accidental self-drops are treated as no-ops.
        let self_drop_target = gtk::DropTarget::new(glib::Type::STRING, gdk4::DragAction::COPY);
        self_drop_target.connect_drop(move |_target, value, _x, _y| {
            value
                .get::<String>()
                .ok()
                .is_some_and(|payload| payload.contains('|') && !payload.contains("file://"))
        });
        picture.add_controller(self_drop_target);
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
            if dur == 0 {
                return;
            }
            // Use the drawn width recorded in draw_func; fall back to the
            // widget's allocated width if draw_func hasn't fired yet.
            let w = {
                let dw = drawn_width.get();
                if dw > 1.0 {
                    dw
                } else {
                    scrubber_weak
                        .upgrade()
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
            if let Some(a) = scrubber_weak.upgrade() {
                a.queue_draw();
            }
        })
    };

    // ── Gesture: click OR drag → seek OR drag in/out marker ──────────────
    // DragMode: 0 = seek playhead, 1 = drag In marker, 2 = drag Out marker.
    // Determined in drag_begin by hit-testing the pointer position against the
    // in/out marker positions (within ±8 px of each marker line).
    let drag_mode: Rc<Cell<u8>> = Rc::new(Cell::new(0));
    let marker_drag_was_playing: Rc<Cell<bool>> = Rc::new(Cell::new(false));
    let playhead_drag_was_playing: Rc<Cell<bool>> = Rc::new(Cell::new(false));
    // Throttle marker-drag seeks to keep demux/decoder churn bounded while
    // still providing continuous visual feedback.
    let last_marker_seek_us: Rc<Cell<i64>> = Rc::new(Cell::new(0));
    let last_playhead_seek_us: Rc<Cell<i64>> = Rc::new(Cell::new(0));
    {
        let scrubber_drag = GestureDrag::new();

        // drag_begin: decide what this gesture controls
        scrubber_drag.connect_drag_begin({
            let seek_from_x = seek_from_x.clone();
            let source_marks = source_marks.clone();
            let drawn_width = drawn_width.clone();
            let drag_mode = drag_mode.clone();
            let marker_drag_was_playing = marker_drag_was_playing.clone();
            let playhead_drag_was_playing = playhead_drag_was_playing.clone();
            let player = player.clone();
            let last_marker_seek_us = last_marker_seek_us.clone();
            let last_playhead_seek_us = last_playhead_seek_us.clone();
            let scrubber_weak = scrubber.downgrade();
            move |_, x, _| {
                let marks = source_marks.borrow();
                let dur = marks.duration_ns;
                if dur == 0 {
                    marker_drag_was_playing.set(false);
                    playhead_drag_was_playing.set(false);
                    drag_mode.set(0);
                    drop(marks);
                    seek_from_x(x);
                    return;
                }
                let w = drawn_width.get().max(1.0);
                let in_x = (marks.in_ns as f64 / dur as f64) * w;
                let out_x = (marks.out_ns as f64 / dur as f64) * w;
                drop(marks);
                const HIT: f64 = 8.0;
                if (x - in_x).abs() <= HIT {
                    playhead_drag_was_playing.set(false);
                    let was_playing = player.borrow().state() == PlayerState::Playing;
                    marker_drag_was_playing.set(was_playing);
                    if was_playing {
                        let _ = player.borrow().pause();
                    }
                    drag_mode.set(1);
                    last_marker_seek_us.set(0);
                    let target_pos = source_marks.borrow().in_ns;
                    source_marks.borrow_mut().display_pos_ns = target_pos;
                    if !cfg!(target_os = "macos") {
                        let _ = player.borrow().seek_accurate(target_pos);
                    }
                    if let Some(a) = scrubber_weak.upgrade() {
                        a.queue_draw();
                    }
                } else if (x - out_x).abs() <= HIT {
                    playhead_drag_was_playing.set(false);
                    let was_playing = player.borrow().state() == PlayerState::Playing;
                    marker_drag_was_playing.set(was_playing);
                    if was_playing {
                        let _ = player.borrow().pause();
                    }
                    drag_mode.set(2);
                    last_marker_seek_us.set(0);
                    let target_pos = source_marks.borrow().out_ns;
                    source_marks.borrow_mut().display_pos_ns = target_pos;
                    if !cfg!(target_os = "macos") {
                        let _ = player.borrow().seek_accurate(target_pos);
                    }
                    if let Some(a) = scrubber_weak.upgrade() {
                        a.queue_draw();
                    }
                } else {
                    marker_drag_was_playing.set(false);
                    let was_playing = player.borrow().state() == PlayerState::Playing;
                    playhead_drag_was_playing.set(was_playing);
                    if was_playing {
                        let _ = player.borrow().pause();
                    }
                    drag_mode.set(0);
                    last_playhead_seek_us.set(0);
                    seek_from_x(x);
                }
            }
        });

        // drag_update: apply seek or marker update
        scrubber_drag.connect_drag_update({
            let source_marks = source_marks.clone();
            let drawn_width = drawn_width.clone();
            let drag_mode = drag_mode.clone();
            let player = player.clone();
            let last_marker_seek_us = last_marker_seek_us.clone();
            let last_playhead_seek_us = last_playhead_seek_us.clone();
            let scrubber_weak = scrubber.downgrade();
            move |gesture, offset_x, _| {
                let (start_x, _) = gesture.start_point().unwrap_or((0.0, 0.0));
                let x = start_x + offset_x;
                match drag_mode.get() {
                    0 => {
                        let w = drawn_width.get().max(1.0);
                        let mut marks = source_marks.borrow_mut();
                        let dur = marks.duration_ns;
                        if dur == 0 {
                            return;
                        }
                        let frac = (x / w).clamp(0.0, 1.0);
                        marks.display_pos_ns = (frac * dur as f64) as u64;
                        let target_pos = marks.display_pos_ns;
                        drop(marks);
                        if !cfg!(target_os = "macos") {
                            let now_us = glib::monotonic_time();
                            if now_us - last_playhead_seek_us.get() >= 33_000 {
                                last_playhead_seek_us.set(now_us);
                                let _ = player.borrow().seek(target_pos);
                            }
                        }
                        if let Some(a) = scrubber_weak.upgrade() {
                            a.queue_draw();
                        }
                    }
                    mode => {
                        let w = drawn_width.get().max(1.0);
                        let frac = (x / w).clamp(0.0, 1.0);
                        let mut marks = source_marks.borrow_mut();
                        let dur = marks.duration_ns;
                        if dur == 0 {
                            return;
                        }
                        let pos_ns = (frac * dur as f64) as u64;
                        let target_pos = if mode == 1 {
                            let new_in = pos_ns.min(marks.out_ns.saturating_sub(1_000_000));
                            marks.in_ns = new_in;
                            new_in
                        } else {
                            let new_out = pos_ns.max(marks.in_ns + 1_000_000);
                            marks.out_ns = new_out;
                            new_out
                        };
                        marks.display_pos_ns = target_pos;
                        drop(marks);
                        if !cfg!(target_os = "macos") {
                            let now_us = glib::monotonic_time();
                            if now_us - last_marker_seek_us.get() >= 33_000 {
                                last_marker_seek_us.set(now_us);
                                let _ = player.borrow().seek_accurate(target_pos);
                            }
                        }
                        if let Some(a) = scrubber_weak.upgrade() {
                            a.queue_draw();
                        }
                    }
                }
            }
        });

        // Ensure the final marker position is always reflected in preview
        // even if the last drag_update was throttled.
        scrubber_drag.connect_drag_end({
            let source_marks = source_marks.clone();
            let drag_mode = drag_mode.clone();
            let player = player.clone();
            let marker_drag_was_playing = marker_drag_was_playing.clone();
            let playhead_drag_was_playing = playhead_drag_was_playing.clone();
            move |_, _, _| {
                let mode = drag_mode.get();
                let target_pos = source_marks.borrow().display_pos_ns;
                if mode != 0 {
                    let _ = player.borrow().seek_accurate(target_pos);
                } else {
                    let _ = player.borrow().seek(target_pos);
                }
                if mode != 0 {
                    if marker_drag_was_playing.get() {
                        let _ = player.borrow().play();
                    }
                } else if playhead_drag_was_playing.get() {
                    let _ = player.borrow().play();
                }
                marker_drag_was_playing.set(false);
                playhead_drag_was_playing.set(false);
                drag_mode.set(0);
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

    let in_label = Label::new(Some("In  00:00:00:00"));
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

    let btn_set_in = Button::with_label("Set In (I)");
    let btn_set_out = Button::with_label("Set Out (O)");
    let btn_prev_frame = Button::with_label("◀▮");
    btn_prev_frame.set_tooltip_text(Some("Step back one frame (←)"));
    let btn_stop = Button::with_label("⏹");
    let btn_play_pause = Button::with_label("▶");
    let btn_next_frame = Button::with_label("▮▶");
    btn_next_frame.set_tooltip_text(Some("Step forward one frame (→)"));
    // "Add to Timeline" split button — primary action (Append) + ▼ dropdown.
    let btn_add = Button::with_label("⬇ Add");
    btn_add.set_tooltip_text(Some(
        "Append clip selection to a matching timeline track, creating one if needed",
    ));
    btn_add.set_sensitive(false);

    let btn_add_more = Button::with_label("▼");
    btn_add_more.set_tooltip_text(Some("More add options: Append, Insert, Overwrite"));
    btn_add_more.set_sensitive(false);

    // Popover for the ▼ side of the split button.
    let add_pop = Popover::new();
    let add_pop_box = GBox::new(Orientation::Vertical, 2);
    add_pop_box.set_margin_start(4);
    add_pop_box.set_margin_end(4);
    add_pop_box.set_margin_top(4);
    add_pop_box.set_margin_bottom(4);

    let btn_pop_append = Button::with_label("⬇ Append");
    btn_pop_append.add_css_class("flat");
    btn_pop_append.set_tooltip_text(Some(
        "Append selection to a matching timeline track, creating one if needed",
    ));

    let btn_pop_insert = Button::with_label("⤵ Insert");
    btn_pop_insert.add_css_class("flat");
    btn_pop_insert.set_tooltip_text(Some(
        "Insert at playhead on a matching track, creating one if needed (,)",
    ));

    let btn_pop_overwrite = Button::with_label("⏺ Overwrite");
    btn_pop_overwrite.add_css_class("flat");
    btn_pop_overwrite.set_tooltip_text(Some(
        "Overwrite at playhead on a matching track, creating one if needed (.)",
    ));

    add_pop_box.append(&btn_pop_append);
    add_pop_box.append(&btn_pop_insert);
    add_pop_box.append(&btn_pop_overwrite);
    add_pop.set_child(Some(&add_pop_box));
    add_pop.set_parent(&btn_add_more);
    {
        let add_pop = add_pop.clone();
        btn_add_more.connect_clicked(move |_| {
            if add_pop.is_visible() {
                add_pop.popdown();
            } else {
                add_pop.popup();
            }
        });
    }

    let add_group = GBox::new(Orientation::Horizontal, 0);
    add_group.add_css_class("linked");
    add_group.append(&btn_add);
    add_group.append(&btn_add_more);

    controls.append(&btn_set_in);
    controls.append(&btn_prev_frame);
    controls.append(&btn_stop);
    controls.append(&btn_play_pause);
    controls.append(&btn_next_frame);
    controls.append(&btn_set_out);
    controls.append(&add_group);
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
            if let Some(a) = scrubber_weak.upgrade() {
                a.queue_draw();
            }
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
            if let Some(a) = scrubber_weak.upgrade() {
                a.queue_draw();
            }
        });
    }

    // Primary "Add" button: dispatches to whichever mode was last used.
    let add_mode: Rc<Cell<AddMode>> = Rc::new(Cell::new(AddMode::Append));
    {
        let on_append = on_append.clone();
        let on_insert = on_insert.clone();
        let on_overwrite = on_overwrite.clone();
        let add_mode = add_mode.clone();
        btn_add.connect_clicked(move |_| match add_mode.get() {
            AddMode::Append => on_append(),
            AddMode::Insert => on_insert(),
            AddMode::Overwrite => on_overwrite(),
        });
    }

    // Popover: Append — updates the primary button label and mode.
    {
        let on_append = on_append.clone();
        let add_pop = add_pop.clone();
        let add_mode = add_mode.clone();
        let btn_add = btn_add.clone();
        btn_pop_append.connect_clicked(move |_| {
            add_pop.popdown();
            add_mode.set(AddMode::Append);
            btn_add.set_label("⬇ Append");
            on_append();
        });
    }

    // Popover: Insert — updates the primary button label and mode.
    {
        let on_insert = on_insert.clone();
        let add_pop = add_pop.clone();
        let add_mode = add_mode.clone();
        let btn_add = btn_add.clone();
        btn_pop_insert.connect_clicked(move |_| {
            add_pop.popdown();
            add_mode.set(AddMode::Insert);
            btn_add.set_label("⤵ Insert");
            on_insert();
        });
    }

    // Popover: Overwrite — updates the primary button label and mode.
    {
        let on_overwrite = on_overwrite.clone();
        let add_pop = add_pop.clone();
        let add_mode = add_mode.clone();
        let btn_add = btn_add.clone();
        btn_pop_overwrite.connect_clicked(move |_| {
            add_pop.popdown();
            add_mode.set(AddMode::Overwrite);
            btn_add.set_label("⏺ Overwrite");
            on_overwrite();
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
                PlayerState::Playing => {
                    let _ = p.pause();
                    btn.set_label("▶");
                }
                _ => {
                    let _ = p.play();
                    btn.set_label("⏸");
                }
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
            if let Some(a) = scrubber_weak.upgrade() {
                a.queue_draw();
            }
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
            if let Some(a) = scrubber_weak.upgrade() {
                a.queue_draw();
            }
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
                    if let Some(a) = scrubber_weak.upgrade() {
                        a.queue_draw();
                    }
                    glib::Propagation::Stop
                }
                Key::o | Key::O => {
                    let mut m = source_marks.borrow_mut();
                    let pos = m.display_pos_ns;
                    m.out_ns = pos.max(m.in_ns + 1_000_000);
                    update_marks_bar(&m);
                    drop(m);
                    if let Some(a) = scrubber_weak.upgrade() {
                        a.queue_draw();
                    }
                    glib::Propagation::Stop
                }
                Key::space => {
                    shuttle_speed.set(0);
                    let p = player.borrow();
                    match p.state() {
                        PlayerState::Playing => {
                            let _ = p.pause();
                            btn_play_pause.set_label("▶");
                        }
                        _ => {
                            let _ = p.play();
                            btn_play_pause.set_label("⏸");
                        }
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
                    if let Some(a) = scrubber_weak.upgrade() {
                        a.queue_draw();
                    }
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
                    if let Some(a) = scrubber_weak.upgrade() {
                        a.queue_draw();
                    }
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
        let btn_add = btn_add.clone();
        let btn_add_more = btn_add_more.clone();
        let btn_close_preview = btn_close_preview.clone();
        let picture_weak = picture.downgrade();
        // Track last prescale size to avoid redundant updates
        let last_prescale_w: Rc<Cell<i32>> = Rc::new(Cell::new(320));
        let last_prescale_h: Rc<Cell<i32>> = Rc::new(Cell::new(180));
        glib::timeout_add_local(std::time::Duration::from_millis(100), move || {
            let p = player.borrow();
            let pos = p.position();
            let dur = p.duration();

            // Adaptive prescale: update resolution to match widget size
            if let Some(pic) = picture_weak.upgrade() {
                let pw = pic.width();
                let ph = pic.height();
                if pw > 0 && ph > 0 {
                    // Target exactly the widget size (no supersample): the
                    // safe_sink's own videoconvertscale would rescale a 2×
                    // supersample back down to widget size anyway, wasting two
                    // scale passes per frame.  1× keeps buffer sizes minimal
                    // through the effects chain and eliminates the redundant
                    // second scale.  Cap at 1920×1080 to bound worst-case cost.
                    let target_w = pw.min(1920);
                    let target_h = ph.min(1080);
                    let prev_w = last_prescale_w.get();
                    let prev_h = last_prescale_h.get();
                    // Only update if size changed by >10% to avoid thrashing
                    let dw = (target_w - prev_w).unsigned_abs();
                    let dh = (target_h - prev_h).unsigned_abs();
                    if dw > (prev_w as u32 / 10) || dh > (prev_h as u32 / 10) {
                        p.set_prescale_resolution(target_w, target_h);
                        last_prescale_w.set(target_w);
                        last_prescale_h.set(target_h);
                    }
                }
            }

            // Sync local frame_ns cache from source_marks
            frame_ns.set(source_marks.borrow().frame_ns);
            p.set_source_frame_duration(frame_ns.get());

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
            // Sync duration when the file was imported before the background probe
            // completed (duration was 0 at import time). The player pipeline prerolls
            // within ~100-300ms so p.duration() is available well before user interaction.
            if dur > 0 {
                let mut m = source_marks.borrow_mut();
                if m.duration_ns == 0 {
                    m.duration_ns = dur;
                    m.out_ns = dur;
                }
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
                btn_add.set_sensitive(!m.path.is_empty());
                btn_add_more.set_sensitive(!m.path.is_empty());
                btn_close_preview.set_sensitive(!m.path.is_empty());
                update_marks_bar(&m);
            }
            drop(p);
            if let Some(a) = scrubber_weak.upgrade() {
                a.queue_draw();
            }
            glib::ControlFlow::Continue
        });
    }

    // set_audio_only: switches between the video picture and the audio-only banner.
    let set_audio_only: Rc<dyn Fn(bool)> = {
        let stack = preview_stack.clone();
        Rc::new(move |audio_only: bool| {
            stack.set_visible_child_name(if audio_only { "audio" } else { "video" });
        })
    };

    (vbox, source_marks, clip_name_label, set_audio_only)
}

fn draw_scrubber(cr: &gtk::cairo::Context, width: f64, marks: &SourceMarks) {
    let height = 20.0;
    let dur = marks.duration_ns;

    // Background
    cr.set_source_rgb(0.15, 0.15, 0.17);
    cr.rectangle(0.0, 0.0, width, height);
    cr.fill().ok();

    if dur == 0 {
        return;
    }

    // In/out selection band
    let in_x = (marks.in_ns as f64 / dur as f64) * width;
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
