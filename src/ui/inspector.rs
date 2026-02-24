use gtk4::prelude::*;
use gtk4::{self as gtk, Box as GBox, Button, Entry, Label, Orientation, Separator};
use std::cell::RefCell;
use std::rc::Rc;
use crate::model::project::Project;

/// Holds references to the inspector's display labels so they can be
/// updated from outside without widget traversal.
#[derive(Clone)]
pub struct InspectorView {
    pub name_entry: Entry,
    pub path_value: Label,
    pub in_value: Label,
    pub out_value: Label,
    pub dur_value: Label,
    pub pos_value: Label,
}

impl InspectorView {
    /// Refresh all fields to show the given clip, or clear if None.
    pub fn update(&self, project: &Project, clip_id: Option<&str>) {
        let clip = clip_id.and_then(|id| {
            project.tracks.iter()
                .flat_map(|t| t.clips.iter())
                .find(|c| c.id == id)
        });

        match clip {
            Some(c) => {
                self.name_entry.set_text(&c.label);
                self.path_value.set_text(
                    std::path::Path::new(&c.source_path)
                        .file_name().and_then(|n| n.to_str())
                        .unwrap_or(&c.source_path),
                );
                self.in_value.set_text(&ns_to_timecode(c.source_in));
                self.out_value.set_text(&ns_to_timecode(c.source_out));
                self.dur_value.set_text(&ns_to_timecode(c.duration()));
                self.pos_value.set_text(&ns_to_timecode(c.timeline_start));
            }
            None => {
                self.name_entry.set_text("");
                for l in [&self.path_value, &self.in_value, &self.out_value,
                           &self.dur_value, &self.pos_value] {
                    l.set_text("—");
                }
            }
        }
    }
}

/// Build the inspector panel.
/// Returns `(widget, InspectorView)` — keep `InspectorView` and call `.update()` on selection changes.
pub fn build_inspector(
    project: Rc<RefCell<Project>>,
    on_clip_updated: impl Fn() + 'static,
) -> (GBox, Rc<InspectorView>) {
    let vbox = GBox::new(Orientation::Vertical, 8);
    vbox.set_width_request(200);
    vbox.set_margin_start(8);
    vbox.set_margin_end(8);
    vbox.set_margin_top(8);

    let title = Label::new(Some("Inspector"));
    title.add_css_class("browser-header");
    vbox.append(&title);

    vbox.append(&Separator::new(Orientation::Horizontal));

    // Clip name
    row_label(&vbox, "Name");
    let name_entry = Entry::new();
    name_entry.set_placeholder_text(Some("Clip name"));
    vbox.append(&name_entry);

    // Source path (read-only)
    row_label(&vbox, "Source");
    let path_value = Label::new(Some("—"));
    path_value.set_halign(gtk::Align::Start);
    path_value.set_ellipsize(gtk4::pango::EllipsizeMode::Start);
    path_value.set_max_width_chars(22);
    path_value.add_css_class("clip-path");
    vbox.append(&path_value);

    vbox.append(&Separator::new(Orientation::Horizontal));

    // Timecode fields
    row_label(&vbox, "In");
    let in_value = value_label("—");
    vbox.append(&in_value);

    row_label(&vbox, "Out");
    let out_value = value_label("—");
    vbox.append(&out_value);

    row_label(&vbox, "Duration");
    let dur_value = value_label("—");
    vbox.append(&dur_value);

    row_label(&vbox, "Timeline Start");
    let pos_value = value_label("—");
    vbox.append(&pos_value);

    vbox.append(&Separator::new(Orientation::Horizontal));

    // Apply name button
    let apply_btn = Button::with_label("Apply Name");
    vbox.append(&apply_btn);

    // Shared state: which clip is selected (set from outside)
    let selected_clip_id: Rc<RefCell<Option<String>>> = Rc::new(RefCell::new(None));

    {
        let project = project.clone();
        let selected_clip_id = selected_clip_id.clone();
        let name_entry_cb = name_entry.clone();
        let on_clip_updated = Rc::new(on_clip_updated);

        apply_btn.connect_clicked(move |_| {
            let new_name = name_entry_cb.text().to_string();
            if new_name.is_empty() { return; }
            let id = selected_clip_id.borrow().clone();
            if let Some(ref clip_id) = id {
                let mut proj = project.borrow_mut();
                for track in &mut proj.tracks {
                    if let Some(clip) = track.clips.iter_mut().find(|c| &c.id == clip_id) {
                        clip.label = new_name.clone();
                        proj.dirty = true;
                        break;
                    }
                }
                on_clip_updated();
            }
        });
    }

    let view = Rc::new(InspectorView {
        name_entry,
        path_value,
        in_value,
        out_value,
        dur_value,
        pos_value,
    });

    (vbox, view)
}

fn row_label(parent: &GBox, text: &str) {
    let l = Label::new(Some(text));
    l.set_halign(gtk4::Align::Start);
    l.add_css_class("clip-path");
    parent.append(&l);
}

fn value_label(text: &str) -> Label {
    let l = Label::new(Some(text));
    l.set_halign(gtk4::Align::Start);
    l
}

fn ns_to_timecode(ns: u64) -> String {
    let total_frames = ns / (1_000_000_000 / 24);
    let h = total_frames / (24 * 3600);
    let m = (total_frames % (24 * 3600)) / (24 * 60);
    let s = (total_frames % (24 * 60)) / 24;
    let f = total_frames % 24;
    if h > 0 {
        format!("{h}:{m:02}:{s:02}:{f:02}")
    } else {
        format!("{m}:{s:02}:{f:02}")
    }
}
