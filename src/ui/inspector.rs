use crate::model::clip::{
    ClipColorLabel, KeyframeInterpolation, NumericKeyframe, Phase1KeyframeProperty,
};
use crate::model::project::Project;
use gdk4;
use gio;
use gtk4::prelude::*;
use gtk4::{
    self as gtk, Box as GBox, Button, CheckButton, DropDown, Entry, Expander, Label, Orientation,
    Scale, Separator, StringList,
};
use std::cell::{Cell, RefCell};
use std::collections::HashSet;
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
    pub path_status_value: Label,
    pub relink_btn: gtk4::Button,
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
    // Denoise / sharpness / blur sliders
    pub denoise_slider: Scale,
    pub sharpness_slider: Scale,
    pub blur_slider: Scale,
    // Video stabilization (export-only)
    pub vidstab_check: gtk4::CheckButton,
    pub vidstab_slider: Scale,
    // Grading sliders
    pub shadows_slider: Scale,
    pub midtones_slider: Scale,
    pub highlights_slider: Scale,
    // FCP Color Adjustments extended fields
    pub exposure_slider: Scale,
    pub black_point_slider: Scale,
    pub highlights_warmth_slider: Scale,
    pub highlights_tint_slider: Scale,
    pub midtones_warmth_slider: Scale,
    pub midtones_tint_slider: Scale,
    pub shadows_warmth_slider: Scale,
    pub shadows_tint_slider: Scale,
    // Audio sliders
    pub volume_slider: Scale,
    pub pan_slider: Scale,
    pub normalize_btn: Button,
    pub measured_loudness_label: Label,
    // LADSPA effects
    pub ladspa_effects_list: GBox,
    // Channel mode
    pub channel_mode_dropdown: gtk4::ComboBoxText,
    // Pitch controls
    pub pitch_shift_slider: Scale,
    pub pitch_preserve_check: gtk4::CheckButton,
    // Track audio controls
    pub role_dropdown: gtk4::ComboBoxText,
    pub duck_check: gtk4::CheckButton,
    pub duck_amount_slider: Scale,
    // EQ sliders (3 bands × 3 params)
    pub eq_freq_sliders: Vec<Scale>,
    pub eq_gain_sliders: Vec<Scale>,
    pub eq_q_sliders: Vec<Scale>,
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
    pub blend_mode_dropdown: gtk4::DropDown,
    pub anamorphic_desqueeze_dropdown: gtk4::DropDown,
    pub position_x_slider: Scale,
    pub position_y_slider: Scale,
    // Title / text overlay
    pub title_entry: Entry,
    pub title_x_slider: Scale,
    pub title_y_slider: Scale,
    pub title_font_btn: gtk4::Button,
    pub title_color_btn: gtk4::ColorDialogButton,
    pub title_outline_width_slider: Scale,
    pub title_outline_color_btn: gtk4::ColorDialogButton,
    pub title_shadow_check: CheckButton,
    pub title_shadow_color_btn: gtk4::ColorDialogButton,
    pub title_shadow_x_slider: Scale,
    pub title_shadow_y_slider: Scale,
    pub title_bg_box_check: CheckButton,
    pub title_bg_box_color_btn: gtk4::ColorDialogButton,
    pub title_bg_box_padding_slider: Scale,
    // Speed
    pub speed_slider: Scale,
    pub reverse_check: CheckButton,
    pub slow_motion_dropdown: DropDown,
    // LUT (color grading)
    pub lut_display_box: GBox,
    pub lut_clear_btn: Button,
    pub match_color_btn: Button,
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
    // Applied frei0r effects
    pub frei0r_effects_section: GBox,
    pub frei0r_effects_list: GBox,
    /// Clipboard for copying/pasting frei0r effects between clips.
    pub frei0r_effects_clipboard: Rc<RefCell<Option<Vec<crate::model::clip::Frei0rEffect>>>>,
    /// Paste button — kept so we can update sensitivity when clipboard changes.
    pub frei0r_paste_btn: Button,
    /// Project handle for frei0r effect mutations from inspector callbacks.
    pub project: Rc<RefCell<Project>>,
    /// Called after frei0r topology changes (add/remove/reorder/toggle) — triggers full pipeline rebuild.
    pub on_frei0r_changed: Rc<dyn Fn()>,
    /// Called after frei0r param slider changes — triggers live pipeline update without rebuild.
    pub on_frei0r_params_changed: Rc<dyn Fn()>,
    /// Push an undoable command through the shared history (provided by window.rs).
    pub on_execute_command: Rc<dyn Fn(Box<dyn crate::undo::EditCommand>)>,
    /// Tracks which effect IDs are currently displayed to avoid rebuilding on every update() tick.
    frei0r_displayed_snapshot: Rc<RefCell<Vec<(String, bool, usize)>>>,
    /// Cached frei0r registry for param type lookup in the inspector.
    frei0r_registry: Rc<RefCell<Option<crate::media::frei0r_registry::Frei0rRegistry>>>,
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

    /// Rebuild the applied frei0r effects list in the Inspector.
    fn rebuild_frei0r_effects_list(
        &self,
        effects: &[crate::model::clip::Frei0rEffect],
    ) {
        // Skip rebuild if the displayed effects haven't changed (avoids destroying
        // slider widgets during playback ticks that call update() repeatedly).
        let snapshot: Vec<(String, bool, usize)> = effects
            .iter()
            .map(|e| (e.id.clone(), e.enabled, e.params.len()))
            .collect();
        if *self.frei0r_displayed_snapshot.borrow() == snapshot {
            return;
        }
        *self.frei0r_displayed_snapshot.borrow_mut() = snapshot;

        // Remove all children.
        while let Some(child) = self.frei0r_effects_list.first_child() {
            self.frei0r_effects_list.remove(&child);
        }

        if effects.is_empty() {
            let empty = Label::new(Some(
                "No effects applied.\nUse the Effects tab to add frei0r filters.",
            ));
            empty.set_wrap(true);
            empty.add_css_class("panel-empty-state");
            empty.set_margin_start(4);
            self.frei0r_effects_list.append(&empty);
            return;
        }

        let effect_count = effects.len();
        for (i, effect) in effects.iter().enumerate() {
            let row = GBox::new(Orientation::Vertical, 2);
            row.set_margin_start(4);
            row.set_margin_end(4);
            row.set_margin_top(2);
            row.set_margin_bottom(2);

            // Header: [✓ enabled] Name  [▲] [▼] [×]
            let header = GBox::new(Orientation::Horizontal, 4);

            let enable_check = CheckButton::new();
            enable_check.set_active(effect.enabled);
            enable_check.set_tooltip_text(Some("Enable/disable this effect"));
            header.append(&enable_check);

            // Enable toggle callback
            {
                let project = self.project.clone();
                let selected_clip_id = self.selected_clip_id.clone();
                let effect_id = effect.id.clone();
                let on_changed = self.on_frei0r_changed.clone();
                let on_execute_command = self.on_execute_command.clone();
                let updating = self.updating.clone();
                enable_check.connect_toggled(move |btn| {
                    if *updating.borrow() {
                        return;
                    }
                    // Clone clip_id and drop the borrow BEFORE calling on_changed.
                    let cid = selected_clip_id.borrow().clone();
                    if let Some(cid) = cid {
                        let track_id = {
                            let proj = project.borrow();
                            proj.tracks.iter()
                                .find(|t| t.clips.iter().any(|c| c.id == cid))
                                .map(|t| t.id.clone())
                                .unwrap_or_default()
                        };
                        on_execute_command(Box::new(crate::undo::ToggleFrei0rEffectCommand {
                            clip_id: cid,
                            track_id,
                            effect_id: effect_id.clone(),
                        }));
                        on_changed();
                    }
                });
            }

            let display_name = humanize_frei0r_name(&effect.plugin_name);
            let name_label = Label::new(Some(&display_name));
            name_label.add_css_class("applied-effect-name");
            name_label.set_halign(gtk4::Align::Start);
            name_label.set_hexpand(true);
            name_label.set_ellipsize(gtk4::pango::EllipsizeMode::End);
            header.append(&name_label);

            // Move up button
            if i > 0 {
                let up_btn = gtk4::Button::from_icon_name("go-up-symbolic");
                up_btn.add_css_class("flat");
                up_btn.set_tooltip_text(Some("Move up"));
                header.append(&up_btn);
                let project = self.project.clone();
                let selected_clip_id = self.selected_clip_id.clone();
                let on_changed = self.on_frei0r_changed.clone();
                let on_execute_command = self.on_execute_command.clone();
                let idx = i;
                up_btn.connect_clicked(move |_| {
                    let cid = selected_clip_id.borrow().clone();
                    if let Some(cid) = cid {
                        let (track_id, valid) = {
                            let proj = project.borrow();
                            let mut tid = String::new();
                            let mut found = false;
                            for track in &proj.tracks {
                                if track.clips.iter().any(|c| c.id == cid) && idx > 0 {
                                    tid = track.id.clone();
                                    found = true;
                                    break;
                                }
                            }
                            (tid, found)
                        };
                        if valid {
                            on_execute_command(Box::new(crate::undo::ReorderFrei0rEffectsCommand {
                                clip_id: cid,
                                track_id,
                                index_a: idx - 1,
                                index_b: idx,
                            }));
                        }
                        on_changed();
                    }
                });
            }

            // Move down button
            if i + 1 < effect_count {
                let down_btn = gtk4::Button::from_icon_name("go-down-symbolic");
                down_btn.add_css_class("flat");
                down_btn.set_tooltip_text(Some("Move down"));
                header.append(&down_btn);
                let project = self.project.clone();
                let selected_clip_id = self.selected_clip_id.clone();
                let on_changed = self.on_frei0r_changed.clone();
                let on_execute_command = self.on_execute_command.clone();
                let idx = i;
                down_btn.connect_clicked(move |_| {
                    let cid = selected_clip_id.borrow().clone();
                    if let Some(cid) = cid {
                        let (track_id, valid) = {
                            let proj = project.borrow();
                            let mut tid = String::new();
                            let mut found = false;
                            for track in &proj.tracks {
                                if let Some(clip) = track.clips.iter().find(|c| c.id == cid) {
                                    if idx + 1 < clip.frei0r_effects.len() {
                                        tid = track.id.clone();
                                        found = true;
                                    }
                                    break;
                                }
                            }
                            (tid, found)
                        };
                        if valid {
                            on_execute_command(Box::new(crate::undo::ReorderFrei0rEffectsCommand {
                                clip_id: cid,
                                track_id,
                                index_a: idx,
                                index_b: idx + 1,
                            }));
                        }
                        on_changed();
                    }
                });
            }

            // Remove button
            let remove_btn = gtk4::Button::from_icon_name("edit-delete-symbolic");
            remove_btn.add_css_class("flat");
            remove_btn.set_tooltip_text(Some("Remove effect"));
            header.append(&remove_btn);
            {
                let project = self.project.clone();
                let selected_clip_id = self.selected_clip_id.clone();
                let effect_id = effect.id.clone();
                let effect_clone = effect.clone();
                let effect_idx = i;
                let on_changed = self.on_frei0r_changed.clone();
                let on_execute_command = self.on_execute_command.clone();
                remove_btn.connect_clicked(move |_| {
                    let cid = selected_clip_id.borrow().clone();
                    if let Some(cid) = cid {
                        let track_id = {
                            let proj = project.borrow();
                            proj.tracks.iter()
                                .find(|t| t.clips.iter().any(|c| c.id == cid))
                                .map(|t| t.id.clone())
                                .unwrap_or_default()
                        };
                        on_execute_command(Box::new(crate::undo::RemoveFrei0rEffectCommand {
                            clip_id: cid,
                            track_id,
                            effect: effect_clone.clone(),
                            index: effect_idx,
                        }));
                        on_changed();
                    }
                });
            }

            row.append(&header);

            // Look up plugin info for param type detection (Bool vs Double).
            let plugin_info = {
                let reg = crate::media::frei0r_registry::Frei0rRegistry::get_or_discover();
                reg.find_by_name(&effect.plugin_name).cloned()
            };

            // Collapsible container for parameter controls.
            let params_box = GBox::new(Orientation::Vertical, 2);
            let has_params = !effect.params.is_empty() || !effect.string_params.is_empty();

            // ── Special UI: graphical editors for specific effects ──
            let is_3point = effect.plugin_name == "3-point-color-balance";
            let is_curves = effect.plugin_name == "curves";
            let is_levels = effect.plugin_name == "levels";

            if is_3point {
                self.build_3point_color_wheels(effect, &params_box);
            } else if is_curves {
                self.build_curves_editor(effect, &params_box);
            } else if is_levels {
                self.build_levels_editor(effect, &params_box);
            } else {
            for (param_name, &param_val) in &effect.params {
                let param_type = plugin_info
                    .as_ref()
                    .and_then(|info| info.params.iter().find(|p| p.name == *param_name))
                    .map(|p| p.param_type)
                    .unwrap_or(crate::media::frei0r_registry::Frei0rParamType::Double);

                let param_row = GBox::new(Orientation::Horizontal, 4);
                param_row.set_margin_start(24);
                let plabel = Label::new(Some(param_name));
                plabel.add_css_class("dim-label");
                plabel.set_halign(gtk4::Align::Start);
                plabel.set_width_chars(12);
                param_row.append(&plabel);

                match param_type {
                    crate::media::frei0r_registry::Frei0rParamType::Bool => {
                        let toggle = CheckButton::new();
                        toggle.set_active(param_val > 0.5);
                        toggle.set_hexpand(true);
                        param_row.append(&toggle);

                        let project = self.project.clone();
                        let selected_clip_id = self.selected_clip_id.clone();
                        let effect_id = effect.id.clone();
                        let pname = param_name.clone();
                        let on_params_changed = self.on_frei0r_params_changed.clone();
                        let updating = self.updating.clone();
                        let on_execute_command = self.on_execute_command.clone();
                        toggle.connect_toggled(move |btn| {
                            if *updating.borrow() {
                                return;
                            }
                            let val = if btn.is_active() { 1.0 } else { 0.0 };
                            let cid = selected_clip_id.borrow().clone();
                            if let Some(cid) = cid {
                                let (track_id, old_params) = {
                                    let proj = project.borrow();
                                    let mut tid = String::new();
                                    let mut old = std::collections::HashMap::new();
                                    for track in &proj.tracks {
                                        if let Some(clip) = track.clips.iter().find(|c| c.id == cid) {
                                            tid = track.id.clone();
                                            if let Some(e) = clip.frei0r_effects.iter().find(|e| e.id == effect_id) {
                                                old = e.params.clone();
                                            }
                                            break;
                                        }
                                    }
                                    (tid, old)
                                };
                                {
                                    let mut proj = project.borrow_mut();
                                    for track in &mut proj.tracks {
                                        if let Some(clip) =
                                            track.clips.iter_mut().find(|c| c.id == cid)
                                        {
                                            if let Some(e) = clip
                                                .frei0r_effects
                                                .iter_mut()
                                                .find(|e| e.id == effect_id)
                                            {
                                                e.params.insert(pname.clone(), val);
                                            }
                                            break;
                                        }
                                    }
                                    proj.dirty = true;
                                }
                                let new_params = {
                                    let proj = project.borrow();
                                    proj.tracks.iter()
                                        .find(|t| t.id == track_id)
                                        .and_then(|t| t.clips.iter().find(|c| c.id == cid))
                                        .and_then(|c| c.frei0r_effects.iter().find(|e| e.id == effect_id))
                                        .map(|e| e.params.clone())
                                        .unwrap_or_else(|| old_params.clone())
                                };
                                on_execute_command(Box::new(crate::undo::SetFrei0rEffectParamsCommand {
                                    clip_id: cid.clone(),
                                    track_id,
                                    effect_id: effect_id.clone(),
                                    old_params,
                                    new_params,
                                }));
                                on_params_changed();
                            }
                        });
                    }
                    crate::media::frei0r_registry::Frei0rParamType::String => {
                        // String params are now handled in the string_params loop below.
                        // If somehow a string param is in the f64 map, show value.
                        let hint = Label::new(Some(&format!("{param_val}")));
                        hint.add_css_class("dim-label");
                        hint.set_hexpand(true);
                        param_row.append(&hint);
                    }
                    _ => {
                        // Double — use a slider.
                        let (mut min, mut max) = plugin_info
                            .as_ref()
                            .and_then(|info| {
                                info.params
                                    .iter()
                                    .find(|p| p.name == *param_name)
                                    .map(|p| (p.min, p.max))
                            })
                            .unwrap_or((0.0, 1.0));
                        // Safety: ensure finite, sane bounds for GTK Scale.
                        if !min.is_finite() || min < -1e6 { min = 0.0; }
                        if !max.is_finite() || max > 1e6 { max = 1.0; }
                        if min >= max { min = 0.0; max = 1.0; }
                        let step = ((max - min) / 100.0).max(f64::MIN_POSITIVE);
                        let slider = Scale::with_range(
                            Orientation::Horizontal,
                            min,
                            max,
                            step,
                        );
                        slider.set_value(param_val);
                        slider.set_draw_value(true);
                        slider.set_digits(2);
                        slider.set_hexpand(true);
                        param_row.append(&slider);

                        let project = self.project.clone();
                        let selected_clip_id = self.selected_clip_id.clone();
                        let effect_id = effect.id.clone();
                        let pname = param_name.clone();
                        let on_params_changed = self.on_frei0r_params_changed.clone();
                        let updating = self.updating.clone();
                        slider.connect_value_changed(move |s| {
                            if *updating.borrow() {
                                return;
                            }
                            let val = s.value();
                            let cid = selected_clip_id.borrow().clone();
                            if let Some(cid) = cid {
                                {
                                    let mut proj = project.borrow_mut();
                                    for track in &mut proj.tracks {
                                        if let Some(clip) =
                                            track.clips.iter_mut().find(|c| c.id == cid)
                                        {
                                            if let Some(e) = clip
                                                .frei0r_effects
                                                .iter_mut()
                                                .find(|e| e.id == effect_id)
                                            {
                                                e.params.insert(pname.clone(), val);
                                            }
                                            break;
                                        }
                                    }
                                    proj.dirty = true;
                                }
                                on_params_changed();
                            }
                        });

                        // Undo: GestureClick + EventControllerFocus snapshot/commit.
                        {
                            type SnapCell = Rc<RefCell<Option<(String, String, String, std::collections::HashMap<String, f64>)>>>;
                            let snap: SnapCell = Rc::new(RefCell::new(None));
                            let project = self.project.clone();
                            let selected_clip_id = self.selected_clip_id.clone();
                            let effect_id_u = effect.id.clone();
                            let on_execute_command = self.on_execute_command.clone();

                            let do_snapshot = {
                                let project = project.clone();
                                let selected_clip_id = selected_clip_id.clone();
                                let effect_id_u = effect_id_u.clone();
                                let snap = snap.clone();
                                move || {
                                    let cid = selected_clip_id.borrow().clone();
                                    if let Some(cid) = cid {
                                        let proj = project.borrow();
                                        for track in &proj.tracks {
                                            if let Some(clip) = track.clips.iter().find(|c| c.id == cid) {
                                                if let Some(e) = clip.frei0r_effects.iter().find(|e| e.id == effect_id_u) {
                                                    *snap.borrow_mut() = Some((
                                                        cid.clone(),
                                                        track.id.clone(),
                                                        effect_id_u.clone(),
                                                        e.params.clone(),
                                                    ));
                                                }
                                                break;
                                            }
                                        }
                                    }
                                }
                            };

                            let do_commit = {
                                let project = project.clone();
                                let snap = snap.clone();
                                let on_execute_command = on_execute_command.clone();
                                move || {
                                    let entry = snap.borrow_mut().take();
                                    if let Some((clip_id, track_id, eff_id, old_params)) = entry {
                                        let new_params = {
                                            let proj = project.borrow();
                                            proj.tracks.iter()
                                                .find(|t| t.id == track_id)
                                                .and_then(|t| t.clips.iter().find(|c| c.id == clip_id))
                                                .and_then(|c| c.frei0r_effects.iter().find(|e| e.id == eff_id))
                                                .map(|e| e.params.clone())
                                                .unwrap_or_else(|| old_params.clone())
                                        };
                                        on_execute_command(Box::new(crate::undo::SetFrei0rEffectParamsCommand {
                                            clip_id,
                                            track_id,
                                            effect_id: eff_id,
                                            old_params,
                                            new_params,
                                        }));
                                    }
                                }
                            };

                            let ges = gtk4::GestureClick::new();
                            {
                                let do_snapshot = do_snapshot.clone();
                                ges.connect_pressed(move |_, _, _, _| { do_snapshot(); });
                            }
                            {
                                let do_commit = do_commit.clone();
                                ges.connect_released(move |_, _, _, _| { do_commit(); });
                            }
                            slider.add_controller(ges);

                            let focus_ctrl = gtk4::EventControllerFocus::new();
                            {
                                let do_snapshot = do_snapshot.clone();
                                focus_ctrl.connect_enter(move |_| { do_snapshot(); });
                            }
                            {
                                let do_commit = do_commit.clone();
                                focus_ctrl.connect_leave(move |_| { do_commit(); });
                            }
                            slider.add_controller(focus_ctrl);
                        }
                    }
                }

                params_box.append(&param_row);
            }

            // String parameter controls — DropDown for enums, Entry for free-form.
            for (param_name, param_val) in &effect.string_params {
                let enum_values = plugin_info
                    .as_ref()
                    .and_then(|info| info.params.iter().find(|p| p.name == *param_name))
                    .and_then(|p| p.enum_values.clone());

                let param_row = GBox::new(Orientation::Horizontal, 4);
                param_row.set_margin_start(24);
                let plabel = Label::new(Some(param_name));
                plabel.add_css_class("dim-label");
                plabel.set_halign(gtk4::Align::Start);
                plabel.set_width_chars(12);
                param_row.append(&plabel);

                if let Some(values) = enum_values {
                    let str_list = StringList::new(&values.iter().map(|s| s.as_str()).collect::<Vec<_>>());
                    let dropdown = DropDown::new(Some(str_list), gtk4::Expression::NONE);
                    dropdown.set_hexpand(true);
                    // Select the current value.
                    if let Some(pos) = values.iter().position(|v| v == param_val) {
                        dropdown.set_selected(pos as u32);
                    }
                    param_row.append(&dropdown);

                    let project = self.project.clone();
                    let selected_clip_id = self.selected_clip_id.clone();
                    let effect_id = effect.id.clone();
                    let pname = param_name.clone();
                    let vals = values.clone();
                    let on_params_changed = self.on_frei0r_params_changed.clone();
                    let updating = self.updating.clone();
                    dropdown.connect_selected_notify(move |dd| {
                        if *updating.borrow() {
                            return;
                        }
                        let idx = dd.selected() as usize;
                        if let Some(new_val) = vals.get(idx) {
                            let cid = selected_clip_id.borrow().clone();
                            if let Some(cid) = cid {
                                {
                                    let mut proj = project.borrow_mut();
                                    for track in &mut proj.tracks {
                                        if let Some(clip) =
                                            track.clips.iter_mut().find(|c| c.id == cid)
                                        {
                                            if let Some(e) = clip
                                                .frei0r_effects
                                                .iter_mut()
                                                .find(|e| e.id == effect_id)
                                            {
                                                e.string_params
                                                    .insert(pname.clone(), new_val.clone());
                                            }
                                            break;
                                        }
                                    }
                                    proj.dirty = true;
                                }
                                on_params_changed();
                            }
                        }
                    });
                } else {
                    // Free-form string parameter — use an Entry.
                    let entry = Entry::new();
                    entry.set_text(param_val);
                    entry.set_hexpand(true);
                    param_row.append(&entry);

                    let project = self.project.clone();
                    let selected_clip_id = self.selected_clip_id.clone();
                    let effect_id = effect.id.clone();
                    let pname = param_name.clone();
                    let on_params_changed = self.on_frei0r_params_changed.clone();
                    let updating = self.updating.clone();
                    entry.connect_changed(move |e| {
                        if *updating.borrow() {
                            return;
                        }
                        let new_val = e.text().to_string();
                        let cid = selected_clip_id.borrow().clone();
                        if let Some(cid) = cid {
                            {
                                let mut proj = project.borrow_mut();
                                for track in &mut proj.tracks {
                                    if let Some(clip) =
                                        track.clips.iter_mut().find(|c| c.id == cid)
                                    {
                                        if let Some(eff) = clip
                                            .frei0r_effects
                                            .iter_mut()
                                            .find(|eff| eff.id == effect_id)
                                        {
                                            eff.string_params
                                                .insert(pname.clone(), new_val.clone());
                                        }
                                        break;
                                    }
                                }
                                proj.dirty = true;
                            }
                            on_params_changed();
                        }
                    });
                }

                params_box.append(&param_row);
            }

            } // end else (non-special generic params)

            let is_special = is_3point || is_curves || is_levels;
            if has_params || is_special {
                let label = if is_3point {
                    "Color Wheels"
                } else if is_curves {
                    "Curve Editor"
                } else if is_levels {
                    "Levels Editor"
                } else {
                    "Parameters"
                };
                let expander = Expander::new(Some(label));
                expander.set_expanded(is_special);
                expander.set_margin_start(4);
                expander.set_child(Some(&params_box));
                row.append(&expander);
            }

            if i + 1 < effect_count {
                row.append(&Separator::new(Orientation::Horizontal));
            }

            self.frei0r_effects_list.append(&row);
        }
    }

    /// Build three color wheels (Shadows, Midtones, Highlights) for the
    /// 3-point-color-balance frei0r effect, wired to update the effect's
    /// RGB params in the project model.
    fn build_3point_color_wheels(
        &self,
        effect: &crate::model::clip::Frei0rEffect,
        container: &GBox,
    ) {
        use crate::ui::color_wheel::build_color_wheel;

        // Zone definitions: (label, r_key, g_key, b_key, default_luminance)
        let zones: &[(&str, &str, &str, &str, f64)] = &[
            ("Midtones", "gray-color-r", "gray-color-g", "gray-color-b", 0.5),
            ("Shadows", "black-color-r", "black-color-g", "black-color-b", 0.0),
            ("Highlights", "white-color-r", "white-color-g", "white-color-b", 1.0),
        ];

        // Large midtones wheel on top.
        let mid_zone = &zones[0];
        let mid_r = *effect.params.get(mid_zone.1).unwrap_or(&mid_zone.4);
        let mid_g = *effect.params.get(mid_zone.2).unwrap_or(&mid_zone.4);
        let mid_b = *effect.params.get(mid_zone.3).unwrap_or(&mid_zone.4);

        let mid_label = Label::new(Some(mid_zone.0));
        mid_label.add_css_class("dim-label");
        mid_label.set_halign(gtk4::Align::Center);
        mid_label.set_margin_top(4);
        container.append(&mid_label);

        let (mid_widget, _mid_setter) = {
            let project = self.project.clone();
            let selected_clip_id = self.selected_clip_id.clone();
            let effect_id = effect.id.clone();
            let on_params_changed = self.on_frei0r_params_changed.clone();
            let updating = self.updating.clone();
            let rk = mid_zone.1.to_string();
            let gk = mid_zone.2.to_string();
            let bk = mid_zone.3.to_string();
            build_color_wheel(160, (mid_r, mid_g, mid_b), move |r, g, b| {
                if *updating.borrow() {
                    return;
                }
                let cid = selected_clip_id.borrow().clone();
                if let Some(cid) = cid {
                    {
                        let mut proj = project.borrow_mut();
                        for track in &mut proj.tracks {
                            if let Some(clip) =
                                track.clips.iter_mut().find(|c| c.id == cid)
                            {
                                if let Some(e) = clip
                                    .frei0r_effects
                                    .iter_mut()
                                    .find(|e| e.id == effect_id)
                                {
                                    e.params.insert(rk.clone(), r);
                                    e.params.insert(gk.clone(), g);
                                    e.params.insert(bk.clone(), b);
                                }
                                break;
                            }
                        }
                        proj.dirty = true;
                    }
                    on_params_changed();
                }
            })
        };
        container.append(&mid_widget);

        // Shadows and Highlights side-by-side.
        let bottom_row = GBox::new(Orientation::Horizontal, 8);
        bottom_row.set_halign(gtk4::Align::Center);
        bottom_row.set_margin_top(8);

        for zone in &zones[1..] {
            let zone_r = *effect.params.get(zone.1).unwrap_or(&zone.4);
            let zone_g = *effect.params.get(zone.2).unwrap_or(&zone.4);
            let zone_b = *effect.params.get(zone.3).unwrap_or(&zone.4);

            let zone_box = GBox::new(Orientation::Vertical, 2);
            let zone_label = Label::new(Some(zone.0));
            zone_label.add_css_class("dim-label");
            zone_label.set_halign(gtk4::Align::Center);
            zone_box.append(&zone_label);

            let (wheel_widget, _setter) = {
                let project = self.project.clone();
                let selected_clip_id = self.selected_clip_id.clone();
                let effect_id = effect.id.clone();
                let on_params_changed = self.on_frei0r_params_changed.clone();
                let updating = self.updating.clone();
                let rk = zone.1.to_string();
                let gk = zone.2.to_string();
                let bk = zone.3.to_string();
                build_color_wheel(120, (zone_r, zone_g, zone_b), move |r, g, b| {
                    if *updating.borrow() {
                        return;
                    }
                    let cid = selected_clip_id.borrow().clone();
                    if let Some(cid) = cid {
                        {
                            let mut proj = project.borrow_mut();
                            for track in &mut proj.tracks {
                                if let Some(clip) =
                                    track.clips.iter_mut().find(|c| c.id == cid)
                                {
                                    if let Some(e) = clip
                                        .frei0r_effects
                                        .iter_mut()
                                        .find(|e| e.id == effect_id)
                                    {
                                        e.params.insert(rk.clone(), r);
                                        e.params.insert(gk.clone(), g);
                                        e.params.insert(bk.clone(), b);
                                    }
                                    break;
                                }
                            }
                            proj.dirty = true;
                        }
                        on_params_changed();
                    }
                })
            };
            zone_box.append(&wheel_widget);
            bottom_row.append(&zone_box);
        }

        container.append(&bottom_row);
    }

    /// Build a graphical curve editor for the frei0r "curves" effect.
    fn build_curves_editor(
        &self,
        effect: &crate::model::clip::Frei0rEffect,
        container: &GBox,
    ) {
        use crate::ui::curves_editor;

        // Extract current state from effect params
        let channel = *effect.params.get("channel").unwrap_or(&0.5);
        let point_count_raw = *effect.params.get("curve-point-number").unwrap_or(&0.2);
        let point_count = ((point_count_raw * 10.0).round() as usize).clamp(2, 5);

        let mut points = Vec::new();
        for i in 1..=point_count {
            let default_val = (i - 1) as f64 / (point_count - 1).max(1) as f64;
            let inp = *effect
                .params
                .get(&format!("point-{i}-input-value"))
                .unwrap_or(&default_val);
            let out = *effect
                .params
                .get(&format!("point-{i}-output-value"))
                .unwrap_or(&default_val);
            points.push((inp, out));
        }
        if points.len() < 2 {
            points = vec![(0.0, 0.0), (1.0, 1.0)];
        }

        let project = self.project.clone();
        let selected_clip_id = self.selected_clip_id.clone();
        let effect_id = effect.id.clone();
        let on_params_changed = self.on_frei0r_params_changed.clone();
        let updating = self.updating.clone();

        let widget = curves_editor::build_curves_widget(channel, points, move |ch_val, pts| {
            if *updating.borrow() {
                return;
            }
            let cid = selected_clip_id.borrow().clone();
            if let Some(cid) = cid {
                {
                    let mut proj = project.borrow_mut();
                    for track in &mut proj.tracks {
                        if let Some(clip) = track.clips.iter_mut().find(|c| c.id == cid) {
                            if let Some(e) =
                                clip.frei0r_effects.iter_mut().find(|e| e.id == effect_id)
                            {
                                e.params.insert("channel".to_string(), ch_val);
                                e.params.insert("show-curves".to_string(), 0.0);
                                e.params.insert(
                                    "curve-point-number".to_string(),
                                    pts.len() as f64 / 10.0,
                                );
                                for (i, &(inp, out)) in pts.iter().enumerate() {
                                    e.params.insert(
                                        format!("point-{}-input-value", i + 1),
                                        inp,
                                    );
                                    e.params.insert(
                                        format!("point-{}-output-value", i + 1),
                                        out,
                                    );
                                }
                                // Clear unused point slots
                                for i in (pts.len() + 1)..=5 {
                                    e.params
                                        .remove(&format!("point-{i}-input-value"));
                                    e.params
                                        .remove(&format!("point-{i}-output-value"));
                                }
                            }
                            break;
                        }
                    }
                    proj.dirty = true;
                }
                on_params_changed();
            }
        });
        container.append(&widget);
    }

    /// Build a graphical levels editor for the frei0r "levels" effect.
    fn build_levels_editor(
        &self,
        effect: &crate::model::clip::Frei0rEffect,
        container: &GBox,
    ) {
        use crate::ui::levels_editor;

        let channel = *effect.params.get("channel").unwrap_or(&0.3);
        let input_black = *effect.params.get("input-black-level").unwrap_or(&0.0);
        let input_white = *effect.params.get("input-white-level").unwrap_or(&1.0);
        let gamma_frei0r = *effect.params.get("gamma").unwrap_or(&0.25);
        let output_black = *effect.params.get("black-output").unwrap_or(&0.0);
        let output_white = *effect.params.get("white-output").unwrap_or(&1.0);

        let project = self.project.clone();
        let selected_clip_id = self.selected_clip_id.clone();
        let effect_id = effect.id.clone();
        let on_params_changed = self.on_frei0r_params_changed.clone();
        let updating = self.updating.clone();

        let widget = levels_editor::build_levels_widget(
            channel,
            input_black,
            input_white,
            gamma_frei0r,
            output_black,
            output_white,
            move |ch_val, ib, iw, gamma, ob, ow| {
                if *updating.borrow() {
                    return;
                }
                let cid = selected_clip_id.borrow().clone();
                if let Some(cid) = cid {
                    {
                        let mut proj = project.borrow_mut();
                        for track in &mut proj.tracks {
                            if let Some(clip) = track.clips.iter_mut().find(|c| c.id == cid) {
                                if let Some(e) =
                                    clip.frei0r_effects.iter_mut().find(|e| e.id == effect_id)
                                {
                                    e.params.insert("channel".to_string(), ch_val);
                                    e.params.insert("input-black-level".to_string(), ib);
                                    e.params.insert("input-white-level".to_string(), iw);
                                    e.params.insert("gamma".to_string(), gamma);
                                    e.params.insert("black-output".to_string(), ob);
                                    e.params.insert("white-output".to_string(), ow);
                                    e.params.insert("show-histogram".to_string(), 0.0);
                                }
                                break;
                            }
                        }
                        proj.dirty = true;
                    }
                    on_params_changed();
                }
            },
        );
        container.append(&widget);
    }

    /// Refresh all fields to show the given clip, or clear if None.
    /// `playhead_ns` is used to display keyframe-evaluated values for animated properties.
    pub fn update(
        &self,
        project: &Project,
        clip_id: Option<&str>,
        playhead_ns: u64,
        missing_media_paths: Option<&HashSet<String>>,
    ) {
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
                let is_title_clip = c.kind == ClipKind::Title;
                let is_adjustment = c.kind == ClipKind::Adjustment;
                let is_visual = is_video || is_image || is_title_clip || is_adjustment;
                self.color_section.set_visible(is_video || is_image || is_adjustment);
                self.audio_section.set_visible(is_video || is_audio);
                self.transform_section.set_visible(is_visual && !is_adjustment);
                self.title_section_box.set_visible(is_visual && !is_adjustment);
                self.speed_section_box.set_visible(!is_title_clip && !is_adjustment);
                self.lut_section_box.set_visible(is_video || is_image || is_adjustment);
                self.chroma_key_section.set_visible(is_video || is_image);
                self.bg_removal_section
                    .set_visible((is_video || is_image) && self.bg_removal_model_available.get());
                self.frei0r_effects_section
                    .set_visible(is_visual);

                // Populate applied frei0r effects list.
                self.rebuild_frei0r_effects_list(&c.frei0r_effects);

                self.name_entry.set_text(&c.label);
                let is_title = c.kind == ClipKind::Title;
                if is_title {
                    self.path_value.set_text("(title clip — no source file)");
                    self.path_value.set_tooltip_text(None);
                } else if is_adjustment {
                    self.path_value.set_text("(adjustment layer — applies effects to tracks below)");
                    self.path_value.set_tooltip_text(None);
                } else {
                    self.path_value.set_text(&c.source_path);
                    self.path_value.set_tooltip_text(Some(&c.source_path));
                }
                let is_missing = !is_title && !is_adjustment
                    && missing_media_paths
                        .map(|paths| paths.contains(&c.source_path))
                        .unwrap_or_else(|| !crate::model::media_library::source_path_exists(&c.source_path));
                if is_missing {
                    self.path_status_value
                        .set_text("Offline — source file is missing");
                    self.path_status_value.remove_css_class("dim-label");
                    self.path_status_value.add_css_class("offline-label");
                    self.relink_btn.set_visible(true);
                } else {
                    self.path_status_value.set_text("Online");
                    self.path_status_value.remove_css_class("offline-label");
                    self.path_status_value.add_css_class("dim-label");
                    self.relink_btn.set_visible(false);
                }
                self.clip_color_label_combo
                    .set_selected(clip_color_label_index(c.color_label));
                self.blend_mode_dropdown.set_selected(
                    crate::model::clip::BlendMode::ALL
                        .iter()
                        .position(|m| *m == c.blend_mode)
                        .unwrap_or(0) as u32,
                );
                let anamorphic_idx = match c.anamorphic_desqueeze {
                    x if (x - 1.33).abs() < 0.01 => 1,
                    x if (x - 1.5).abs() < 0.01 => 2,
                    x if (x - 1.8).abs() < 0.01 => 3,
                    x if (x - 2.0).abs() < 0.01 => 4,
                    _ => 0,
                };
                self.anamorphic_desqueeze_dropdown.set_selected(anamorphic_idx);
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
                self.blur_slider.set_value(c.blur as f64);
                self.vidstab_check.set_active(c.vidstab_enabled);
                self.vidstab_slider.set_value(c.vidstab_smoothing as f64);
                self.shadows_slider.set_value(c.shadows as f64);
                self.midtones_slider.set_value(c.midtones as f64);
                self.highlights_slider.set_value(c.highlights as f64);
                self.exposure_slider.set_value(c.exposure as f64);
                self.black_point_slider.set_value(c.black_point as f64);
                self.highlights_warmth_slider
                    .set_value(c.highlights_warmth as f64);
                self.highlights_tint_slider
                    .set_value(c.highlights_tint as f64);
                self.midtones_warmth_slider
                    .set_value(c.midtones_warmth as f64);
                self.midtones_tint_slider.set_value(c.midtones_tint as f64);
                self.shadows_warmth_slider
                    .set_value(c.shadows_warmth as f64);
                self.shadows_tint_slider.set_value(c.shadows_tint as f64);
                // For keyframed properties, show the evaluated value at the playhead
                let vol_val = c.value_for_phase1_property_at_timeline_ns(
                    Phase1KeyframeProperty::Volume,
                    playhead_ns,
                );
                self.volume_slider.set_value(linear_to_db_volume(vol_val));
                self.pan_slider
                    .set_value(c.value_for_phase1_property_at_timeline_ns(
                        Phase1KeyframeProperty::Pan,
                        playhead_ns,
                    ));
                // Measured loudness
                if let Some(lufs) = c.measured_loudness_lufs {
                    self.measured_loudness_label
                        .set_text(&format!("{lufs:.1} LUFS"));
                } else {
                    self.measured_loudness_label.set_text("");
                }
                // LADSPA effects list — interactive controls
                {
                    while let Some(child) = self.ladspa_effects_list.first_child() {
                        self.ladspa_effects_list.remove(&child);
                    }
                    if c.ladspa_effects.is_empty() {
                        let hint = Label::new(Some("No audio effects applied"));
                        hint.add_css_class("dim-label");
                        hint.set_halign(gtk4::Align::Start);
                        self.ladspa_effects_list.append(&hint);
                    } else {
                        let reg = crate::media::ladspa_registry::LadspaRegistry::get_or_discover();
                        let clip_id = c.id.clone();
                        for (effect_idx, effect) in c.ladspa_effects.iter().enumerate() {
                            let effect_id = effect.id.clone();
                            let display_name = reg
                                .find_by_name(&effect.plugin_name)
                                .map(|p| p.display_name.clone())
                                .unwrap_or_else(|| effect.plugin_name.clone());

                            let effect_box = GBox::new(Orientation::Vertical, 2);
                            effect_box.set_margin_bottom(4);

                            // Header row: [✓] [Name] [▲] [▼] [×]
                            let header_row = GBox::new(Orientation::Horizontal, 4);
                            let enable_check = gtk4::CheckButton::new();
                            enable_check.set_active(effect.enabled);
                            header_row.append(&enable_check);
                            let name_label = Label::new(Some(&display_name));
                            name_label.set_hexpand(true);
                            name_label.set_halign(gtk4::Align::Start);
                            header_row.append(&name_label);
                            let btn_up = Button::with_label("\u{25b2}");
                            btn_up.set_tooltip_text(Some("Move up"));
                            btn_up.add_css_class("flat");
                            btn_up.set_sensitive(effect_idx > 0);
                            header_row.append(&btn_up);
                            let btn_down = Button::with_label("\u{25bc}");
                            btn_down.set_tooltip_text(Some("Move down"));
                            btn_down.add_css_class("flat");
                            btn_down.set_sensitive(effect_idx < c.ladspa_effects.len() - 1);
                            header_row.append(&btn_down);
                            let btn_remove = Button::with_label("\u{00d7}");
                            btn_remove.set_tooltip_text(Some("Remove"));
                            btn_remove.add_css_class("flat");
                            header_row.append(&btn_remove);
                            effect_box.append(&header_row);

                            // Wire enable toggle
                            {
                                let project = self.project.clone();
                                let clip_id = clip_id.clone();
                                let effect_id = effect_id.clone();
                                let on_changed = self.on_frei0r_changed.clone();
                                enable_check.connect_toggled(move |btn| {
                                    let mut proj = project.borrow_mut();
                                    for track in &mut proj.tracks {
                                        if let Some(clip) = track.clips.iter_mut().find(|c| c.id == clip_id) {
                                            if let Some(e) = clip.ladspa_effects.iter_mut().find(|e| e.id == effect_id) {
                                                e.enabled = btn.is_active();
                                                proj.dirty = true;
                                            }
                                            break;
                                        }
                                    }
                                    drop(proj);
                                    on_changed();
                                });
                            }
                            // Wire remove
                            {
                                let project = self.project.clone();
                                let clip_id = clip_id.clone();
                                let effect_id = effect_id.clone();
                                let on_changed = self.on_frei0r_changed.clone();
                                btn_remove.connect_clicked(move |_| {
                                    let mut proj = project.borrow_mut();
                                    for track in &mut proj.tracks {
                                        if let Some(clip) = track.clips.iter_mut().find(|c| c.id == clip_id) {
                                            clip.ladspa_effects.retain(|e| e.id != effect_id);
                                            proj.dirty = true;
                                            break;
                                        }
                                    }
                                    drop(proj);
                                    on_changed();
                                });
                            }
                            // Wire move up
                            {
                                let project = self.project.clone();
                                let clip_id = clip_id.clone();
                                let effect_id = effect_id.clone();
                                let on_changed = self.on_frei0r_changed.clone();
                                btn_up.connect_clicked(move |_| {
                                    let mut proj = project.borrow_mut();
                                    for track in &mut proj.tracks {
                                        if let Some(clip) = track.clips.iter_mut().find(|c| c.id == clip_id) {
                                            if let Some(pos) = clip.ladspa_effects.iter().position(|e| e.id == effect_id) {
                                                if pos > 0 {
                                                    clip.ladspa_effects.swap(pos, pos - 1);
                                                    proj.dirty = true;
                                                }
                                            }
                                            break;
                                        }
                                    }
                                    drop(proj);
                                    on_changed();
                                });
                            }
                            // Wire move down
                            {
                                let project = self.project.clone();
                                let clip_id = clip_id.clone();
                                let effect_id = effect_id.clone();
                                let on_changed = self.on_frei0r_changed.clone();
                                btn_down.connect_clicked(move |_| {
                                    let mut proj = project.borrow_mut();
                                    for track in &mut proj.tracks {
                                        if let Some(clip) = track.clips.iter_mut().find(|c| c.id == clip_id) {
                                            let len = clip.ladspa_effects.len();
                                            if let Some(pos) = clip.ladspa_effects.iter().position(|e| e.id == effect_id) {
                                                if pos + 1 < len {
                                                    clip.ladspa_effects.swap(pos, pos + 1);
                                                    proj.dirty = true;
                                                }
                                            }
                                            break;
                                        }
                                    }
                                    drop(proj);
                                    on_changed();
                                });
                            }

                            // Parameter sliders
                            if let Some(info) = reg.find_by_name(&effect.plugin_name) {
                                for param_info in &info.params {
                                    let val = effect.params.get(&param_info.name).copied().unwrap_or(param_info.default_value);
                                    let param_row = GBox::new(Orientation::Vertical, 1);
                                    let param_label = Label::new(Some(&param_info.display_name));
                                    param_label.set_halign(gtk4::Align::Start);
                                    param_label.add_css_class("dim-label");
                                    param_row.append(&param_label);

                                    let min = param_info.min;
                                    let max = param_info.max;
                                    let step = (max - min).abs() / 100.0;
                                    let slider = Scale::with_range(
                                        Orientation::Horizontal,
                                        min,
                                        max,
                                        if step > 0.0 { step } else { 0.01 },
                                    );
                                    slider.set_value(val);
                                    slider.set_draw_value(true);
                                    slider.set_digits(2);
                                    slider.add_mark(param_info.default_value, gtk4::PositionType::Bottom, None);
                                    param_row.append(&slider);

                                    // Wire slider
                                    let project = self.project.clone();
                                    let clip_id = clip_id.clone();
                                    let effect_id = effect_id.clone();
                                    let param_name = param_info.name.clone();
                                    let on_changed = self.on_frei0r_changed.clone();
                                    slider.connect_value_changed(move |s| {
                                        let mut proj = project.borrow_mut();
                                        for track in &mut proj.tracks {
                                            if let Some(clip) = track.clips.iter_mut().find(|c| c.id == clip_id) {
                                                if let Some(e) = clip.ladspa_effects.iter_mut().find(|e| e.id == effect_id) {
                                                    e.params.insert(param_name.clone(), s.value());
                                                    proj.dirty = true;
                                                }
                                                break;
                                            }
                                        }
                                        drop(proj);
                                        on_changed();
                                    });

                                    effect_box.append(&param_row);
                                }
                            }

                            if effect_idx < c.ladspa_effects.len() - 1 {
                                effect_box.append(&Separator::new(Orientation::Horizontal));
                            }
                            self.ladspa_effects_list.append(&effect_box);
                        }
                    }
                }
                // Channel mode
                #[allow(deprecated)]
                self.channel_mode_dropdown
                    .set_active_id(Some(c.audio_channel_mode.as_str()));
                // Pitch controls
                self.pitch_shift_slider
                    .set_value(c.pitch_shift_semitones);
                self.pitch_preserve_check.set_active(c.pitch_preserve);
                // Track audio controls — read from the clip's track.
                if let Some(track) = project.tracks.iter().find(|t| t.clips.iter().any(|tc| tc.id == c.id)) {
                    #[allow(deprecated)]
                    self.role_dropdown.set_active_id(Some(track.audio_role.as_str()));
                    self.duck_check.set_active(track.duck);
                    self.duck_amount_slider.set_value(track.duck_amount_db);
                }
                // EQ sliders
                for (i, band) in c.eq_bands.iter().enumerate() {
                    if i < self.eq_freq_sliders.len() {
                        self.eq_freq_sliders[i].set_value(band.freq);
                        self.eq_gain_sliders[i].set_value(band.gain);
                        self.eq_q_sliders[i].set_value(band.q);
                    }
                }
                self.crop_left_slider
                    .set_value(c.value_for_phase1_property_at_timeline_ns(
                        Phase1KeyframeProperty::CropLeft,
                        playhead_ns,
                    ));
                self.crop_right_slider
                    .set_value(c.value_for_phase1_property_at_timeline_ns(
                        Phase1KeyframeProperty::CropRight,
                        playhead_ns,
                    ));
                self.crop_top_slider
                    .set_value(c.value_for_phase1_property_at_timeline_ns(
                        Phase1KeyframeProperty::CropTop,
                        playhead_ns,
                    ));
                self.crop_bottom_slider
                    .set_value(c.value_for_phase1_property_at_timeline_ns(
                        Phase1KeyframeProperty::CropBottom,
                        playhead_ns,
                    ));
                self.rotate_spin
                    .set_value(c.value_for_phase1_property_at_timeline_ns(
                        Phase1KeyframeProperty::Rotate,
                        playhead_ns,
                    ));
                self.flip_h_btn.set_active(c.flip_h);
                self.flip_v_btn.set_active(c.flip_v);
                self.scale_slider
                    .set_value(c.value_for_phase1_property_at_timeline_ns(
                        Phase1KeyframeProperty::Scale,
                        playhead_ns,
                    ));
                self.opacity_slider
                    .set_value(c.value_for_phase1_property_at_timeline_ns(
                        Phase1KeyframeProperty::Opacity,
                        playhead_ns,
                    ));
                self.position_x_slider
                    .set_value(c.value_for_phase1_property_at_timeline_ns(
                        Phase1KeyframeProperty::PositionX,
                        playhead_ns,
                    ));
                self.position_y_slider
                    .set_value(c.value_for_phase1_property_at_timeline_ns(
                        Phase1KeyframeProperty::PositionY,
                        playhead_ns,
                    ));
                self.title_entry.set_text(&c.title_text);
                self.title_font_btn.set_label(&c.title_font);
                {
                    let rgba = c.title_color;
                    let r = ((rgba >> 24) & 0xFF) as f32 / 255.0;
                    let g = ((rgba >> 16) & 0xFF) as f32 / 255.0;
                    let b = ((rgba >> 8) & 0xFF) as f32 / 255.0;
                    let a = (rgba & 0xFF) as f32 / 255.0;
                    self.title_color_btn.set_rgba(&gdk4::RGBA::new(r, g, b, a));
                }
                self.title_x_slider.set_value(c.title_x);
                self.title_y_slider.set_value(c.title_y);
                self.title_outline_width_slider.set_value(c.title_outline_width);
                {
                    let rgba = c.title_outline_color;
                    let r = ((rgba >> 24) & 0xFF) as f32 / 255.0;
                    let g = ((rgba >> 16) & 0xFF) as f32 / 255.0;
                    let b = ((rgba >> 8) & 0xFF) as f32 / 255.0;
                    let a = (rgba & 0xFF) as f32 / 255.0;
                    self.title_outline_color_btn.set_rgba(&gdk4::RGBA::new(r, g, b, a));
                }
                self.title_shadow_check.set_active(c.title_shadow);
                {
                    let rgba = c.title_shadow_color;
                    let r = ((rgba >> 24) & 0xFF) as f32 / 255.0;
                    let g = ((rgba >> 16) & 0xFF) as f32 / 255.0;
                    let b = ((rgba >> 8) & 0xFF) as f32 / 255.0;
                    let a = (rgba & 0xFF) as f32 / 255.0;
                    self.title_shadow_color_btn.set_rgba(&gdk4::RGBA::new(r, g, b, a));
                }
                self.title_shadow_x_slider.set_value(c.title_shadow_offset_x);
                self.title_shadow_y_slider.set_value(c.title_shadow_offset_y);
                self.title_bg_box_check.set_active(c.title_bg_box);
                {
                    let rgba = c.title_bg_box_color;
                    let r = ((rgba >> 24) & 0xFF) as f32 / 255.0;
                    let g = ((rgba >> 16) & 0xFF) as f32 / 255.0;
                    let b = ((rgba >> 8) & 0xFF) as f32 / 255.0;
                    let a = (rgba & 0xFF) as f32 / 255.0;
                    self.title_bg_box_color_btn.set_rgba(&gdk4::RGBA::new(r, g, b, a));
                }
                self.title_bg_box_padding_slider.set_value(c.title_bg_box_padding);
                // When speed keyframes are present, don't auto-update the slider —
                // the user sets it to the desired value before clicking
                // "Set Speed Keyframe". Auto-resetting would clobber their input.
                // The slider updates when navigating keyframes (Prev/Next KF) or
                // when the clip selection changes.
                if c.speed_keyframes.is_empty() {
                    self.speed_slider.set_value(c.speed);
                }
                self.reverse_check.set_active(c.reverse);
                self.slow_motion_dropdown.set_selected(match c.slow_motion_interp {
                    crate::model::clip::SlowMotionInterp::Off => 0,
                    crate::model::clip::SlowMotionInterp::Blend => 1,
                    crate::model::clip::SlowMotionInterp::OpticalFlow => 2,
                });
                // LUT
                // Rebuild LUT list display
                while let Some(child) = self.lut_display_box.first_child() {
                    self.lut_display_box.remove(&child);
                }
                if c.lut_paths.is_empty() {
                    let none_label = Label::new(Some("None"));
                    none_label.set_halign(gtk4::Align::Start);
                    none_label.add_css_class("clip-path");
                    self.lut_display_box.append(&none_label);
                    self.lut_clear_btn.set_sensitive(false);
                } else {
                    for (i, path) in c.lut_paths.iter().enumerate() {
                        let name = std::path::Path::new(path)
                            .file_name()
                            .and_then(|n| n.to_str())
                            .unwrap_or(path.as_str());
                        let label = Label::new(Some(&format!("{}. {}", i + 1, name)));
                        label.set_halign(gtk4::Align::Start);
                        label.set_ellipsize(gtk4::pango::EllipsizeMode::Start);
                        label.set_tooltip_text(Some(path));
                        label.add_css_class("clip-path");
                        self.lut_display_box.append(&label);
                    }
                    self.lut_clear_btn.set_sensitive(true);
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
                    &self.path_status_value,
                    &self.in_value,
                    &self.out_value,
                    &self.dur_value,
                    &self.pos_value,
                ] {
                    l.set_text("—");
                }
                self.path_value.set_tooltip_text(None);
                self.path_status_value.remove_css_class("offline-label");
                self.path_status_value.add_css_class("dim-label");
                self.relink_btn.set_visible(false);
                self.brightness_slider.set_value(0.0);
                self.contrast_slider.set_value(1.0);
                self.saturation_slider.set_value(1.0);
                self.temperature_slider.set_value(6500.0);
                self.tint_slider.set_value(0.0);
                self.denoise_slider.set_value(0.0);
                self.sharpness_slider.set_value(0.0);
                self.blur_slider.set_value(0.0);
                self.vidstab_check.set_active(false);
                self.vidstab_slider.set_value(0.5);
                self.shadows_slider.set_value(0.0);
                self.midtones_slider.set_value(0.0);
                self.highlights_slider.set_value(0.0);
                self.volume_slider.set_value(0.0);
                self.pan_slider.set_value(0.0);
                self.measured_loudness_label.set_text("");
                let eq_defaults = crate::model::clip::default_eq_bands();
                for (i, band) in eq_defaults.iter().enumerate() {
                    if i < self.eq_freq_sliders.len() {
                        self.eq_freq_sliders[i].set_value(band.freq);
                        self.eq_gain_sliders[i].set_value(band.gain);
                        self.eq_q_sliders[i].set_value(band.q);
                    }
                }
                self.crop_left_slider.set_value(0.0);
                self.crop_right_slider.set_value(0.0);
                self.crop_top_slider.set_value(0.0);
                self.crop_bottom_slider.set_value(0.0);
                self.rotate_spin.set_value(0.0);
                self.flip_h_btn.set_active(false);
                self.flip_v_btn.set_active(false);
                self.scale_slider.set_value(1.0);
                self.opacity_slider.set_value(1.0);
                self.blend_mode_dropdown.set_selected(0);
                self.anamorphic_desqueeze_dropdown.set_selected(0);
                self.position_x_slider.set_value(0.0);
                self.position_y_slider.set_value(0.0);
                self.title_entry.set_text("");
                self.title_font_btn.set_label("Sans Bold 36");
                self.title_color_btn.set_rgba(&gdk4::RGBA::new(1.0, 1.0, 1.0, 1.0));
                self.title_x_slider.set_value(0.5);
                self.title_y_slider.set_value(0.9);
                self.title_outline_width_slider.set_value(0.0);
                self.title_outline_color_btn.set_rgba(&gdk4::RGBA::new(0.0, 0.0, 0.0, 1.0));
                self.title_shadow_check.set_active(false);
                self.title_shadow_color_btn.set_rgba(&gdk4::RGBA::new(0.0, 0.0, 0.0, 0.67));
                self.title_shadow_x_slider.set_value(2.0);
                self.title_shadow_y_slider.set_value(2.0);
                self.title_bg_box_check.set_active(false);
                self.title_bg_box_color_btn.set_rgba(&gdk4::RGBA::new(0.0, 0.0, 0.0, 0.53));
                self.title_bg_box_padding_slider.set_value(8.0);
                self.speed_slider.set_value(1.0);
                self.reverse_check.set_active(false);
                self.slow_motion_dropdown.set_selected(0);
                while let Some(child) = self.lut_display_box.first_child() {
                    self.lut_display_box.remove(&child);
                }
                let none_label = Label::new(Some("None"));
                none_label.set_halign(gtk4::Align::Start);
                none_label.add_css_class("clip-path");
                self.lut_display_box.append(&none_label);
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
                        &c.pan_keyframes[..],
                        &c.speed_keyframes[..],
                        &c.rotate_keyframes[..],
                        &c.crop_left_keyframes[..],
                        &c.crop_right_keyframes[..],
                        &c.crop_top_keyframes[..],
                        &c.crop_bottom_keyframes[..],
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
                // Audio keyframe indicator (volume or pan)
                if c.has_keyframe_at_local_ns_for_property(
                    Phase1KeyframeProperty::Volume,
                    local,
                    tolerance,
                ) || c.has_keyframe_at_local_ns_for_property(
                    Phase1KeyframeProperty::Pan,
                    local,
                    tolerance,
                ) {
                    self.audio_keyframe_indicator_label.set_text("◆ Aud KF");
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

    /// Lightweight update of keyframed inspector controls and keyframe
    /// indicators.  Called when the playhead moves (scrub, nav, playback) without
    /// a full clip re-selection.
    pub fn update_keyframed_sliders(&self, project: &Project, playhead_ns: u64) {
        let clip = self.selected_clip_id.borrow().clone().and_then(|id| {
            project
                .tracks
                .iter()
                .flat_map(|t| t.clips.iter())
                .find(|c| c.id == id)
                .cloned()
        });
        if let Some(c) = clip {
            *self.updating.borrow_mut() = true;
            self.volume_slider.set_value(linear_to_db_volume(
                c.value_for_phase1_property_at_timeline_ns(
                    Phase1KeyframeProperty::Volume,
                    playhead_ns,
                ),
            ));
            self.pan_slider
                .set_value(c.value_for_phase1_property_at_timeline_ns(
                    Phase1KeyframeProperty::Pan,
                    playhead_ns,
                ));
            self.scale_slider
                .set_value(c.value_for_phase1_property_at_timeline_ns(
                    Phase1KeyframeProperty::Scale,
                    playhead_ns,
                ));
            self.opacity_slider
                .set_value(c.value_for_phase1_property_at_timeline_ns(
                    Phase1KeyframeProperty::Opacity,
                    playhead_ns,
                ));
            self.position_x_slider
                .set_value(c.value_for_phase1_property_at_timeline_ns(
                    Phase1KeyframeProperty::PositionX,
                    playhead_ns,
                ));
            self.position_y_slider
                .set_value(c.value_for_phase1_property_at_timeline_ns(
                    Phase1KeyframeProperty::PositionY,
                    playhead_ns,
                ));
            self.rotate_spin
                .set_value(c.value_for_phase1_property_at_timeline_ns(
                    Phase1KeyframeProperty::Rotate,
                    playhead_ns,
                ));
            self.crop_left_slider
                .set_value(c.value_for_phase1_property_at_timeline_ns(
                    Phase1KeyframeProperty::CropLeft,
                    playhead_ns,
                ));
            self.crop_right_slider
                .set_value(c.value_for_phase1_property_at_timeline_ns(
                    Phase1KeyframeProperty::CropRight,
                    playhead_ns,
                ));
            self.crop_top_slider
                .set_value(c.value_for_phase1_property_at_timeline_ns(
                    Phase1KeyframeProperty::CropTop,
                    playhead_ns,
                ));
            self.crop_bottom_slider
                .set_value(c.value_for_phase1_property_at_timeline_ns(
                    Phase1KeyframeProperty::CropBottom,
                    playhead_ns,
                ));
            *self.updating.borrow_mut() = false;
        }
        self.update_keyframe_indicator(project, playhead_ns);
    }
}

/// Build the inspector panel.
/// Returns `(widget, InspectorView)` — keep `InspectorView` and call `.update()` on selection changes.
///
/// - `on_clip_changed`: fired when the clip name is applied (triggers full project-changed cycle).
/// - `on_color_changed`: fired on every color/effects slider movement with
///   `(brightness, contrast, saturation, temperature, tint, denoise, sharpness, blur, shadows, midtones, highlights, ...)`;
///   should update the program player's video filter elements directly without a full pipeline reload.
/// - `on_audio_changed`: fired on every audio slider movement with `(clip_id, volume, pan)`.
pub fn build_inspector(
    project: Rc<RefCell<Project>>,
    on_clip_changed: impl Fn() + 'static,
    on_color_changed: impl Fn(f32, f32, f32, f32, f32, f32, f32, f32, f32, f32, f32, f32, f32, f32, f32, f32, f32, f32, f32)
        + 'static,
    on_audio_changed: impl Fn(&str, f32, f32) + 'static,
    on_eq_changed: impl Fn(&str, [crate::model::clip::EqBand; 3]) + 'static,
    on_transform_changed: impl Fn(i32, i32, i32, i32, i32, bool, bool, f64, f64, f64) + 'static,
    on_anamorphic_changed: impl Fn(f64) + 'static,
    on_title_changed: impl Fn(String, f64, f64) + 'static,
    on_title_style_changed: impl Fn() + 'static,
    on_speed_changed: impl Fn(f64) + 'static,
    on_lut_changed: impl Fn(Option<String>) + 'static,
    on_opacity_changed: impl Fn(f64) + 'static,
    on_reverse_changed: impl Fn(bool) + 'static,
    on_chroma_key_changed: impl Fn() + 'static,
    on_chroma_key_slider_changed: impl Fn(f32, f32) + 'static,
    on_bg_removal_changed: impl Fn() + 'static,
    on_vidstab_changed: impl Fn() + 'static,
    on_frei0r_changed: impl Fn() + 'static,
    on_frei0r_params_changed: impl Fn() + 'static,
    on_speed_keyframe_changed: impl Fn(&str, f64, &[NumericKeyframe]) + 'static,
    current_playhead_ns: impl Fn() -> u64 + 'static,
    on_seek_to: impl Fn(u64) + 'static,
    on_normalize_audio: impl Fn(&str) + 'static,
    on_duck_changed: impl Fn(&str, bool, f64) + 'static,
    on_role_changed: impl Fn(&str, &str) + 'static,
    on_execute_command: impl Fn(Box<dyn crate::undo::EditCommand>) + 'static,
) -> (GBox, Rc<InspectorView>) {
    // Wrap frei0r callbacks in Rc so they can be cloned into multiple closures.
    let on_normalize_audio: Rc<dyn Fn(&str)> = Rc::new(on_normalize_audio);
    let on_duck_changed: Rc<dyn Fn(&str, bool, f64)> = Rc::new(on_duck_changed);
    let on_role_changed: Rc<dyn Fn(&str, &str)> = Rc::new(on_role_changed);
    let on_execute_command: Rc<dyn Fn(Box<dyn crate::undo::EditCommand>)> = Rc::new(on_execute_command);
    let on_vidstab_changed: Rc<dyn Fn()> = Rc::new(on_vidstab_changed);
    let on_frei0r_changed: Rc<dyn Fn()> = Rc::new(on_frei0r_changed);
    let on_frei0r_params_changed: Rc<dyn Fn()> = Rc::new(on_frei0r_params_changed);

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
    path_value.set_selectable(true);
    content_box.append(&path_value);
    row_label(&content_box, "Media Status");
    let path_status_value = value_label("—");
    path_status_value.add_css_class("dim-label");
    content_box.append(&path_status_value);

    let relink_btn = gtk4::Button::with_label("Relink…");
    relink_btn.set_tooltip_text(Some("Relink offline media by searching a folder"));
    relink_btn.add_css_class("small-btn");
    relink_btn.set_visible(false);
    content_box.append(&relink_btn);

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

    row_label(&color_inner, "Exposure");
    let exposure_slider = Scale::with_range(Orientation::Horizontal, -1.0, 1.0, 0.01);
    exposure_slider.set_value(0.0);
    exposure_slider.set_draw_value(true);
    exposure_slider.set_digits(2);
    exposure_slider.add_mark(0.0, gtk4::PositionType::Bottom, None);
    color_inner.append(&exposure_slider);

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

    row_label(&color_inner, "Black Point");
    let black_point_slider = Scale::with_range(Orientation::Horizontal, -1.0, 1.0, 0.01);
    black_point_slider.set_value(0.0);
    black_point_slider.set_draw_value(true);
    black_point_slider.set_digits(2);
    black_point_slider.add_mark(0.0, gtk4::PositionType::Bottom, None);
    color_inner.append(&black_point_slider);

    let ds_title = Label::new(Some("Denoise / Sharpness / Blur"));
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

    row_label(&color_inner, "Blur");
    let blur_slider = Scale::with_range(Orientation::Horizontal, 0.0, 1.0, 0.01);
    blur_slider.set_value(0.0);
    blur_slider.set_draw_value(true);
    blur_slider.set_digits(2);
    blur_slider.add_mark(0.0, gtk4::PositionType::Bottom, None);
    color_inner.append(&blur_slider);

    let stab_title = Label::new(Some("Stabilization"));
    stab_title.set_halign(gtk::Align::Start);
    stab_title.add_css_class("browser-header");
    color_inner.append(&stab_title);

    let vidstab_row = gtk4::Box::new(Orientation::Horizontal, 6);
    let vidstab_check = gtk4::CheckButton::with_label("Enable");
    vidstab_check.set_active(false);
    vidstab_row.append(&vidstab_check);
    let vidstab_note = Label::new(Some("(applied on export)"));
    vidstab_note.add_css_class("dim-label");
    vidstab_row.append(&vidstab_note);
    color_inner.append(&vidstab_row);

    row_label(&color_inner, "Smoothing");
    let vidstab_slider = Scale::with_range(Orientation::Horizontal, 0.0, 1.0, 0.01);
    vidstab_slider.set_value(0.5);
    vidstab_slider.set_draw_value(true);
    vidstab_slider.set_digits(2);
    vidstab_slider.add_mark(0.5, gtk4::PositionType::Bottom, None);
    color_inner.append(&vidstab_slider);

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

    row_label(&color_inner, "Highlights Warmth");
    let highlights_warmth_slider = Scale::with_range(Orientation::Horizontal, -1.0, 1.0, 0.01);
    highlights_warmth_slider.set_value(0.0);
    highlights_warmth_slider.set_draw_value(true);
    highlights_warmth_slider.set_digits(2);
    highlights_warmth_slider.add_mark(0.0, gtk4::PositionType::Bottom, None);
    color_inner.append(&highlights_warmth_slider);

    row_label(&color_inner, "Highlights Tint");
    let highlights_tint_slider = Scale::with_range(Orientation::Horizontal, -1.0, 1.0, 0.01);
    highlights_tint_slider.set_value(0.0);
    highlights_tint_slider.set_draw_value(true);
    highlights_tint_slider.set_digits(2);
    highlights_tint_slider.add_mark(0.0, gtk4::PositionType::Bottom, None);
    color_inner.append(&highlights_tint_slider);

    row_label(&color_inner, "Midtones Warmth");
    let midtones_warmth_slider = Scale::with_range(Orientation::Horizontal, -1.0, 1.0, 0.01);
    midtones_warmth_slider.set_value(0.0);
    midtones_warmth_slider.set_draw_value(true);
    midtones_warmth_slider.set_digits(2);
    midtones_warmth_slider.add_mark(0.0, gtk4::PositionType::Bottom, None);
    color_inner.append(&midtones_warmth_slider);

    row_label(&color_inner, "Midtones Tint");
    let midtones_tint_slider = Scale::with_range(Orientation::Horizontal, -1.0, 1.0, 0.01);
    midtones_tint_slider.set_value(0.0);
    midtones_tint_slider.set_draw_value(true);
    midtones_tint_slider.set_digits(2);
    midtones_tint_slider.add_mark(0.0, gtk4::PositionType::Bottom, None);
    color_inner.append(&midtones_tint_slider);

    row_label(&color_inner, "Shadows Warmth");
    let shadows_warmth_slider = Scale::with_range(Orientation::Horizontal, -1.0, 1.0, 0.01);
    shadows_warmth_slider.set_value(0.0);
    shadows_warmth_slider.set_draw_value(true);
    shadows_warmth_slider.set_digits(2);
    shadows_warmth_slider.add_mark(0.0, gtk4::PositionType::Bottom, None);
    color_inner.append(&shadows_warmth_slider);

    row_label(&color_inner, "Shadows Tint");
    let shadows_tint_slider = Scale::with_range(Orientation::Horizontal, -1.0, 1.0, 0.01);
    shadows_tint_slider.set_value(0.0);
    shadows_tint_slider.set_draw_value(true);
    shadows_tint_slider.set_digits(2);
    shadows_tint_slider.add_mark(0.0, gtk4::PositionType::Bottom, None);
    color_inner.append(&shadows_tint_slider);

    // ── Match Color button ───────────────────────────────────────────────────
    let match_color_sep = Separator::new(Orientation::Horizontal);
    color_inner.append(&match_color_sep);
    let match_color_btn = Button::with_label("Match Color…");
    match_color_btn.set_tooltip_text(Some(
        "Automatically adjust this clip's color to match another clip",
    ));
    color_inner.append(&match_color_btn);

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

    // Create shared state needed by effects and later sections.
    let selected_clip_id: Rc<RefCell<Option<String>>> = Rc::new(RefCell::new(None));

    // ── Applied Frei0r Effects section (Video + Image only) ──────────────
    let frei0r_effects_section = GBox::new(Orientation::Vertical, 8);
    content_box.append(&frei0r_effects_section);

    frei0r_effects_section.append(&Separator::new(Orientation::Horizontal));

    // Header row: "Applied Effects" expander + Copy/Paste buttons
    let frei0r_header_row = GBox::new(Orientation::Horizontal, 4);
    let frei0r_effects_expander = Expander::new(Some("Applied Effects"));
    frei0r_effects_expander.set_expanded(true);
    frei0r_effects_expander.set_hexpand(true);
    frei0r_header_row.append(&frei0r_effects_expander);

    let frei0r_effects_clipboard: Rc<RefCell<Option<Vec<crate::model::clip::Frei0rEffect>>>> =
        Rc::new(RefCell::new(None));

    let frei0r_copy_btn = Button::from_icon_name("edit-copy-symbolic");
    frei0r_copy_btn.set_tooltip_text(Some("Copy effects from this clip"));
    frei0r_copy_btn.add_css_class("flat");
    frei0r_header_row.append(&frei0r_copy_btn);

    let frei0r_paste_btn = Button::from_icon_name("edit-paste-symbolic");
    frei0r_paste_btn.set_tooltip_text(Some("Paste effects to this clip"));
    frei0r_paste_btn.add_css_class("flat");
    frei0r_paste_btn.set_sensitive(false);
    frei0r_header_row.append(&frei0r_paste_btn);

    frei0r_effects_section.append(&frei0r_header_row);

    let frei0r_effects_list = GBox::new(Orientation::Vertical, 4);
    frei0r_effects_expander.set_child(Some(&frei0r_effects_list));

    let frei0r_empty_label = Label::new(Some("No effects applied.\nUse the Effects tab to add frei0r filters."));
    frei0r_empty_label.set_wrap(true);
    frei0r_empty_label.add_css_class("panel-empty-state");
    frei0r_empty_label.set_margin_start(4);
    frei0r_effects_list.append(&frei0r_empty_label);

    // Wire copy effects button
    {
        let clipboard = frei0r_effects_clipboard.clone();
        let selected_clip_id = selected_clip_id.clone();
        let project = project.clone();
        let paste_btn = frei0r_paste_btn.clone();
        frei0r_copy_btn.connect_clicked(move |_| {
            let cid = selected_clip_id.borrow().clone();
            if let Some(cid) = cid {
                let proj = project.borrow();
                for track in &proj.tracks {
                    if let Some(clip) = track.clips.iter().find(|c| c.id == cid) {
                        let copied: Vec<crate::model::clip::Frei0rEffect> = clip
                            .frei0r_effects
                            .iter()
                            .cloned()
                            .collect();
                        let has_effects = !copied.is_empty();
                        *clipboard.borrow_mut() = Some(copied);
                        paste_btn.set_sensitive(has_effects);
                        return;
                    }
                }
            }
        });
    }

    // Wire paste effects button
    {
        let clipboard = frei0r_effects_clipboard.clone();
        let selected_clip_id = selected_clip_id.clone();
        let project = project.clone();
        let on_frei0r_changed = on_frei0r_changed.clone();
        let on_execute_command = on_execute_command.clone();
        frei0r_paste_btn.connect_clicked(move |_| {
            let effects_to_paste: Vec<_> = {
                let cb = clipboard.borrow();
                match cb.as_ref() {
                    Some(effects) if !effects.is_empty() => effects
                        .iter()
                        .map(|e| {
                            let mut new_effect = e.clone();
                            new_effect.id = uuid::Uuid::new_v4().to_string();
                            new_effect
                        })
                        .collect(),
                    _ => return,
                }
            };
            let cid = selected_clip_id.borrow().clone();
            if let Some(cid) = cid {
                let track_id = {
                    let proj = project.borrow();
                    proj.tracks.iter()
                        .find(|t| t.clips.iter().any(|c| c.id == cid))
                        .map(|t| t.id.clone())
                        .unwrap_or_default()
                };
                let insert_index = {
                    let proj = project.borrow();
                    proj.tracks.iter()
                        .find(|t| t.id == track_id)
                        .and_then(|t| t.clips.iter().find(|c| c.id == cid))
                        .map(|c| c.frei0r_effects.len())
                        .unwrap_or(0)
                };
                // Push one AddFrei0rEffectCommand per pasted effect so each can be undone.
                for (offset, effect) in effects_to_paste.into_iter().enumerate() {
                    on_execute_command(Box::new(crate::undo::AddFrei0rEffectCommand {
                        clip_id: cid.clone(),
                        track_id: track_id.clone(),
                        effect,
                        index: insert_index + offset,
                    }));
                }
                on_frei0r_changed();
            }
        });
    }

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

    let normalize_row = GBox::new(Orientation::Horizontal, 6);
    let normalize_btn = Button::with_label("Normalize\u{2026}");
    normalize_btn.set_tooltip_text(Some(
        "Analyze clip loudness and adjust volume to a target level",
    ));
    let measured_loudness_label = Label::new(None);
    measured_loudness_label.add_css_class("dim-label");
    measured_loudness_label.set_halign(gtk4::Align::Start);
    measured_loudness_label.set_hexpand(true);
    normalize_row.append(&normalize_btn);
    normalize_row.append(&measured_loudness_label);
    audio_inner.append(&normalize_row);

    // ── Audio keyframe navigation + animation mode ──
    let audio_keyframe_nav_row = GBox::new(Orientation::Horizontal, 4);
    let audio_prev_keyframe_btn = Button::with_label("◀ Prev KF");
    audio_prev_keyframe_btn.set_tooltip_text(Some("Jump to previous audio keyframe"));
    let audio_next_keyframe_btn = Button::with_label("Next KF ▶");
    audio_next_keyframe_btn.set_tooltip_text(Some("Jump to next audio keyframe"));
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
        "When active, volume and pan slider changes auto-create keyframes (Shift+K)",
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
    let pan_keyframe_row = GBox::new(Orientation::Horizontal, 6);
    let pan_set_keyframe_btn = Button::with_label("Set Pan Keyframe");
    let pan_remove_keyframe_btn = Button::with_label("Remove Pan Keyframe");
    pan_keyframe_row.append(&pan_set_keyframe_btn);
    pan_keyframe_row.append(&pan_remove_keyframe_btn);
    audio_inner.append(&pan_keyframe_row);

    // ── Equalizer sub-section (inside Audio) ───────────────────────────────
    let eq_expander = Expander::new(Some("Equalizer"));
    eq_expander.set_expanded(false);
    audio_inner.append(&eq_expander);
    let eq_inner = GBox::new(Orientation::Vertical, 4);
    eq_expander.set_child(Some(&eq_inner));

    let eq_band_labels = ["Low Band", "Mid Band", "High Band"];
    let eq_freq_ranges: [(f64, f64, f64); 3] = [
        (20.0, 1000.0, 200.0),
        (200.0, 8000.0, 1000.0),
        (1000.0, 20000.0, 5000.0),
    ];
    let mut eq_freq_sliders: Vec<Scale> = Vec::new();
    let mut eq_gain_sliders: Vec<Scale> = Vec::new();
    let mut eq_q_sliders: Vec<Scale> = Vec::new();
    for i in 0..3 {
        let band_label = Label::new(Some(eq_band_labels[i]));
        band_label.set_halign(gtk4::Align::Start);
        band_label.add_css_class("dim-label");
        eq_inner.append(&band_label);

        row_label(&eq_inner, "Freq (Hz)");
        let freq_slider = Scale::with_range(
            Orientation::Horizontal,
            eq_freq_ranges[i].0,
            eq_freq_ranges[i].1,
            1.0,
        );
        freq_slider.set_value(eq_freq_ranges[i].2);
        freq_slider.set_draw_value(true);
        freq_slider.set_digits(0);
        freq_slider.add_mark(eq_freq_ranges[i].2, gtk4::PositionType::Bottom, None);
        eq_inner.append(&freq_slider);
        eq_freq_sliders.push(freq_slider);

        row_label(&eq_inner, "Gain (dB)");
        let gain_slider = Scale::with_range(Orientation::Horizontal, -24.0, 12.0, 0.1);
        gain_slider.set_value(0.0);
        gain_slider.set_draw_value(true);
        gain_slider.set_digits(1);
        gain_slider.add_mark(0.0, gtk4::PositionType::Bottom, None);
        eq_inner.append(&gain_slider);
        eq_gain_sliders.push(gain_slider);

        row_label(&eq_inner, "Q");
        let q_slider = Scale::with_range(Orientation::Horizontal, 0.1, 10.0, 0.1);
        q_slider.set_value(1.0);
        q_slider.set_draw_value(true);
        q_slider.set_digits(1);
        q_slider.add_mark(1.0, gtk4::PositionType::Bottom, None);
        eq_inner.append(&q_slider);
        eq_q_sliders.push(q_slider);
    }

    // ── Channels sub-section (inside Audio) ────────────────────────────────
    row_label(&audio_inner, "Channels");
    #[allow(deprecated)]
    let channel_mode_dropdown = gtk4::ComboBoxText::new();
    for mode in crate::model::clip::AudioChannelMode::ALL {
        #[allow(deprecated)]
        channel_mode_dropdown.append(Some(mode.as_str()), mode.label());
    }
    #[allow(deprecated)]
    channel_mode_dropdown.set_active_id(Some("stereo"));
    audio_inner.append(&channel_mode_dropdown);

    // ── Pitch sub-section (inside Audio) ───────────────────────────────────
    let pitch_expander = Expander::new(Some("Pitch"));
    pitch_expander.set_expanded(false);
    audio_inner.append(&pitch_expander);
    let pitch_inner = GBox::new(Orientation::Vertical, 4);
    pitch_expander.set_child(Some(&pitch_inner));

    row_label(&pitch_inner, "Pitch Shift (semitones)");
    let pitch_shift_slider = Scale::with_range(Orientation::Horizontal, -12.0, 12.0, 0.5);
    pitch_shift_slider.set_value(0.0);
    pitch_shift_slider.set_draw_value(true);
    pitch_shift_slider.set_digits(1);
    pitch_shift_slider.add_mark(0.0, gtk4::PositionType::Bottom, Some("0"));
    pitch_shift_slider.add_mark(-12.0, gtk4::PositionType::Bottom, Some("-12"));
    pitch_shift_slider.add_mark(12.0, gtk4::PositionType::Bottom, Some("+12"));
    pitch_inner.append(&pitch_shift_slider);

    let pitch_preserve_check =
        gtk4::CheckButton::with_label("Preserve pitch during speed changes");
    pitch_preserve_check.set_tooltip_text(Some(
        "Use Rubberband time-stretch to keep audio pitch constant when clip speed is changed",
    ));
    pitch_inner.append(&pitch_preserve_check);

    let pitch_hint = Label::new(Some("Pitch shift via Rubberband.\n0 = original pitch, \u{00b1}12 = \u{00b1}1 octave."));
    pitch_hint.set_halign(gtk4::Align::Start);
    pitch_hint.add_css_class("dim-label");
    pitch_inner.append(&pitch_hint);

    // ── Applied Audio Effects (LADSPA) sub-section ────────────────────────
    let ladspa_effects_expander = Expander::new(Some("Applied Audio Effects"));
    ladspa_effects_expander.set_expanded(false);
    audio_inner.append(&ladspa_effects_expander);
    let ladspa_effects_list = GBox::new(Orientation::Vertical, 4);
    ladspa_effects_expander.set_child(Some(&ladspa_effects_list));

    // ── Track Audio sub-section (Role + Ducking) ──────────────────────────
    let duck_expander = Expander::new(Some("Track Audio"));
    duck_expander.set_expanded(false);
    audio_inner.append(&duck_expander);
    let duck_inner = GBox::new(Orientation::Vertical, 4);
    duck_expander.set_child(Some(&duck_inner));

    // Audio Role dropdown
    row_label(&duck_inner, "Audio Role");
    #[allow(deprecated)]
    let role_dropdown = gtk4::ComboBoxText::new();
    for role in crate::model::track::AudioRole::ALL {
        #[allow(deprecated)]
        role_dropdown.append(Some(role.as_str()), role.label());
    }
    #[allow(deprecated)]
    role_dropdown.set_active_id(Some("none"));
    duck_inner.append(&role_dropdown);

    let duck_check = gtk4::CheckButton::with_label("Duck this track when dialogue is present");
    duck_check.set_active(false);
    duck_inner.append(&duck_check);

    row_label(&duck_inner, "Duck Amount (dB)");
    let duck_amount_slider = Scale::with_range(Orientation::Horizontal, -24.0, 0.0, 0.5);
    duck_amount_slider.set_value(-6.0);
    duck_amount_slider.set_draw_value(true);
    duck_amount_slider.set_digits(1);
    duck_amount_slider.add_mark(-6.0, gtk4::PositionType::Bottom, Some("-6 dB"));
    duck_amount_slider.add_mark(-12.0, gtk4::PositionType::Bottom, Some("-12 dB"));
    duck_inner.append(&duck_amount_slider);

    let duck_hint = Label::new(Some("Lowers this track\u{2019}s volume when audio from\nnon-ducked tracks (e.g. dialogue) is playing"));
    duck_hint.set_halign(gtk4::Align::Start);
    duck_hint.add_css_class("dim-label");
    duck_inner.append(&duck_hint);

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
    let interp_dropdown =
        gtk4::DropDown::from_strings(&["Linear", "Ease In", "Ease Out", "Ease In/Out"]);
    interp_dropdown.set_selected(0);
    interp_dropdown.set_tooltip_text(Some("Interpolation mode for new keyframes"));
    interp_dropdown.set_hexpand(true);
    interp_row.append(&interp_label);
    interp_row.append(&interp_dropdown);
    transform_inner.append(&interp_row);

    transform_inner.append(&Separator::new(Orientation::Horizontal));

    row_label(&transform_inner, "Blend Mode");
    let blend_mode_dropdown = gtk4::DropDown::from_strings(&[
        "Normal", "Multiply", "Screen", "Overlay", "Add", "Difference", "Soft Light",
    ]);
    blend_mode_dropdown.set_selected(0);
    blend_mode_dropdown.set_halign(gtk4::Align::Start);
    blend_mode_dropdown.set_hexpand(true);
    blend_mode_dropdown.set_tooltip_text(Some("Compositing blend mode"));
    transform_inner.append(&blend_mode_dropdown);

    row_label(&transform_inner, "Anamorphic Desqueeze");
    let anamorphic_desqueeze_dropdown = gtk4::DropDown::from_strings(&[
        "None (1.0x)", "1.33x", "1.5x", "1.8x", "2.0x",
    ]);
    anamorphic_desqueeze_dropdown.set_selected(0);
    anamorphic_desqueeze_dropdown.set_halign(gtk4::Align::Start);
    anamorphic_desqueeze_dropdown.set_hexpand(true);
    anamorphic_desqueeze_dropdown.set_tooltip_text(Some("Anamorphic lens desqueeze factor"));
    transform_inner.append(&anamorphic_desqueeze_dropdown);

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
    let crop_left_keyframe_row = GBox::new(Orientation::Horizontal, 6);
    let crop_left_set_keyframe_btn = Button::with_label("Set Crop Left Keyframe");
    let crop_left_remove_keyframe_btn = Button::with_label("Remove Crop Left Keyframe");
    crop_left_keyframe_row.append(&crop_left_set_keyframe_btn);
    crop_left_keyframe_row.append(&crop_left_remove_keyframe_btn);
    transform_inner.append(&crop_left_keyframe_row);

    row_label(&transform_inner, "Crop Right");
    let crop_right_slider = Scale::with_range(Orientation::Horizontal, 0.0, 500.0, 2.0);
    crop_right_slider.set_value(0.0);
    crop_right_slider.set_draw_value(true);
    crop_right_slider.set_digits(0);
    transform_inner.append(&crop_right_slider);
    let crop_right_keyframe_row = GBox::new(Orientation::Horizontal, 6);
    let crop_right_set_keyframe_btn = Button::with_label("Set Crop Right Keyframe");
    let crop_right_remove_keyframe_btn = Button::with_label("Remove Crop Right Keyframe");
    crop_right_keyframe_row.append(&crop_right_set_keyframe_btn);
    crop_right_keyframe_row.append(&crop_right_remove_keyframe_btn);
    transform_inner.append(&crop_right_keyframe_row);

    row_label(&transform_inner, "Crop Top");
    let crop_top_slider = Scale::with_range(Orientation::Horizontal, 0.0, 500.0, 2.0);
    crop_top_slider.set_value(0.0);
    crop_top_slider.set_draw_value(true);
    crop_top_slider.set_digits(0);
    transform_inner.append(&crop_top_slider);
    let crop_top_keyframe_row = GBox::new(Orientation::Horizontal, 6);
    let crop_top_set_keyframe_btn = Button::with_label("Set Crop Top Keyframe");
    let crop_top_remove_keyframe_btn = Button::with_label("Remove Crop Top Keyframe");
    crop_top_keyframe_row.append(&crop_top_set_keyframe_btn);
    crop_top_keyframe_row.append(&crop_top_remove_keyframe_btn);
    transform_inner.append(&crop_top_keyframe_row);

    row_label(&transform_inner, "Crop Bottom");
    let crop_bottom_slider = Scale::with_range(Orientation::Horizontal, 0.0, 500.0, 2.0);
    crop_bottom_slider.set_value(0.0);
    crop_bottom_slider.set_draw_value(true);
    crop_bottom_slider.set_digits(0);
    transform_inner.append(&crop_bottom_slider);
    let crop_bottom_keyframe_row = GBox::new(Orientation::Horizontal, 6);
    let crop_bottom_set_keyframe_btn = Button::with_label("Set Crop Bottom Keyframe");
    let crop_bottom_remove_keyframe_btn = Button::with_label("Remove Crop Bottom Keyframe");
    crop_bottom_keyframe_row.append(&crop_bottom_set_keyframe_btn);
    crop_bottom_keyframe_row.append(&crop_bottom_remove_keyframe_btn);
    transform_inner.append(&crop_bottom_keyframe_row);

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
            let rad = (-rotate_value.get() - 90.0).to_radians();
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
    let rotate_keyframe_row = GBox::new(Orientation::Horizontal, 6);
    let rotate_set_keyframe_btn = Button::with_label("Set Rotate Keyframe");
    let rotate_remove_keyframe_btn = Button::with_label("Remove Rotate Keyframe");
    rotate_keyframe_row.append(&rotate_set_keyframe_btn);
    rotate_keyframe_row.append(&rotate_remove_keyframe_btn);
    transform_inner.append(&rotate_keyframe_row);

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

    row_label(&title_inner, "Font");
    let title_font_btn = gtk4::Button::with_label("Sans Bold 36");
    title_font_btn.set_tooltip_text(Some("Click to choose a font"));
    title_inner.append(&title_font_btn);

    row_label(&title_inner, "Text Color");
    let title_color_dialog = gtk4::ColorDialog::new();
    title_color_dialog.set_with_alpha(true);
    let title_color_btn = gtk4::ColorDialogButton::new(Some(title_color_dialog));
    title_color_btn.set_rgba(&gdk4::RGBA::new(1.0, 1.0, 1.0, 1.0));
    title_inner.append(&title_color_btn);

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

    row_label(&title_inner, "Outline Width");
    let title_outline_width_slider = Scale::with_range(Orientation::Horizontal, 0.0, 10.0, 0.5);
    title_outline_width_slider.set_value(0.0);
    title_outline_width_slider.set_draw_value(true);
    title_outline_width_slider.set_digits(1);
    title_outline_width_slider.set_hexpand(true);
    title_inner.append(&title_outline_width_slider);

    row_label(&title_inner, "Outline Color");
    let title_outline_color_dialog = gtk4::ColorDialog::new();
    title_outline_color_dialog.set_with_alpha(true);
    let title_outline_color_btn = gtk4::ColorDialogButton::new(Some(title_outline_color_dialog));
    title_outline_color_btn.set_rgba(&gdk4::RGBA::new(0.0, 0.0, 0.0, 1.0));
    title_inner.append(&title_outline_color_btn);

    let title_shadow_check = CheckButton::with_label("Drop Shadow");
    title_inner.append(&title_shadow_check);

    row_label(&title_inner, "Shadow Color");
    let title_shadow_color_dialog = gtk4::ColorDialog::new();
    title_shadow_color_dialog.set_with_alpha(true);
    let title_shadow_color_btn = gtk4::ColorDialogButton::new(Some(title_shadow_color_dialog));
    title_shadow_color_btn.set_rgba(&gdk4::RGBA::new(0.0, 0.0, 0.0, 0.67));
    title_inner.append(&title_shadow_color_btn);

    row_label(&title_inner, "Shadow Offset X");
    let title_shadow_x_slider = Scale::with_range(Orientation::Horizontal, -10.0, 10.0, 0.5);
    title_shadow_x_slider.set_value(2.0);
    title_shadow_x_slider.set_draw_value(true);
    title_shadow_x_slider.set_digits(1);
    title_shadow_x_slider.set_hexpand(true);
    title_inner.append(&title_shadow_x_slider);

    row_label(&title_inner, "Shadow Offset Y");
    let title_shadow_y_slider = Scale::with_range(Orientation::Horizontal, -10.0, 10.0, 0.5);
    title_shadow_y_slider.set_value(2.0);
    title_shadow_y_slider.set_draw_value(true);
    title_shadow_y_slider.set_digits(1);
    title_shadow_y_slider.set_hexpand(true);
    title_inner.append(&title_shadow_y_slider);

    let title_bg_box_check = CheckButton::with_label("Background Box");
    title_inner.append(&title_bg_box_check);

    row_label(&title_inner, "Box Color");
    let title_bg_box_color_dialog = gtk4::ColorDialog::new();
    title_bg_box_color_dialog.set_with_alpha(true);
    let title_bg_box_color_btn = gtk4::ColorDialogButton::new(Some(title_bg_box_color_dialog));
    title_bg_box_color_btn.set_rgba(&gdk4::RGBA::new(0.0, 0.0, 0.0, 0.53));
    title_inner.append(&title_bg_box_color_btn);

    row_label(&title_inner, "Box Padding");
    let title_bg_box_padding_slider = Scale::with_range(Orientation::Horizontal, 0.0, 30.0, 1.0);
    title_bg_box_padding_slider.set_value(8.0);
    title_bg_box_padding_slider.set_draw_value(true);
    title_bg_box_padding_slider.set_digits(0);
    title_bg_box_padding_slider.set_hexpand(true);
    title_inner.append(&title_bg_box_padding_slider);

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

    let speed_keyframe_row = GBox::new(Orientation::Horizontal, 6);
    let speed_set_keyframe_btn = Button::with_label("Set Speed Keyframe");
    let speed_remove_keyframe_btn = Button::with_label("Remove Speed Keyframe");
    speed_keyframe_row.append(&speed_set_keyframe_btn);
    speed_keyframe_row.append(&speed_remove_keyframe_btn);
    speed_inner.append(&speed_keyframe_row);

    let speed_keyframe_nav_row = GBox::new(Orientation::Horizontal, 4);
    let speed_prev_keyframe_btn = Button::with_label("◀ Prev KF");
    speed_prev_keyframe_btn.set_tooltip_text(Some("Jump to previous speed keyframe"));
    let speed_next_keyframe_btn = Button::with_label("Next KF ▶");
    speed_next_keyframe_btn.set_tooltip_text(Some("Jump to next speed keyframe"));
    speed_keyframe_nav_row.append(&speed_prev_keyframe_btn);
    speed_keyframe_nav_row.append(&speed_next_keyframe_btn);
    speed_inner.append(&speed_keyframe_nav_row);

    let reverse_check = CheckButton::with_label("Reverse (play clip backwards)");
    reverse_check.set_tooltip_text(Some(
        "Play this clip in reverse in Program Monitor preview and export. A ◀ badge appears on the timeline clip.",
    ));
    speed_inner.append(&reverse_check);

    // Slow-motion interpolation dropdown
    row_label(&speed_inner, "Slow-Motion Interpolation:");
    let smo_interp_model = StringList::new(&["Off", "Frame Blending", "Optical Flow"]);
    let slow_motion_dropdown = DropDown::new(Some(smo_interp_model), gtk4::Expression::NONE);
    slow_motion_dropdown.set_selected(0);
    slow_motion_dropdown.set_tooltip_text(Some(
        "Synthesizes intermediate frames on export for smooth slow-motion (clips with speed < 1.0 only)",
    ));
    speed_inner.append(&slow_motion_dropdown);
    let smo_note = Label::new(Some("Synthesizes frames on export (slow-motion clips only)"));
    smo_note.set_halign(gtk4::Align::Start);
    smo_note.add_css_class("clip-path");
    speed_inner.append(&smo_note);

    // ── LUT section (Video + Image only) ─────────────────────────────────────
    let lut_section_box = GBox::new(Orientation::Vertical, 8);
    content_box.append(&lut_section_box);

    lut_section_box.append(&Separator::new(Orientation::Horizontal));
    let lut_expander = Expander::new(Some("Color LUT"));
    lut_expander.set_expanded(false);
    lut_section_box.append(&lut_expander);
    let lut_inner = GBox::new(Orientation::Vertical, 8);
    lut_expander.set_child(Some(&lut_inner));

    let lut_display_box = GBox::new(Orientation::Vertical, 2);
    let lut_none_label = Label::new(Some("None"));
    lut_none_label.set_halign(gtk4::Align::Start);
    lut_none_label.add_css_class("clip-path");
    lut_display_box.append(&lut_none_label);
    lut_inner.append(&lut_display_box);

    let lut_btn_row = GBox::new(Orientation::Horizontal, 8);
    let lut_import_btn = Button::with_label("Add LUT…");
    let lut_clear_btn = Button::with_label("Clear All");
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
    let updating: Rc<RefCell<bool>> = Rc::new(RefCell::new(false));

    let on_clip_changed: Rc<dyn Fn()> = Rc::new(on_clip_changed);
    let on_color_changed: Rc<
        dyn Fn(
            f32,
            f32,
            f32,
            f32,
            f32,
            f32,
            f32,
            f32,
            f32,
            f32,
            f32,
            f32,
            f32,
            f32,
            f32,
            f32,
            f32,
            f32,
            f32,
        ),
    > = Rc::new(on_color_changed);
    let on_audio_changed: Rc<dyn Fn(&str, f32, f32)> = Rc::new(on_audio_changed);
    let on_eq_changed: Rc<dyn Fn(&str, [crate::model::clip::EqBand; 3])> =
        Rc::new(on_eq_changed);
    let on_transform_changed: Rc<dyn Fn(i32, i32, i32, i32, i32, bool, bool, f64, f64, f64)> =
        Rc::new(on_transform_changed);
    let on_title_changed: Rc<dyn Fn(String, f64, f64)> = Rc::new(on_title_changed);
    let on_title_style_changed: Rc<dyn Fn()> = Rc::new(on_title_style_changed);
    let on_speed_changed: Rc<dyn Fn(f64)> = Rc::new(on_speed_changed);
    let on_lut_changed: Rc<dyn Fn(Option<String>)> = Rc::new(on_lut_changed);
    let on_opacity_changed: Rc<dyn Fn(f64)> = Rc::new(on_opacity_changed);
    let on_reverse_changed: Rc<dyn Fn(bool)> = Rc::new(on_reverse_changed);
    let on_chroma_key_changed: Rc<dyn Fn()> = Rc::new(on_chroma_key_changed);
    let on_chroma_key_slider_changed: Rc<dyn Fn(f32, f32)> = Rc::new(on_chroma_key_slider_changed);
    let on_bg_removal_changed: Rc<dyn Fn()> = Rc::new(on_bg_removal_changed);
    let on_speed_keyframe_changed: Rc<dyn Fn(&str, f64, &[NumericKeyframe])> =
        Rc::new(on_speed_keyframe_changed);
    let current_playhead_ns: Rc<dyn Fn() -> u64> = Rc::new(current_playhead_ns);
    let on_seek_to: Rc<dyn Fn(u64)> = Rc::new(on_seek_to);

    // Apply name button — triggers full on_project_changed
    {
        let project = project.clone();
        let selected_clip_id = selected_clip_id.clone();
        let name_entry_cb = name_entry.clone();
        let on_clip_changed = on_clip_changed.clone();
        let on_execute_command = on_execute_command.clone();

        apply_btn.connect_clicked(move |_| {
            let new_name = name_entry_cb.text().to_string();
            if new_name.is_empty() {
                return;
            }
            let id = selected_clip_id.borrow().clone();
            if let Some(ref clip_id) = id {
                let (old_label, track_id) = {
                    let proj = project.borrow();
                    let mut old = String::new();
                    let mut tid = String::new();
                    for track in &proj.tracks {
                        if let Some(clip) = track.clips.iter().find(|c| &c.id == clip_id) {
                            old = clip.label.clone();
                            tid = track.id.clone();
                            break;
                        }
                    }
                    (old, tid)
                };
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
                if old_label != new_name {
                    on_execute_command(Box::new(crate::undo::SetClipLabelCommand {
                        clip_id: clip_id.clone(),
                        track_id,
                        old_label,
                        new_label: new_name,
                    }));
                }
                on_clip_changed();
            }
        });
    }

    // Helper: connect an effects slider — updates the model field then fires on_color_changed
    // with all current values so the program player can update its filters directly.
    fn connect_color_slider(
        slider: &Scale,
        project: Rc<RefCell<Project>>,
        selected_clip_id: Rc<RefCell<Option<String>>>,
        updating: Rc<RefCell<bool>>,
        on_color_changed: Rc<
            dyn Fn(
                f32,
                f32,
                f32,
                f32,
                f32,
                f32,
                f32,
                f32,
                f32,
                f32,
                f32,
                f32,
                f32,
                f32,
                f32,
                f32,
                f32,
                f32,
                f32,
            ),
        >,
        brightness_slider: Scale,
        contrast_slider: Scale,
        saturation_slider: Scale,
        temperature_slider: Scale,
        tint_slider: Scale,
        denoise_slider: Scale,
        sharpness_slider: Scale,
        blur_slider: Scale,
        shadows_slider: Scale,
        midtones_slider: Scale,
        highlights_slider: Scale,
        exposure_slider: Scale,
        black_point_slider: Scale,
        highlights_warmth_slider: Scale,
        highlights_tint_slider: Scale,
        midtones_warmth_slider: Scale,
        midtones_tint_slider: Scale,
        shadows_warmth_slider: Scale,
        shadows_tint_slider: Scale,
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
                let bl = blur_slider.value() as f32;
                let shd = shadows_slider.value() as f32;
                let mid = midtones_slider.value() as f32;
                let hil = highlights_slider.value() as f32;
                let exp = exposure_slider.value() as f32;
                let bp = black_point_slider.value() as f32;
                let hw = highlights_warmth_slider.value() as f32;
                let ht = highlights_tint_slider.value() as f32;
                let mw = midtones_warmth_slider.value() as f32;
                let mt = midtones_tint_slider.value() as f32;
                let sw = shadows_warmth_slider.value() as f32;
                let st = shadows_tint_slider.value() as f32;
                on_color_changed(
                    b, c, sat, temp, tnt, d, sh, bl, shd, mid, hil, exp, bp, hw, ht, mw, mt, sw, st,
                );
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
                blur_slider.clone(),
                shadows_slider.clone(),
                midtones_slider.clone(),
                highlights_slider.clone(),
                exposure_slider.clone(),
                black_point_slider.clone(),
                highlights_warmth_slider.clone(),
                highlights_tint_slider.clone(),
                midtones_warmth_slider.clone(),
                midtones_tint_slider.clone(),
                shadows_warmth_slider.clone(),
                shadows_tint_slider.clone(),
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
    wire_color_slider!(blur_slider, |clip, v| clip.blur = v);
    // Wire vidstab smoothing slider — triggers proxy re-request, no preview pipeline rebuild.
    {
        let project = project.clone();
        let selected_clip_id = selected_clip_id.clone();
        let updating = updating.clone();
        let on_vidstab_changed = on_vidstab_changed.clone();
        vidstab_slider.connect_value_changed(move |slider| {
            if *updating.borrow() {
                return;
            }
            let v = slider.value() as f32;
            let id = selected_clip_id.borrow().clone();
            if let Some(ref clip_id) = id {
                {
                    let mut proj = project.borrow_mut();
                    for track in &mut proj.tracks {
                        if let Some(clip) = track.clips.iter_mut().find(|c| &c.id == clip_id) {
                            clip.vidstab_smoothing = v;
                            proj.dirty = true;
                            break;
                        }
                    }
                }
                on_vidstab_changed();
            }
        });
    }

    // Wire vidstab enable checkbox — triggers proxy re-request when proxy mode is enabled.
    {
        let project = project.clone();
        let selected_clip_id = selected_clip_id.clone();
        let updating = updating.clone();
        let on_vidstab_changed = on_vidstab_changed.clone();
        vidstab_check.connect_toggled(move |btn| {
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
                            clip.vidstab_enabled = enabled;
                            proj.dirty = true;
                            break;
                        }
                    }
                }
                on_vidstab_changed();
            }
        });
    }

    wire_color_slider!(shadows_slider, |clip, v| clip.shadows = v);
    wire_color_slider!(midtones_slider, |clip, v| clip.midtones = v);
    wire_color_slider!(highlights_slider, |clip, v| clip.highlights = v);
    wire_color_slider!(exposure_slider, |clip, v| clip.exposure = v);
    wire_color_slider!(black_point_slider, |clip, v| clip.black_point = v);
    wire_color_slider!(highlights_warmth_slider, |clip, v| clip.highlights_warmth =
        v);
    wire_color_slider!(highlights_tint_slider, |clip, v| clip.highlights_tint = v);
    wire_color_slider!(midtones_warmth_slider, |clip, v| clip.midtones_warmth = v);
    wire_color_slider!(midtones_tint_slider, |clip, v| clip.midtones_tint = v);
    wire_color_slider!(shadows_warmth_slider, |clip, v| clip.shadows_warmth = v);
    wire_color_slider!(shadows_tint_slider, |clip, v| clip.shadows_tint = v);

    // Undo support for color sliders: GestureClick + EventControllerFocus snapshot/commit.
    fn attach_color_undo(
        slider: &Scale,
        project: Rc<RefCell<Project>>,
        selected_clip_id: Rc<RefCell<Option<String>>>,
        on_execute_command: Rc<dyn Fn(Box<dyn crate::undo::EditCommand>)>,
    ) {
        type SnapCell = Rc<RefCell<Option<(String, String, crate::undo::ClipColorSnapshot)>>>;
        let snap: SnapCell = Rc::new(RefCell::new(None));

        let do_snapshot = {
            let project = project.clone();
            let selected_clip_id = selected_clip_id.clone();
            let snap = snap.clone();
            move || {
                let cid = selected_clip_id.borrow().clone();
                if let Some(cid) = cid {
                    let proj = project.borrow();
                    for track in &proj.tracks {
                        if let Some(clip) = track.clips.iter().find(|c| c.id == cid) {
                            *snap.borrow_mut() = Some((
                                cid.clone(),
                                track.id.clone(),
                                crate::undo::ClipColorSnapshot::from_clip(clip),
                            ));
                            break;
                        }
                    }
                }
            }
        };

        let do_commit = {
            let project = project.clone();
            let snap = snap.clone();
            let on_execute_command = on_execute_command.clone();
            move || {
                let entry = snap.borrow_mut().take();
                if let Some((clip_id, track_id, old_color)) = entry {
                    let new_color = {
                        let proj = project.borrow();
                        proj.tracks.iter()
                            .find(|t| t.id == track_id)
                            .and_then(|t| t.clips.iter().find(|c| c.id == clip_id))
                            .map(|clip| crate::undo::ClipColorSnapshot::from_clip(clip))
                    };
                    if let Some(new_color) = new_color {
                        on_execute_command(Box::new(crate::undo::SetClipColorCommand {
                            clip_id,
                            track_id,
                            old_color,
                            new_color,
                        }));
                    }
                }
            }
        };

        let ges = gtk4::GestureClick::new();
        {
            let do_snapshot = do_snapshot.clone();
            ges.connect_pressed(move |_, _, _, _| { do_snapshot(); });
        }
        {
            let do_commit = do_commit.clone();
            ges.connect_released(move |_, _, _, _| { do_commit(); });
        }
        slider.add_controller(ges);

        let focus_ctrl = gtk4::EventControllerFocus::new();
        {
            let do_snapshot = do_snapshot.clone();
            focus_ctrl.connect_enter(move |_| { do_snapshot(); });
        }
        {
            focus_ctrl.connect_leave(move |_| { do_commit(); });
        }
        slider.add_controller(focus_ctrl);
    }

    for s in [
        &brightness_slider, &contrast_slider, &saturation_slider,
        &temperature_slider, &tint_slider, &denoise_slider,
        &sharpness_slider, &blur_slider, &shadows_slider,
        &midtones_slider, &highlights_slider, &exposure_slider,
        &black_point_slider, &highlights_warmth_slider, &highlights_tint_slider,
        &midtones_warmth_slider, &midtones_tint_slider,
        &shadows_warmth_slider, &shadows_tint_slider,
    ] {
        attach_color_undo(
            s,
            project.clone(),
            selected_clip_id.clone(),
            on_execute_command.clone(),
        );
    }

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
        let on_clip_changed = on_clip_changed.clone();
        blend_mode_dropdown.connect_selected_notify(move |combo| {
            if *updating.borrow() {
                return;
            }
            let id = selected_clip_id.borrow().clone();
            if let Some(ref clip_id) = id {
                let mode = crate::model::clip::BlendMode::ALL
                    .get(combo.selected() as usize)
                    .copied()
                    .unwrap_or_default();
                {
                    let mut proj = project.borrow_mut();
                    for track in &mut proj.tracks {
                        if let Some(clip) = track.clips.iter_mut().find(|c| &c.id == clip_id) {
                            clip.blend_mode = mode;
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
        let selected_clip_id = selected_clip_id.clone();
        let updating = updating.clone();
        let on_anamorphic_changed = Rc::new(on_anamorphic_changed);
        anamorphic_desqueeze_dropdown.connect_selected_notify(move |combo| {
            if *updating.borrow() {
                return;
            }
            if selected_clip_id.borrow().is_some() {
                let factor = match combo.selected() {
                    1 => 1.33,
                    2 => 1.5,
                    3 => 1.8,
                    4 => 2.0,
                    _ => 1.0,
                };
                on_anamorphic_changed(factor);
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
                // Use lightweight audio update (syncs keyframes to player without
                // full pipeline reload). on_clip_changed would cause a heavy rebuild
                // and visible playhead jump for every slider tick.
                on_audio_changed(clip_id, linear_vol, pan_slider_cb.value() as f32);
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
        let animation_mode = animation_mode.clone();
        let current_playhead_ns = current_playhead_ns.clone();
        let interp_dropdown = interp_dropdown.clone();
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
                            let has_kfs = !clip.pan_keyframes.is_empty();
                            if animation_mode.get() || has_kfs {
                                let interp = interp_idx_to_enum(interp_dropdown.selected());
                                clip.upsert_phase1_keyframe_at_timeline_ns_with_interp(
                                    Phase1KeyframeProperty::Pan,
                                    current_playhead_ns(),
                                    val as f64,
                                    interp,
                                );
                            } else {
                                clip.pan = val;
                            }
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

    // Undo controllers for volume and pan sliders (shared snapshot).
    {
        type VolSnapCell = Rc<RefCell<Option<(String, String, f32, f32)>>>;
        let vol_snap: VolSnapCell = Rc::new(RefCell::new(None));

        let do_vol_snapshot = {
            let project = project.clone();
            let selected_clip_id = selected_clip_id.clone();
            let vol_snap = vol_snap.clone();
            move || {
                let cid = selected_clip_id.borrow().clone();
                if let Some(cid) = cid {
                    let proj = project.borrow();
                    for track in &proj.tracks {
                        if let Some(clip) = track.clips.iter().find(|c| c.id == cid) {
                            *vol_snap.borrow_mut() = Some((
                                cid.clone(),
                                track.id.clone(),
                                clip.volume,
                                clip.pan,
                            ));
                            break;
                        }
                    }
                }
            }
        };

        let do_vol_commit = {
            let project = project.clone();
            let vol_snap = vol_snap.clone();
            let on_execute_command = on_execute_command.clone();
            move || {
                let entry = vol_snap.borrow_mut().take();
                if let Some((clip_id, track_id, old_volume, old_pan)) = entry {
                    let (new_volume, new_pan) = {
                        let proj = project.borrow();
                        proj.tracks.iter()
                            .find(|t| t.id == track_id)
                            .and_then(|t| t.clips.iter().find(|c| c.id == clip_id))
                            .map(|c| (c.volume, c.pan))
                            .unwrap_or((old_volume, old_pan))
                    };
                    on_execute_command(Box::new(crate::undo::SetClipVolumeCommand {
                        clip_id,
                        track_id,
                        old_volume,
                        new_volume,
                        old_pan,
                        new_pan,
                    }));
                }
            }
        };

        for slider in [&volume_slider, &pan_slider] {
            let ges = gtk4::GestureClick::new();
            let snap_c = do_vol_snapshot.clone();
            let commit_c = do_vol_commit.clone();
            ges.connect_pressed(move |_, _, _, _| { snap_c(); });
            ges.connect_released(move |_, _, _, _| { commit_c(); });
            slider.add_controller(ges);

            let focus_ctrl = gtk4::EventControllerFocus::new();
            let snap_c = do_vol_snapshot.clone();
            let commit_c = do_vol_commit.clone();
            focus_ctrl.connect_enter(move |_| { snap_c(); });
            focus_ctrl.connect_leave(move |_| { commit_c(); });
            slider.add_controller(focus_ctrl);
        }
    }

    // Wire Normalize button
    {
        let selected_clip_id = selected_clip_id.clone();
        let on_normalize_audio = on_normalize_audio.clone();
        normalize_btn.connect_clicked(move |_| {
            let id = selected_clip_id.borrow().clone();
            if let Some(ref clip_id) = id {
                on_normalize_audio(clip_id);
            }
        });
    }

    // Wire Channel mode dropdown
    {
        let project = project.clone();
        let selected_clip_id = selected_clip_id.clone();
        let updating = updating.clone();
        let on_clip_changed = on_clip_changed.clone();
        #[allow(deprecated)]
        channel_mode_dropdown.connect_changed(move |combo| {
            if *updating.borrow() { return; }
            let id = selected_clip_id.borrow().clone();
            #[allow(deprecated)]
            if let (Some(ref clip_id), Some(mode_id)) = (id, combo.active_id()) {
                {
                    let mut proj = project.borrow_mut();
                    for track in &mut proj.tracks {
                        if let Some(clip) = track.clips.iter_mut().find(|c| &c.id == clip_id) {
                            clip.audio_channel_mode =
                                crate::model::clip::AudioChannelMode::from_str(&mode_id);
                            proj.dirty = true;
                            break;
                        }
                    }
                }
                on_clip_changed();
            }
        });
    }

    // Wire Pitch controls
    {
        let project = project.clone();
        let selected_clip_id = selected_clip_id.clone();
        let updating = updating.clone();
        let on_clip_changed = on_clip_changed.clone();
        let pitch_preserve_check_cb = pitch_preserve_check.clone();
        pitch_shift_slider.connect_value_changed(move |s| {
            if *updating.borrow() { return; }
            let id = selected_clip_id.borrow().clone();
            if let Some(ref clip_id) = id {
                {
                    let mut proj = project.borrow_mut();
                    for track in &mut proj.tracks {
                        if let Some(clip) = track.clips.iter_mut().find(|c| &c.id == clip_id) {
                            clip.pitch_shift_semitones = s.value();
                            clip.pitch_preserve = pitch_preserve_check_cb.is_active();
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
        let on_clip_changed = on_clip_changed.clone();
        pitch_preserve_check.connect_toggled(move |btn| {
            if *updating.borrow() { return; }
            let id = selected_clip_id.borrow().clone();
            if let Some(ref clip_id) = id {
                {
                    let mut proj = project.borrow_mut();
                    for track in &mut proj.tracks {
                        if let Some(clip) = track.clips.iter_mut().find(|c| &c.id == clip_id) {
                            clip.pitch_preserve = btn.is_active();
                            proj.dirty = true;
                            break;
                        }
                    }
                }
                on_clip_changed();
            }
        });
    }

    // Wire Role dropdown
    {
        let selected_clip_id = selected_clip_id.clone();
        let updating = updating.clone();
        let on_role_changed = on_role_changed.clone();
        #[allow(deprecated)]
        role_dropdown.connect_changed(move |combo| {
            if *updating.borrow() { return; }
            let id = selected_clip_id.borrow().clone();
            #[allow(deprecated)]
            if let (Some(ref clip_id), Some(role_id)) = (id, combo.active_id()) {
                on_role_changed(clip_id, &role_id);
            }
        });
    }

    // Wire Duck controls
    {
        let selected_clip_id = selected_clip_id.clone();
        let updating = updating.clone();
        let on_duck_changed = on_duck_changed.clone();
        let duck_amount_slider_cb = duck_amount_slider.clone();
        duck_check.connect_toggled(move |btn| {
            if *updating.borrow() { return; }
            let id = selected_clip_id.borrow().clone();
            if let Some(ref clip_id) = id {
                on_duck_changed(clip_id, btn.is_active(), duck_amount_slider_cb.value());
            }
        });
    }
    {
        let selected_clip_id = selected_clip_id.clone();
        let updating = updating.clone();
        let on_duck_changed = on_duck_changed.clone();
        let duck_check_cb = duck_check.clone();
        duck_amount_slider.connect_value_changed(move |s| {
            if *updating.borrow() { return; }
            let id = selected_clip_id.borrow().clone();
            if let Some(ref clip_id) = id {
                on_duck_changed(clip_id, duck_check_cb.is_active(), s.value());
            }
        });
    }

    // Wire EQ sliders — one handler per slider, reads all 9 values and fires on_eq_changed.
    for bi in 0..3usize {
        // Wire freq slider
        {
            let project = project.clone();
            let selected_clip_id = selected_clip_id.clone();
            let updating = updating.clone();
            let on_eq_changed = on_eq_changed.clone();
            let fs: Vec<Scale> = eq_freq_sliders.iter().cloned().collect();
            let gs: Vec<Scale> = eq_gain_sliders.iter().cloned().collect();
            let qs: Vec<Scale> = eq_q_sliders.iter().cloned().collect();
            eq_freq_sliders[bi].connect_value_changed(move |_| {
                if *updating.borrow() { return; }
                let id = selected_clip_id.borrow().clone();
                if let Some(ref clip_id) = id {
                    let bands = [
                        crate::model::clip::EqBand { freq: fs[0].value(), gain: gs[0].value(), q: qs[0].value() },
                        crate::model::clip::EqBand { freq: fs[1].value(), gain: gs[1].value(), q: qs[1].value() },
                        crate::model::clip::EqBand { freq: fs[2].value(), gain: gs[2].value(), q: qs[2].value() },
                    ];
                    { let mut proj = project.borrow_mut(); for track in &mut proj.tracks { if let Some(clip) = track.clips.iter_mut().find(|c| &c.id == clip_id) { clip.eq_bands = bands; proj.dirty = true; break; } } }
                    on_eq_changed(clip_id, bands);
                }
            });
        }
        // Wire gain slider
        {
            let project = project.clone();
            let selected_clip_id = selected_clip_id.clone();
            let updating = updating.clone();
            let on_eq_changed = on_eq_changed.clone();
            let fs: Vec<Scale> = eq_freq_sliders.iter().cloned().collect();
            let gs: Vec<Scale> = eq_gain_sliders.iter().cloned().collect();
            let qs: Vec<Scale> = eq_q_sliders.iter().cloned().collect();
            eq_gain_sliders[bi].connect_value_changed(move |_| {
                if *updating.borrow() { return; }
                let id = selected_clip_id.borrow().clone();
                if let Some(ref clip_id) = id {
                    let bands = [
                        crate::model::clip::EqBand { freq: fs[0].value(), gain: gs[0].value(), q: qs[0].value() },
                        crate::model::clip::EqBand { freq: fs[1].value(), gain: gs[1].value(), q: qs[1].value() },
                        crate::model::clip::EqBand { freq: fs[2].value(), gain: gs[2].value(), q: qs[2].value() },
                    ];
                    { let mut proj = project.borrow_mut(); for track in &mut proj.tracks { if let Some(clip) = track.clips.iter_mut().find(|c| &c.id == clip_id) { clip.eq_bands = bands; proj.dirty = true; break; } } }
                    on_eq_changed(clip_id, bands);
                }
            });
        }
        // Wire Q slider
        {
            let project = project.clone();
            let selected_clip_id = selected_clip_id.clone();
            let updating = updating.clone();
            let on_eq_changed = on_eq_changed.clone();
            let fs: Vec<Scale> = eq_freq_sliders.iter().cloned().collect();
            let gs: Vec<Scale> = eq_gain_sliders.iter().cloned().collect();
            let qs: Vec<Scale> = eq_q_sliders.iter().cloned().collect();
            eq_q_sliders[bi].connect_value_changed(move |_| {
                if *updating.borrow() { return; }
                let id = selected_clip_id.borrow().clone();
                if let Some(ref clip_id) = id {
                    let bands = [
                        crate::model::clip::EqBand { freq: fs[0].value(), gain: gs[0].value(), q: qs[0].value() },
                        crate::model::clip::EqBand { freq: fs[1].value(), gain: gs[1].value(), q: qs[1].value() },
                        crate::model::clip::EqBand { freq: fs[2].value(), gain: gs[2].value(), q: qs[2].value() },
                    ];
                    { let mut proj = project.borrow_mut(); for track in &mut proj.tracks { if let Some(clip) = track.clips.iter_mut().find(|c| &c.id == clip_id) { clip.eq_bands = bands; proj.dirty = true; break; } } }
                    on_eq_changed(clip_id, bands);
                }
            });
        }
    }

    // Undo controllers for EQ sliders (shared snapshot for all 9 sliders).
    {
        type EqSnapCell = Rc<RefCell<Option<(String, String, [crate::model::clip::EqBand; 3])>>>;
        let eq_snap: EqSnapCell = Rc::new(RefCell::new(None));

        let do_eq_snapshot = {
            let project = project.clone();
            let selected_clip_id = selected_clip_id.clone();
            let eq_snap = eq_snap.clone();
            move || {
                let cid = selected_clip_id.borrow().clone();
                if let Some(cid) = cid {
                    let proj = project.borrow();
                    for track in &proj.tracks {
                        if let Some(clip) = track.clips.iter().find(|c| c.id == cid) {
                            *eq_snap.borrow_mut() = Some((
                                cid.clone(),
                                track.id.clone(),
                                clip.eq_bands,
                            ));
                            break;
                        }
                    }
                }
            }
        };

        let do_eq_commit = {
            let project = project.clone();
            let eq_snap = eq_snap.clone();
            let on_execute_command = on_execute_command.clone();
            move || {
                let entry = eq_snap.borrow_mut().take();
                if let Some((clip_id, track_id, old_eq_bands)) = entry {
                    let new_eq_bands = {
                        let proj = project.borrow();
                        proj.tracks.iter()
                            .find(|t| t.id == track_id)
                            .and_then(|t| t.clips.iter().find(|c| c.id == clip_id))
                            .map(|c| c.eq_bands)
                            .unwrap_or(old_eq_bands)
                    };
                    on_execute_command(Box::new(crate::undo::SetClipEqCommand {
                        clip_id,
                        track_id,
                        old_eq_bands,
                        new_eq_bands,
                    }));
                }
            }
        };

        let all_eq: Vec<Scale> = eq_freq_sliders.iter()
            .chain(eq_gain_sliders.iter())
            .chain(eq_q_sliders.iter())
            .cloned()
            .collect();

        for s in &all_eq {
            let ges = gtk4::GestureClick::new();
            let snap_c = do_eq_snapshot.clone();
            let commit_c = do_eq_commit.clone();
            ges.connect_pressed(move |_, _, _, _| { snap_c(); });
            ges.connect_released(move |_, _, _, _| { commit_c(); });
            s.add_controller(ges);

            let focus_ctrl = gtk4::EventControllerFocus::new();
            let snap_c = do_eq_snapshot.clone();
            let commit_c = do_eq_commit.clone();
            focus_ctrl.connect_enter(move |_| { snap_c(); });
            focus_ctrl.connect_leave(move |_| { commit_c(); });
            s.add_controller(focus_ctrl);
        }
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
                    proj.tracks
                        .iter()
                        .flat_map(|t| t.clips.iter())
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
                    proj.tracks
                        .iter()
                        .flat_map(|t| t.clips.iter())
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
                    proj.tracks
                        .iter()
                        .flat_map(|t| t.clips.iter())
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
                    proj.tracks
                        .iter()
                        .flat_map(|t| t.clips.iter())
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
        move || match interp_dropdown.selected() {
            1 => KeyframeInterpolation::EaseIn,
            2 => KeyframeInterpolation::EaseOut,
            3 => KeyframeInterpolation::EaseInOut,
            _ => KeyframeInterpolation::Linear,
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
    connect_phase1_keyframe_buttons(
        &pan_set_keyframe_btn,
        &pan_remove_keyframe_btn,
        Phase1KeyframeProperty::Pan,
        project.clone(),
        selected_clip_id.clone(),
        updating.clone(),
        current_playhead_ns.clone(),
        on_clip_changed.clone(),
        Rc::new({
            let pan_slider = pan_slider.clone();
            move || pan_slider.value().clamp(-1.0, 1.0)
        }),
        interp_provider.clone(),
    );
    connect_phase1_keyframe_buttons(
        &crop_left_set_keyframe_btn,
        &crop_left_remove_keyframe_btn,
        Phase1KeyframeProperty::CropLeft,
        project.clone(),
        selected_clip_id.clone(),
        updating.clone(),
        current_playhead_ns.clone(),
        on_clip_changed.clone(),
        Rc::new({
            let crop_left_slider = crop_left_slider.clone();
            move || crop_left_slider.value().clamp(0.0, 500.0).round()
        }),
        interp_provider.clone(),
    );
    connect_phase1_keyframe_buttons(
        &crop_right_set_keyframe_btn,
        &crop_right_remove_keyframe_btn,
        Phase1KeyframeProperty::CropRight,
        project.clone(),
        selected_clip_id.clone(),
        updating.clone(),
        current_playhead_ns.clone(),
        on_clip_changed.clone(),
        Rc::new({
            let crop_right_slider = crop_right_slider.clone();
            move || crop_right_slider.value().clamp(0.0, 500.0).round()
        }),
        interp_provider.clone(),
    );
    connect_phase1_keyframe_buttons(
        &crop_top_set_keyframe_btn,
        &crop_top_remove_keyframe_btn,
        Phase1KeyframeProperty::CropTop,
        project.clone(),
        selected_clip_id.clone(),
        updating.clone(),
        current_playhead_ns.clone(),
        on_clip_changed.clone(),
        Rc::new({
            let crop_top_slider = crop_top_slider.clone();
            move || crop_top_slider.value().clamp(0.0, 500.0).round()
        }),
        interp_provider.clone(),
    );
    connect_phase1_keyframe_buttons(
        &crop_bottom_set_keyframe_btn,
        &crop_bottom_remove_keyframe_btn,
        Phase1KeyframeProperty::CropBottom,
        project.clone(),
        selected_clip_id.clone(),
        updating.clone(),
        current_playhead_ns.clone(),
        on_clip_changed.clone(),
        Rc::new({
            let crop_bottom_slider = crop_bottom_slider.clone();
            move || crop_bottom_slider.value().clamp(0.0, 500.0).round()
        }),
        interp_provider.clone(),
    );
    connect_phase1_keyframe_buttons(
        &rotate_set_keyframe_btn,
        &rotate_remove_keyframe_btn,
        Phase1KeyframeProperty::Rotate,
        project.clone(),
        selected_clip_id.clone(),
        updating.clone(),
        current_playhead_ns.clone(),
        on_clip_changed.clone(),
        Rc::new({
            let rotate_spin = rotate_spin.clone();
            move || rotate_spin.value().clamp(-180.0, 180.0).round()
        }),
        interp_provider.clone(),
    );
    // Speed keyframe buttons use a lightweight path that updates the
    // ProgramClip's speed data in-place without a full pipeline rebuild,
    // avoiding GStreamer qtdemux race conditions during rapid edits.
    {
        let project = project.clone();
        let selected_clip_id = selected_clip_id.clone();
        let updating = updating.clone();
        let current_playhead_ns = current_playhead_ns.clone();
        let on_speed_keyframe_changed = on_speed_keyframe_changed.clone();
        let speed_slider = speed_slider.clone();
        let interp_provider = interp_provider.clone();
        speed_set_keyframe_btn.connect_clicked(move |_| {
            if *updating.borrow() {
                return;
            }
            let Some(clip_id) = selected_clip_id.borrow().clone() else {
                return;
            };
            let timeline_pos_ns = current_playhead_ns();
            let value = speed_slider.value().clamp(0.25, 4.0);
            let interp = interp_provider();
            let result = {
                let mut proj = project.borrow_mut();
                let mut found = None;
                for track in &mut proj.tracks {
                    if let Some(clip) = track.clips.iter_mut().find(|c| c.id == clip_id) {
                        clip.upsert_phase1_keyframe_at_timeline_ns_with_interp(
                            Phase1KeyframeProperty::Speed,
                            timeline_pos_ns,
                            value,
                            interp,
                        );
                        found = Some((clip.speed, clip.speed_keyframes.clone()));
                        break;
                    }
                }
                if found.is_some() {
                    proj.dirty = true;
                }
                found
            };
            if let Some((speed, kfs)) = result {
                on_speed_keyframe_changed(&clip_id, speed, &kfs);
            }
        });
    }
    {
        let project = project.clone();
        let selected_clip_id = selected_clip_id.clone();
        let updating = updating.clone();
        let current_playhead_ns = current_playhead_ns.clone();
        let on_speed_keyframe_changed = on_speed_keyframe_changed.clone();
        speed_remove_keyframe_btn.connect_clicked(move |_| {
            if *updating.borrow() {
                return;
            }
            let Some(clip_id) = selected_clip_id.borrow().clone() else {
                return;
            };
            let timeline_pos_ns = current_playhead_ns();
            let result = {
                let mut proj = project.borrow_mut();
                let mut found = None;
                for track in &mut proj.tracks {
                    if let Some(clip) = track.clips.iter_mut().find(|c| c.id == clip_id) {
                        let removed = clip.remove_phase1_keyframe_at_timeline_ns(
                            Phase1KeyframeProperty::Speed,
                            timeline_pos_ns,
                        );
                        if removed {
                            found = Some((clip.speed, clip.speed_keyframes.clone()));
                        }
                        break;
                    }
                }
                if found.is_some() {
                    proj.dirty = true;
                }
                found
            };
            if let Some((speed, kfs)) = result {
                on_speed_keyframe_changed(&clip_id, speed, &kfs);
            }
        });
    }

    // ── Keyframe navigation button wiring ──
    prev_keyframe_btn.connect_clicked({
        let project = project.clone();
        let selected_clip_id = selected_clip_id.clone();
        let current_playhead_ns = current_playhead_ns.clone();
        let on_seek_to = on_seek_to.clone();
        move |_| {
            let Some(clip_id) = selected_clip_id.borrow().clone() else {
                return;
            };
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
            let Some(clip_id) = selected_clip_id.borrow().clone() else {
                return;
            };
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

    // ── Speed keyframe navigation button wiring ──
    // Nav buttons also sync the speed slider to the keyframe value at the
    // destination, so the user sees the correct speed for that keyframe.
    speed_prev_keyframe_btn.connect_clicked({
        let project = project.clone();
        let selected_clip_id = selected_clip_id.clone();
        let current_playhead_ns = current_playhead_ns.clone();
        let on_seek_to = on_seek_to.clone();
        let speed_slider = speed_slider.clone();
        let updating = updating.clone();
        move |_| {
            let Some(clip_id) = selected_clip_id.borrow().clone() else {
                return;
            };
            let playhead = current_playhead_ns();
            let proj = project.borrow();
            for track in &proj.tracks {
                if let Some(clip) = track.clips.iter().find(|c| c.id == clip_id) {
                    let local = clip.local_timeline_position_ns(playhead);
                    if let Some(prev_local) =
                        clip.prev_keyframe_local_ns_for_property(Phase1KeyframeProperty::Speed, local)
                    {
                        let timeline_ns = clip.timeline_start.saturating_add(prev_local);
                        let speed_at_kf = clip.speed_at_local_timeline_ns(prev_local);
                        drop(proj);
                        *updating.borrow_mut() = true;
                        speed_slider.set_value(speed_at_kf);
                        *updating.borrow_mut() = false;
                        on_seek_to(timeline_ns);
                    }
                    break;
                }
            }
        }
    });
    speed_next_keyframe_btn.connect_clicked({
        let project = project.clone();
        let selected_clip_id = selected_clip_id.clone();
        let current_playhead_ns = current_playhead_ns.clone();
        let on_seek_to = on_seek_to.clone();
        let speed_slider = speed_slider.clone();
        let updating = updating.clone();
        move |_| {
            let Some(clip_id) = selected_clip_id.borrow().clone() else {
                return;
            };
            let playhead = current_playhead_ns();
            let proj = project.borrow();
            for track in &proj.tracks {
                if let Some(clip) = track.clips.iter().find(|c| c.id == clip_id) {
                    let local = clip.local_timeline_position_ns(playhead);
                    if let Some(next_local) =
                        clip.next_keyframe_local_ns_for_property(Phase1KeyframeProperty::Speed, local)
                    {
                        let timeline_ns = clip.timeline_start.saturating_add(next_local);
                        let speed_at_kf = clip.speed_at_local_timeline_ns(next_local);
                        drop(proj);
                        *updating.borrow_mut() = true;
                        speed_slider.set_value(speed_at_kf);
                        *updating.borrow_mut() = false;
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
            let Some(clip_id) = selected_clip_id.borrow().clone() else {
                return;
            };
            let playhead = current_playhead_ns();
            let proj = project.borrow();
            for track in &proj.tracks {
                if let Some(clip) = track.clips.iter().find(|c| c.id == clip_id) {
                    let local = clip.local_timeline_position_ns(playhead);
                    let prev_volume = clip
                        .prev_keyframe_local_ns_for_property(Phase1KeyframeProperty::Volume, local);
                    let prev_pan = clip
                        .prev_keyframe_local_ns_for_property(Phase1KeyframeProperty::Pan, local);
                    let prev_local = match (prev_volume, prev_pan) {
                        (Some(a), Some(b)) => Some(a.max(b)),
                        (Some(a), None) => Some(a),
                        (None, Some(b)) => Some(b),
                        (None, None) => None,
                    };
                    if let Some(prev_local) = prev_local {
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
            let Some(clip_id) = selected_clip_id.borrow().clone() else {
                return;
            };
            let playhead = current_playhead_ns();
            let proj = project.borrow();
            for track in &proj.tracks {
                if let Some(clip) = track.clips.iter().find(|c| c.id == clip_id) {
                    let local = clip.local_timeline_position_ns(playhead);
                    let next_volume = clip
                        .next_keyframe_local_ns_for_property(Phase1KeyframeProperty::Volume, local);
                    let next_pan = clip
                        .next_keyframe_local_ns_for_property(Phase1KeyframeProperty::Pan, local);
                    let next_local = match (next_volume, next_pan) {
                        (Some(a), Some(b)) => Some(a.min(b)),
                        (Some(a), None) => Some(a),
                        (None, Some(b)) => Some(b),
                        (None, None) => None,
                    };
                    if let Some(next_local) = next_local {
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

    // Title font button — opens a FontDialog, updates clip model + live preview
    {
        let project = project.clone();
        let selected_clip_id = selected_clip_id.clone();
        let updating = updating.clone();
        let on_title_style_changed = on_title_style_changed.clone();
        let on_title_changed = on_title_changed.clone();
        let title_x = title_x_slider.clone();
        let title_y = title_y_slider.clone();
        let title_entry_font = title_entry.clone();
        let font_btn = title_font_btn.clone();
        title_font_btn.connect_clicked(move |_btn| {
            if *updating.borrow() {
                return;
            }
            let dialog = gtk4::FontDialog::new();
            let current_label = font_btn.label().map(|l| l.to_string()).unwrap_or_default();
            let initial = if current_label.is_empty() {
                None
            } else {
                pango::FontDescription::from_string(&current_label).into()
            };
            let project_c = project.clone();
            let id_c = selected_clip_id.clone();
            let on_style = on_title_style_changed.clone();
            let on_title = on_title_changed.clone();
            let tx = title_x.clone();
            let ty = title_y.clone();
            let te = title_entry_font.clone();
            let fb = font_btn.clone();
            let parent: Option<&gtk4::Window> = None;
            dialog.choose_font(parent, initial.as_ref(), None::<&gio::Cancellable>, move |result| {
                if let Ok(desc) = result {
                    let font_str = desc.to_string();
                    fb.set_label(&font_str);
                    let id = id_c.borrow().clone();
                    if let Some(ref clip_id) = id {
                        {
                            let mut proj = project_c.borrow_mut();
                            for track in &mut proj.tracks {
                                if let Some(clip) = track.clips.iter_mut().find(|c| &c.id == clip_id) {
                                    clip.title_font = font_str.clone();
                                    proj.dirty = true;
                                    break;
                                }
                            }
                        }
                        on_title(te.text().to_string(), tx.value(), ty.value());
                        on_style();
                    }
                }
            });
        });
    }

    // Title text color button
    {
        let project = project.clone();
        let selected_clip_id = selected_clip_id.clone();
        let updating = updating.clone();
        let on_title_changed = on_title_changed.clone();
        let title_x = title_x_slider.clone();
        let title_y = title_y_slider.clone();
        let title_entry_c = title_entry.clone();
        title_color_btn.connect_rgba_notify(move |btn| {
            if *updating.borrow() {
                return;
            }
            let rgba = btn.rgba();
            let r = (rgba.red() * 255.0).round() as u32;
            let g = (rgba.green() * 255.0).round() as u32;
            let b = (rgba.blue() * 255.0).round() as u32;
            let a = (rgba.alpha() * 255.0).round() as u32;
            let color = (r << 24) | (g << 16) | (b << 8) | a;
            let id = selected_clip_id.borrow().clone();
            if let Some(ref clip_id) = id {
                {
                    let mut proj = project.borrow_mut();
                    for track in &mut proj.tracks {
                        if let Some(clip) = track.clips.iter_mut().find(|c| &c.id == clip_id) {
                            clip.title_color = color;
                            proj.dirty = true;
                            break;
                        }
                    }
                }
                on_title_changed(title_entry_c.text().to_string(), title_x.value(), title_y.value());
            }
        });
    }

    // Title outline width slider
    {
        let project = project.clone();
        let selected_clip_id = selected_clip_id.clone();
        let updating = updating.clone();
        let on_title_style_changed = on_title_style_changed.clone();
        title_outline_width_slider.connect_value_changed(move |sl| {
            if *updating.borrow() {
                return;
            }
            let val = sl.value();
            let id = selected_clip_id.borrow().clone();
            if let Some(ref clip_id) = id {
                {
                    let mut proj = project.borrow_mut();
                    for track in &mut proj.tracks {
                        if let Some(clip) = track.clips.iter_mut().find(|c| &c.id == clip_id) {
                            clip.title_outline_width = val;
                            proj.dirty = true;
                            break;
                        }
                    }
                }
                on_title_style_changed();
            }
        });
    }

    // Title outline color button
    {
        let project = project.clone();
        let selected_clip_id = selected_clip_id.clone();
        let updating = updating.clone();
        let on_title_style_changed = on_title_style_changed.clone();
        title_outline_color_btn.connect_rgba_notify(move |btn| {
            if *updating.borrow() {
                return;
            }
            let rgba = btn.rgba();
            let r = (rgba.red() * 255.0).round() as u32;
            let g = (rgba.green() * 255.0).round() as u32;
            let b = (rgba.blue() * 255.0).round() as u32;
            let a = (rgba.alpha() * 255.0).round() as u32;
            let color = (r << 24) | (g << 16) | (b << 8) | a;
            let id = selected_clip_id.borrow().clone();
            if let Some(ref clip_id) = id {
                {
                    let mut proj = project.borrow_mut();
                    for track in &mut proj.tracks {
                        if let Some(clip) = track.clips.iter_mut().find(|c| &c.id == clip_id) {
                            clip.title_outline_color = color;
                            proj.dirty = true;
                            break;
                        }
                    }
                }
                on_title_style_changed();
            }
        });
    }

    // Title shadow toggle
    {
        let project = project.clone();
        let selected_clip_id = selected_clip_id.clone();
        let updating = updating.clone();
        let on_title_style_changed = on_title_style_changed.clone();
        title_shadow_check.connect_toggled(move |btn| {
            if *updating.borrow() {
                return;
            }
            let active = btn.is_active();
            let id = selected_clip_id.borrow().clone();
            if let Some(ref clip_id) = id {
                {
                    let mut proj = project.borrow_mut();
                    for track in &mut proj.tracks {
                        if let Some(clip) = track.clips.iter_mut().find(|c| &c.id == clip_id) {
                            clip.title_shadow = active;
                            proj.dirty = true;
                            break;
                        }
                    }
                }
                on_title_style_changed();
            }
        });
    }

    // Title shadow color button
    {
        let project = project.clone();
        let selected_clip_id = selected_clip_id.clone();
        let updating = updating.clone();
        let on_title_style_changed = on_title_style_changed.clone();
        title_shadow_color_btn.connect_rgba_notify(move |btn| {
            if *updating.borrow() {
                return;
            }
            let rgba = btn.rgba();
            let r = (rgba.red() * 255.0).round() as u32;
            let g = (rgba.green() * 255.0).round() as u32;
            let b = (rgba.blue() * 255.0).round() as u32;
            let a = (rgba.alpha() * 255.0).round() as u32;
            let color = (r << 24) | (g << 16) | (b << 8) | a;
            let id = selected_clip_id.borrow().clone();
            if let Some(ref clip_id) = id {
                {
                    let mut proj = project.borrow_mut();
                    for track in &mut proj.tracks {
                        if let Some(clip) = track.clips.iter_mut().find(|c| &c.id == clip_id) {
                            clip.title_shadow_color = color;
                            proj.dirty = true;
                            break;
                        }
                    }
                }
                on_title_style_changed();
            }
        });
    }

    // Title shadow offset X slider
    {
        let project = project.clone();
        let selected_clip_id = selected_clip_id.clone();
        let updating = updating.clone();
        let on_title_style_changed = on_title_style_changed.clone();
        title_shadow_x_slider.connect_value_changed(move |sl| {
            if *updating.borrow() {
                return;
            }
            let val = sl.value();
            let id = selected_clip_id.borrow().clone();
            if let Some(ref clip_id) = id {
                {
                    let mut proj = project.borrow_mut();
                    for track in &mut proj.tracks {
                        if let Some(clip) = track.clips.iter_mut().find(|c| &c.id == clip_id) {
                            clip.title_shadow_offset_x = val;
                            proj.dirty = true;
                            break;
                        }
                    }
                }
                on_title_style_changed();
            }
        });
    }

    // Title shadow offset Y slider
    {
        let project = project.clone();
        let selected_clip_id = selected_clip_id.clone();
        let updating = updating.clone();
        let on_title_style_changed = on_title_style_changed.clone();
        title_shadow_y_slider.connect_value_changed(move |sl| {
            if *updating.borrow() {
                return;
            }
            let val = sl.value();
            let id = selected_clip_id.borrow().clone();
            if let Some(ref clip_id) = id {
                {
                    let mut proj = project.borrow_mut();
                    for track in &mut proj.tracks {
                        if let Some(clip) = track.clips.iter_mut().find(|c| &c.id == clip_id) {
                            clip.title_shadow_offset_y = val;
                            proj.dirty = true;
                            break;
                        }
                    }
                }
                on_title_style_changed();
            }
        });
    }

    // Title background box toggle
    {
        let project = project.clone();
        let selected_clip_id = selected_clip_id.clone();
        let updating = updating.clone();
        let on_title_style_changed = on_title_style_changed.clone();
        title_bg_box_check.connect_toggled(move |btn| {
            if *updating.borrow() {
                return;
            }
            let active = btn.is_active();
            let id = selected_clip_id.borrow().clone();
            if let Some(ref clip_id) = id {
                {
                    let mut proj = project.borrow_mut();
                    for track in &mut proj.tracks {
                        if let Some(clip) = track.clips.iter_mut().find(|c| &c.id == clip_id) {
                            clip.title_bg_box = active;
                            proj.dirty = true;
                            break;
                        }
                    }
                }
                on_title_style_changed();
            }
        });
    }

    // Title background box color button
    {
        let project = project.clone();
        let selected_clip_id = selected_clip_id.clone();
        let updating = updating.clone();
        let on_title_style_changed = on_title_style_changed.clone();
        title_bg_box_color_btn.connect_rgba_notify(move |btn| {
            if *updating.borrow() {
                return;
            }
            let rgba = btn.rgba();
            let r = (rgba.red() * 255.0).round() as u32;
            let g = (rgba.green() * 255.0).round() as u32;
            let b = (rgba.blue() * 255.0).round() as u32;
            let a = (rgba.alpha() * 255.0).round() as u32;
            let color = (r << 24) | (g << 16) | (b << 8) | a;
            let id = selected_clip_id.borrow().clone();
            if let Some(ref clip_id) = id {
                {
                    let mut proj = project.borrow_mut();
                    for track in &mut proj.tracks {
                        if let Some(clip) = track.clips.iter_mut().find(|c| &c.id == clip_id) {
                            clip.title_bg_box_color = color;
                            proj.dirty = true;
                            break;
                        }
                    }
                }
                on_title_style_changed();
            }
        });
    }

    // Title background box padding slider
    {
        let project = project.clone();
        let selected_clip_id = selected_clip_id.clone();
        let updating = updating.clone();
        let on_title_style_changed = on_title_style_changed.clone();
        title_bg_box_padding_slider.connect_value_changed(move |sl| {
            if *updating.borrow() {
                return;
            }
            let val = sl.value();
            let id = selected_clip_id.borrow().clone();
            if let Some(ref clip_id) = id {
                {
                    let mut proj = project.borrow_mut();
                    for track in &mut proj.tracks {
                        if let Some(clip) = track.clips.iter_mut().find(|c| &c.id == clip_id) {
                            clip.title_bg_box_padding = val;
                            proj.dirty = true;
                            break;
                        }
                    }
                }
                on_title_style_changed();
            }
        });
    }

    // Speed slider — when speed keyframes are present, live-update the
    // nearest keyframe at the playhead. Without keyframes, update clip.speed.
    // Uses a pending-update pattern to avoid re-entrancy panics: the model
    // is updated immediately, but the ProgramPlayer sync is deferred.
    {
        let project = project.clone();
        let selected_clip_id = selected_clip_id.clone();
        let updating = updating.clone();
        let on_speed_changed = on_speed_changed.clone();
        let on_speed_keyframe_changed = on_speed_keyframe_changed.clone();
        let current_playhead_ns = current_playhead_ns.clone();
        let speed_kf_pending = Rc::new(Cell::new(false));
        speed_slider.connect_value_changed(move |sl| {
            if *updating.borrow() {
                return;
            }
            let speed = sl.value();
            if let Some(ref id) = *selected_clip_id.borrow() {
                // When speed keyframes exist, the slider is just a value
                // picker — the user clicks "Set Speed Keyframe" to apply.
                // This avoids all the re-entrancy and pipeline crash issues
                // that come from live-updating keyframes during drags.
                let has_keyframes = {
                    let proj = project.borrow();
                    proj.tracks
                        .iter()
                        .flat_map(|t| t.clips.iter())
                        .find(|c| c.id == *id)
                        .map(|c| !c.speed_keyframes.is_empty())
                        .unwrap_or(false)
                };
                if has_keyframes {
                    // Show tooltip hint on the slider.
                    sl.set_tooltip_text(Some("Click \"Set Speed Keyframe\" to apply"));
                    return;
                }
                // No keyframes: update base speed directly.
                sl.set_tooltip_text(None);
                let changed = {
                    let mut proj = project.borrow_mut();
                    let mut changed = false;
                    for track in &mut proj.tracks {
                        if let Some(clip) = track.clips.iter_mut().find(|c| c.id == *id) {
                            clip.speed = speed;
                            proj.dirty = true;
                            changed = true;
                            break;
                        }
                    }
                    changed
                };
                if changed {
                    // Defer the project reload to avoid re-entrancy:
                    // on_speed_changed → on_project_changed borrows the
                    // project; if GTK processes another scroll event
                    // during that borrow, the handler's borrow_mut panics.
                    if !speed_kf_pending.get() {
                        speed_kf_pending.set(true);
                        let cb = on_speed_changed.clone();
                        let pending = speed_kf_pending.clone();
                        glib::idle_add_local_once(move || {
                            pending.set(false);
                            cb(speed);
                        });
                    }
                }
            }
        });
    }

    // Undo controller for speed slider.
    {
        type SpeedSnapCell = Rc<RefCell<Option<(String, String, f64)>>>;
        let speed_snap: SpeedSnapCell = Rc::new(RefCell::new(None));

        let do_speed_snapshot = {
            let project = project.clone();
            let selected_clip_id = selected_clip_id.clone();
            let speed_snap = speed_snap.clone();
            move || {
                let cid = selected_clip_id.borrow().clone();
                if let Some(cid) = cid {
                    let proj = project.borrow();
                    for track in &proj.tracks {
                        if let Some(clip) = track.clips.iter().find(|c| c.id == cid) {
                            *speed_snap.borrow_mut() = Some((
                                cid.clone(),
                                track.id.clone(),
                                clip.speed,
                            ));
                            break;
                        }
                    }
                }
            }
        };

        let do_speed_commit = {
            let project = project.clone();
            let speed_snap = speed_snap.clone();
            let on_execute_command = on_execute_command.clone();
            move || {
                let entry = speed_snap.borrow_mut().take();
                if let Some((clip_id, track_id, old_speed)) = entry {
                    let new_speed = {
                        let proj = project.borrow();
                        proj.tracks.iter()
                            .find(|t| t.id == track_id)
                            .and_then(|t| t.clips.iter().find(|c| c.id == clip_id))
                            .map(|c| c.speed)
                            .unwrap_or(old_speed)
                    };
                    if (new_speed - old_speed).abs() > 1e-9 {
                        on_execute_command(Box::new(crate::undo::SetClipSpeedCommand {
                            clip_id,
                            track_id,
                            old_speed,
                            new_speed,
                        }));
                    }
                }
            }
        };

        let ges = gtk4::GestureClick::new();
        {
            let do_speed_snapshot = do_speed_snapshot.clone();
            ges.connect_pressed(move |_, _, _, _| { do_speed_snapshot(); });
        }
        {
            let do_speed_commit = do_speed_commit.clone();
            ges.connect_released(move |_, _, _, _| { do_speed_commit(); });
        }
        speed_slider.add_controller(ges);

        let focus_ctrl = gtk4::EventControllerFocus::new();
        {
            let do_speed_snapshot = do_speed_snapshot.clone();
            focus_ctrl.connect_enter(move |_| { do_speed_snapshot(); });
        }
        {
            focus_ctrl.connect_leave(move |_| { do_speed_commit(); });
        }
        speed_slider.add_controller(focus_ctrl);
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

    // Slow-motion interpolation dropdown
    {
        let project = project.clone();
        let selected_clip_id = selected_clip_id.clone();
        let updating = updating.clone();
        let on_clip_changed = on_clip_changed.clone();
        slow_motion_dropdown.connect_selected_notify(move |dd| {
            if *updating.borrow() {
                return;
            }
            let interp = match dd.selected() {
                1 => crate::model::clip::SlowMotionInterp::Blend,
                2 => crate::model::clip::SlowMotionInterp::OpticalFlow,
                _ => crate::model::clip::SlowMotionInterp::Off,
            };
            if let Some(ref id) = *selected_clip_id.borrow() {
                let mut proj = project.borrow_mut();
                let mut found = false;
                for track in &mut proj.tracks {
                    for clip in &mut track.clips {
                        if clip.id == *id {
                            clip.slow_motion_interp = interp;
                            found = true;
                        }
                    }
                }
                if found {
                    proj.dirty = true;
                }
            }
            on_clip_changed();
        });
    }

    // LUT add button
    {
        let project = project.clone();
        let selected_clip_id = selected_clip_id.clone();
        let on_lut_changed = on_lut_changed.clone();
        let lut_display_box = lut_display_box.clone();
        let lut_clear_btn = lut_clear_btn.clone();
        lut_import_btn.connect_clicked(move |btn| {
            let dialog = gtk4::FileDialog::new();
            dialog.set_title("Add LUT");
            let filter = gtk4::FileFilter::new();
            filter.add_pattern("*.cube");
            filter.set_name(Some("3D LUT Files (*.cube)"));
            let filters = gio::ListStore::new::<gtk4::FileFilter>();
            filters.append(&filter);
            dialog.set_filters(Some(&filters));

            let project = project.clone();
            let selected_clip_id = selected_clip_id.clone();
            let on_lut_changed = on_lut_changed.clone();
            let lut_display_box = lut_display_box.clone();
            let lut_clear_btn = lut_clear_btn.clone();
            let window = btn.root().and_then(|r| r.downcast::<gtk4::Window>().ok());

            dialog.open(window.as_ref(), gio::Cancellable::NONE, move |result| {
                if let Ok(file) = result {
                    if let Some(path) = file.path() {
                        let path_str = path.to_string_lossy().to_string();
                        let id = selected_clip_id.borrow().clone();
                        let mut count = 0usize;
                        if let Some(ref clip_id) = id {
                            let mut proj = project.borrow_mut();
                            for track in &mut proj.tracks {
                                if let Some(clip) =
                                    track.clips.iter_mut().find(|c| &c.id == clip_id)
                                {
                                    clip.lut_paths.push(path_str.clone());
                                    count = clip.lut_paths.len();
                                    proj.dirty = true;
                                    break;
                                }
                            }
                        }
                        // Rebuild display
                        while let Some(child) = lut_display_box.first_child() {
                            lut_display_box.remove(&child);
                        }
                        // Re-read clip paths
                        let lut_paths: Vec<String> = {
                            let id = selected_clip_id.borrow();
                            let proj = project.borrow();
                            if let Some(ref clip_id) = *id {
                                proj.tracks.iter()
                                    .flat_map(|t| t.clips.iter())
                                    .find(|c| &c.id == clip_id)
                                    .map(|c| c.lut_paths.clone())
                                    .unwrap_or_default()
                            } else { Vec::new() }
                        };
                        for (i, p) in lut_paths.iter().enumerate() {
                            let name = std::path::Path::new(p)
                                .file_name()
                                .and_then(|n| n.to_str())
                                .unwrap_or(p)
                                .to_string();
                            let label = Label::new(Some(&format!("{}. {}", i + 1, name)));
                            label.set_halign(gtk4::Align::Start);
                            label.set_ellipsize(gtk4::pango::EllipsizeMode::Start);
                            label.set_tooltip_text(Some(p.as_str()));
                            label.add_css_class("clip-path");
                            lut_display_box.append(&label);
                        }
                        lut_clear_btn.set_sensitive(count > 0);
                        on_lut_changed(Some(path_str));
                    }
                }
            });
        });
    }

    // LUT clear all button
    {
        let project = project.clone();
        let selected_clip_id = selected_clip_id.clone();
        let on_lut_changed = on_lut_changed.clone();
        let lut_display_box_clear = lut_display_box.clone();
        let lut_clear_btn_cb = lut_clear_btn.clone();
        lut_clear_btn.connect_clicked(move |_| {
            let id = selected_clip_id.borrow().clone();
            if let Some(ref clip_id) = id {
                let mut proj = project.borrow_mut();
                for track in &mut proj.tracks {
                    if let Some(clip) = track.clips.iter_mut().find(|c| &c.id == clip_id) {
                        clip.lut_paths.clear();
                        proj.dirty = true;
                        break;
                    }
                }
            }
            while let Some(child) = lut_display_box_clear.first_child() {
                lut_display_box_clear.remove(&child);
            }
            let none_label = Label::new(Some("None"));
            none_label.set_halign(gtk4::Align::Start);
            none_label.add_css_class("clip-path");
            lut_display_box_clear.append(&none_label);
            lut_clear_btn_cb.set_sensitive(false);
            on_lut_changed(None);
        });
    }

    // ── Match Color button ────────────────────────────────────────────────────
    {
        let project = project.clone();
        let selected_clip_id = selected_clip_id.clone();
        let on_color_changed = on_color_changed.clone();
        let updating = updating.clone();
        let brightness_slider = brightness_slider.clone();
        let contrast_slider = contrast_slider.clone();
        let saturation_slider = saturation_slider.clone();
        let temperature_slider = temperature_slider.clone();
        let tint_slider = tint_slider.clone();
        let exposure_slider = exposure_slider.clone();
        let black_point_slider = black_point_slider.clone();
        let shadows_slider = shadows_slider.clone();
        let midtones_slider = midtones_slider.clone();
        let highlights_slider = highlights_slider.clone();
        let highlights_warmth_slider = highlights_warmth_slider.clone();
        let highlights_tint_slider = highlights_tint_slider.clone();
        let midtones_warmth_slider = midtones_warmth_slider.clone();
        let midtones_tint_slider = midtones_tint_slider.clone();
        let shadows_warmth_slider = shadows_warmth_slider.clone();
        let shadows_tint_slider = shadows_tint_slider.clone();
        let denoise_slider = denoise_slider.clone();
        let sharpness_slider = sharpness_slider.clone();
        let blur_slider = blur_slider.clone();
        let on_lut_changed = on_lut_changed.clone();
        let lut_display_box = lut_display_box.clone();
        match_color_btn.connect_clicked(move |btn| {
            let source_id = selected_clip_id.borrow().clone();
            let Some(source_clip_id) = source_id else {
                return;
            };

            // Collect other video/image clips as reference candidates.
            let proj = project.borrow();
            let mut candidates: Vec<(String, String, String)> = Vec::new(); // (clip_id, label, track_id)
            for track in &proj.tracks {
                for clip in &track.clips {
                    if clip.id != source_clip_id
                        && (clip.kind == crate::model::clip::ClipKind::Video
                            || clip.kind == crate::model::clip::ClipKind::Image)
                    {
                        candidates.push((
                            clip.id.clone(),
                            clip.label.clone(),
                            track.id.clone(),
                        ));
                    }
                }
            }
            drop(proj);

            if candidates.is_empty() {
                return;
            }

            // Build a simple dialog with a dropdown of candidate clips.
            let window = btn.root().and_then(|r| r.downcast::<gtk4::Window>().ok());
            let dialog = gtk4::Window::builder()
                .title("Match Color — Select Reference Clip")
                .modal(true)
                .default_width(360)
                .default_height(160)
                .build();
            if let Some(ref w) = window {
                dialog.set_transient_for(Some(w));
            }

            let vbox = GBox::new(Orientation::Vertical, 12);
            vbox.set_margin_start(16);
            vbox.set_margin_end(16);
            vbox.set_margin_top(16);
            vbox.set_margin_bottom(16);
            dialog.set_child(Some(&vbox));

            let label = Label::new(Some("Select the reference clip to match:"));
            label.set_halign(gtk4::Align::Start);
            vbox.append(&label);

            let labels: Vec<String> = candidates.iter().map(|(_, l, _)| l.clone()).collect();
            let string_list = gtk4::StringList::new(&labels.iter().map(|s| s.as_str()).collect::<Vec<_>>());
            let dropdown = gtk4::DropDown::new(Some(string_list), gtk4::Expression::NONE);
            dropdown.set_selected(0);
            vbox.append(&dropdown);

            let lut_check = CheckButton::with_label("Also generate .cube LUT for fine matching");
            vbox.append(&lut_check);

            let btn_row = GBox::new(Orientation::Horizontal, 8);
            btn_row.set_halign(gtk4::Align::End);
            let cancel_btn = Button::with_label("Cancel");
            let ok_btn = Button::with_label("Match");
            ok_btn.add_css_class("suggested-action");
            btn_row.append(&cancel_btn);
            btn_row.append(&ok_btn);
            vbox.append(&btn_row);

            let dialog_cancel = dialog.clone();
            cancel_btn.connect_clicked(move |_| {
                dialog_cancel.close();
            });

            let dialog_ok = dialog.clone();
            let project = project.clone();
            let selected_clip_id = selected_clip_id.clone();
            let on_color_changed = on_color_changed.clone();
            let updating = updating.clone();
            let brightness_slider = brightness_slider.clone();
            let contrast_slider = contrast_slider.clone();
            let saturation_slider = saturation_slider.clone();
            let temperature_slider = temperature_slider.clone();
            let tint_slider = tint_slider.clone();
            let exposure_slider = exposure_slider.clone();
            let black_point_slider = black_point_slider.clone();
            let shadows_slider = shadows_slider.clone();
            let midtones_slider = midtones_slider.clone();
            let highlights_slider = highlights_slider.clone();
            let highlights_warmth_slider = highlights_warmth_slider.clone();
            let highlights_tint_slider = highlights_tint_slider.clone();
            let midtones_warmth_slider = midtones_warmth_slider.clone();
            let midtones_tint_slider = midtones_tint_slider.clone();
            let shadows_warmth_slider = shadows_warmth_slider.clone();
            let shadows_tint_slider = shadows_tint_slider.clone();
            let denoise_slider = denoise_slider.clone();
            let sharpness_slider = sharpness_slider.clone();
            let blur_slider = blur_slider.clone();
            let on_lut_changed = on_lut_changed.clone();
            let lut_display_box = lut_display_box.clone();
            ok_btn.connect_clicked(move |_| {
                let idx = dropdown.selected() as usize;
                if idx >= candidates.len() {
                    dialog_ok.close();
                    return;
                }
                let ref_clip_id = candidates[idx].0.clone();
                let gen_lut = lut_check.is_active();

                // Gather source and reference clip info.
                let clip_info = {
                    let proj = project.borrow();
                    let find = |id: &str| -> Option<(String, u64, u64, String)> {
                        for track in &proj.tracks {
                            if let Some(c) = track.clips.iter().find(|c| c.id == id) {
                                return Some((
                                    c.source_path.clone(),
                                    c.source_in,
                                    c.source_out,
                                    track.id.clone(),
                                ));
                            }
                        }
                        None
                    };
                    let ref_grading = proj.tracks.iter()
                        .flat_map(|t| t.clips.iter())
                        .find(|c| c.id == ref_clip_id)
                        .map(crate::media::color_match::ReferenceGrading::from_clip);
                    match (find(&source_clip_id), find(&ref_clip_id)) {
                        (Some(s), Some(r)) => Some((s, r, ref_grading)),
                        _ => None,
                    }
                };

                let Some((src, reff, ref_grading)) = clip_info else {
                    dialog_ok.close();
                    return;
                };

                let params = crate::media::color_match::MatchColorParams {
                    source_path: src.0,
                    source_in_ns: src.1,
                    source_out_ns: src.2,
                    reference_path: reff.0,
                    reference_in_ns: reff.1,
                    reference_out_ns: reff.2,
                    sample_count: 8,
                    generate_lut: gen_lut,
                    lut_output_dir: None,
                    reference_grading: ref_grading,
                };

                match crate::media::color_match::run_match_color(&params) {
                    Ok(outcome) => {
                        let r = &outcome.slider_result;
                        {
                            let mut proj = project.borrow_mut();
                            for track in &mut proj.tracks {
                                if let Some(clip) =
                                    track.clips.iter_mut().find(|c| c.id == source_clip_id)
                                {
                                    clip.brightness = r.brightness;
                                    clip.contrast = r.contrast;
                                    clip.saturation = r.saturation;
                                    clip.temperature = r.temperature;
                                    clip.tint = r.tint;
                                    clip.exposure = r.exposure;
                                    clip.black_point = r.black_point;
                                    clip.shadows = r.shadows;
                                    clip.midtones = r.midtones;
                                    clip.highlights = r.highlights;
                                    clip.highlights_warmth = r.highlights_warmth;
                                    clip.highlights_tint = r.highlights_tint;
                                    clip.midtones_warmth = r.midtones_warmth;
                                    clip.midtones_tint = r.midtones_tint;
                                    clip.shadows_warmth = r.shadows_warmth;
                                    clip.shadows_tint = r.shadows_tint;
                                    if let Some(ref lp) = outcome.lut_path {
                                        clip.lut_paths.push(lp.clone());
                                    }
                                    proj.dirty = true;
                                    break;
                                }
                            }
                        }

                        // Update sliders to reflect new values.
                        *updating.borrow_mut() = true;
                        brightness_slider.set_value(r.brightness as f64);
                        contrast_slider.set_value(r.contrast as f64);
                        saturation_slider.set_value(r.saturation as f64);
                        temperature_slider.set_value(r.temperature as f64);
                        tint_slider.set_value(r.tint as f64);
                        exposure_slider.set_value(r.exposure as f64);
                        black_point_slider.set_value(r.black_point as f64);
                        shadows_slider.set_value(r.shadows as f64);
                        midtones_slider.set_value(r.midtones as f64);
                        highlights_slider.set_value(r.highlights as f64);
                        highlights_warmth_slider.set_value(r.highlights_warmth as f64);
                        highlights_tint_slider.set_value(r.highlights_tint as f64);
                        midtones_warmth_slider.set_value(r.midtones_warmth as f64);
                        midtones_tint_slider.set_value(r.midtones_tint as f64);
                        shadows_warmth_slider.set_value(r.shadows_warmth as f64);
                        shadows_tint_slider.set_value(r.shadows_tint as f64);
                        *updating.borrow_mut() = false;

                        // Notify about color change.
                        on_color_changed(
                            r.brightness,
                            r.contrast,
                            r.saturation,
                            r.temperature,
                            r.tint,
                            denoise_slider.value() as f32,
                            sharpness_slider.value() as f32,
                            blur_slider.value() as f32,
                            r.shadows,
                            r.midtones,
                            r.highlights,
                            r.exposure,
                            r.black_point,
                            r.highlights_warmth,
                            r.highlights_tint,
                            r.midtones_warmth,
                            r.midtones_tint,
                            r.shadows_warmth,
                            r.shadows_tint,
                        );

                        // Update LUT label for generated or cleared LUT.
                        if let Some(ref lut_path) = outcome.lut_path {
                            // Re-read and rebuild display from current clip state
                            let lut_paths: Vec<String> = {
                                let proj = project.borrow();
                                proj.tracks.iter()
                                    .flat_map(|t| t.clips.iter())
                                    .find(|c| c.id == source_clip_id)
                                    .map(|c| c.lut_paths.clone())
                                    .unwrap_or_default()
                            };
                            while let Some(child) = lut_display_box.first_child() {
                                lut_display_box.remove(&child);
                            }
                            for (i, p) in lut_paths.iter().enumerate() {
                                let name = std::path::Path::new(p)
                                    .file_name()
                                    .and_then(|n| n.to_str())
                                    .unwrap_or(p)
                                    .to_string();
                                let label = Label::new(Some(&format!("{}. {}", i + 1, name)));
                                label.set_halign(gtk4::Align::Start);
                                label.set_ellipsize(gtk4::pango::EllipsizeMode::Start);
                                label.add_css_class("clip-path");
                                lut_display_box.append(&label);
                            }
                            on_lut_changed(Some(lut_path.clone()));
                        }

                        log::info!("color_match: applied to clip {source_clip_id}");
                    }
                    Err(e) => {
                        log::error!("color_match failed: {e}");
                    }
                }

                dialog_ok.close();
            });

            dialog.present();
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
        path_status_value,
        relink_btn,
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
        blur_slider,
        vidstab_check,
        vidstab_slider,
        shadows_slider,
        midtones_slider,
        highlights_slider,
        exposure_slider,
        black_point_slider,
        highlights_warmth_slider,
        highlights_tint_slider,
        midtones_warmth_slider,
        midtones_tint_slider,
        shadows_warmth_slider,
        shadows_tint_slider,
        volume_slider,
        pan_slider,
        normalize_btn,
        measured_loudness_label,
        ladspa_effects_list,
        channel_mode_dropdown,
        pitch_shift_slider,
        pitch_preserve_check,
        role_dropdown,
        duck_check,
        duck_amount_slider,
        eq_freq_sliders,
        eq_gain_sliders,
        eq_q_sliders,
        crop_left_slider,
        crop_right_slider,
        crop_top_slider,
        crop_bottom_slider,
        rotate_spin,
        flip_h_btn,
        flip_v_btn,
        scale_slider,
        opacity_slider,
        blend_mode_dropdown,
        anamorphic_desqueeze_dropdown,
        position_x_slider,
        position_y_slider,
        title_entry,
        title_x_slider,
        title_y_slider,
        title_font_btn,
        title_color_btn,
        title_outline_width_slider,
        title_outline_color_btn,
        title_shadow_check,
        title_shadow_color_btn,
        title_shadow_x_slider,
        title_shadow_y_slider,
        title_bg_box_check,
        title_bg_box_color_btn,
        title_bg_box_padding_slider,
        speed_slider,
        reverse_check,
        slow_motion_dropdown,
        lut_display_box,
        lut_clear_btn,
        match_color_btn,
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
        frei0r_effects_section,
        frei0r_effects_list,
        frei0r_effects_clipboard: frei0r_effects_clipboard.clone(),
        frei0r_paste_btn: frei0r_paste_btn.clone(),
        project,
        on_frei0r_changed,
        on_frei0r_params_changed,
        on_execute_command,
        frei0r_displayed_snapshot: Rc::new(RefCell::new(Vec::new())),
        frei0r_registry: Rc::new(RefCell::new(None)),
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
    -(deg.round().clamp(-180.0, 180.0) as i32)
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

/// Convert a frei0r plugin name like `"cartoon"` or `"color-distance"` to
/// `"Cartoon"` or `"Color Distance"`.
fn humanize_frei0r_name(name: &str) -> String {
    name.split(|c: char| c == '-' || c == '_')
        .map(|word| {
            let mut chars = word.chars();
            match chars.next() {
                None => String::new(),
                Some(first) => {
                    let upper: String = first.to_uppercase().collect();
                    upper + chars.as_str()
                }
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}
