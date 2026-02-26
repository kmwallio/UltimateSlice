use gtk4::prelude::*;
use gtk4::{self as gtk, Box as GBox, CheckButton, Dialog, Label, Orientation, ResponseType, Stack, StackSidebar};
use std::rc::Rc;
use crate::ui_state::{PlaybackPriority, ProxyMode, PreferencesState};

pub fn show_preferences_dialog(
    parent: &gtk::Window,
    current: PreferencesState,
    on_save: Rc<dyn Fn(PreferencesState)>,
) {
    let dialog = Dialog::builder()
        .title("Preferences")
        .transient_for(parent)
        .modal(true)
        .default_width(640)
        .default_height(420)
        .build();
    dialog.add_button("Cancel", ResponseType::Cancel);
    dialog.add_button("Save", ResponseType::Accept);

    let body = GBox::new(Orientation::Horizontal, 0);
    body.set_margin_start(12);
    body.set_margin_end(12);
    body.set_margin_top(12);
    body.set_margin_bottom(12);

    let stack = Stack::new();
    stack.set_hexpand(true);
    stack.set_vexpand(true);
    stack.set_margin_start(12);
    stack.set_margin_end(8);
    stack.set_margin_top(4);
    stack.set_margin_bottom(4);

    let sidebar = StackSidebar::new();
    sidebar.set_stack(&stack);
    sidebar.set_margin_start(8);
    sidebar.set_margin_end(8);
    sidebar.set_margin_top(8);
    sidebar.set_margin_bottom(8);
    sidebar.set_vexpand(true);

    let general_box = GBox::new(Orientation::Vertical, 8);
    general_box.set_margin_start(8);
    general_box.set_margin_end(8);
    general_box.set_margin_top(8);
    let general_label = Label::new(Some("General preferences will appear here."));
    general_label.set_halign(gtk::Align::Start);
    general_box.append(&general_label);
    stack.add_titled(&general_box, Some("general"), "General");

    let playback_box = GBox::new(Orientation::Vertical, 10);
    playback_box.set_margin_start(8);
    playback_box.set_margin_end(8);
    playback_box.set_margin_top(8);
    let playback_label = Label::new(Some("Playback / Performance"));
    playback_label.set_halign(gtk::Align::Start);
    playback_label.add_css_class("title-4");
    let hw_accel = CheckButton::with_label("Enable hardware acceleration");
    hw_accel.set_active(current.hardware_acceleration_enabled);
    hw_accel.set_halign(gtk::Align::Start);
    let playback_priority = gtk4::ComboBoxText::new();
    playback_priority.append(Some("smooth"), "Smooth (prioritize playback continuity)");
    playback_priority.append(Some("balanced"), "Balanced");
    playback_priority.append(Some("accurate"), "Accurate (prioritize seek/frame precision)");
    playback_priority.set_active_id(Some(current.playback_priority.as_str()));
    playback_priority.set_halign(gtk::Align::Start);
    let hint = Label::new(Some("Applies to source preview playback immediately (with non-GL fallback when needed)."));
    hint.set_halign(gtk::Align::Start);
    hint.add_css_class("dim-label");
    let priority_hint = Label::new(Some("Program monitor playback priority controls smoothness vs frame precision during active playback."));
    priority_hint.set_halign(gtk::Align::Start);
    priority_hint.add_css_class("dim-label");
    playback_box.append(&playback_label);
    playback_box.append(&hw_accel);
    playback_box.append(&hint);
    playback_box.append(&Label::new(Some("Program monitor playback priority")));
    playback_box.append(&playback_priority);
    playback_box.append(&priority_hint);

    let proxy_label = Label::new(Some("Proxy preview mode"));
    proxy_label.set_halign(gtk::Align::Start);
    let proxy_mode = gtk4::ComboBoxText::new();
    proxy_mode.append(Some("off"), "Off (use original media)");
    proxy_mode.append(Some("half_res"), "Half resolution");
    proxy_mode.append(Some("quarter_res"), "Quarter resolution");
    proxy_mode.set_active_id(Some(current.proxy_mode.as_str()));
    proxy_mode.set_halign(gtk::Align::Start);
    let proxy_hint = Label::new(Some("Generate lightweight proxy files for smoother preview playback. Export always uses original media."));
    proxy_hint.set_halign(gtk::Align::Start);
    proxy_hint.add_css_class("dim-label");
    proxy_hint.set_wrap(true);
    proxy_hint.set_max_width_chars(60);
    playback_box.append(&proxy_label);
    playback_box.append(&proxy_mode);
    playback_box.append(&proxy_hint);
    stack.add_titled(&playback_box, Some("playback"), "Playback");

    body.append(&sidebar);
    body.append(&stack);
    dialog.content_area().append(&body);

    dialog.connect_response(move |d, resp| {
        if resp == ResponseType::Accept {
            on_save(PreferencesState {
                hardware_acceleration_enabled: hw_accel.is_active(),
                playback_priority: PlaybackPriority::from_str(playback_priority.active_id().as_deref().unwrap_or("smooth")),
                proxy_mode: ProxyMode::from_str(proxy_mode.active_id().as_deref().unwrap_or("off")),
            });
        }
        d.close();
    });
    dialog.present();
}
