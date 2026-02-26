use gtk4::prelude::*;
use gtk4::{self as gtk, Box as GBox, Button, Label, Orientation, Overlay, Picture};
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
/// Returns `(widget, pos_label, picture_a, picture_b)`.
/// `picture_a` displays the primary (outgoing) clip; `picture_b` displays the incoming
/// transition clip. The caller controls cross-dissolve by setting widget opacity on
/// each picture each poll tick via `Widget::set_opacity()`.
pub fn build_program_monitor(
    program_player: Rc<RefCell<ProgramPlayer>>,
    paintable_a: gdk4::Paintable,
    paintable_b: gdk4::Paintable,
    on_stop: impl Fn() + 'static,
    on_play_pause: impl Fn() + 'static,
    on_toggle_popout: impl Fn() + 'static,
) -> (GBox, Label, Picture, Picture) {
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

    (root, pos_label, picture_a, picture_b)
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
