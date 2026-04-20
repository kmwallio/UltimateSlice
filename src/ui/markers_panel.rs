//! Marker List Panel.
//!
//! Bottom-of-window panel (alongside Keyframes / Transcript) that shows every
//! project marker in a scrollable list. Each row displays a color swatch,
//! editable label, timecode, truncated notes, and a delete button.
//!
//! - **Double-click a row** → seek playhead to that marker's position.
//! - **Inline editing** — click label/notes to edit; color via swatch popover.
//! - **All mutations** go through undo commands (`AddMarkerCommand`,
//!   `RemoveMarkerCommand`, `EditMarkerCommand`).

use crate::model::project::{FrameRate, Marker, Project};
use crate::ui::timecode::format_ns_as_timecode;
use crate::ui::timeline::TimelineState;
use gtk4::prelude::*;
use gtk4::{self as gtk, Box as GBox, Button, Label, ListBox, Orientation, ScrolledWindow};
use std::cell::RefCell;
use std::rc::Rc;

/// Preset marker color palette (RGBA packed as 0xRRGGBBAA).
const MARKER_COLOR_PRESETS: &[(u32, &str)] = &[
    (0xFF8C00FF, "Orange"),
    (0xFF0000FF, "Red"),
    (0xFFFF00FF, "Yellow"),
    (0x00CC00FF, "Green"),
    (0x00BFBFFF, "Teal"),
    (0x3366FFFF, "Blue"),
    (0x9933FFFF, "Purple"),
    (0xFF33CCFF, "Magenta"),
];

/// Shared view state for the markers panel, held in an `Rc` by `window.rs`.
pub struct MarkersPanelView {
    list_box: ListBox,
    status_label: Label,
    project: Rc<RefCell<Project>>,
    timeline_state: Rc<RefCell<TimelineState>>,
    on_project_changed: Rc<dyn Fn()>,
    on_seek: Rc<dyn Fn(u64)>,
}

impl MarkersPanelView {
    /// Rebuild the list from the current project markers.
    pub fn rebuild_from_project(&self, project: &Project) {
        // Remove existing rows.
        while let Some(child) = self.list_box.first_child() {
            self.list_box.remove(&child);
        }

        let count = project.markers.len();
        self.status_label.set_text(&format!(
            "{count} marker{}",
            if count == 1 { "" } else { "s" }
        ));

        let frame_rate = project.frame_rate.clone();
        for marker in &project.markers {
            let row = self.build_marker_row(marker, &frame_rate);
            self.list_box.append(&row);
        }
    }

    fn build_marker_row(&self, marker: &Marker, frame_rate: &FrameRate) -> gtk::ListBoxRow {
        let row = gtk::ListBoxRow::new();
        row.set_selectable(true);
        row.set_activatable(true);

        let hbox = GBox::new(Orientation::Horizontal, 6);
        hbox.set_margin_start(4);
        hbox.set_margin_end(4);
        hbox.set_margin_top(2);
        hbox.set_margin_bottom(2);

        // ── Color swatch button ──
        let (r, g, b, _a) = unpack_rgba(marker.color);
        let color_btn = Button::new();
        color_btn.set_label("●");
        color_btn.add_css_class("flat");
        color_btn.set_size_request(22, 22);
        color_btn.set_tooltip_text(Some("Change marker color"));
        let css = format!(
            "button {{ color: rgb({},{},{}); }}",
            (r * 255.0) as u8,
            (g * 255.0) as u8,
            (b * 255.0) as u8,
        );
        let provider = gtk::CssProvider::new();
        provider.load_from_string(&css);
        color_btn
            .style_context()
            .add_provider(&provider, gtk::STYLE_PROVIDER_PRIORITY_APPLICATION);
        hbox.append(&color_btn);

        // Wire color button → popover with preset swatches
        {
            let marker_id = marker.id.clone();
            let project = self.project.clone();
            let timeline_state = self.timeline_state.clone();
            let on_project_changed = self.on_project_changed.clone();
            color_btn.connect_clicked(move |btn| {
                let pop = gtk::Popover::new();
                pop.set_parent(btn);
                let color_row = GBox::new(Orientation::Horizontal, 2);
                color_row.set_margin_start(4);
                color_row.set_margin_end(4);
                color_row.set_margin_top(4);
                color_row.set_margin_bottom(4);
                for &(rgba, label) in MARKER_COLOR_PRESETS {
                    let swatch = Button::new();
                    swatch.set_label("●");
                    swatch.add_css_class("flat");
                    swatch.set_size_request(22, 22);
                    swatch.set_tooltip_text(Some(label));
                    let (sr, sg, sb, _) = unpack_rgba(rgba);
                    let swatch_css = format!(
                        "button {{ color: rgb({},{},{}); }}",
                        (sr * 255.0) as u8,
                        (sg * 255.0) as u8,
                        (sb * 255.0) as u8,
                    );
                    let sp = gtk::CssProvider::new();
                    sp.load_from_string(&swatch_css);
                    swatch
                        .style_context()
                        .add_provider(&sp, gtk::STYLE_PROVIDER_PRIORITY_APPLICATION);
                    let mid = marker_id.clone();
                    let proj = project.clone();
                    let ts = timeline_state.clone();
                    let opc = on_project_changed.clone();
                    let pop_ref = pop.downgrade();
                    swatch.connect_clicked(move |_| {
                        let old = {
                            let p = proj.borrow();
                            p.markers.iter().find(|m| m.id == mid).cloned()
                        };
                        if let Some(old) = old {
                            let mut new_marker = old.clone();
                            new_marker.color = rgba;
                            let cmd = crate::undo::EditMarkerCommand {
                                marker_id: mid.clone(),
                                old_state: old,
                                new_state: new_marker,
                            };
                            let mut st = ts.borrow_mut();
                            let mut p = proj.borrow_mut();
                            st.history.execute(Box::new(cmd), &mut p);
                            drop(p);
                            drop(st);
                            opc();
                        }
                        if let Some(p) = pop_ref.upgrade() {
                            p.popdown();
                        }
                    });
                    color_row.append(&swatch);
                }
                pop.set_child(Some(&color_row));
                pop.popup();
            });
        }

        // ── Label (editable) ──
        let label_text = if marker.label.is_empty() {
            "Marker"
        } else {
            &marker.label
        };
        let label_btn = Button::with_label(label_text);
        label_btn.add_css_class("flat");
        label_btn.set_hexpand(false);
        label_btn.set_tooltip_text(Some("Click to rename"));
        hbox.append(&label_btn);

        // Wire label editing via popover with Entry
        {
            let marker_id = marker.id.clone();
            let project = self.project.clone();
            let timeline_state = self.timeline_state.clone();
            let on_project_changed = self.on_project_changed.clone();
            label_btn.connect_clicked(move |btn| {
                let pop = gtk::Popover::new();
                pop.set_parent(btn);
                let entry = gtk::Entry::new();
                {
                    let p = project.borrow();
                    if let Some(m) = p.markers.iter().find(|m| m.id == marker_id) {
                        entry.set_text(&m.label);
                    }
                }
                let mid = marker_id.clone();
                let proj = project.clone();
                let ts = timeline_state.clone();
                let opc = on_project_changed.clone();
                let pop_ref = pop.downgrade();
                entry.connect_activate(move |e| {
                    let new_label = e.text().to_string();
                    let old = {
                        let p = proj.borrow();
                        p.markers.iter().find(|m| m.id == mid).cloned()
                    };
                    if let Some(old) = old {
                        let mut new_marker = old.clone();
                        new_marker.label = new_label;
                        let cmd = crate::undo::EditMarkerCommand {
                            marker_id: mid.clone(),
                            old_state: old,
                            new_state: new_marker,
                        };
                        let mut st = ts.borrow_mut();
                        let mut p = proj.borrow_mut();
                        st.history.execute(Box::new(cmd), &mut p);
                        drop(p);
                        drop(st);
                        opc();
                    }
                    if let Some(p) = pop_ref.upgrade() {
                        p.popdown();
                    }
                });
                pop.set_child(Some(&entry));
                pop.popup();
                entry.grab_focus();
            });
        }

        // ── Timecode ──
        let tc = format_ns_as_timecode(marker.position_ns, frame_rate);
        let tc_label = Label::new(Some(&tc));
        tc_label.add_css_class("monospace");
        tc_label.set_width_chars(12);
        tc_label.set_xalign(0.0);
        hbox.append(&tc_label);

        // ── Notes (truncated with tooltip) ──
        let notes_text = if marker.notes.is_empty() {
            "—"
        } else {
            &marker.notes
        };
        let notes_btn = Button::with_label(notes_text);
        notes_btn.add_css_class("flat");
        notes_btn.set_hexpand(true);
        notes_btn.set_tooltip_text(if marker.notes.is_empty() {
            Some("Click to add notes")
        } else {
            Some(&marker.notes)
        });
        // Truncate long labels
        if let Some(child) = notes_btn.child() {
            if let Some(lbl) = child.downcast_ref::<Label>() {
                lbl.set_ellipsize(gtk4::pango::EllipsizeMode::End);
                lbl.set_max_width_chars(30);
                lbl.set_xalign(0.0);
            }
        }
        hbox.append(&notes_btn);

        // Wire notes editing via popover
        {
            let marker_id = marker.id.clone();
            let project = self.project.clone();
            let timeline_state = self.timeline_state.clone();
            let on_project_changed = self.on_project_changed.clone();
            notes_btn.connect_clicked(move |btn| {
                let pop = gtk::Popover::new();
                pop.set_parent(btn);
                let entry = gtk::Entry::new();
                entry.set_width_chars(30);
                entry.set_placeholder_text(Some("Notes…"));
                {
                    let p = project.borrow();
                    if let Some(m) = p.markers.iter().find(|m| m.id == marker_id) {
                        entry.set_text(&m.notes);
                    }
                }
                let mid = marker_id.clone();
                let proj = project.clone();
                let ts = timeline_state.clone();
                let opc = on_project_changed.clone();
                let pop_ref = pop.downgrade();
                entry.connect_activate(move |e| {
                    let new_notes = e.text().to_string();
                    let old = {
                        let p = proj.borrow();
                        p.markers.iter().find(|m| m.id == mid).cloned()
                    };
                    if let Some(old) = old {
                        let mut new_marker = old.clone();
                        new_marker.notes = new_notes;
                        let cmd = crate::undo::EditMarkerCommand {
                            marker_id: mid.clone(),
                            old_state: old,
                            new_state: new_marker,
                        };
                        let mut st = ts.borrow_mut();
                        let mut p = proj.borrow_mut();
                        st.history.execute(Box::new(cmd), &mut p);
                        drop(p);
                        drop(st);
                        opc();
                    }
                    if let Some(p) = pop_ref.upgrade() {
                        p.popdown();
                    }
                });
                pop.set_child(Some(&entry));
                pop.popup();
                entry.grab_focus();
            });
        }

        // ── Delete button ──
        let del_btn = Button::with_label("🗑");
        del_btn.add_css_class("flat");
        del_btn.set_tooltip_text(Some("Delete marker"));
        {
            let marker_id = marker.id.clone();
            let project = self.project.clone();
            let timeline_state = self.timeline_state.clone();
            let on_project_changed = self.on_project_changed.clone();
            del_btn.connect_clicked(move |_| {
                let old = {
                    let p = project.borrow();
                    p.markers.iter().find(|m| m.id == marker_id).cloned()
                };
                if let Some(marker) = old {
                    let mut st = timeline_state.borrow_mut();
                    let mut p = project.borrow_mut();
                    st.history.execute(
                        Box::new(crate::undo::RemoveMarkerCommand { marker }),
                        &mut p,
                    );
                    drop(p);
                    drop(st);
                    on_project_changed();
                }
            });
        }
        hbox.append(&del_btn);

        row.set_child(Some(&hbox));

        // Store marker id for double-click seek
        unsafe {
            row.set_data("marker-id", marker.id.clone());
        }

        row
    }
}

/// Build the markers panel widget. Returns the root box and the shared view
/// handle for refreshing from `on_project_changed`.
pub fn build_markers_panel(
    project: Rc<RefCell<Project>>,
    timeline_state: Rc<RefCell<TimelineState>>,
    on_project_changed: Rc<dyn Fn()>,
    on_seek: Rc<dyn Fn(u64)>,
) -> (GBox, Rc<MarkersPanelView>) {
    let root = GBox::new(Orientation::Vertical, 0);
    root.set_margin_start(4);
    root.set_margin_end(4);
    root.set_margin_top(4);
    root.set_margin_bottom(4);

    // ── Header row ──
    let header = GBox::new(Orientation::Horizontal, 6);
    let title = Label::new(Some("Markers"));
    title.add_css_class("browser-header");
    title.set_hexpand(true);
    title.set_xalign(0.0);
    header.append(&title);

    let status_label = Label::new(Some("0 markers"));
    status_label.set_xalign(1.0);
    header.append(&status_label);

    let add_btn = Button::with_label("+ Add");
    add_btn.add_css_class("small-btn");
    add_btn.set_tooltip_text(Some("Add marker at playhead (M)"));
    header.append(&add_btn);

    root.append(&header);

    // ── Column headers row ──
    let col_header = GBox::new(Orientation::Horizontal, 6);
    col_header.set_margin_start(4);
    col_header.set_margin_end(4);
    col_header.set_margin_top(2);
    let col_color = Label::new(Some(""));
    col_color.set_size_request(22, -1);
    col_header.append(&col_color);
    let col_name = Label::new(Some("Name"));
    col_name.set_hexpand(false);
    col_name.set_xalign(0.0);
    col_name.add_css_class("dim-label");
    col_header.append(&col_name);
    let col_time = Label::new(Some("Time"));
    col_time.set_width_chars(12);
    col_time.set_xalign(0.0);
    col_time.add_css_class("dim-label");
    col_header.append(&col_time);
    let col_notes = Label::new(Some("Notes"));
    col_notes.set_hexpand(true);
    col_notes.set_xalign(0.0);
    col_notes.add_css_class("dim-label");
    col_header.append(&col_notes);
    let col_del = Label::new(Some(""));
    col_del.set_size_request(30, -1);
    col_header.append(&col_del);
    root.append(&col_header);

    // ── Scrollable list ──
    let list_box = ListBox::new();
    list_box.set_selection_mode(gtk::SelectionMode::Single);
    list_box.add_css_class("boxed-list");

    let scroller = ScrolledWindow::new();
    scroller.set_vexpand(true);
    scroller.set_hexpand(true);
    scroller.set_child(Some(&list_box));
    root.append(&scroller);

    let view = Rc::new(MarkersPanelView {
        list_box: list_box.clone(),
        status_label,
        project: project.clone(),
        timeline_state: timeline_state.clone(),
        on_project_changed: on_project_changed.clone(),
        on_seek: on_seek.clone(),
    });

    // ── Double-click row → seek ──
    {
        let project_ref = project.clone();
        let seek = on_seek.clone();
        list_box.connect_row_activated(move |_, row| {
            let marker_id: Option<String> =
                unsafe { row.data::<String>("marker-id").map(|p| p.as_ref().clone()) };
            if let Some(mid) = marker_id {
                let p = project_ref.borrow();
                if let Some(m) = p.markers.iter().find(|m| m.id == mid) {
                    seek(m.position_ns);
                }
            }
        });
    }

    // ── Add button ──
    {
        let project_ref = project.clone();
        let ts = timeline_state.clone();
        let opc = on_project_changed.clone();
        add_btn.connect_clicked(move |_| {
            let pos = {
                let st = ts.borrow();
                st.playhead_ns
            };
            let marker = Marker::new(pos, "Marker");
            {
                let mut st = ts.borrow_mut();
                let mut p = project_ref.borrow_mut();
                st.history
                    .execute(Box::new(crate::undo::AddMarkerCommand { marker }), &mut p);
            }
            opc();
        });
    }

    // Initial build
    {
        let p = project.borrow();
        view.rebuild_from_project(&p);
    }

    (root, view)
}

fn unpack_rgba(packed: u32) -> (f64, f64, f64, f64) {
    let r = ((packed >> 24) & 0xFF) as f64 / 255.0;
    let g = ((packed >> 16) & 0xFF) as f64 / 255.0;
    let b = ((packed >> 8) & 0xFF) as f64 / 255.0;
    let a = (packed & 0xFF) as f64 / 255.0;
    (r, g, b, a)
}
