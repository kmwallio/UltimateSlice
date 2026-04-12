use crate::model::clip::{
    ClipColorLabel, KeyframeInterpolation, NumericKeyframe, Phase1KeyframeProperty,
    SubtitleHighlightMode,
};
use crate::model::transition::{
    is_supported_transition_kind, max_transition_duration_ns, supported_transition_definitions,
    validate_track_transition_request, TransitionAlignment, DEFAULT_TRANSITION_DURATION_NS,
    MIN_TRANSITION_DURATION_NS,
};

// -----------------------------------------------------------------------
// Inspector slider range constants (P2.7)
// -----------------------------------------------------------------------
//
// Promotes the most-duplicated `Scale::with_range(…)` literal arguments
// to named constants. The "rule of three" applies — only ranges used 3+
// times or with clear semantic meaning are promoted. Single-use values
// (mask feather, vi pad, eq Q, etc.) are intentionally left inline so
// the magic-number list doesn't bloat.
//
// Existing transform-bounds constants from `model::transform_bounds`
// (`SCALE_MIN/MAX`, `POSITION_MIN/MAX`, `CROP_MIN_PX/MAX_PX`, `OPACITY_*`,
// `ROTATE_MIN_DEG/MAX_DEG`) cover the transform sliders and are not
// duplicated here.

/// Range for "color slider" inspector controls — symmetric ±1.0 with 0.01
/// step. Used by brightness, exposure, tint, sharpness, and the eight
/// grading sliders (shadows/midtones/highlights × value/warmth/tint).
const COLOR_SLIDER_MIN: f64 = -1.0;
const COLOR_SLIDER_MAX: f64 = 1.0;
const COLOR_SLIDER_STEP: f64 = 0.01;

/// Range for "unit slider" inspector controls — `[0.0, 1.0]` with 0.01 step.
/// Used by denoise, blur, vidstab smoothing, chroma key tolerance/softness,
/// background-removal threshold, mask center x/y, opacity, and title x/y.
const UNIT_SLIDER_MIN: f64 = 0.0;
const UNIT_SLIDER_MAX: f64 = 1.0;
const UNIT_SLIDER_STEP: f64 = 0.01;

/// Range for "double slider" inspector controls — `[0.0, 2.0]` with 0.01
/// step. Used by contrast and saturation (where `1.0` is the neutral
/// midpoint and `2.0` is doubled).
const DOUBLE_SLIDER_MIN: f64 = 0.0;
const DOUBLE_SLIDER_MAX: f64 = 2.0;
const DOUBLE_SLIDER_STEP: f64 = 0.01;

/// Range for the colour-temperature slider, in Kelvin. Spans tungsten to
/// daylight (2000 K to 10000 K) with 100 K steps; the neutral default is
/// 6500 K (D65).
const COLOR_TEMPERATURE_MIN_K: f64 = 2000.0;
const COLOR_TEMPERATURE_MAX_K: f64 = 10000.0;
const COLOR_TEMPERATURE_STEP_K: f64 = 100.0;

/// Clipboard for copy/paste subtitle style between clips.
#[derive(Clone)]
pub struct SubtitleStyleClipboard {
    pub font: String,
    pub color: u32,
    pub outline_color: u32,
    pub outline_width: f64,
    pub bg_box: bool,
    pub bg_box_color: u32,
    pub highlight_mode: SubtitleHighlightMode,
    pub highlight_color: u32,
    pub position_y: f64,
    pub word_window_secs: f64,
    pub subtitle_bold: bool,
    pub subtitle_italic: bool,
    pub subtitle_underline: bool,
    pub subtitle_shadow: bool,
    pub subtitle_shadow_color: u32,
    pub subtitle_shadow_offset_x: f64,
    pub subtitle_shadow_offset_y: f64,
    pub highlight_flags: crate::model::clip::SubtitleHighlightFlags,
    pub bg_highlight_color: u32,
    pub highlight_stroke_color: u32,
}
use crate::model::project::{FrameRate, MotionTrackerReference, Project};
use gdk4;
use gio;
use gtk4::prelude::*;

/// State for an in-flight SAM background inference job. Stored in
/// `InspectorView::sam_job_handle` and drained by a polling tick that
/// runs on the GTK main thread. The `clip_id` is captured at spawn
/// time so the result is applied to the same clip the user clicked
/// on, even if they switched the Inspector's selection during the
/// ~6 s inference window.
#[cfg(feature = "ai-inference")]
pub struct SamJobInFlight {
    pub handle: crate::media::sam_job::SamJobHandle,
    pub clip_id: String,
}
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

fn sync_title_font_button(button: &gtk4::Button, font_desc: &str) {
    let normalized = crate::media::title_font::normalize_title_font_label(font_desc);
    let tooltip =
        crate::media::title_font::build_title_font_tooltip(font_desc, "Click to choose a font");
    button.set_label(&normalized);
    button.set_tooltip_text(Some(&tooltip));
}

fn sync_subtitle_font_button(button: &gtk4::Button, font_desc: &str) {
    let normalized = crate::media::title_font::normalize_subtitle_font_label(font_desc);
    let tooltip = crate::media::title_font::build_subtitle_font_tooltip(
        font_desc,
        "Click to choose a subtitle font",
    );
    button.set_label(&normalized);
    button.set_tooltip_text(Some(&tooltip));
}

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

#[derive(Clone)]
struct MatchAudioCandidate {
    clip_id: String,
    label: String,
    duration_ns: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum MatchAudioDialogMode {
    MatchVoice,
    ChooseRegion,
}

impl MatchAudioDialogMode {
    fn from_index(index: u32) -> Self {
        match index {
            1 => Self::ChooseRegion,
            _ => Self::MatchVoice,
        }
    }

    fn description(self) -> &'static str {
        match self {
            Self::MatchVoice => {
                "Recommended: match the full trimmed clips and prioritize dialogue or voice automatically."
            }
            Self::ChooseRegion => {
                "Advanced: enter exact source/reference In/Out timecodes to match a specific phrase."
            }
        }
    }

    fn shows_region_fields(self) -> bool {
        matches!(self, Self::ChooseRegion)
    }
}

fn audio_match_clip_label(label: &str, clip_id: &str) -> String {
    let trimmed = label.trim();
    if trimmed.is_empty() {
        clip_id.to_string()
    } else {
        trimmed.to_string()
    }
}

fn clip_supports_tracking_analysis(clip: &crate::model::clip::Clip) -> Result<(), &'static str> {
    match clip.kind {
        crate::model::clip::ClipKind::Video => {}
        _ => {
            return Err("Tracking analysis currently requires a video clip with decodable frames.")
        }
    }
    if clip.source_path.trim().is_empty() {
        return Err("Tracking analysis is unavailable because this clip has no source media path.");
    }
    if (clip.speed - 1.0).abs() > f64::EPSILON || !clip.speed_keyframes.is_empty() {
        return Err("Tracking analysis currently requires an unretimed source clip.");
    }
    if clip.source_duration() == 0 {
        return Err("Tracking analysis needs a clip with visible source duration.");
    }
    Ok(())
}

fn tracking_reference_label(reference: &MotionTrackerReference) -> String {
    let clip_label = reference.clip_label.trim();
    let tracker_label = reference.tracker_label.trim();
    let base = match (clip_label.is_empty(), tracker_label.is_empty()) {
        (true, true) => reference.source_clip_id.clone(),
        (true, false) => tracker_label.to_string(),
        (false, true) => clip_label.to_string(),
        (false, false) => format!("{clip_label} — {tracker_label}"),
    };
    if !reference.enabled {
        format!("{base} (disabled)")
    } else if reference.sample_count == 0 {
        format!("{base} (run Track Region first)")
    } else {
        base
    }
}

fn set_audio_match_region_entries(
    start_entry: &Entry,
    end_entry: &Entry,
    region: crate::media::audio_match::AnalysisRegionNs,
    frame_rate: &FrameRate,
) {
    start_entry.set_text(&crate::ui::timecode::format_ns_as_timecode(
        region.start_ns,
        frame_rate,
    ));
    end_entry.set_text(&crate::ui::timecode::format_ns_as_timecode(
        region.end_ns,
        frame_rate,
    ));
}

fn parse_audio_match_region_entries(
    start_entry: &Entry,
    end_entry: &Entry,
    duration_ns: u64,
    frame_rate: &FrameRate,
    label: &str,
) -> Result<crate::media::audio_match::AnalysisRegionNs, String> {
    let start_ns =
        crate::ui::timecode::parse_timecode_to_ns(start_entry.text().as_ref(), frame_rate)
            .map_err(|e| format!("{label} in: {e}"))?;
    let end_ns = crate::ui::timecode::parse_timecode_to_ns(end_entry.text().as_ref(), frame_rate)
        .map_err(|e| format!("{label} out: {e}"))?;
    if end_ns <= start_ns {
        return Err(format!("{label} out must be after {label} in."));
    }
    if end_ns > duration_ns {
        return Err(format!(
            "{label} range exceeds clip duration ({}).",
            crate::ui::timecode::format_ns_as_timecode(duration_ns, frame_rate)
        ));
    }
    Ok(crate::media::audio_match::AnalysisRegionNs { start_ns, end_ns })
}

fn sync_match_audio_mode_ui(
    dialog: &gtk4::Window,
    description_label: &Label,
    region_box: &GBox,
    mode: MatchAudioDialogMode,
) {
    description_label.set_text(mode.description());
    region_box.set_visible(mode.shows_region_fields());
    dialog.set_default_size(460, if mode.shows_region_fields() { 420 } else { 260 });
}

fn sync_match_audio_channel_mode_ui(
    description_label: &Label,
    mode: crate::media::audio_match::AudioMatchChannelMode,
) {
    description_label.set_text(mode.description());
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
    pub voice_enhance_check: CheckButton,
    pub voice_enhance_strength_slider: Scale,
    pub voice_enhance_status_label: Label,
    pub voice_enhance_retry_btn: Button,
    pub voice_isolation_slider: Scale,
    pub vi_pad_slider: Scale,
    pub vi_fade_slider: Scale,
    pub vi_floor_slider: Scale,
    pub vi_source_dropdown: gtk4::DropDown,
    pub vi_silence_threshold_slider: Scale,
    pub vi_silence_min_ms_slider: Scale,
    pub vi_silence_actions_row: GBox,
    pub vi_suggest_btn: Button,
    pub vi_analyze_btn: Button,
    pub vi_intervals_label: Label,
    pub pan_slider: Scale,
    pub normalize_btn: Button,
    pub match_audio_btn: Button,
    pub clear_match_eq_btn: Button,
    pub match_eq_curve: gtk4::DrawingArea,
    pub match_eq_curve_state: Rc<RefCell<Vec<crate::model::clip::EqBand>>>,
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
    pub surround_position_dropdown: gtk4::ComboBoxText,
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
    pub motion_blur_check: CheckButton,
    pub motion_blur_shutter_slider: Scale,
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
    /// Backing model for `slow_motion_dropdown`. Held so the window glue
    /// can append/remove the "AI Interpolation (RIFE)" entry when the RIFE
    /// model is installed/removed at runtime.
    pub slow_motion_model: StringList,
    /// `true` while the dropdown contains the AI Interpolation entry. The
    /// window glue toggles this in sync with the on-disk RIFE model.
    pub slow_motion_has_ai: Cell<bool>,
    /// Status row showing AI frame-interpolation sidecar progress
    /// (`Generating…` / `Ready` / `Error` / `Model not installed`).
    pub frame_interp_status: Label,
    // Transitions
    pub transition_kind_dropdown: gtk4::ComboBoxText,
    pub transition_duration_ms: gtk4::SpinButton,
    pub transition_alignment_dropdown: gtk4::ComboBoxText,
    pub transition_clear_btn: Button,
    pub transition_status_label: Label,
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
    pub transition_section: GBox,
    pub lut_section_box: GBox,
    // Audition / clip versions
    pub audition_section_box: GBox,
    pub audition_takes_list: gtk4::ListBox,
    pub audition_add_take_btn: Button,
    pub audition_remove_take_btn: Button,
    pub audition_finalize_btn: Button,
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
    // Subtitles (speech-to-text)
    pub subtitle_section: GBox,
    pub subtitle_controls_box: GBox,
    pub subtitle_no_model_box: GBox,
    pub subtitle_generate_btn: Button,
    pub subtitle_generate_spinner: gtk4::Spinner,
    pub subtitle_generate_label: Label,
    pub subtitle_language_dropdown: gtk4::DropDown,
    pub subtitle_expander: Expander,
    pub subtitle_segments_section: GBox,
    pub compound_subtitle_label: gtk4::TextView,
    pub subtitle_segments_expander: Expander,
    pub subtitle_list_box: GBox,
    /// Tracks displayed segment IDs to avoid rebuilding on every update tick.
    pub subtitle_segments_snapshot: Rc<RefCell<Vec<String>>>,
    pub subtitle_clear_btn: Button,
    pub subtitle_error_label: Label,
    pub subtitle_font_btn: gtk4::Button,
    pub subtitle_color_btn: gtk4::ColorDialogButton,
    pub subtitle_highlight_dropdown: gtk4::DropDown,
    pub sub_bold_btn: gtk4::ToggleButton,
    pub sub_italic_btn: gtk4::ToggleButton,
    pub sub_underline_btn: gtk4::ToggleButton,
    pub sub_shadow_btn: gtk4::ToggleButton,
    pub sub_visible_check: CheckButton,
    pub hl_bold_check: CheckButton,
    pub hl_color_check: CheckButton,
    pub hl_underline_check: CheckButton,
    pub hl_stroke_check: CheckButton,
    pub hl_italic_check: CheckButton,
    pub hl_bg_check: CheckButton,
    pub hl_shadow_check: CheckButton,
    pub subtitle_bg_highlight_color_btn: gtk4::ColorDialogButton,
    pub subtitle_highlight_stroke_color_btn: gtk4::ColorDialogButton,
    pub subtitle_highlight_stroke_color_row: GBox,
    pub subtitle_bg_highlight_color_row: GBox,
    pub subtitle_highlight_color_btn: gtk4::ColorDialogButton,
    pub subtitle_highlight_color_row: GBox,
    pub subtitle_word_window_slider: Scale,
    pub subtitle_position_slider: Scale,
    pub subtitle_outline_color_btn: gtk4::ColorDialogButton,
    pub subtitle_bg_box_check: CheckButton,
    pub subtitle_bg_color_btn: gtk4::ColorDialogButton,
    pub subtitle_export_srt_btn: Button,
    pub subtitle_import_srt_btn: Button,
    pub subtitle_copy_style_btn: Button,
    pub subtitle_paste_style_btn: Button,
    pub subtitle_style_clipboard: Rc<RefCell<Option<SubtitleStyleClipboard>>>,
    pub subtitle_style_box: GBox,
    /// Set to `true` when the STT model is present; controls section content.
    pub stt_model_available: Cell<bool>,
    /// Set to `true` while an STT job is in flight for the selected clip.
    pub stt_generating: Cell<bool>,
    // Shape mask
    pub mask_section: GBox,
    pub mask_enable: CheckButton,
    pub mask_shape_dropdown: gtk4::DropDown,
    pub mask_center_x_slider: Scale,
    pub mask_center_y_slider: Scale,
    pub mask_width_slider: Scale,
    pub mask_height_slider: Scale,
    pub mask_rotation_spin: gtk4::SpinButton,
    pub mask_feather_slider: Scale,
    pub mask_expansion_slider: Scale,
    pub mask_invert_check: CheckButton,
    pub mask_path_editor_box: GBox,
    pub mask_rect_ellipse_controls: GBox,
    /// "Generate with SAM" button in the shape-mask panel. Always created so
    /// downstream populate code can touch it unconditionally; the click
    /// handler + visibility are gated on `feature = "ai-inference"`.
    pub sam_generate_btn: Button,
    /// In-flight SAM job state, `Some` while a background thread is still
    /// running and `None` once the result has been applied or dropped.
    /// The polling tick installed in `build_inspector` drains this.
    #[cfg(feature = "ai-inference")]
    pub sam_job_handle: Rc<RefCell<Option<SamJobInFlight>>>,
    // HSL Qualifier (secondary color correction)
    pub hsl_section: GBox,
    pub hsl_enable: CheckButton,
    pub hsl_invert: CheckButton,
    pub hsl_view_mask: CheckButton,
    pub hsl_hue_min: Scale,
    pub hsl_hue_max: Scale,
    pub hsl_hue_softness: Scale,
    pub hsl_sat_min: Scale,
    pub hsl_sat_max: Scale,
    pub hsl_sat_softness: Scale,
    pub hsl_lum_min: Scale,
    pub hsl_lum_max: Scale,
    pub hsl_lum_softness: Scale,
    pub hsl_brightness: Scale,
    pub hsl_contrast: Scale,
    pub hsl_saturation: Scale,
    // Motion tracking
    pub tracking_section: GBox,
    pub tracking_tracker_dropdown: gtk4::DropDown,
    pub tracking_add_btn: Button,
    pub tracking_remove_btn: Button,
    pub tracking_label_entry: Entry,
    pub tracking_edit_region_btn: gtk4::ToggleButton,
    pub tracking_center_x_slider: Scale,
    pub tracking_center_y_slider: Scale,
    pub tracking_width_slider: Scale,
    pub tracking_height_slider: Scale,
    pub tracking_rotation_spin: gtk4::SpinButton,
    pub tracking_run_btn: Button,
    pub tracking_cancel_btn: Button,
    pub tracking_auto_crop_btn: Button,
    pub tracking_auto_crop_padding_slider: Scale,
    pub tracking_status_label: Label,
    pub tracking_target_dropdown: gtk4::DropDown,
    pub tracking_reference_dropdown: gtk4::DropDown,
    pub tracking_clear_binding_btn: Button,
    pub tracking_binding_status_label: Label,
    pub selected_motion_tracker_id: Rc<RefCell<Option<String>>>,
    pub tracking_tracker_ids: Rc<RefCell<Vec<Option<String>>>>,
    pub tracking_reference_choices: Rc<RefCell<Vec<Option<MotionTrackerReference>>>>,
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
    /// Repopulate the audition takes ListBox from the given clip. Must be
    /// called whenever the active clip is an audition clip OR when its
    /// `audition_takes` / `audition_active_take_index` change. Each row
    /// stores its take index in `widget-name` so click handlers can
    /// recover the index without re-parsing labels.
    pub fn refresh_audition_takes_list(&self, clip: &crate::model::clip::Clip) {
        // Clear existing rows.
        while let Some(row) = self.audition_takes_list.first_child() {
            self.audition_takes_list.remove(&row);
        }
        let Some(takes) = clip.audition_takes.as_ref() else {
            self.audition_remove_take_btn.set_sensitive(false);
            return;
        };
        let active = clip.audition_active_take_index;
        for (i, take) in takes.iter().enumerate() {
            let row = gtk4::ListBoxRow::new();
            row.set_widget_name(&format!("audition-take-{}", i));
            let row_box = GBox::new(Orientation::Horizontal, 8);
            row_box.set_margin_top(6);
            row_box.set_margin_bottom(6);
            row_box.set_margin_start(8);
            row_box.set_margin_end(8);
            let label_text = if take.label.is_empty() {
                format!("Take {}", i + 1)
            } else {
                take.label.clone()
            };
            let label_box = GBox::new(Orientation::Vertical, 2);
            let title_lbl = Label::new(Some(&label_text));
            title_lbl.set_xalign(0.0);
            if i == active {
                title_lbl.add_css_class("heading");
            }
            label_box.append(&title_lbl);
            let dur_secs = take
                .source_out
                .saturating_sub(take.source_in) as f64
                / 1_000_000_000.0;
            let stem = std::path::Path::new(&take.source_path)
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("");
            let sub_lbl = Label::new(Some(&format!("{}  ·  {:.2}s", stem, dur_secs)));
            sub_lbl.set_xalign(0.0);
            sub_lbl.add_css_class("dim-label");
            sub_lbl.add_css_class("caption");
            label_box.append(&sub_lbl);
            label_box.set_hexpand(true);
            row_box.append(&label_box);
            if i == active {
                let badge = Label::new(Some("Active"));
                badge.add_css_class("accent");
                badge.add_css_class("caption-heading");
                row_box.append(&badge);
            }
            row.set_child(Some(&row_box));
            self.audition_takes_list.append(&row);
            if i == active {
                self.audition_takes_list.select_row(Some(&row));
            }
        }
        // Remove button enabled only when a non-active row is selected.
        self.audition_remove_take_btn.set_sensitive(false);
    }

    /// Get the currently selected interpolation mode from the dropdown.
    pub fn selected_interpolation(&self) -> KeyframeInterpolation {
        match self.interp_dropdown.selected() {
            1 => KeyframeInterpolation::EaseIn,
            2 => KeyframeInterpolation::EaseOut,
            3 => KeyframeInterpolation::EaseInOut,
            _ => KeyframeInterpolation::Linear,
        }
    }

    pub fn current_motion_tracker_id(&self) -> Option<String> {
        self.tracking_tracker_ids
            .borrow()
            .get(self.tracking_tracker_dropdown.selected() as usize)
            .cloned()
            .flatten()
            .or_else(|| self.selected_motion_tracker_id.borrow().clone())
    }

    pub fn selected_tracking_reference_choice(&self) -> Option<MotionTrackerReference> {
        self.tracking_reference_choices
            .borrow()
            .get(self.tracking_reference_dropdown.selected() as usize)
            .cloned()
            .flatten()
    }

    pub fn tracking_target_is_mask(&self) -> bool {
        self.tracking_target_dropdown.selected() == 1
    }

    fn sync_tracking_tracker_controls(&self, clip: &crate::model::clip::Clip) {
        let selected_tracker_id = self
            .selected_motion_tracker_id
            .borrow()
            .clone()
            .filter(|tracker_id| clip.motion_tracker_ref(tracker_id).is_some())
            .or_else(|| {
                clip.motion_trackers
                    .first()
                    .map(|tracker| tracker.id.clone())
            });
        *self.selected_motion_tracker_id.borrow_mut() = selected_tracker_id.clone();

        let tracker_labels: Vec<String> = if clip.motion_trackers.is_empty() {
            vec!["No trackers yet".to_string()]
        } else {
            clip.motion_trackers
                .iter()
                .map(|tracker| tracker.label.clone())
                .collect()
        };
        let tracker_label_refs = tracker_labels
            .iter()
            .map(|label| label.as_str())
            .collect::<Vec<_>>();
        let tracker_model = gtk4::StringList::new(&tracker_label_refs);
        self.tracking_tracker_dropdown
            .set_model(Some(&tracker_model));

        let tracker_ids: Vec<Option<String>> = if clip.motion_trackers.is_empty() {
            vec![None]
        } else {
            clip.motion_trackers
                .iter()
                .map(|tracker| Some(tracker.id.clone()))
                .collect()
        };
        *self.tracking_tracker_ids.borrow_mut() = tracker_ids;

        let selected_index = selected_tracker_id
            .as_ref()
            .and_then(|tracker_id| {
                clip.motion_trackers
                    .iter()
                    .position(|tracker| tracker.id == *tracker_id)
            })
            .unwrap_or(0);
        self.tracking_tracker_dropdown
            .set_selected(selected_index.min(u32::MAX as usize) as u32);

        let can_analyze = clip_supports_tracking_analysis(clip).is_ok();
        let selected_tracker = selected_tracker_id
            .as_ref()
            .and_then(|tracker_id| clip.motion_tracker_ref(tracker_id));

        self.tracking_add_btn.set_sensitive(can_analyze);
        self.tracking_remove_btn
            .set_sensitive(can_analyze && selected_tracker.is_some());
        self.tracking_label_entry
            .set_sensitive(can_analyze && selected_tracker.is_some());
        self.tracking_edit_region_btn
            .set_sensitive(can_analyze && selected_tracker.is_some());
        self.tracking_center_x_slider
            .set_sensitive(can_analyze && selected_tracker.is_some());
        self.tracking_center_y_slider
            .set_sensitive(can_analyze && selected_tracker.is_some());
        self.tracking_width_slider
            .set_sensitive(can_analyze && selected_tracker.is_some());
        self.tracking_height_slider
            .set_sensitive(can_analyze && selected_tracker.is_some());
        self.tracking_rotation_spin
            .set_sensitive(can_analyze && selected_tracker.is_some());
        self.tracking_run_btn
            .set_sensitive(can_analyze && selected_tracker.is_some());
        self.tracking_auto_crop_btn
            .set_sensitive(can_analyze && selected_tracker.is_some());
        self.tracking_auto_crop_padding_slider
            .set_sensitive(can_analyze && selected_tracker.is_some());
        self.tracking_cancel_btn.set_sensitive(false);
        if !can_analyze || selected_tracker.is_none() {
            self.tracking_edit_region_btn.set_active(false);
        }

        if let Some(tracker) = selected_tracker {
            self.tracking_label_entry.set_text(&tracker.label);
            self.tracking_center_x_slider
                .set_value(tracker.analysis_region.center_x);
            self.tracking_center_y_slider
                .set_value(tracker.analysis_region.center_y);
            self.tracking_width_slider
                .set_value(tracker.analysis_region.width);
            self.tracking_height_slider
                .set_value(tracker.analysis_region.height);
            self.tracking_rotation_spin
                .set_value(tracker.analysis_region.rotation_deg);
            self.tracking_run_btn.set_label(
                if tracker.samples.is_empty() && tracker.analysis_end_ns.is_none() {
                    "Track Region"
                } else {
                    "Re-run Tracking"
                },
            );
        } else {
            let default_region = crate::model::clip::TrackingRegion::default();
            self.tracking_label_entry.set_text("");
            self.tracking_center_x_slider
                .set_value(default_region.center_x);
            self.tracking_center_y_slider
                .set_value(default_region.center_y);
            self.tracking_width_slider.set_value(default_region.width);
            self.tracking_height_slider.set_value(default_region.height);
            self.tracking_rotation_spin
                .set_value(default_region.rotation_deg);
            self.tracking_run_btn.set_label("Track Region");
        }
    }

    fn sync_tracking_reference_controls(&self, project: &Project, clip: &crate::model::clip::Clip) {
        let target_labels = if clip.masks.is_empty() {
            vec!["Clip Transform"]
        } else {
            vec!["Clip Transform", "First Mask"]
        };
        let target_model = gtk4::StringList::new(&target_labels);
        self.tracking_target_dropdown.set_model(Some(&target_model));
        let mask_binding = clip
            .masks
            .first()
            .and_then(|mask| mask.tracking_binding.clone());
        let clip_binding = clip.tracking_binding.clone();
        let active_binding = mask_binding.clone().or(clip_binding.clone());
        let target_is_mask = mask_binding.is_some() && !clip.masks.is_empty();
        self.tracking_target_dropdown
            .set_selected(if target_is_mask { 1 } else { 0 });
        self.tracking_target_dropdown
            .set_sensitive(!clip.masks.is_empty());

        let available_references = project.motion_tracker_references();
        let has_usable_reference = available_references
            .iter()
            .any(|reference| reference.enabled && reference.sample_count > 0);
        let mut reference_choices = vec![None];
        let mut reference_labels = vec!["None".to_string()];
        for reference in available_references.iter().cloned() {
            reference_labels.push(tracking_reference_label(&reference));
            reference_choices.push(Some(reference));
        }
        let reference_label_refs = reference_labels
            .iter()
            .map(|label| label.as_str())
            .collect::<Vec<_>>();
        let reference_model = gtk4::StringList::new(&reference_label_refs);
        self.tracking_reference_dropdown
            .set_model(Some(&reference_model));

        let selected_index = active_binding
            .as_ref()
            .and_then(|binding| {
                reference_choices.iter().position(|choice| {
                    choice
                        .as_ref()
                        .map(|reference| {
                            reference.source_clip_id == binding.source_clip_id
                                && reference.tracker_id == binding.tracker_id
                        })
                        .unwrap_or(false)
                })
            })
            .unwrap_or(0);
        *self.tracking_reference_choices.borrow_mut() = reference_choices;
        self.tracking_reference_dropdown
            .set_selected(selected_index.min(u32::MAX as usize) as u32);
        self.tracking_clear_binding_btn
            .set_sensitive(active_binding.is_some());
        self.tracking_binding_status_label.remove_css_class("error");

        if let Some(binding) = active_binding {
            let binding_reference = available_references
                .iter()
                .find(|reference| {
                    reference.source_clip_id == binding.source_clip_id
                        && reference.tracker_id == binding.tracker_id
                })
                .cloned();
            let binding_label = binding_reference
                .as_ref()
                .map(tracking_reference_label)
                .unwrap_or_else(|| format!("{} — {}", binding.source_clip_id, binding.tracker_id));
            let target_label = if target_is_mask {
                "first mask"
            } else {
                "clip transform"
            };
            match project
                .clip_ref(&binding.source_clip_id)
                .and_then(|source_clip| source_clip.motion_tracker_ref(&binding.tracker_id))
            {
                Some(tracker) if !tracker.enabled => {
                    self.tracking_binding_status_label.add_css_class("error");
                    self.tracking_binding_status_label.set_text(&format!(
                        "Attached to {binding_label} on the {target_label}, but that tracker is disabled."
                    ));
                }
                Some(tracker) if tracker.samples.is_empty() => {
                    self.tracking_binding_status_label.add_css_class("error");
                    let source_label = project
                        .clip_ref(&binding.source_clip_id)
                        .map(|source_clip| {
                            let label = source_clip.label.trim();
                            if label.is_empty() {
                                binding.source_clip_id.clone()
                            } else {
                                label.to_string()
                            }
                        })
                        .unwrap_or_else(|| binding.source_clip_id.clone());
                    self.tracking_binding_status_label.set_text(&format!(
                        "Attached to {binding_label} on the {target_label}, but that tracker has no motion samples. Select {source_label} and run Track Region again."
                    ));
                }
                Some(_) => {
                    self.tracking_binding_status_label.set_text(&format!(
                        "Attached to {binding_label} on the {target_label}."
                    ));
                }
                None => {
                    self.tracking_binding_status_label.add_css_class("error");
                    self.tracking_binding_status_label.set_text(&format!(
                        "Attached to {binding_label} on the {target_label}, but that tracker no longer exists."
                    ));
                }
            }
        } else if !has_usable_reference {
            self.tracking_binding_status_label.set_text(
                "No motion trackers with sampled motion are available in the project yet. Select a source clip and run Track Region first.",
            );
        } else if clip.masks.is_empty() {
            self.tracking_binding_status_label
                .set_text("Choose a tracker with sampled motion to drive this clip transform.");
        } else {
            self.tracking_binding_status_label
                .set_text(
                    "Choose a tracker with sampled motion to drive this clip transform or the first mask.",
                );
        }
    }

    /// Rebuild the applied frei0r effects list in the Inspector.
    fn rebuild_frei0r_effects_list(&self, effects: &[crate::model::clip::Frei0rEffect]) {
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
                        let track_id = project
                            .borrow()
                            .find_track_id_for_clip(&cid)
                            .unwrap_or_default();
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
                            on_execute_command(Box::new(
                                crate::undo::ReorderFrei0rEffectsCommand {
                                    clip_id: cid,
                                    track_id,
                                    index_a: idx - 1,
                                    index_b: idx,
                                },
                            ));
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
                            on_execute_command(Box::new(
                                crate::undo::ReorderFrei0rEffectsCommand {
                                    clip_id: cid,
                                    track_id,
                                    index_a: idx,
                                    index_b: idx + 1,
                                },
                            ));
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
                        let track_id = project
                            .borrow()
                            .find_track_id_for_clip(&cid)
                            .unwrap_or_default();
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
                                            if let Some(clip) =
                                                track.clips.iter().find(|c| c.id == cid)
                                            {
                                                tid = track.id.clone();
                                                if let Some(e) = clip
                                                    .frei0r_effects
                                                    .iter()
                                                    .find(|e| e.id == effect_id)
                                                {
                                                    old = e.params.clone();
                                                }
                                                break;
                                            }
                                        }
                                        (tid, old)
                                    };
                                    {
                                        let mut proj = project.borrow_mut();
                                        if let Some(clip) = proj.clip_mut(&cid) {
                                            if let Some(e) = clip
                                                .frei0r_effects
                                                .iter_mut()
                                                .find(|e| e.id == effect_id)
                                            {
                                                e.params.insert(pname.clone(), val);
                                            }
                                        }
                                        proj.dirty = true;
                                    }
                                    let new_params = {
                                        let proj = project.borrow();
                                        proj.tracks
                                            .iter()
                                            .find(|t| t.id == track_id)
                                            .and_then(|t| t.clips.iter().find(|c| c.id == cid))
                                            .and_then(|c| {
                                                c.frei0r_effects.iter().find(|e| e.id == effect_id)
                                            })
                                            .map(|e| e.params.clone())
                                            .unwrap_or_else(|| old_params.clone())
                                    };
                                    on_execute_command(Box::new(
                                        crate::undo::SetFrei0rEffectParamsCommand {
                                            clip_id: cid.clone(),
                                            track_id,
                                            effect_id: effect_id.clone(),
                                            old_params,
                                            new_params,
                                        },
                                    ));
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
                            if !min.is_finite() || min < -1e6 {
                                min = 0.0;
                            }
                            if !max.is_finite() || max > 1e6 {
                                max = 1.0;
                            }
                            if min >= max {
                                min = 0.0;
                                max = 1.0;
                            }
                            let step = ((max - min) / 100.0).max(f64::MIN_POSITIVE);
                            let slider = Scale::with_range(Orientation::Horizontal, min, max, step);
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
                                        if let Some(clip) = proj.clip_mut(&cid) {
                                            if let Some(e) = clip
                                                .frei0r_effects
                                                .iter_mut()
                                                .find(|e| e.id == effect_id)
                                            {
                                                e.params.insert(pname.clone(), val);
                                            }
                                        }
                                        proj.dirty = true;
                                    }
                                    on_params_changed();
                                }
                            });

                            // Undo: GestureClick + EventControllerFocus snapshot/commit.
                            {
                                type SnapCell = Rc<
                                    RefCell<
                                        Option<(
                                            String,
                                            String,
                                            String,
                                            std::collections::HashMap<String, f64>,
                                        )>,
                                    >,
                                >;
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
                                                if let Some(clip) =
                                                    track.clips.iter().find(|c| c.id == cid)
                                                {
                                                    if let Some(e) = clip
                                                        .frei0r_effects
                                                        .iter()
                                                        .find(|e| e.id == effect_id_u)
                                                    {
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
                                        if let Some((clip_id, track_id, eff_id, old_params)) = entry
                                        {
                                            let new_params = {
                                                let proj = project.borrow();
                                                proj.tracks
                                                    .iter()
                                                    .find(|t| t.id == track_id)
                                                    .and_then(|t| {
                                                        t.clips.iter().find(|c| c.id == clip_id)
                                                    })
                                                    .and_then(|c| {
                                                        c.frei0r_effects
                                                            .iter()
                                                            .find(|e| e.id == eff_id)
                                                    })
                                                    .map(|e| e.params.clone())
                                                    .unwrap_or_else(|| old_params.clone())
                                            };
                                            on_execute_command(Box::new(
                                                crate::undo::SetFrei0rEffectParamsCommand {
                                                    clip_id,
                                                    track_id,
                                                    effect_id: eff_id,
                                                    old_params,
                                                    new_params,
                                                },
                                            ));
                                        }
                                    }
                                };

                                let ges = gtk4::GestureClick::new();
                                {
                                    let do_snapshot = do_snapshot.clone();
                                    ges.connect_pressed(move |_, _, _, _| {
                                        do_snapshot();
                                    });
                                }
                                {
                                    let do_commit = do_commit.clone();
                                    ges.connect_released(move |_, _, _, _| {
                                        do_commit();
                                    });
                                }
                                slider.add_controller(ges);

                                let focus_ctrl = gtk4::EventControllerFocus::new();
                                {
                                    let do_snapshot = do_snapshot.clone();
                                    focus_ctrl.connect_enter(move |_| {
                                        do_snapshot();
                                    });
                                }
                                {
                                    let do_commit = do_commit.clone();
                                    focus_ctrl.connect_leave(move |_| {
                                        do_commit();
                                    });
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
                        let str_list =
                            StringList::new(&values.iter().map(|s| s.as_str()).collect::<Vec<_>>());
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
                                        if let Some(clip) = proj.clip_mut(&cid) {
                                            if let Some(e) = clip
                                                .frei0r_effects
                                                .iter_mut()
                                                .find(|e| e.id == effect_id)
                                            {
                                                e.string_params
                                                    .insert(pname.clone(), new_val.clone());
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
                                    if let Some(clip) = proj.clip_mut(&cid) {
                                        if let Some(eff) = clip
                                            .frei0r_effects
                                            .iter_mut()
                                            .find(|eff| eff.id == effect_id)
                                        {
                                            eff.string_params
                                                .insert(pname.clone(), new_val.clone());
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
            (
                "Midtones",
                "gray-color-r",
                "gray-color-g",
                "gray-color-b",
                0.5,
            ),
            (
                "Shadows",
                "black-color-r",
                "black-color-g",
                "black-color-b",
                0.0,
            ),
            (
                "Highlights",
                "white-color-r",
                "white-color-g",
                "white-color-b",
                1.0,
            ),
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
                        if let Some(clip) = proj.clip_mut(&cid) {
                            if let Some(e) =
                                clip.frei0r_effects.iter_mut().find(|e| e.id == effect_id)
                            {
                                e.params.insert(rk.clone(), r);
                                e.params.insert(gk.clone(), g);
                                e.params.insert(bk.clone(), b);
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
                            if let Some(clip) = proj.clip_mut(&cid) {
                                if let Some(e) =
                                    clip.frei0r_effects.iter_mut().find(|e| e.id == effect_id)
                                {
                                    e.params.insert(rk.clone(), r);
                                    e.params.insert(gk.clone(), g);
                                    e.params.insert(bk.clone(), b);
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
    fn build_curves_editor(&self, effect: &crate::model::clip::Frei0rEffect, container: &GBox) {
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
                    if let Some(clip) = proj.clip_mut(&cid) {
                        if let Some(e) = clip.frei0r_effects.iter_mut().find(|e| e.id == effect_id)
                        {
                            e.params.insert("channel".to_string(), ch_val);
                            e.params.insert("show-curves".to_string(), 0.0);
                            e.params
                                .insert("curve-point-number".to_string(), pts.len() as f64 / 10.0);
                            for (i, &(inp, out)) in pts.iter().enumerate() {
                                e.params.insert(format!("point-{}-input-value", i + 1), inp);
                                e.params
                                    .insert(format!("point-{}-output-value", i + 1), out);
                            }
                            // Clear unused point slots
                            for i in (pts.len() + 1)..=5 {
                                e.params.remove(&format!("point-{i}-input-value"));
                                e.params.remove(&format!("point-{i}-output-value"));
                            }
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
    fn build_levels_editor(&self, effect: &crate::model::clip::Frei0rEffect, container: &GBox) {
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
                        if let Some(clip) = proj.clip_mut(&cid) {
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

        let clip = clip_id.and_then(|id| project.clip_ref(id));

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
                let is_compound = c.kind == ClipKind::Compound;
                let is_multicam = c.kind == ClipKind::Multicam;
                let is_audition = c.kind == ClipKind::Audition;
                let is_visual = is_video
                    || is_image
                    || is_title_clip
                    || is_adjustment
                    || is_compound
                    || is_multicam
                    || is_audition;
                self.color_section
                    .set_visible(is_video || is_image || is_adjustment || is_audition);
                self.audio_section.set_visible(is_video || is_audio || is_audition);
                self.transform_section.set_visible(is_visual);
                self.title_section_box
                    .set_visible(is_visual && !is_adjustment && !is_compound && !is_multicam && !is_audition);
                self.speed_section_box
                    .set_visible(!is_title_clip && !is_adjustment && !is_compound && !is_multicam && !is_audition);
                // Audition section is visible only for audition clips. Repopulate the takes list.
                self.audition_section_box.set_visible(is_audition);
                if is_audition {
                    self.refresh_audition_takes_list(c);
                }
                self.lut_section_box
                    .set_visible(is_video || is_image || is_adjustment);
                self.chroma_key_section.set_visible(is_video || is_image);
                self.bg_removal_section
                    .set_visible((is_video || is_image) && self.bg_removal_model_available.get());
                self.subtitle_section
                    .set_visible(is_video || is_audio || is_compound);
                self.tracking_section.set_visible(is_visual);
                let has_stt_model = self.stt_model_available.get();
                let generating = self.stt_generating.get();
                // For compound clips, hide the per-clip generate/clear controls;
                // the aggregated segment list shows below via subtitle_segments_section.
                self.subtitle_no_model_box
                    .set_visible(!is_compound && (!has_stt_model && !generating));
                self.subtitle_controls_box.set_visible(
                    !is_compound
                        && (has_stt_model || !c.subtitle_segments.is_empty() || generating),
                );
                self.subtitle_generate_btn
                    .set_sensitive(has_stt_model && c.subtitle_segments.is_empty() && !generating);
                self.subtitle_generate_spinner.set_visible(generating);
                self.subtitle_generate_spinner.set_spinning(generating);
                if generating {
                    self.subtitle_generate_label.set_text("Generating\u{2026}");
                } else {
                    self.subtitle_generate_label.set_text("Generate Subtitles");
                }
                self.subtitle_clear_btn
                    .set_sensitive(!c.subtitle_segments.is_empty() && !generating);
                // Show segment list and style controls when subtitles exist.
                // For compound clips, collect subtitle segments from all
                // internal clips so the user can see them at root level.
                let visible_segments: Vec<crate::model::clip::SubtitleSegment> = if is_compound {
                    fn collect_compound_segments(
                        tracks: &[crate::model::track::Track],
                    ) -> Vec<crate::model::clip::SubtitleSegment> {
                        let mut segs = Vec::new();
                        for track in tracks {
                            for clip in &track.clips {
                                if let Some(ref inner) = clip.compound_tracks {
                                    segs.extend(collect_compound_segments(inner));
                                } else {
                                    segs.extend(clip.subtitle_segments.iter().cloned());
                                }
                            }
                        }
                        segs
                    }
                    c.compound_tracks
                        .as_deref()
                        .map(collect_compound_segments)
                        .unwrap_or_default()
                } else {
                    // Filter to only show segments within the visible duration.
                    // Subtitle times are clip-local (0 = source_in), so compare
                    // against source_duration rather than source-absolute bounds.
                    let src_dur = c.source_duration();
                    c.subtitle_segments
                        .iter()
                        .filter(|s| s.start_ns < src_dur && s.end_ns > 0)
                        .cloned()
                        .collect()
                };
                let has_subtitles = !visible_segments.is_empty();
                log::debug!(
                    "inspector subtitle: clip={} kind={:?} own={} visible={} has_subtitles={}",
                    c.id,
                    c.kind,
                    c.subtitle_segments.len(),
                    visible_segments.len(),
                    has_subtitles,
                );
                {
                    use gtk4::prelude::TextBufferExt;
                    use gtk4::prelude::TextViewExt;
                    if is_compound {
                        // Hide the expander and separate segments section; show
                        // the aggregated info directly in the subtitle_section.
                        self.subtitle_expander.set_visible(false);
                        self.subtitle_segments_section.set_visible(false);
                        let mut info =
                            format!("Subtitles ({} segments)\n\n", visible_segments.len());
                        for seg in &visible_segments {
                            let s = seg.start_ns as f64 / 1e9;
                            let e = seg.end_ns as f64 / 1e9;
                            let txt = if seg.text.len() > 50 {
                                format!("{}…", &seg.text[..50])
                            } else {
                                seg.text.clone()
                            };
                            info.push_str(&format!("{:.1}s – {:.1}s  {}\n", s, e, txt));
                        }
                        if visible_segments.is_empty() {
                            info = "No subtitles.\nDouble-click compound to edit individual clips."
                                .into();
                        } else {
                            info.push_str("\nDouble-click compound to edit segments.");
                        }
                        self.compound_subtitle_label
                            .buffer()
                            .set_text(info.trim_end());
                        self.compound_subtitle_label.set_visible(true);
                    } else {
                        self.subtitle_expander.set_visible(true);
                        self.compound_subtitle_label.set_visible(false);
                        self.compound_subtitle_label.buffer().set_text("");
                        self.subtitle_segments_section.set_visible(has_subtitles);
                        if has_subtitles {
                            self.subtitle_segments_expander.set_label(Some(&format!(
                                "Subtitle Segments ({})",
                                visible_segments.len()
                            )));
                        }
                    }
                }

                // Only rebuild the segment list if the visible segment IDs changed.
                let current_ids: Vec<String> =
                    visible_segments.iter().map(|s| s.id.clone()).collect();
                let needs_rebuild = *self.subtitle_segments_snapshot.borrow() != current_ids;
                if needs_rebuild {
                    while let Some(child) = self.subtitle_list_box.first_child() {
                        self.subtitle_list_box.remove(&child);
                    }
                    if has_subtitles {
                        let project = self.project.clone();
                        let on_cmd = self.on_execute_command.clone();
                        let clip_id = c.id.clone();

                        for (i, seg) in visible_segments.iter().enumerate() {
                            let row = GBox::new(Orientation::Vertical, 1);

                            // Header row: timecode + delete button
                            let header = GBox::new(Orientation::Horizontal, 4);
                            let start_s = seg.start_ns as f64 / 1_000_000_000.0;
                            let end_s = seg.end_ns as f64 / 1_000_000_000.0;
                            let tc_label =
                                Label::new(Some(&format!("{:.1}s – {:.1}s", start_s, end_s)));
                            tc_label.set_halign(gtk::Align::Start);
                            tc_label.set_hexpand(true);
                            tc_label.add_css_class("dim-label");
                            tc_label.set_margin_start(4);
                            header.append(&tc_label);

                            let del_btn = Button::new();
                            del_btn.set_icon_name("edit-delete-symbolic");
                            del_btn.add_css_class("flat");
                            del_btn.add_css_class("circular");
                            del_btn.set_tooltip_text(Some("Delete this segment"));
                            {
                                let seg_id = seg.id.clone();
                                let seg_clone = seg.clone();
                                let seg_idx = i;
                                let clip_id_d = clip_id.clone();
                                let project_d = project.clone();
                                let on_cmd_d = on_cmd.clone();
                                del_btn.connect_clicked(move |_| {
                                    let track_id = project_d
                                        .borrow()
                                        .find_track_id_for_clip(&clip_id_d)
                                        .unwrap_or_default();
                                    on_cmd_d(Box::new(crate::undo::DeleteSubtitleSegmentCommand {
                                        clip_id: clip_id_d.clone(),
                                        track_id,
                                        segment_id: seg_id.clone(),
                                        deleted_segment: seg_clone.clone(),
                                        index: seg_idx,
                                    }));
                                });
                            }
                            header.append(&del_btn);
                            row.append(&header);

                            // Editable text entry
                            let entry = gtk4::Entry::new();
                            entry.set_text(&seg.text);
                            entry.set_hexpand(true);
                            entry.add_css_class("flat");

                            let seg_id = seg.id.clone();
                            // `last_committed_text` tracks the last value that was
                            // pushed to the undo stack. `connect_changed` mutates
                            // the model directly on every keystroke (so preview /
                            // export reflect edits without waiting for a commit
                            // boundary), and Enter / focus-out push a single
                            // EditSubtitleTextCommand spanning from the last
                            // committed text to the current text so undo works at
                            // sentence-level granularity instead of per-keystroke.
                            let last_committed_text = Rc::new(RefCell::new(seg.text.clone()));
                            let clip_id_c = clip_id.clone();
                            let project_c = project.clone();
                            let on_cmd_c = on_cmd.clone();

                            // Live model update on every keystroke. Bypasses the
                            // undo system on purpose: per-keystroke commands would
                            // make undo unusable. The commit handlers below push
                            // a consolidated command for undo.
                            //
                            // We re-sync per-word entries so karaoke / word
                            // highlight rendering uses the edited text instead
                            // of the original Whisper tokens.
                            {
                                let project_live = project_c.clone();
                                let clip_id_live = clip_id_c.clone();
                                let seg_id_live = seg_id.clone();
                                entry.connect_changed(move |e| {
                                    let new_text = e.text().to_string();
                                    let mut proj = project_live.borrow_mut();
                                    if let Some(clip) = proj.clip_mut(&clip_id_live) {
                                        if let Some(seg) = clip
                                            .subtitle_segments
                                            .iter_mut()
                                            .find(|s| s.id == seg_id_live)
                                        {
                                            if seg.text != new_text {
                                                seg.set_text_and_resync_words(new_text);
                                                proj.dirty = true;
                                            }
                                        }
                                    }
                                });
                            }

                            // Shared commit logic for both Enter and focus-out.
                            // Pushes a single EditSubtitleTextCommand from the
                            // last committed value to the current value, so undo
                            // collapses an entire typing session into one entry.
                            let commit_edit = {
                                let seg_id = seg_id.clone();
                                let last_committed_text = last_committed_text.clone();
                                let clip_id_c = clip_id_c.clone();
                                let on_cmd_c = on_cmd_c.clone();
                                Rc::new(move |new_text: String| {
                                    let prev = last_committed_text.borrow().clone();
                                    if new_text == prev {
                                        return;
                                    }
                                    on_cmd_c(Box::new(crate::undo::EditSubtitleTextCommand {
                                        clip_id: clip_id_c.clone(),
                                        track_id: String::new(),
                                        segment_id: seg_id.clone(),
                                        old_text: prev.clone(),
                                        new_text: new_text.clone(),
                                    }));
                                    *last_committed_text.borrow_mut() = new_text;
                                })
                            };

                            // Commit on Enter.
                            {
                                let commit = commit_edit.clone();
                                entry.connect_activate(move |e| {
                                    commit(e.text().to_string());
                                });
                            }
                            // Commit on focus-out (Tab, click away).
                            {
                                let commit = commit_edit.clone();
                                let focus = gtk4::EventControllerFocus::new();
                                focus.connect_leave(move |ctl| {
                                    if let Some(widget) = ctl.widget() {
                                        if let Some(entry) = widget.downcast_ref::<gtk4::Entry>() {
                                            commit(entry.text().to_string());
                                        }
                                    }
                                });
                                entry.add_controller(focus);
                            }
                            row.append(&entry);
                            self.subtitle_list_box.append(&row);
                        }
                    }
                    *self.subtitle_segments_snapshot.borrow_mut() = current_ids;
                }
                // Style controls are always visible so users can configure before generating.
                self.subtitle_style_box.set_visible(true);
                sync_subtitle_font_button(&self.subtitle_font_btn, &c.subtitle_font);
                let (r, g, b, a) = crate::ui::colors::rgba_u32_to_f32(c.subtitle_color);
                self.subtitle_color_btn
                    .set_rgba(&gdk4::RGBA::new(r, g, b, a));
                // Sync base style toggles
                self.sub_bold_btn.set_active(c.subtitle_bold);
                self.sub_italic_btn.set_active(c.subtitle_italic);
                self.sub_underline_btn.set_active(c.subtitle_underline);
                self.sub_shadow_btn.set_active(c.subtitle_shadow);
                self.sub_visible_check.set_active(c.subtitle_visible);

                // Sync highlight flags checkboxes
                let flags = &c.subtitle_highlight_flags;
                self.hl_bold_check.set_active(flags.bold);
                self.hl_color_check.set_active(flags.color);
                self.hl_underline_check.set_active(flags.underline);
                self.hl_stroke_check.set_active(flags.stroke);
                self.hl_italic_check.set_active(flags.italic);
                self.hl_bg_check.set_active(flags.background);
                self.hl_shadow_check.set_active(flags.shadow);

                // Show highlight color row when color or stroke is checked
                self.subtitle_highlight_color_row
                    .set_visible(flags.color || flags.stroke);
                // Show stroke color row only when stroke is checked, so the
                // user can pick a different colour for the karaoke stroke
                // than for the karaoke text fill.
                self.subtitle_highlight_stroke_color_row
                    .set_visible(flags.stroke);
                // Show bg highlight color row when background is checked
                self.subtitle_bg_highlight_color_row
                    .set_visible(flags.background);

                self.subtitle_word_window_slider
                    .set_value(c.subtitle_word_window_secs);
                // Show word window slider only when any highlight flag is set.
                self.subtitle_word_window_slider
                    .set_visible(!flags.is_none());
                self.subtitle_position_slider
                    .set_value(c.subtitle_position_y);
                let (or_, og, ob, oa) =
                    crate::ui::colors::rgba_u32_to_f32(c.subtitle_outline_color);
                self.subtitle_outline_color_btn
                    .set_rgba(&gdk4::RGBA::new(or_, og, ob, oa));
                self.subtitle_bg_box_check.set_active(c.subtitle_bg_box);
                let (bgr, bgg, bgb, bga) =
                    crate::ui::colors::rgba_u32_to_f32(c.subtitle_bg_box_color);
                self.subtitle_bg_color_btn
                    .set_rgba(&gdk4::RGBA::new(bgr, bgg, bgb, bga));
                self.subtitle_export_srt_btn
                    .set_sensitive(!c.subtitle_segments.is_empty());
                if flags.color || flags.stroke {
                    let (hr, hg, hb, ha) =
                        crate::ui::colors::rgba_u32_to_f32(c.subtitle_highlight_color);
                    self.subtitle_highlight_color_btn
                        .set_rgba(&gdk4::RGBA::new(hr, hg, hb, ha));
                }
                if flags.stroke {
                    let (sr, sg, sb, sa) = crate::ui::colors::rgba_u32_to_f32(
                        c.subtitle_highlight_stroke_color,
                    );
                    self.subtitle_highlight_stroke_color_btn
                        .set_rgba(&gdk4::RGBA::new(sr, sg, sb, sa));
                }
                if flags.background {
                    let (bhr, bhg, bhb, bha) =
                        crate::ui::colors::rgba_u32_to_f32(c.subtitle_bg_highlight_color);
                    self.subtitle_bg_highlight_color_btn
                        .set_rgba(&gdk4::RGBA::new(bhr, bhg, bhb, bha));
                }
                self.mask_section
                    .set_visible(is_video || is_image || is_title_clip || is_adjustment);
                self.frei0r_effects_section
                    .set_visible(is_visual && !is_compound && !is_multicam);

                // SAM button sensitivity: only enable on clip kinds
                // the pipeline can decode a frame from, when SAM is
                // installed, and when no SAM job is currently
                // running on this Inspector instance.
                #[cfg(feature = "ai-inference")]
                {
                    let sam_ready =
                        crate::media::sam_cache::find_sam_model_paths().is_some();
                    let job_busy = self.sam_job_handle.borrow().is_some();
                    self.sam_generate_btn.set_sensitive(
                        sam_ready
                            && !job_busy
                            && (is_video || is_image),
                    );
                }

                // Populate mask section from masks[0].
                if let Some(mask) = c.masks.first() {
                    self.mask_enable.set_active(mask.enabled);
                    self.mask_shape_dropdown.set_selected(match mask.shape {
                        crate::model::clip::MaskShape::Rectangle => 0,
                        crate::model::clip::MaskShape::Ellipse => 1,
                        crate::model::clip::MaskShape::Path => 2,
                    });
                    let is_path = matches!(mask.shape, crate::model::clip::MaskShape::Path);
                    self.mask_rect_ellipse_controls.set_visible(!is_path);
                    self.mask_path_editor_box.set_visible(is_path);
                    self.mask_center_x_slider.set_value(mask.center_x);
                    self.mask_center_y_slider.set_value(mask.center_y);
                    self.mask_width_slider.set_value(mask.width);
                    self.mask_height_slider.set_value(mask.height);
                    self.mask_rotation_spin.set_value(mask.rotation);
                    self.mask_feather_slider.set_value(mask.feather);
                    self.mask_expansion_slider.set_value(mask.expansion);
                    self.mask_invert_check.set_active(mask.invert);
                } else {
                    self.mask_enable.set_active(false);
                    self.mask_shape_dropdown.set_selected(0);
                    self.mask_rect_ellipse_controls.set_visible(true);
                    self.mask_path_editor_box.set_visible(false);
                    self.mask_center_x_slider.set_value(0.5);
                    self.mask_center_y_slider.set_value(0.5);
                    self.mask_width_slider.set_value(0.25);
                    self.mask_height_slider.set_value(0.25);
                    self.mask_rotation_spin.set_value(0.0);
                    self.mask_feather_slider.set_value(0.0);
                    self.mask_expansion_slider.set_value(0.0);
                    self.mask_invert_check.set_active(false);
                }

                // HSL Qualifier section visibility — same eligible kinds as
                // the primary Color panel (visual clips only).
                self.hsl_section.set_visible(is_visual && !is_compound);
                if let Some(q) = c.hsl_qualifier.as_ref() {
                    self.hsl_enable.set_active(q.enabled);
                    self.hsl_invert.set_active(q.invert);
                    self.hsl_view_mask.set_active(q.view_mask);
                    self.hsl_hue_min.set_value(q.hue_min);
                    self.hsl_hue_max.set_value(q.hue_max);
                    self.hsl_hue_softness.set_value(q.hue_softness);
                    self.hsl_sat_min.set_value(q.sat_min);
                    self.hsl_sat_max.set_value(q.sat_max);
                    self.hsl_sat_softness.set_value(q.sat_softness);
                    self.hsl_lum_min.set_value(q.lum_min);
                    self.hsl_lum_max.set_value(q.lum_max);
                    self.hsl_lum_softness.set_value(q.lum_softness);
                    self.hsl_brightness.set_value(q.brightness);
                    self.hsl_contrast.set_value(q.contrast);
                    self.hsl_saturation.set_value(q.saturation);
                } else {
                    self.hsl_enable.set_active(false);
                    self.hsl_invert.set_active(false);
                    self.hsl_view_mask.set_active(false);
                    self.hsl_hue_min.set_value(0.0);
                    self.hsl_hue_max.set_value(360.0);
                    self.hsl_hue_softness.set_value(0.0);
                    self.hsl_sat_min.set_value(0.0);
                    self.hsl_sat_max.set_value(1.0);
                    self.hsl_sat_softness.set_value(0.0);
                    self.hsl_lum_min.set_value(0.0);
                    self.hsl_lum_max.set_value(1.0);
                    self.hsl_lum_softness.set_value(0.0);
                    self.hsl_brightness.set_value(0.0);
                    self.hsl_contrast.set_value(1.0);
                    self.hsl_saturation.set_value(1.0);
                }

                self.sync_tracking_tracker_controls(c);
                self.sync_tracking_reference_controls(project, c);

                // Populate applied frei0r effects list.
                self.rebuild_frei0r_effects_list(&c.frei0r_effects);

                self.name_entry.set_text(&c.label);
                let is_title = c.kind == ClipKind::Title;
                if is_title {
                    self.path_value.set_text("(title clip — no source file)");
                    self.path_value.set_tooltip_text(None);
                } else if is_adjustment {
                    self.path_value
                        .set_text("(adjustment layer — applies effects to tracks below)");
                    self.path_value.set_tooltip_text(None);
                } else {
                    self.path_value.set_text(&c.source_path);
                    self.path_value.set_tooltip_text(Some(&c.source_path));
                }
                let is_missing = !is_title
                    && !is_adjustment
                    && missing_media_paths
                        .map(|paths| paths.contains(&c.source_path))
                        .unwrap_or_else(|| {
                            !crate::model::media_library::source_path_exists(&c.source_path)
                        });
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
                self.anamorphic_desqueeze_dropdown
                    .set_selected(anamorphic_idx);
                self.in_value.set_text(&ns_to_timecode(c.source_in));
                self.out_value.set_text(&ns_to_timecode(c.source_out));
                self.dur_value.set_text(&ns_to_timecode(c.duration()));
                self.pos_value.set_text(&ns_to_timecode(c.timeline_start));
                let current_transition = &c.outgoing_transition;
                let current_kind_id =
                    if is_supported_transition_kind(current_transition.kind_trimmed()) {
                        current_transition.kind_trimmed()
                    } else {
                        ""
                    };
                self.transition_kind_dropdown
                    .set_active_id(Some(current_kind_id));
                self.transition_alignment_dropdown
                    .set_active_id(Some(current_transition.alignment.as_str()));
                self.transition_clear_btn
                    .set_sensitive(current_transition.is_active());
                if let Some((track, clip_index)) = project.tracks.iter().find_map(|track| {
                    track
                        .clips
                        .iter()
                        .position(|clip| clip.id == c.id)
                        .map(|clip_index| (track, clip_index))
                }) {
                    if let Some(next_clip) = track.clips.get(clip_index + 1) {
                        let max_duration_ns = max_transition_duration_ns(c, next_clip);
                        let max_duration_ms =
                            (max_duration_ns.max(MIN_TRANSITION_DURATION_NS) as f64) / 1_000_000.0;
                        let display_duration_ns = if current_transition.is_active() {
                            current_transition.duration_ns.clamp(
                                MIN_TRANSITION_DURATION_NS,
                                max_duration_ns.max(MIN_TRANSITION_DURATION_NS),
                            )
                        } else {
                            DEFAULT_TRANSITION_DURATION_NS.clamp(
                                MIN_TRANSITION_DURATION_NS,
                                max_duration_ns.max(MIN_TRANSITION_DURATION_NS),
                            )
                        };
                        let current_has_kind = !current_kind_id.is_empty();
                        let boundary_supports_transition =
                            max_duration_ns >= MIN_TRANSITION_DURATION_NS;
                        self.transition_duration_ms.set_range(
                            (MIN_TRANSITION_DURATION_NS as f64) / 1_000_000.0,
                            max_duration_ms,
                        );
                        self.transition_duration_ms
                            .set_value((display_duration_ns as f64) / 1_000_000.0);
                        self.transition_kind_dropdown
                            .set_sensitive(boundary_supports_transition);
                        self.transition_duration_ms
                            .set_sensitive(boundary_supports_transition && current_has_kind);
                        self.transition_alignment_dropdown
                            .set_sensitive(boundary_supports_transition && current_has_kind);
                        let status = if !boundary_supports_transition {
                            format!(
                                "This cut is too short for a transition. Max overlap here is {:.0} ms.",
                                max_duration_ns as f64 / 1_000_000.0
                            )
                        } else if current_transition.is_active()
                            && current_transition.duration_ns > max_duration_ns
                        {
                            format!(
                                "Max duration at this cut is {:.0} ms. The saved transition will clamp when you update it.",
                                max_duration_ns as f64 / 1_000_000.0
                            )
                        } else if current_transition.is_active() {
                            format!(
                                "Max duration at this cut is {:.0} ms.",
                                max_duration_ns as f64 / 1_000_000.0
                            )
                        } else {
                            format!(
                                "Choose a transition for the cut after this clip. Max duration here is {:.0} ms.",
                                max_duration_ns as f64 / 1_000_000.0
                            )
                        };
                        self.transition_status_label.set_text(&status);
                    } else {
                        let default_duration_ms =
                            (DEFAULT_TRANSITION_DURATION_NS as f64) / 1_000_000.0;
                        self.transition_duration_ms.set_range(
                            (MIN_TRANSITION_DURATION_NS as f64) / 1_000_000.0,
                            default_duration_ms
                                .max((MIN_TRANSITION_DURATION_NS as f64) / 1_000_000.0),
                        );
                        let display_duration_ns = if current_transition.is_active() {
                            current_transition
                                .duration_ns
                                .max(MIN_TRANSITION_DURATION_NS)
                        } else {
                            DEFAULT_TRANSITION_DURATION_NS
                        };
                        self.transition_duration_ms
                            .set_value((display_duration_ns as f64) / 1_000_000.0);
                        self.transition_kind_dropdown.set_sensitive(false);
                        self.transition_duration_ms.set_sensitive(false);
                        self.transition_alignment_dropdown.set_sensitive(false);
                        let status = if current_transition.is_active() {
                            "This clip has no following clip on the same track. Remove the saved transition or add another clip after it."
                        } else {
                            "Add another clip after this one on the same track to enable outgoing transitions."
                        };
                        self.transition_status_label.set_text(status);
                    }
                } else {
                    self.transition_kind_dropdown.set_sensitive(false);
                    self.transition_duration_ms.set_sensitive(false);
                    self.transition_alignment_dropdown.set_sensitive(false);
                    self.transition_clear_btn.set_sensitive(false);
                    self.transition_status_label
                        .set_text("Transition controls are unavailable for this clip.");
                }
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
                self.motion_blur_check.set_active(c.motion_blur_enabled);
                self.motion_blur_shutter_slider
                    .set_value(c.motion_blur_shutter_angle);
                self.motion_blur_shutter_slider
                    .set_sensitive(c.motion_blur_enabled);
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
                self.voice_enhance_check.set_active(c.voice_enhance);
                self.voice_enhance_strength_slider
                    .set_value((c.voice_enhance_strength * 100.0) as f64);
                self.voice_enhance_strength_slider
                    .set_sensitive(c.voice_enhance);
                self.voice_isolation_slider
                    .set_value((c.voice_isolation * 100.0) as f64);
                self.voice_isolation_slider.set_sensitive(true);
                self.vi_pad_slider
                    .set_value(c.voice_isolation_pad_ms as f64);
                self.vi_fade_slider
                    .set_value(c.voice_isolation_fade_ms as f64);
                self.vi_floor_slider
                    .set_value((c.voice_isolation_floor * 100.0) as f64);
                let vi_detail_visible = c.voice_isolation > 0.0;
                self.vi_pad_slider.set_visible(vi_detail_visible);
                self.vi_fade_slider.set_visible(vi_detail_visible);
                self.vi_floor_slider.set_visible(vi_detail_visible);
                self.vi_source_dropdown.set_visible(vi_detail_visible);
                let is_silence_mode = matches!(
                    c.voice_isolation_source,
                    crate::model::clip::VoiceIsolationSource::Silence
                );
                self.vi_source_dropdown
                    .set_selected(if is_silence_mode { 1 } else { 0 });
                self.vi_silence_threshold_slider
                    .set_value(c.voice_isolation_silence_threshold_db as f64);
                self.vi_silence_min_ms_slider
                    .set_value(c.voice_isolation_silence_min_ms as f64);
                let silence_visible = vi_detail_visible && is_silence_mode;
                self.vi_silence_threshold_slider
                    .set_visible(silence_visible);
                self.vi_silence_min_ms_slider.set_visible(silence_visible);
                self.vi_silence_actions_row.set_visible(silence_visible);
                if c.voice_isolation_speech_intervals.is_empty() {
                    self.vi_intervals_label.set_text("Not analyzed");
                } else {
                    self.vi_intervals_label.set_text(&format!(
                        "Speech intervals: {}",
                        c.voice_isolation_speech_intervals.len()
                    ));
                }
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
                // Match EQ clear button + curve visibility/state
                let show_match_eq = c.has_match_eq();
                self.clear_match_eq_btn.set_visible(show_match_eq);
                self.match_eq_curve.set_visible(show_match_eq);
                {
                    let mut state = self.match_eq_curve_state.borrow_mut();
                    state.clear();
                    state.extend_from_slice(&c.match_eq_bands);
                }
                self.match_eq_curve.queue_draw();
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
                                    if let Some(clip) = proj.clip_mut(&clip_id) {
                                        if let Some(e) = clip
                                            .ladspa_effects
                                            .iter_mut()
                                            .find(|e| e.id == effect_id)
                                        {
                                            e.enabled = btn.is_active();
                                        }
                                    }
                                    proj.dirty = true;
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
                                    if let Some(clip) = proj.clip_mut(&clip_id) {
                                        clip.ladspa_effects.retain(|e| e.id != effect_id);
                                    }
                                    proj.dirty = true;
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
                                    if let Some(clip) = proj.clip_mut(&clip_id) {
                                        if let Some(pos) = clip
                                            .ladspa_effects
                                            .iter()
                                            .position(|e| e.id == effect_id)
                                        {
                                            if pos > 0 {
                                                clip.ladspa_effects.swap(pos, pos - 1);
                                            }
                                        }
                                    }
                                    proj.dirty = true;
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
                                    if let Some(clip) = proj.clip_mut(&clip_id) {
                                        let len = clip.ladspa_effects.len();
                                        if let Some(pos) = clip
                                            .ladspa_effects
                                            .iter()
                                            .position(|e| e.id == effect_id)
                                        {
                                            if pos + 1 < len {
                                                clip.ladspa_effects.swap(pos, pos + 1);
                                            }
                                        }
                                    }
                                    proj.dirty = true;
                                    drop(proj);
                                    on_changed();
                                });
                            }

                            // Parameter sliders
                            if let Some(info) = reg.find_by_name(&effect.plugin_name) {
                                for param_info in &info.params {
                                    let val = effect
                                        .params
                                        .get(&param_info.name)
                                        .copied()
                                        .unwrap_or(param_info.default_value);
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
                                    slider.add_mark(
                                        param_info.default_value,
                                        gtk4::PositionType::Bottom,
                                        None,
                                    );
                                    param_row.append(&slider);

                                    // Wire slider
                                    let project = self.project.clone();
                                    let clip_id = clip_id.clone();
                                    let effect_id = effect_id.clone();
                                    let param_name = param_info.name.clone();
                                    let on_changed = self.on_frei0r_changed.clone();
                                    slider.connect_value_changed(move |s| {
                                        let mut proj = project.borrow_mut();
                                        if let Some(clip) = proj.clip_mut(&clip_id) {
                                            if let Some(e) = clip
                                                .ladspa_effects
                                                .iter_mut()
                                                .find(|e| e.id == effect_id)
                                            {
                                                e.params.insert(param_name.clone(), s.value());
                                            }
                                        }
                                        proj.dirty = true;
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
                self.pitch_shift_slider.set_value(c.pitch_shift_semitones);
                self.pitch_preserve_check.set_active(c.pitch_preserve);
                // Track audio controls — read from the clip's track.
                if let Some(track) = project
                    .tracks
                    .iter()
                    .find(|t| t.clips.iter().any(|tc| tc.id == c.id))
                {
                    #[allow(deprecated)]
                    self.role_dropdown
                        .set_active_id(Some(track.audio_role.as_str()));
                    #[allow(deprecated)]
                    self.surround_position_dropdown
                        .set_active_id(Some(track.surround_position.as_str()));
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
                self.blend_mode_dropdown.set_sensitive(!is_adjustment);
                self.anamorphic_desqueeze_dropdown
                    .set_sensitive(!is_adjustment);
                self.flip_h_btn.set_sensitive(!is_adjustment);
                self.flip_v_btn.set_sensitive(!is_adjustment);
                self.title_entry.set_text(&c.title_text);
                sync_title_font_button(&self.title_font_btn, &c.title_font);
                {
                    let (r, g, b, a) = crate::ui::colors::rgba_u32_to_f32(c.title_color);
                    self.title_color_btn.set_rgba(&gdk4::RGBA::new(r, g, b, a));
                }
                self.title_x_slider.set_value(c.title_x);
                self.title_y_slider.set_value(c.title_y);
                self.title_outline_width_slider
                    .set_value(c.title_outline_width);
                {
                    let (r, g, b, a) = crate::ui::colors::rgba_u32_to_f32(c.title_outline_color);
                    self.title_outline_color_btn
                        .set_rgba(&gdk4::RGBA::new(r, g, b, a));
                }
                self.title_shadow_check.set_active(c.title_shadow);
                {
                    let (r, g, b, a) = crate::ui::colors::rgba_u32_to_f32(c.title_shadow_color);
                    self.title_shadow_color_btn
                        .set_rgba(&gdk4::RGBA::new(r, g, b, a));
                }
                self.title_shadow_x_slider
                    .set_value(c.title_shadow_offset_x);
                self.title_shadow_y_slider
                    .set_value(c.title_shadow_offset_y);
                self.title_bg_box_check.set_active(c.title_bg_box);
                {
                    let (r, g, b, a) = crate::ui::colors::rgba_u32_to_f32(c.title_bg_box_color);
                    self.title_bg_box_color_btn
                        .set_rgba(&gdk4::RGBA::new(r, g, b, a));
                }
                self.title_bg_box_padding_slider
                    .set_value(c.title_bg_box_padding);
                // When speed keyframes are present, don't auto-update the slider —
                // the user sets it to the desired value before clicking
                // "Set Speed Keyframe". Auto-resetting would clobber their input.
                // The slider updates when navigating keyframes (Prev/Next KF) or
                // when the clip selection changes.
                if c.speed_keyframes.is_empty() {
                    self.speed_slider.set_value(c.speed);
                }
                self.reverse_check.set_active(c.reverse);
                self.slow_motion_dropdown
                    .set_selected(match c.slow_motion_interp {
                        crate::model::clip::SlowMotionInterp::Off => 0,
                        crate::model::clip::SlowMotionInterp::Blend => 1,
                        crate::model::clip::SlowMotionInterp::OpticalFlow => 2,
                        // If a clip arrives with the AI variant but the
                        // model is not installed, fall back to displaying
                        // "Off" — the cache will refuse to generate
                        // anything until the model appears, at which
                        // point the dropdown will gain the AI entry and
                        // a re-load will select it correctly.
                        crate::model::clip::SlowMotionInterp::Ai => {
                            if self.slow_motion_has_ai.get() {
                                3
                            } else {
                                0
                            }
                        }
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
                log::debug!(
                    "inspector subtitle: update called with clip=None → hiding content_box"
                );
                #[cfg(feature = "ai-inference")]
                self.sam_generate_btn.set_sensitive(false);
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
                self.transition_kind_dropdown.set_active_id(Some(""));
                self.transition_duration_ms.set_range(
                    (MIN_TRANSITION_DURATION_NS as f64) / 1_000_000.0,
                    (DEFAULT_TRANSITION_DURATION_NS as f64) / 1_000_000.0,
                );
                self.transition_duration_ms
                    .set_value((DEFAULT_TRANSITION_DURATION_NS as f64) / 1_000_000.0);
                self.transition_alignment_dropdown
                    .set_active_id(Some(TransitionAlignment::EndOnCut.as_str()));
                self.transition_kind_dropdown.set_sensitive(false);
                self.transition_duration_ms.set_sensitive(false);
                self.transition_alignment_dropdown.set_sensitive(false);
                self.transition_clear_btn.set_sensitive(false);
                self.transition_status_label.set_text(
                    "Select a clip with a following clip to edit its outgoing transition.",
                );
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
                self.motion_blur_check.set_active(false);
                self.motion_blur_shutter_slider.set_value(180.0);
                self.motion_blur_shutter_slider.set_sensitive(false);
                self.shadows_slider.set_value(0.0);
                self.midtones_slider.set_value(0.0);
                self.highlights_slider.set_value(0.0);
                self.volume_slider.set_value(0.0);
                self.voice_enhance_check.set_active(false);
                self.voice_enhance_strength_slider.set_value(50.0);
                self.voice_enhance_strength_slider.set_sensitive(false);
                self.voice_isolation_slider.set_value(0.0);
                self.vi_source_dropdown.set_visible(false);
                self.vi_silence_threshold_slider.set_visible(false);
                self.vi_silence_min_ms_slider.set_visible(false);
                self.vi_silence_actions_row.set_visible(false);
                self.vi_intervals_label.set_text("Not analyzed");
                self.pan_slider.set_value(0.0);
                self.measured_loudness_label.set_text("");
                self.clear_match_eq_btn.set_visible(false);
                self.match_eq_curve.set_visible(false);
                self.match_eq_curve_state.borrow_mut().clear();
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
                self.blend_mode_dropdown.set_sensitive(true);
                self.anamorphic_desqueeze_dropdown.set_sensitive(true);
                self.flip_h_btn.set_sensitive(true);
                self.flip_v_btn.set_sensitive(true);
                self.title_entry.set_text("");
                sync_title_font_button(&self.title_font_btn, "Sans Bold 36");
                self.title_color_btn
                    .set_rgba(&gdk4::RGBA::new(1.0, 1.0, 1.0, 1.0));
                self.title_x_slider.set_value(0.5);
                self.title_y_slider.set_value(0.9);
                self.title_outline_width_slider.set_value(0.0);
                self.title_outline_color_btn
                    .set_rgba(&gdk4::RGBA::new(0.0, 0.0, 0.0, 1.0));
                self.title_shadow_check.set_active(false);
                self.title_shadow_color_btn
                    .set_rgba(&gdk4::RGBA::new(0.0, 0.0, 0.0, 0.67));
                self.title_shadow_x_slider.set_value(2.0);
                self.title_shadow_y_slider.set_value(2.0);
                self.title_bg_box_check.set_active(false);
                self.title_bg_box_color_btn
                    .set_rgba(&gdk4::RGBA::new(0.0, 0.0, 0.0, 0.53));
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
                *self.selected_motion_tracker_id.borrow_mut() = None;
                self.tracking_tracker_dropdown
                    .set_model(Some(&gtk4::StringList::new(&["No trackers yet"])));
                self.tracking_tracker_dropdown.set_selected(0);
                *self.tracking_tracker_ids.borrow_mut() = vec![None];
                self.tracking_add_btn.set_sensitive(false);
                self.tracking_remove_btn.set_sensitive(false);
                self.tracking_label_entry.set_text("");
                self.tracking_label_entry.set_sensitive(false);
                self.tracking_edit_region_btn.set_active(false);
                self.tracking_edit_region_btn.set_sensitive(false);
                self.tracking_center_x_slider.set_value(0.5);
                self.tracking_center_y_slider.set_value(0.5);
                self.tracking_width_slider.set_value(0.25);
                self.tracking_height_slider.set_value(0.25);
                self.tracking_rotation_spin.set_value(0.0);
                self.tracking_center_x_slider.set_sensitive(false);
                self.tracking_center_y_slider.set_sensitive(false);
                self.tracking_width_slider.set_sensitive(false);
                self.tracking_height_slider.set_sensitive(false);
                self.tracking_rotation_spin.set_sensitive(false);
                self.tracking_run_btn.set_label("Track Region");
                self.tracking_run_btn.set_sensitive(false);
                self.tracking_cancel_btn.set_sensitive(false);
                self.tracking_status_label
                    .set_text("Select a visual clip to create or attach motion tracking.");
                self.tracking_target_dropdown
                    .set_model(Some(&gtk4::StringList::new(&["Clip Transform"])));
                self.tracking_target_dropdown.set_selected(0);
                self.tracking_target_dropdown.set_sensitive(false);
                self.tracking_reference_dropdown
                    .set_model(Some(&gtk4::StringList::new(&["None"])));
                self.tracking_reference_dropdown.set_selected(0);
                *self.tracking_reference_choices.borrow_mut() = vec![None];
                self.tracking_clear_binding_btn.set_sensitive(false);
                self.tracking_binding_status_label
                    .set_text("No motion trackers are available in the project yet.");
            }
        }
        *self.updating.borrow_mut() = false;
    }

    /// Update the keyframe indicator label based on the playhead position.
    pub fn update_keyframe_indicator(&self, project: &Project, playhead_ns: u64) {
        let clip = self
            .selected_clip_id
            .borrow()
            .clone()
            .and_then(|id| project.clip_ref(&id).cloned());
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
        let clip = self
            .selected_clip_id
            .borrow()
            .clone()
            .and_then(|id| project.clip_ref(&id).cloned());
        if let Some(c) = clip {
            *self.updating.borrow_mut() = true;
            self.volume_slider.set_value(linear_to_db_volume(
                c.value_for_phase1_property_at_timeline_ns(
                    Phase1KeyframeProperty::Volume,
                    playhead_ns,
                ),
            ));
            self.voice_isolation_slider
                .set_value((c.voice_isolation * 100.0) as f64);
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
    on_color_changed: impl Fn(
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
        ) + 'static,
    on_audio_changed: impl Fn(&str, f32, f32, f32) + 'static,
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
    on_analyze_voice_isolation_silence: impl Fn(&str) + 'static,
    on_suggest_voice_isolation_threshold: impl Fn(&str) -> Option<f32> + 'static,
    on_match_audio: impl Fn(
            &str,
            Option<crate::media::audio_match::AnalysisRegionNs>,
            crate::media::audio_match::AudioMatchChannelMode,
            &str,
            Option<crate::media::audio_match::AnalysisRegionNs>,
            crate::media::audio_match::AudioMatchChannelMode,
        ) + 'static,
    on_duck_changed: impl Fn(&str, bool, f64) + 'static,
    on_role_changed: impl Fn(&str, &str) + 'static,
    on_surround_position_changed: impl Fn(&str, &str) + 'static,
    on_execute_command: impl Fn(Box<dyn crate::undo::EditCommand>) + 'static,
    on_clear_match_eq: impl Fn(&str) + 'static,
    on_request_sam_prompt: impl Fn(Box<dyn Fn(f64, f64, f64, f64) + 'static>) + 'static,
) -> (GBox, Rc<InspectorView>) {
    // Bring transform-bound constants into scope so the slider/range/clamp
    // sites below can use them by short name instead of literal magic
    // numbers.  See `src/model/transform_bounds.rs` for the canonical
    // values.
    use crate::model::transform_bounds::{
        CROP_MAX_PX, CROP_MIN_PX, POSITION_MAX, POSITION_MIN, ROTATE_MAX_DEG, ROTATE_MIN_DEG,
        SCALE_MAX, SCALE_MIN,
    };
    // Wrap frei0r callbacks in Rc so they can be cloned into multiple closures.
    let on_normalize_audio: Rc<dyn Fn(&str)> = Rc::new(on_normalize_audio);
    let on_analyze_voice_isolation_silence: Rc<dyn Fn(&str)> =
        Rc::new(on_analyze_voice_isolation_silence);
    let on_suggest_voice_isolation_threshold: Rc<dyn Fn(&str) -> Option<f32>> =
        Rc::new(on_suggest_voice_isolation_threshold);
    let on_match_audio: Rc<
        dyn Fn(
            &str,
            Option<crate::media::audio_match::AnalysisRegionNs>,
            crate::media::audio_match::AudioMatchChannelMode,
            &str,
            Option<crate::media::audio_match::AnalysisRegionNs>,
            crate::media::audio_match::AudioMatchChannelMode,
        ),
    > = Rc::new(on_match_audio);
    let on_duck_changed: Rc<dyn Fn(&str, bool, f64)> = Rc::new(on_duck_changed);
    let on_role_changed: Rc<dyn Fn(&str, &str)> = Rc::new(on_role_changed);
    let on_surround_position_changed: Rc<dyn Fn(&str, &str)> =
        Rc::new(on_surround_position_changed);
    let on_clear_match_eq: Rc<dyn Fn(&str)> = Rc::new(on_clear_match_eq);
    let on_request_sam_prompt: Rc<dyn Fn(Box<dyn Fn(f64, f64, f64, f64) + 'static>)> =
        Rc::new(on_request_sam_prompt);
    let on_execute_command: Rc<dyn Fn(Box<dyn crate::undo::EditCommand>)> =
        Rc::new(on_execute_command);
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

    // ── Transition section ───────────────────────────────────────────────────
    let transition_section = GBox::new(Orientation::Vertical, 8);

    transition_section.append(&Separator::new(Orientation::Horizontal));
    let transition_expander = Expander::new(Some("Transition"));
    transition_expander.set_expanded(false);
    transition_section.append(&transition_expander);
    let transition_inner = GBox::new(Orientation::Vertical, 8);
    transition_expander.set_child(Some(&transition_inner));

    row_label(&transition_inner, "Type");
    let transition_kind_dropdown = gtk4::ComboBoxText::new();
    transition_kind_dropdown.append(Some(""), "None");
    for transition in supported_transition_definitions() {
        transition_kind_dropdown.append(Some(transition.kind), transition.label);
    }
    transition_kind_dropdown.set_active_id(Some(""));
    transition_kind_dropdown.set_halign(gtk4::Align::Start);
    transition_kind_dropdown.set_hexpand(true);
    transition_inner.append(&transition_kind_dropdown);

    row_label(&transition_inner, "Duration (ms)");
    let transition_duration_ms = gtk4::SpinButton::with_range(
        (MIN_TRANSITION_DURATION_NS as f64) / 1_000_000.0,
        10_000.0,
        10.0,
    );
    transition_duration_ms.set_value((DEFAULT_TRANSITION_DURATION_NS as f64) / 1_000_000.0);
    transition_duration_ms.set_digits(0);
    transition_duration_ms.set_halign(gtk::Align::Start);
    transition_inner.append(&transition_duration_ms);

    row_label(&transition_inner, "Alignment");
    let transition_alignment_dropdown = gtk4::ComboBoxText::new();
    for alignment in TransitionAlignment::ALL {
        transition_alignment_dropdown.append(Some(alignment.as_str()), alignment.label());
    }
    transition_alignment_dropdown.set_active_id(Some(TransitionAlignment::EndOnCut.as_str()));
    transition_alignment_dropdown.set_halign(gtk::Align::Start);
    transition_alignment_dropdown.set_hexpand(true);
    transition_alignment_dropdown.set_tooltip_text(Some(
        "Controls where the overlap sits relative to the cut in preview, export, and background prerender.",
    ));
    transition_inner.append(&transition_alignment_dropdown);

    let transition_clear_btn = Button::with_label("Remove Transition");
    transition_clear_btn.set_halign(gtk::Align::Start);
    transition_clear_btn.add_css_class("small-btn");
    transition_inner.append(&transition_clear_btn);

    let transition_status_label = Label::new(Some(
        "Select a clip with a following clip to edit its outgoing transition.",
    ));
    transition_status_label.set_halign(gtk::Align::Start);
    transition_status_label.set_xalign(0.0);
    transition_status_label.set_wrap(true);
    transition_status_label.add_css_class("dim-label");
    transition_inner.append(&transition_status_label);

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
    let exposure_slider = Scale::with_range(
        Orientation::Horizontal,
        COLOR_SLIDER_MIN,
        COLOR_SLIDER_MAX,
        COLOR_SLIDER_STEP,
    );
    exposure_slider.set_value(0.0);
    exposure_slider.set_draw_value(true);
    exposure_slider.set_digits(2);
    exposure_slider.add_mark(0.0, gtk4::PositionType::Bottom, None);
    color_inner.append(&exposure_slider);

    row_label(&color_inner, "Brightness");
    let brightness_slider = Scale::with_range(
        Orientation::Horizontal,
        COLOR_SLIDER_MIN,
        COLOR_SLIDER_MAX,
        COLOR_SLIDER_STEP,
    );
    brightness_slider.set_value(0.0);
    brightness_slider.set_draw_value(true);
    brightness_slider.set_digits(2);
    brightness_slider.add_mark(0.0, gtk4::PositionType::Bottom, None);
    color_inner.append(&brightness_slider);

    row_label(&color_inner, "Contrast");
    let contrast_slider = Scale::with_range(
        Orientation::Horizontal,
        DOUBLE_SLIDER_MIN,
        DOUBLE_SLIDER_MAX,
        DOUBLE_SLIDER_STEP,
    );
    contrast_slider.set_value(1.0);
    contrast_slider.set_draw_value(true);
    contrast_slider.set_digits(2);
    contrast_slider.add_mark(1.0, gtk4::PositionType::Bottom, None);
    color_inner.append(&contrast_slider);

    row_label(&color_inner, "Saturation");
    let saturation_slider = Scale::with_range(
        Orientation::Horizontal,
        DOUBLE_SLIDER_MIN,
        DOUBLE_SLIDER_MAX,
        DOUBLE_SLIDER_STEP,
    );
    saturation_slider.set_value(1.0);
    saturation_slider.set_draw_value(true);
    saturation_slider.set_digits(2);
    saturation_slider.add_mark(1.0, gtk4::PositionType::Bottom, None);
    color_inner.append(&saturation_slider);

    row_label(&color_inner, "Temperature (K)");
    let temperature_slider = Scale::with_range(
        Orientation::Horizontal,
        COLOR_TEMPERATURE_MIN_K,
        COLOR_TEMPERATURE_MAX_K,
        COLOR_TEMPERATURE_STEP_K,
    );
    temperature_slider.set_value(6500.0);
    temperature_slider.set_draw_value(true);
    temperature_slider.set_digits(0);
    temperature_slider.add_mark(6500.0, gtk4::PositionType::Bottom, None);
    color_inner.append(&temperature_slider);

    row_label(&color_inner, "Tint");
    let tint_slider = Scale::with_range(
        Orientation::Horizontal,
        COLOR_SLIDER_MIN,
        COLOR_SLIDER_MAX,
        COLOR_SLIDER_STEP,
    );
    tint_slider.set_value(0.0);
    tint_slider.set_draw_value(true);
    tint_slider.set_digits(2);
    tint_slider.add_mark(0.0, gtk4::PositionType::Bottom, None);
    color_inner.append(&tint_slider);

    row_label(&color_inner, "Black Point");
    let black_point_slider = Scale::with_range(
        Orientation::Horizontal,
        COLOR_SLIDER_MIN,
        COLOR_SLIDER_MAX,
        COLOR_SLIDER_STEP,
    );
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
    let denoise_slider = Scale::with_range(
        Orientation::Horizontal,
        UNIT_SLIDER_MIN,
        UNIT_SLIDER_MAX,
        UNIT_SLIDER_STEP,
    );
    denoise_slider.set_value(0.0);
    denoise_slider.set_draw_value(true);
    denoise_slider.set_digits(2);
    denoise_slider.add_mark(0.0, gtk4::PositionType::Bottom, None);
    color_inner.append(&denoise_slider);

    row_label(&color_inner, "Sharpness");
    let sharpness_slider = Scale::with_range(
        Orientation::Horizontal,
        COLOR_SLIDER_MIN,
        COLOR_SLIDER_MAX,
        COLOR_SLIDER_STEP,
    );
    sharpness_slider.set_value(0.0);
    sharpness_slider.set_draw_value(true);
    sharpness_slider.set_digits(2);
    sharpness_slider.add_mark(0.0, gtk4::PositionType::Bottom, None);
    color_inner.append(&sharpness_slider);

    row_label(&color_inner, "Blur");
    let blur_slider = Scale::with_range(
        Orientation::Horizontal,
        UNIT_SLIDER_MIN,
        UNIT_SLIDER_MAX,
        UNIT_SLIDER_STEP,
    );
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
    let vidstab_slider = Scale::with_range(
        Orientation::Horizontal,
        UNIT_SLIDER_MIN,
        UNIT_SLIDER_MAX,
        UNIT_SLIDER_STEP,
    );
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
    let shadows_slider = Scale::with_range(
        Orientation::Horizontal,
        COLOR_SLIDER_MIN,
        COLOR_SLIDER_MAX,
        COLOR_SLIDER_STEP,
    );
    shadows_slider.set_value(0.0);
    shadows_slider.set_draw_value(true);
    shadows_slider.set_digits(2);
    shadows_slider.add_mark(0.0, gtk4::PositionType::Bottom, None);
    color_inner.append(&shadows_slider);

    row_label(&color_inner, "Midtones");
    let midtones_slider = Scale::with_range(
        Orientation::Horizontal,
        COLOR_SLIDER_MIN,
        COLOR_SLIDER_MAX,
        COLOR_SLIDER_STEP,
    );
    midtones_slider.set_value(0.0);
    midtones_slider.set_draw_value(true);
    midtones_slider.set_digits(2);
    midtones_slider.add_mark(0.0, gtk4::PositionType::Bottom, None);
    color_inner.append(&midtones_slider);

    row_label(&color_inner, "Highlights");
    let highlights_slider = Scale::with_range(
        Orientation::Horizontal,
        COLOR_SLIDER_MIN,
        COLOR_SLIDER_MAX,
        COLOR_SLIDER_STEP,
    );
    highlights_slider.set_value(0.0);
    highlights_slider.set_draw_value(true);
    highlights_slider.set_digits(2);
    highlights_slider.add_mark(0.0, gtk4::PositionType::Bottom, None);
    color_inner.append(&highlights_slider);

    row_label(&color_inner, "Highlights Warmth");
    let highlights_warmth_slider = Scale::with_range(
        Orientation::Horizontal,
        COLOR_SLIDER_MIN,
        COLOR_SLIDER_MAX,
        COLOR_SLIDER_STEP,
    );
    highlights_warmth_slider.set_value(0.0);
    highlights_warmth_slider.set_draw_value(true);
    highlights_warmth_slider.set_digits(2);
    highlights_warmth_slider.add_mark(0.0, gtk4::PositionType::Bottom, None);
    color_inner.append(&highlights_warmth_slider);

    row_label(&color_inner, "Highlights Tint");
    let highlights_tint_slider = Scale::with_range(
        Orientation::Horizontal,
        COLOR_SLIDER_MIN,
        COLOR_SLIDER_MAX,
        COLOR_SLIDER_STEP,
    );
    highlights_tint_slider.set_value(0.0);
    highlights_tint_slider.set_draw_value(true);
    highlights_tint_slider.set_digits(2);
    highlights_tint_slider.add_mark(0.0, gtk4::PositionType::Bottom, None);
    color_inner.append(&highlights_tint_slider);

    row_label(&color_inner, "Midtones Warmth");
    let midtones_warmth_slider = Scale::with_range(
        Orientation::Horizontal,
        COLOR_SLIDER_MIN,
        COLOR_SLIDER_MAX,
        COLOR_SLIDER_STEP,
    );
    midtones_warmth_slider.set_value(0.0);
    midtones_warmth_slider.set_draw_value(true);
    midtones_warmth_slider.set_digits(2);
    midtones_warmth_slider.add_mark(0.0, gtk4::PositionType::Bottom, None);
    color_inner.append(&midtones_warmth_slider);

    row_label(&color_inner, "Midtones Tint");
    let midtones_tint_slider = Scale::with_range(
        Orientation::Horizontal,
        COLOR_SLIDER_MIN,
        COLOR_SLIDER_MAX,
        COLOR_SLIDER_STEP,
    );
    midtones_tint_slider.set_value(0.0);
    midtones_tint_slider.set_draw_value(true);
    midtones_tint_slider.set_digits(2);
    midtones_tint_slider.add_mark(0.0, gtk4::PositionType::Bottom, None);
    color_inner.append(&midtones_tint_slider);

    row_label(&color_inner, "Shadows Warmth");
    let shadows_warmth_slider = Scale::with_range(
        Orientation::Horizontal,
        COLOR_SLIDER_MIN,
        COLOR_SLIDER_MAX,
        COLOR_SLIDER_STEP,
    );
    shadows_warmth_slider.set_value(0.0);
    shadows_warmth_slider.set_draw_value(true);
    shadows_warmth_slider.set_digits(2);
    shadows_warmth_slider.add_mark(0.0, gtk4::PositionType::Bottom, None);
    color_inner.append(&shadows_warmth_slider);

    row_label(&color_inner, "Shadows Tint");
    let shadows_tint_slider = Scale::with_range(
        Orientation::Horizontal,
        COLOR_SLIDER_MIN,
        COLOR_SLIDER_MAX,
        COLOR_SLIDER_STEP,
    );
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
    let chroma_tolerance_slider = Scale::with_range(
        Orientation::Horizontal,
        UNIT_SLIDER_MIN,
        UNIT_SLIDER_MAX,
        UNIT_SLIDER_STEP,
    );
    chroma_tolerance_slider.set_value(0.3);
    chroma_tolerance_slider.set_draw_value(true);
    chroma_tolerance_slider.set_digits(2);
    chroma_tolerance_slider.add_mark(0.3, gtk4::PositionType::Bottom, None);
    chroma_key_inner.append(&chroma_tolerance_slider);

    row_label(&chroma_key_inner, "Edge Softness");
    let chroma_softness_slider = Scale::with_range(
        Orientation::Horizontal,
        UNIT_SLIDER_MIN,
        UNIT_SLIDER_MAX,
        UNIT_SLIDER_STEP,
    );
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
    let bg_removal_threshold_slider = Scale::with_range(
        Orientation::Horizontal,
        UNIT_SLIDER_MIN,
        UNIT_SLIDER_MAX,
        UNIT_SLIDER_STEP,
    );
    bg_removal_threshold_slider.set_value(0.5);
    bg_removal_threshold_slider.set_draw_value(true);
    bg_removal_threshold_slider.set_digits(2);
    bg_removal_threshold_slider.add_mark(0.5, gtk4::PositionType::Bottom, None);
    bg_removal_inner.append(&bg_removal_threshold_slider);

    // ── Subtitles section (Video + Audio clips) ────────────────────────
    let subtitle_section = GBox::new(Orientation::Vertical, 8);
    content_box.append(&subtitle_section);

    subtitle_section.append(&Separator::new(Orientation::Horizontal));
    let subtitle_expander = Expander::new(Some("Subtitles"));
    subtitle_expander.set_expanded(false);
    subtitle_section.append(&subtitle_expander);
    let subtitle_inner = GBox::new(Orientation::Vertical, 8);
    subtitle_expander.set_child(Some(&subtitle_inner));

    // TextView for compound clips showing aggregated subtitle info from
    // internal clips.  Placed directly in subtitle_section (NOT inside the
    // expander) so it renders without any expander-related layout issues.
    let compound_subtitle_buf = gtk4::TextBuffer::new(None::<&gtk4::TextTagTable>);
    let compound_subtitle_label = gtk4::TextView::with_buffer(&compound_subtitle_buf);
    compound_subtitle_label.set_editable(false);
    compound_subtitle_label.set_cursor_visible(false);
    compound_subtitle_label.set_wrap_mode(gtk4::WrapMode::WordChar);
    compound_subtitle_label.add_css_class("dim-label");
    compound_subtitle_label.set_vexpand(false);
    subtitle_section.append(&compound_subtitle_label);

    // "No model" warning box — shown when whisper model is not installed.
    let subtitle_no_model_box = GBox::new(Orientation::Vertical, 6);
    let no_model_icon_label = Label::new(Some("Speech-to-text model not installed"));
    no_model_icon_label.set_halign(gtk::Align::Start);
    no_model_icon_label.add_css_class("warning");
    subtitle_no_model_box.append(&no_model_icon_label);
    let no_model_hint = Label::new(Some(
        "Download a Whisper GGML model (e.g. ggml-base.en.bin) and place it in the models directory. \
         See Preferences \u{2192} Models for details.",
    ));
    no_model_hint.set_halign(gtk::Align::Start);
    no_model_hint.add_css_class("dim-label");
    no_model_hint.set_wrap(true);
    no_model_hint.set_max_width_chars(40);
    subtitle_no_model_box.append(&no_model_hint);
    subtitle_inner.append(&subtitle_no_model_box);

    // Controls box — shown when model IS installed.
    let subtitle_controls_box = GBox::new(Orientation::Vertical, 8);
    subtitle_inner.append(&subtitle_controls_box);

    // Language selector
    row_label(&subtitle_controls_box, "Language");
    let lang_model = gtk4::StringList::new(&[
        "auto", "en", "es", "fr", "de", "it", "pt", "ja", "zh", "ko", "ru", "ar", "hi",
    ]);
    let subtitle_language_dropdown =
        gtk4::DropDown::new(Some(lang_model), Option::<gtk4::Expression>::None);
    subtitle_language_dropdown.set_selected(0);
    subtitle_controls_box.append(&subtitle_language_dropdown);

    // Generate button with spinner
    let subtitle_generate_btn = Button::new();
    subtitle_generate_btn
        .set_tooltip_text(Some("Run speech-to-text to generate subtitle segments"));
    let gen_btn_box = GBox::new(Orientation::Horizontal, 6);
    gen_btn_box.set_halign(gtk::Align::Center);
    let subtitle_generate_spinner = gtk4::Spinner::new();
    subtitle_generate_spinner.set_visible(false);
    let subtitle_generate_label = Label::new(Some("Generate Subtitles"));
    gen_btn_box.append(&subtitle_generate_spinner);
    gen_btn_box.append(&subtitle_generate_label);
    subtitle_generate_btn.set_child(Some(&gen_btn_box));
    subtitle_controls_box.append(&subtitle_generate_btn);

    // Error label (hidden by default)
    let subtitle_error_label = Label::new(None);
    subtitle_error_label.set_halign(gtk::Align::Start);
    subtitle_error_label.add_css_class("error");
    subtitle_error_label.set_wrap(true);
    subtitle_error_label.set_max_width_chars(40);
    subtitle_error_label.set_visible(false);
    subtitle_controls_box.append(&subtitle_error_label);

    // ── Style controls (visible when subtitles exist) ────────────────
    let subtitle_style_box = GBox::new(Orientation::Vertical, 6);
    subtitle_controls_box.append(&subtitle_style_box);

    // Render-subtitles toggle: hides this clip's subtitles from the
    // preview overlay, export burn-in, and SRT sidecar without
    // touching the underlying segment data. The transcript editor and
    // voice isolation (Subtitles source) keep working when off.
    let sub_visible_check = CheckButton::with_label("Render subtitles");
    sub_visible_check.set_tooltip_text(Some(
        "When off, subtitles are hidden from the preview, export burn-in, \
         and SRT sidecar. The transcript editor and voice isolation still \
         use the segment data.",
    ));
    sub_visible_check.set_active(true);
    subtitle_style_box.append(&sub_visible_check);

    subtitle_style_box.append(&Separator::new(Orientation::Horizontal));
    let style_label = Label::new(Some("Style"));
    style_label.set_halign(gtk::Align::Start);
    style_label.add_css_class("heading");
    subtitle_style_box.append(&style_label);

    row_label(&subtitle_style_box, "Font");
    let subtitle_font_btn = gtk4::Button::with_label("Sans Bold 24");
    sync_subtitle_font_button(&subtitle_font_btn, "Sans Bold 24");
    subtitle_style_box.append(&subtitle_font_btn);

    // Base style toggle buttons
    let subtitle_style_row = GBox::new(Orientation::Horizontal, 4);
    let sub_bold_btn = gtk4::ToggleButton::with_label("B");
    sub_bold_btn.set_tooltip_text(Some("Bold"));
    let sub_italic_btn = gtk4::ToggleButton::with_label("I");
    sub_italic_btn.set_tooltip_text(Some("Italic"));
    let sub_underline_btn = gtk4::ToggleButton::with_label("U");
    sub_underline_btn.set_tooltip_text(Some("Underline"));
    let sub_shadow_btn = gtk4::ToggleButton::with_label("S");
    sub_shadow_btn.set_tooltip_text(Some("Shadow"));
    subtitle_style_row.append(&sub_bold_btn);
    subtitle_style_row.append(&sub_italic_btn);
    subtitle_style_row.append(&sub_underline_btn);
    subtitle_style_row.append(&sub_shadow_btn);
    subtitle_style_box.append(&subtitle_style_row);

    row_label(&subtitle_style_box, "Text Color");
    let sub_color_dialog = gtk4::ColorDialog::new();
    sub_color_dialog.set_with_alpha(true);
    let subtitle_color_btn = gtk4::ColorDialogButton::new(Some(sub_color_dialog));
    subtitle_color_btn.set_rgba(&gdk4::RGBA::new(1.0, 1.0, 1.0, 1.0));
    subtitle_style_box.append(&subtitle_color_btn);

    row_label(&subtitle_style_box, "Word Highlight");
    // Keep the dropdown for legacy compat (hidden), but drive UI from checkboxes.
    let highlight_model = gtk4::StringList::new(&["None", "Bold", "Color", "Underline", "Stroke"]);
    let subtitle_highlight_dropdown =
        gtk4::DropDown::new(Some(highlight_model), Option::<gtk4::Expression>::None);
    subtitle_highlight_dropdown.set_selected(0);
    subtitle_highlight_dropdown.set_visible(false);

    let hl_checks_box = GBox::new(Orientation::Horizontal, 4);
    hl_checks_box.set_halign(gtk::Align::Start);
    let hl_bold_check = CheckButton::with_label("Bold");
    let hl_color_check = CheckButton::with_label("Color");
    let hl_underline_check = CheckButton::with_label("Underline");
    let hl_stroke_check = CheckButton::with_label("Stroke");
    hl_checks_box.append(&hl_bold_check);
    hl_checks_box.append(&hl_color_check);
    hl_checks_box.append(&hl_underline_check);
    hl_checks_box.append(&hl_stroke_check);
    let hl_checks_box2 = GBox::new(Orientation::Horizontal, 4);
    hl_checks_box2.set_halign(gtk::Align::Start);
    let hl_italic_check = CheckButton::with_label("Italic");
    let hl_bg_check = CheckButton::with_label("Background");
    let hl_shadow_check = CheckButton::with_label("Shadow");
    hl_checks_box2.append(&hl_italic_check);
    hl_checks_box2.append(&hl_bg_check);
    hl_checks_box2.append(&hl_shadow_check);
    subtitle_style_box.append(&hl_checks_box);
    subtitle_style_box.append(&hl_checks_box2);

    let subtitle_highlight_color_row = GBox::new(Orientation::Vertical, 4);
    row_label(&subtitle_highlight_color_row, "Highlight Color");
    let sub_hl_color_dialog = gtk4::ColorDialog::new();
    sub_hl_color_dialog.set_with_alpha(true);
    let subtitle_highlight_color_btn = gtk4::ColorDialogButton::new(Some(sub_hl_color_dialog));
    subtitle_highlight_color_btn.set_rgba(&gdk4::RGBA::new(1.0, 1.0, 0.0, 1.0));
    subtitle_highlight_color_row.append(&subtitle_highlight_color_btn);
    subtitle_style_box.append(&subtitle_highlight_color_row);

    // Independent stroke colour for the karaoke stroke effect — only
    // visible when the Stroke highlight flag is enabled. Defaults to the
    // same value as the highlight colour for legacy projects.
    let subtitle_highlight_stroke_color_row = GBox::new(Orientation::Vertical, 4);
    row_label(&subtitle_highlight_stroke_color_row, "Highlight Stroke Color");
    let sub_hl_stroke_color_dialog = gtk4::ColorDialog::new();
    sub_hl_stroke_color_dialog.set_with_alpha(true);
    let subtitle_highlight_stroke_color_btn =
        gtk4::ColorDialogButton::new(Some(sub_hl_stroke_color_dialog));
    subtitle_highlight_stroke_color_btn.set_rgba(&gdk4::RGBA::new(0.0, 0.0, 0.0, 1.0));
    subtitle_highlight_stroke_color_row.append(&subtitle_highlight_stroke_color_btn);
    subtitle_highlight_stroke_color_row.set_visible(false);
    subtitle_style_box.append(&subtitle_highlight_stroke_color_row);

    let subtitle_bg_highlight_color_row = GBox::new(Orientation::Vertical, 4);
    row_label(&subtitle_bg_highlight_color_row, "BG Highlight Color");
    let sub_bg_hl_color_dialog = gtk4::ColorDialog::new();
    sub_bg_hl_color_dialog.set_with_alpha(true);
    let subtitle_bg_highlight_color_btn =
        gtk4::ColorDialogButton::new(Some(sub_bg_hl_color_dialog));
    subtitle_bg_highlight_color_btn.set_rgba(&gdk4::RGBA::new(1.0, 1.0, 0.0, 0.5));
    subtitle_bg_highlight_color_row.append(&subtitle_bg_highlight_color_btn);
    subtitle_bg_highlight_color_row.set_visible(false);
    subtitle_style_box.append(&subtitle_bg_highlight_color_row);

    let subtitle_word_window_slider = Scale::with_range(Orientation::Horizontal, 2.0, 10.0, 1.0);
    subtitle_word_window_slider.set_value(4.0);
    subtitle_word_window_slider.set_draw_value(true);
    subtitle_word_window_slider.set_digits(0);
    subtitle_word_window_slider.add_mark(4.0, gtk4::PositionType::Bottom, None);
    subtitle_word_window_slider
        .set_tooltip_text(Some("Number of words shown per group in highlight mode"));
    subtitle_style_box.append(&subtitle_word_window_slider);

    row_label(&subtitle_style_box, "Vertical Position");
    let subtitle_position_slider = Scale::with_range(Orientation::Horizontal, 0.05, 0.95, 0.05);
    subtitle_position_slider.set_value(0.85);
    subtitle_position_slider.set_draw_value(true);
    subtitle_position_slider.set_digits(2);
    subtitle_position_slider.add_mark(0.85, gtk4::PositionType::Bottom, None);
    subtitle_position_slider.add_mark(0.10, gtk4::PositionType::Bottom, Some("Top"));
    subtitle_position_slider.add_mark(0.50, gtk4::PositionType::Bottom, Some("Mid"));
    subtitle_position_slider.set_tooltip_text(Some("Vertical position: 0 = top, 1 = bottom"));
    subtitle_style_box.append(&subtitle_position_slider);

    row_label(&subtitle_style_box, "Outline Color");
    let sub_outline_color_dialog = gtk4::ColorDialog::new();
    sub_outline_color_dialog.set_with_alpha(true);
    let subtitle_outline_color_btn = gtk4::ColorDialogButton::new(Some(sub_outline_color_dialog));
    subtitle_outline_color_btn.set_rgba(&gdk4::RGBA::new(0.0, 0.0, 0.0, 1.0));
    subtitle_style_box.append(&subtitle_outline_color_btn);

    let subtitle_bg_box_check = CheckButton::with_label("Background Box");
    subtitle_bg_box_check.set_active(true);
    subtitle_style_box.append(&subtitle_bg_box_check);

    row_label(&subtitle_style_box, "Background Color");
    let sub_bg_color_dialog = gtk4::ColorDialog::new();
    sub_bg_color_dialog.set_with_alpha(true);
    let subtitle_bg_color_btn = gtk4::ColorDialogButton::new(Some(sub_bg_color_dialog));
    subtitle_bg_color_btn.set_rgba(&gdk4::RGBA::new(0.0, 0.0, 0.0, 0.6));
    subtitle_style_box.append(&subtitle_bg_color_btn);

    // Copy/Paste Style buttons
    let style_clipboard: Rc<RefCell<Option<SubtitleStyleClipboard>>> = Rc::new(RefCell::new(None));
    let subtitle_copy_paste_box = GBox::new(Orientation::Horizontal, 4);
    let subtitle_copy_style_btn = Button::with_label("Copy Style");
    subtitle_copy_style_btn.set_hexpand(true);
    subtitle_copy_style_btn.set_tooltip_text(Some("Copy this clip's subtitle style"));
    let subtitle_paste_style_btn = Button::with_label("Paste Style");
    subtitle_paste_style_btn.set_hexpand(true);
    subtitle_paste_style_btn.set_sensitive(false);
    subtitle_paste_style_btn.set_tooltip_text(Some("Apply copied subtitle style to this clip"));
    subtitle_copy_paste_box.append(&subtitle_copy_style_btn);
    subtitle_copy_paste_box.append(&subtitle_paste_style_btn);
    subtitle_style_box.append(&subtitle_copy_paste_box);

    // Copy/Paste signal handlers are wired in window.rs where timeline_state is available.

    // Action buttons row
    let subtitle_actions_box = GBox::new(Orientation::Horizontal, 4);
    subtitle_controls_box.append(&subtitle_actions_box);

    let subtitle_clear_btn = Button::with_label("Clear Subtitles");
    subtitle_clear_btn.add_css_class("destructive-action");
    subtitle_clear_btn.set_hexpand(true);
    subtitle_actions_box.append(&subtitle_clear_btn);

    let subtitle_export_srt_btn = Button::with_label("Export SRT");
    subtitle_export_srt_btn.set_hexpand(true);
    subtitle_export_srt_btn.set_tooltip_text(Some("Export all subtitles as an SRT file"));
    subtitle_actions_box.append(&subtitle_export_srt_btn);

    let subtitle_import_srt_btn = Button::with_label("Import SRT");
    subtitle_import_srt_btn.set_hexpand(true);
    subtitle_import_srt_btn.set_tooltip_text(Some(
        "Import an SRT file as subtitle segments for this clip",
    ));
    subtitle_actions_box.append(&subtitle_import_srt_btn);

    // ── Subtitle Segments section (separate expander for editing) ─────
    let subtitle_segments_section = GBox::new(Orientation::Vertical, 8);
    content_box.append(&subtitle_segments_section);

    subtitle_segments_section.append(&Separator::new(Orientation::Horizontal));
    let segments_expander = Expander::new(Some("Subtitle Segments"));
    segments_expander.set_expanded(false);
    subtitle_segments_section.append(&segments_expander);

    let subtitle_list_box = GBox::new(Orientation::Vertical, 2);
    segments_expander.set_child(Some(&subtitle_list_box));

    // ── Shape Mask section (Video + Image + Title only) ──────────────
    let mask_section = GBox::new(Orientation::Vertical, 8);
    content_box.append(&mask_section);

    mask_section.append(&Separator::new(Orientation::Horizontal));
    let mask_expander = Expander::new(Some("Shape Mask"));
    mask_expander.set_expanded(false);
    mask_section.append(&mask_expander);
    let mask_inner = GBox::new(Orientation::Vertical, 8);
    mask_expander.set_child(Some(&mask_inner));

    let mask_enable = CheckButton::with_label("Enable Mask");
    mask_inner.append(&mask_enable);

    row_label(&mask_inner, "Shape");
    let mask_shape_model = gtk4::StringList::new(&["Rectangle", "Ellipse", "Path"]);
    let mask_shape_dropdown =
        gtk4::DropDown::new(Some(mask_shape_model), Option::<gtk4::Expression>::None);
    mask_shape_dropdown.set_selected(0);
    mask_inner.append(&mask_shape_dropdown);

    // Rect/Ellipse controls container
    let mask_rect_ellipse_controls = GBox::new(Orientation::Vertical, 8);
    mask_inner.append(&mask_rect_ellipse_controls);

    row_label(&mask_rect_ellipse_controls, "Center X");
    let mask_center_x_slider = Scale::with_range(
        Orientation::Horizontal,
        UNIT_SLIDER_MIN,
        UNIT_SLIDER_MAX,
        UNIT_SLIDER_STEP,
    );
    mask_center_x_slider.set_value(0.5);
    mask_center_x_slider.set_draw_value(true);
    mask_center_x_slider.set_digits(2);
    mask_center_x_slider.add_mark(0.5, gtk4::PositionType::Bottom, None);
    mask_rect_ellipse_controls.append(&mask_center_x_slider);

    row_label(&mask_rect_ellipse_controls, "Center Y");
    let mask_center_y_slider = Scale::with_range(
        Orientation::Horizontal,
        UNIT_SLIDER_MIN,
        UNIT_SLIDER_MAX,
        UNIT_SLIDER_STEP,
    );
    mask_center_y_slider.set_value(0.5);
    mask_center_y_slider.set_draw_value(true);
    mask_center_y_slider.set_digits(2);
    mask_center_y_slider.add_mark(0.5, gtk4::PositionType::Bottom, None);
    mask_rect_ellipse_controls.append(&mask_center_y_slider);

    row_label(&mask_rect_ellipse_controls, "Width");
    let mask_width_slider = Scale::with_range(Orientation::Horizontal, 0.01, 0.5, 0.01);
    mask_width_slider.set_value(0.25);
    mask_width_slider.set_draw_value(true);
    mask_width_slider.set_digits(2);
    mask_rect_ellipse_controls.append(&mask_width_slider);

    row_label(&mask_rect_ellipse_controls, "Height");
    let mask_height_slider = Scale::with_range(Orientation::Horizontal, 0.01, 0.5, 0.01);
    mask_height_slider.set_value(0.25);
    mask_height_slider.set_draw_value(true);
    mask_height_slider.set_digits(2);
    mask_rect_ellipse_controls.append(&mask_height_slider);

    row_label(&mask_rect_ellipse_controls, "Rotation");
    let mask_rotation_spin = gtk4::SpinButton::with_range(-180.0, 180.0, 1.0);
    mask_rotation_spin.set_value(0.0);
    mask_rotation_spin.set_digits(0);
    mask_rect_ellipse_controls.append(&mask_rotation_spin);

    row_label(&mask_rect_ellipse_controls, "Feather");
    let mask_feather_slider = Scale::with_range(Orientation::Horizontal, 0.0, 0.5, 0.01);
    mask_feather_slider.set_value(0.0);
    mask_feather_slider.set_draw_value(true);
    mask_feather_slider.set_digits(2);
    mask_rect_ellipse_controls.append(&mask_feather_slider);

    row_label(&mask_rect_ellipse_controls, "Expansion");
    let mask_expansion_slider = Scale::with_range(Orientation::Horizontal, -0.5, 0.5, 0.01);
    mask_expansion_slider.set_value(0.0);
    mask_expansion_slider.set_draw_value(true);
    mask_expansion_slider.set_digits(2);
    mask_expansion_slider.add_mark(0.0, gtk4::PositionType::Bottom, None);
    mask_rect_ellipse_controls.append(&mask_expansion_slider);

    let mask_invert_check = CheckButton::with_label("Invert Mask");
    mask_rect_ellipse_controls.append(&mask_invert_check);

    // Path editor controls container (initially hidden)
    let mask_path_editor_box = GBox::new(Orientation::Vertical, 8);
    mask_path_editor_box.set_visible(false);
    mask_inner.append(&mask_path_editor_box);

    let path_points_label = Label::new(Some("Path Points"));
    path_points_label.set_halign(gtk4::Align::Start);
    path_points_label.add_css_class("clip-path");
    mask_path_editor_box.append(&path_points_label);

    let add_point_btn = Button::with_label("Add Point");
    mask_path_editor_box.append(&add_point_btn);

    // ── "Generate with SAM" button (Phase 2b/2) ──────────────────────
    //
    // A separator + button row at the bottom of the Shape Mask panel.
    // Runs SAM 3 against a hardcoded centre-region box prompt and
    // replaces `masks[0]` with the resulting bezier polygon. Real
    // drag-to-box UI on the Program Monitor lands in Phase 2b/3.
    //
    // The button is always created so populate code can touch it
    // without cfg gates, but the click handler + visibility are only
    // wired when the `ai-inference` feature is enabled. Without the
    // feature the button stays hidden.
    mask_inner.append(&Separator::new(Orientation::Horizontal));
    let sam_generate_btn = Button::with_label("Generate with SAM");
    sam_generate_btn.set_tooltip_text(Some(
        "Run SAM 3 on the centre of the clip's first visible frame \
         and replace the existing mask.",
    ));
    // Disabled until the populate path sees a compatible clip AND the
    // SAM model is installed. Phase 2b/3 will switch this to enable
    // only after the user has drawn a box on the Program Monitor.
    sam_generate_btn.set_sensitive(false);
    #[cfg(not(feature = "ai-inference"))]
    sam_generate_btn.set_visible(false);
    mask_inner.append(&sam_generate_btn);

    // ── HSL Qualifier section (secondary color correction) ────────────
    let hsl_section = GBox::new(Orientation::Vertical, 8);
    content_box.append(&hsl_section);
    hsl_section.append(&Separator::new(Orientation::Horizontal));
    let hsl_expander = Expander::new(Some("HSL Qualifier"));
    hsl_expander.set_expanded(false);
    hsl_section.append(&hsl_expander);
    let hsl_inner = GBox::new(Orientation::Vertical, 8);
    hsl_expander.set_child(Some(&hsl_inner));

    // Enable / invert / view-mask toggles.
    let hsl_toggles_row = GBox::new(Orientation::Horizontal, 8);
    let hsl_enable = CheckButton::with_label("Enable");
    let hsl_invert = CheckButton::with_label("Invert");
    let hsl_view_mask = CheckButton::with_label("View Mask");
    hsl_toggles_row.append(&hsl_enable);
    hsl_toggles_row.append(&hsl_invert);
    hsl_toggles_row.append(&hsl_view_mask);
    hsl_inner.append(&hsl_toggles_row);

    // Range subgroup.
    let hsl_range_label = Label::new(Some("Range"));
    hsl_range_label.set_halign(gtk::Align::Start);
    hsl_range_label.add_css_class("browser-header");
    hsl_inner.append(&hsl_range_label);

    row_label(&hsl_inner, "Hue Min");
    let hsl_hue_min = Scale::with_range(Orientation::Horizontal, 0.0, 360.0, 1.0);
    hsl_hue_min.set_value(0.0);
    hsl_hue_min.set_draw_value(true);
    hsl_hue_min.set_digits(0);
    hsl_inner.append(&hsl_hue_min);

    row_label(&hsl_inner, "Hue Max");
    let hsl_hue_max = Scale::with_range(Orientation::Horizontal, 0.0, 360.0, 1.0);
    hsl_hue_max.set_value(360.0);
    hsl_hue_max.set_draw_value(true);
    hsl_hue_max.set_digits(0);
    hsl_inner.append(&hsl_hue_max);

    row_label(&hsl_inner, "Hue Softness");
    let hsl_hue_softness = Scale::with_range(Orientation::Horizontal, 0.0, 60.0, 1.0);
    hsl_hue_softness.set_value(0.0);
    hsl_hue_softness.set_draw_value(true);
    hsl_hue_softness.set_digits(0);
    hsl_inner.append(&hsl_hue_softness);

    row_label(&hsl_inner, "Sat Min");
    let hsl_sat_min = Scale::with_range(Orientation::Horizontal, 0.0, 1.0, 0.01);
    hsl_sat_min.set_value(0.0);
    hsl_sat_min.set_draw_value(true);
    hsl_sat_min.set_digits(2);
    hsl_inner.append(&hsl_sat_min);

    row_label(&hsl_inner, "Sat Max");
    let hsl_sat_max = Scale::with_range(Orientation::Horizontal, 0.0, 1.0, 0.01);
    hsl_sat_max.set_value(1.0);
    hsl_sat_max.set_draw_value(true);
    hsl_sat_max.set_digits(2);
    hsl_inner.append(&hsl_sat_max);

    row_label(&hsl_inner, "Sat Softness");
    let hsl_sat_softness = Scale::with_range(Orientation::Horizontal, 0.0, 0.5, 0.01);
    hsl_sat_softness.set_value(0.0);
    hsl_sat_softness.set_draw_value(true);
    hsl_sat_softness.set_digits(2);
    hsl_inner.append(&hsl_sat_softness);

    row_label(&hsl_inner, "Lum Min");
    let hsl_lum_min = Scale::with_range(Orientation::Horizontal, 0.0, 1.0, 0.01);
    hsl_lum_min.set_value(0.0);
    hsl_lum_min.set_draw_value(true);
    hsl_lum_min.set_digits(2);
    hsl_inner.append(&hsl_lum_min);

    row_label(&hsl_inner, "Lum Max");
    let hsl_lum_max = Scale::with_range(Orientation::Horizontal, 0.0, 1.0, 0.01);
    hsl_lum_max.set_value(1.0);
    hsl_lum_max.set_draw_value(true);
    hsl_lum_max.set_digits(2);
    hsl_inner.append(&hsl_lum_max);

    row_label(&hsl_inner, "Lum Softness");
    let hsl_lum_softness = Scale::with_range(Orientation::Horizontal, 0.0, 0.5, 0.01);
    hsl_lum_softness.set_value(0.0);
    hsl_lum_softness.set_draw_value(true);
    hsl_lum_softness.set_digits(2);
    hsl_inner.append(&hsl_lum_softness);

    // Secondary grade subgroup.
    let hsl_grade_label = Label::new(Some("Secondary Grade"));
    hsl_grade_label.set_halign(gtk::Align::Start);
    hsl_grade_label.add_css_class("browser-header");
    hsl_inner.append(&hsl_grade_label);

    row_label(&hsl_inner, "Brightness");
    let hsl_brightness = Scale::with_range(Orientation::Horizontal, -1.0, 1.0, 0.01);
    hsl_brightness.set_value(0.0);
    hsl_brightness.set_draw_value(true);
    hsl_brightness.set_digits(2);
    hsl_brightness.add_mark(0.0, gtk4::PositionType::Bottom, None);
    hsl_inner.append(&hsl_brightness);

    row_label(&hsl_inner, "Contrast");
    let hsl_contrast = Scale::with_range(Orientation::Horizontal, 0.0, 2.0, 0.01);
    hsl_contrast.set_value(1.0);
    hsl_contrast.set_draw_value(true);
    hsl_contrast.set_digits(2);
    hsl_contrast.add_mark(1.0, gtk4::PositionType::Bottom, None);
    hsl_inner.append(&hsl_contrast);

    row_label(&hsl_inner, "Saturation");
    let hsl_saturation = Scale::with_range(Orientation::Horizontal, 0.0, 2.0, 0.01);
    hsl_saturation.set_value(1.0);
    hsl_saturation.set_draw_value(true);
    hsl_saturation.set_digits(2);
    hsl_saturation.add_mark(1.0, gtk4::PositionType::Bottom, None);
    hsl_inner.append(&hsl_saturation);

    // Create shared state needed by effects and later sections.
    let selected_clip_id: Rc<RefCell<Option<String>>> = Rc::new(RefCell::new(None));
    let selected_motion_tracker_id: Rc<RefCell<Option<String>>> = Rc::new(RefCell::new(None));
    let tracking_tracker_ids: Rc<RefCell<Vec<Option<String>>>> = Rc::new(RefCell::new(vec![None]));
    let tracking_reference_choices: Rc<RefCell<Vec<Option<MotionTrackerReference>>>> =
        Rc::new(RefCell::new(vec![None]));

    // ── Motion Tracking section ───────────────────────────────────────────────
    let tracking_section = GBox::new(Orientation::Vertical, 8);
    content_box.append(&tracking_section);
    tracking_section.append(&Separator::new(Orientation::Horizontal));
    let tracking_expander = Expander::new(Some("Motion Tracking"));
    tracking_expander.set_expanded(false);
    tracking_section.append(&tracking_expander);
    let tracking_inner = GBox::new(Orientation::Vertical, 8);
    tracking_expander.set_child(Some(&tracking_inner));

    row_label(&tracking_inner, "Tracker");
    let tracking_tracker_row = GBox::new(Orientation::Horizontal, 6);
    let tracking_tracker_dropdown = gtk4::DropDown::new(
        Some(gtk4::StringList::new(&["No trackers yet"])),
        Option::<gtk4::Expression>::None,
    );
    tracking_tracker_dropdown.set_hexpand(true);
    tracking_tracker_row.append(&tracking_tracker_dropdown);
    let tracking_add_btn = Button::from_icon_name("list-add-symbolic");
    tracking_add_btn.set_tooltip_text(Some("Add a motion tracker to this clip"));
    tracking_add_btn.add_css_class("flat");
    tracking_tracker_row.append(&tracking_add_btn);
    let tracking_remove_btn = Button::from_icon_name("edit-delete-symbolic");
    tracking_remove_btn.set_tooltip_text(Some("Remove the selected motion tracker"));
    tracking_remove_btn.add_css_class("flat");
    tracking_tracker_row.append(&tracking_remove_btn);
    tracking_inner.append(&tracking_tracker_row);

    row_label(&tracking_inner, "Label");
    let tracking_label_entry = Entry::new();
    tracking_label_entry.set_placeholder_text(Some("Tracker label"));
    tracking_inner.append(&tracking_label_entry);

    let tracking_edit_region_btn = gtk4::ToggleButton::with_label("Edit Region in Monitor");
    tracking_edit_region_btn.set_tooltip_text(Some(
        "Show the analysis rectangle in the Program Monitor and drag it to position the tracker",
    ));
    tracking_inner.append(&tracking_edit_region_btn);

    row_label(&tracking_inner, "Region Center X");
    let tracking_center_x_slider = Scale::with_range(
        Orientation::Horizontal,
        UNIT_SLIDER_MIN,
        UNIT_SLIDER_MAX,
        UNIT_SLIDER_STEP,
    );
    tracking_center_x_slider.set_value(0.5);
    tracking_center_x_slider.set_draw_value(true);
    tracking_center_x_slider.set_digits(2);
    tracking_center_x_slider.add_mark(0.5, gtk4::PositionType::Bottom, None);
    tracking_inner.append(&tracking_center_x_slider);

    row_label(&tracking_inner, "Region Center Y");
    let tracking_center_y_slider = Scale::with_range(
        Orientation::Horizontal,
        UNIT_SLIDER_MIN,
        UNIT_SLIDER_MAX,
        UNIT_SLIDER_STEP,
    );
    tracking_center_y_slider.set_value(0.5);
    tracking_center_y_slider.set_draw_value(true);
    tracking_center_y_slider.set_digits(2);
    tracking_center_y_slider.add_mark(0.5, gtk4::PositionType::Bottom, None);
    tracking_inner.append(&tracking_center_y_slider);

    row_label(&tracking_inner, "Region Width");
    let tracking_width_slider = Scale::with_range(Orientation::Horizontal, 0.05, 1.0, 0.01);
    tracking_width_slider.set_value(0.25);
    tracking_width_slider.set_draw_value(true);
    tracking_width_slider.set_digits(2);
    tracking_inner.append(&tracking_width_slider);

    row_label(&tracking_inner, "Region Height");
    let tracking_height_slider = Scale::with_range(Orientation::Horizontal, 0.05, 1.0, 0.01);
    tracking_height_slider.set_value(0.25);
    tracking_height_slider.set_draw_value(true);
    tracking_height_slider.set_digits(2);
    tracking_inner.append(&tracking_height_slider);

    row_label(&tracking_inner, "Region Rotation");
    let tracking_rotation_spin = gtk4::SpinButton::with_range(-180.0, 180.0, 1.0);
    tracking_rotation_spin.set_value(0.0);
    tracking_rotation_spin.set_digits(0);
    tracking_inner.append(&tracking_rotation_spin);

    let tracking_job_row = GBox::new(Orientation::Horizontal, 6);
    let tracking_run_btn = Button::with_label("Track Region");
    tracking_run_btn.set_tooltip_text(Some(
        "Analyze the selected clip and generate motion samples for the current region",
    ));
    tracking_job_row.append(&tracking_run_btn);
    let tracking_cancel_btn = Button::with_label("Cancel");
    tracking_cancel_btn.set_sensitive(false);
    tracking_job_row.append(&tracking_cancel_btn);
    tracking_inner.append(&tracking_job_row);

    let tracking_auto_crop_btn = Button::with_label("Auto-Crop to Project Aspect");
    tracking_auto_crop_btn.set_tooltip_text(Some(
        "Run the tracker and reframe the clip so the tracked region fills the project frame at the project's aspect ratio (e.g. horizontal → vertical). Undoable — use Ctrl+Z to revert, or Clear Attachment to remove",
    ));
    tracking_auto_crop_btn.set_sensitive(false);
    tracking_inner.append(&tracking_auto_crop_btn);

    row_label(&tracking_inner, "Crop Padding");
    let tracking_auto_crop_padding_slider =
        Scale::with_range(Orientation::Horizontal, 0.0, 0.5, 0.05);
    tracking_auto_crop_padding_slider.set_value(0.1);
    tracking_auto_crop_padding_slider.set_draw_value(true);
    tracking_auto_crop_padding_slider.set_digits(2);
    tracking_auto_crop_padding_slider.set_tooltip_text(Some(
        "Extra headroom around the tracked region (0 = tight crop, 0.5 = generous margin). Takes effect the next time you click Auto-Crop, or immediately if an auto-crop is already active",
    ));
    tracking_auto_crop_padding_slider.set_sensitive(false);
    tracking_inner.append(&tracking_auto_crop_padding_slider);

    let tracking_status_label = Label::new(Some(
        "Select a visual clip to create or attach motion tracking.",
    ));
    tracking_status_label.set_wrap(true);
    tracking_status_label.set_halign(gtk4::Align::Start);
    tracking_status_label.add_css_class("dim-label");
    tracking_inner.append(&tracking_status_label);

    tracking_inner.append(&Separator::new(Orientation::Horizontal));

    row_label(&tracking_inner, "Attach To");
    let tracking_target_dropdown = gtk4::DropDown::new(
        Some(gtk4::StringList::new(&["Clip Transform"])),
        Option::<gtk4::Expression>::None,
    );
    tracking_target_dropdown.set_selected(0);
    tracking_target_dropdown.set_sensitive(false);
    tracking_inner.append(&tracking_target_dropdown);

    row_label(&tracking_inner, "Follow Tracker");
    let tracking_reference_dropdown = gtk4::DropDown::new(
        Some(gtk4::StringList::new(&["None"])),
        Option::<gtk4::Expression>::None,
    );
    tracking_reference_dropdown.set_selected(0);
    tracking_inner.append(&tracking_reference_dropdown);

    let tracking_clear_binding_btn = Button::with_label("Clear Attachment");
    tracking_clear_binding_btn.set_sensitive(false);
    tracking_inner.append(&tracking_clear_binding_btn);

    let tracking_binding_status_label =
        Label::new(Some("No motion trackers are available in the project yet."));
    tracking_binding_status_label.set_wrap(true);
    tracking_binding_status_label.set_halign(gtk4::Align::Start);
    tracking_binding_status_label.add_css_class("dim-label");
    tracking_inner.append(&tracking_binding_status_label);

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

    let frei0r_empty_label = Label::new(Some(
        "No effects applied.\nUse the Effects tab to add frei0r filters.",
    ));
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
                if let Some(clip) = proj.clip_ref(&cid) {
                    let copied: Vec<crate::model::clip::Frei0rEffect> =
                        clip.frei0r_effects.iter().cloned().collect();
                    let has_effects = !copied.is_empty();
                    *clipboard.borrow_mut() = Some(copied);
                    paste_btn.set_sensitive(has_effects);
                    return;
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
                let track_id = project
                    .borrow()
                    .find_track_id_for_clip(&cid)
                    .unwrap_or_default();
                let insert_index = {
                    let proj = project.borrow();
                    proj.tracks
                        .iter()
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

    // ── Enhance Voice (one-knob FFmpeg chain, applied before voice isolation) ──
    let voice_enhance_check = CheckButton::with_label("Enhance Voice");
    voice_enhance_check.set_tooltip_text(Some(
        "Apply a one-knob cleanup chain (high-pass, FFT noise reduction, \
         presence EQ, gentle compressor) to this clip's audio. Runs \
         before Voice Isolation. Preview goes through a background \
         prerender — the first time you toggle on or change strength, \
         ffmpeg generates a sidecar file in the background and the \
         status bar shows progress. Preview and export are byte-identical.",
    ));
    audio_inner.append(&voice_enhance_check);

    row_label(&audio_inner, "  Strength");
    let voice_enhance_strength_slider =
        Scale::with_range(Orientation::Horizontal, 0.0, 100.0, 1.0);
    voice_enhance_strength_slider.set_value(50.0);
    voice_enhance_strength_slider.set_draw_value(true);
    voice_enhance_strength_slider.set_digits(0);
    voice_enhance_strength_slider.add_mark(0.0, gtk4::PositionType::Bottom, Some("Subtle"));
    voice_enhance_strength_slider.add_mark(100.0, gtk4::PositionType::Bottom, Some("Strong"));
    voice_enhance_strength_slider.set_sensitive(false);
    voice_enhance_strength_slider.set_tooltip_text(Some(
        "Scales noise-reduction depth, presence boost, and compression. \
         0 = subtle clean-up, 100 = broadcast voice.",
    ));
    audio_inner.append(&voice_enhance_strength_slider);

    // Per-clip cache status row: shows whether the prerender is ready,
    // running, or failed for the current strength. Updated by the
    // window.rs poll loop from `VoiceEnhanceCache::status()`. The
    // **Retry** button is only visible when the cache reports `Failed`.
    let voice_enhance_status_row = GBox::new(Orientation::Horizontal, 6);
    voice_enhance_status_row.set_margin_top(2);
    let voice_enhance_status_label = Label::new(None);
    voice_enhance_status_label.set_halign(gtk::Align::Start);
    voice_enhance_status_label.add_css_class("dim-label");
    voice_enhance_status_label.set_visible(false);
    let voice_enhance_retry_btn = Button::with_label("Retry");
    voice_enhance_retry_btn.set_tooltip_text(Some(
        "Re-queue the voice enhance ffmpeg job for this clip.",
    ));
    voice_enhance_retry_btn.set_visible(false);
    voice_enhance_status_row.append(&voice_enhance_status_label);
    voice_enhance_status_row.append(&voice_enhance_retry_btn);
    audio_inner.append(&voice_enhance_status_row);

    row_label(&audio_inner, "Voice Isolation");
    let voice_isolation_slider = Scale::with_range(Orientation::Horizontal, 0.0, 100.0, 1.0);
    voice_isolation_slider.set_value(0.0);
    voice_isolation_slider.set_draw_value(true);
    voice_isolation_slider.set_digits(0);
    voice_isolation_slider.add_mark(0.0, gtk4::PositionType::Bottom, Some("Off"));
    voice_isolation_slider.add_mark(100.0, gtk4::PositionType::Bottom, Some("Max"));
    voice_isolation_slider.set_tooltip_text(Some(
        "Duck volume between spoken words (requires generated subtitles)",
    ));
    audio_inner.append(&voice_isolation_slider);

    row_label(&audio_inner, "  Padding (ms)");
    let vi_pad_slider = Scale::with_range(Orientation::Horizontal, 0.0, 500.0, 5.0);
    vi_pad_slider.set_value(80.0);
    vi_pad_slider.set_draw_value(true);
    vi_pad_slider.set_digits(0);
    vi_pad_slider.set_tooltip_text(Some(
        "Extend word boundaries to merge close words into continuous speech",
    ));
    audio_inner.append(&vi_pad_slider);

    row_label(&audio_inner, "  Fade (ms)");
    let vi_fade_slider = Scale::with_range(Orientation::Horizontal, 0.0, 200.0, 1.0);
    vi_fade_slider.set_value(25.0);
    vi_fade_slider.set_draw_value(true);
    vi_fade_slider.set_digits(0);
    vi_fade_slider.set_tooltip_text(Some(
        "Smooth transition time between speech and ducked regions",
    ));
    audio_inner.append(&vi_fade_slider);

    row_label(&audio_inner, "  Floor");
    let vi_floor_slider = Scale::with_range(Orientation::Horizontal, 0.0, 100.0, 1.0);
    vi_floor_slider.set_value(0.0);
    vi_floor_slider.set_draw_value(true);
    vi_floor_slider.set_digits(0);
    vi_floor_slider.add_mark(0.0, gtk4::PositionType::Bottom, Some("Silent"));
    vi_floor_slider.add_mark(100.0, gtk4::PositionType::Bottom, Some("Full"));
    vi_floor_slider.set_tooltip_text(Some(
        "Minimum volume during ducked regions (preserves room tone)",
    ));
    audio_inner.append(&vi_floor_slider);

    // ── Voice isolation source: Subtitles (default) or Silence-detect ──
    row_label(&audio_inner, "  Source");
    let vi_source_dropdown = gtk4::DropDown::from_strings(&["Subtitles", "Silence Detect"]);
    vi_source_dropdown.set_tooltip_text(Some(
        "Where to derive speech regions from. Subtitles uses generated word timings. \
         Silence Detect uses ffmpeg silencedetect — works without subtitles.",
    ));
    audio_inner.append(&vi_source_dropdown);

    // Silence-mode-only controls (visible only when source = Silence Detect)
    row_label(&audio_inner, "  Silence threshold");
    let vi_silence_threshold_slider = Scale::with_range(Orientation::Horizontal, -60.0, -10.0, 1.0);
    vi_silence_threshold_slider.set_value(-30.0);
    vi_silence_threshold_slider.set_draw_value(true);
    vi_silence_threshold_slider.set_digits(0);
    vi_silence_threshold_slider.add_mark(-60.0, gtk4::PositionType::Bottom, Some("-60 dB"));
    vi_silence_threshold_slider.add_mark(-30.0, gtk4::PositionType::Bottom, Some("-30 dB"));
    vi_silence_threshold_slider.add_mark(-10.0, gtk4::PositionType::Bottom, Some("-10 dB"));
    vi_silence_threshold_slider.set_tooltip_text(Some(
        "Audio below this dB level is treated as silence. Lower = stricter \
         (only treat near-silence as gaps). Click Suggest to auto-pick.",
    ));
    audio_inner.append(&vi_silence_threshold_slider);

    row_label(&audio_inner, "  Min gap (ms)");
    let vi_silence_min_ms_slider = Scale::with_range(Orientation::Horizontal, 50.0, 2000.0, 10.0);
    vi_silence_min_ms_slider.set_value(200.0);
    vi_silence_min_ms_slider.set_draw_value(true);
    vi_silence_min_ms_slider.set_digits(0);
    vi_silence_min_ms_slider.set_tooltip_text(Some(
        "Minimum silence duration to count as a gap. Higher = ignore \
         brief pauses between words.",
    ));
    audio_inner.append(&vi_silence_min_ms_slider);

    let vi_silence_actions_row = GBox::new(Orientation::Horizontal, 6);
    let vi_suggest_btn = Button::with_label("Suggest");
    vi_suggest_btn.set_tooltip_text(Some(
        "Analyze the clip's noise floor with ffmpeg astats and pick a \
         threshold automatically (5th percentile RMS + 6 dB headroom).",
    ));
    let vi_analyze_btn = Button::with_label("Analyze Audio");
    vi_analyze_btn.set_tooltip_text(Some(
        "Run silencedetect to find speech regions. Required before \
         silence-mode voice isolation can take effect.",
    ));
    let vi_intervals_label = Label::new(Some("Not analyzed"));
    vi_intervals_label.add_css_class("dim-label");
    vi_intervals_label.set_halign(gtk4::Align::Start);
    vi_intervals_label.set_hexpand(true);
    vi_silence_actions_row.append(&vi_suggest_btn);
    vi_silence_actions_row.append(&vi_analyze_btn);
    vi_silence_actions_row.append(&vi_intervals_label);
    audio_inner.append(&vi_silence_actions_row);

    let normalize_row = GBox::new(Orientation::Horizontal, 6);
    let normalize_btn = Button::with_label("Normalize\u{2026}");
    normalize_btn.set_tooltip_text(Some(
        "Analyze clip loudness and adjust volume to a target level",
    ));
    let match_audio_btn = Button::with_label("Match Audio\u{2026}");
    match_audio_btn.set_tooltip_text(Some(
        "Analyze this clip against a reference clip and apply matched loudness plus a 7-band mic-match EQ (great for making a lav mic sound more like a shotgun mic)",
    ));
    let clear_match_eq_btn = Button::with_label("Clear Match EQ");
    clear_match_eq_btn.set_tooltip_text(Some(
        "Clear the 7-band match EQ from the previous Match Audio (leaves the user 3-band EQ untouched)",
    ));
    clear_match_eq_btn.set_visible(false);
    let measured_loudness_label = Label::new(None);
    measured_loudness_label.add_css_class("dim-label");
    measured_loudness_label.set_halign(gtk4::Align::Start);
    measured_loudness_label.set_hexpand(true);
    normalize_row.append(&normalize_btn);
    normalize_row.append(&match_audio_btn);
    normalize_row.append(&clear_match_eq_btn);
    normalize_row.append(&measured_loudness_label);
    audio_inner.append(&normalize_row);

    // ── Match EQ frequency-response curve (7-band, read-only) ──
    let match_eq_curve_state: Rc<RefCell<Vec<crate::model::clip::EqBand>>> =
        Rc::new(RefCell::new(Vec::new()));
    let match_eq_curve = gtk4::DrawingArea::new();
    match_eq_curve.set_content_width(240);
    match_eq_curve.set_content_height(60);
    match_eq_curve.set_hexpand(true);
    match_eq_curve.set_vexpand(false);
    match_eq_curve.set_visible(false);
    match_eq_curve.set_tooltip_text(Some(
        "Match EQ — 7-band frequency response from the most recent Match Audio (mic match)",
    ));
    {
        let curve_state = match_eq_curve_state.clone();
        match_eq_curve.set_draw_func(move |_da, cr, ww, wh| {
            let bands = curve_state.borrow();
            if bands.is_empty() {
                return;
            }
            let w = ww as f64;
            let h = wh as f64;
            // Background
            cr.set_source_rgba(0.10, 0.10, 0.12, 0.95);
            cr.rectangle(0.0, 0.0, w, h);
            cr.fill().ok();
            // Border
            cr.set_source_rgba(0.30, 0.30, 0.34, 0.9);
            cr.set_line_width(1.0);
            cr.rectangle(0.5, 0.5, w - 1.0, h - 1.0);
            cr.stroke().ok();
            // 0 dB centerline
            let mid_y = h * 0.5;
            cr.set_source_rgba(0.45, 0.45, 0.48, 0.7);
            cr.set_dash(&[2.0, 3.0], 0.0);
            cr.move_to(0.0, mid_y);
            cr.line_to(w, mid_y);
            cr.stroke().ok();
            cr.set_dash(&[], 0.0);

            // Map frequency (log scale, 60 Hz to 12 kHz) to x.
            let log_lo = 60f64.log10();
            let log_hi = 12_000f64.log10();
            let x_for_freq = |f: f64| -> f64 {
                let lf = f.log10().clamp(log_lo, log_hi);
                ((lf - log_lo) / (log_hi - log_lo)) * w
            };
            // Map gain (-12 to +12 dB) to y.
            let max_gain_db = 12.0;
            let y_for_gain = |g: f64| -> f64 {
                let clamped = g.clamp(-max_gain_db, max_gain_db);
                mid_y - (clamped / max_gain_db) * (h * 0.45)
            };

            // Build a smooth curve through the band peaks.
            let mut points: Vec<(f64, f64)> = Vec::with_capacity(bands.len() + 2);
            points.push((0.0, mid_y));
            for band in bands.iter() {
                points.push((x_for_freq(band.freq), y_for_gain(band.gain)));
            }
            points.push((w, mid_y));

            // Filled curve (subtle teal)
            cr.set_source_rgba(0.25, 0.78, 0.78, 0.20);
            cr.move_to(0.0, mid_y);
            for (x, y) in &points {
                cr.line_to(*x, *y);
            }
            cr.line_to(w, mid_y);
            cr.close_path();
            cr.fill().ok();

            // Stroke curve
            cr.set_source_rgba(0.45, 0.95, 0.95, 0.95);
            cr.set_line_width(1.6);
            cr.move_to(points[0].0, points[0].1);
            for (x, y) in points.iter().skip(1) {
                cr.line_to(*x, *y);
            }
            cr.stroke().ok();

            // Band markers (small circles)
            cr.set_source_rgba(1.0, 1.0, 1.0, 0.85);
            for band in bands.iter() {
                let x = x_for_freq(band.freq);
                let y = y_for_gain(band.gain);
                cr.arc(x, y, 2.0, 0.0, std::f64::consts::TAU);
                cr.fill().ok();
            }
        });
    }
    audio_inner.append(&match_eq_curve);

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
    let pan_slider = Scale::with_range(
        Orientation::Horizontal,
        COLOR_SLIDER_MIN,
        COLOR_SLIDER_MAX,
        COLOR_SLIDER_STEP,
    );
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

    let pitch_preserve_check = gtk4::CheckButton::with_label("Preserve pitch during speed changes");
    pitch_preserve_check.set_tooltip_text(Some(
        "Use Rubberband time-stretch to keep audio pitch constant when clip speed is changed",
    ));
    pitch_inner.append(&pitch_preserve_check);

    let pitch_hint = Label::new(Some(
        "Pitch shift via Rubberband.\n0 = original pitch, \u{00b1}12 = \u{00b1}1 octave.",
    ));
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

    // Surround Position dropdown — controls per-track channel routing in
    // surround (5.1 / 7.1) exports. `Auto` resolves the destination from
    // `audio_role` (Dialogue → Center, Music → Front L/R, etc.). Has no
    // effect on stereo exports.
    row_label(&duck_inner, "Surround Position");
    #[allow(deprecated)]
    let surround_position_dropdown = gtk4::ComboBoxText::new();
    for pos in crate::model::track::SurroundPositionOverride::ALL {
        #[allow(deprecated)]
        surround_position_dropdown.append(Some(pos.as_str()), pos.label());
    }
    #[allow(deprecated)]
    surround_position_dropdown.set_active_id(Some("auto"));
    surround_position_dropdown.set_tooltip_text(Some(
        "Per-track destination for the multichannel upmix when exporting in 5.1 / 7.1 surround. \
         Auto picks a sensible default based on Audio Role. Ignored for stereo exports.",
    ));
    duck_inner.append(&surround_position_dropdown);

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
        "Normal",
        "Multiply",
        "Screen",
        "Overlay",
        "Add",
        "Difference",
        "Soft Light",
    ]);
    blend_mode_dropdown.set_selected(0);
    blend_mode_dropdown.set_halign(gtk4::Align::Start);
    blend_mode_dropdown.set_hexpand(true);
    blend_mode_dropdown.set_tooltip_text(Some("Compositing blend mode"));
    transform_inner.append(&blend_mode_dropdown);

    row_label(&transform_inner, "Anamorphic Desqueeze");
    let anamorphic_desqueeze_dropdown =
        gtk4::DropDown::from_strings(&["None (1.0x)", "1.33x", "1.5x", "1.8x", "2.0x"]);
    anamorphic_desqueeze_dropdown.set_selected(0);
    anamorphic_desqueeze_dropdown.set_halign(gtk4::Align::Start);
    anamorphic_desqueeze_dropdown.set_hexpand(true);
    anamorphic_desqueeze_dropdown.set_tooltip_text(Some("Anamorphic lens desqueeze factor"));
    transform_inner.append(&anamorphic_desqueeze_dropdown);

    // ── Motion Blur (export-only, gated on animated transform / fast speed) ──
    let motion_blur_row = gtk4::Box::new(Orientation::Horizontal, 6);
    let motion_blur_check = CheckButton::with_label("Motion Blur");
    motion_blur_check.set_active(false);
    motion_blur_check.set_tooltip_text(Some(
        "Render motion blur for keyframed transforms and fast-speed clips. Always applied at export; live preview when Background Render is on. Auto-skipped on static clips.",
    ));
    motion_blur_row.append(&motion_blur_check);
    let motion_blur_note = Label::new(Some("(export + background render)"));
    motion_blur_note.add_css_class("dim-label");
    motion_blur_row.append(&motion_blur_note);
    transform_inner.append(&motion_blur_row);

    row_label(&transform_inner, "Shutter Angle");
    let motion_blur_shutter_slider =
        Scale::with_range(Orientation::Horizontal, 0.0, 720.0, 1.0);
    motion_blur_shutter_slider.set_value(180.0);
    motion_blur_shutter_slider.set_draw_value(true);
    motion_blur_shutter_slider.set_digits(0);
    motion_blur_shutter_slider.add_mark(180.0, gtk4::PositionType::Bottom, Some("180°"));
    motion_blur_shutter_slider.add_mark(360.0, gtk4::PositionType::Bottom, Some("360°"));
    motion_blur_shutter_slider.set_hexpand(true);
    motion_blur_shutter_slider
        .set_tooltip_text(Some("Shutter angle in degrees: 180° = cinematic, 360° = full natural blur"));
    transform_inner.append(&motion_blur_shutter_slider);

    row_label(&transform_inner, "Scale");
    let scale_slider = Scale::with_range(Orientation::Horizontal, SCALE_MIN, SCALE_MAX, 0.05);
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
    let opacity_slider = Scale::with_range(
        Orientation::Horizontal,
        UNIT_SLIDER_MIN,
        UNIT_SLIDER_MAX,
        UNIT_SLIDER_STEP,
    );
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
    let position_x_slider =
        Scale::with_range(Orientation::Horizontal, POSITION_MIN, POSITION_MAX, 0.01);
    position_x_slider.set_value(0.0);
    position_x_slider.set_draw_value(true);
    position_x_slider.set_digits(2);
    position_x_slider.add_mark(POSITION_MIN, gtk4::PositionType::Bottom, Some("⇤"));
    position_x_slider.add_mark(-1.0, gtk4::PositionType::Bottom, Some("←"));
    position_x_slider.add_mark(0.0, gtk4::PositionType::Bottom, Some("·"));
    position_x_slider.add_mark(1.0, gtk4::PositionType::Bottom, Some("→"));
    position_x_slider.add_mark(POSITION_MAX, gtk4::PositionType::Bottom, Some("⇥"));
    position_x_slider.set_hexpand(true);
    position_x_slider.set_tooltip_text(Some(
        "Horizontal position: −1 = canvas left edge, 0 = center, +1 = canvas right edge. Values past ±1 push the clip off-canvas.",
    ));
    transform_inner.append(&position_x_slider);
    let position_x_keyframe_row = GBox::new(Orientation::Horizontal, 6);
    let position_x_set_keyframe_btn = Button::with_label("Set Position X Keyframe");
    let position_x_remove_keyframe_btn = Button::with_label("Remove Position X Keyframe");
    position_x_keyframe_row.append(&position_x_set_keyframe_btn);
    position_x_keyframe_row.append(&position_x_remove_keyframe_btn);
    transform_inner.append(&position_x_keyframe_row);

    row_label(&transform_inner, "Position Y");
    let position_y_slider =
        Scale::with_range(Orientation::Horizontal, POSITION_MIN, POSITION_MAX, 0.01);
    position_y_slider.set_value(0.0);
    position_y_slider.set_draw_value(true);
    position_y_slider.set_digits(2);
    position_y_slider.add_mark(POSITION_MIN, gtk4::PositionType::Bottom, Some("⤒"));
    position_y_slider.add_mark(-1.0, gtk4::PositionType::Bottom, Some("↑"));
    position_y_slider.add_mark(0.0, gtk4::PositionType::Bottom, Some("·"));
    position_y_slider.add_mark(1.0, gtk4::PositionType::Bottom, Some("↓"));
    position_y_slider.add_mark(POSITION_MAX, gtk4::PositionType::Bottom, Some("⤓"));
    position_y_slider.set_hexpand(true);
    position_y_slider.set_tooltip_text(Some(
        "Vertical position: −1 = canvas top edge, 0 = center, +1 = canvas bottom edge. Values past ±1 push the clip off-canvas.",
    ));
    transform_inner.append(&position_y_slider);
    let position_y_keyframe_row = GBox::new(Orientation::Horizontal, 6);
    let position_y_set_keyframe_btn = Button::with_label("Set Position Y Keyframe");
    let position_y_remove_keyframe_btn = Button::with_label("Remove Position Y Keyframe");
    position_y_keyframe_row.append(&position_y_set_keyframe_btn);
    position_y_keyframe_row.append(&position_y_remove_keyframe_btn);
    transform_inner.append(&position_y_keyframe_row);

    row_label(&transform_inner, "Crop Left");
    let crop_left_slider =
        Scale::with_range(Orientation::Horizontal, CROP_MIN_PX, CROP_MAX_PX, 2.0);
    crop_left_slider.set_value(0.0);
    crop_left_slider.set_draw_value(true);
    crop_left_slider.set_digits(0);
    crop_left_slider.set_tooltip_text(Some("Crop from the left edge, in project pixels (0–4000)."));
    transform_inner.append(&crop_left_slider);
    let crop_left_keyframe_row = GBox::new(Orientation::Horizontal, 6);
    let crop_left_set_keyframe_btn = Button::with_label("Set Crop Left Keyframe");
    let crop_left_remove_keyframe_btn = Button::with_label("Remove Crop Left Keyframe");
    crop_left_keyframe_row.append(&crop_left_set_keyframe_btn);
    crop_left_keyframe_row.append(&crop_left_remove_keyframe_btn);
    transform_inner.append(&crop_left_keyframe_row);

    row_label(&transform_inner, "Crop Right");
    let crop_right_slider =
        Scale::with_range(Orientation::Horizontal, CROP_MIN_PX, CROP_MAX_PX, 2.0);
    crop_right_slider.set_value(0.0);
    crop_right_slider.set_draw_value(true);
    crop_right_slider.set_digits(0);
    crop_right_slider.set_tooltip_text(Some(
        "Crop from the right edge, in project pixels (0–4000).",
    ));
    transform_inner.append(&crop_right_slider);
    let crop_right_keyframe_row = GBox::new(Orientation::Horizontal, 6);
    let crop_right_set_keyframe_btn = Button::with_label("Set Crop Right Keyframe");
    let crop_right_remove_keyframe_btn = Button::with_label("Remove Crop Right Keyframe");
    crop_right_keyframe_row.append(&crop_right_set_keyframe_btn);
    crop_right_keyframe_row.append(&crop_right_remove_keyframe_btn);
    transform_inner.append(&crop_right_keyframe_row);

    row_label(&transform_inner, "Crop Top");
    let crop_top_slider = Scale::with_range(Orientation::Horizontal, CROP_MIN_PX, CROP_MAX_PX, 2.0);
    crop_top_slider.set_value(0.0);
    crop_top_slider.set_draw_value(true);
    crop_top_slider.set_digits(0);
    crop_top_slider.set_tooltip_text(Some("Crop from the top edge, in project pixels (0–4000)."));
    transform_inner.append(&crop_top_slider);
    let crop_top_keyframe_row = GBox::new(Orientation::Horizontal, 6);
    let crop_top_set_keyframe_btn = Button::with_label("Set Crop Top Keyframe");
    let crop_top_remove_keyframe_btn = Button::with_label("Remove Crop Top Keyframe");
    crop_top_keyframe_row.append(&crop_top_set_keyframe_btn);
    crop_top_keyframe_row.append(&crop_top_remove_keyframe_btn);
    transform_inner.append(&crop_top_keyframe_row);

    row_label(&transform_inner, "Crop Bottom");
    let crop_bottom_slider =
        Scale::with_range(Orientation::Horizontal, CROP_MIN_PX, CROP_MAX_PX, 2.0);
    crop_bottom_slider.set_value(0.0);
    crop_bottom_slider.set_draw_value(true);
    crop_bottom_slider.set_digits(0);
    crop_bottom_slider.set_tooltip_text(Some(
        "Crop from the bottom edge, in project pixels (0–4000).",
    ));
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
    let rotate_spin = gtk4::SpinButton::with_range(ROTATE_MIN_DEG, ROTATE_MAX_DEG, 1.0);
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

    content_box.append(&transition_section);

    // ── Audition / clip-versions section (Audition clips only) ────────────
    let audition_section_box = GBox::new(Orientation::Vertical, 8);
    content_box.append(&audition_section_box);
    audition_section_box.append(&Separator::new(Orientation::Horizontal));
    let audition_expander = Expander::new(Some("Audition"));
    audition_expander.set_expanded(true);
    audition_section_box.append(&audition_expander);
    let audition_inner = GBox::new(Orientation::Vertical, 8);
    audition_expander.set_child(Some(&audition_inner));

    let audition_help = Label::new(Some(
        "Click a take to make it active. The Program Monitor and export will use\nthe active take. Other takes are kept for nondestructive A/B comparison.",
    ));
    audition_help.set_xalign(0.0);
    audition_help.set_wrap(true);
    audition_help.add_css_class("dim-label");
    audition_inner.append(&audition_help);

    let audition_takes_list = gtk4::ListBox::new();
    audition_takes_list.set_selection_mode(gtk4::SelectionMode::Single);
    audition_takes_list.add_css_class("boxed-list");
    audition_inner.append(&audition_takes_list);

    let audition_btn_row = GBox::new(Orientation::Horizontal, 6);
    audition_btn_row.set_homogeneous(true);
    let audition_add_take_btn = Button::with_label("Add Take from Source");
    audition_add_take_btn.set_tooltip_text(Some(
        "Add a new alternate take from the Source Monitor's currently marked region.",
    ));
    audition_btn_row.append(&audition_add_take_btn);
    let audition_remove_take_btn = Button::with_label("Remove Take");
    audition_remove_take_btn.set_tooltip_text(Some(
        "Remove the selected (non-active) take from the audition.",
    ));
    audition_remove_take_btn.set_sensitive(false);
    audition_btn_row.append(&audition_remove_take_btn);
    audition_inner.append(&audition_btn_row);

    let audition_finalize_btn = Button::with_label("Finalize Audition");
    audition_finalize_btn.set_tooltip_text(Some(
        "Collapse this audition to a normal clip using only the active take. Discards alternate takes.",
    ));
    audition_finalize_btn.add_css_class("destructive-action");
    audition_inner.append(&audition_finalize_btn);

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
    sync_title_font_button(&title_font_btn, "Sans Bold 36");
    title_inner.append(&title_font_btn);

    row_label(&title_inner, "Text Color");
    let title_color_dialog = gtk4::ColorDialog::new();
    title_color_dialog.set_with_alpha(true);
    let title_color_btn = gtk4::ColorDialogButton::new(Some(title_color_dialog));
    title_color_btn.set_rgba(&gdk4::RGBA::new(1.0, 1.0, 1.0, 1.0));
    title_inner.append(&title_color_btn);

    row_label(&title_inner, "Position X");
    let title_x_slider = Scale::with_range(
        Orientation::Horizontal,
        UNIT_SLIDER_MIN,
        UNIT_SLIDER_MAX,
        UNIT_SLIDER_STEP,
    );
    title_x_slider.set_value(0.5);
    title_x_slider.set_hexpand(true);
    title_inner.append(&title_x_slider);

    row_label(&title_inner, "Position Y");
    let title_y_slider = Scale::with_range(
        Orientation::Horizontal,
        UNIT_SLIDER_MIN,
        UNIT_SLIDER_MAX,
        UNIT_SLIDER_STEP,
    );
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

    // Slow-motion interpolation dropdown.  The "AI Interpolation (RIFE)"
    // entry is only shown when the RIFE ONNX model is actually installed —
    // the window glue toggles it dynamically as the model appears/disappears.
    row_label(&speed_inner, "Slow-Motion Interpolation:");
    let smo_initial_has_ai = crate::media::frame_interp_cache::find_model_path().is_some();
    let smo_interp_items: &[&str] = if smo_initial_has_ai {
        &[
            "Off",
            "Frame Blending",
            "Optical Flow",
            "AI Interpolation (RIFE)",
        ]
    } else {
        &["Off", "Frame Blending", "Optical Flow"]
    };
    let smo_interp_model = StringList::new(smo_interp_items);
    let slow_motion_model = smo_interp_model.clone();
    let slow_motion_has_ai = Cell::new(smo_initial_has_ai);
    let slow_motion_dropdown = DropDown::new(Some(smo_interp_model), gtk4::Expression::NONE);
    slow_motion_dropdown.set_selected(0);
    slow_motion_dropdown.set_tooltip_text(Some(
        "Synthesizes intermediate frames for smooth slow-motion (clips with speed < 1.0 only).\n\
         • Frame Blending: fast, soft.\n\
         • Optical Flow: ffmpeg motion compensation, sharper.\n\
         • AI Interpolation (RIFE): learned model, best quality. Precomputes a sidecar in the background.",
    ));
    speed_inner.append(&slow_motion_dropdown);
    let smo_note = Label::new(Some(
        "Synthesizes frames for slow-motion clips (preview + export)",
    ));
    smo_note.set_halign(gtk4::Align::Start);
    smo_note.add_css_class("clip-path");
    speed_inner.append(&smo_note);
    // Status row for AI interpolation sidecar generation.
    let frame_interp_status = Label::new(None);
    frame_interp_status.set_halign(gtk4::Align::Start);
    frame_interp_status.add_css_class("clip-path");
    frame_interp_status.set_visible(false);
    speed_inner.append(&frame_interp_status);

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
    let on_audio_changed: Rc<dyn Fn(&str, f32, f32, f32)> = Rc::new(on_audio_changed);
    let on_eq_changed: Rc<dyn Fn(&str, [crate::model::clip::EqBand; 3])> = Rc::new(on_eq_changed);
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
                    if let Some(clip) = proj.clip_mut(clip_id) {
                        clip.label = new_name.clone();
                    }
                    proj.dirty = true;
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
                    if let Some(clip) = proj.clip_mut(clip_id) {
                        apply(clip, val);
                    }
                    proj.dirty = true;
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
                    if let Some(clip) = proj.clip_mut(clip_id) {
                        clip.vidstab_smoothing = v;
                    }
                    proj.dirty = true;
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
                    if let Some(clip) = proj.clip_mut(clip_id) {
                        clip.vidstab_enabled = enabled;
                    }
                    proj.dirty = true;
                }
                on_vidstab_changed();
            }
        });
    }

    // Wire motion-blur enable checkbox. Export-only feature, so we just
    // mark dirty — no proxy rebuild, no preview pipeline rebuild.
    {
        let project = project.clone();
        let selected_clip_id = selected_clip_id.clone();
        let updating = updating.clone();
        let shutter_slider = motion_blur_shutter_slider.clone();
        motion_blur_check.connect_toggled(move |btn| {
            if *updating.borrow() {
                return;
            }
            let enabled = btn.is_active();
            shutter_slider.set_sensitive(enabled);
            let id = selected_clip_id.borrow().clone();
            if let Some(ref clip_id) = id {
                let mut proj = project.borrow_mut();
                if let Some(clip) = proj.clip_mut(clip_id) {
                    clip.motion_blur_enabled = enabled;
                }
                proj.dirty = true;
            }
        });
    }

    // Wire motion-blur shutter-angle slider — export-only, no preview rebuild.
    {
        let project = project.clone();
        let selected_clip_id = selected_clip_id.clone();
        let updating = updating.clone();
        motion_blur_shutter_slider.connect_value_changed(move |slider| {
            if *updating.borrow() {
                return;
            }
            let v = slider.value();
            let id = selected_clip_id.borrow().clone();
            if let Some(ref clip_id) = id {
                let mut proj = project.borrow_mut();
                if let Some(clip) = proj.clip_mut(clip_id) {
                    clip.motion_blur_shutter_angle = v;
                }
                proj.dirty = true;
            }
        });
    }

    // Wire "Enhance Voice" toggle. The toggle changes the SHAPE of the
    // GStreamer chain (elements added/removed), so it triggers a slot
    // rebuild via `on_clip_changed`. The strength slider below stays
    // on the live-update path because it only changes property values
    // on already-existing elements.
    {
        let project = project.clone();
        let selected_clip_id = selected_clip_id.clone();
        let updating = updating.clone();
        let on_clip_changed = on_clip_changed.clone();
        let strength_slider = voice_enhance_strength_slider.clone();
        voice_enhance_check.connect_toggled(move |btn| {
            if *updating.borrow() {
                return;
            }
            let enabled = btn.is_active();
            strength_slider.set_sensitive(enabled);
            let id = selected_clip_id.borrow().clone();
            if let Some(ref clip_id) = id {
                {
                    let mut proj = project.borrow_mut();
                    if let Some(clip) = proj.clip_mut(clip_id) {
                        clip.voice_enhance = enabled;
                    }
                    proj.dirty = true;
                }
                on_clip_changed();
            }
        });
    }
    // Strength slider — writes the model immediately, but debounces the
    // `on_clip_changed` call by ~350 ms so dragging the slider doesn't
    // spawn a new ffmpeg prerender job per tick. The cache key includes
    // the strength rounded to 1%, so the trailing-edge value the user
    // releases on is the one that gets a job — and bouncing back to a
    // previously-rendered strength is an instant cache hit.
    {
        use glib::translate::FromGlib;
        let project = project.clone();
        let selected_clip_id = selected_clip_id.clone();
        let updating = updating.clone();
        let on_clip_changed = on_clip_changed.clone();
        // Raw glib SourceId of the pending debounce timer (0 = none).
        // Stored as u32 inside a Cell so the slider closure can read,
        // cancel, and replace it without inner mutability gymnastics.
        let debounce_timer: Rc<Cell<u32>> = Rc::new(Cell::new(0));
        voice_enhance_strength_slider.connect_value_changed(move |s| {
            if *updating.borrow() {
                return;
            }
            let val = (s.value() / 100.0).clamp(0.0, 1.0) as f32;
            let id = selected_clip_id.borrow().clone();
            if let Some(ref clip_id) = id {
                {
                    let mut proj = project.borrow_mut();
                    if let Some(clip) = proj.clip_mut(clip_id) {
                        clip.voice_enhance_strength = val;
                    }
                    proj.dirty = true;
                }
                // Cancel any in-flight debounce timer so we always
                // fire after the user has been still for ~350 ms.
                let prev = debounce_timer.get();
                if prev != 0 {
                    unsafe { glib::SourceId::from_glib(prev) }.remove();
                    debounce_timer.set(0);
                }
                let timer = debounce_timer.clone();
                let cb = on_clip_changed.clone();
                let new_id = glib::timeout_add_local_once(
                    std::time::Duration::from_millis(350),
                    move || {
                        timer.set(0);
                        cb();
                    },
                );
                debounce_timer.set(new_id.as_raw());
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
                        proj.tracks
                            .iter()
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
            ges.connect_pressed(move |_, _, _, _| {
                do_snapshot();
            });
        }
        {
            let do_commit = do_commit.clone();
            ges.connect_released(move |_, _, _, _| {
                do_commit();
            });
        }
        slider.add_controller(ges);

        let focus_ctrl = gtk4::EventControllerFocus::new();
        {
            let do_snapshot = do_snapshot.clone();
            focus_ctrl.connect_enter(move |_| {
                do_snapshot();
            });
        }
        {
            focus_ctrl.connect_leave(move |_| {
                do_commit();
            });
        }
        slider.add_controller(focus_ctrl);
    }

    for s in [
        &brightness_slider,
        &contrast_slider,
        &saturation_slider,
        &temperature_slider,
        &tint_slider,
        &denoise_slider,
        &sharpness_slider,
        &blur_slider,
        &shadows_slider,
        &midtones_slider,
        &highlights_slider,
        &exposure_slider,
        &black_point_slider,
        &highlights_warmth_slider,
        &highlights_tint_slider,
        &midtones_warmth_slider,
        &midtones_tint_slider,
        &shadows_warmth_slider,
        &shadows_tint_slider,
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
                    if let Some(clip) = proj.clip_mut(clip_id) {
                        clip.color_label = color_label;
                    }
                    proj.dirty = true;
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
                    if let Some(clip) = proj.clip_mut(clip_id) {
                        clip.blend_mode = mode;
                    }
                    proj.dirty = true;
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
        let voice_isolation_slider = voice_isolation_slider.clone();
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
                    if let Some(clip) = proj.clip_mut(clip_id) {
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
                    }
                    proj.dirty = true;
                }
                // Use lightweight audio update (syncs keyframes to player without
                // full pipeline reload). on_clip_changed would cause a heavy rebuild
                // and visible playhead jump for every slider tick.
                on_audio_changed(
                    clip_id,
                    linear_vol,
                    pan_slider_cb.value() as f32,
                    voice_isolation_slider.value() as f32 / 100.0,
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
        let voice_isolation_slider = voice_isolation_slider.clone();
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
                    if let Some(clip) = proj.clip_mut(clip_id) {
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
                    }
                    proj.dirty = true;
                }
                on_audio_changed(
                    clip_id,
                    db_to_linear_volume(volume_slider_cb.value()) as f32,
                    pan_slider_cb.value() as f32,
                    voice_isolation_slider.value() as f32 / 100.0,
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
                            *vol_snap.borrow_mut() =
                                Some((cid.clone(), track.id.clone(), clip.volume, clip.pan));
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
                        proj.tracks
                            .iter()
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
            ges.connect_pressed(move |_, _, _, _| {
                snap_c();
            });
            ges.connect_released(move |_, _, _, _| {
                commit_c();
            });
            slider.add_controller(ges);

            let focus_ctrl = gtk4::EventControllerFocus::new();
            let snap_c = do_vol_snapshot.clone();
            let commit_c = do_vol_commit.clone();
            focus_ctrl.connect_enter(move |_| {
                snap_c();
            });
            focus_ctrl.connect_leave(move |_| {
                commit_c();
            });
            slider.add_controller(focus_ctrl);
        }
    }

    // Undo controller for voice isolation slider
    {
        type VoiceIsoSnapCell = Rc<RefCell<Option<(String, String, f32)>>>;
        let voice_iso_snap: VoiceIsoSnapCell = Rc::new(RefCell::new(None));

        let do_vi_snapshot = {
            let project = project.clone();
            let selected_clip_id = selected_clip_id.clone();
            let voice_iso_snap = voice_iso_snap.clone();
            move || {
                let cid = selected_clip_id.borrow().clone();
                if let Some(cid) = cid {
                    let mut proj = project.borrow_mut();
                    if let Some(clip) = proj.clip_mut(&cid) {
                        *voice_iso_snap.borrow_mut() =
                            Some((cid.clone(), String::new(), clip.voice_isolation));
                    }
                }
            }
        };

        let do_vi_commit = {
            let project = project.clone();
            let voice_iso_snap = voice_iso_snap.clone();
            let on_execute_command = on_execute_command.clone();
            move || {
                let entry = voice_iso_snap.borrow_mut().take();
                if let Some((clip_id, track_id, old_amount)) = entry {
                    let new_amount = {
                        let mut proj = project.borrow_mut();
                        proj.clip_mut(&clip_id)
                            .map(|c| c.voice_isolation)
                            .unwrap_or(old_amount)
                    };
                    on_execute_command(Box::new(crate::undo::SetClipVoiceIsolationCommand {
                        clip_id,
                        track_id,
                        old_amount,
                        new_amount,
                    }));
                }
            }
        };

        let ges = gtk4::GestureClick::new();
        let snap_c = do_vi_snapshot.clone();
        let commit_c = do_vi_commit.clone();
        ges.connect_pressed(move |_, _, _, _| {
            snap_c();
        });
        ges.connect_released(move |_, _, _, _| {
            commit_c();
        });
        voice_isolation_slider.add_controller(ges);

        let focus_ctrl = gtk4::EventControllerFocus::new();
        let snap_c = do_vi_snapshot.clone();
        let commit_c = do_vi_commit.clone();
        focus_ctrl.connect_enter(move |_| {
            snap_c();
        });
        focus_ctrl.connect_leave(move |_| {
            commit_c();
        });
        voice_isolation_slider.add_controller(focus_ctrl);

        let project_c = project.clone();
        let selected_clip_id_c = selected_clip_id.clone();
        let updating_c = updating.clone();
        let on_audio_changed_c = on_audio_changed.clone();
        let volume_slider_c = volume_slider.clone();
        let pan_slider_c = pan_slider.clone();
        let vi_pad_vis = vi_pad_slider.clone();
        let vi_fade_vis = vi_fade_slider.clone();
        let vi_floor_vis = vi_floor_slider.clone();
        voice_isolation_slider.connect_value_changed(move |s| {
            if *updating_c.borrow() {
                return;
            }
            let val = (s.value() / 100.0) as f32;
            let show_detail = val > 0.0;
            vi_pad_vis.set_visible(show_detail);
            vi_fade_vis.set_visible(show_detail);
            vi_floor_vis.set_visible(show_detail);
            let id = selected_clip_id_c.borrow().clone();
            if let Some(ref clip_id) = id {
                {
                    let mut proj = project_c.borrow_mut();
                    if let Some(clip) = proj.clip_mut(clip_id) {
                        clip.voice_isolation = val;
                    }
                    proj.dirty = true;
                }
                on_audio_changed_c(
                    clip_id,
                    db_to_linear_volume(volume_slider_c.value()) as f32,
                    pan_slider_c.value() as f32,
                    val,
                );
            }
        });
    }

    // Wire voice isolation detail sliders (pad, fade, floor).
    // These update the model and mark dirty; the next on_project_changed
    // rebuilds ProgramClips with the new values.
    for (slider, apply_fn) in [
        (
            &vi_pad_slider as &Scale,
            (|clip: &mut crate::model::clip::Clip, v: f64| {
                clip.voice_isolation_pad_ms = v as f32;
            }) as fn(&mut crate::model::clip::Clip, f64),
        ),
        (
            &vi_fade_slider as &Scale,
            (|clip: &mut crate::model::clip::Clip, v: f64| {
                clip.voice_isolation_fade_ms = v as f32;
            }) as fn(&mut crate::model::clip::Clip, f64),
        ),
        (
            &vi_floor_slider as &Scale,
            (|clip: &mut crate::model::clip::Clip, v: f64| {
                clip.voice_isolation_floor = (v / 100.0) as f32;
            }) as fn(&mut crate::model::clip::Clip, f64),
        ),
    ] {
        let project = project.clone();
        let selected_clip_id = selected_clip_id.clone();
        let updating = updating.clone();
        let on_clip_changed = on_clip_changed.clone();
        let slider = slider.clone();
        slider.connect_value_changed(move |s| {
            if *updating.borrow() {
                return;
            }
            let id = selected_clip_id.borrow().clone();
            if let Some(ref clip_id) = id {
                {
                    let mut proj = project.borrow_mut();
                    if let Some(clip) = proj.clip_mut(clip_id) {
                        apply_fn(clip, s.value());
                    }
                    proj.dirty = true;
                }
                on_clip_changed();
            }
        });
    }

    // Wire voice isolation source dropdown — toggles silence-mode controls,
    // updates the model, and pushes an undo command.
    {
        let project = project.clone();
        let selected_clip_id = selected_clip_id.clone();
        let updating = updating.clone();
        let on_clip_changed = on_clip_changed.clone();
        let on_execute_command = on_execute_command.clone();
        let vi_silence_threshold_vis = vi_silence_threshold_slider.clone();
        let vi_silence_min_ms_vis = vi_silence_min_ms_slider.clone();
        let vi_silence_actions_vis = vi_silence_actions_row.clone();
        let vi_intervals_label_c = vi_intervals_label.clone();
        vi_source_dropdown.connect_selected_notify(move |dd| {
            if *updating.borrow() {
                return;
            }
            let new_source = if dd.selected() == 1 {
                crate::model::clip::VoiceIsolationSource::Silence
            } else {
                crate::model::clip::VoiceIsolationSource::Subtitles
            };
            let is_silence = matches!(
                new_source,
                crate::model::clip::VoiceIsolationSource::Silence
            );
            vi_silence_threshold_vis.set_visible(is_silence);
            vi_silence_min_ms_vis.set_visible(is_silence);
            vi_silence_actions_vis.set_visible(is_silence);
            let id = selected_clip_id.borrow().clone();
            if let Some(ref clip_id) = id {
                let old_source = {
                    let proj = project.borrow();
                    proj.clip_ref(clip_id)
                        .map(|c| c.voice_isolation_source)
                        .unwrap_or_default()
                };
                if old_source == new_source {
                    return;
                }
                {
                    let mut proj = project.borrow_mut();
                    if let Some(clip) = proj.clip_mut(clip_id) {
                        clip.voice_isolation_source = new_source;
                    }
                    proj.dirty = true;
                }
                // Refresh the intervals label for the new source.
                {
                    let proj = project.borrow();
                    if let Some(c) = proj.clip_ref(clip_id) {
                        if is_silence && c.voice_isolation_speech_intervals.is_empty() {
                            vi_intervals_label_c.set_text("Not analyzed");
                        } else if is_silence {
                            vi_intervals_label_c.set_text(&format!(
                                "Speech intervals: {}",
                                c.voice_isolation_speech_intervals.len()
                            ));
                        } else {
                            vi_intervals_label_c.set_text("Not analyzed");
                        }
                    }
                }
                on_execute_command(Box::new(crate::undo::SetClipVoiceIsolationSourceCommand {
                    clip_id: clip_id.clone(),
                    track_id: String::new(),
                    old_source,
                    new_source,
                }));
                on_clip_changed();
            }
        });
    }

    // Wire silence-mode threshold + min-gap sliders. Param changes invalidate
    // the cached intervals so the user must re-Analyze.
    {
        type SilenceParamsSnap = Rc<RefCell<Option<(String, f32, u32, Vec<(u64, u64)>)>>>;
        let snap: SilenceParamsSnap = Rc::new(RefCell::new(None));

        let do_snap = {
            let project = project.clone();
            let selected_clip_id = selected_clip_id.clone();
            let snap = snap.clone();
            move || {
                let cid = selected_clip_id.borrow().clone();
                if let Some(cid) = cid {
                    let proj = project.borrow();
                    if let Some(clip) = proj.clip_ref(&cid) {
                        *snap.borrow_mut() = Some((
                            cid.clone(),
                            clip.voice_isolation_silence_threshold_db,
                            clip.voice_isolation_silence_min_ms,
                            clip.voice_isolation_speech_intervals.clone(),
                        ));
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
                if let Some((clip_id, old_threshold, old_min_ms, old_intervals)) = entry {
                    let (new_threshold, new_min_ms) = {
                        let proj = project.borrow();
                        proj.clip_ref(&clip_id)
                            .map(|c| {
                                (
                                    c.voice_isolation_silence_threshold_db,
                                    c.voice_isolation_silence_min_ms,
                                )
                            })
                            .unwrap_or((old_threshold, old_min_ms))
                    };
                    if (new_threshold - old_threshold).abs() < f32::EPSILON
                        && new_min_ms == old_min_ms
                    {
                        return;
                    }
                    on_execute_command(Box::new(
                        crate::undo::SetClipVoiceIsolationSilenceParamsCommand {
                            clip_id,
                            track_id: String::new(),
                            old_threshold_db: old_threshold,
                            new_threshold_db: new_threshold,
                            old_min_ms,
                            new_min_ms,
                            old_intervals,
                        },
                    ));
                }
            }
        };

        // Live model write + cache invalidation on every value change so the
        // playhead reflects the new params immediately. Undo capture happens
        // at gesture end via the click controller below.
        for (slider, apply_fn) in [
            (
                &vi_silence_threshold_slider as &Scale,
                (|clip: &mut crate::model::clip::Clip, v: f64| {
                    clip.voice_isolation_silence_threshold_db = v as f32;
                    clip.voice_isolation_speech_intervals.clear();
                }) as fn(&mut crate::model::clip::Clip, f64),
            ),
            (
                &vi_silence_min_ms_slider as &Scale,
                (|clip: &mut crate::model::clip::Clip, v: f64| {
                    clip.voice_isolation_silence_min_ms = v as u32;
                    clip.voice_isolation_speech_intervals.clear();
                }) as fn(&mut crate::model::clip::Clip, f64),
            ),
        ] {
            let project = project.clone();
            let selected_clip_id = selected_clip_id.clone();
            let updating = updating.clone();
            let on_clip_changed = on_clip_changed.clone();
            let slider = slider.clone();
            let vi_intervals_label_c = vi_intervals_label.clone();
            slider.connect_value_changed(move |s| {
                if *updating.borrow() {
                    return;
                }
                let id = selected_clip_id.borrow().clone();
                if let Some(ref clip_id) = id {
                    {
                        let mut proj = project.borrow_mut();
                        if let Some(clip) = proj.clip_mut(clip_id) {
                            apply_fn(clip, s.value());
                        }
                        proj.dirty = true;
                    }
                    vi_intervals_label_c.set_text("Not analyzed");
                    on_clip_changed();
                }
            });
        }

        for slider in [&vi_silence_threshold_slider, &vi_silence_min_ms_slider] {
            let ges = gtk4::GestureClick::new();
            let snap_c = do_snap.clone();
            let commit_c = do_commit.clone();
            ges.connect_pressed(move |_, _, _, _| {
                snap_c();
            });
            ges.connect_released(move |_, _, _, _| {
                commit_c();
            });
            slider.add_controller(ges);

            let focus_ctrl = gtk4::EventControllerFocus::new();
            let snap_c = do_snap.clone();
            let commit_c = do_commit.clone();
            focus_ctrl.connect_enter(move |_| {
                snap_c();
            });
            focus_ctrl.connect_leave(move |_| {
                commit_c();
            });
            slider.add_controller(focus_ctrl);
        }
    }

    // Suggest button — analyze noise floor via astats and update the threshold
    // slider. The slider's value-changed handler then propagates to the model
    // and invalidates the intervals cache.
    {
        let selected_clip_id = selected_clip_id.clone();
        let on_suggest_voice_isolation_threshold = on_suggest_voice_isolation_threshold.clone();
        let vi_silence_threshold_slider_c = vi_silence_threshold_slider.clone();
        vi_suggest_btn.connect_clicked(move |_| {
            let id = selected_clip_id.borrow().clone();
            if let Some(ref clip_id) = id {
                if let Some(suggested_db) = on_suggest_voice_isolation_threshold(clip_id) {
                    vi_silence_threshold_slider_c.set_value(suggested_db as f64);
                }
            }
        });
    }

    // Analyze Audio button — runs silencedetect, stores intervals, refreshes label.
    // The callback handles ffmpeg shell-out, undo push, and on_project_changed.
    {
        let project = project.clone();
        let selected_clip_id = selected_clip_id.clone();
        let on_analyze_voice_isolation_silence = on_analyze_voice_isolation_silence.clone();
        let vi_intervals_label_c = vi_intervals_label.clone();
        vi_analyze_btn.connect_clicked(move |_| {
            let id = selected_clip_id.borrow().clone();
            if let Some(ref clip_id) = id {
                on_analyze_voice_isolation_silence(clip_id);
                let proj = project.borrow();
                if let Some(c) = proj.clip_ref(clip_id) {
                    if c.voice_isolation_speech_intervals.is_empty() {
                        vi_intervals_label_c.set_text("Analysis returned no speech");
                    } else {
                        vi_intervals_label_c.set_text(&format!(
                            "Speech intervals: {}",
                            c.voice_isolation_speech_intervals.len()
                        ));
                    }
                }
            }
        });
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
    {
        let selected_clip_id = selected_clip_id.clone();
        let on_clear_match_eq = on_clear_match_eq.clone();
        clear_match_eq_btn.connect_clicked(move |_btn| {
            let id = selected_clip_id.borrow().clone();
            if let Some(clip_id) = id {
                on_clear_match_eq(&clip_id);
            }
        });
    }
    {
        let project = project.clone();
        let selected_clip_id = selected_clip_id.clone();
        let on_match_audio = on_match_audio.clone();
        match_audio_btn.connect_clicked(move |btn| {
            let source_id = selected_clip_id.borrow().clone();
            let Some(source_clip_id) = source_id else {
                return;
            };

            let (frame_rate, source_duration_ns, candidates) = {
                let proj = project.borrow();
                // Use recursive lookup for source clip (may be inside a compound).
                let source_duration_ns = proj.clip_ref(&source_clip_id).map(|c| {
                    c.source_duration()
                });
                // Collect candidate clips from ALL tracks (including inside compounds).
                let mut candidates = Vec::new();
                fn collect_candidates(
                    tracks: &[crate::model::track::Track],
                    source_id: &str,
                    candidates: &mut Vec<MatchAudioCandidate>,
                    frame_rate: &crate::model::project::FrameRate,
                ) {
                    for track in tracks {
                        for clip in &track.clips {
                            if clip.id == source_id {
                                continue;
                            }
                            if matches!(
                                clip.kind,
                                crate::model::clip::ClipKind::Video
                                    | crate::model::clip::ClipKind::Audio
                            ) {
                                let duration_ns = clip.source_duration();
                                let clip_label = audio_match_clip_label(&clip.label, &clip.id);
                                candidates.push(MatchAudioCandidate {
                                    clip_id: clip.id.clone(),
                                    label: format!(
                                        "{} ({})",
                                        clip_label,
                                        crate::ui::timecode::format_ns_as_timecode(
                                            duration_ns,
                                            frame_rate,
                                        )
                                    ),
                                    duration_ns,
                                });
                            }
                            // Recurse into compound clips
                            if let Some(ref inner) = clip.compound_tracks {
                                collect_candidates(inner, source_id, candidates, frame_rate);
                            }
                        }
                    }
                }
                collect_candidates(&proj.tracks, &source_clip_id, &mut candidates, &proj.frame_rate);
                (proj.frame_rate.clone(), source_duration_ns, candidates)
            };
            let Some(source_duration_ns) = source_duration_ns else {
                return;
            };

            if candidates.is_empty() {
                return;
            }
            let candidates = Rc::new(candidates);

            let window = btn.root().and_then(|r| r.downcast::<gtk4::Window>().ok());
            let dialog = gtk4::Window::builder()
                .title("Match Audio")
                .modal(true)
                .default_width(460)
                .build();
            dialog.set_resizable(false);
            if let Some(ref w) = window {
                dialog.set_transient_for(Some(w));
            }

            let vbox = GBox::new(Orientation::Vertical, 12);
            vbox.set_margin_start(16);
            vbox.set_margin_end(16);
            vbox.set_margin_top(16);
            vbox.set_margin_bottom(16);
            dialog.set_child(Some(&vbox));

            let label = Label::new(Some(
                "Choose a reference clip, then keep the simple voice match or switch to an exact region.",
            ));
            label.set_halign(gtk4::Align::Start);
            label.set_wrap(true);
            vbox.append(&label);

            let labels: Vec<String> = candidates
                .iter()
                .map(|candidate| candidate.label.clone())
                .collect();
            let string_list =
                gtk4::StringList::new(&labels.iter().map(|s| s.as_str()).collect::<Vec<_>>());
            let reference_dropdown = gtk4::DropDown::new(Some(string_list), gtk4::Expression::NONE);
            reference_dropdown.set_selected(0);
            reference_dropdown.set_hexpand(true);

            let channel_labels = crate::media::audio_match::AudioMatchChannelMode::ALL
                .iter()
                .map(|mode| mode.label())
                .collect::<Vec<_>>();
            let channel_list = gtk4::StringList::new(&channel_labels);
            let channel_dropdown = gtk4::DropDown::new(Some(channel_list), gtk4::Expression::NONE);
            channel_dropdown.set_selected(0);
            channel_dropdown.set_hexpand(true);

            let mode_list =
                gtk4::StringList::new(&["Match voice (Recommended)", "Choose region..."]);
            let mode_dropdown = gtk4::DropDown::new(Some(mode_list), gtk4::Expression::NONE);
            mode_dropdown.set_selected(0);
            mode_dropdown.set_hexpand(true);

            let form = gtk4::Grid::builder()
                .column_spacing(12)
                .row_spacing(8)
                .build();
            vbox.append(&form);

            let mode_label = Label::new(Some("Match mode"));
            mode_label.set_halign(gtk4::Align::Start);
            form.attach(&mode_label, 0, 0, 1, 1);
            form.attach(&mode_dropdown, 1, 0, 1, 1);

            let reference_label = Label::new(Some("Reference clip"));
            reference_label.set_halign(gtk4::Align::Start);
            form.attach(&reference_label, 0, 1, 1, 1);
            form.attach(&reference_dropdown, 1, 1, 1, 1);

            let channel_label = Label::new(Some("Channel handling"));
            channel_label.set_halign(gtk4::Align::Start);
            form.attach(&channel_label, 0, 2, 1, 1);
            form.attach(&channel_dropdown, 1, 2, 1, 1);

            let mode_description = Label::new(None);
            mode_description.set_halign(gtk4::Align::Start);
            mode_description.set_wrap(true);
            mode_description.add_css_class("dim-label");
            vbox.append(&mode_description);

            let channel_description = Label::new(None);
            channel_description.set_halign(gtk4::Align::Start);
            channel_description.set_wrap(true);
            channel_description.add_css_class("dim-label");
            vbox.append(&channel_description);

            let region_box = GBox::new(Orientation::Vertical, 8);
            vbox.append(&region_box);

            let region_hint = Label::new(Some(
                "Timecode uses HH:MM:SS:FF (or MM:SS:FF) at the project frame rate.",
            ));
            region_hint.set_halign(gtk4::Align::Start);
            region_hint.add_css_class("dim-label");
            region_hint.set_wrap(true);
            region_box.append(&region_hint);

            let region_form = gtk4::Grid::builder()
                .column_spacing(12)
                .row_spacing(8)
                .build();
            region_box.append(&region_form);

            let source_in_label = Label::new(Some("Source in"));
            source_in_label.set_halign(gtk4::Align::Start);
            region_form.attach(&source_in_label, 0, 0, 1, 1);
            let source_in_entry = Entry::new();
            source_in_entry.set_hexpand(true);
            region_form.attach(&source_in_entry, 1, 0, 1, 1);

            let source_out_label = Label::new(Some("Source out"));
            source_out_label.set_halign(gtk4::Align::Start);
            region_form.attach(&source_out_label, 0, 1, 1, 1);
            let source_out_entry = Entry::new();
            source_out_entry.set_hexpand(true);
            region_form.attach(&source_out_entry, 1, 1, 1, 1);

            let source_hint = Label::new(Some(&format!(
                "Source clip length: {}",
                crate::ui::timecode::format_ns_as_timecode(source_duration_ns, &frame_rate)
            )));
            source_hint.set_halign(gtk4::Align::Start);
            source_hint.add_css_class("dim-label");
            region_form.attach(&source_hint, 1, 2, 1, 1);

            let reference_in_label = Label::new(Some("Reference in"));
            reference_in_label.set_halign(gtk4::Align::Start);
            region_form.attach(&reference_in_label, 0, 3, 1, 1);
            let reference_in_entry = Entry::new();
            reference_in_entry.set_hexpand(true);
            region_form.attach(&reference_in_entry, 1, 3, 1, 1);

            let reference_out_label = Label::new(Some("Reference out"));
            reference_out_label.set_halign(gtk4::Align::Start);
            region_form.attach(&reference_out_label, 0, 4, 1, 1);
            let reference_out_entry = Entry::new();
            reference_out_entry.set_hexpand(true);
            region_form.attach(&reference_out_entry, 1, 4, 1, 1);

            let reference_hint = Label::new(None);
            reference_hint.set_halign(gtk4::Align::Start);
            reference_hint.add_css_class("dim-label");
            region_form.attach(&reference_hint, 1, 5, 1, 1);

            let error_label = Label::new(None);
            error_label.set_halign(gtk4::Align::Start);
            error_label.set_wrap(true);
            error_label.add_css_class("error");
            error_label.set_visible(false);
            vbox.append(&error_label);

            set_audio_match_region_entries(
                &source_in_entry,
                &source_out_entry,
                crate::media::audio_match::AnalysisRegionNs {
                    start_ns: 0,
                    end_ns: source_duration_ns,
                },
                &frame_rate,
            );

            let candidates_for_update = candidates.clone();
            let reference_hint_for_update = reference_hint.clone();
            let reference_in_for_update = reference_in_entry.clone();
            let reference_out_for_update = reference_out_entry.clone();
            let frame_rate_for_update = frame_rate.clone();
            let update_reference_range = Rc::new(move |selected: u32| {
                if let Some(candidate) = candidates_for_update.get(selected as usize) {
                    reference_hint_for_update.set_text(&format!(
                        "Reference clip length: {}",
                        crate::ui::timecode::format_ns_as_timecode(
                            candidate.duration_ns,
                            &frame_rate_for_update
                        )
                    ));
                    set_audio_match_region_entries(
                        &reference_in_for_update,
                        &reference_out_for_update,
                        crate::media::audio_match::AnalysisRegionNs {
                            start_ns: 0,
                            end_ns: candidate.duration_ns,
                        },
                        &frame_rate_for_update,
                    );
                }
            });
            update_reference_range(reference_dropdown.selected());
            let update_reference_range_notify = update_reference_range.clone();
            let error_label_for_reference_change = error_label.clone();
            reference_dropdown.connect_selected_notify(move |dd| {
                error_label_for_reference_change.set_visible(false);
                update_reference_range_notify(dd.selected());
            });

            let dialog_for_mode = dialog.clone();
            let mode_description_for_sync = mode_description.clone();
            let region_box_for_sync = region_box.clone();
            let sync_mode = Rc::new(move |selected: u32| {
                sync_match_audio_mode_ui(
                    &dialog_for_mode,
                    &mode_description_for_sync,
                    &region_box_for_sync,
                    MatchAudioDialogMode::from_index(selected),
                );
            });
            sync_mode(mode_dropdown.selected());
            let sync_mode_notify = sync_mode.clone();
            let error_label_for_mode_change = error_label.clone();
            mode_dropdown.connect_selected_notify(move |dd| {
                error_label_for_mode_change.set_visible(false);
                sync_mode_notify(dd.selected());
            });

            let channel_description_for_sync = channel_description.clone();
            let sync_channel_mode = Rc::new(move |selected: u32| {
                let mode = crate::media::audio_match::AudioMatchChannelMode::ALL
                    .get(selected as usize)
                    .copied()
                    .unwrap_or_default();
                sync_match_audio_channel_mode_ui(&channel_description_for_sync, mode);
            });
            sync_channel_mode(channel_dropdown.selected());
            let sync_channel_mode_notify = sync_channel_mode.clone();
            let error_label_for_channel_change = error_label.clone();
            channel_dropdown.connect_selected_notify(move |dd| {
                error_label_for_channel_change.set_visible(false);
                sync_channel_mode_notify(dd.selected());
            });

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
            let on_match_audio = on_match_audio.clone();
            let candidates = candidates.clone();
            let channel_dropdown = channel_dropdown.clone();
            let mode_dropdown = mode_dropdown.clone();
            let reference_dropdown = reference_dropdown.clone();
            let source_in_entry = source_in_entry.clone();
            let source_out_entry = source_out_entry.clone();
            let reference_in_entry = reference_in_entry.clone();
            let reference_out_entry = reference_out_entry.clone();
            let error_label = error_label.clone();
            let frame_rate = frame_rate.clone();
            ok_btn.connect_clicked(move |_| {
                let idx = reference_dropdown.selected() as usize;
                if idx >= candidates.len() {
                    error_label.set_text("Select a reference clip.");
                    error_label.set_visible(true);
                    return;
                }
                let mode = MatchAudioDialogMode::from_index(mode_dropdown.selected());
                let channel_mode = crate::media::audio_match::AudioMatchChannelMode::ALL
                    .get(channel_dropdown.selected() as usize)
                    .copied()
                    .unwrap_or_default();
                let (source_region, reference_region) = if mode.shows_region_fields() {
                    let source_region = match parse_audio_match_region_entries(
                        &source_in_entry,
                        &source_out_entry,
                        source_duration_ns,
                        &frame_rate,
                        "Source",
                    ) {
                        Ok(region) => region,
                        Err(error) => {
                            error_label.set_text(&error);
                            error_label.set_visible(true);
                            return;
                        }
                    };
                    let reference_region = match parse_audio_match_region_entries(
                        &reference_in_entry,
                        &reference_out_entry,
                        candidates[idx].duration_ns,
                        &frame_rate,
                        "Reference",
                    ) {
                        Ok(region) => region,
                        Err(error) => {
                            error_label.set_text(&error);
                            error_label.set_visible(true);
                            return;
                        }
                    };
                    (Some(source_region), Some(reference_region))
                } else {
                    (None, None)
                };
                error_label.set_visible(false);
                on_match_audio(
                    &source_clip_id,
                    source_region,
                    channel_mode,
                    &candidates[idx].clip_id,
                    reference_region,
                    channel_mode,
                );
                dialog_ok.close();
            });

            dialog.present();
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
            if *updating.borrow() {
                return;
            }
            let id = selected_clip_id.borrow().clone();
            #[allow(deprecated)]
            if let (Some(ref clip_id), Some(mode_id)) = (id, combo.active_id()) {
                {
                    let mut proj = project.borrow_mut();
                    if let Some(clip) = proj.clip_mut(clip_id) {
                        clip.audio_channel_mode =
                            crate::model::clip::AudioChannelMode::from_str(&mode_id);
                    }
                    proj.dirty = true;
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
            if *updating.borrow() {
                return;
            }
            let id = selected_clip_id.borrow().clone();
            if let Some(ref clip_id) = id {
                {
                    let mut proj = project.borrow_mut();
                    if let Some(clip) = proj.clip_mut(clip_id) {
                        clip.pitch_shift_semitones = s.value();
                        clip.pitch_preserve = pitch_preserve_check_cb.is_active();
                    }
                    proj.dirty = true;
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
            if *updating.borrow() {
                return;
            }
            let id = selected_clip_id.borrow().clone();
            if let Some(ref clip_id) = id {
                {
                    let mut proj = project.borrow_mut();
                    if let Some(clip) = proj.clip_mut(clip_id) {
                        clip.pitch_preserve = btn.is_active();
                    }
                    proj.dirty = true;
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
            if *updating.borrow() {
                return;
            }
            let id = selected_clip_id.borrow().clone();
            #[allow(deprecated)]
            if let (Some(ref clip_id), Some(role_id)) = (id, combo.active_id()) {
                on_role_changed(clip_id, &role_id);
            }
        });
    }

    // Wire Surround Position dropdown
    {
        let selected_clip_id = selected_clip_id.clone();
        let updating = updating.clone();
        let on_surround_position_changed = on_surround_position_changed.clone();
        #[allow(deprecated)]
        surround_position_dropdown.connect_changed(move |combo| {
            if *updating.borrow() {
                return;
            }
            let id = selected_clip_id.borrow().clone();
            #[allow(deprecated)]
            if let (Some(ref clip_id), Some(pos_id)) = (id, combo.active_id()) {
                on_surround_position_changed(clip_id, &pos_id);
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
            if *updating.borrow() {
                return;
            }
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
            if *updating.borrow() {
                return;
            }
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
                if *updating.borrow() {
                    return;
                }
                let id = selected_clip_id.borrow().clone();
                if let Some(ref clip_id) = id {
                    let bands = [
                        crate::model::clip::EqBand {
                            freq: fs[0].value(),
                            gain: gs[0].value(),
                            q: qs[0].value(),
                        },
                        crate::model::clip::EqBand {
                            freq: fs[1].value(),
                            gain: gs[1].value(),
                            q: qs[1].value(),
                        },
                        crate::model::clip::EqBand {
                            freq: fs[2].value(),
                            gain: gs[2].value(),
                            q: qs[2].value(),
                        },
                    ];
                    {
                        let mut proj = project.borrow_mut();
                        if let Some(clip) = proj.clip_mut(clip_id) {
                            clip.eq_bands = bands;
                        }
                        proj.dirty = true;
                    }
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
                if *updating.borrow() {
                    return;
                }
                let id = selected_clip_id.borrow().clone();
                if let Some(ref clip_id) = id {
                    let bands = [
                        crate::model::clip::EqBand {
                            freq: fs[0].value(),
                            gain: gs[0].value(),
                            q: qs[0].value(),
                        },
                        crate::model::clip::EqBand {
                            freq: fs[1].value(),
                            gain: gs[1].value(),
                            q: qs[1].value(),
                        },
                        crate::model::clip::EqBand {
                            freq: fs[2].value(),
                            gain: gs[2].value(),
                            q: qs[2].value(),
                        },
                    ];
                    {
                        let mut proj = project.borrow_mut();
                        if let Some(clip) = proj.clip_mut(clip_id) {
                            clip.eq_bands = bands;
                        }
                        proj.dirty = true;
                    }
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
                if *updating.borrow() {
                    return;
                }
                let id = selected_clip_id.borrow().clone();
                if let Some(ref clip_id) = id {
                    let bands = [
                        crate::model::clip::EqBand {
                            freq: fs[0].value(),
                            gain: gs[0].value(),
                            q: qs[0].value(),
                        },
                        crate::model::clip::EqBand {
                            freq: fs[1].value(),
                            gain: gs[1].value(),
                            q: qs[1].value(),
                        },
                        crate::model::clip::EqBand {
                            freq: fs[2].value(),
                            gain: gs[2].value(),
                            q: qs[2].value(),
                        },
                    ];
                    {
                        let mut proj = project.borrow_mut();
                        if let Some(clip) = proj.clip_mut(clip_id) {
                            clip.eq_bands = bands;
                        }
                        proj.dirty = true;
                    }
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
                            *eq_snap.borrow_mut() =
                                Some((cid.clone(), track.id.clone(), clip.eq_bands));
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
                        proj.tracks
                            .iter()
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

        let all_eq: Vec<Scale> = eq_freq_sliders
            .iter()
            .chain(eq_gain_sliders.iter())
            .chain(eq_q_sliders.iter())
            .cloned()
            .collect();

        for s in &all_eq {
            let ges = gtk4::GestureClick::new();
            let snap_c = do_eq_snapshot.clone();
            let commit_c = do_eq_commit.clone();
            ges.connect_pressed(move |_, _, _, _| {
                snap_c();
            });
            ges.connect_released(move |_, _, _, _| {
                commit_c();
            });
            s.add_controller(ges);

            let focus_ctrl = gtk4::EventControllerFocus::new();
            let snap_c = do_eq_snapshot.clone();
            let commit_c = do_eq_commit.clone();
            focus_ctrl.connect_enter(move |_| {
                snap_c();
            });
            focus_ctrl.connect_leave(move |_| {
                commit_c();
            });
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
                    if let Some(clip) = proj.clip_mut(clip_id) {
                        apply(clip, val);
                    }
                    proj.dirty = true;
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
            let rot = spin.value().clamp(ROTATE_MIN_DEG, ROTATE_MAX_DEG).round() as i32;
            rotate_value.set(rot as f64);
            rotate_dial.queue_draw();
            if *updating.borrow() {
                return;
            }
            let id = selected_clip_id.borrow().clone();
            if let Some(ref clip_id) = id {
                {
                    let mut proj = project.borrow_mut();
                    if let Some(clip) = proj.clip_mut(clip_id) {
                        clip.rotate = rot;
                    }
                    proj.dirty = true;
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
                    if let Some(clip) = proj.clip_mut(clip_id) {
                        clip.flip_h = fh;
                    }
                    proj.dirty = true;
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
                    if let Some(clip) = proj.clip_mut(clip_id) {
                        clip.flip_v = fv;
                    }
                    proj.dirty = true;
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
                    if let Some(clip) = proj.clip_mut(clip_id) {
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
                    }
                    proj.dirty = true;
                }
                if animation_mode.get() || {
                    let proj = project.borrow();
                    proj.clip_ref(clip_id)
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
                    if let Some(clip) = proj.clip_mut(clip_id) {
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
                    }
                    proj.dirty = true;
                }
                if animation_mode.get() || {
                    let proj = project.borrow();
                    proj.clip_ref(clip_id)
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
                    if let Some(clip) = proj.clip_mut(clip_id) {
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
                    }
                    proj.dirty = true;
                }
                if animation_mode.get() || {
                    let proj = project.borrow();
                    proj.clip_ref(clip_id)
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
                    if let Some(clip) = proj.clip_mut(clip_id) {
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
                    }
                    proj.dirty = true;
                }
                if animation_mode.get() || {
                    let proj = project.borrow();
                    proj.clip_ref(clip_id)
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
                    if let Some(clip) = proj.clip_mut(&clip_id) {
                        clip.upsert_phase1_keyframe_at_timeline_ns_with_interp(
                            property,
                            timeline_pos_ns,
                            value,
                            interp,
                        );
                        changed = true;
                    }
                    if changed {
                        proj.dirty = true;
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
                    if let Some(clip) = proj.clip_mut(&clip_id) {
                        removed =
                            clip.remove_phase1_keyframe_at_timeline_ns(property, timeline_pos_ns);
                    }
                    if removed {
                        proj.dirty = true;
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
            move || {
                crop_left_slider
                    .value()
                    .clamp(CROP_MIN_PX, CROP_MAX_PX)
                    .round()
            }
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
            move || {
                crop_right_slider
                    .value()
                    .clamp(CROP_MIN_PX, CROP_MAX_PX)
                    .round()
            }
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
            move || {
                crop_top_slider
                    .value()
                    .clamp(CROP_MIN_PX, CROP_MAX_PX)
                    .round()
            }
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
            move || {
                crop_bottom_slider
                    .value()
                    .clamp(CROP_MIN_PX, CROP_MAX_PX)
                    .round()
            }
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
            move || {
                rotate_spin
                    .value()
                    .clamp(ROTATE_MIN_DEG, ROTATE_MAX_DEG)
                    .round()
            }
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
                if let Some(clip) = proj.clip_mut(&clip_id) {
                    clip.upsert_phase1_keyframe_at_timeline_ns_with_interp(
                        Phase1KeyframeProperty::Speed,
                        timeline_pos_ns,
                        value,
                        interp,
                    );
                    found = Some((clip.speed, clip.speed_keyframes.clone()));
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
                if let Some(clip) = proj.clip_mut(&clip_id) {
                    let removed = clip.remove_phase1_keyframe_at_timeline_ns(
                        Phase1KeyframeProperty::Speed,
                        timeline_pos_ns,
                    );
                    if removed {
                        found = Some((clip.speed, clip.speed_keyframes.clone()));
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
            if let Some(clip) = proj.clip_ref(&clip_id) {
                let local = clip.local_timeline_position_ns(playhead);
                if let Some(prev_local) = clip.prev_keyframe_local_ns(local) {
                    let timeline_ns = clip.timeline_start.saturating_add(prev_local);
                    on_seek_to(timeline_ns);
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
            if let Some(clip) = proj.clip_ref(&clip_id) {
                let local = clip.local_timeline_position_ns(playhead);
                if let Some(next_local) = clip.next_keyframe_local_ns(local) {
                    let timeline_ns = clip.timeline_start.saturating_add(next_local);
                    on_seek_to(timeline_ns);
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
            if let Some(clip) = proj.clip_ref(&clip_id) {
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
            if let Some(clip) = proj.clip_ref(&clip_id) {
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
            if let Some(clip) = proj.clip_ref(&clip_id) {
                let local = clip.local_timeline_position_ns(playhead);
                let prev_volume =
                    clip.prev_keyframe_local_ns_for_property(Phase1KeyframeProperty::Volume, local);
                let prev_pan =
                    clip.prev_keyframe_local_ns_for_property(Phase1KeyframeProperty::Pan, local);
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
            if let Some(clip) = proj.clip_ref(&clip_id) {
                let local = clip.local_timeline_position_ns(playhead);
                let next_volume =
                    clip.next_keyframe_local_ns_for_property(Phase1KeyframeProperty::Volume, local);
                let next_pan =
                    clip.next_keyframe_local_ns_for_property(Phase1KeyframeProperty::Pan, local);
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
                    if let Some(clip) = proj.clip_mut(clip_id) {
                        clip.title_text = text.clone();
                    }
                    proj.dirty = true;
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
                    if let Some(clip) = proj.clip_mut(clip_id) {
                        clip.title_x = x;
                    }
                    proj.dirty = true;
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
                    if let Some(clip) = proj.clip_mut(clip_id) {
                        clip.title_y = y;
                    }
                    proj.dirty = true;
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
            dialog.choose_font(
                parent,
                initial.as_ref(),
                None::<&gio::Cancellable>,
                move |result| {
                    if let Ok(desc) = result {
                        let font_str = desc.to_string();
                        sync_title_font_button(&fb, &font_str);
                        let id = id_c.borrow().clone();
                        if let Some(ref clip_id) = id {
                            {
                                let mut proj = project_c.borrow_mut();
                                if let Some(clip) = proj.clip_mut(clip_id) {
                                    clip.title_font = font_str.clone();
                                }
                                proj.dirty = true;
                            }
                            on_title(te.text().to_string(), tx.value(), ty.value());
                            on_style();
                        }
                    }
                },
            );
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
                    if let Some(clip) = proj.clip_mut(clip_id) {
                        clip.title_color = color;
                    }
                    proj.dirty = true;
                }
                on_title_changed(
                    title_entry_c.text().to_string(),
                    title_x.value(),
                    title_y.value(),
                );
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
                    if let Some(clip) = proj.clip_mut(clip_id) {
                        clip.title_outline_width = val;
                    }
                    proj.dirty = true;
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
                    if let Some(clip) = proj.clip_mut(clip_id) {
                        clip.title_outline_color = color;
                    }
                    proj.dirty = true;
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
                    if let Some(clip) = proj.clip_mut(clip_id) {
                        clip.title_shadow = active;
                    }
                    proj.dirty = true;
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
                    if let Some(clip) = proj.clip_mut(clip_id) {
                        clip.title_shadow_color = color;
                    }
                    proj.dirty = true;
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
                    if let Some(clip) = proj.clip_mut(clip_id) {
                        clip.title_shadow_offset_x = val;
                    }
                    proj.dirty = true;
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
                    if let Some(clip) = proj.clip_mut(clip_id) {
                        clip.title_shadow_offset_y = val;
                    }
                    proj.dirty = true;
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
                    if let Some(clip) = proj.clip_mut(clip_id) {
                        clip.title_bg_box = active;
                    }
                    proj.dirty = true;
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
                    if let Some(clip) = proj.clip_mut(clip_id) {
                        clip.title_bg_box_color = color;
                    }
                    proj.dirty = true;
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
                    if let Some(clip) = proj.clip_mut(clip_id) {
                        clip.title_bg_box_padding = val;
                    }
                    proj.dirty = true;
                }
                on_title_style_changed();
            }
        });
    }

    // Transition controls
    {
        let project = project.clone();
        let selected_clip_id = selected_clip_id.clone();
        let updating = updating.clone();
        let transition_kind_dropdown = transition_kind_dropdown.clone();
        let transition_duration_ms = transition_duration_ms.clone();
        let transition_alignment_dropdown = transition_alignment_dropdown.clone();
        let transition_status_label = transition_status_label.clone();
        let on_execute_command = on_execute_command.clone();
        let on_clip_changed = on_clip_changed.clone();
        let transition_kind_dropdown_for_apply = transition_kind_dropdown.clone();
        let transition_duration_ms_for_apply = transition_duration_ms.clone();
        let transition_alignment_dropdown_for_apply = transition_alignment_dropdown.clone();
        let apply_transition_edit: Rc<dyn Fn(Option<String>)> = Rc::new(
            move |kind_override: Option<String>| {
                if *updating.borrow() {
                    return;
                }
                let Some(clip_id) = selected_clip_id.borrow().clone() else {
                    return;
                };
                let kind = kind_override.unwrap_or_else(|| {
                    transition_kind_dropdown_for_apply
                        .active_id()
                        .map(|id| id.to_string())
                        .unwrap_or_default()
                });
                let alignment = transition_alignment_dropdown_for_apply
                    .active_id()
                    .as_deref()
                    .and_then(TransitionAlignment::from_str)
                    .unwrap_or(TransitionAlignment::EndOnCut);
                let duration_ns = (transition_duration_ms_for_apply.value().round().max(0.0)
                    as u64)
                    .saturating_mul(1_000_000);
                let transition_change =
                    {
                        let proj = project.borrow();
                        proj.tracks.iter().find_map(|track| {
                            track.clips.iter().position(|clip| clip.id == clip_id).map(
                                |clip_index| {
                                    (
                                        track.id.clone(),
                                        track.clips[clip_index].outgoing_transition.clone(),
                                        validate_track_transition_request(
                                            track,
                                            clip_index,
                                            &kind,
                                            duration_ns,
                                            alignment,
                                        ),
                                    )
                                },
                            )
                        })
                    };
                let Some((track_id, old_transition, validated)) = transition_change else {
                    return;
                };
                let new_transition = match validated {
                    Ok(validated) => validated.transition,
                    Err(err) => {
                        let message = match err {
                            crate::model::transition::TransitionValidationError::MissingFollowingClip => {
                                "This clip needs a following clip on the same track to add a transition."
                                    .to_string()
                            }
                            crate::model::transition::TransitionValidationError::UnsupportedKind {
                                kind,
                            } => {
                                format!("Unsupported transition type: {kind}.")
                            }
                            crate::model::transition::TransitionValidationError::MissingDuration => {
                                "Transition duration must be greater than 0 ms.".to_string()
                            }
                            crate::model::transition::TransitionValidationError::BoundaryTooShort {
                                max_duration_ns,
                            } => format!(
                                "This cut is too short for a transition. Max overlap here is {:.0} ms.",
                                max_duration_ns as f64 / 1_000_000.0
                            ),
                        };
                        transition_status_label.set_text(&message);
                        return;
                    }
                };
                if new_transition == old_transition {
                    return;
                }
                on_execute_command(Box::new(crate::undo::SetClipTransitionCommand {
                    clip_id: clip_id.clone(),
                    track_id,
                    old_transition,
                    new_transition,
                }));
                on_clip_changed();
            },
        );

        {
            let apply_transition_edit = apply_transition_edit.clone();
            transition_kind_dropdown.connect_changed(move |_| {
                apply_transition_edit(None);
            });
        }
        {
            let apply_transition_edit = apply_transition_edit.clone();
            transition_duration_ms.connect_value_changed(move |_| {
                apply_transition_edit(None);
            });
        }
        {
            let apply_transition_edit = apply_transition_edit.clone();
            transition_alignment_dropdown.connect_changed(move |_| {
                apply_transition_edit(None);
            });
        }
        transition_clear_btn.connect_clicked(move |_| {
            apply_transition_edit(Some(String::new()));
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
                    proj.clip_ref(id)
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
                    if let Some(clip) = proj.clip_mut(id) {
                        clip.speed = speed;
                        changed = true;
                    }
                    if changed {
                        proj.dirty = true;
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
                            *speed_snap.borrow_mut() =
                                Some((cid.clone(), track.id.clone(), clip.speed));
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
                        proj.tracks
                            .iter()
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
            ges.connect_pressed(move |_, _, _, _| {
                do_speed_snapshot();
            });
        }
        {
            let do_speed_commit = do_speed_commit.clone();
            ges.connect_released(move |_, _, _, _| {
                do_speed_commit();
            });
        }
        speed_slider.add_controller(ges);

        let focus_ctrl = gtk4::EventControllerFocus::new();
        {
            let do_speed_snapshot = do_speed_snapshot.clone();
            focus_ctrl.connect_enter(move |_| {
                do_speed_snapshot();
            });
        }
        {
            focus_ctrl.connect_leave(move |_| {
                do_speed_commit();
            });
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
                if let Some(clip) = proj.clip_mut(id) {
                    clip.reverse = reversed;
                    found = true;
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
                3 => crate::model::clip::SlowMotionInterp::Ai,
                _ => crate::model::clip::SlowMotionInterp::Off,
            };
            if let Some(ref id) = *selected_clip_id.borrow() {
                let mut proj = project.borrow_mut();
                let mut found = false;
                if let Some(clip) = proj.clip_mut(id) {
                    clip.slow_motion_interp = interp;
                    found = true;
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
                            if let Some(clip) = proj.clip_mut(clip_id) {
                                clip.lut_paths.push(path_str.clone());
                                count = clip.lut_paths.len();
                            }
                            proj.dirty = true;
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
                                proj.clip_ref(clip_id)
                                    .map(|c| c.lut_paths.clone())
                                    .unwrap_or_default()
                            } else {
                                Vec::new()
                            }
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
                if let Some(clip) = proj.clip_mut(clip_id) {
                    clip.lut_paths.clear();
                }
                proj.dirty = true;
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

            // Collect other video/image clips as reference candidates (recursively, including compounds).
            let proj = project.borrow();
            let mut candidates: Vec<(String, String, String)> = Vec::new(); // (clip_id, label, track_id)
            fn collect_color_candidates(
                tracks: &[crate::model::track::Track],
                source_id: &str,
                candidates: &mut Vec<(String, String, String)>,
            ) {
                for track in tracks {
                    for clip in &track.clips {
                        if clip.id != source_id
                            && (clip.kind == crate::model::clip::ClipKind::Video
                                || clip.kind == crate::model::clip::ClipKind::Image)
                        {
                            candidates.push((
                                clip.id.clone(),
                                clip.label.clone(),
                                track.id.clone(),
                            ));
                        }
                        if let Some(ref inner) = clip.compound_tracks {
                            collect_color_candidates(inner, source_id, candidates);
                        }
                    }
                }
            }
            collect_color_candidates(&proj.tracks, &source_clip_id, &mut candidates);
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
            let string_list =
                gtk4::StringList::new(&labels.iter().map(|s| s.as_str()).collect::<Vec<_>>());
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
                    let find = |id: &str| -> Option<(String, u64, u64)> {
                        proj.clip_ref(id)
                            .map(|c| (c.source_path.clone(), c.source_in, c.source_out))
                    };
                    let ref_grading = proj
                        .clip_ref(&ref_clip_id)
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

                let (src_path, src_in, src_out) = src;
                let (ref_path, ref_in, ref_out) = reff;
                let params = crate::media::color_match::MatchColorParams {
                    source_path: src_path,
                    source_in_ns: src_in,
                    source_out_ns: src_out,
                    reference_path: ref_path,
                    reference_in_ns: ref_in,
                    reference_out_ns: ref_out,
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
                            if let Some(clip) = proj.clip_mut(&source_clip_id) {
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
                            }
                            proj.dirty = true;
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
                                proj.clip_ref(&source_clip_id)
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
                    if let Some(clip) = proj.clip_mut(clip_id) {
                        clip.chroma_key_enabled = enabled;
                    }
                    proj.dirty = true;
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
                    if let Some(clip) = proj.clip_mut(clip_id) {
                        clip.chroma_key_color = 0x00FF00;
                    }
                    proj.dirty = true;
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
                    if let Some(clip) = proj.clip_mut(clip_id) {
                        clip.chroma_key_color = 0x0000FF;
                    }
                    proj.dirty = true;
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
                    if let Some(clip) = proj.clip_mut(clip_id) {
                        clip.chroma_key_color = color;
                    }
                    proj.dirty = true;
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
                    if let Some(clip) = proj.clip_mut(clip_id) {
                        clip.chroma_key_tolerance = val;
                    }
                    proj.dirty = true;
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
                    if let Some(clip) = proj.clip_mut(clip_id) {
                        clip.chroma_key_softness = val;
                    }
                    proj.dirty = true;
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
                    if let Some(clip) = proj.clip_mut(clip_id) {
                        clip.bg_removal_enabled = enabled;
                    }
                    proj.dirty = true;
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
                    if let Some(clip) = proj.clip_mut(clip_id) {
                        clip.bg_removal_threshold = val;
                    }
                    proj.dirty = true;
                }
                on_bg_removal_changed();
            }
        });
    }

    // ── Shape Mask signals ────────────────────────────────────────────────
    // Enable/shape changes trigger a pipeline rebuild (adds/removes probe element).
    // Slider changes use on_frei0r_params_changed for live updates (probe reads
    // from shared Arc<Mutex> state without needing a rebuild).
    {
        let project = project.clone();
        let updating = updating.clone();
        let selected_clip_id = selected_clip_id.clone();
        let on_mask_changed = on_frei0r_changed.clone();
        mask_enable.connect_toggled(move |btn| {
            if *updating.borrow() {
                return;
            }
            let enabled = btn.is_active();
            let id = selected_clip_id.borrow().clone();
            if let Some(ref clip_id) = id {
                {
                    let mut proj = project.borrow_mut();
                    if let Some(clip) = proj.clip_mut(clip_id) {
                        if enabled && clip.masks.is_empty() {
                            clip.masks.push(crate::model::clip::ClipMask::new(
                                crate::model::clip::MaskShape::Rectangle,
                            ));
                        } else if !enabled && !clip.masks.is_empty() {
                            clip.masks[0].enabled = false;
                        } else if enabled && !clip.masks.is_empty() {
                            clip.masks[0].enabled = true;
                        }
                    }
                    proj.dirty = true;
                }
                on_mask_changed();
            }
        });
    }
    {
        let project = project.clone();
        let updating = updating.clone();
        let selected_clip_id = selected_clip_id.clone();
        let on_mask_changed = on_frei0r_changed.clone();
        mask_shape_dropdown.connect_selected_notify(move |dd| {
            if *updating.borrow() {
                return;
            }
            let id = selected_clip_id.borrow().clone();
            if let Some(ref clip_id) = id {
                {
                    let mut proj = project.borrow_mut();
                    if let Some(clip) = proj.clip_mut(clip_id) {
                        if let Some(m) = clip.masks.first_mut() {
                            match dd.selected() {
                                1 => {
                                    m.shape = crate::model::clip::MaskShape::Ellipse;
                                }
                                2 => {
                                    m.shape = crate::model::clip::MaskShape::Path;
                                    if m.path.is_none() {
                                        m.path = Some(crate::model::clip::default_diamond_path());
                                    }
                                }
                                _ => {
                                    m.shape = crate::model::clip::MaskShape::Rectangle;
                                }
                            }
                            proj.dirty = true;
                        }
                    }
                }
                on_mask_changed();
            }
        });
    }
    // Wire mask numeric sliders — use on_frei0r_params_changed (live update,
    // no pipeline rebuild) since the probe reads from shared state.
    macro_rules! wire_mask_slider {
        ($slider:expr, $field:ident) => {{
            let project = project.clone();
            let updating = updating.clone();
            let selected_clip_id = selected_clip_id.clone();
            let on_mask_live = on_frei0r_params_changed.clone();
            $slider.connect_value_changed(move |s| {
                if *updating.borrow() {
                    return;
                }
                let val = s.value();
                let id = selected_clip_id.borrow().clone();
                if let Some(ref clip_id) = id {
                    {
                        let mut proj = project.borrow_mut();
                        if let Some(clip) = proj.clip_mut(clip_id) {
                            if let Some(m) = clip.masks.first_mut() {
                                m.$field = val;
                            }
                        }
                        proj.dirty = true;
                    }
                    on_mask_live();
                }
            });
        }};
    }
    wire_mask_slider!(mask_center_x_slider, center_x);
    wire_mask_slider!(mask_center_y_slider, center_y);
    wire_mask_slider!(mask_width_slider, width);
    wire_mask_slider!(mask_height_slider, height);
    wire_mask_slider!(mask_feather_slider, feather);
    wire_mask_slider!(mask_expansion_slider, expansion);
    {
        let project = project.clone();
        let updating = updating.clone();
        let selected_clip_id = selected_clip_id.clone();
        let on_mask_live = on_frei0r_params_changed.clone();
        mask_rotation_spin.connect_value_changed(move |s| {
            if *updating.borrow() {
                return;
            }
            let val = s.value();
            let id = selected_clip_id.borrow().clone();
            if let Some(ref clip_id) = id {
                {
                    let mut proj = project.borrow_mut();
                    if let Some(clip) = proj.clip_mut(clip_id) {
                        if let Some(m) = clip.masks.first_mut() {
                            m.rotation = val;
                        }
                    }
                    proj.dirty = true;
                }
                on_mask_live();
            }
        });
    }
    {
        let project = project.clone();
        let updating = updating.clone();
        let selected_clip_id = selected_clip_id.clone();
        let on_mask_live = on_frei0r_params_changed.clone();
        mask_invert_check.connect_toggled(move |btn| {
            if *updating.borrow() {
                return;
            }
            let invert = btn.is_active();
            let id = selected_clip_id.borrow().clone();
            if let Some(ref clip_id) = id {
                {
                    let mut proj = project.borrow_mut();
                    if let Some(clip) = proj.clip_mut(clip_id) {
                        if let Some(m) = clip.masks.first_mut() {
                            m.invert = invert;
                        }
                    }
                    proj.dirty = true;
                }
                on_mask_live();
            }
        });
    }

    // ── "Generate with SAM" wiring (Phase 2b/2) ───────────────────────
    //
    // Declared unconditionally so the struct-literal at the end of
    // `build_inspector` can list it. Under `ai-inference` it's a
    // live `Rc<RefCell<Option<SamJobInFlight>>>`; without the feature
    // we skip the whole block and the struct field never exists.
    #[cfg(feature = "ai-inference")]
    let sam_job_handle: Rc<RefCell<Option<SamJobInFlight>>> =
        Rc::new(RefCell::new(None));

    #[cfg(feature = "ai-inference")]
    {
        // Click handler: confirm-if-existing-mask → resolve clip →
        // build hardcoded centre-region BoxPrompt → spawn_sam_job.
        //
        // The button label flips to "Generating…" and goes insensitive
        // until the polling tick drains the result. The polling tick
        // (installed just below) handles the success + error paths.
        let project_click = project.clone();
        let selected_clip_id_click = selected_clip_id.clone();
        let sam_job_handle_click = sam_job_handle.clone();
        let button_click = sam_generate_btn.clone();
        let on_request_sam_prompt_click = on_request_sam_prompt.clone();
        sam_generate_btn.connect_clicked(move |_| {
            // Short-circuit if another SAM job is already in flight.
            // The button is normally set insensitive while a job runs,
            // but the guard defends against signal re-entry.
            if sam_job_handle_click.borrow().is_some() {
                return;
            }

            // Resolve the selected clip: capture id + source path +
            // source_in + source_out in a brief borrow so the worker
            // thread doesn't touch the project.
            let Some(clip_id) = selected_clip_id_click.borrow().clone() else {
                return;
            };
            let clip_info = {
                let proj = project_click.borrow();
                proj.tracks.iter().find_map(|t| {
                    t.clips.iter().find(|c| c.id == clip_id).map(|c| {
                        (
                            c.source_path.clone(),
                            c.source_in,
                            c.source_out,
                            // "Does masks[0] look non-default?" — used
                            // by the confirmation dialog gate below.
                            !c.masks.is_empty()
                                && (c
                                    .masks
                                    .first()
                                    .map(|m| {
                                        m.path.is_some()
                                            || !matches!(
                                                m.shape,
                                                crate::model::clip::MaskShape::Rectangle
                                            )
                                            || (m.center_x - 0.5).abs() > f64::EPSILON
                                            || (m.center_y - 0.5).abs() > f64::EPSILON
                                            || (m.width - 0.25).abs() > f64::EPSILON
                                            || (m.height - 0.25).abs() > f64::EPSILON
                                            || m.rotation != 0.0
                                            || m.feather != 0.0
                                            || m.expansion != 0.0
                                            || m.invert
                                    })
                                    .unwrap_or(false)),
                        )
                    })
                })
            };
            let Some((source_path, source_in, _source_out, has_existing_mask)) =
                clip_info
            else {
                return;
            };

            // Helper: enter SAM prompt mode on the Program Monitor and,
            // once the user has drawn a box (or clicked a point),
            // spawn the SAM job with the captured normalized prompt.
            //
            // This replaces the Phase 2b/2 hardcoded centre box with
            // a real user-drawn box.
            let enter_prompt = {
                let clip_id = clip_id.clone();
                let source_path = source_path.clone();
                let sam_job_handle_click = sam_job_handle_click.clone();
                let button_click = button_click.clone();
                let on_request_sam_prompt_click = on_request_sam_prompt_click.clone();
                move || {
                    button_click.set_label("Draw box on clip…");
                    button_click.set_sensitive(false);

                    let clip_id = clip_id.clone();
                    let source_path = source_path.clone();
                    let sam_job_handle_click = sam_job_handle_click.clone();
                    let button_click = button_click.clone();

                    on_request_sam_prompt_click(Box::new(
                        move |nx1: f64, ny1: f64, nx2: f64, ny2: f64| {
                            // The overlay fires this with normalized
                            // clip-local coords. Build a prompt and
                            // dispatch the background SAM job.
                            let normalized_box = (
                                nx1 as f32,
                                ny1 as f32,
                                nx2 as f32,
                                ny2 as f32,
                            );
                            let prompt =
                                crate::media::sam_cache::BoxPrompt::from_corners(
                                    0.0, 0.0, 1.0, 1.0,
                                );
                            let input = crate::media::sam_job::SamJobInput {
                                source_path: std::path::PathBuf::from(
                                    &source_path,
                                ),
                                frame_ns: source_in,
                                prompt,
                                normalized_box: Some(normalized_box),
                                tolerance_px: 2.0,
                            };
                            let handle =
                                crate::media::sam_job::spawn_sam_job(input);
                            *sam_job_handle_click.borrow_mut() =
                                Some(SamJobInFlight {
                                    handle,
                                    clip_id: clip_id.clone(),
                                });
                            button_click.set_label("Generating… (~6s)");
                            // Button stays insensitive (already set above).
                        },
                    ));
                }
            };

            if has_existing_mask {
                // Confirmation dialog. Use gtk::AlertDialog::choose so
                // the click handler doesn't block; the callback
                // dispatches the spawn path only if the user picks
                // "Replace".
                let parent = button_click
                    .root()
                    .and_then(|r| r.downcast::<gtk::Window>().ok());
                let alert = gtk::AlertDialog::builder()
                    .message("Replace existing mask?")
                    .detail(
                        "This will overwrite the current mask on this \
                         clip with a new SAM-generated mask. Continue?",
                    )
                    .buttons(["Cancel", "Replace"])
                    .cancel_button(0)
                    .default_button(1)
                    .modal(true)
                    .build();
                let enter_prompt = enter_prompt.clone();
                alert.choose(
                    parent.as_ref(),
                    gio::Cancellable::NONE,
                    move |res| {
                        if matches!(res, Ok(1)) {
                            enter_prompt();
                        }
                    },
                );
            } else {
                enter_prompt();
            }
        });

        // Poll tick: drain the in-flight job once per 100 ms on the
        // GTK main thread. On Success, replace masks[0] with the
        // bezier polygon, mark the project dirty, and fire
        // on_frei0r_changed so the pipeline picks up the new shape.
        // On Error, pop a gtk::AlertDialog with the error text.
        // In either case, restore the button state.
        let project_tick = project.clone();
        let sam_job_handle_tick = sam_job_handle.clone();
        let button_tick = sam_generate_btn.clone();
        let on_frei0r_changed_tick = on_frei0r_changed.clone();
        glib::timeout_add_local(
            std::time::Duration::from_millis(100),
            move || {
                use crate::media::sam_job::SamJobResult;

                // Fast path: no job in flight, nothing to do. Use a
                // nested borrow so we can free it before taking the
                // job result out.
                let has_result = {
                    let borrow = sam_job_handle_tick.borrow();
                    match borrow.as_ref() {
                        Some(inflight) => inflight.handle.try_recv(),
                        None => None,
                    }
                };
                let Some(result) = has_result else {
                    return glib::ControlFlow::Continue;
                };

                // Take the in-flight state out so follow-up clicks
                // can spawn a fresh job.
                let inflight = sam_job_handle_tick.borrow_mut().take();
                let Some(inflight) = inflight else {
                    // Should never happen since we just observed a
                    // Some above, but defend against races anyway.
                    return glib::ControlFlow::Continue;
                };
                let clip_id = inflight.clip_id;

                // Restore the button regardless of outcome.
                button_tick.set_label("Generate with SAM");
                button_tick.set_sensitive(true);

                match result {
                    SamJobResult::Success {
                        mask_points,
                        score,
                    } => {
                        log::info!(
                            "SAM: clip={} points={} score={:.3}",
                            clip_id,
                            mask_points.len(),
                            score
                        );
                        let applied = {
                            let mut proj = project_tick.borrow_mut();
                            let mut found = false;
                            for track in proj.tracks.iter_mut() {
                                if let Some(clip) =
                                    track.clips.iter_mut().find(|c| c.id == clip_id)
                                {
                                    let new_mask =
                                        crate::model::clip::ClipMask::new_path(
                                            mask_points.clone(),
                                        );
                                    if clip.masks.is_empty() {
                                        clip.masks.push(new_mask);
                                    } else {
                                        clip.masks[0] = new_mask;
                                    }
                                    found = true;
                                    break;
                                }
                            }
                            if found {
                                proj.dirty = true;
                            }
                            found
                        };
                        if applied {
                            on_frei0r_changed_tick();
                        } else {
                            log::warn!(
                                "SAM: clip {} no longer exists; mask discarded",
                                clip_id
                            );
                        }
                    }
                    SamJobResult::Error(msg) => {
                        log::warn!("SAM: clip={} error={}", clip_id, msg);
                        let parent = button_tick
                            .root()
                            .and_then(|r| r.downcast::<gtk::Window>().ok());
                        let alert = gtk::AlertDialog::builder()
                            .message("SAM mask generation failed")
                            .detail(&msg)
                            .buttons(["OK"])
                            .modal(true)
                            .build();
                        alert.show(parent.as_ref());
                    }
                }

                glib::ControlFlow::Continue
            },
        );
    }

    // ── HSL Qualifier wiring ──────────────────────────────────────────
    // The HSL pad probe is always present in the effects chain; it reads
    // the clip's Option<HslQualifier> from the shared slot state. Live
    // updates do not require a pipeline rebuild.
    {
        let project = project.clone();
        let updating = updating.clone();
        let selected_clip_id = selected_clip_id.clone();
        let on_hsl_changed = on_frei0r_params_changed.clone();
        hsl_enable.connect_toggled(move |btn| {
            if *updating.borrow() {
                return;
            }
            let enabled = btn.is_active();
            let id = selected_clip_id.borrow().clone();
            if let Some(ref clip_id) = id {
                {
                    let mut proj = project.borrow_mut();
                    if let Some(clip) = proj.clip_mut(clip_id) {
                        if clip.hsl_qualifier.is_none() {
                            clip.hsl_qualifier =
                                Some(crate::model::clip::HslQualifier::default());
                        }
                        if let Some(ref mut q) = clip.hsl_qualifier {
                            q.enabled = enabled;
                        }
                    }
                    proj.dirty = true;
                }
                on_hsl_changed();
            }
        });
    }
    {
        let project = project.clone();
        let updating = updating.clone();
        let selected_clip_id = selected_clip_id.clone();
        let on_hsl_changed = on_frei0r_params_changed.clone();
        hsl_invert.connect_toggled(move |btn| {
            if *updating.borrow() {
                return;
            }
            let v = btn.is_active();
            let id = selected_clip_id.borrow().clone();
            if let Some(ref clip_id) = id {
                {
                    let mut proj = project.borrow_mut();
                    if let Some(clip) = proj.clip_mut(clip_id) {
                        if let Some(ref mut q) = clip.hsl_qualifier {
                            q.invert = v;
                        }
                    }
                    proj.dirty = true;
                }
                on_hsl_changed();
            }
        });
    }
    {
        let project = project.clone();
        let updating = updating.clone();
        let selected_clip_id = selected_clip_id.clone();
        let on_hsl_changed = on_frei0r_params_changed.clone();
        hsl_view_mask.connect_toggled(move |btn| {
            if *updating.borrow() {
                return;
            }
            let v = btn.is_active();
            let id = selected_clip_id.borrow().clone();
            if let Some(ref clip_id) = id {
                {
                    let mut proj = project.borrow_mut();
                    if let Some(clip) = proj.clip_mut(clip_id) {
                        if let Some(ref mut q) = clip.hsl_qualifier {
                            q.view_mask = v;
                        }
                    }
                    // view_mask is a preview-only debug flag, not a
                    // persistent project mutation.
                }
                on_hsl_changed();
            }
        });
    }
    macro_rules! wire_hsl_slider {
        ($slider:expr, $field:ident) => {{
            let project = project.clone();
            let updating = updating.clone();
            let selected_clip_id = selected_clip_id.clone();
            let on_hsl_live = on_frei0r_params_changed.clone();
            $slider.connect_value_changed(move |s| {
                if *updating.borrow() {
                    return;
                }
                let val = s.value();
                let id = selected_clip_id.borrow().clone();
                if let Some(ref clip_id) = id {
                    {
                        let mut proj = project.borrow_mut();
                        if let Some(clip) = proj.clip_mut(clip_id) {
                            if clip.hsl_qualifier.is_none() {
                                clip.hsl_qualifier =
                                    Some(crate::model::clip::HslQualifier::default());
                            }
                            if let Some(ref mut q) = clip.hsl_qualifier {
                                q.$field = val;
                            }
                        }
                        proj.dirty = true;
                    }
                    on_hsl_live();
                }
            });
        }};
    }
    wire_hsl_slider!(hsl_hue_min, hue_min);
    wire_hsl_slider!(hsl_hue_max, hue_max);
    wire_hsl_slider!(hsl_hue_softness, hue_softness);
    wire_hsl_slider!(hsl_sat_min, sat_min);
    wire_hsl_slider!(hsl_sat_max, sat_max);
    wire_hsl_slider!(hsl_sat_softness, sat_softness);
    wire_hsl_slider!(hsl_lum_min, lum_min);
    wire_hsl_slider!(hsl_lum_max, lum_max);
    wire_hsl_slider!(hsl_lum_softness, lum_softness);
    wire_hsl_slider!(hsl_brightness, brightness);
    wire_hsl_slider!(hsl_contrast, contrast);
    wire_hsl_slider!(hsl_saturation, saturation);

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
        voice_enhance_check,
        voice_enhance_strength_slider,
        voice_enhance_status_label,
        voice_enhance_retry_btn,
        voice_isolation_slider,
        vi_pad_slider,
        vi_fade_slider,
        vi_floor_slider,
        vi_source_dropdown,
        vi_silence_threshold_slider,
        vi_silence_min_ms_slider,
        vi_silence_actions_row,
        vi_suggest_btn,
        vi_analyze_btn,
        vi_intervals_label,
        pan_slider,
        normalize_btn,
        match_audio_btn,
        clear_match_eq_btn,
        match_eq_curve,
        match_eq_curve_state,
        measured_loudness_label,
        ladspa_effects_list,
        channel_mode_dropdown,
        pitch_shift_slider,
        pitch_preserve_check,
        role_dropdown,
        surround_position_dropdown,
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
        motion_blur_check,
        motion_blur_shutter_slider,
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
        slow_motion_model,
        slow_motion_has_ai,
        frame_interp_status,
        transition_kind_dropdown,
        transition_duration_ms,
        transition_alignment_dropdown,
        transition_clear_btn,
        transition_status_label,
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
        transition_section,
        lut_section_box,
        audition_section_box,
        audition_takes_list,
        audition_add_take_btn,
        audition_remove_take_btn,
        audition_finalize_btn,
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
        subtitle_section,
        subtitle_controls_box,
        subtitle_no_model_box,
        subtitle_generate_btn,
        subtitle_generate_spinner,
        subtitle_generate_label,
        subtitle_language_dropdown,
        subtitle_expander,
        subtitle_segments_section,
        compound_subtitle_label,
        subtitle_segments_expander: segments_expander,
        subtitle_list_box,
        subtitle_segments_snapshot: Rc::new(RefCell::new(Vec::new())),
        subtitle_clear_btn,
        subtitle_error_label,
        subtitle_font_btn,
        subtitle_color_btn,
        subtitle_highlight_dropdown,
        sub_bold_btn,
        sub_italic_btn,
        sub_underline_btn,
        sub_shadow_btn,
        sub_visible_check,
        hl_bold_check,
        hl_color_check,
        hl_underline_check,
        hl_stroke_check,
        hl_italic_check,
        hl_bg_check,
        hl_shadow_check,
        subtitle_bg_highlight_color_btn,
        subtitle_highlight_stroke_color_btn,
        subtitle_highlight_stroke_color_row,
        subtitle_bg_highlight_color_row,
        subtitle_highlight_color_btn,
        subtitle_highlight_color_row,
        subtitle_word_window_slider,
        subtitle_position_slider,
        subtitle_outline_color_btn,
        subtitle_bg_box_check,
        subtitle_bg_color_btn,
        subtitle_export_srt_btn,
        subtitle_import_srt_btn,
        subtitle_copy_style_btn,
        subtitle_paste_style_btn,
        subtitle_style_clipboard: style_clipboard,
        subtitle_style_box,
        stt_model_available: Cell::new(false),
        stt_generating: Cell::new(false),
        mask_section,
        mask_enable,
        mask_shape_dropdown,
        mask_center_x_slider,
        mask_center_y_slider,
        mask_width_slider,
        mask_height_slider,
        mask_rotation_spin,
        mask_feather_slider,
        mask_expansion_slider,
        mask_invert_check,
        mask_path_editor_box,
        mask_rect_ellipse_controls,
        sam_generate_btn,
        #[cfg(feature = "ai-inference")]
        sam_job_handle,
        hsl_section,
        hsl_enable,
        hsl_invert,
        hsl_view_mask,
        hsl_hue_min,
        hsl_hue_max,
        hsl_hue_softness,
        hsl_sat_min,
        hsl_sat_max,
        hsl_sat_softness,
        hsl_lum_min,
        hsl_lum_max,
        hsl_lum_softness,
        hsl_brightness,
        hsl_contrast,
        hsl_saturation,
        tracking_section,
        tracking_tracker_dropdown,
        tracking_add_btn,
        tracking_remove_btn,
        tracking_label_entry,
        tracking_edit_region_btn,
        tracking_center_x_slider,
        tracking_center_y_slider,
        tracking_width_slider,
        tracking_height_slider,
        tracking_rotation_spin,
        tracking_run_btn,
        tracking_cancel_btn,
        tracking_auto_crop_btn,
        tracking_auto_crop_padding_slider,
        tracking_status_label,
        tracking_target_dropdown,
        tracking_reference_dropdown,
        tracking_clear_binding_btn,
        tracking_binding_status_label,
        selected_motion_tracker_id,
        tracking_tracker_ids,
        tracking_reference_choices,
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

    // ── Audition section wiring ───────────────────────────────────────────
    // Click a take row → switch active take (undoable). Re-fetch the index
    // from the row's widget-name set in `refresh_audition_takes_list`.
    {
        let view_weak = Rc::downgrade(&view);
        view.audition_takes_list
            .connect_row_activated(move |_list, row| {
                let Some(view) = view_weak.upgrade() else { return };
                let cid = view.selected_clip_id.borrow().clone();
                let Some(cid) = cid else { return };
                let name = row.widget_name();
                let Some(idx_str) = name.strip_prefix("audition-take-") else {
                    return;
                };
                let Ok(new_index) = idx_str.parse::<usize>() else {
                    return;
                };
                // Snapshot the clip before mutation so undo restores any
                // host-field tweaks the user made while the previous take
                // was active.
                let snapshot = view
                    .project
                    .borrow()
                    .clip_ref(&cid)
                    .cloned();
                if snapshot.is_none() {
                    return;
                }
                if snapshot.as_ref().unwrap().audition_active_take_index == new_index {
                    // Already active; flip the remove-button sensitivity
                    // for the user's selection feedback.
                    view.audition_remove_take_btn.set_sensitive(false);
                    return;
                }
                (view.on_execute_command)(Box::new(
                    crate::undo::SetActiveAuditionTakeCommand {
                        clip_id: cid,
                        new_index,
                        before_snapshot: snapshot,
                    },
                ));
                (view.on_frei0r_changed)();
            });
    }
    // Toggle Remove Take button sensitivity based on selection — only
    // non-active rows can be removed.
    {
        let view_weak = Rc::downgrade(&view);
        view.audition_takes_list
            .connect_row_selected(move |_list, row| {
                let Some(view) = view_weak.upgrade() else { return };
                let Some(row) = row else {
                    view.audition_remove_take_btn.set_sensitive(false);
                    return;
                };
                let name = row.widget_name();
                let Some(idx_str) = name.strip_prefix("audition-take-") else {
                    view.audition_remove_take_btn.set_sensitive(false);
                    return;
                };
                let Ok(idx) = idx_str.parse::<usize>() else {
                    view.audition_remove_take_btn.set_sensitive(false);
                    return;
                };
                let cid = view.selected_clip_id.borrow().clone();
                let active = cid
                    .as_ref()
                    .and_then(|id| {
                        view.project
                            .borrow()
                            .clip_ref(id)
                            .map(|c| c.audition_active_take_index)
                    })
                    .unwrap_or(0);
                view.audition_remove_take_btn.set_sensitive(idx != active);
            });
    }
    // Remove the currently selected (non-active) take.
    {
        let view_weak = Rc::downgrade(&view);
        view.audition_remove_take_btn
            .connect_clicked(move |_| {
                let Some(view) = view_weak.upgrade() else { return };
                let cid = view.selected_clip_id.borrow().clone();
                let Some(cid) = cid else { return };
                let Some(row) = view.audition_takes_list.selected_row() else {
                    return;
                };
                let name = row.widget_name();
                let Some(idx_str) = name.strip_prefix("audition-take-") else {
                    return;
                };
                let Ok(take_index) = idx_str.parse::<usize>() else {
                    return;
                };
                (view.on_execute_command)(Box::new(crate::undo::RemoveAuditionTakeCommand {
                    clip_id: cid,
                    take_index,
                    removed: std::cell::RefCell::new(None),
                }));
                (view.on_frei0r_changed)();
            });
    }
    // Add Take From Source — pulls the source monitor's currently loaded
    // clip + In/Out marks via the `selected_clip_id`'s source path. Without
    // a source-monitor handle on the inspector, we synthesize the take from
    // a duplicate of the audition's currently-active take so the user can
    // immediately add another candidate by importing/loading a different
    // file in the source monitor first; alternatively the timeline context
    // menu's "Add Take from Source Monitor" entry handles the marked-region
    // case.
    {
        let view_weak = Rc::downgrade(&view);
        view.audition_add_take_btn.connect_clicked(move |_| {
            let Some(view) = view_weak.upgrade() else { return };
            let cid = view.selected_clip_id.borrow().clone();
            let Some(cid) = cid else { return };
            // Build a new take from the active take as a starting point.
            let take = {
                let proj = view.project.borrow();
                let Some(clip) = proj.clip_ref(&cid) else { return };
                let active = clip.audition_active_take_index;
                let label_n = clip
                    .audition_takes
                    .as_ref()
                    .map(|t| t.len() + 1)
                    .unwrap_or(1);
                let base = clip
                    .audition_takes
                    .as_ref()
                    .and_then(|t| t.get(active))
                    .cloned();
                let Some(base) = base else { return };
                crate::model::clip::AuditionTake {
                    id: uuid::Uuid::new_v4().to_string(),
                    label: format!("Take {}", label_n),
                    source_path: base.source_path.clone(),
                    source_in: base.source_in,
                    source_out: base.source_out,
                    source_timecode_base_ns: base.source_timecode_base_ns,
                    media_duration_ns: base.media_duration_ns,
                }
            };
            (view.on_execute_command)(Box::new(crate::undo::AddAuditionTakeCommand {
                clip_id: cid,
                take,
            }));
            (view.on_frei0r_changed)();
        });
    }
    // Finalize → collapse the audition to a plain clip referencing only the
    // active take.
    {
        let view_weak = Rc::downgrade(&view);
        view.audition_finalize_btn.connect_clicked(move |_| {
            let Some(view) = view_weak.upgrade() else { return };
            let cid = view.selected_clip_id.borrow().clone();
            let Some(cid) = cid else { return };
            let snapshot = view.project.borrow().clip_ref(&cid).cloned();
            if snapshot.is_none() {
                return;
            }
            (view.on_execute_command)(Box::new(crate::undo::FinalizeAuditionCommand {
                clip_id: cid,
                before_snapshot: snapshot,
            }));
            (view.on_frei0r_changed)();
        });
    }

    (vbox, view)
}

fn dial_point_to_degrees(x: f64, y: f64, width: f64, height: f64) -> i32 {
    let cx = width / 2.0;
    let cy = height / 2.0;
    let mut deg = ((y - cy).atan2(x - cx).to_degrees() + 90.0).rem_euclid(360.0);
    if deg > 180.0 {
        deg -= 360.0;
    }
    -(deg.round().clamp(
        crate::model::transform_bounds::ROTATE_MIN_DEG,
        crate::model::transform_bounds::ROTATE_MAX_DEG,
    ) as i32)
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
