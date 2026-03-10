use crate::model::clip::{ClipColorLabel, KeyframeInterpolation, Phase1KeyframeProperty};
use crate::model::project::Project;
use gdk4;
use gio;
use gtk4::prelude::*;
use gtk4::{
    self as gtk, Box as GBox, Button, CheckButton, Entry, Expander, Label, Orientation, Scale,
    Separator,
};
use std::cell::{Cell, RefCell};
use std::rc::Rc;

const VOLUME_DB_MIN: f64 = -100.0;
const VOLUME_DB_MAX: f64 = 12.0;
const VOLUME_LINEAR_MAX: f64 = 3.981_071_705_5; // +12 dB

fn db_to_linear_volume(db: f64) -> f64 {
    (10.0f64)
        .powf(db.clamp(VOLUME_DB_MIN, VOLUME_DB_MAX) / 20.0)
        .clamp(0.0, VOLUME_LINEAR_MAX)
}

fn linear_to_db_volume(linear: f64) -> f64 {
    if linear <= 0.0 {
        VOLUME_DB_MIN
    } else {
        (20.0 * linear.log10()).clamp(VOLUME_DB_MIN, VOLUME_DB_MAX)
    }
}

fn interp_idx_to_enum(idx: u32) -> KeyframeInterpolation {
    match idx {
        1 => KeyframeInterpolation::EaseIn,
        2 => KeyframeInterpolation::EaseOut,
        3 => KeyframeInterpolation::EaseInOut,
        _ => KeyframeInterpolation::Linear,
    }
}

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
    pub clip_color_label_combo: gtk4::DropDown,
    // Color correction sliders
    pub brightness_slider: Scale,
    pub contrast_slider: Scale,
    pub saturation_slider: Scale,
    pub temperature_slider: Scale,
    pub tint_slider: Scale,
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
    pub rotate_spin: gtk4::SpinButton,
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
    pub reverse_check: CheckButton,
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
    // Chroma key
    pub chroma_key_section: GBox,
    pub chroma_key_enable: CheckButton,
    pub chroma_green_btn: gtk4::ToggleButton,
    pub chroma_blue_btn: gtk4::ToggleButton,
    pub chroma_custom_btn: gtk4::ToggleButton,
    pub chroma_color_btn: gtk4::ColorDialogButton,
    pub chroma_custom_color_row: GBox,
    pub chroma_tolerance_slider: Scale,
    pub chroma_softness_slider: Scale,
    // Background removal
    pub bg_removal_section: GBox,
    pub bg_removal_enable: CheckButton,
    pub bg_removal_threshold_slider: Scale,
    /// Set to `true` when the ONNX model is present; controls section visibility.
    pub bg_removal_model_available: Cell<bool>,
    // Keyframe navigation and animation mode
    pub keyframe_indicator_label: Label,
    pub animation_mode: Rc<Cell<bool>>,
    pub animation_mode_btn: gtk4::ToggleButton,
    pub interp_dropdown: gtk4::DropDown,
    // Audio keyframe navigation
    pub audio_keyframe_indicator_label: Label,
    pub audio_animation_mode_btn: gtk4::ToggleButton,
}

impl InspectorView {
    /// Get the currently selected interpolation mode from the dropdown.
    pub fn selected_interpolation(&self) -> KeyframeInterpolation {
        match self.interp_dropdown.selected() {
            1 => KeyframeInterpolation::EaseIn,
            2 => KeyframeInterpolation::EaseOut,
            3 => KeyframeInterpolation::EaseInOut,
            _ => KeyframeInterpolation::Linear,
        }
    }

    /// Refresh all fields to show the given clip, or clear if None.
    /// `playhead_ns` is used to display keyframe-evaluated values for animated properties.
    pub fn update(&self, project: &Project, clip_id: Option<&str>, playhead_ns: u64) {
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
                self.chroma_key_section.set_visible(is_video || is_image);
                self.bg_removal_section
                    .set_visible((is_video || is_image) && self.bg_removal_model_available.get());

                self.name_entry.set_text(&c.label);
                self.path_value.set_text(
                    std::path::Path::new(&c.source_path)
                        .file_name()
                        .and_then(|n| n.to_str())
                        .unwrap_or(&c.source_path),
                );
                self.clip_color_label_combo
                    .set_selected(clip_color_label_index(c.color_label));
                self.in_value.set_text(&ns_to_timecode(c.source_in));
                self.out_value.set_text(&ns_to_timecode(c.source_out));
                self.dur_value.set_text(&ns_to_timecode(c.duration()));
                self.pos_value.set_text(&ns_to_timecode(c.timeline_start));
                self.brightness_slider.set_value(c.brightness as f64);
                self.contrast_slider.set_value(c.contrast as f64);
                self.saturation_slider.set_value(c.saturation as f64);
                self.temperature_slider.set_value(c.temperature as f64);
                self.tint_slider.set_value(c.tint as f64);
                self.denoise_slider.set_value(c.denoise as f64);
                self.sharpness_slider.set_value(c.sharpness as f64);
                self.shadows_slider.set_value(c.shadows as f64);
                self.midtones_slider.set_value(c.midtones as f64);
                self.highlights_slider.set_value(c.highlights as f64);
                // For keyframed properties, show the evaluated value at the playhead
                let vol_val = c.value_for_phase1_property_at_timeline_ns(
                    Phase1KeyframeProperty::Volume,
                    playhead_ns,
                );
                self.volume_slider
                    .set_value(linear_to_db_volume(vol_val));
                self.pan_slider.set_value(c.pan as f64);
                self.crop_left_slider.set_value(c.crop_left as f64);
                self.crop_right_slider.set_value(c.crop_right as f64);
                self.crop_top_slider.set_value(c.crop_top as f64);
                self.crop_bottom_slider.set_value(c.crop_bottom as f64);
                self.rotate_spin.set_value(c.rotate as f64);
                self.flip_h_btn.set_active(c.flip_h);
                self.flip_v_btn.set_active(c.flip_v);
                self.scale_slider.set_value(
                    c.value_for_phase1_property_at_timeline_ns(
                        Phase1KeyframeProperty::Scale,
                        playhead_ns,
                    ),
                );
                self.opacity_slider.set_value(
                    c.value_for_phase1_property_at_timeline_ns(
                        Phase1KeyframeProperty::Opacity,
                        playhead_ns,
                    ),
                );
                self.position_x_slider.set_value(
                    c.value_for_phase1_property_at_timeline_ns(
                        Phase1KeyframeProperty::PositionX,
                        playhead_ns,
                    ),
                );
                self.position_y_slider.set_value(
                    c.value_for_phase1_property_at_timeline_ns(
                        Phase1KeyframeProperty::PositionY,
                        playhead_ns,
                    ),
                );
                self.title_entry.set_text(&c.title_text);
                self.title_x_slider.set_value(c.title_x);
                self.title_y_slider.set_value(c.title_y);
                self.speed_slider.set_value(c.speed);
                self.reverse_check.set_active(c.reverse);
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
                // Chroma Key
                self.chroma_key_enable.set_active(c.chroma_key_enabled);
                self.chroma_tolerance_slider
                    .set_value(c.chroma_key_tolerance as f64);
                self.chroma_softness_slider
                    .set_value(c.chroma_key_softness as f64);
                match c.chroma_key_color {
                    0x00FF00 => {
                        self.chroma_green_btn.set_active(true);
                        self.chroma_custom_color_row.set_visible(false);
                    }
                    0x0000FF => {
                        self.chroma_blue_btn.set_active(true);
                        self.chroma_custom_color_row.set_visible(false);
                    }
                    custom => {
                        self.chroma_custom_btn.set_active(true);
                        self.chroma_custom_color_row.set_visible(true);
                        let r = ((custom >> 16) & 0xFF) as f32 / 255.0;
                        let g = ((custom >> 8) & 0xFF) as f32 / 255.0;
                        let b = (custom & 0xFF) as f32 / 255.0;
                        self.chroma_color_btn
                            .set_rgba(&gdk4::RGBA::new(r, g, b, 1.0));
                    }
                }
                // Background Removal
                self.bg_removal_enable.set_active(c.bg_removal_enabled);
                self.bg_removal_threshold_slider
                    .set_value(c.bg_removal_threshold);
            }
            None => {
                self.name_entry.set_text("");
                self.clip_color_label_combo
                    .set_selected(clip_color_label_index(ClipColorLabel::None));
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
                self.temperature_slider.set_value(6500.0);
                self.tint_slider.set_value(0.0);
                self.denoise_slider.set_value(0.0);
                self.sharpness_slider.set_value(0.0);
                self.shadows_slider.set_value(0.0);
                self.midtones_slider.set_value(0.0);
                self.highlights_slider.set_value(0.0);
                self.volume_slider.set_value(0.0);
                self.pan_slider.set_value(0.0);
                self.crop_left_slider.set_value(0.0);
                self.crop_right_slider.set_value(0.0);
                self.crop_top_slider.set_value(0.0);
                self.crop_bottom_slider.set_value(0.0);
                self.rotate_spin.set_value(0.0);
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
                self.reverse_check.set_active(false);
                self.lut_path_label.set_text("None");
                self.lut_clear_btn.set_sensitive(false);
                // Chroma Key defaults
                self.chroma_key_enable.set_active(false);
                self.chroma_green_btn.set_active(true);
                self.chroma_custom_color_row.set_visible(false);
                self.chroma_tolerance_slider.set_value(0.3);
                self.chroma_softness_slider.set_value(0.1);
                // Background Removal defaults
                self.bg_removal_enable.set_active(false);
                self.bg_removal_threshold_slider.set_value(0.5);
            }
        }
        *self.updating.borrow_mut() = false;
    }

    /// Update the keyframe indicator label based on the playhead position.
    pub fn update_keyframe_indicator(&self, project: &Project, playhead_ns: u64) {
        let clip = self.selected_clip_id.borrow().clone().and_then(|id| {
            project
                .tracks
                .iter()
                .flat_map(|t| t.clips.iter())
                .find(|c| c.id == id)
                .cloned()
        });
        match clip {
            Some(c) => {
                let local = c.local_timeline_position_ns(playhead_ns);
                let tolerance = project.frame_rate.frame_duration_ns() / 2;
                if c.has_keyframe_at_local_ns(local, tolerance) {
                    self.keyframe_indicator_label.set_text("◆ Keyframe");
                    // Update dropdown to show the interpolation of the first matching keyframe
                    let all_kfs = [
                        &c.scale_keyframes[..],
                        &c.opacity_keyframes[..],
                        &c.position_x_keyframes[..],
                        &c.position_y_keyframes[..],
                        &c.volume_keyframes[..],
                    ];
                    for kfs in &all_kfs {
                        if let Some(kf) = kfs.iter().find(|kf| {
                            (kf.time_ns as i64 - local as i64).unsigned_abs() <= tolerance
                        }) {
                            let idx = match kf.interpolation {
                                KeyframeInterpolation::Linear => 0,
                                KeyframeInterpolation::EaseIn => 1,
                                KeyframeInterpolation::EaseOut => 2,
                                KeyframeInterpolation::EaseInOut => 3,
                            };
                            self.interp_dropdown.set_selected(idx);
                            break;
                        }
                    }
                } else {
                    self.keyframe_indicator_label.set_text("");
                }
                // Audio (volume) keyframe indicator
                if c.has_keyframe_at_local_ns_for_property(
                    Phase1KeyframeProperty::Volume,
                    local,
                    tolerance,
                ) {
                    self.audio_keyframe_indicator_label
                        .set_text("◆ Vol KF");
                } else {
                    self.audio_keyframe_indicator_label.set_text("");
                }
            }
            None => {
                self.keyframe_indicator_label.set_text("");
                self.audio_keyframe_indicator_label.set_text("");
            }
        }
    }
}

/// Build the inspector panel.
/// Returns `(widget, InspectorView)` — keep `InspectorView` and call `.update()` on selection changes.
///
/// - `on_clip_changed`: fired when the clip name is applied (triggers full project-changed cycle).
/// - `on_color_changed`: fired on every color/effects slider movement with
///   `(brightness, contrast, saturation, temperature, tint, denoise, sharpness, shadows, midtones, highlights)`;
///   should update the program player's video filter elements directly without a full pipeline reload.
/// - `on_audio_changed`: fired on every audio slider movement with `(clip_id, volume, pan)`.
pub fn build_inspector(
    project: Rc<RefCell<Project>>,
    on_clip_changed: impl Fn() + 'static,
    on_color_changed: impl Fn(f32, f32, f32, f32, f32, f32, f32, f32, f32, f32) + 'static,
    on_audio_changed: impl Fn(&str, f32, f32) + 'static,
    on_transform_changed: impl Fn(i32, i32, i32, i32, i32, bool, bool, f64, f64, f64) + 'static,
    on_title_changed: impl Fn(String, f64, f64) + 'static,
    on_speed_changed: impl Fn(f64) + 'static,
    on_lut_changed: impl Fn(Option<String>) + 'static,
    on_opacity_changed: impl Fn(f64) + 'static,
    on_reverse_changed: impl Fn(bool) + 'static,
    on_chroma_key_changed: impl Fn() + 'static,
    on_chroma_key_slider_changed: impl Fn(f32, f32) + 'static,
    on_bg_removal_changed: impl Fn() + 'static,
    current_playhead_ns: impl Fn() -> u64 + 'static,
    on_seek_to: impl Fn(u64) + 'static,
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

    row_label(&content_box, "Clip Color Label");
    let clip_color_label_combo = gtk4::DropDown::from_strings(&[
        "None", "Red", "Orange", "Yellow", "Green", "Teal", "Blue", "Purple", "Magenta",
    ]);
    clip_color_label_combo.set_selected(clip_color_label_index(ClipColorLabel::None));
    clip_color_label_combo.set_halign(gtk4::Align::Start);
    clip_color_label_combo.set_hexpand(true);
    content_box.append(&clip_color_label_combo);

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

    row_label(&color_inner, "Temperature (K)");
    let temperature_slider = Scale::with_range(Orientation::Horizontal, 2000.0, 10000.0, 100.0);
    temperature_slider.set_value(6500.0);
    temperature_slider.set_draw_value(true);
    temperature_slider.set_digits(0);
    temperature_slider.add_mark(6500.0, gtk4::PositionType::Bottom, None);
    color_inner.append(&temperature_slider);

    row_label(&color_inner, "Tint");
    let tint_slider = Scale::with_range(Orientation::Horizontal, -1.0, 1.0, 0.01);
    tint_slider.set_value(0.0);
    tint_slider.set_draw_value(true);
    tint_slider.set_digits(2);
    tint_slider.add_mark(0.0, gtk4::PositionType::Bottom, None);
    color_inner.append(&tint_slider);

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

    // ── Chroma Key section (Video + Image only) ──────────────────────────────
    let chroma_key_section = GBox::new(Orientation::Vertical, 8);
    content_box.append(&chroma_key_section);

    chroma_key_section.append(&Separator::new(Orientation::Horizontal));
    let chroma_key_expander = Expander::new(Some("Chroma Key"));
    chroma_key_expander.set_expanded(false);
    chroma_key_section.append(&chroma_key_expander);
    let chroma_key_inner = GBox::new(Orientation::Vertical, 8);
    chroma_key_expander.set_child(Some(&chroma_key_inner));

    let chroma_key_enable = CheckButton::with_label("Enable Chroma Key");
    chroma_key_inner.append(&chroma_key_enable);

    row_label(&chroma_key_inner, "Key Color");
    let chroma_key_color_row = GBox::new(Orientation::Horizontal, 8);
    let chroma_green_btn = gtk4::ToggleButton::with_label("Green");
    let chroma_blue_btn = gtk4::ToggleButton::with_label("Blue");
    let chroma_custom_btn = gtk4::ToggleButton::with_label("Custom");
    chroma_green_btn.set_active(true);
    chroma_blue_btn.set_group(Some(&chroma_green_btn));
    chroma_custom_btn.set_group(Some(&chroma_green_btn));
    chroma_key_color_row.append(&chroma_green_btn);
    chroma_key_color_row.append(&chroma_blue_btn);
    chroma_key_color_row.append(&chroma_custom_btn);
    chroma_key_inner.append(&chroma_key_color_row);

    let chroma_custom_color_row = GBox::new(Orientation::Horizontal, 8);
    let chroma_color_dialog = gtk4::ColorDialog::new();
    chroma_color_dialog.set_with_alpha(false);
    let chroma_color_btn = gtk4::ColorDialogButton::new(Some(chroma_color_dialog));
    chroma_color_btn.set_rgba(&gdk4::RGBA::new(0.0, 1.0, 0.0, 1.0));
    chroma_custom_color_row.append(&chroma_color_btn);
    chroma_key_inner.append(&chroma_custom_color_row);
    chroma_custom_color_row.set_visible(false);

    row_label(&chroma_key_inner, "Tolerance");
    let chroma_tolerance_slider = Scale::with_range(Orientation::Horizontal, 0.0, 1.0, 0.01);
    chroma_tolerance_slider.set_value(0.3);
    chroma_tolerance_slider.set_draw_value(true);
    chroma_tolerance_slider.set_digits(2);
    chroma_tolerance_slider.add_mark(0.3, gtk4::PositionType::Bottom, None);
    chroma_key_inner.append(&chroma_tolerance_slider);

    row_label(&chroma_key_inner, "Edge Softness");
    let chroma_softness_slider = Scale::with_range(Orientation::Horizontal, 0.0, 1.0, 0.01);
    chroma_softness_slider.set_value(0.1);
    chroma_softness_slider.set_draw_value(true);
    chroma_softness_slider.set_digits(2);
    chroma_softness_slider.add_mark(0.1, gtk4::PositionType::Bottom, None);
    chroma_key_inner.append(&chroma_softness_slider);

    // ── Background Removal section (Video + Image only) ──────────────────────
    let bg_removal_section = GBox::new(Orientation::Vertical, 8);
    content_box.append(&bg_removal_section);

    bg_removal_section.append(&Separator::new(Orientation::Horizontal));
    let bg_removal_expander = Expander::new(Some("Background Removal"));
    bg_removal_expander.set_expanded(false);
    bg_removal_section.append(&bg_removal_expander);
    let bg_removal_inner = GBox::new(Orientation::Vertical, 8);
    bg_removal_expander.set_child(Some(&bg_removal_inner));

    let bg_removal_enable = CheckButton::with_label("Enable Background Removal");
    bg_removal_inner.append(&bg_removal_enable);

    row_label(&bg_removal_inner, "Threshold");
    let bg_removal_threshold_slider = Scale::with_range(Orientation::Horizontal, 0.0, 1.0, 0.01);
    bg_removal_threshold_slider.set_value(0.5);
    bg_removal_threshold_slider.set_draw_value(true);
    bg_removal_threshold_slider.set_digits(2);
    bg_removal_threshold_slider.add_mark(0.5, gtk4::PositionType::Bottom, None);
    bg_removal_inner.append(&bg_removal_threshold_slider);

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
    let volume_slider =
        Scale::with_range(Orientation::Horizontal, VOLUME_DB_MIN, VOLUME_DB_MAX, 0.1);
    volume_slider.set_value(0.0);
    volume_slider.set_draw_value(true);
    volume_slider.set_digits(1);
    volume_slider.add_mark(VOLUME_DB_MIN, gtk4::PositionType::Bottom, Some("-100 dB"));
    volume_slider.add_mark(0.0, gtk4::PositionType::Bottom, Some("0 dB"));
    volume_slider.add_mark(VOLUME_DB_MAX, gtk4::PositionType::Bottom, Some("+12 dB"));
    audio_inner.append(&volume_slider);
    let volume_keyframe_row = GBox::new(Orientation::Horizontal, 6);
    let volume_set_keyframe_btn = Button::with_label("Set Volume Keyframe");
    let volume_remove_keyframe_btn = Button::with_label("Remove Volume Keyframe");
    volume_keyframe_row.append(&volume_set_keyframe_btn);
    volume_keyframe_row.append(&volume_remove_keyframe_btn);
    audio_inner.append(&volume_keyframe_row);

    // ── Audio keyframe navigation + animation mode ──
    let audio_keyframe_nav_row = GBox::new(Orientation::Horizontal, 4);
    let audio_prev_keyframe_btn = Button::with_label("◀ Prev KF");
    audio_prev_keyframe_btn.set_tooltip_text(Some("Jump to previous volume keyframe"));
    let audio_next_keyframe_btn = Button::with_label("Next KF ▶");
    audio_next_keyframe_btn.set_tooltip_text(Some("Jump to next volume keyframe"));
    let audio_keyframe_indicator_label = Label::new(None);
    audio_keyframe_indicator_label.add_css_class("dim-label");
    audio_keyframe_indicator_label.set_hexpand(true);
    audio_keyframe_indicator_label.set_halign(gtk4::Align::Center);
    audio_keyframe_nav_row.append(&audio_prev_keyframe_btn);
    audio_keyframe_nav_row.append(&audio_keyframe_indicator_label);
    audio_keyframe_nav_row.append(&audio_next_keyframe_btn);
    audio_inner.append(&audio_keyframe_nav_row);

    let audio_animation_mode_btn = gtk4::ToggleButton::with_label("⏺ Record Keyframes");
    audio_animation_mode_btn.set_tooltip_text(Some(
        "When active, volume slider changes auto-create keyframes (Shift+K)",
    ));
    audio_animation_mode_btn.set_active(false);
    audio_inner.append(&audio_animation_mode_btn);

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

    // ── Keyframe navigation + animation mode ──
    let keyframe_nav_row = GBox::new(Orientation::Horizontal, 4);
    let prev_keyframe_btn = Button::with_label("◀ Prev KF");
    prev_keyframe_btn.set_tooltip_text(Some("Jump to previous keyframe (Alt+Left)"));
    let next_keyframe_btn = Button::with_label("Next KF ▶");
    next_keyframe_btn.set_tooltip_text(Some("Jump to next keyframe (Alt+Right)"));
    let keyframe_indicator_label = Label::new(None);
    keyframe_indicator_label.add_css_class("dim-label");
    keyframe_indicator_label.set_hexpand(true);
    keyframe_indicator_label.set_halign(gtk4::Align::Center);
    keyframe_nav_row.append(&prev_keyframe_btn);
    keyframe_nav_row.append(&keyframe_indicator_label);
    keyframe_nav_row.append(&next_keyframe_btn);
    transform_inner.append(&keyframe_nav_row);

    let animation_mode = Rc::new(Cell::new(false));
    let animation_mode_btn = gtk4::ToggleButton::with_label("⏺ Record Keyframes");
    animation_mode_btn.set_tooltip_text(Some(
        "When active, transform drags and slider changes auto-create keyframes (Shift+K)",
    ));
    animation_mode_btn.set_active(false);
    transform_inner.append(&animation_mode_btn);

    let interp_row = GBox::new(Orientation::Horizontal, 4);
    let interp_label = Label::new(Some("Interpolation"));
    interp_label.set_halign(gtk4::Align::Start);
    let interp_dropdown = gtk4::DropDown::from_strings(&["Linear", "Ease In", "Ease Out", "Ease In/Out"]);
    interp_dropdown.set_selected(0);
    interp_dropdown.set_tooltip_text(Some("Interpolation mode for new keyframes"));
    interp_dropdown.set_hexpand(true);
    interp_row.append(&interp_label);
    interp_row.append(&interp_dropdown);
    transform_inner.append(&interp_row);

    transform_inner.append(&Separator::new(Orientation::Horizontal));

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
    let scale_keyframe_row = GBox::new(Orientation::Horizontal, 6);
    let scale_set_keyframe_btn = Button::with_label("Set Scale Keyframe");
    let scale_remove_keyframe_btn = Button::with_label("Remove Scale Keyframe");
    scale_keyframe_row.append(&scale_set_keyframe_btn);
    scale_keyframe_row.append(&scale_remove_keyframe_btn);
    transform_inner.append(&scale_keyframe_row);

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
    let opacity_keyframe_row = GBox::new(Orientation::Horizontal, 6);
    let opacity_set_keyframe_btn = Button::with_label("Set Opacity Keyframe");
    let opacity_remove_keyframe_btn = Button::with_label("Remove Opacity Keyframe");
    opacity_keyframe_row.append(&opacity_set_keyframe_btn);
    opacity_keyframe_row.append(&opacity_remove_keyframe_btn);
    transform_inner.append(&opacity_keyframe_row);

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
    let position_x_keyframe_row = GBox::new(Orientation::Horizontal, 6);
    let position_x_set_keyframe_btn = Button::with_label("Set Position X Keyframe");
    let position_x_remove_keyframe_btn = Button::with_label("Remove Position X Keyframe");
    position_x_keyframe_row.append(&position_x_set_keyframe_btn);
    position_x_keyframe_row.append(&position_x_remove_keyframe_btn);
    transform_inner.append(&position_x_keyframe_row);

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
    let position_y_keyframe_row = GBox::new(Orientation::Horizontal, 6);
    let position_y_set_keyframe_btn = Button::with_label("Set Position Y Keyframe");
    let position_y_remove_keyframe_btn = Button::with_label("Remove Position Y Keyframe");
    position_y_keyframe_row.append(&position_y_set_keyframe_btn);
    position_y_keyframe_row.append(&position_y_remove_keyframe_btn);
    transform_inner.append(&position_y_keyframe_row);

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
    let rotate_row = GBox::new(Orientation::Horizontal, 8);
    let rotate_value = Rc::new(Cell::new(0.0_f64));
    let rotate_dial = gtk4::DrawingArea::new();
    rotate_dial.set_content_width(72);
    rotate_dial.set_content_height(72);
    rotate_dial.set_hexpand(false);
    rotate_dial.set_vexpand(false);
    rotate_dial.set_tooltip_text(Some("Drag dial to rotate clip (−180° to +180°)"));
    {
        let rotate_value = rotate_value.clone();
        rotate_dial.set_draw_func(move |_da, cr, ww, wh| {
            let w = ww as f64;
            let h = wh as f64;
            let cx = w / 2.0;
            let cy = h / 2.0;
            let r = w.min(h) * 0.42;
            cr.save().ok();
            cr.set_source_rgba(0.15, 0.15, 0.15, 0.95);
            cr.arc(cx, cy, r, 0.0, std::f64::consts::TAU);
            cr.fill_preserve().ok();
            cr.set_source_rgba(0.85, 0.85, 0.85, 0.9);
            cr.set_line_width(1.5);
            cr.stroke().ok();
            // 0° marker (up)
            cr.move_to(cx, cy - r + 4.0);
            cr.line_to(cx, cy - r - 6.0);
            cr.set_source_rgba(1.0, 0.95, 0.3, 0.9);
            cr.set_line_width(2.0);
            cr.stroke().ok();
            // Needle
            let rad = (rotate_value.get() - 90.0).to_radians();
            let nx = cx + rad.cos() * (r - 8.0);
            let ny = cy + rad.sin() * (r - 8.0);
            cr.move_to(cx, cy);
            cr.line_to(nx, ny);
            cr.set_source_rgba(0.25, 0.55, 1.0, 1.0);
            cr.set_line_width(2.2);
            cr.stroke().ok();
            cr.restore().ok();
        });
    }
    let rotate_spin = gtk4::SpinButton::with_range(-180.0, 180.0, 1.0);
    rotate_spin.set_digits(0);
    rotate_spin.set_value(0.0);
    rotate_spin.set_hexpand(true);
    rotate_spin.set_tooltip_text(Some("Rotation angle in degrees"));
    rotate_row.append(&rotate_dial);
    rotate_row.append(&rotate_spin);
    transform_inner.append(&rotate_row);

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

    let reverse_check = CheckButton::with_label("Reverse (play clip backwards)");
    reverse_check.set_tooltip_text(Some(
        "Play this clip in reverse in Program Monitor preview and export. A ◀ badge appears on the timeline clip.",
    ));
    speed_inner.append(&reverse_check);

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

    let on_clip_changed: Rc<dyn Fn()> = Rc::new(on_clip_changed);
    let on_color_changed: Rc<dyn Fn(f32, f32, f32, f32, f32, f32, f32, f32, f32, f32)> =
        Rc::new(on_color_changed);
    let on_audio_changed: Rc<dyn Fn(&str, f32, f32)> = Rc::new(on_audio_changed);
    let on_transform_changed: Rc<dyn Fn(i32, i32, i32, i32, i32, bool, bool, f64, f64, f64)> =
        Rc::new(on_transform_changed);
    let on_title_changed: Rc<dyn Fn(String, f64, f64)> = Rc::new(on_title_changed);
    let on_speed_changed: Rc<dyn Fn(f64)> = Rc::new(on_speed_changed);
    let on_lut_changed: Rc<dyn Fn(Option<String>)> = Rc::new(on_lut_changed);
    let on_opacity_changed: Rc<dyn Fn(f64)> = Rc::new(on_opacity_changed);
    let on_reverse_changed: Rc<dyn Fn(bool)> = Rc::new(on_reverse_changed);
    let on_chroma_key_changed: Rc<dyn Fn()> = Rc::new(on_chroma_key_changed);
    let on_chroma_key_slider_changed: Rc<dyn Fn(f32, f32)> = Rc::new(on_chroma_key_slider_changed);
    let on_bg_removal_changed: Rc<dyn Fn()> = Rc::new(on_bg_removal_changed);
    let current_playhead_ns: Rc<dyn Fn() -> u64> = Rc::new(current_playhead_ns);
    let on_seek_to: Rc<dyn Fn(u64)> = Rc::new(on_seek_to);

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
    // with all ten current values so the program player can update its filters directly.
    fn connect_color_slider(
        slider: &Scale,
        project: Rc<RefCell<Project>>,
        selected_clip_id: Rc<RefCell<Option<String>>>,
        updating: Rc<RefCell<bool>>,
        on_color_changed: Rc<dyn Fn(f32, f32, f32, f32, f32, f32, f32, f32, f32, f32)>,
        brightness_slider: Scale,
        contrast_slider: Scale,
        saturation_slider: Scale,
        temperature_slider: Scale,
        tint_slider: Scale,
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
                let temp = temperature_slider.value() as f32;
                let tnt = tint_slider.value() as f32;
                let d = denoise_slider.value() as f32;
                let sh = sharpness_slider.value() as f32;
                let shd = shadows_slider.value() as f32;
                let mid = midtones_slider.value() as f32;
                let hil = highlights_slider.value() as f32;
                on_color_changed(b, c, sat, temp, tnt, d, sh, shd, mid, hil);
            }
        });
    }

    macro_rules! wire_color_slider {
        ($slider:expr, $apply:expr) => {
            connect_color_slider(
                &$slider,
                project.clone(),
                selected_clip_id.clone(),
                updating.clone(),
                on_color_changed.clone(),
                brightness_slider.clone(),
                contrast_slider.clone(),
                saturation_slider.clone(),
                temperature_slider.clone(),
                tint_slider.clone(),
                denoise_slider.clone(),
                sharpness_slider.clone(),
                shadows_slider.clone(),
                midtones_slider.clone(),
                highlights_slider.clone(),
                $apply,
            );
        };
    }

    wire_color_slider!(brightness_slider, |clip, v| clip.brightness = v);
    wire_color_slider!(contrast_slider, |clip, v| clip.contrast = v);
    wire_color_slider!(saturation_slider, |clip, v| clip.saturation = v);
    wire_color_slider!(temperature_slider, |clip, v| clip.temperature = v);
    wire_color_slider!(tint_slider, |clip, v| clip.tint = v);
    wire_color_slider!(denoise_slider, |clip, v| clip.denoise = v);
    wire_color_slider!(sharpness_slider, |clip, v| clip.sharpness = v);
    wire_color_slider!(shadows_slider, |clip, v| clip.shadows = v);
    wire_color_slider!(midtones_slider, |clip, v| clip.midtones = v);
    wire_color_slider!(highlights_slider, |clip, v| clip.highlights = v);

    // Wire audio sliders
    {
        let project = project.clone();
        let selected_clip_id = selected_clip_id.clone();
        let updating = updating.clone();
        let on_clip_changed = on_clip_changed.clone();
        clip_color_label_combo.connect_selected_notify(move |combo| {
            if *updating.borrow() {
                return;
            }
            let id = selected_clip_id.borrow().clone();
            if let Some(ref clip_id) = id {
                let color_label = clip_color_label_from_index(combo.selected());
                {
                    let mut proj = project.borrow_mut();
                    for track in &mut proj.tracks {
                        if let Some(clip) = track.clips.iter_mut().find(|c| &c.id == clip_id) {
                            clip.color_label = color_label;
                            proj.dirty = true;
                            break;
                        }
                    }
                }
                on_clip_changed();
            }
        });
    }

    {
        let project = project.clone();
        let selected_clip_id = selected_clip_id.clone();
        let updating = updating.clone();
        let on_audio_changed = on_audio_changed.clone();
        let pan_slider_cb = pan_slider.clone();
        let animation_mode = animation_mode.clone();
        let current_playhead_ns = current_playhead_ns.clone();
        let on_clip_changed = on_clip_changed.clone();
        let interp_dropdown = interp_dropdown.clone();
        volume_slider.connect_value_changed(move |s| {
            if *updating.borrow() {
                return;
            }
            let linear_vol = db_to_linear_volume(s.value()) as f32;
            let id = selected_clip_id.borrow().clone();
            if let Some(ref clip_id) = id {
                {
                    let mut proj = project.borrow_mut();
                    for track in &mut proj.tracks {
                        if let Some(clip) = track.clips.iter_mut().find(|c| &c.id == clip_id) {
                            let has_kfs = !clip.volume_keyframes.is_empty();
                            if animation_mode.get() || has_kfs {
                                let interp = interp_idx_to_enum(interp_dropdown.selected());
                                clip.upsert_phase1_keyframe_at_timeline_ns_with_interp(
                                    Phase1KeyframeProperty::Volume,
                                    current_playhead_ns(),
                                    linear_vol as f64,
                                    interp,
                                );
                            } else {
                                clip.volume = linear_vol;
                            }
                            proj.dirty = true;
                            break;
                        }
                    }
                }
                if animation_mode.get() || {
                    let proj = project.borrow();
                    proj.tracks.iter().flat_map(|t| t.clips.iter())
                        .find(|c| &c.id == clip_id)
                        .map_or(false, |c| !c.volume_keyframes.is_empty())
                } {
                    on_clip_changed();
                } else {
                    on_audio_changed(clip_id, linear_vol, pan_slider_cb.value() as f32);
                }
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
                    clip_id,
                    db_to_linear_volume(volume_slider_cb.value()) as f32,
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
        rotate_s: gtk4::SpinButton,
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
                let rot = rotate_s.value().round() as i32;
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
        rotate_spin.clone(),
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
        rotate_spin.clone(),
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
        rotate_spin.clone(),
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
        rotate_spin.clone(),
        flip_h_btn.clone(),
        flip_v_btn.clone(),
        scale_slider.clone(),
        position_x_slider.clone(),
        position_y_slider.clone(),
        |clip, v| clip.crop_bottom = v,
    );

    // Wire rotate dial + numeric spin
    {
        let project = project.clone();
        let selected_clip_id = selected_clip_id.clone();
        let updating = updating.clone();
        let on_transform_changed = on_transform_changed.clone();
        let rotate_value = rotate_value.clone();
        let rotate_dial = rotate_dial.clone();
        let crop_left_s = crop_left_slider.clone();
        let crop_right_s = crop_right_slider.clone();
        let crop_top_s = crop_top_slider.clone();
        let crop_bottom_s = crop_bottom_slider.clone();
        let flip_h_b = flip_h_btn.clone();
        let flip_v_b = flip_v_btn.clone();
        let scale_s = scale_slider.clone();
        let pos_x_s = position_x_slider.clone();
        let pos_y_s = position_y_slider.clone();
        rotate_spin.connect_value_changed(move |spin| {
            let rot = spin.value().clamp(-180.0, 180.0).round() as i32;
            rotate_value.set(rot as f64);
            rotate_dial.queue_draw();
            if *updating.borrow() {
                return;
            }
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
    {
        let rotate_spin = rotate_spin.clone();
        let rotate_dial = rotate_dial.clone();
        let updating = updating.clone();
        let rotate_dial_for_click = rotate_dial.clone();
        let click = gtk4::GestureClick::new();
        click.set_button(1);
        click.connect_pressed(move |_g, _n, x, y| {
            if *updating.borrow() {
                return;
            }
            let rot = dial_point_to_degrees(
                x,
                y,
                rotate_dial_for_click.width() as f64,
                rotate_dial_for_click.height() as f64,
            );
            rotate_spin.set_value(rot as f64);
        });
        rotate_dial.add_controller(click);
    }
    {
        let rotate_spin = rotate_spin.clone();
        let rotate_dial = rotate_dial.clone();
        let updating = updating.clone();
        let drag_start = Rc::new(RefCell::new((0.0_f64, 0.0_f64)));
        let drag = gtk4::GestureDrag::new();
        drag.set_button(1);
        {
            let drag_start = drag_start.clone();
            let rotate_spin = rotate_spin.clone();
            let rotate_dial = rotate_dial.clone();
            let updating = updating.clone();
            drag.connect_drag_begin(move |_g, x, y| {
                if *updating.borrow() {
                    return;
                }
                *drag_start.borrow_mut() = (x, y);
                let rot = dial_point_to_degrees(
                    x,
                    y,
                    rotate_dial.width() as f64,
                    rotate_dial.height() as f64,
                );
                rotate_spin.set_value(rot as f64);
            });
        }
        {
            let drag_start = drag_start.clone();
            let rotate_spin = rotate_spin.clone();
            let rotate_dial = rotate_dial.clone();
            let updating = updating.clone();
            drag.connect_drag_update(move |_g, off_x, off_y| {
                if *updating.borrow() {
                    return;
                }
                let (sx, sy) = *drag_start.borrow();
                let x = sx + off_x;
                let y = sy + off_y;
                let rot = dial_point_to_degrees(
                    x,
                    y,
                    rotate_dial.width() as f64,
                    rotate_dial.height() as f64,
                );
                rotate_spin.set_value(rot as f64);
            });
        }
        rotate_dial.add_controller(drag);
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
        let rotate_s = rotate_spin.clone();
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
                let rot = rotate_s.value().round() as i32;
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
        let rotate_s = rotate_spin.clone();
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
                let rot = rotate_s.value().round() as i32;
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
        let rotate_s = rotate_spin.clone();
        let flip_h_b = flip_h_btn.clone();
        let flip_v_b = flip_v_btn.clone();
        let pos_x_s = position_x_slider.clone();
        let pos_y_s = position_y_slider.clone();
        let scale_s2 = scale_slider.clone();
        let animation_mode = animation_mode.clone();
        let current_playhead_ns = current_playhead_ns.clone();
        let on_clip_changed = on_clip_changed.clone();
        let interp_dropdown_s = interp_dropdown.clone();
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
                            let has_kfs = !clip.scale_keyframes.is_empty();
                            if animation_mode.get() || has_kfs {
                                let interp = interp_idx_to_enum(interp_dropdown_s.selected());
                                clip.upsert_phase1_keyframe_at_timeline_ns_with_interp(
                                    Phase1KeyframeProperty::Scale,
                                    current_playhead_ns(),
                                    sc,
                                    interp,
                                );
                            } else {
                                clip.scale = sc;
                            }
                            proj.dirty = true;
                            break;
                        }
                    }
                }
                if animation_mode.get() || {
                    let proj = project.borrow();
                    proj.tracks.iter().flat_map(|t| t.clips.iter())
                        .find(|c| &c.id == clip_id)
                        .map_or(false, |c| !c.scale_keyframes.is_empty())
                } {
                    on_clip_changed();
                } else {
                    let cl = crop_left_s.value() as i32;
                    let cr = crop_right_s.value() as i32;
                    let ct = crop_top_s.value() as i32;
                    let cb = crop_bottom_s.value() as i32;
                    let rot = rotate_s.value().round() as i32;
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
            }
        });
        let _ = scale_s2; // silence unused warning
    }
    {
        let project = project.clone();
        let selected_clip_id = selected_clip_id.clone();
        let updating = updating.clone();
        let on_opacity_changed = on_opacity_changed.clone();
        let animation_mode = animation_mode.clone();
        let current_playhead_ns = current_playhead_ns.clone();
        let on_clip_changed = on_clip_changed.clone();
        let interp_dropdown_o = interp_dropdown.clone();
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
                            let has_kfs = !clip.opacity_keyframes.is_empty();
                            if animation_mode.get() || has_kfs {
                                let interp = interp_idx_to_enum(interp_dropdown_o.selected());
                                clip.upsert_phase1_keyframe_at_timeline_ns_with_interp(
                                    Phase1KeyframeProperty::Opacity,
                                    current_playhead_ns(),
                                    opacity,
                                    interp,
                                );
                            } else {
                                clip.opacity = opacity;
                            }
                            proj.dirty = true;
                            break;
                        }
                    }
                }
                if animation_mode.get() || {
                    let proj = project.borrow();
                    proj.tracks.iter().flat_map(|t| t.clips.iter())
                        .find(|c| &c.id == clip_id)
                        .map_or(false, |c| !c.opacity_keyframes.is_empty())
                } {
                    on_clip_changed();
                } else {
                    on_opacity_changed(opacity);
                }
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
        let rotate_s = rotate_spin.clone();
        let flip_h_b = flip_h_btn.clone();
        let flip_v_b = flip_v_btn.clone();
        let scale_s = scale_slider.clone();
        let pos_y_s = position_y_slider.clone();
        let pos_x_s2 = position_x_slider.clone();
        let animation_mode = animation_mode.clone();
        let current_playhead_ns = current_playhead_ns.clone();
        let on_clip_changed = on_clip_changed.clone();
        let interp_dropdown_px = interp_dropdown.clone();
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
                            let has_kfs = !clip.position_x_keyframes.is_empty();
                            if animation_mode.get() || has_kfs {
                                let interp = interp_idx_to_enum(interp_dropdown_px.selected());
                                clip.upsert_phase1_keyframe_at_timeline_ns_with_interp(
                                    Phase1KeyframeProperty::PositionX,
                                    current_playhead_ns(),
                                    px,
                                    interp,
                                );
                            } else {
                                clip.position_x = px;
                            }
                            proj.dirty = true;
                            break;
                        }
                    }
                }
                if animation_mode.get() || {
                    let proj = project.borrow();
                    proj.tracks.iter().flat_map(|t| t.clips.iter())
                        .find(|c| &c.id == clip_id)
                        .map_or(false, |c| !c.position_x_keyframes.is_empty())
                } {
                    on_clip_changed();
                } else {
                    let cl = crop_left_s.value() as i32;
                    let cr = crop_right_s.value() as i32;
                    let ct = crop_top_s.value() as i32;
                    let cb = crop_bottom_s.value() as i32;
                    let rot = rotate_s.value().round() as i32;
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
        let rotate_s = rotate_spin.clone();
        let flip_h_b = flip_h_btn.clone();
        let flip_v_b = flip_v_btn.clone();
        let scale_s = scale_slider.clone();
        let pos_x_s = position_x_slider.clone();
        let pos_y_s2 = position_y_slider.clone();
        let animation_mode = animation_mode.clone();
        let current_playhead_ns = current_playhead_ns.clone();
        let on_clip_changed = on_clip_changed.clone();
        let interp_dropdown_py = interp_dropdown.clone();
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
                            let has_kfs = !clip.position_y_keyframes.is_empty();
                            if animation_mode.get() || has_kfs {
                                let interp = interp_idx_to_enum(interp_dropdown_py.selected());
                                clip.upsert_phase1_keyframe_at_timeline_ns_with_interp(
                                    Phase1KeyframeProperty::PositionY,
                                    current_playhead_ns(),
                                    py,
                                    interp,
                                );
                            } else {
                                clip.position_y = py;
                            }
                            proj.dirty = true;
                            break;
                        }
                    }
                }
                if animation_mode.get() || {
                    let proj = project.borrow();
                    proj.tracks.iter().flat_map(|t| t.clips.iter())
                        .find(|c| &c.id == clip_id)
                        .map_or(false, |c| !c.position_y_keyframes.is_empty())
                } {
                    on_clip_changed();
                } else {
                    let cl = crop_left_s.value() as i32;
                    let cr = crop_right_s.value() as i32;
                    let ct = crop_top_s.value() as i32;
                    let cb = crop_bottom_s.value() as i32;
                    let rot = rotate_s.value().round() as i32;
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
            }
        });
        let _ = pos_y_s2;
    }

    fn connect_phase1_keyframe_buttons(
        set_btn: &Button,
        remove_btn: &Button,
        property: Phase1KeyframeProperty,
        project: Rc<RefCell<Project>>,
        selected_clip_id: Rc<RefCell<Option<String>>>,
        updating: Rc<RefCell<bool>>,
        current_playhead_ns: Rc<dyn Fn() -> u64>,
        on_clip_changed: Rc<dyn Fn()>,
        value_provider: Rc<dyn Fn() -> f64>,
        interp_provider: Rc<dyn Fn() -> KeyframeInterpolation>,
    ) {
        set_btn.connect_clicked({
            let project = project.clone();
            let selected_clip_id = selected_clip_id.clone();
            let updating = updating.clone();
            let current_playhead_ns = current_playhead_ns.clone();
            let on_clip_changed = on_clip_changed.clone();
            let value_provider = value_provider.clone();
            let interp_provider = interp_provider.clone();
            move |_| {
                if *updating.borrow() {
                    return;
                }
                let Some(clip_id) = selected_clip_id.borrow().clone() else {
                    return;
                };
                let timeline_pos_ns = current_playhead_ns();
                let value = value_provider();
                let interp = interp_provider();
                let mut changed = false;
                {
                    let mut proj = project.borrow_mut();
                    for track in &mut proj.tracks {
                        if let Some(clip) = track.clips.iter_mut().find(|c| c.id == clip_id) {
                            clip.upsert_phase1_keyframe_at_timeline_ns_with_interp(
                                property,
                                timeline_pos_ns,
                                value,
                                interp,
                            );
                            proj.dirty = true;
                            changed = true;
                            break;
                        }
                    }
                }
                if changed {
                    on_clip_changed();
                }
            }
        });

        remove_btn.connect_clicked({
            let project = project.clone();
            let selected_clip_id = selected_clip_id.clone();
            let updating = updating.clone();
            let current_playhead_ns = current_playhead_ns.clone();
            let on_clip_changed = on_clip_changed.clone();
            move |_| {
                if *updating.borrow() {
                    return;
                }
                let Some(clip_id) = selected_clip_id.borrow().clone() else {
                    return;
                };
                let timeline_pos_ns = current_playhead_ns();
                let mut removed = false;
                {
                    let mut proj = project.borrow_mut();
                    for track in &mut proj.tracks {
                        if let Some(clip) = track.clips.iter_mut().find(|c| c.id == clip_id) {
                            removed = clip
                                .remove_phase1_keyframe_at_timeline_ns(property, timeline_pos_ns);
                            if removed {
                                proj.dirty = true;
                            }
                            break;
                        }
                    }
                }
                if removed {
                    on_clip_changed();
                }
            }
        });
    }

    let interp_provider: Rc<dyn Fn() -> KeyframeInterpolation> = Rc::new({
        let interp_dropdown = interp_dropdown.clone();
        move || {
            match interp_dropdown.selected() {
                1 => KeyframeInterpolation::EaseIn,
                2 => KeyframeInterpolation::EaseOut,
                3 => KeyframeInterpolation::EaseInOut,
                _ => KeyframeInterpolation::Linear,
            }
        }
    });

    connect_phase1_keyframe_buttons(
        &scale_set_keyframe_btn,
        &scale_remove_keyframe_btn,
        Phase1KeyframeProperty::Scale,
        project.clone(),
        selected_clip_id.clone(),
        updating.clone(),
        current_playhead_ns.clone(),
        on_clip_changed.clone(),
        Rc::new({
            let scale_slider = scale_slider.clone();
            move || scale_slider.value()
        }),
        interp_provider.clone(),
    );
    connect_phase1_keyframe_buttons(
        &opacity_set_keyframe_btn,
        &opacity_remove_keyframe_btn,
        Phase1KeyframeProperty::Opacity,
        project.clone(),
        selected_clip_id.clone(),
        updating.clone(),
        current_playhead_ns.clone(),
        on_clip_changed.clone(),
        Rc::new({
            let opacity_slider = opacity_slider.clone();
            move || opacity_slider.value().clamp(0.0, 1.0)
        }),
        interp_provider.clone(),
    );
    connect_phase1_keyframe_buttons(
        &position_x_set_keyframe_btn,
        &position_x_remove_keyframe_btn,
        Phase1KeyframeProperty::PositionX,
        project.clone(),
        selected_clip_id.clone(),
        updating.clone(),
        current_playhead_ns.clone(),
        on_clip_changed.clone(),
        Rc::new({
            let position_x_slider = position_x_slider.clone();
            move || position_x_slider.value()
        }),
        interp_provider.clone(),
    );
    connect_phase1_keyframe_buttons(
        &position_y_set_keyframe_btn,
        &position_y_remove_keyframe_btn,
        Phase1KeyframeProperty::PositionY,
        project.clone(),
        selected_clip_id.clone(),
        updating.clone(),
        current_playhead_ns.clone(),
        on_clip_changed.clone(),
        Rc::new({
            let position_y_slider = position_y_slider.clone();
            move || position_y_slider.value()
        }),
        interp_provider.clone(),
    );
    connect_phase1_keyframe_buttons(
        &volume_set_keyframe_btn,
        &volume_remove_keyframe_btn,
        Phase1KeyframeProperty::Volume,
        project.clone(),
        selected_clip_id.clone(),
        updating.clone(),
        current_playhead_ns.clone(),
        on_clip_changed.clone(),
        Rc::new({
            let volume_slider = volume_slider.clone();
            move || db_to_linear_volume(volume_slider.value())
        }),
        interp_provider.clone(),
    );

    // ── Keyframe navigation button wiring ──
    prev_keyframe_btn.connect_clicked({
        let project = project.clone();
        let selected_clip_id = selected_clip_id.clone();
        let current_playhead_ns = current_playhead_ns.clone();
        let on_seek_to = on_seek_to.clone();
        move |_| {
            let Some(clip_id) = selected_clip_id.borrow().clone() else { return };
            let playhead = current_playhead_ns();
            let proj = project.borrow();
            for track in &proj.tracks {
                if let Some(clip) = track.clips.iter().find(|c| c.id == clip_id) {
                    let local = clip.local_timeline_position_ns(playhead);
                    if let Some(prev_local) = clip.prev_keyframe_local_ns(local) {
                        let timeline_ns = clip.timeline_start.saturating_add(prev_local);
                        on_seek_to(timeline_ns);
                    }
                    break;
                }
            }
        }
    });
    next_keyframe_btn.connect_clicked({
        let project = project.clone();
        let selected_clip_id = selected_clip_id.clone();
        let current_playhead_ns = current_playhead_ns.clone();
        let on_seek_to = on_seek_to.clone();
        move |_| {
            let Some(clip_id) = selected_clip_id.borrow().clone() else { return };
            let playhead = current_playhead_ns();
            let proj = project.borrow();
            for track in &proj.tracks {
                if let Some(clip) = track.clips.iter().find(|c| c.id == clip_id) {
                    let local = clip.local_timeline_position_ns(playhead);
                    if let Some(next_local) = clip.next_keyframe_local_ns(local) {
                        let timeline_ns = clip.timeline_start.saturating_add(next_local);
                        on_seek_to(timeline_ns);
                    }
                    break;
                }
            }
        }
    });

    // ── Animation mode toggle wiring (synced between transform + audio sections) ──
    animation_mode_btn.connect_toggled({
        let animation_mode = animation_mode.clone();
        let audio_btn = audio_animation_mode_btn.clone();
        move |btn| {
            animation_mode.set(btn.is_active());
            if audio_btn.is_active() != btn.is_active() {
                audio_btn.set_active(btn.is_active());
            }
        }
    });
    audio_animation_mode_btn.connect_toggled({
        let animation_mode = animation_mode.clone();
        let transform_btn = animation_mode_btn.clone();
        move |btn| {
            animation_mode.set(btn.is_active());
            if transform_btn.is_active() != btn.is_active() {
                transform_btn.set_active(btn.is_active());
            }
        }
    });

    // ── Audio (volume) keyframe navigation ──
    audio_prev_keyframe_btn.connect_clicked({
        let project = project.clone();
        let selected_clip_id = selected_clip_id.clone();
        let current_playhead_ns = current_playhead_ns.clone();
        let on_seek_to = on_seek_to.clone();
        move |_| {
            let Some(clip_id) = selected_clip_id.borrow().clone() else { return };
            let playhead = current_playhead_ns();
            let proj = project.borrow();
            for track in &proj.tracks {
                if let Some(clip) = track.clips.iter().find(|c| c.id == clip_id) {
                    let local = clip.local_timeline_position_ns(playhead);
                    if let Some(prev_local) = clip.prev_keyframe_local_ns_for_property(
                        Phase1KeyframeProperty::Volume,
                        local,
                    ) {
                        let timeline_ns = clip.timeline_start.saturating_add(prev_local);
                        on_seek_to(timeline_ns);
                    }
                    break;
                }
            }
        }
    });
    audio_next_keyframe_btn.connect_clicked({
        let project = project.clone();
        let selected_clip_id = selected_clip_id.clone();
        let current_playhead_ns = current_playhead_ns.clone();
        let on_seek_to = on_seek_to.clone();
        move |_| {
            let Some(clip_id) = selected_clip_id.borrow().clone() else { return };
            let playhead = current_playhead_ns();
            let proj = project.borrow();
            for track in &proj.tracks {
                if let Some(clip) = track.clips.iter().find(|c| c.id == clip_id) {
                    let local = clip.local_timeline_position_ns(playhead);
                    if let Some(next_local) = clip.next_keyframe_local_ns_for_property(
                        Phase1KeyframeProperty::Volume,
                        local,
                    ) {
                        let timeline_ns = clip.timeline_start.saturating_add(next_local);
                        on_seek_to(timeline_ns);
                    }
                    break;
                }
            }
        }
    });

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

    // Reverse checkbox
    {
        let project = project.clone();
        let selected_clip_id = selected_clip_id.clone();
        let updating = updating.clone();
        let on_reverse_changed = on_reverse_changed.clone();
        reverse_check.connect_toggled(move |btn| {
            if *updating.borrow() {
                return;
            }
            let reversed = btn.is_active();
            if let Some(ref id) = *selected_clip_id.borrow() {
                let mut proj = project.borrow_mut();
                let mut found = false;
                for track in &mut proj.tracks {
                    for clip in &mut track.clips {
                        if clip.id == *id {
                            clip.reverse = reversed;
                            found = true;
                        }
                    }
                }
                if found {
                    proj.dirty = true;
                }
            }
            on_reverse_changed(reversed);
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

    // Chroma Key enable toggle — toggling on/off changes pipeline topology → full rebuild
    {
        let project = project.clone();
        let selected_clip_id = selected_clip_id.clone();
        let updating = updating.clone();
        let on_chroma_key_changed = on_chroma_key_changed.clone();
        chroma_key_enable.connect_toggled(move |btn| {
            if *updating.borrow() {
                return;
            }
            let enabled = btn.is_active();
            let id = selected_clip_id.borrow().clone();
            if let Some(ref clip_id) = id {
                {
                    let mut proj = project.borrow_mut();
                    for track in &mut proj.tracks {
                        if let Some(clip) = track.clips.iter_mut().find(|c| &c.id == clip_id) {
                            clip.chroma_key_enabled = enabled;
                            proj.dirty = true;
                            break;
                        }
                    }
                }
                on_chroma_key_changed();
            }
        });
    }

    // Chroma Key color preset buttons
    {
        let project = project.clone();
        let selected_clip_id = selected_clip_id.clone();
        let updating = updating.clone();
        let on_chroma_key_changed = on_chroma_key_changed.clone();
        let custom_row = chroma_custom_color_row.clone();
        chroma_green_btn.connect_toggled(move |btn| {
            if *updating.borrow() || !btn.is_active() {
                return;
            }
            custom_row.set_visible(false);
            let id = selected_clip_id.borrow().clone();
            if let Some(ref clip_id) = id {
                {
                    let mut proj = project.borrow_mut();
                    for track in &mut proj.tracks {
                        if let Some(clip) = track.clips.iter_mut().find(|c| &c.id == clip_id) {
                            clip.chroma_key_color = 0x00FF00;
                            proj.dirty = true;
                            break;
                        }
                    }
                }
                on_chroma_key_changed();
            }
        });
    }
    {
        let project = project.clone();
        let selected_clip_id = selected_clip_id.clone();
        let updating = updating.clone();
        let on_chroma_key_changed = on_chroma_key_changed.clone();
        let custom_row = chroma_custom_color_row.clone();
        chroma_blue_btn.connect_toggled(move |btn| {
            if *updating.borrow() || !btn.is_active() {
                return;
            }
            custom_row.set_visible(false);
            let id = selected_clip_id.borrow().clone();
            if let Some(ref clip_id) = id {
                {
                    let mut proj = project.borrow_mut();
                    for track in &mut proj.tracks {
                        if let Some(clip) = track.clips.iter_mut().find(|c| &c.id == clip_id) {
                            clip.chroma_key_color = 0x0000FF;
                            proj.dirty = true;
                            break;
                        }
                    }
                }
                on_chroma_key_changed();
            }
        });
    }
    {
        let custom_row = chroma_custom_color_row.clone();
        let updating = updating.clone();
        chroma_custom_btn.connect_toggled(move |btn| {
            if *updating.borrow() {
                return;
            }
            custom_row.set_visible(btn.is_active());
        });
    }

    // Chroma Key color picker — ColorDialogButton rgba notify
    {
        let project = project.clone();
        let selected_clip_id = selected_clip_id.clone();
        let updating = updating.clone();
        let on_chroma_key_changed = on_chroma_key_changed.clone();
        chroma_color_btn.connect_rgba_notify(move |btn| {
            if *updating.borrow() {
                return;
            }
            let rgba = btn.rgba();
            let r = (rgba.red() * 255.0).round() as u32;
            let g = (rgba.green() * 255.0).round() as u32;
            let b = (rgba.blue() * 255.0).round() as u32;
            let color = (r << 16) | (g << 8) | b;
            let id = selected_clip_id.borrow().clone();
            if let Some(ref clip_id) = id {
                {
                    let mut proj = project.borrow_mut();
                    for track in &mut proj.tracks {
                        if let Some(clip) = track.clips.iter_mut().find(|c| &c.id == clip_id) {
                            clip.chroma_key_color = color;
                            proj.dirty = true;
                            break;
                        }
                    }
                }
                on_chroma_key_changed();
            }
        });
    }

    // Chroma Key tolerance slider — live property update (no pipeline rebuild)
    {
        let project = project.clone();
        let selected_clip_id = selected_clip_id.clone();
        let updating = updating.clone();
        let on_chroma_key_slider_changed = on_chroma_key_slider_changed.clone();
        let softness_slider = chroma_softness_slider.clone();
        chroma_tolerance_slider.connect_value_changed(move |s| {
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
                            clip.chroma_key_tolerance = val;
                            proj.dirty = true;
                            break;
                        }
                    }
                }
                on_chroma_key_slider_changed(val, softness_slider.value() as f32);
            }
        });
    }

    // Chroma Key softness slider — live property update (no pipeline rebuild)
    {
        let project = project.clone();
        let selected_clip_id = selected_clip_id.clone();
        let updating = updating.clone();
        let on_chroma_key_slider_changed = on_chroma_key_slider_changed.clone();
        let tolerance_slider = chroma_tolerance_slider.clone();
        chroma_softness_slider.connect_value_changed(move |s| {
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
                            clip.chroma_key_softness = val;
                            proj.dirty = true;
                            break;
                        }
                    }
                }
                on_chroma_key_slider_changed(tolerance_slider.value() as f32, val);
            }
        });
    }

    // ── Background Removal signals ───────────────────────────────────────────
    {
        let project = project.clone();
        let updating = updating.clone();
        let selected_clip_id = selected_clip_id.clone();
        let on_bg_removal_changed = on_bg_removal_changed.clone();
        bg_removal_enable.connect_toggled(move |btn| {
            if *updating.borrow() {
                return;
            }
            let enabled = btn.is_active();
            let id = selected_clip_id.borrow().clone();
            if let Some(ref clip_id) = id {
                {
                    let mut proj = project.borrow_mut();
                    for track in &mut proj.tracks {
                        if let Some(clip) = track.clips.iter_mut().find(|c| &c.id == clip_id) {
                            clip.bg_removal_enabled = enabled;
                            proj.dirty = true;
                            break;
                        }
                    }
                }
                on_bg_removal_changed();
            }
        });
    }
    {
        let project = project.clone();
        let updating = updating.clone();
        let selected_clip_id = selected_clip_id.clone();
        let on_bg_removal_changed = on_bg_removal_changed.clone();
        bg_removal_threshold_slider.connect_value_changed(move |s| {
            if *updating.borrow() {
                return;
            }
            let val = s.value();
            let id = selected_clip_id.borrow().clone();
            if let Some(ref clip_id) = id {
                {
                    let mut proj = project.borrow_mut();
                    for track in &mut proj.tracks {
                        if let Some(clip) = track.clips.iter_mut().find(|c| &c.id == clip_id) {
                            clip.bg_removal_threshold = val;
                            proj.dirty = true;
                            break;
                        }
                    }
                }
                on_bg_removal_changed();
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
        clip_color_label_combo,
        brightness_slider,
        contrast_slider,
        saturation_slider,
        temperature_slider,
        tint_slider,
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
        rotate_spin,
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
        reverse_check,
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
        chroma_key_section,
        chroma_key_enable,
        chroma_green_btn,
        chroma_blue_btn,
        chroma_custom_btn,
        chroma_color_btn,
        chroma_custom_color_row,
        chroma_tolerance_slider,
        chroma_softness_slider,
        bg_removal_section,
        bg_removal_enable,
        bg_removal_threshold_slider,
        bg_removal_model_available: Cell::new(false),
        keyframe_indicator_label,
        animation_mode,
        animation_mode_btn,
        interp_dropdown,
        audio_keyframe_indicator_label,
        audio_animation_mode_btn,
    });

    (vbox, view)
}

fn dial_point_to_degrees(x: f64, y: f64, width: f64, height: f64) -> i32 {
    let cx = width / 2.0;
    let cy = height / 2.0;
    let mut deg = ((y - cy).atan2(x - cx).to_degrees() + 90.0).rem_euclid(360.0);
    if deg > 180.0 {
        deg -= 360.0;
    }
    deg.round().clamp(-180.0, 180.0) as i32
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

fn clip_color_label_index(label: ClipColorLabel) -> u32 {
    match label {
        ClipColorLabel::None => 0,
        ClipColorLabel::Red => 1,
        ClipColorLabel::Orange => 2,
        ClipColorLabel::Yellow => 3,
        ClipColorLabel::Green => 4,
        ClipColorLabel::Teal => 5,
        ClipColorLabel::Blue => 6,
        ClipColorLabel::Purple => 7,
        ClipColorLabel::Magenta => 8,
    }
}

fn clip_color_label_from_index(index: u32) -> ClipColorLabel {
    match index {
        1 => ClipColorLabel::Red,
        2 => ClipColorLabel::Orange,
        3 => ClipColorLabel::Yellow,
        4 => ClipColorLabel::Green,
        5 => ClipColorLabel::Teal,
        6 => ClipColorLabel::Blue,
        7 => ClipColorLabel::Purple,
        8 => ClipColorLabel::Magenta,
        _ => ClipColorLabel::None,
    }
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
