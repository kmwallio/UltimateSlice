//! Color-tag legend popover.
//!
//! Small popover in the status bar that lets users attach a per-project
//! meaning to each `ClipColorLabel` (e.g. Red = "B-roll",
//! Green = "Interview"). Persisted on `Project.color_label_names` and
//! round-tripped through FCPXML as `us:color-label-names` on `<event>`.
//!
//! The popover is rebuilt from the current project state on every open so
//! swapping projects mid-session immediately reflects the active project's
//! legend without extra wiring from `window.rs`.

use crate::model::clip::ClipColorLabel;
use crate::model::project::Project;
use gtk4 as gtk;
use gtk4::prelude::*;
use gtk4::{Box as GBox, DrawingArea, Entry, Label, MenuButton, Orientation, Popover};
use std::cell::RefCell;
use std::rc::Rc;

/// Build the color-tag legend MenuButton. The returned widget is meant to
/// live on the timeline / status bar.
///
/// `on_project_changed` fires after the user edits a legend entry and the
/// project actually changed (trimmed-equal edits are silently skipped).
pub fn build_color_legend_button(
    project: Rc<RefCell<Project>>,
    on_project_changed: Rc<dyn Fn()>,
) -> MenuButton {
    let popover = Popover::new();
    popover.set_autohide(true);
    // Rebuild contents on each open so the latest project state wins.
    {
        let project = project.clone();
        let popover_weak = popover.downgrade();
        let on_project_changed = on_project_changed.clone();
        popover.connect_show(move |_| {
            if let Some(popover) = popover_weak.upgrade() {
                let content = build_legend_content(project.clone(), on_project_changed.clone());
                popover.set_child(Some(&content));
            }
        });
    }
    // Also populate once up-front so the popover has content before its
    // first `show` signal fires (e.g. when a caller queries the child).
    popover.set_child(Some(&build_legend_content(
        project.clone(),
        on_project_changed.clone(),
    )));

    let menu_btn = MenuButton::new();
    menu_btn.set_label("Color Legend");
    menu_btn.set_popover(Some(&popover));
    menu_btn.set_tooltip_text(Some(
        "Label what each clip color means in this project (e.g. Red = B-roll). Saved with the project.",
    ));
    menu_btn
}

fn build_legend_content(project: Rc<RefCell<Project>>, on_project_changed: Rc<dyn Fn()>) -> GBox {
    let container = GBox::new(Orientation::Vertical, 4);
    container.set_margin_top(10);
    container.set_margin_bottom(10);
    container.set_margin_start(12);
    container.set_margin_end(12);

    let header = Label::new(Some("Color Legend"));
    header.add_css_class("heading");
    header.set_xalign(0.0);
    container.append(&header);

    let hint = Label::new(Some(
        "Describe what each color means in this project. Leave blank to reset to the default.",
    ));
    hint.add_css_class("dim-label");
    hint.set_wrap(true);
    hint.set_max_width_chars(36);
    hint.set_xalign(0.0);
    hint.set_margin_bottom(4);
    container.append(&hint);

    for label in ClipColorLabel::PALETTE {
        let row = GBox::new(Orientation::Horizontal, 8);

        let swatch = DrawingArea::new();
        swatch.set_content_width(16);
        swatch.set_content_height(16);
        swatch.set_valign(gtk::Align::Center);
        let (r, g, b) = label.swatch_rgb();
        swatch.set_draw_func(move |_da, cr, w, h| {
            let radius = (w.min(h) as f64) * 0.5;
            let cx = w as f64 / 2.0;
            let cy = h as f64 / 2.0;
            cr.arc(cx, cy, radius - 1.0, 0.0, std::f64::consts::TAU);
            cr.set_source_rgb(r, g, b);
            let _ = cr.fill_preserve();
            cr.set_source_rgba(0.05, 0.05, 0.06, 0.7);
            cr.set_line_width(1.0);
            let _ = cr.stroke();
        });
        row.append(&swatch);

        let entry = Entry::new();
        entry.set_placeholder_text(Some(label.default_display_name()));
        entry.set_width_chars(22);
        entry.set_hexpand(true);
        entry.set_max_length(64);
        // Preload current value.
        let initial = project
            .borrow()
            .color_label_names
            .get(&label)
            .cloned()
            .unwrap_or_default();
        entry.set_text(&initial);

        // Commit on focus-leave or Enter-pressed. We intentionally skip
        // per-keystroke updates to avoid spamming the dirty flag and to
        // let users type freely without intermediate "half-words" landing
        // in the saved project.
        let commit = {
            let project = project.clone();
            let on_project_changed = on_project_changed.clone();
            Rc::new(move |text: &str| {
                let changed = {
                    let mut proj = project.borrow_mut();
                    let prev_dirty = proj.dirty;
                    let changed = proj.set_color_label_name(label, text);
                    if changed {
                        proj.dirty = true;
                    } else {
                        proj.dirty = prev_dirty;
                    }
                    changed
                };
                if changed {
                    on_project_changed();
                }
            })
        };
        {
            let commit = commit.clone();
            entry.connect_activate(move |e| commit(&e.text()));
        }
        {
            let commit = commit.clone();
            let focus = gtk::EventControllerFocus::new();
            focus.connect_leave({
                let entry_weak = entry.downgrade();
                move |_| {
                    if let Some(entry) = entry_weak.upgrade() {
                        commit(&entry.text());
                    }
                }
            });
            entry.add_controller(focus);
        }

        row.append(&entry);
        container.append(&row);
    }

    container
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn commit_is_noop_when_text_matches_existing_override() {
        let mut p = Project::new("noop");
        p.set_color_label_name(ClipColorLabel::Red, "B-roll");
        let changed = p.set_color_label_name(ClipColorLabel::Red, "B-roll");
        assert!(!changed);
    }
}
