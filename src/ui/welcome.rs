use gtk4::prelude::*;
use gtk4::{Align, Box as GBox, Button, Label, Orientation};
use std::rc::Rc;

use crate::project_versions::RecoverableAutosave;

/// Build the welcome panel shown on fresh launch (no startup project).
/// Contains branding, New/Open buttons, optional crash-recovery section,
/// and a recent projects list.
pub fn build_welcome_panel(
    on_new_project: Rc<dyn Fn()>,
    on_open_project: Rc<dyn Fn()>,
    on_open_recent: Rc<dyn Fn(String)>,
    recoverable: Vec<RecoverableAutosave>,
    on_recover: Rc<dyn Fn(RecoverableAutosave)>,
    on_discard_autosave: Rc<dyn Fn(RecoverableAutosave)>,
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

    // ── Recovery section ─────────────────────────────────────────────
    if !recoverable.is_empty() {
        let spacer_r = GBox::new(Orientation::Vertical, 0);
        spacer_r.set_height_request(12);
        inner.append(&spacer_r);

        let recovery_frame = GBox::new(Orientation::Vertical, 6);
        recovery_frame.add_css_class("welcome-recovery-section");

        let recovery_header = Label::new(Some("⚠ Recover Unsaved Work"));
        recovery_header.add_css_class("welcome-recovery-header");
        recovery_header.set_halign(Align::Start);
        recovery_frame.append(&recovery_header);

        for entry in recoverable {
            let row = GBox::new(Orientation::Horizontal, 8);
            row.set_hexpand(true);

            // Project title and path info
            let info_box = GBox::new(Orientation::Vertical, 2);
            info_box.set_hexpand(true);

            let title_label = Label::new(Some(&entry.metadata.project_title));
            title_label.set_halign(Align::Start);
            title_label.set_ellipsize(gtk4::pango::EllipsizeMode::Middle);
            info_box.append(&title_label);

            let detail = if let Some(ref fp) = entry.metadata.project_file_path {
                let fname = std::path::Path::new(fp)
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or(fp);
                format!(
                    "{} — {}",
                    format_autosave_age(entry.metadata.saved_at_unix_secs),
                    fname
                )
            } else {
                format!(
                    "{} — unsaved project",
                    format_autosave_age(entry.metadata.saved_at_unix_secs)
                )
            };
            let detail_label = Label::new(Some(&detail));
            detail_label.add_css_class("dim-label");
            detail_label.set_halign(Align::Start);
            detail_label.set_ellipsize(gtk4::pango::EllipsizeMode::End);
            info_box.append(&detail_label);

            row.append(&info_box);

            // Recover button
            let btn_recover = Button::with_label("Recover");
            btn_recover.add_css_class("suggested-action");
            {
                let cb = on_recover.clone();
                let e = entry.clone();
                btn_recover.connect_clicked(move |_| cb(e.clone()));
            }
            row.append(&btn_recover);

            // Discard button
            let btn_discard = Button::with_label("Discard");
            btn_discard.add_css_class("destructive-action");
            {
                let cb = on_discard_autosave.clone();
                let e = entry.clone();
                let row_weak = row.downgrade();
                btn_discard.connect_clicked(move |_| {
                    cb(e.clone());
                    // Remove this row from the UI
                    if let Some(r) = row_weak.upgrade() {
                        if let Some(parent) = r.parent() {
                            if let Some(bx) = parent.downcast_ref::<GBox>() {
                                bx.remove(&r);
                            }
                        }
                    }
                });
            }
            row.append(&btn_discard);

            recovery_frame.append(&row);
        }

        inner.append(&recovery_frame);
    }

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

/// Format a unix timestamp as a human-readable age string (e.g. "2 hours ago").
fn format_autosave_age(saved_at: u64) -> String {
    let now = crate::project_versions::now_unix_secs();
    let diff = now.saturating_sub(saved_at);
    if diff < 60 {
        "just now".to_string()
    } else if diff < 3600 {
        let mins = diff / 60;
        format!("{mins} min ago")
    } else if diff < 86400 {
        let hours = diff / 3600;
        if hours == 1 {
            "1 hour ago".to_string()
        } else {
            format!("{hours} hours ago")
        }
    } else {
        let days = diff / 86400;
        if days == 1 {
            "1 day ago".to_string()
        } else {
            format!("{days} days ago")
        }
    }
}
