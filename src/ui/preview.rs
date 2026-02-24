use gtk4::prelude::*;
use gtk4::{self as gtk, Box as GBox, Button, Label, Orientation, Picture};
use glib;
use std::cell::RefCell;
use std::rc::Rc;
use crate::media::player::{Player, PlayerState};

/// Builds the preview panel: a video picture + transport controls.
/// Returns `(widget, player)` — `player` must be kept alive.
pub fn build_preview(player: Rc<RefCell<Player>>, paintable: gdk4::Paintable) -> GBox {
    let vbox = GBox::new(Orientation::Vertical, 0);
    vbox.set_hexpand(true);
    vbox.set_vexpand(true);

    // Video display
    let picture = Picture::new();
    picture.set_paintable(Some(&paintable));
    picture.set_vexpand(true);
    picture.set_hexpand(true);
    picture.set_content_fit(gtk::ContentFit::Contain);
    picture.add_css_class("preview-picture");
    vbox.append(&picture);

    // Timecode label
    let timecode_label = Label::new(Some("0:00 / 0:00"));
    timecode_label.set_margin_top(4);
    vbox.append(&timecode_label);

    // Transport bar
    let controls = GBox::new(Orientation::Horizontal, 6);
    controls.set_halign(gtk::Align::Center);
    controls.set_margin_top(4);
    controls.set_margin_bottom(8);

    let btn_play_pause = Button::with_label("▶");
    let btn_stop = Button::with_label("⏹");

    controls.append(&btn_stop);
    controls.append(&btn_play_pause);
    vbox.append(&controls);

    // Play/Pause toggle
    {
        let player = player.clone();
        let btn = btn_play_pause.clone();
        btn_play_pause.connect_clicked(move |_| {
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
        btn_stop.connect_clicked(move |_| {
            let p = player.borrow();
            let _ = p.stop();
            btn.set_label("▶");
        });
    }

    // Update timecode every 250ms
    {
        let player = player.clone();
        let label = timecode_label.clone();
        glib::timeout_add_local(std::time::Duration::from_millis(250), move || {
            let p = player.borrow();
            let pos = p.position();
            let dur = p.duration();
            label.set_text(&format!(
                "{} / {}",
                ns_to_timecode(pos),
                ns_to_timecode(dur)
            ));
            glib::ControlFlow::Continue
        });
    }

    vbox
}

fn ns_to_timecode(ns: u64) -> String {
    let secs = ns / 1_000_000_000;
    let m = secs / 60;
    let s = secs % 60;
    format!("{m}:{s:02}")
}
