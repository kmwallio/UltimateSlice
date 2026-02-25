use gtk4::prelude::*;
use gtk4::{self as gtk, Box as GBox, Button, Entry, Label, Orientation, Scale, Separator};
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
    /// Which clip is currently shown (kept in sync by update())
    pub selected_clip_id: Rc<RefCell<Option<String>>>,
    // Color correction sliders
    pub brightness_slider: Scale,
    pub contrast_slider: Scale,
    pub saturation_slider: Scale,
    /// Set true while update() runs to suppress feedback from slider signals
    pub updating: Rc<RefCell<bool>>,
}

impl InspectorView {
    /// Refresh all fields to show the given clip, or clear if None.
    pub fn update(&self, project: &Project, clip_id: Option<&str>) {
        let clip = clip_id.and_then(|id| {
            project.tracks.iter()
                .flat_map(|t| t.clips.iter())
                .find(|c| c.id == id)
        });

        // Suppress slider value-changed signals while we set values programmatically
        *self.updating.borrow_mut() = true;
        *self.selected_clip_id.borrow_mut() = clip_id.map(|s| s.to_owned());

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
                self.brightness_slider.set_value(c.brightness as f64);
                self.contrast_slider.set_value(c.contrast as f64);
                self.saturation_slider.set_value(c.saturation as f64);
            }
            None => {
                self.name_entry.set_text("");
                for l in [&self.path_value, &self.in_value, &self.out_value,
                           &self.dur_value, &self.pos_value] {
                    l.set_text("—");
                }
                self.brightness_slider.set_value(0.0);
                self.contrast_slider.set_value(1.0);
                self.saturation_slider.set_value(1.0);
            }
        }
        *self.updating.borrow_mut() = false;
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

    // Color correction section
    let color_title = Label::new(Some("Color"));
    color_title.set_halign(gtk::Align::Start);
    color_title.add_css_class("browser-header");
    vbox.append(&color_title);

    row_label(&vbox, "Brightness");
    let brightness_slider = Scale::with_range(Orientation::Horizontal, -1.0, 1.0, 0.01);
    brightness_slider.set_value(0.0);
    brightness_slider.set_draw_value(true);
    brightness_slider.set_digits(2);
    brightness_slider.add_mark(0.0, gtk4::PositionType::Bottom, None);
    vbox.append(&brightness_slider);

    row_label(&vbox, "Contrast");
    let contrast_slider = Scale::with_range(Orientation::Horizontal, 0.0, 2.0, 0.01);
    contrast_slider.set_value(1.0);
    contrast_slider.set_draw_value(true);
    contrast_slider.set_digits(2);
    contrast_slider.add_mark(1.0, gtk4::PositionType::Bottom, None);
    vbox.append(&contrast_slider);

    row_label(&vbox, "Saturation");
    let saturation_slider = Scale::with_range(Orientation::Horizontal, 0.0, 2.0, 0.01);
    saturation_slider.set_value(1.0);
    saturation_slider.set_draw_value(true);
    saturation_slider.set_digits(2);
    saturation_slider.add_mark(1.0, gtk4::PositionType::Bottom, None);
    vbox.append(&saturation_slider);

    vbox.append(&Separator::new(Orientation::Horizontal));

    // Apply name button
    let apply_btn = Button::with_label("Apply Name");
    vbox.append(&apply_btn);

    // Shared state: which clip is selected (set from outside via InspectorView::update())
    let selected_clip_id: Rc<RefCell<Option<String>>> = Rc::new(RefCell::new(None));
    let updating: Rc<RefCell<bool>> = Rc::new(RefCell::new(false));

    let on_clip_updated = Rc::new(on_clip_updated);

    // Apply name button
    {
        let project = project.clone();
        let selected_clip_id = selected_clip_id.clone();
        let name_entry_cb = name_entry.clone();
        let on_clip_updated = on_clip_updated.clone();

        apply_btn.connect_clicked(move |_| {
            let new_name = name_entry_cb.text().to_string();
            if new_name.is_empty() { return; }
            let id = selected_clip_id.borrow().clone();
            if let Some(ref clip_id) = id {
                {
                    let mut proj = project.borrow_mut();
                    for track in &mut proj.tracks {
                        if let Some(clip) = track.clips.iter_mut().find(|c| &c.id == clip_id) {
                            clip.label = new_name.clone();
                            proj.dirty = true;
                            break;
                        }
                    }
                }
                on_clip_updated();
            }
        });
    }

    // Helper: connect a color slider to update the clip and notify
    fn connect_color_slider(
        slider: &Scale,
        project: Rc<RefCell<Project>>,
        selected_clip_id: Rc<RefCell<Option<String>>>,
        updating: Rc<RefCell<bool>>,
        on_clip_updated: Rc<dyn Fn()>,
        apply: fn(&mut crate::model::clip::Clip, f32),
    ) {
        slider.connect_value_changed(move |s| {
            if *updating.borrow() { return; }
            let val = s.value() as f32;
            let id = selected_clip_id.borrow().clone();
            if let Some(ref clip_id) = id {
                {
                    let mut proj = project.borrow_mut();
                    for track in &mut proj.tracks {
                        if let Some(clip) = track.clips.iter_mut().find(|c| &c.id == clip_id) {
                            apply(clip, val);
                            proj.dirty = true;
                            break;
                        }
                    }
                }
                on_clip_updated();
            }
        });
    }

    connect_color_slider(
        &brightness_slider, project.clone(), selected_clip_id.clone(),
        updating.clone(), on_clip_updated.clone(),
        |clip, v| clip.brightness = v,
    );
    connect_color_slider(
        &contrast_slider, project.clone(), selected_clip_id.clone(),
        updating.clone(), on_clip_updated.clone(),
        |clip, v| clip.contrast = v,
    );
    connect_color_slider(
        &saturation_slider, project.clone(), selected_clip_id.clone(),
        updating.clone(), on_clip_updated.clone(),
        |clip, v| clip.saturation = v,
    );

    let view = Rc::new(InspectorView {
        name_entry,
        path_value,
        in_value,
        out_value,
        dur_value,
        pos_value,
        selected_clip_id,
        brightness_slider,
        contrast_slider,
        saturation_slider,
        updating,
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
