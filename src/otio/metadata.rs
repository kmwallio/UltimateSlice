use serde::{de::DeserializeOwned, Deserialize, Serialize};
use serde_json::Value;

use crate::model::clip::{
    AngleSwitch, AudioChannelMode, AuditionTake, BlendMode, ClipColorLabel, ClipMask, EqBand,
    Frei0rEffect, LadspaEffect, MotionTracker, MulticamAngle, NumericKeyframe, SlowMotionInterp,
    SubtitleHighlightFlags, SubtitleHighlightMode, SubtitleSegment, TrackingBinding,
    VoiceIsolationSource,
};
use crate::model::track::Track;

pub(crate) const ULTIMATESLICE_OTIO_METADATA_VERSION: u32 = 1;

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub(crate) struct UltimateSliceClipOtioMetadata {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) kind: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) speed: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) reverse: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) volume: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) pan: Option<f64>,
    /// 3-band parametric EQ: low/mid/high. Stored as a fixed-size array.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) eq_bands: Option<[EqBand; 3]>,
    /// 7-band match EQ from audio matching (mic-match correction).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) match_eq_bands: Option<Vec<EqBand>>,
    /// One-knob "Enhance Voice" toggle (FFmpeg HPF + denoise + EQ + compressor
    /// chain, applied at export only). Defaults to false.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) voice_enhance: Option<bool>,
    /// Strength of the voice-enhance chain, 0.0..=1.0.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) voice_enhance_strength: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) voice_isolation: Option<f64>,
    /// Voice isolation tunables.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) voice_isolation_pad_ms: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) voice_isolation_fade_ms: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) voice_isolation_floor: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) voice_isolation_source: Option<VoiceIsolationSource>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) voice_isolation_silence_threshold_db: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) voice_isolation_silence_min_ms: Option<u32>,
    /// Last measured integrated loudness in LUFS (informational).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) measured_loudness_lufs: Option<f64>,
    /// Chroma key (green/blue screen).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) chroma_key_enabled: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) chroma_key_color: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) chroma_key_tolerance: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) chroma_key_softness: Option<f64>,
    /// AI background removal.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) bg_removal_enabled: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) bg_removal_threshold: Option<f64>,
    /// Freeze frame.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) freeze_frame: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) freeze_frame_source_ns: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) freeze_frame_hold_duration_ns: Option<u64>,
    /// Video stabilization (export-only via libvidstab).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) vidstab_enabled: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) vidstab_smoothing: Option<f64>,
    /// Motion blur for keyframed transforms / fast-speed clips (export-only).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) motion_blur_enabled: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) motion_blur_shutter_angle: Option<f64>,
    /// Semantic clip color label for timeline tinting.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) color_label: Option<ClipColorLabel>,
    /// Anamorphic desqueeze factor (1.0, 1.33, 1.5, 1.8, 2.0).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) anamorphic_desqueeze: Option<f64>,
    /// Clip group identifiers (loose group + strict A/V link group).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) group_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) link_group_id: Option<String>,
    /// Source media absolute timecode base (ns).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) source_timecode_base_ns: Option<u64>,
    /// True when this image clip is a prerendered animated SVG source.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) animated_svg: Option<bool>,
    /// Frei0r video filter effects chain.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) frei0r_effects: Option<Vec<Frei0rEffect>>,
    /// LADSPA audio effects chain.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) ladspa_effects: Option<Vec<LadspaEffect>>,
    /// Shape masks (rectangles/ellipses) restricting the visible area.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) masks: Option<Vec<ClipMask>>,
    /// Motion trackers authored on the source clip.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) motion_trackers: Option<Vec<MotionTracker>>,
    /// Optional transform-level motion-tracking attachment.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) tracking_binding: Option<TrackingBinding>,
    /// Internal tracks for compound (nested timeline) clips. Recursive — each
    /// internal track holds full `Clip` objects which themselves can be
    /// compound. Serialized via the existing serde derives on `Track`/`Clip`.
    /// Only emitted when present so non-compound clips don't bloat the JSON.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) compound_tracks: Option<Vec<Track>>,
    /// Camera angles for multicam clips.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) multicam_angles: Option<Vec<MulticamAngle>>,
    /// Angle switch points for multicam clips, sorted by `position_ns`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) multicam_switches: Option<Vec<AngleSwitch>>,
    /// Alternate takes for audition clips.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) audition_takes: Option<Vec<AuditionTake>>,
    /// Index of the active audition take. Only meaningful when
    /// `audition_takes.is_some()`. Default 0.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) audition_active_take_index: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) brightness: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) contrast: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) saturation: Option<f64>,
    /// Color temperature in Kelvin (2000–10000, neutral 6500).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) temperature: Option<f64>,
    /// Tint shift on the green–magenta axis (−1.0..1.0).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) tint: Option<f64>,
    /// Image filters: denoise, sharpness, blur (each −1.0..1.0 or 0..1).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) denoise: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) sharpness: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) blur: Option<f64>,
    /// Color grading sliders (10 fields, each −1.0..1.0).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) shadows: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) midtones: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) highlights: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) exposure: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) black_point: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) highlights_warmth: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) highlights_tint: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) midtones_warmth: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) midtones_tint: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) shadows_warmth: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) shadows_tint: Option<f64>,
    /// HSL Qualifier (secondary color correction).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) hsl_qualifier: Option<crate::model::clip::HslQualifier>,
    /// Pitch shift in semitones (−12.0..+12.0) and pitch-preserve flag.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) pitch_shift_semitones: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) pitch_preserve: Option<bool>,
    /// Audio channel routing (Stereo / Left / Right / MonoMix).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) audio_channel_mode: Option<AudioChannelMode>,
    /// Variable speed keyframes + slow-motion interpolation mode.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) speed_keyframes: Option<Vec<NumericKeyframe>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) slow_motion_interp: Option<SlowMotionInterp>,
    /// Ordered .cube LUT file paths applied sequentially via FFmpeg lut3d.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) lut_paths: Option<Vec<String>>,
    /// Color/image keyframes.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) brightness_keyframes: Option<Vec<NumericKeyframe>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) contrast_keyframes: Option<Vec<NumericKeyframe>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) saturation_keyframes: Option<Vec<NumericKeyframe>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) temperature_keyframes: Option<Vec<NumericKeyframe>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) tint_keyframes: Option<Vec<NumericKeyframe>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) blur_keyframes: Option<Vec<NumericKeyframe>>,
    /// Audio keyframes.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) volume_keyframes: Option<Vec<NumericKeyframe>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) pan_keyframes: Option<Vec<NumericKeyframe>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) eq_low_gain_keyframes: Option<Vec<NumericKeyframe>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) eq_mid_gain_keyframes: Option<Vec<NumericKeyframe>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) eq_high_gain_keyframes: Option<Vec<NumericKeyframe>>,
    /// Crop edge keyframes.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) crop_left_keyframes: Option<Vec<NumericKeyframe>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) crop_right_keyframes: Option<Vec<NumericKeyframe>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) crop_top_keyframes: Option<Vec<NumericKeyframe>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) crop_bottom_keyframes: Option<Vec<NumericKeyframe>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) opacity: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) scale: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) position_x: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) position_y: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) rotate: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) flip_h: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) flip_v: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) crop_left: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) crop_right: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) crop_top: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) crop_bottom: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) blend_mode: Option<BlendMode>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) opacity_keyframes: Option<Vec<NumericKeyframe>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) scale_keyframes: Option<Vec<NumericKeyframe>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) position_x_keyframes: Option<Vec<NumericKeyframe>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) position_y_keyframes: Option<Vec<NumericKeyframe>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) rotate_keyframes: Option<Vec<NumericKeyframe>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) title_text: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) title_font: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) title_color: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) title_x: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) title_y: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) title_template: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) title_outline_color: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) title_outline_width: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) title_shadow: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) title_shadow_color: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) title_shadow_offset_x: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) title_shadow_offset_y: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) title_bg_box: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) title_bg_box_color: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) title_bg_box_padding: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) title_clip_bg_color: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) title_secondary_text: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) subtitle_segments: Option<Vec<SubtitleSegment>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) subtitles_language: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) subtitle_font: Option<String>,
    /// Base subtitle text styling (always-on, applies to every subtitle line).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) subtitle_bold: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) subtitle_italic: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) subtitle_underline: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) subtitle_shadow: Option<bool>,
    /// Whether subtitles are rendered/exported for this clip. When `false`,
    /// the segment data is preserved (transcript editor and voice isolation
    /// keep working) but the preview overlay, export burn-in, and SRT
    /// sidecar all skip the clip.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) subtitle_visible: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) subtitle_shadow_color: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) subtitle_shadow_offset_x: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) subtitle_shadow_offset_y: Option<f64>,
    /// Multi-effect karaoke highlight flags (replaces single-mode enum).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) subtitle_highlight_flags: Option<SubtitleHighlightFlags>,
    /// Background highlight color behind the active word.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) subtitle_bg_highlight_color: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) subtitle_color: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) subtitle_outline_color: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) subtitle_outline_width: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) subtitle_bg_box: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) subtitle_bg_box_color: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) subtitle_highlight_mode: Option<SubtitleHighlightMode>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) subtitle_highlight_color: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) subtitle_highlight_stroke_color: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) subtitle_word_window_secs: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) subtitle_position_y: Option<f64>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub(crate) struct UltimateSliceTrackOtioMetadata {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) muted: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) locked: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) soloed: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) audio_role: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) duck: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) duck_amount_db: Option<f64>,
    /// Per-track surround channel routing override for advanced audio mode
    /// (5.1 / 7.1 surround exports). Stored as the snake_case `as_str()`
    /// representation of `SurroundPositionOverride`. `None` (or missing on
    /// older OTIO files) means `Auto`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) surround_position: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub(crate) struct UltimateSliceProjectOtioMetadata {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) width: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) height: Option<u32>,
    /// Project master audio gain in dB (Loudness Radar normalize target).
    /// `None` means "not authored" and reimport leaves the project default
    /// (0.0). Serialized only when non-zero so pre-loudness projects don't
    /// gain any new bytes on round-trip.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) master_gain_db: Option<f64>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub(crate) struct UltimateSliceTransitionOtioMetadata {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) transition_kind: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) transition_alignment: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub(crate) struct UltimateSliceMarkerOtioMetadata {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) color_rgba: Option<String>,
}

fn wrap_section<T: Serialize>(section: &str, payload: &T) -> Value {
    let mut ultimateslice = serde_json::Map::new();
    ultimateslice.insert(
        "version".to_string(),
        Value::from(ULTIMATESLICE_OTIO_METADATA_VERSION),
    );
    let payload_value = match serde_json::to_value(payload) {
        Ok(value) => value,
        Err(err) => {
            log::error!("failed to serialize OTIO ultimateslice {section} metadata: {err}");
            Value::Null
        }
    };
    ultimateslice.insert(section.to_string(), payload_value);

    let mut root = serde_json::Map::new();
    root.insert("ultimateslice".to_string(), Value::Object(ultimateslice));
    Value::Object(root)
}

fn parse_section<T: DeserializeOwned>(metadata: &Value, section: &str) -> Option<T> {
    let us = metadata.get("ultimateslice")?;
    let candidate = us.get(section).cloned().unwrap_or_else(|| us.clone());
    match serde_json::from_value(candidate) {
        Ok(parsed) => Some(parsed),
        Err(err) => {
            log::warn!("failed to parse OTIO ultimateslice {section} metadata: {err}");
            None
        }
    }
}

pub(crate) fn wrap_clip_metadata(metadata: &UltimateSliceClipOtioMetadata) -> Value {
    wrap_section("clip", metadata)
}

pub(crate) fn wrap_track_metadata(metadata: &UltimateSliceTrackOtioMetadata) -> Value {
    wrap_section("track", metadata)
}

pub(crate) fn wrap_project_metadata(metadata: &UltimateSliceProjectOtioMetadata) -> Value {
    wrap_section("project", metadata)
}

pub(crate) fn wrap_transition_metadata(metadata: &UltimateSliceTransitionOtioMetadata) -> Value {
    wrap_section("transition", metadata)
}

pub(crate) fn wrap_marker_metadata(metadata: &UltimateSliceMarkerOtioMetadata) -> Value {
    wrap_section("marker", metadata)
}

pub(crate) fn clip_metadata_from_root(metadata: &Value) -> Option<UltimateSliceClipOtioMetadata> {
    parse_section(metadata, "clip")
}

pub(crate) fn track_metadata_from_root(metadata: &Value) -> Option<UltimateSliceTrackOtioMetadata> {
    parse_section(metadata, "track")
}

pub(crate) fn project_metadata_from_root(
    metadata: &Value,
) -> Option<UltimateSliceProjectOtioMetadata> {
    parse_section(metadata, "project")
}

pub(crate) fn transition_metadata_from_root(
    metadata: &Value,
) -> Option<UltimateSliceTransitionOtioMetadata> {
    parse_section(metadata, "transition")
}

pub(crate) fn marker_metadata_from_root(
    metadata: &Value,
) -> Option<UltimateSliceMarkerOtioMetadata> {
    parse_section(metadata, "marker")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn wrap_clip_metadata_emits_versioned_section() {
        let value = wrap_clip_metadata(&UltimateSliceClipOtioMetadata {
            kind: Some("Title".to_string()),
            speed: Some(1.25),
            scale: Some(1.5),
            ..UltimateSliceClipOtioMetadata::default()
        });

        assert_eq!(
            value["ultimateslice"]["version"].as_u64(),
            Some(ULTIMATESLICE_OTIO_METADATA_VERSION as u64)
        );
        assert_eq!(value["ultimateslice"]["clip"]["kind"], "Title");
        assert_eq!(value["ultimateslice"]["clip"]["speed"], 1.25);
        assert_eq!(value["ultimateslice"]["clip"]["scale"], 1.5);
    }

    #[test]
    fn parse_clip_metadata_accepts_legacy_flat_shape() {
        let value = json!({
            "ultimateslice": {
                "kind": "Video",
                "speed": 0.5,
                "reverse": true
            }
        });

        let parsed = clip_metadata_from_root(&value).expect("clip metadata should parse");
        assert_eq!(parsed.kind.as_deref(), Some("Video"));
        assert_eq!(parsed.speed, Some(0.5));
        assert_eq!(parsed.reverse, Some(true));
    }

    #[test]
    fn parse_track_metadata_accepts_nested_structured_shape() {
        let value = json!({
            "ultimateslice": {
                "version": 1,
                "track": {
                    "muted": true,
                    "audio_role": "dialogue"
                }
            }
        });

        let parsed = track_metadata_from_root(&value).expect("track metadata should parse");
        assert_eq!(parsed.muted, Some(true));
        assert_eq!(parsed.audio_role.as_deref(), Some("dialogue"));
    }

    #[test]
    fn clip_metadata_roundtrips_user_eq_and_match_eq_bands() {
        let user_eq = [
            EqBand {
                freq: 200.0,
                gain: -2.5,
                q: 1.0,
            },
            EqBand {
                freq: 1000.0,
                gain: 1.0,
                q: 1.2,
            },
            EqBand {
                freq: 5000.0,
                gain: 3.5,
                q: 0.9,
            },
        ];
        let match_eq = vec![
            EqBand {
                freq: 100.0,
                gain: -3.0,
                q: 1.5,
            },
            EqBand {
                freq: 200.0,
                gain: -2.0,
                q: 1.0,
            },
            EqBand {
                freq: 400.0,
                gain: 0.0,
                q: 1.5,
            },
            EqBand {
                freq: 800.0,
                gain: 1.0,
                q: 1.0,
            },
            EqBand {
                freq: 2000.0,
                gain: 3.0,
                q: 1.0,
            },
            EqBand {
                freq: 5000.0,
                gain: 2.5,
                q: 1.0,
            },
            EqBand {
                freq: 9000.0,
                gain: 0.0,
                q: 1.5,
            },
        ];
        let value = wrap_clip_metadata(&UltimateSliceClipOtioMetadata {
            eq_bands: Some(user_eq),
            match_eq_bands: Some(match_eq.clone()),
            ..UltimateSliceClipOtioMetadata::default()
        });

        let parsed = clip_metadata_from_root(&value).expect("clip metadata should parse");
        let parsed_user_eq = parsed.eq_bands.expect("user eq should round-trip");
        assert_eq!(parsed_user_eq[0].gain, -2.5);
        assert_eq!(parsed_user_eq[1].freq, 1000.0);
        assert_eq!(parsed_user_eq[2].q, 0.9);
        let parsed_match_eq = parsed.match_eq_bands.expect("match eq should round-trip");
        assert_eq!(parsed_match_eq.len(), 7);
        assert_eq!(parsed_match_eq[0].gain, -3.0);
        assert_eq!(parsed_match_eq[4].gain, 3.0);
    }

    #[test]
    fn clip_metadata_omits_empty_match_eq_bands() {
        let value = wrap_clip_metadata(&UltimateSliceClipOtioMetadata::default());
        // Both fields are Option::None by default — must not appear in JSON.
        assert!(value["ultimateslice"]["clip"]
            .get("match_eq_bands")
            .is_none());
        assert!(value["ultimateslice"]["clip"].get("eq_bands").is_none());
    }

    #[test]
    fn clip_metadata_roundtrips_transform_keyframes_and_blend_mode() {
        let value = wrap_clip_metadata(&UltimateSliceClipOtioMetadata {
            blend_mode: Some(BlendMode::Screen),
            position_x: Some(0.25),
            scale_keyframes: Some(vec![NumericKeyframe {
                time_ns: 1_000_000_000,
                value: 1.35,
                interpolation: crate::model::clip::KeyframeInterpolation::EaseInOut,
                bezier_controls: Some(crate::model::clip::BezierControls {
                    x1: 0.2,
                    y1: 0.0,
                    x2: 0.8,
                    y2: 1.0,
                }),
            }]),
            ..UltimateSliceClipOtioMetadata::default()
        });

        let parsed = clip_metadata_from_root(&value).expect("clip metadata should parse");
        assert_eq!(parsed.blend_mode, Some(BlendMode::Screen));
        assert_eq!(parsed.position_x, Some(0.25));
        assert_eq!(
            parsed
                .scale_keyframes
                .expect("scale keyframes should round-trip")[0]
                .value,
            1.35
        );
    }
}
