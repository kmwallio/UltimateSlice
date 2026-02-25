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
    // Denoise / sharpness sliders
    pub denoise_slider: Scale,
    pub sharpness_slider: Scale,
    // Audio sliders
    pub volume_slider: Scale,
    pub pan_slider: Scale,
    // Transform sliders/controls
    pub crop_left_slider: Scale,
    pub crop_right_slider: Scale,
    pub crop_top_slider: Scale,
    pub crop_bottom_slider: Scale,
    pub rotate_combo: gtk4::ComboBoxText,
    pub flip_h_btn: gtk4::ToggleButton,
    pub flip_v_btn: gtk4::ToggleButton,
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
                self.denoise_slider.set_value(c.denoise as f64);
                self.sharpness_slider.set_value(c.sharpness as f64);
                self.volume_slider.set_value(c.volume as f64);
                self.pan_slider.set_value(c.pan as f64);
                self.crop_left_slider.set_value(c.crop_left as f64);
                self.crop_right_slider.set_value(c.crop_right as f64);
                self.crop_top_slider.set_value(c.crop_top as f64);
                self.crop_bottom_slider.set_value(c.crop_bottom as f64);
                self.rotate_combo.set_active_id(Some(&c.rotate.to_string()));
                self.flip_h_btn.set_active(c.flip_h);
                self.flip_v_btn.set_active(c.flip_v);
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
                self.denoise_slider.set_value(0.0);
                self.sharpness_slider.set_value(0.0);
                self.volume_slider.set_value(1.0);
                self.pan_slider.set_value(0.0);
                self.crop_left_slider.set_value(0.0);
                self.crop_right_slider.set_value(0.0);
                self.crop_top_slider.set_value(0.0);
                self.crop_bottom_slider.set_value(0.0);
                self.rotate_combo.set_active_id(Some("0"));
                self.flip_h_btn.set_active(false);
                self.flip_v_btn.set_active(false);
            }
        }
        *self.updating.borrow_mut() = false;
    }
}

/// Build the inspector panel.
/// Returns `(widget, InspectorView)` — keep `InspectorView` and call `.update()` on selection changes.
///
/// - `on_clip_changed`: fired when the clip name is applied (triggers full project-changed cycle).
/// - `on_color_changed`: fired on every color/effects slider movement with
///   `(brightness, contrast, saturation, denoise, sharpness)`;
///   should update the program player's video filter elements directly without a full pipeline reload.
/// - `on_audio_changed`: fired on every audio slider movement with `(volume, pan)`.
pub fn build_inspector(
    project: Rc<RefCell<Project>>,
    on_clip_changed: impl Fn() + 'static,
    on_color_changed: impl Fn(f32, f32, f32, f32, f32) + 'static,
    on_audio_changed: impl Fn(f32, f32) + 'static,
    on_transform_changed: impl Fn(i32, i32, i32, i32, i32, bool, bool) + 'static,
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

    // Denoise / Sharpness section
    let ds_title = Label::new(Some("Denoise / Sharpness"));
    ds_title.set_halign(gtk::Align::Start);
    ds_title.add_css_class("browser-header");
    vbox.append(&ds_title);

    row_label(&vbox, "Denoise");
    let denoise_slider = Scale::with_range(Orientation::Horizontal, 0.0, 1.0, 0.01);
    denoise_slider.set_value(0.0);
    denoise_slider.set_draw_value(true);
    denoise_slider.set_digits(2);
    denoise_slider.add_mark(0.0, gtk4::PositionType::Bottom, None);
    vbox.append(&denoise_slider);

    row_label(&vbox, "Sharpness");
    let sharpness_slider = Scale::with_range(Orientation::Horizontal, -1.0, 1.0, 0.01);
    sharpness_slider.set_value(0.0);
    sharpness_slider.set_draw_value(true);
    sharpness_slider.set_digits(2);
    sharpness_slider.add_mark(0.0, gtk4::PositionType::Bottom, None);
    vbox.append(&sharpness_slider);

    // Audio section
    vbox.append(&Separator::new(Orientation::Horizontal));
    let audio_title = Label::new(Some("Audio"));
    audio_title.set_halign(gtk::Align::Start);
    audio_title.add_css_class("browser-header");
    vbox.append(&audio_title);

    row_label(&vbox, "Volume");
    let volume_slider = Scale::with_range(Orientation::Horizontal, 0.0, 2.0, 0.01);
    volume_slider.set_value(1.0);
    volume_slider.set_draw_value(true);
    volume_slider.set_digits(2);
    volume_slider.add_mark(1.0, gtk4::PositionType::Bottom, None);
    vbox.append(&volume_slider);

    row_label(&vbox, "Pan");
    let pan_slider = Scale::with_range(Orientation::Horizontal, -1.0, 1.0, 0.01);
    pan_slider.set_value(0.0);
    pan_slider.set_draw_value(true);
    pan_slider.set_digits(2);
    pan_slider.add_mark(0.0, gtk4::PositionType::Bottom, None);
    vbox.append(&pan_slider);

    // Transform section
    vbox.append(&Separator::new(Orientation::Horizontal));
    let transform_title = Label::new(Some("Transform"));
    transform_title.set_halign(gtk::Align::Start);
    transform_title.add_css_class("browser-header");
    vbox.append(&transform_title);

    row_label(&vbox, "Crop Left");
    let crop_left_slider = Scale::with_range(Orientation::Horizontal, 0.0, 500.0, 2.0);
    crop_left_slider.set_value(0.0);
    crop_left_slider.set_draw_value(true);
    crop_left_slider.set_digits(0);
    vbox.append(&crop_left_slider);

    row_label(&vbox, "Crop Right");
    let crop_right_slider = Scale::with_range(Orientation::Horizontal, 0.0, 500.0, 2.0);
    crop_right_slider.set_value(0.0);
    crop_right_slider.set_draw_value(true);
    crop_right_slider.set_digits(0);
    vbox.append(&crop_right_slider);

    row_label(&vbox, "Crop Top");
    let crop_top_slider = Scale::with_range(Orientation::Horizontal, 0.0, 500.0, 2.0);
    crop_top_slider.set_value(0.0);
    crop_top_slider.set_draw_value(true);
    crop_top_slider.set_digits(0);
    vbox.append(&crop_top_slider);

    row_label(&vbox, "Crop Bottom");
    let crop_bottom_slider = Scale::with_range(Orientation::Horizontal, 0.0, 500.0, 2.0);
    crop_bottom_slider.set_value(0.0);
    crop_bottom_slider.set_draw_value(true);
    crop_bottom_slider.set_digits(0);
    vbox.append(&crop_bottom_slider);

    row_label(&vbox, "Rotate");
    let rotate_combo = gtk4::ComboBoxText::new();
    rotate_combo.append(Some("0"),   "0°");
    rotate_combo.append(Some("90"),  "90° CW");
    rotate_combo.append(Some("180"), "180°");
    rotate_combo.append(Some("270"), "270° CW");
    rotate_combo.set_active_id(Some("0"));
    vbox.append(&rotate_combo);

    row_label(&vbox, "Flip");
    let flip_row = GBox::new(Orientation::Horizontal, 8);
    let flip_h_btn = gtk4::ToggleButton::with_label("Flip H");
    let flip_v_btn = gtk4::ToggleButton::with_label("Flip V");
    flip_row.append(&flip_h_btn);
    flip_row.append(&flip_v_btn);
    vbox.append(&flip_row);

    vbox.append(&Separator::new(Orientation::Horizontal));

    // Apply name button
    let apply_btn = Button::with_label("Apply Name");
    vbox.append(&apply_btn);
    let selected_clip_id: Rc<RefCell<Option<String>>> = Rc::new(RefCell::new(None));
    let updating: Rc<RefCell<bool>> = Rc::new(RefCell::new(false));

    let on_clip_changed = Rc::new(on_clip_changed);
    let on_color_changed: Rc<dyn Fn(f32, f32, f32, f32, f32)> = Rc::new(on_color_changed);
    let on_audio_changed: Rc<dyn Fn(f32, f32)> = Rc::new(on_audio_changed);
    let on_transform_changed: Rc<dyn Fn(i32, i32, i32, i32, i32, bool, bool)> = Rc::new(on_transform_changed);

    // Apply name button — triggers full on_project_changed
    {
        let project = project.clone();
        let selected_clip_id = selected_clip_id.clone();
        let name_entry_cb = name_entry.clone();
        let on_clip_changed = on_clip_changed.clone();

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
                on_clip_changed();
            }
        });
    }

    // Helper: connect an effects slider — updates the model field then fires on_color_changed
    // with all five current values so the program player can update its filters directly.
    fn connect_color_slider(
        slider: &Scale,
        project: Rc<RefCell<Project>>,
        selected_clip_id: Rc<RefCell<Option<String>>>,
        updating: Rc<RefCell<bool>>,
        on_color_changed: Rc<dyn Fn(f32, f32, f32, f32, f32)>,
        brightness_slider: Scale,
        contrast_slider: Scale,
        saturation_slider: Scale,
        denoise_slider: Scale,
        sharpness_slider: Scale,
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
                // Fire lightweight effects callback with all five current values
                let b   = brightness_slider.value() as f32;
                let c   = contrast_slider.value() as f32;
                let sat = saturation_slider.value() as f32;
                let d   = denoise_slider.value() as f32;
                let sh  = sharpness_slider.value() as f32;
                on_color_changed(b, c, sat, d, sh);
            }
        });
    }

    connect_color_slider(
        &brightness_slider, project.clone(), selected_clip_id.clone(),
        updating.clone(), on_color_changed.clone(),
        brightness_slider.clone(), contrast_slider.clone(), saturation_slider.clone(),
        denoise_slider.clone(), sharpness_slider.clone(),
        |clip, v| clip.brightness = v,
    );
    connect_color_slider(
        &contrast_slider, project.clone(), selected_clip_id.clone(),
        updating.clone(), on_color_changed.clone(),
        brightness_slider.clone(), contrast_slider.clone(), saturation_slider.clone(),
        denoise_slider.clone(), sharpness_slider.clone(),
        |clip, v| clip.contrast = v,
    );
    connect_color_slider(
        &saturation_slider, project.clone(), selected_clip_id.clone(),
        updating.clone(), on_color_changed.clone(),
        brightness_slider.clone(), contrast_slider.clone(), saturation_slider.clone(),
        denoise_slider.clone(), sharpness_slider.clone(),
        |clip, v| clip.saturation = v,
    );
    connect_color_slider(
        &denoise_slider, project.clone(), selected_clip_id.clone(),
        updating.clone(), on_color_changed.clone(),
        brightness_slider.clone(), contrast_slider.clone(), saturation_slider.clone(),
        denoise_slider.clone(), sharpness_slider.clone(),
        |clip, v| clip.denoise = v,
    );
    connect_color_slider(
        &sharpness_slider, project.clone(), selected_clip_id.clone(),
        updating.clone(), on_color_changed.clone(),
        brightness_slider.clone(), contrast_slider.clone(), saturation_slider.clone(),
        denoise_slider.clone(), sharpness_slider.clone(),
        |clip, v| clip.sharpness = v,
    );

    // Wire audio sliders
    {
        let project = project.clone();
        let selected_clip_id = selected_clip_id.clone();
        let updating = updating.clone();
        let on_audio_changed = on_audio_changed.clone();
        let volume_slider_cb = volume_slider.clone();
        let pan_slider_cb = pan_slider.clone();
        volume_slider.connect_value_changed(move |s| {
            if *updating.borrow() { return; }
            let val = s.value() as f32;
            let id = selected_clip_id.borrow().clone();
            if let Some(ref clip_id) = id {
                {
                    let mut proj = project.borrow_mut();
                    for track in &mut proj.tracks {
                        if let Some(clip) = track.clips.iter_mut().find(|c| &c.id == clip_id) {
                            clip.volume = val;
                            proj.dirty = true;
                            break;
                        }
                    }
                }
                on_audio_changed(volume_slider_cb.value() as f32, pan_slider_cb.value() as f32);
            }
        });
    }
    {
        let project = project.clone();
        let selected_clip_id = selected_clip_id.clone();
        let updating = updating.clone();
        let on_audio_changed = on_audio_changed.clone();
        let volume_slider_cb = volume_slider.clone();
        let pan_slider_cb = pan_slider.clone();
        pan_slider.connect_value_changed(move |s| {
            if *updating.borrow() { return; }
            let val = s.value() as f32;
            let id = selected_clip_id.borrow().clone();
            if let Some(ref clip_id) = id {
                {
                    let mut proj = project.borrow_mut();
                    for track in &mut proj.tracks {
                        if let Some(clip) = track.clips.iter_mut().find(|c| &c.id == clip_id) {
                            clip.pan = val;
                            proj.dirty = true;
                            break;
                        }
                    }
                }
                on_audio_changed(volume_slider_cb.value() as f32, pan_slider_cb.value() as f32);
            }
        });
    }

    // Wire transform sliders and buttons
    // Helper macro-style: a closure that reads all transform values and fires the callback
    fn connect_transform_slider(
        slider: &Scale,
        project: Rc<RefCell<Project>>,
        selected_clip_id: Rc<RefCell<Option<String>>>,
        updating: Rc<RefCell<bool>>,
        on_transform_changed: Rc<dyn Fn(i32, i32, i32, i32, i32, bool, bool)>,
        crop_left_s: Scale,
        crop_right_s: Scale,
        crop_top_s: Scale,
        crop_bottom_s: Scale,
        rotate_c: gtk4::ComboBoxText,
        flip_h_b: gtk4::ToggleButton,
        flip_v_b: gtk4::ToggleButton,
        apply: fn(&mut crate::model::clip::Clip, i32),
    ) {
        slider.connect_value_changed(move |s| {
            if *updating.borrow() { return; }
            let val = s.value() as i32;
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
                let cl = crop_left_s.value() as i32;
                let cr = crop_right_s.value() as i32;
                let ct = crop_top_s.value() as i32;
                let cb = crop_bottom_s.value() as i32;
                let rot = rotate_c.active_id()
                    .and_then(|id| id.parse::<i32>().ok())
                    .unwrap_or(0);
                let fh = flip_h_b.is_active();
                let fv = flip_v_b.is_active();
                on_transform_changed(cl, cr, ct, cb, rot, fh, fv);
            }
        });
    }

    connect_transform_slider(
        &crop_left_slider, project.clone(), selected_clip_id.clone(),
        updating.clone(), on_transform_changed.clone(),
        crop_left_slider.clone(), crop_right_slider.clone(),
        crop_top_slider.clone(), crop_bottom_slider.clone(),
        rotate_combo.clone(), flip_h_btn.clone(), flip_v_btn.clone(),
        |clip, v| clip.crop_left = v,
    );
    connect_transform_slider(
        &crop_right_slider, project.clone(), selected_clip_id.clone(),
        updating.clone(), on_transform_changed.clone(),
        crop_left_slider.clone(), crop_right_slider.clone(),
        crop_top_slider.clone(), crop_bottom_slider.clone(),
        rotate_combo.clone(), flip_h_btn.clone(), flip_v_btn.clone(),
        |clip, v| clip.crop_right = v,
    );
    connect_transform_slider(
        &crop_top_slider, project.clone(), selected_clip_id.clone(),
        updating.clone(), on_transform_changed.clone(),
        crop_left_slider.clone(), crop_right_slider.clone(),
        crop_top_slider.clone(), crop_bottom_slider.clone(),
        rotate_combo.clone(), flip_h_btn.clone(), flip_v_btn.clone(),
        |clip, v| clip.crop_top = v,
    );
    connect_transform_slider(
        &crop_bottom_slider, project.clone(), selected_clip_id.clone(),
        updating.clone(), on_transform_changed.clone(),
        crop_left_slider.clone(), crop_right_slider.clone(),
        crop_top_slider.clone(), crop_bottom_slider.clone(),
        rotate_combo.clone(), flip_h_btn.clone(), flip_v_btn.clone(),
        |clip, v| clip.crop_bottom = v,
    );

    // Wire rotate combo
    {
        let project = project.clone();
        let selected_clip_id = selected_clip_id.clone();
        let updating = updating.clone();
        let on_transform_changed = on_transform_changed.clone();
        let crop_left_s = crop_left_slider.clone();
        let crop_right_s = crop_right_slider.clone();
        let crop_top_s = crop_top_slider.clone();
        let crop_bottom_s = crop_bottom_slider.clone();
        let flip_h_b = flip_h_btn.clone();
        let flip_v_b = flip_v_btn.clone();
        rotate_combo.connect_changed(move |combo| {
            if *updating.borrow() { return; }
            let rot = combo.active_id()
                .and_then(|id| id.parse::<i32>().ok())
                .unwrap_or(0);
            let id = selected_clip_id.borrow().clone();
            if let Some(ref clip_id) = id {
                {
                    let mut proj = project.borrow_mut();
                    for track in &mut proj.tracks {
                        if let Some(clip) = track.clips.iter_mut().find(|c| &c.id == clip_id) {
                            clip.rotate = rot;
                            proj.dirty = true;
                            break;
                        }
                    }
                }
                let cl = crop_left_s.value() as i32;
                let cr = crop_right_s.value() as i32;
                let ct = crop_top_s.value() as i32;
                let cb = crop_bottom_s.value() as i32;
                let fh = flip_h_b.is_active();
                let fv = flip_v_b.is_active();
                on_transform_changed(cl, cr, ct, cb, rot, fh, fv);
            }
        });
    }

    // Wire flip buttons
    {
        let project = project.clone();
        let selected_clip_id = selected_clip_id.clone();
        let updating = updating.clone();
        let on_transform_changed = on_transform_changed.clone();
        let crop_left_s = crop_left_slider.clone();
        let crop_right_s = crop_right_slider.clone();
        let crop_top_s = crop_top_slider.clone();
        let crop_bottom_s = crop_bottom_slider.clone();
        let rotate_c = rotate_combo.clone();
        let flip_v_b = flip_v_btn.clone();
        flip_h_btn.connect_toggled(move |btn| {
            if *updating.borrow() { return; }
            let fh = btn.is_active();
            let id = selected_clip_id.borrow().clone();
            if let Some(ref clip_id) = id {
                {
                    let mut proj = project.borrow_mut();
                    for track in &mut proj.tracks {
                        if let Some(clip) = track.clips.iter_mut().find(|c| &c.id == clip_id) {
                            clip.flip_h = fh;
                            proj.dirty = true;
                            break;
                        }
                    }
                }
                let cl = crop_left_s.value() as i32;
                let cr = crop_right_s.value() as i32;
                let ct = crop_top_s.value() as i32;
                let cb = crop_bottom_s.value() as i32;
                let rot = rotate_c.active_id()
                    .and_then(|id| id.parse::<i32>().ok())
                    .unwrap_or(0);
                let fv = flip_v_b.is_active();
                on_transform_changed(cl, cr, ct, cb, rot, fh, fv);
            }
        });
    }
    {
        let project = project.clone();
        let selected_clip_id = selected_clip_id.clone();
        let updating = updating.clone();
        let on_transform_changed = on_transform_changed.clone();
        let crop_left_s = crop_left_slider.clone();
        let crop_right_s = crop_right_slider.clone();
        let crop_top_s = crop_top_slider.clone();
        let crop_bottom_s = crop_bottom_slider.clone();
        let rotate_c = rotate_combo.clone();
        let flip_h_b = flip_h_btn.clone();
        flip_v_btn.connect_toggled(move |btn| {
            if *updating.borrow() { return; }
            let fv = btn.is_active();
            let id = selected_clip_id.borrow().clone();
            if let Some(ref clip_id) = id {
                {
                    let mut proj = project.borrow_mut();
                    for track in &mut proj.tracks {
                        if let Some(clip) = track.clips.iter_mut().find(|c| &c.id == clip_id) {
                            clip.flip_v = fv;
                            proj.dirty = true;
                            break;
                        }
                    }
                }
                let cl = crop_left_s.value() as i32;
                let cr = crop_right_s.value() as i32;
                let ct = crop_top_s.value() as i32;
                let cb = crop_bottom_s.value() as i32;
                let rot = rotate_c.active_id()
                    .and_then(|id| id.parse::<i32>().ok())
                    .unwrap_or(0);
                let fh = flip_h_b.is_active();
                on_transform_changed(cl, cr, ct, cb, rot, fh, fv);
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
        selected_clip_id,
        brightness_slider,
        contrast_slider,
        saturation_slider,
        denoise_slider,
        sharpness_slider,
        volume_slider,
        pan_slider,
        crop_left_slider,
        crop_right_slider,
        crop_top_slider,
        crop_bottom_slider,
        rotate_combo,
        flip_h_btn,
        flip_v_btn,
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
