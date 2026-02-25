use gtk4::prelude::*;
use gtk4::{self as gtk, Box as GBox, Button, Label, Orientation, Picture};
use glib;
use std::cell::RefCell;
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
pub fn build_program_monitor(
    program_player: Rc<RefCell<ProgramPlayer>>,
    paintable: gdk4::Paintable,
) -> GBox {
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

    // Video display
    let picture = Picture::new();
    picture.set_paintable(Some(&paintable));
    picture.set_hexpand(true);
    picture.set_vexpand(true);
    picture.set_content_fit(gtk::ContentFit::Contain);
    picture.add_css_class("preview-video");

    root.append(&picture);

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

    // 100 ms timer: poll position and update timecode label
    {
        let pp = program_player.clone();
        glib::timeout_add_local(std::time::Duration::from_millis(100), move || {
            pp.borrow_mut().poll();
            let pos_ns = pp.borrow().timeline_pos_ns;
            pos_label.set_text(&format_timecode(pos_ns));
            glib::ControlFlow::Continue
        });
    }

    root
}

fn format_timecode(ns: u64) -> String {
    let total_frames = ns / (1_000_000_000 / 30);
    let frames = total_frames % 30;
    let secs   = ns / 1_000_000_000;
    let s      = secs % 60;
    let m      = (secs / 60) % 60;
    let h      = secs / 3600;
    format!("{h:02}:{m:02}:{s:02};{frames:02}")
}

