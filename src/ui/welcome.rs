use gtk4::prelude::*;
use gtk4::{Align, Box as GBox, Button, Label, Orientation};
use std::rc::Rc;

/// Build the welcome panel shown on fresh launch (no startup project).
/// Contains branding, New/Open buttons, and a recent projects list.
pub fn build_welcome_panel(
    on_new_project: Rc<dyn Fn()>,
    on_open_project: Rc<dyn Fn()>,
    on_open_recent: Rc<dyn Fn(String)>,
) -> GBox {
    let outer = GBox::new(Orientation::Vertical, 0);
    outer.set_valign(Align::Center);
    outer.set_halign(Align::Center);
    outer.set_vexpand(true);
    outer.set_hexpand(true);

    let inner = GBox::new(Orientation::Vertical, 12);
    inner.set_halign(Align::Center);
    inner.set_margin_start(40);
    inner.set_margin_end(40);
    inner.set_margin_top(40);
    inner.set_margin_bottom(40);

    // Title
    let title = Label::new(Some("UltimateSlice"));
    title.add_css_class("welcome-title");
    inner.append(&title);

    // Subtitle
    let subtitle = Label::new(Some("Non-linear Video Editor"));
    subtitle.add_css_class("welcome-subtitle");
    inner.append(&subtitle);

    // Spacer
    let spacer = GBox::new(Orientation::Vertical, 0);
    spacer.set_height_request(16);
    inner.append(&spacer);

    // Action buttons
    let btn_row = GBox::new(Orientation::Horizontal, 12);
    btn_row.set_halign(Align::Center);

    let btn_new = Button::with_label("New Project");
    btn_new.add_css_class("suggested-action");
    btn_new.set_width_request(160);
    {
        let cb = on_new_project.clone();
        btn_new.connect_clicked(move |_| cb());
    }
    btn_row.append(&btn_new);

    let btn_open = Button::with_label("Open Project\u{2026}");
    btn_open.set_width_request(160);
    {
        let cb = on_open_project.clone();
        btn_open.connect_clicked(move |_| cb());
    }
    btn_row.append(&btn_open);

    inner.append(&btn_row);

    // Recent projects section
    let recent_entries = crate::recent::load();
    if !recent_entries.is_empty() {
        let spacer2 = GBox::new(Orientation::Vertical, 0);
        spacer2.set_height_request(12);
        inner.append(&spacer2);

        let recent_header = Label::new(Some("Recent Projects"));
        recent_header.add_css_class("welcome-section-header");
        recent_header.set_halign(Align::Start);
        inner.append(&recent_header);

        let recent_list = GBox::new(Orientation::Vertical, 2);
        recent_list.set_width_request(400);

        for path_str in &recent_entries {
            let display_name = std::path::Path::new(path_str)
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or(path_str)
                .to_string();

            // Get file modification date for display
            let date_str = std::fs::metadata(path_str)
                .and_then(|m| m.modified())
                .ok()
                .and_then(|t| {
                    let secs = t.duration_since(std::time::UNIX_EPOCH).ok()?.as_secs();
                    let days = secs / 86400;
                    let (y, m, d) = crate::project_versions::days_to_ymd(days);
                    Some(format!("{y:04}-{m:02}-{d:02}"))
                })
                .unwrap_or_default();

            let row = Button::new();
            row.add_css_class("flat");
            row.add_css_class("welcome-recent-item");

            let row_box = GBox::new(Orientation::Horizontal, 8);
            row_box.set_hexpand(true);

            let name_label = Label::new(Some(&display_name));
            name_label.set_halign(Align::Start);
            name_label.set_hexpand(true);
            name_label.set_ellipsize(gtk4::pango::EllipsizeMode::Middle);
            row_box.append(&name_label);

            if !date_str.is_empty() {
                let date_label = Label::new(Some(&date_str));
                date_label.add_css_class("dim-label");
                date_label.set_halign(Align::End);
                row_box.append(&date_label);
            }

            row.set_child(Some(&row_box));
            row.set_tooltip_text(Some(path_str));

            let path_owned = path_str.clone();
            let cb = on_open_recent.clone();
            row.connect_clicked(move |_| {
                cb(path_owned.clone());
            });

            recent_list.append(&row);
        }

        inner.append(&recent_list);
    }

    // Tip
    let spacer3 = GBox::new(Orientation::Vertical, 0);
    spacer3.set_height_request(16);
    inner.append(&spacer3);

    let tip = Label::new(Some(
        "Tip: Drag media files onto the timeline to start editing.",
    ));
    tip.add_css_class("welcome-tip");
    inner.append(&tip);

    outer.append(&inner);
    outer
}
