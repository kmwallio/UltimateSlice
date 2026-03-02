use crate::model::project::Project;
use gio;
use gtk4::prelude::*;
use gtk4::{
    self as gtk, Box as GBox, Button, Entry, Expander, Label, Orientation, Scale, Separator,
};
use std::cell::RefCell;
use std::rc::Rc;

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
    // Grading sliders
    pub shadows_slider: Scale,
    pub midtones_slider: Scale,
    pub highlights_slider: Scale,
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
    pub scale_slider: Scale,
    pub opacity_slider: Scale,
    pub position_x_slider: Scale,
    pub position_y_slider: Scale,
    // Title / text overlay
    pub title_entry: Entry,
    pub title_x_slider: Scale,
    pub title_y_slider: Scale,
    // Speed
    pub speed_slider: Scale,
    // LUT (color grading)
    pub lut_path_label: Label,
    pub lut_clear_btn: Button,
    /// Set true while update() runs to suppress feedback from slider signals
    pub updating: Rc<RefCell<bool>>,
    // Section containers for show/hide per clip kind
    pub content_box: GBox,
    pub empty_state_label: Label,
    pub color_section: GBox,
    pub audio_section: GBox,
    pub transform_section: GBox,
    pub title_section_box: GBox,
    pub speed_section_box: GBox,
    pub lut_section_box: GBox,
}

impl InspectorView {
    /// Refresh all fields to show the given clip, or clear if None.
    pub fn update(&self, project: &Project, clip_id: Option<&str>) {
        use crate::model::clip::ClipKind;

        let clip = clip_id.and_then(|id| {
            project
                .tracks
                .iter()
                .flat_map(|t| t.clips.iter())
                .find(|c| c.id == id)
        });

        // Show content only when a clip is selected; otherwise show empty-state guidance.
        let has_clip = clip_id.is_some();
        self.content_box.set_sensitive(has_clip);
        self.content_box.set_visible(has_clip);
        self.empty_state_label.set_visible(!has_clip);

        // Suppress slider value-changed signals while we set values programmatically
        *self.updating.borrow_mut() = true;
        *self.selected_clip_id.borrow_mut() = clip_id.map(|s| s.to_owned());

        match clip {
            Some(c) => {
                // Show/hide sections based on clip kind
                let is_video = c.kind == ClipKind::Video;
                let is_audio = c.kind == ClipKind::Audio;
                let is_image = c.kind == ClipKind::Image;
                self.color_section.set_visible(is_video || is_image);
                self.audio_section.set_visible(is_video || is_audio);
                self.transform_section.set_visible(is_video || is_image);
                self.title_section_box.set_visible(is_video || is_image);
                self.speed_section_box.set_visible(true);
                self.lut_section_box.set_visible(is_video || is_image);

                self.name_entry.set_text(&c.label);
                self.path_value.set_text(
                    std::path::Path::new(&c.source_path)
                        .file_name()
                        .and_then(|n| n.to_str())
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
                self.shadows_slider.set_value(c.shadows as f64);
                self.midtones_slider.set_value(c.midtones as f64);
                self.highlights_slider.set_value(c.highlights as f64);
                self.volume_slider.set_value(c.volume as f64);
                self.pan_slider.set_value(c.pan as f64);
                self.crop_left_slider.set_value(c.crop_left as f64);
                self.crop_right_slider.set_value(c.crop_right as f64);
                self.crop_top_slider.set_value(c.crop_top as f64);
                self.crop_bottom_slider.set_value(c.crop_bottom as f64);
                self.rotate_combo.set_active_id(Some(&c.rotate.to_string()));
                self.flip_h_btn.set_active(c.flip_h);
                self.flip_v_btn.set_active(c.flip_v);
                self.scale_slider.set_value(c.scale);
                self.opacity_slider.set_value(c.opacity);
                self.position_x_slider.set_value(c.position_x);
                self.position_y_slider.set_value(c.position_y);
                self.title_entry.set_text(&c.title_text);
                self.title_x_slider.set_value(c.title_x);
                self.title_y_slider.set_value(c.title_y);
                self.speed_slider.set_value(c.speed);
                // LUT
                match &c.lut_path {
                    Some(p) => {
                        let name = std::path::Path::new(p)
                            .file_name()
                            .and_then(|n| n.to_str())
                            .unwrap_or(p.as_str());
                        self.lut_path_label.set_text(name);
                        self.lut_clear_btn.set_sensitive(true);
                    }
                    None => {
                        self.lut_path_label.set_text("None");
                        self.lut_clear_btn.set_sensitive(false);
                    }
                }
            }
            None => {
                self.name_entry.set_text("");
                for l in [
                    &self.path_value,
                    &self.in_value,
                    &self.out_value,
                    &self.dur_value,
                    &self.pos_value,
                ] {
                    l.set_text("—");
                }
                self.brightness_slider.set_value(0.0);
                self.contrast_slider.set_value(1.0);
                self.saturation_slider.set_value(1.0);
                self.denoise_slider.set_value(0.0);
                self.sharpness_slider.set_value(0.0);
                self.shadows_slider.set_value(0.0);
                self.midtones_slider.set_value(0.0);
                self.highlights_slider.set_value(0.0);
                self.volume_slider.set_value(1.0);
                self.pan_slider.set_value(0.0);
                self.crop_left_slider.set_value(0.0);
                self.crop_right_slider.set_value(0.0);
                self.crop_top_slider.set_value(0.0);
                self.crop_bottom_slider.set_value(0.0);
                self.rotate_combo.set_active_id(Some("0"));
                self.flip_h_btn.set_active(false);
                self.flip_v_btn.set_active(false);
                self.scale_slider.set_value(1.0);
                self.opacity_slider.set_value(1.0);
                self.position_x_slider.set_value(0.0);
                self.position_y_slider.set_value(0.0);
                self.title_entry.set_text("");
                self.title_x_slider.set_value(0.5);
                self.title_y_slider.set_value(0.9);
                self.speed_slider.set_value(1.0);
                self.lut_path_label.set_text("None");
                self.lut_clear_btn.set_sensitive(false);
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
///   `(brightness, contrast, saturation, denoise, sharpness, shadows, midtones, highlights)`;
///   should update the program player's video filter elements directly without a full pipeline reload.
/// - `on_audio_changed`: fired on every audio slider movement with `(volume, pan)`.
pub fn build_inspector(
    project: Rc<RefCell<Project>>,
    on_clip_changed: impl Fn() + 'static,
    on_color_changed: impl Fn(f32, f32, f32, f32, f32, f32, f32, f32) + 'static,
    on_audio_changed: impl Fn(f32, f32) + 'static,
    on_transform_changed: impl Fn(i32, i32, i32, i32, i32, bool, bool, f64, f64, f64) + 'static,
    on_title_changed: impl Fn(String, f64, f64) + 'static,
    on_speed_changed: impl Fn(f64) + 'static,
    on_lut_changed: impl Fn(Option<String>) + 'static,
    on_opacity_changed: impl Fn(f64) + 'static,
) -> (GBox, Rc<InspectorView>) {
    let vbox = GBox::new(Orientation::Vertical, 8);
    vbox.set_width_request(260);
    vbox.set_margin_start(8);
    vbox.set_margin_end(8);
    vbox.set_margin_top(8);

    let title = Label::new(Some("Inspector"));
    title.add_css_class("browser-header");
    vbox.append(&title);

    let empty_state_label = Label::new(Some(
        "Select a clip in the timeline to edit its properties.",
    ));
    empty_state_label.set_halign(gtk::Align::Start);
    empty_state_label.set_xalign(0.0);
    empty_state_label.set_wrap(true);
    empty_state_label.add_css_class("panel-empty-state");
    vbox.append(&empty_state_label);

    // content_box holds everything below the header; shown when a clip is selected
    let content_box = GBox::new(Orientation::Vertical, 8);
    content_box.set_sensitive(false);
    content_box.set_visible(false);
    vbox.append(&content_box);

    content_box.append(&Separator::new(Orientation::Horizontal));

    // Clip name
    row_label(&content_box, "Name");
    let name_entry = Entry::new();
    name_entry.set_placeholder_text(Some("Clip name"));
    content_box.append(&name_entry);

    // Source path (read-only)
    row_label(&content_box, "Source");
    let path_value = Label::new(Some("—"));
    path_value.set_halign(gtk::Align::Start);
    path_value.set_ellipsize(gtk4::pango::EllipsizeMode::Start);
    path_value.set_max_width_chars(22);
    path_value.add_css_class("clip-path");
    content_box.append(&path_value);

    content_box.append(&Separator::new(Orientation::Horizontal));

    // Timecode fields
    row_label(&content_box, "In");
    let in_value = value_label("—");
    content_box.append(&in_value);

    row_label(&content_box, "Out");
    let out_value = value_label("—");
    content_box.append(&out_value);

    row_label(&content_box, "Duration");
    let dur_value = value_label("—");
    content_box.append(&dur_value);

    row_label(&content_box, "Timeline Start");
    let pos_value = value_label("—");
    content_box.append(&pos_value);

    // ── Color + Denoise/Sharpness section (Video + Image only) ───────────────
    let color_section = GBox::new(Orientation::Vertical, 8);
    content_box.append(&color_section);

    color_section.append(&Separator::new(Orientation::Horizontal));
    let color_expander = Expander::new(Some("Color & Denoise"));
    color_expander.set_expanded(true);
    color_section.append(&color_expander);
    let color_inner = GBox::new(Orientation::Vertical, 8);
    color_expander.set_child(Some(&color_inner));

    row_label(&color_inner, "Brightness");
    let brightness_slider = Scale::with_range(Orientation::Horizontal, -1.0, 1.0, 0.01);
    brightness_slider.set_value(0.0);
    brightness_slider.set_draw_value(true);
    brightness_slider.set_digits(2);
    brightness_slider.add_mark(0.0, gtk4::PositionType::Bottom, None);
    color_inner.append(&brightness_slider);

    row_label(&color_inner, "Contrast");
    let contrast_slider = Scale::with_range(Orientation::Horizontal, 0.0, 2.0, 0.01);
    contrast_slider.set_value(1.0);
    contrast_slider.set_draw_value(true);
    contrast_slider.set_digits(2);
    contrast_slider.add_mark(1.0, gtk4::PositionType::Bottom, None);
    color_inner.append(&contrast_slider);

    row_label(&color_inner, "Saturation");
    let saturation_slider = Scale::with_range(Orientation::Horizontal, 0.0, 2.0, 0.01);
    saturation_slider.set_value(1.0);
    saturation_slider.set_draw_value(true);
    saturation_slider.set_digits(2);
    saturation_slider.add_mark(1.0, gtk4::PositionType::Bottom, None);
    color_inner.append(&saturation_slider);

    let ds_title = Label::new(Some("Denoise / Sharpness"));
    ds_title.set_halign(gtk::Align::Start);
    ds_title.add_css_class("browser-header");
    color_inner.append(&ds_title);

    row_label(&color_inner, "Denoise");
    let denoise_slider = Scale::with_range(Orientation::Horizontal, 0.0, 1.0, 0.01);
    denoise_slider.set_value(0.0);
    denoise_slider.set_draw_value(true);
    denoise_slider.set_digits(2);
    denoise_slider.add_mark(0.0, gtk4::PositionType::Bottom, None);
    color_inner.append(&denoise_slider);

    row_label(&color_inner, "Sharpness");
    let sharpness_slider = Scale::with_range(Orientation::Horizontal, -1.0, 1.0, 0.01);
    sharpness_slider.set_value(0.0);
    sharpness_slider.set_draw_value(true);
    sharpness_slider.set_digits(2);
    sharpness_slider.add_mark(0.0, gtk4::PositionType::Bottom, None);
    color_inner.append(&sharpness_slider);

    let grading_title = Label::new(Some("Grading"));
    grading_title.set_halign(gtk::Align::Start);
    grading_title.add_css_class("browser-header");
    color_inner.append(&grading_title);

    row_label(&color_inner, "Shadows");
    let shadows_slider = Scale::with_range(Orientation::Horizontal, -1.0, 1.0, 0.01);
    shadows_slider.set_value(0.0);
    shadows_slider.set_draw_value(true);
    shadows_slider.set_digits(2);
    shadows_slider.add_mark(0.0, gtk4::PositionType::Bottom, None);
    color_inner.append(&shadows_slider);

    row_label(&color_inner, "Midtones");
    let midtones_slider = Scale::with_range(Orientation::Horizontal, -1.0, 1.0, 0.01);
    midtones_slider.set_value(0.0);
    midtones_slider.set_draw_value(true);
    midtones_slider.set_digits(2);
    midtones_slider.add_mark(0.0, gtk4::PositionType::Bottom, None);
    color_inner.append(&midtones_slider);

    row_label(&color_inner, "Highlights");
    let highlights_slider = Scale::with_range(Orientation::Horizontal, -1.0, 1.0, 0.01);
    highlights_slider.set_value(0.0);
    highlights_slider.set_draw_value(true);
    highlights_slider.set_digits(2);
    highlights_slider.add_mark(0.0, gtk4::PositionType::Bottom, None);
    color_inner.append(&highlights_slider);

    // ── Audio section (Video + Audio only) ───────────────────────────────────
    let audio_section = GBox::new(Orientation::Vertical, 8);
    content_box.append(&audio_section);

    audio_section.append(&Separator::new(Orientation::Horizontal));
    let audio_expander = Expander::new(Some("Audio"));
    audio_expander.set_expanded(false);
    audio_section.append(&audio_expander);
    let audio_inner = GBox::new(Orientation::Vertical, 8);
    audio_expander.set_child(Some(&audio_inner));

    row_label(&audio_inner, "Volume");
    let volume_slider = Scale::with_range(Orientation::Horizontal, 0.0, 2.0, 0.01);
    volume_slider.set_value(1.0);
    volume_slider.set_draw_value(true);
    volume_slider.set_digits(2);
    volume_slider.add_mark(1.0, gtk4::PositionType::Bottom, None);
    audio_inner.append(&volume_slider);

    row_label(&audio_inner, "Pan");
    let pan_slider = Scale::with_range(Orientation::Horizontal, -1.0, 1.0, 0.01);
    pan_slider.set_value(0.0);
    pan_slider.set_draw_value(true);
    pan_slider.set_digits(2);
    pan_slider.add_mark(0.0, gtk4::PositionType::Bottom, None);
    audio_inner.append(&pan_slider);

    // ── Transform section (Video + Image only) ───────────────────────────────
    let transform_section = GBox::new(Orientation::Vertical, 8);
    content_box.append(&transform_section);

    transform_section.append(&Separator::new(Orientation::Horizontal));
    let transform_expander = Expander::new(Some("Transform"));
    transform_expander.set_expanded(false);
    transform_section.append(&transform_expander);
    let transform_inner = GBox::new(Orientation::Vertical, 8);
    transform_expander.set_child(Some(&transform_inner));

    row_label(&transform_inner, "Scale");
    let scale_slider = Scale::with_range(Orientation::Horizontal, 0.1, 4.0, 0.05);
    scale_slider.set_value(1.0);
    scale_slider.set_draw_value(true);
    scale_slider.set_digits(2);
    scale_slider.add_mark(0.5, gtk4::PositionType::Bottom, Some("½×"));
    scale_slider.add_mark(1.0, gtk4::PositionType::Bottom, Some("1×"));
    scale_slider.add_mark(2.0, gtk4::PositionType::Bottom, Some("2×"));
    scale_slider.set_hexpand(true);
    scale_slider.set_tooltip_text(Some("Scale: <1 = shrink with black borders, >1 = zoom in"));
    transform_inner.append(&scale_slider);

    row_label(&transform_inner, "Opacity");
    let opacity_slider = Scale::with_range(Orientation::Horizontal, 0.0, 1.0, 0.01);
    opacity_slider.set_value(1.0);
    opacity_slider.set_draw_value(true);
    opacity_slider.set_digits(2);
    opacity_slider.add_mark(0.0, gtk4::PositionType::Bottom, Some("0%"));
    opacity_slider.add_mark(1.0, gtk4::PositionType::Bottom, Some("100%"));
    opacity_slider.set_hexpand(true);
    opacity_slider.set_tooltip_text(Some("Clip opacity for compositing"));
    transform_inner.append(&opacity_slider);

    row_label(&transform_inner, "Position X");
    let position_x_slider = Scale::with_range(Orientation::Horizontal, -1.0, 1.0, 0.01);
    position_x_slider.set_value(0.0);
    position_x_slider.set_draw_value(true);
    position_x_slider.set_digits(2);
    position_x_slider.add_mark(-1.0, gtk4::PositionType::Bottom, Some("←"));
    position_x_slider.add_mark(0.0, gtk4::PositionType::Bottom, Some("·"));
    position_x_slider.add_mark(1.0, gtk4::PositionType::Bottom, Some("→"));
    position_x_slider.set_hexpand(true);
    position_x_slider.set_tooltip_text(Some(
        "Horizontal position: −1 = left, 0 = center, +1 = right",
    ));
    transform_inner.append(&position_x_slider);

    row_label(&transform_inner, "Position Y");
    let position_y_slider = Scale::with_range(Orientation::Horizontal, -1.0, 1.0, 0.01);
    position_y_slider.set_value(0.0);
    position_y_slider.set_draw_value(true);
    position_y_slider.set_digits(2);
    position_y_slider.add_mark(-1.0, gtk4::PositionType::Bottom, Some("↑"));
    position_y_slider.add_mark(0.0, gtk4::PositionType::Bottom, Some("·"));
    position_y_slider.add_mark(1.0, gtk4::PositionType::Bottom, Some("↓"));
    position_y_slider.set_hexpand(true);
    position_y_slider
        .set_tooltip_text(Some("Vertical position: −1 = top, 0 = center, +1 = bottom"));
    transform_inner.append(&position_y_slider);

    row_label(&transform_inner, "Crop Left");
    let crop_left_slider = Scale::with_range(Orientation::Horizontal, 0.0, 500.0, 2.0);
    crop_left_slider.set_value(0.0);
    crop_left_slider.set_draw_value(true);
    crop_left_slider.set_digits(0);
    transform_inner.append(&crop_left_slider);

    row_label(&transform_inner, "Crop Right");
    let crop_right_slider = Scale::with_range(Orientation::Horizontal, 0.0, 500.0, 2.0);
    crop_right_slider.set_value(0.0);
    crop_right_slider.set_draw_value(true);
    crop_right_slider.set_digits(0);
    transform_inner.append(&crop_right_slider);

    row_label(&transform_inner, "Crop Top");
    let crop_top_slider = Scale::with_range(Orientation::Horizontal, 0.0, 500.0, 2.0);
    crop_top_slider.set_value(0.0);
    crop_top_slider.set_draw_value(true);
    crop_top_slider.set_digits(0);
    transform_inner.append(&crop_top_slider);

    row_label(&transform_inner, "Crop Bottom");
    let crop_bottom_slider = Scale::with_range(Orientation::Horizontal, 0.0, 500.0, 2.0);
    crop_bottom_slider.set_value(0.0);
    crop_bottom_slider.set_draw_value(true);
    crop_bottom_slider.set_digits(0);
    transform_inner.append(&crop_bottom_slider);

    row_label(&transform_inner, "Rotate");
    let rotate_combo = gtk4::ComboBoxText::new();
    rotate_combo.append(Some("0"), "0°");
    rotate_combo.append(Some("90"), "90° CW");
    rotate_combo.append(Some("180"), "180°");
    rotate_combo.append(Some("270"), "270° CW");
    rotate_combo.set_active_id(Some("0"));
    transform_inner.append(&rotate_combo);

    row_label(&transform_inner, "Flip");
    let flip_row = GBox::new(Orientation::Horizontal, 8);
    let flip_h_btn = gtk4::ToggleButton::with_label("Flip H");
    let flip_v_btn = gtk4::ToggleButton::with_label("Flip V");
    flip_row.append(&flip_h_btn);
    flip_row.append(&flip_v_btn);
    transform_inner.append(&flip_row);

    // ── Title Overlay section (Video + Image only) ───────────────────────────
    let title_section_box = GBox::new(Orientation::Vertical, 8);
    content_box.append(&title_section_box);

    title_section_box.append(&Separator::new(Orientation::Horizontal));
    let title_expander = Expander::new(Some("Title Overlay"));
    title_expander.set_expanded(false);
    title_section_box.append(&title_expander);
    let title_inner = GBox::new(Orientation::Vertical, 8);
    title_expander.set_child(Some(&title_inner));

    let title_entry = Entry::new();
    title_entry.set_placeholder_text(Some("Overlay text…"));
    title_inner.append(&title_entry);

    row_label(&title_inner, "Position X");
    let title_x_slider = Scale::with_range(Orientation::Horizontal, 0.0, 1.0, 0.01);
    title_x_slider.set_value(0.5);
    title_x_slider.set_hexpand(true);
    title_inner.append(&title_x_slider);

    row_label(&title_inner, "Position Y");
    let title_y_slider = Scale::with_range(Orientation::Horizontal, 0.0, 1.0, 0.01);
    title_y_slider.set_value(0.9);
    title_y_slider.set_hexpand(true);
    title_inner.append(&title_y_slider);

    // ── Speed section (all clip types) ───────────────────────────────────────
    let speed_section_box = GBox::new(Orientation::Vertical, 8);
    content_box.append(&speed_section_box);

    speed_section_box.append(&Separator::new(Orientation::Horizontal));
    let speed_expander = Expander::new(Some("Speed"));
    speed_expander.set_expanded(false);
    speed_section_box.append(&speed_expander);
    let speed_inner = GBox::new(Orientation::Vertical, 8);
    speed_expander.set_child(Some(&speed_inner));

    row_label(&speed_inner, "Speed Multiplier");
    let speed_slider = Scale::with_range(Orientation::Horizontal, 0.25, 4.0, 0.05);
    speed_slider.set_value(1.0);
    speed_slider.set_draw_value(true);
    speed_slider.set_digits(2);
    speed_slider.add_mark(0.5, gtk4::PositionType::Bottom, Some("½×"));
    speed_slider.add_mark(1.0, gtk4::PositionType::Bottom, Some("1×"));
    speed_slider.add_mark(2.0, gtk4::PositionType::Bottom, Some("2×"));
    speed_slider.set_hexpand(true);
    speed_slider.set_tooltip_text(Some("Playback speed: <1 = slow motion, >1 = fast forward"));
    speed_inner.append(&speed_slider);

    // ── LUT section (Video + Image only) ─────────────────────────────────────
    let lut_section_box = GBox::new(Orientation::Vertical, 8);
    content_box.append(&lut_section_box);

    lut_section_box.append(&Separator::new(Orientation::Horizontal));
    let lut_expander = Expander::new(Some("Color LUT"));
    lut_expander.set_expanded(false);
    lut_section_box.append(&lut_expander);
    let lut_inner = GBox::new(Orientation::Vertical, 8);
    lut_expander.set_child(Some(&lut_inner));

    let lut_path_label = Label::new(Some("None"));
    lut_path_label.set_halign(gtk4::Align::Start);
    lut_path_label.set_ellipsize(gtk4::pango::EllipsizeMode::Start);
    lut_path_label.set_max_width_chars(22);
    lut_path_label.add_css_class("clip-path");
    lut_inner.append(&lut_path_label);

    let lut_btn_row = GBox::new(Orientation::Horizontal, 8);
    let lut_import_btn = Button::with_label("Import LUT…");
    let lut_clear_btn = Button::with_label("Clear");
    lut_clear_btn.set_sensitive(false);
    lut_btn_row.append(&lut_import_btn);
    lut_btn_row.append(&lut_clear_btn);
    lut_inner.append(&lut_btn_row);

    let lut_note = Label::new(Some("Applied on export (.cube)"));
    lut_note.set_halign(gtk4::Align::Start);
    lut_note.add_css_class("clip-path");
    lut_inner.append(&lut_note);

    // Apply name button
    content_box.append(&Separator::new(Orientation::Horizontal));
    let apply_btn = Button::with_label("Apply Name");
    content_box.append(&apply_btn);
    let selected_clip_id: Rc<RefCell<Option<String>>> = Rc::new(RefCell::new(None));
    let updating: Rc<RefCell<bool>> = Rc::new(RefCell::new(false));

    let on_clip_changed = Rc::new(on_clip_changed);
    let on_color_changed: Rc<dyn Fn(f32, f32, f32, f32, f32, f32, f32, f32)> = Rc::new(on_color_changed);
    let on_audio_changed: Rc<dyn Fn(f32, f32)> = Rc::new(on_audio_changed);
    let on_transform_changed: Rc<dyn Fn(i32, i32, i32, i32, i32, bool, bool, f64, f64, f64)> =
        Rc::new(on_transform_changed);
    let on_title_changed: Rc<dyn Fn(String, f64, f64)> = Rc::new(on_title_changed);
    let on_speed_changed: Rc<dyn Fn(f64)> = Rc::new(on_speed_changed);
    let on_lut_changed: Rc<dyn Fn(Option<String>)> = Rc::new(on_lut_changed);
    let on_opacity_changed: Rc<dyn Fn(f64)> = Rc::new(on_opacity_changed);

    // Apply name button — triggers full on_project_changed
    {
        let project = project.clone();
        let selected_clip_id = selected_clip_id.clone();
        let name_entry_cb = name_entry.clone();
        let on_clip_changed = on_clip_changed.clone();

        apply_btn.connect_clicked(move |_| {
            let new_name = name_entry_cb.text().to_string();
            if new_name.is_empty() {
                return;
            }
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
    // with all eight current values so the program player can update its filters directly.
    fn connect_color_slider(
        slider: &Scale,
        project: Rc<RefCell<Project>>,
        selected_clip_id: Rc<RefCell<Option<String>>>,
        updating: Rc<RefCell<bool>>,
        on_color_changed: Rc<dyn Fn(f32, f32, f32, f32, f32, f32, f32, f32)>,
        brightness_slider: Scale,
        contrast_slider: Scale,
        saturation_slider: Scale,
        denoise_slider: Scale,
        sharpness_slider: Scale,
        shadows_slider: Scale,
        midtones_slider: Scale,
        highlights_slider: Scale,
        apply: fn(&mut crate::model::clip::Clip, f32),
    ) {
        slider.connect_value_changed(move |s| {
            if *updating.borrow() {
                return;
            }
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
                let b = brightness_slider.value() as f32;
                let c = contrast_slider.value() as f32;
                let sat = saturation_slider.value() as f32;
                let d = denoise_slider.value() as f32;
                let sh = sharpness_slider.value() as f32;
                let shd = shadows_slider.value() as f32;
                let mid = midtones_slider.value() as f32;
                let hil = highlights_slider.value() as f32;
                on_color_changed(b, c, sat, d, sh, shd, mid, hil);
            }
        });
    }

    connect_color_slider(
        &brightness_slider,
        project.clone(),
        selected_clip_id.clone(),
        updating.clone(),
        on_color_changed.clone(),
        brightness_slider.clone(),
        contrast_slider.clone(),
        saturation_slider.clone(),
        denoise_slider.clone(),
        sharpness_slider.clone(),
        shadows_slider.clone(),
        midtones_slider.clone(),
        highlights_slider.clone(),
        |clip, v| clip.brightness = v,
    );
    connect_color_slider(
        &contrast_slider,
        project.clone(),
        selected_clip_id.clone(),
        updating.clone(),
        on_color_changed.clone(),
        brightness_slider.clone(),
        contrast_slider.clone(),
        saturation_slider.clone(),
        denoise_slider.clone(),
        sharpness_slider.clone(),
        shadows_slider.clone(),
        midtones_slider.clone(),
        highlights_slider.clone(),
        |clip, v| clip.contrast = v,
    );
    connect_color_slider(
        &saturation_slider,
        project.clone(),
        selected_clip_id.clone(),
        updating.clone(),
        on_color_changed.clone(),
        brightness_slider.clone(),
        contrast_slider.clone(),
        saturation_slider.clone(),
        denoise_slider.clone(),
        sharpness_slider.clone(),
        shadows_slider.clone(),
        midtones_slider.clone(),
        highlights_slider.clone(),
        |clip, v| clip.saturation = v,
    );
    connect_color_slider(
        &denoise_slider,
        project.clone(),
        selected_clip_id.clone(),
        updating.clone(),
        on_color_changed.clone(),
        brightness_slider.clone(),
        contrast_slider.clone(),
        saturation_slider.clone(),
        denoise_slider.clone(),
        sharpness_slider.clone(),
        shadows_slider.clone(),
        midtones_slider.clone(),
        highlights_slider.clone(),
        |clip, v| clip.denoise = v,
    );
    connect_color_slider(
        &sharpness_slider,
        project.clone(),
        selected_clip_id.clone(),
        updating.clone(),
        on_color_changed.clone(),
        brightness_slider.clone(),
        contrast_slider.clone(),
        saturation_slider.clone(),
        denoise_slider.clone(),
        sharpness_slider.clone(),
        shadows_slider.clone(),
        midtones_slider.clone(),
        highlights_slider.clone(),
        |clip, v| clip.sharpness = v,
    );
    connect_color_slider(
        &shadows_slider,
        project.clone(),
        selected_clip_id.clone(),
        updating.clone(),
        on_color_changed.clone(),
        brightness_slider.clone(),
        contrast_slider.clone(),
        saturation_slider.clone(),
        denoise_slider.clone(),
        sharpness_slider.clone(),
        shadows_slider.clone(),
        midtones_slider.clone(),
        highlights_slider.clone(),
        |clip, v| clip.shadows = v,
    );
    connect_color_slider(
        &midtones_slider,
        project.clone(),
        selected_clip_id.clone(),
        updating.clone(),
        on_color_changed.clone(),
        brightness_slider.clone(),
        contrast_slider.clone(),
        saturation_slider.clone(),
        denoise_slider.clone(),
        sharpness_slider.clone(),
        shadows_slider.clone(),
        midtones_slider.clone(),
        highlights_slider.clone(),
        |clip, v| clip.midtones = v,
    );
    connect_color_slider(
        &highlights_slider,
        project.clone(),
        selected_clip_id.clone(),
        updating.clone(),
        on_color_changed.clone(),
        brightness_slider.clone(),
        contrast_slider.clone(),
        saturation_slider.clone(),
        denoise_slider.clone(),
        sharpness_slider.clone(),
        shadows_slider.clone(),
        midtones_slider.clone(),
        highlights_slider.clone(),
        |clip, v| clip.highlights = v,
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
            if *updating.borrow() {
                return;
            }
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
                on_audio_changed(
                    volume_slider_cb.value() as f32,
                    pan_slider_cb.value() as f32,
                );
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
            if *updating.borrow() {
                return;
            }
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
                on_audio_changed(
                    volume_slider_cb.value() as f32,
                    pan_slider_cb.value() as f32,
                );
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
        on_transform_changed: Rc<dyn Fn(i32, i32, i32, i32, i32, bool, bool, f64, f64, f64)>,
        crop_left_s: Scale,
        crop_right_s: Scale,
        crop_top_s: Scale,
        crop_bottom_s: Scale,
        rotate_c: gtk4::ComboBoxText,
        flip_h_b: gtk4::ToggleButton,
        flip_v_b: gtk4::ToggleButton,
        scale_s: Scale,
        pos_x_s: Scale,
        pos_y_s: Scale,
        apply: fn(&mut crate::model::clip::Clip, i32),
    ) {
        slider.connect_value_changed(move |s| {
            if *updating.borrow() {
                return;
            }
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
                let rot = rotate_c
                    .active_id()
                    .and_then(|id| id.parse::<i32>().ok())
                    .unwrap_or(0);
                let fh = flip_h_b.is_active();
                let fv = flip_v_b.is_active();
                let sc = scale_s.value();
                let px = pos_x_s.value();
                let py = pos_y_s.value();
                on_transform_changed(cl, cr, ct, cb, rot, fh, fv, sc, px, py);
            }
        });
    }

    connect_transform_slider(
        &crop_left_slider,
        project.clone(),
        selected_clip_id.clone(),
        updating.clone(),
        on_transform_changed.clone(),
        crop_left_slider.clone(),
        crop_right_slider.clone(),
        crop_top_slider.clone(),
        crop_bottom_slider.clone(),
        rotate_combo.clone(),
        flip_h_btn.clone(),
        flip_v_btn.clone(),
        scale_slider.clone(),
        position_x_slider.clone(),
        position_y_slider.clone(),
        |clip, v| clip.crop_left = v,
    );
    connect_transform_slider(
        &crop_right_slider,
        project.clone(),
        selected_clip_id.clone(),
        updating.clone(),
        on_transform_changed.clone(),
        crop_left_slider.clone(),
        crop_right_slider.clone(),
        crop_top_slider.clone(),
        crop_bottom_slider.clone(),
        rotate_combo.clone(),
        flip_h_btn.clone(),
        flip_v_btn.clone(),
        scale_slider.clone(),
        position_x_slider.clone(),
        position_y_slider.clone(),
        |clip, v| clip.crop_right = v,
    );
    connect_transform_slider(
        &crop_top_slider,
        project.clone(),
        selected_clip_id.clone(),
        updating.clone(),
        on_transform_changed.clone(),
        crop_left_slider.clone(),
        crop_right_slider.clone(),
        crop_top_slider.clone(),
        crop_bottom_slider.clone(),
        rotate_combo.clone(),
        flip_h_btn.clone(),
        flip_v_btn.clone(),
        scale_slider.clone(),
        position_x_slider.clone(),
        position_y_slider.clone(),
        |clip, v| clip.crop_top = v,
    );
    connect_transform_slider(
        &crop_bottom_slider,
        project.clone(),
        selected_clip_id.clone(),
        updating.clone(),
        on_transform_changed.clone(),
        crop_left_slider.clone(),
        crop_right_slider.clone(),
        crop_top_slider.clone(),
        crop_bottom_slider.clone(),
        rotate_combo.clone(),
        flip_h_btn.clone(),
        flip_v_btn.clone(),
        scale_slider.clone(),
        position_x_slider.clone(),
        position_y_slider.clone(),
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
        let scale_s = scale_slider.clone();
        let pos_x_s = position_x_slider.clone();
        let pos_y_s = position_y_slider.clone();
        rotate_combo.connect_changed(move |combo| {
            if *updating.borrow() {
                return;
            }
            let rot = combo
                .active_id()
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
                on_transform_changed(
                    cl,
                    cr,
                    ct,
                    cb,
                    rot,
                    fh,
                    fv,
                    scale_s.value(),
                    pos_x_s.value(),
                    pos_y_s.value(),
                );
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
        let scale_s = scale_slider.clone();
        let pos_x_s = position_x_slider.clone();
        let pos_y_s = position_y_slider.clone();
        flip_h_btn.connect_toggled(move |btn| {
            if *updating.borrow() {
                return;
            }
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
                let rot = rotate_c
                    .active_id()
                    .and_then(|id| id.parse::<i32>().ok())
                    .unwrap_or(0);
                let fv = flip_v_b.is_active();
                on_transform_changed(
                    cl,
                    cr,
                    ct,
                    cb,
                    rot,
                    fh,
                    fv,
                    scale_s.value(),
                    pos_x_s.value(),
                    pos_y_s.value(),
                );
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
        let scale_s = scale_slider.clone();
        let pos_x_s = position_x_slider.clone();
        let pos_y_s = position_y_slider.clone();
        flip_v_btn.connect_toggled(move |btn| {
            if *updating.borrow() {
                return;
            }
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
                let rot = rotate_c
                    .active_id()
                    .and_then(|id| id.parse::<i32>().ok())
                    .unwrap_or(0);
                let fh = flip_h_b.is_active();
                on_transform_changed(
                    cl,
                    cr,
                    ct,
                    cb,
                    rot,
                    fh,
                    fv,
                    scale_s.value(),
                    pos_x_s.value(),
                    pos_y_s.value(),
                );
            }
        });
    }

    // Wire scale and position sliders
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
        let flip_v_b = flip_v_btn.clone();
        let pos_x_s = position_x_slider.clone();
        let pos_y_s = position_y_slider.clone();
        let scale_s2 = scale_slider.clone();
        scale_slider.connect_value_changed(move |sl| {
            if *updating.borrow() {
                return;
            }
            let sc = sl.value();
            let id = selected_clip_id.borrow().clone();
            if let Some(ref clip_id) = id {
                {
                    let mut proj = project.borrow_mut();
                    for track in &mut proj.tracks {
                        if let Some(clip) = track.clips.iter_mut().find(|c| &c.id == clip_id) {
                            clip.scale = sc;
                            proj.dirty = true;
                            break;
                        }
                    }
                }
                let cl = crop_left_s.value() as i32;
                let cr = crop_right_s.value() as i32;
                let ct = crop_top_s.value() as i32;
                let cb = crop_bottom_s.value() as i32;
                let rot = rotate_c
                    .active_id()
                    .and_then(|id| id.parse::<i32>().ok())
                    .unwrap_or(0);
                let fh = flip_h_b.is_active();
                let fv = flip_v_b.is_active();
                on_transform_changed(
                    cl,
                    cr,
                    ct,
                    cb,
                    rot,
                    fh,
                    fv,
                    sc,
                    pos_x_s.value(),
                    pos_y_s.value(),
                );
            }
        });
        let _ = scale_s2; // silence unused warning
    }
    {
        let project = project.clone();
        let selected_clip_id = selected_clip_id.clone();
        let updating = updating.clone();
        let on_opacity_changed = on_opacity_changed.clone();
        opacity_slider.connect_value_changed(move |sl| {
            if *updating.borrow() {
                return;
            }
            let opacity = sl.value().clamp(0.0, 1.0);
            let id = selected_clip_id.borrow().clone();
            if let Some(ref clip_id) = id {
                {
                    let mut proj = project.borrow_mut();
                    for track in &mut proj.tracks {
                        if let Some(clip) = track.clips.iter_mut().find(|c| &c.id == clip_id) {
                            clip.opacity = opacity;
                            proj.dirty = true;
                            break;
                        }
                    }
                }
                on_opacity_changed(opacity);
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
        let flip_v_b = flip_v_btn.clone();
        let scale_s = scale_slider.clone();
        let pos_y_s = position_y_slider.clone();
        let pos_x_s2 = position_x_slider.clone();
        position_x_slider.connect_value_changed(move |sl| {
            if *updating.borrow() {
                return;
            }
            let px = sl.value();
            let id = selected_clip_id.borrow().clone();
            if let Some(ref clip_id) = id {
                {
                    let mut proj = project.borrow_mut();
                    for track in &mut proj.tracks {
                        if let Some(clip) = track.clips.iter_mut().find(|c| &c.id == clip_id) {
                            clip.position_x = px;
                            proj.dirty = true;
                            break;
                        }
                    }
                }
                let cl = crop_left_s.value() as i32;
                let cr = crop_right_s.value() as i32;
                let ct = crop_top_s.value() as i32;
                let cb = crop_bottom_s.value() as i32;
                let rot = rotate_c
                    .active_id()
                    .and_then(|id| id.parse::<i32>().ok())
                    .unwrap_or(0);
                let fh = flip_h_b.is_active();
                let fv = flip_v_b.is_active();
                on_transform_changed(
                    cl,
                    cr,
                    ct,
                    cb,
                    rot,
                    fh,
                    fv,
                    scale_s.value(),
                    px,
                    pos_y_s.value(),
                );
            }
        });
        let _ = pos_x_s2;
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
        let flip_v_b = flip_v_btn.clone();
        let scale_s = scale_slider.clone();
        let pos_x_s = position_x_slider.clone();
        let pos_y_s2 = position_y_slider.clone();
        position_y_slider.connect_value_changed(move |sl| {
            if *updating.borrow() {
                return;
            }
            let py = sl.value();
            let id = selected_clip_id.borrow().clone();
            if let Some(ref clip_id) = id {
                {
                    let mut proj = project.borrow_mut();
                    for track in &mut proj.tracks {
                        if let Some(clip) = track.clips.iter_mut().find(|c| &c.id == clip_id) {
                            clip.position_y = py;
                            proj.dirty = true;
                            break;
                        }
                    }
                }
                let cl = crop_left_s.value() as i32;
                let cr = crop_right_s.value() as i32;
                let ct = crop_top_s.value() as i32;
                let cb = crop_bottom_s.value() as i32;
                let rot = rotate_c
                    .active_id()
                    .and_then(|id| id.parse::<i32>().ok())
                    .unwrap_or(0);
                let fh = flip_h_b.is_active();
                let fv = flip_v_b.is_active();
                on_transform_changed(
                    cl,
                    cr,
                    ct,
                    cb,
                    rot,
                    fh,
                    fv,
                    scale_s.value(),
                    pos_x_s.value(),
                    py,
                );
            }
        });
        let _ = pos_y_s2;
    }

    // Title entry and position sliders
    {
        let project = project.clone();
        let selected_clip_id = selected_clip_id.clone();
        let updating = updating.clone();
        let title_x = title_x_slider.clone();
        let title_y = title_y_slider.clone();
        let on_title_changed = on_title_changed.clone();
        title_entry.connect_changed(move |entry| {
            if *updating.borrow() {
                return;
            }
            let text = entry.text().to_string();
            let id = selected_clip_id.borrow().clone();
            if let Some(ref clip_id) = id {
                {
                    let mut proj = project.borrow_mut();
                    for track in &mut proj.tracks {
                        if let Some(clip) = track.clips.iter_mut().find(|c| &c.id == clip_id) {
                            clip.title_text = text.clone();
                            proj.dirty = true;
                            break;
                        }
                    }
                }
                on_title_changed(text, title_x.value(), title_y.value());
            }
        });
    }
    {
        let project = project.clone();
        let selected_clip_id = selected_clip_id.clone();
        let updating = updating.clone();
        let title_entry_x = title_entry.clone();
        let title_y = title_y_slider.clone();
        let on_title_changed = on_title_changed.clone();
        title_x_slider.connect_value_changed(move |sl| {
            if *updating.borrow() {
                return;
            }
            let x = sl.value();
            let id = selected_clip_id.borrow().clone();
            if let Some(ref clip_id) = id {
                {
                    let mut proj = project.borrow_mut();
                    for track in &mut proj.tracks {
                        if let Some(clip) = track.clips.iter_mut().find(|c| &c.id == clip_id) {
                            clip.title_x = x;
                            proj.dirty = true;
                            break;
                        }
                    }
                }
                on_title_changed(title_entry_x.text().to_string(), x, title_y.value());
            }
        });
    }
    {
        let project = project.clone();
        let selected_clip_id = selected_clip_id.clone();
        let updating = updating.clone();
        let title_entry_y = title_entry.clone();
        let title_x = title_x_slider.clone();
        let on_title_changed = on_title_changed.clone();
        title_y_slider.connect_value_changed(move |sl| {
            if *updating.borrow() {
                return;
            }
            let y = sl.value();
            let id = selected_clip_id.borrow().clone();
            if let Some(ref clip_id) = id {
                {
                    let mut proj = project.borrow_mut();
                    for track in &mut proj.tracks {
                        if let Some(clip) = track.clips.iter_mut().find(|c| &c.id == clip_id) {
                            clip.title_y = y;
                            proj.dirty = true;
                            break;
                        }
                    }
                }
                on_title_changed(title_entry_y.text().to_string(), title_x.value(), y);
            }
        });
    }

    // Speed slider
    {
        let project = project.clone();
        let selected_clip_id = selected_clip_id.clone();
        let updating = updating.clone();
        let on_speed_changed = on_speed_changed.clone();
        speed_slider.connect_value_changed(move |sl| {
            if *updating.borrow() {
                return;
            }
            let speed = sl.value();
            if let Some(ref id) = *selected_clip_id.borrow() {
                let mut proj = project.borrow_mut();
                let mut found = false;
                for track in &mut proj.tracks {
                    for clip in &mut track.clips {
                        if clip.id == *id {
                            clip.speed = speed;
                            found = true;
                        }
                    }
                }
                if found {
                    proj.dirty = true;
                }
            }
            on_speed_changed(speed);
        });
    }

    // LUT import button
    {
        let project = project.clone();
        let selected_clip_id = selected_clip_id.clone();
        let on_lut_changed = on_lut_changed.clone();
        let lut_path_label = lut_path_label.clone();
        let lut_clear_btn = lut_clear_btn.clone();
        lut_import_btn.connect_clicked(move |btn| {
            let dialog = gtk4::FileDialog::new();
            dialog.set_title("Import LUT");
            let filter = gtk4::FileFilter::new();
            filter.add_pattern("*.cube");
            filter.set_name(Some("3D LUT Files (*.cube)"));
            let filters = gio::ListStore::new::<gtk4::FileFilter>();
            filters.append(&filter);
            dialog.set_filters(Some(&filters));

            let project = project.clone();
            let selected_clip_id = selected_clip_id.clone();
            let on_lut_changed = on_lut_changed.clone();
            let lut_path_label = lut_path_label.clone();
            let lut_clear_btn = lut_clear_btn.clone();
            let window = btn.root().and_then(|r| r.downcast::<gtk4::Window>().ok());

            dialog.open(window.as_ref(), gio::Cancellable::NONE, move |result| {
                if let Ok(file) = result {
                    if let Some(path) = file.path() {
                        let path_str = path.to_string_lossy().to_string();
                        let id = selected_clip_id.borrow().clone();
                        if let Some(ref clip_id) = id {
                            let mut proj = project.borrow_mut();
                            for track in &mut proj.tracks {
                                if let Some(clip) =
                                    track.clips.iter_mut().find(|c| &c.id == clip_id)
                                {
                                    clip.lut_path = Some(path_str.clone());
                                    proj.dirty = true;
                                    break;
                                }
                            }
                        }
                        let name = path
                            .file_name()
                            .and_then(|n| n.to_str())
                            .unwrap_or(&path_str)
                            .to_string();
                        lut_path_label.set_text(&name);
                        lut_clear_btn.set_sensitive(true);
                        on_lut_changed(Some(path_str));
                    }
                }
            });
        });
    }

    // LUT clear button
    {
        let project = project.clone();
        let selected_clip_id = selected_clip_id.clone();
        let on_lut_changed = on_lut_changed.clone();
        let lut_path_label = lut_path_label.clone();
        let lut_clear_btn_cb = lut_clear_btn.clone();
        lut_clear_btn.connect_clicked(move |_| {
            let id = selected_clip_id.borrow().clone();
            if let Some(ref clip_id) = id {
                let mut proj = project.borrow_mut();
                for track in &mut proj.tracks {
                    if let Some(clip) = track.clips.iter_mut().find(|c| &c.id == clip_id) {
                        clip.lut_path = None;
                        proj.dirty = true;
                        break;
                    }
                }
            }
            lut_path_label.set_text("None");
            lut_clear_btn_cb.set_sensitive(false);
            on_lut_changed(None);
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
        shadows_slider,
        midtones_slider,
        highlights_slider,
        volume_slider,
        pan_slider,
        crop_left_slider,
        crop_right_slider,
        crop_top_slider,
        crop_bottom_slider,
        rotate_combo,
        flip_h_btn,
        flip_v_btn,
        scale_slider,
        opacity_slider,
        position_x_slider,
        position_y_slider,
        title_entry,
        title_x_slider,
        title_y_slider,
        speed_slider,
        lut_path_label,
        lut_clear_btn,
        updating,
        content_box,
        empty_state_label,
        color_section,
        audio_section,
        transform_section,
        title_section_box,
        speed_section_box,
        lut_section_box,
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
