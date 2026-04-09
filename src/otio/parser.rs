//! Import an OpenTimelineIO JSON file into an UltimateSlice `Project`.

use anyhow::{Context, Result};
use serde_json::Value;
use std::path::{Component, Path, PathBuf};

use crate::model::clip::{Clip, ClipKind};
use crate::model::project::{FrameRate, Marker, Project};
use crate::model::track::{AudioRole, Track, TrackKind};
use crate::model::transition::{OutgoingTransition, TransitionAlignment};

use super::metadata::{
    clip_metadata_from_root, marker_metadata_from_root, project_metadata_from_root,
    track_metadata_from_root, transition_metadata_from_root,
};
use super::schema::*;

/// Parse an OTIO JSON string into a `Project`.
pub fn parse_otio(json: &str) -> Result<Project> {
    parse_otio_with_path(json, None)
}

pub fn parse_otio_with_path(json: &str, otio_path: Option<&Path>) -> Result<Project> {
    let timeline: OtioTimeline = serde_json::from_str(json).context("failed to parse OTIO JSON")?;
    let otio_dir = otio_path.and_then(Path::parent);

    // -- Frame rate ---------------------------------------------------------
    let rate = timeline
        .global_start_time
        .as_ref()
        .map(|rt| rt.rate)
        .unwrap_or(24.0);
    let frame_rate = rate_to_frame_rate(rate);

    // -- Resolution from metadata -------------------------------------------
    let (width, height) = extract_resolution(&timeline.metadata);

    let mut project = Project::new(&timeline.name);
    project.width = width;
    project.height = height;
    project.frame_rate = frame_rate;
    // Clear default tracks that Project::new creates.
    project.tracks.clear();

    // -- Markers collected from all tracks ----------------------------------
    let mut all_markers: Vec<Marker> = Vec::new();

    // -- Tracks -------------------------------------------------------------
    for otio_track in &timeline.tracks.children {
        let kind = match otio_track.kind.as_str() {
            "Audio" => TrackKind::Audio,
            _ => TrackKind::Video,
        };

        let mut track = match kind {
            TrackKind::Video => Track::new_video(&otio_track.name),
            TrackKind::Audio => Track::new_audio(&otio_track.name),
        };

        // Restore track metadata if present.
        if let Some(us) = track_metadata_from_root(&otio_track.metadata) {
            if let Some(v) = us.muted {
                track.muted = v;
            }
            if let Some(v) = us.locked {
                track.locked = v;
            }
            if let Some(v) = us.soloed {
                track.soloed = v;
            }
            if let Some(role) = us.audio_role.as_deref() {
                track.audio_role = AudioRole::from_str(role);
            }
            if let Some(v) = us.duck {
                track.duck = v;
            }
            if let Some(db) = us.duck_amount_db {
                track.duck_amount_db = db;
            }
        }

        // Walk children: Clips advance cursor, Gaps advance cursor without
        // creating a clip, Transitions attach to the *preceding* clip.
        let mut cursor_ns: u64 = 0;

        for child in &otio_track.children {
            match child {
                OtioTrackChild::Gap(gap) => {
                    if let Some(ref sr) = gap.source_range {
                        cursor_ns += rational_time_to_ns(&sr.duration);
                    }
                }

                OtioTrackChild::Clip(otio_clip) => {
                    let clip = otio_clip_to_clip(otio_clip, cursor_ns, kind, rate, otio_dir);
                    let dur = clip.duration();
                    track.clips.push(clip);
                    cursor_ns += dur;
                }

                OtioTrackChild::Transition(trans) => {
                    // Attach transition info to the preceding clip.
                    if let Some(prev) = track.clips.last_mut() {
                        let metadata = transition_metadata_from_root(&trans.metadata);
                        let kind_name = metadata.as_ref().and_then(|us| us.transition_kind.clone());
                        let transition_name =
                            kind_name.unwrap_or_else(|| match trans.transition_type.as_str() {
                                "SMPTE_Dissolve" => "cross_dissolve".into(),
                                _ => trans.transition_type.clone(),
                            });
                        let in_ns = rational_time_to_ns(&trans.in_offset);
                        let out_ns = rational_time_to_ns(&trans.out_offset);
                        let duration_ns = in_ns + out_ns;
                        let alignment = metadata
                            .as_ref()
                            .and_then(|us| us.transition_alignment.as_deref())
                            .and_then(TransitionAlignment::from_str)
                            .unwrap_or_else(|| {
                                TransitionAlignment::from_before_cut_duration(in_ns, duration_ns)
                            });
                        prev.outgoing_transition =
                            OutgoingTransition::new(transition_name, duration_ns, alignment);
                    }
                }
            }
        }

        // Collect track-level markers.
        for m in &otio_track.markers {
            let pos_ns = rational_time_to_ns(&m.marked_range.start_time);
            let color = parse_marker_color(m);
            all_markers.push(Marker {
                id: uuid::Uuid::new_v4().to_string(),
                position_ns: pos_ns,
                label: m.name.clone(),
                color,
            });
        }

        project.tracks.push(track);
    }

    project.markers = all_markers;
    project.dirty = true;
    Ok(project)
}

/// Convert a single `OtioClip` to a model `Clip`.
fn otio_clip_to_clip(
    otio_clip: &OtioClip,
    timeline_start_ns: u64,
    track_kind: TrackKind,
    _rate: f64,
    otio_dir: Option<&Path>,
) -> Clip {
    // Source range.
    let (source_in, source_out) = otio_clip
        .source_range
        .as_ref()
        .map(|sr| {
            let sin = rational_time_to_ns(&sr.start_time);
            let dur = rational_time_to_ns(&sr.duration);
            (sin, sin + dur)
        })
        .unwrap_or((0, 0));

    // Source path from media reference.
    let source_path = match &otio_clip.media_reference {
        Some(OtioMediaReference::External(ext)) => url_to_path(&ext.target_url, otio_dir),
        _ => String::new(),
    };

    // Clip kind — check UltimateSlice metadata first, else derive from track.
    let us_meta = clip_metadata_from_root(&otio_clip.metadata);
    let kind = us_meta
        .as_ref()
        .and_then(|us| us.kind.as_deref())
        .and_then(parse_clip_kind)
        .unwrap_or(match track_kind {
            TrackKind::Video => {
                if source_path.is_empty() {
                    ClipKind::Title
                } else {
                    ClipKind::Video
                }
            }
            TrackKind::Audio => ClipKind::Audio,
        });

    let mut clip = Clip::new(
        &source_path,
        source_out.saturating_sub(source_in),
        timeline_start_ns,
        kind,
    );
    clip.source_in = source_in;
    clip.source_out = source_out;
    clip.label = otio_clip.name.clone();

    // Restore UltimateSlice-specific metadata when available.
    if let Some(us) = us_meta.as_ref() {
        if let Some(v) = us.speed {
            clip.speed = v;
        }
        if let Some(v) = us.reverse {
            clip.reverse = v;
        }
        if let Some(v) = us.volume {
            clip.volume = v as f32;
        }
        if let Some(v) = us.pan {
            clip.pan = v as f32;
        }
        if let Some(v) = us.eq_bands {
            clip.eq_bands = v;
        }
        if let Some(v) = us.match_eq_bands.clone() {
            clip.match_eq_bands = v;
        }
        if let Some(v) = us.voice_isolation {
            clip.voice_isolation = v as f32;
        }
        if let Some(v) = us.voice_isolation_pad_ms {
            clip.voice_isolation_pad_ms = v as f32;
        }
        if let Some(v) = us.voice_isolation_fade_ms {
            clip.voice_isolation_fade_ms = v as f32;
        }
        if let Some(v) = us.voice_isolation_floor {
            clip.voice_isolation_floor = v as f32;
        }
        if let Some(v) = us.voice_isolation_source {
            clip.voice_isolation_source = v;
        }
        if let Some(v) = us.voice_isolation_silence_threshold_db {
            clip.voice_isolation_silence_threshold_db = v as f32;
        }
        if let Some(v) = us.voice_isolation_silence_min_ms {
            clip.voice_isolation_silence_min_ms = v;
        }
        if let Some(v) = us.measured_loudness_lufs {
            clip.measured_loudness_lufs = Some(v);
        }
        if let Some(v) = us.chroma_key_enabled {
            clip.chroma_key_enabled = v;
        }
        if let Some(v) = us.chroma_key_color {
            clip.chroma_key_color = v;
        }
        if let Some(v) = us.chroma_key_tolerance {
            clip.chroma_key_tolerance = v as f32;
        }
        if let Some(v) = us.chroma_key_softness {
            clip.chroma_key_softness = v as f32;
        }
        if let Some(v) = us.bg_removal_enabled {
            clip.bg_removal_enabled = v;
        }
        if let Some(v) = us.bg_removal_threshold {
            clip.bg_removal_threshold = v;
        }
        if let Some(v) = us.freeze_frame {
            clip.freeze_frame = v;
        }
        if let Some(v) = us.freeze_frame_source_ns {
            clip.freeze_frame_source_ns = Some(v);
        }
        if let Some(v) = us.freeze_frame_hold_duration_ns {
            clip.freeze_frame_hold_duration_ns = Some(v);
        }
        if let Some(v) = us.vidstab_enabled {
            clip.vidstab_enabled = v;
        }
        if let Some(v) = us.vidstab_smoothing {
            clip.vidstab_smoothing = v as f32;
        }
        if let Some(v) = us.color_label {
            clip.color_label = v;
        }
        if let Some(v) = us.anamorphic_desqueeze {
            clip.anamorphic_desqueeze = v;
        }
        if let Some(v) = us.group_id.as_ref() {
            clip.group_id = Some(v.clone());
        }
        if let Some(v) = us.link_group_id.as_ref() {
            clip.link_group_id = Some(v.clone());
        }
        if let Some(v) = us.source_timecode_base_ns {
            clip.source_timecode_base_ns = Some(v);
        }
        if let Some(v) = us.animated_svg {
            clip.animated_svg = v;
        }
        if let Some(v) = us.brightness {
            clip.brightness = v as f32;
        }
        if let Some(v) = us.contrast {
            clip.contrast = v as f32;
        }
        if let Some(v) = us.saturation {
            clip.saturation = v as f32;
        }
        if let Some(v) = us.temperature {
            clip.temperature = v as f32;
        }
        if let Some(v) = us.tint {
            clip.tint = v as f32;
        }
        if let Some(v) = us.denoise {
            clip.denoise = v as f32;
        }
        if let Some(v) = us.sharpness {
            clip.sharpness = v as f32;
        }
        if let Some(v) = us.blur {
            clip.blur = v as f32;
        }
        if let Some(v) = us.shadows {
            clip.shadows = v as f32;
        }
        if let Some(v) = us.midtones {
            clip.midtones = v as f32;
        }
        if let Some(v) = us.highlights {
            clip.highlights = v as f32;
        }
        if let Some(v) = us.exposure {
            clip.exposure = v as f32;
        }
        if let Some(v) = us.black_point {
            clip.black_point = v as f32;
        }
        if let Some(v) = us.highlights_warmth {
            clip.highlights_warmth = v as f32;
        }
        if let Some(v) = us.highlights_tint {
            clip.highlights_tint = v as f32;
        }
        if let Some(v) = us.midtones_warmth {
            clip.midtones_warmth = v as f32;
        }
        if let Some(v) = us.midtones_tint {
            clip.midtones_tint = v as f32;
        }
        if let Some(v) = us.shadows_warmth {
            clip.shadows_warmth = v as f32;
        }
        if let Some(v) = us.shadows_tint {
            clip.shadows_tint = v as f32;
        }
        if let Some(v) = us.pitch_shift_semitones {
            clip.pitch_shift_semitones = v;
        }
        if let Some(v) = us.pitch_preserve {
            clip.pitch_preserve = v;
        }
        if let Some(v) = us.audio_channel_mode {
            clip.audio_channel_mode = v;
        }
        if let Some(v) = us.speed_keyframes.as_ref() {
            clip.speed_keyframes = v.clone();
        }
        if let Some(v) = us.slow_motion_interp {
            clip.slow_motion_interp = v;
        }
        if let Some(v) = us.lut_paths.as_ref() {
            clip.lut_paths = v.clone();
        }
        if let Some(v) = us.brightness_keyframes.as_ref() {
            clip.brightness_keyframes = v.clone();
        }
        if let Some(v) = us.contrast_keyframes.as_ref() {
            clip.contrast_keyframes = v.clone();
        }
        if let Some(v) = us.saturation_keyframes.as_ref() {
            clip.saturation_keyframes = v.clone();
        }
        if let Some(v) = us.temperature_keyframes.as_ref() {
            clip.temperature_keyframes = v.clone();
        }
        if let Some(v) = us.tint_keyframes.as_ref() {
            clip.tint_keyframes = v.clone();
        }
        if let Some(v) = us.blur_keyframes.as_ref() {
            clip.blur_keyframes = v.clone();
        }
        if let Some(v) = us.volume_keyframes.as_ref() {
            clip.volume_keyframes = v.clone();
        }
        if let Some(v) = us.pan_keyframes.as_ref() {
            clip.pan_keyframes = v.clone();
        }
        if let Some(v) = us.eq_low_gain_keyframes.as_ref() {
            clip.eq_low_gain_keyframes = v.clone();
        }
        if let Some(v) = us.eq_mid_gain_keyframes.as_ref() {
            clip.eq_mid_gain_keyframes = v.clone();
        }
        if let Some(v) = us.eq_high_gain_keyframes.as_ref() {
            clip.eq_high_gain_keyframes = v.clone();
        }
        if let Some(v) = us.crop_left_keyframes.as_ref() {
            clip.crop_left_keyframes = v.clone();
        }
        if let Some(v) = us.crop_right_keyframes.as_ref() {
            clip.crop_right_keyframes = v.clone();
        }
        if let Some(v) = us.crop_top_keyframes.as_ref() {
            clip.crop_top_keyframes = v.clone();
        }
        if let Some(v) = us.crop_bottom_keyframes.as_ref() {
            clip.crop_bottom_keyframes = v.clone();
        }
        if let Some(v) = us.opacity {
            clip.opacity = v;
        }
        if let Some(v) = us.scale {
            clip.scale = v;
        }
        if let Some(v) = us.position_x {
            clip.position_x = v;
        }
        if let Some(v) = us.position_y {
            clip.position_y = v;
        }
        if let Some(v) = us.rotate {
            clip.rotate = v;
        }
        if let Some(v) = us.flip_h {
            clip.flip_h = v;
        }
        if let Some(v) = us.flip_v {
            clip.flip_v = v;
        }
        if let Some(v) = us.crop_left {
            clip.crop_left = v;
        }
        if let Some(v) = us.crop_right {
            clip.crop_right = v;
        }
        if let Some(v) = us.crop_top {
            clip.crop_top = v;
        }
        if let Some(v) = us.crop_bottom {
            clip.crop_bottom = v;
        }
        if let Some(v) = us.blend_mode {
            clip.blend_mode = v;
        }
        if let Some(v) = us.opacity_keyframes.as_ref() {
            clip.opacity_keyframes = v.clone();
        }
        if let Some(v) = us.scale_keyframes.as_ref() {
            clip.scale_keyframes = v.clone();
        }
        if let Some(v) = us.position_x_keyframes.as_ref() {
            clip.position_x_keyframes = v.clone();
        }
        if let Some(v) = us.position_y_keyframes.as_ref() {
            clip.position_y_keyframes = v.clone();
        }
        if let Some(v) = us.rotate_keyframes.as_ref() {
            clip.rotate_keyframes = v.clone();
        }
        if let Some(v) = us.title_text.as_ref() {
            clip.title_text = v.clone();
        }
        if let Some(v) = us.title_font.as_ref() {
            clip.title_font = v.clone();
        }
        if let Some(v) = us.title_color {
            clip.title_color = v;
        }
        if let Some(v) = us.title_x {
            clip.title_x = v;
        }
        if let Some(v) = us.title_y {
            clip.title_y = v;
        }
        if let Some(v) = us.title_template.as_ref() {
            clip.title_template = v.clone();
        }
        if let Some(v) = us.title_outline_color {
            clip.title_outline_color = v;
        }
        if let Some(v) = us.title_outline_width {
            clip.title_outline_width = v;
        }
        if let Some(v) = us.title_shadow {
            clip.title_shadow = v;
        }
        if let Some(v) = us.title_shadow_color {
            clip.title_shadow_color = v;
        }
        if let Some(v) = us.title_shadow_offset_x {
            clip.title_shadow_offset_x = v;
        }
        if let Some(v) = us.title_shadow_offset_y {
            clip.title_shadow_offset_y = v;
        }
        if let Some(v) = us.title_bg_box {
            clip.title_bg_box = v;
        }
        if let Some(v) = us.title_bg_box_color {
            clip.title_bg_box_color = v;
        }
        if let Some(v) = us.title_bg_box_padding {
            clip.title_bg_box_padding = v;
        }
        if let Some(v) = us.title_clip_bg_color {
            clip.title_clip_bg_color = v;
        }
        if let Some(v) = us.title_secondary_text.as_ref() {
            clip.title_secondary_text = v.clone();
        }
        if let Some(v) = us.subtitle_segments.as_ref() {
            clip.subtitle_segments = v.clone();
        }
        if let Some(v) = us.subtitles_language.as_ref() {
            clip.subtitles_language = v.clone();
        }
        if let Some(v) = us.subtitle_font.as_ref() {
            clip.subtitle_font = v.clone();
        }
        if let Some(v) = us.subtitle_color {
            clip.subtitle_color = v;
        }
        if let Some(v) = us.subtitle_outline_color {
            clip.subtitle_outline_color = v;
        }
        if let Some(v) = us.subtitle_outline_width {
            clip.subtitle_outline_width = v;
        }
        if let Some(v) = us.subtitle_bg_box {
            clip.subtitle_bg_box = v;
        }
        if let Some(v) = us.subtitle_bg_box_color {
            clip.subtitle_bg_box_color = v;
        }
        if let Some(v) = us.subtitle_highlight_mode {
            clip.subtitle_highlight_mode = v;
        }
        if let Some(v) = us.subtitle_highlight_color {
            clip.subtitle_highlight_color = v;
        }
        if let Some(v) = us.subtitle_word_window_secs {
            clip.subtitle_word_window_secs = v;
        }
        if let Some(v) = us.subtitle_position_y {
            clip.subtitle_position_y = v;
        }
    }

    if us_meta.as_ref().and_then(|us| us.speed).is_none() {
        // Check OTIO effects for speed (LinearTimeWarp from other tools).
        for eff in &otio_clip.effects {
            if eff.name == "LinearTimeWarp" {
                if let Some(scalar) = eff.metadata.get("time_scalar").and_then(|v| v.as_f64()) {
                    clip.speed = scalar;
                }
            }
        }
    }

    clip
}

/// Convert an OTIO media reference URL/path to a local path.
fn url_to_path(url: &str, otio_dir: Option<&Path>) -> String {
    if !url.is_empty() && url.contains("://") && !url.starts_with("file://") {
        return url.to_string();
    }
    let stripped = url.strip_prefix("file://").unwrap_or(url);
    // Decode percent-encoded characters.
    let decoded = percent_decode(stripped);
    let decoded_path = Path::new(&decoded);
    if decoded.is_empty() || decoded_path.is_absolute() || otio_dir.is_none() {
        return decoded;
    }
    normalize_joined_path(otio_dir.expect("checked is_some above").join(decoded_path))
        .to_string_lossy()
        .to_string()
}

fn normalize_joined_path(path: PathBuf) -> PathBuf {
    let mut normalized = PathBuf::new();
    let is_absolute = path.is_absolute();
    for component in path.components() {
        match component {
            Component::Prefix(prefix) => normalized.push(prefix.as_os_str()),
            Component::RootDir => normalized.push(component.as_os_str()),
            Component::CurDir => {}
            Component::ParentDir => {
                if !normalized.pop() && !is_absolute {
                    normalized.push("..");
                }
            }
            Component::Normal(part) => normalized.push(part),
        }
    }
    normalized
}

/// Simple percent-decode (%20 → space, etc.).
fn percent_decode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let Ok(byte) =
                u8::from_str_radix(std::str::from_utf8(&bytes[i + 1..i + 3]).unwrap_or(""), 16)
            {
                out.push(byte as char);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i] as char);
        i += 1;
    }
    out
}

/// Convert a floating-point frame rate to the closest standard `FrameRate`.
fn rate_to_frame_rate(rate: f64) -> FrameRate {
    // Common rates: 23.976, 24, 25, 29.97, 30, 48, 50, 59.94, 60
    let candidates: &[(u32, u32)] = &[
        (24000, 1001), // 23.976
        (24, 1),
        (25, 1),
        (30000, 1001), // 29.97
        (30, 1),
        (48, 1),
        (50, 1),
        (60000, 1001), // 59.94
        (60, 1),
    ];

    let mut best = (24u32, 1u32);
    let mut best_diff = f64::MAX;
    for &(num, den) in candidates {
        let candidate_rate = num as f64 / den as f64;
        let diff = (candidate_rate - rate).abs();
        if diff < best_diff {
            best_diff = diff;
            best = (num, den);
        }
    }

    // If nothing is close (> 0.5 fps off), use the rate directly.
    if best_diff > 0.5 {
        let rounded = rate.round() as u32;
        FrameRate {
            numerator: rounded.max(1),
            denominator: 1,
        }
    } else {
        FrameRate {
            numerator: best.0,
            denominator: best.1,
        }
    }
}

/// Extract width/height from timeline metadata, defaulting to 1920×1080.
fn extract_resolution(metadata: &Value) -> (u32, u32) {
    let us = project_metadata_from_root(metadata).unwrap_or_default();
    (us.width.unwrap_or(1920), us.height.unwrap_or(1080))
}

/// Parse a `ClipKind` debug name back to the enum.
fn parse_clip_kind(s: &str) -> Option<ClipKind> {
    match s {
        "Video" => Some(ClipKind::Video),
        "Audio" => Some(ClipKind::Audio),
        "Image" => Some(ClipKind::Image),
        "Title" => Some(ClipKind::Title),
        "Adjustment" => Some(ClipKind::Adjustment),
        "Compound" => Some(ClipKind::Compound),
        "Multicam" => Some(ClipKind::Multicam),
        _ => None,
    }
}

/// Parse marker color from OTIO marker metadata or color name.
fn parse_marker_color(m: &OtioMarker) -> u32 {
    // Try UltimateSlice metadata first.
    if let Some(hex) = marker_metadata_from_root(&m.metadata).and_then(|us| us.color_rgba) {
        if let Ok(rgba) = u32::from_str_radix(&hex, 16) {
            return rgba;
        }
    }
    // Fall back to OTIO colour name.
    match m.color.as_str() {
        "RED" => 0xFF0000FF,
        "GREEN" => 0x00FF00FF,
        "BLUE" => 0x0000FFFF,
        "YELLOW" => 0xFFFF00FF,
        "ORANGE" => 0xFF8C00FF,
        "WHITE" => 0xFFFFFFFF,
        "BLACK" => 0x000000FF,
        "MAGENTA" | "PINK" => 0xFF00FFFF,
        "CYAN" => 0x00FFFFFF,
        _ => 0xFF8C00FF, // default orange
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn minimal_otio() -> String {
        r#"{
            "OTIO_SCHEMA": "Timeline.1",
            "name": "Test",
            "global_start_time": { "OTIO_SCHEMA": "RationalTime.1", "value": 0.0, "rate": 24.0 },
            "tracks": {
                "OTIO_SCHEMA": "Stack.1",
                "name": "tracks",
                "children": []
            }
        }"#
        .into()
    }

    #[test]
    fn test_parse_minimal() {
        let p = parse_otio(&minimal_otio()).unwrap();
        assert_eq!(p.title, "Test");
        assert_eq!(p.frame_rate.numerator, 24);
        assert_eq!(p.frame_rate.denominator, 1);
        assert!(p.tracks.is_empty());
    }

    #[test]
    fn test_parse_single_clip() {
        let json = r#"{
            "OTIO_SCHEMA": "Timeline.1",
            "name": "One Clip",
            "global_start_time": { "OTIO_SCHEMA": "RationalTime.1", "value": 0.0, "rate": 24.0 },
            "tracks": {
                "OTIO_SCHEMA": "Stack.1",
                "name": "tracks",
                "children": [{
                    "OTIO_SCHEMA": "Track.1",
                    "name": "V1",
                    "kind": "Video",
                    "children": [{
                        "OTIO_SCHEMA": "Clip.1",
                        "name": "shot_01",
                        "source_range": {
                            "OTIO_SCHEMA": "TimeRange.1",
                            "start_time": { "OTIO_SCHEMA": "RationalTime.1", "value": 0.0, "rate": 24.0 },
                            "duration": { "OTIO_SCHEMA": "RationalTime.1", "value": 48.0, "rate": 24.0 }
                        },
                        "media_reference": {
                            "OTIO_SCHEMA": "ExternalReference.1",
                            "target_url": "file:///footage/clip1.mp4"
                        }
                    }]
                }]
            }
        }"#;
        let p = parse_otio(json).unwrap();
        assert_eq!(p.tracks.len(), 1);
        assert_eq!(p.tracks[0].clips.len(), 1);
        let clip = &p.tracks[0].clips[0];
        assert_eq!(clip.label, "shot_01");
        assert_eq!(clip.source_path, "/footage/clip1.mp4");
        assert_eq!(clip.timeline_start, 0);
        // 48 frames at 24fps = 2 seconds = 2_000_000_000 ns
        assert!((clip.source_out as i64 - 2_000_000_000i64).unsigned_abs() < 42_000_000);
    }

    #[test]
    fn test_parse_with_gap() {
        let json = r#"{
            "OTIO_SCHEMA": "Timeline.1",
            "name": "Gap Test",
            "global_start_time": { "OTIO_SCHEMA": "RationalTime.1", "value": 0.0, "rate": 24.0 },
            "tracks": {
                "OTIO_SCHEMA": "Stack.1",
                "name": "tracks",
                "children": [{
                    "OTIO_SCHEMA": "Track.1",
                    "name": "V1",
                    "kind": "Video",
                    "children": [
                        {
                            "OTIO_SCHEMA": "Clip.1",
                            "name": "A",
                            "source_range": {
                                "OTIO_SCHEMA": "TimeRange.1",
                                "start_time": { "OTIO_SCHEMA": "RationalTime.1", "value": 0.0, "rate": 24.0 },
                                "duration": { "OTIO_SCHEMA": "RationalTime.1", "value": 24.0, "rate": 24.0 }
                            },
                            "media_reference": {
                                "OTIO_SCHEMA": "ExternalReference.1",
                                "target_url": "file:///a.mp4"
                            }
                        },
                        {
                            "OTIO_SCHEMA": "Gap.1",
                            "name": "",
                            "source_range": {
                                "OTIO_SCHEMA": "TimeRange.1",
                                "start_time": { "OTIO_SCHEMA": "RationalTime.1", "value": 0.0, "rate": 24.0 },
                                "duration": { "OTIO_SCHEMA": "RationalTime.1", "value": 48.0, "rate": 24.0 }
                            }
                        },
                        {
                            "OTIO_SCHEMA": "Clip.1",
                            "name": "B",
                            "source_range": {
                                "OTIO_SCHEMA": "TimeRange.1",
                                "start_time": { "OTIO_SCHEMA": "RationalTime.1", "value": 0.0, "rate": 24.0 },
                                "duration": { "OTIO_SCHEMA": "RationalTime.1", "value": 24.0, "rate": 24.0 }
                            },
                            "media_reference": {
                                "OTIO_SCHEMA": "ExternalReference.1",
                                "target_url": "file:///b.mp4"
                            }
                        }
                    ]
                }]
            }
        }"#;
        let p = parse_otio(json).unwrap();
        assert_eq!(p.tracks[0].clips.len(), 2);
        let a = &p.tracks[0].clips[0];
        let b = &p.tracks[0].clips[1];
        assert_eq!(a.timeline_start, 0);
        // A is 24 frames = 1s, gap is 48 frames = 2s, so B starts at 3s.
        let expected_b_start: u64 = 3_000_000_000;
        assert!((b.timeline_start as i64 - expected_b_start as i64).unsigned_abs() < 42_000_000);
    }

    #[test]
    fn test_parse_with_transition() {
        let json = r#"{
            "OTIO_SCHEMA": "Timeline.1",
            "name": "Trans Test",
            "global_start_time": { "OTIO_SCHEMA": "RationalTime.1", "value": 0.0, "rate": 24.0 },
            "tracks": {
                "OTIO_SCHEMA": "Stack.1",
                "name": "tracks",
                "children": [{
                    "OTIO_SCHEMA": "Track.1",
                    "name": "V1",
                    "kind": "Video",
                    "children": [
                        {
                            "OTIO_SCHEMA": "Clip.1",
                            "name": "A",
                            "source_range": {
                                "OTIO_SCHEMA": "TimeRange.1",
                                "start_time": { "OTIO_SCHEMA": "RationalTime.1", "value": 0.0, "rate": 24.0 },
                                "duration": { "OTIO_SCHEMA": "RationalTime.1", "value": 72.0, "rate": 24.0 }
                            },
                            "media_reference": {
                                "OTIO_SCHEMA": "ExternalReference.1",
                                "target_url": "file:///a.mp4"
                            }
                        },
                        {
                            "OTIO_SCHEMA": "Transition.1",
                            "name": "cross dissolve",
                            "transition_type": "SMPTE_Dissolve",
                            "in_offset": { "OTIO_SCHEMA": "RationalTime.1", "value": 12.0, "rate": 24.0 },
                            "out_offset": { "OTIO_SCHEMA": "RationalTime.1", "value": 12.0, "rate": 24.0 }
                        },
                        {
                            "OTIO_SCHEMA": "Clip.1",
                            "name": "B",
                            "source_range": {
                                "OTIO_SCHEMA": "TimeRange.1",
                                "start_time": { "OTIO_SCHEMA": "RationalTime.1", "value": 0.0, "rate": 24.0 },
                                "duration": { "OTIO_SCHEMA": "RationalTime.1", "value": 72.0, "rate": 24.0 }
                            },
                            "media_reference": {
                                "OTIO_SCHEMA": "ExternalReference.1",
                                "target_url": "file:///b.mp4"
                            }
                        }
                    ]
                }]
            }
        }"#;
        let p = parse_otio(json).unwrap();
        let a = &p.tracks[0].clips[0];
        assert_eq!(a.outgoing_transition.kind, "cross_dissolve");
        assert!(a.outgoing_transition.duration_ns > 0);
    }

    #[test]
    fn test_roundtrip() {
        use crate::model::clip::Clip;
        use crate::model::track::{AudioRole, Track};

        let mut p = Project::new("Roundtrip");
        p.frame_rate = FrameRate {
            numerator: 24,
            denominator: 1,
        };
        p.tracks.clear();
        let mut track = Track::new_audio("A1");
        track.audio_role = AudioRole::Dialogue;
        track.add_clip(Clip::new(
            "/footage/a.mp4",
            2_000_000_000,
            0,
            ClipKind::Audio,
        ));
        track.add_clip(Clip::new(
            "/footage/b.mp4",
            3_000_000_000,
            5_000_000_000,
            ClipKind::Audio,
        ));
        p.tracks.push(track);

        let json = crate::otio::writer::write_otio(&p).unwrap();
        let p2 = parse_otio(&json).unwrap();

        assert_eq!(p2.title, "Roundtrip");
        assert_eq!(p2.tracks.len(), 1);
        assert_eq!(p2.tracks[0].clips.len(), 2);
        assert_eq!(p2.tracks[0].audio_role, AudioRole::Dialogue);
        // First clip at 0, second at 5s.
        assert_eq!(p2.tracks[0].clips[0].timeline_start, 0);
        let diff = (p2.tracks[0].clips[1].timeline_start as i64 - 5_000_000_000i64).unsigned_abs();
        assert!(diff < 42_000_000, "second clip start off by {diff} ns");
    }

    #[test]
    fn test_parse_legacy_flat_track_metadata_restores_audio_role() {
        let json = r#"{
            "OTIO_SCHEMA": "Timeline.1",
            "name": "Legacy Track Meta",
            "global_start_time": { "OTIO_SCHEMA": "RationalTime.1", "value": 0.0, "rate": 24.0 },
            "tracks": {
                "OTIO_SCHEMA": "Stack.1",
                "name": "tracks",
                "children": [{
                    "OTIO_SCHEMA": "Track.1",
                    "name": "A1",
                    "kind": "Audio",
                    "children": [],
                    "metadata": {
                        "ultimateslice": {
                            "muted": false,
                            "locked": false,
                            "soloed": true,
                            "audio_role": "Dialogue",
                            "duck": true,
                            "duck_amount_db": -9.0
                        }
                    }
                }]
            }
        }"#;

        let p = parse_otio(json).unwrap();
        assert_eq!(p.tracks.len(), 1);
        assert_eq!(p.tracks[0].audio_role, AudioRole::Dialogue);
        assert!(p.tracks[0].soloed);
        assert!(p.tracks[0].duck);
        assert_eq!(p.tracks[0].duck_amount_db, -9.0);
    }

    #[test]
    fn test_roundtrip_title_and_subtitle_metadata() {
        use crate::model::clip::{SubtitleHighlightMode, SubtitleSegment, SubtitleWord};
        use crate::model::track::Track;

        let mut p = Project::new("Text Roundtrip");
        p.frame_rate = FrameRate {
            numerator: 24,
            denominator: 1,
        };
        p.tracks.clear();

        let mut track = Track::new_video("V1");

        let mut title = Clip::new("", 2_000_000_000, 0, ClipKind::Title);
        title.label = "Lower Third".into();
        title.title_text = "Primary".into();
        title.title_font = "Sans Bold 42".into();
        title.title_color = 0xFFCC00FF;
        title.title_x = 0.3;
        title.title_y = 0.7;
        title.title_template = "lower_third".into();
        title.title_outline_color = 0x000000FF;
        title.title_outline_width = 3.0;
        title.title_shadow = true;
        title.title_shadow_color = 0x112233AA;
        title.title_shadow_offset_x = 5.0;
        title.title_shadow_offset_y = 7.0;
        title.title_bg_box = true;
        title.title_bg_box_color = 0x44556677;
        title.title_bg_box_padding = 9.0;
        title.title_clip_bg_color = 0x01020304;
        title.title_secondary_text = "Secondary".into();
        track.add_clip(title);

        let mut clip = Clip::new(
            "/footage/dialogue.mp4",
            3_000_000_000,
            2_500_000_000,
            ClipKind::Video,
        );
        clip.subtitle_segments = vec![SubtitleSegment {
            id: "seg-1".into(),
            start_ns: 100_000_000,
            end_ns: 900_000_000,
            text: "hello world".into(),
            words: vec![
                SubtitleWord {
                    start_ns: 100_000_000,
                    end_ns: 400_000_000,
                    text: "hello".into(),
                },
                SubtitleWord {
                    start_ns: 450_000_000,
                    end_ns: 900_000_000,
                    text: "world".into(),
                },
            ],
        }];
        clip.subtitles_language = "en".into();
        clip.subtitle_font = "Sans Bold 24".into();
        clip.subtitle_color = 0xFFFFFFFF;
        clip.subtitle_outline_color = 0x000000FF;
        clip.subtitle_outline_width = 2.5;
        clip.subtitle_bg_box = false;
        clip.subtitle_bg_box_color = 0x12345678;
        clip.subtitle_highlight_mode = SubtitleHighlightMode::Color;
        clip.subtitle_highlight_color = 0x00FF00FF;
        clip.subtitle_word_window_secs = 3.5;
        clip.subtitle_position_y = 0.82;
        track.add_clip(clip);
        p.tracks.push(track);

        let json = crate::otio::writer::write_otio(&p).unwrap();
        let p2 = parse_otio(&json).unwrap();

        let title2 = &p2.tracks[0].clips[0];
        assert_eq!(title2.kind, ClipKind::Title);
        assert_eq!(title2.title_text, "Primary");
        assert_eq!(title2.title_font, "Sans Bold 42");
        assert_eq!(title2.title_template, "lower_third");
        assert!(title2.title_shadow);
        assert_eq!(title2.title_secondary_text, "Secondary");
        assert_eq!(title2.title_clip_bg_color, 0x01020304);

        let clip2 = &p2.tracks[0].clips[1];
        assert_eq!(clip2.subtitle_segments.len(), 1);
        assert_eq!(clip2.subtitle_segments[0].words.len(), 2);
        assert_eq!(clip2.subtitles_language, "en");
        assert_eq!(clip2.subtitle_highlight_mode, SubtitleHighlightMode::Color);
        assert!(!clip2.subtitle_bg_box);
        assert_eq!(clip2.subtitle_position_y, 0.82);
    }

    #[test]
    fn test_roundtrip_transform_and_keyframe_metadata() {
        use crate::model::clip::{
            BezierControls, BlendMode, KeyframeInterpolation, NumericKeyframe,
        };
        use crate::model::track::Track;

        let mut p = Project::new("Transform Roundtrip");
        p.frame_rate = FrameRate {
            numerator: 24,
            denominator: 1,
        };
        p.tracks.clear();

        let mut track = Track::new_video("V1");
        let mut clip = Clip::new(
            "/footage/composite.mov",
            4_000_000_000,
            1_000_000_000,
            ClipKind::Video,
        );
        clip.scale = 1.35;
        clip.position_x = 0.15;
        clip.position_y = -0.22;
        clip.rotate = 27;
        clip.flip_h = true;
        clip.flip_v = true;
        clip.crop_left = 14;
        clip.crop_right = 6;
        clip.crop_top = 9;
        clip.crop_bottom = 3;
        clip.blend_mode = BlendMode::Screen;
        clip.scale_keyframes = vec![
            NumericKeyframe {
                time_ns: 0,
                value: 1.0,
                interpolation: KeyframeInterpolation::Linear,
                bezier_controls: Some(BezierControls {
                    x1: 0.2,
                    y1: 0.0,
                    x2: 0.8,
                    y2: 1.0,
                }),
            },
            NumericKeyframe {
                time_ns: 1_500_000_000,
                value: 1.5,
                interpolation: KeyframeInterpolation::EaseInOut,
                bezier_controls: None,
            },
        ];
        clip.position_x_keyframes = vec![NumericKeyframe {
            time_ns: 750_000_000,
            value: 0.35,
            interpolation: KeyframeInterpolation::EaseOut,
            bezier_controls: None,
        }];
        clip.position_y_keyframes = vec![NumericKeyframe {
            time_ns: 500_000_000,
            value: -0.4,
            interpolation: KeyframeInterpolation::EaseIn,
            bezier_controls: None,
        }];
        clip.rotate_keyframes = vec![NumericKeyframe {
            time_ns: 1_000_000_000,
            value: 42.0,
            interpolation: KeyframeInterpolation::EaseInOut,
            bezier_controls: None,
        }];
        clip.opacity_keyframes = vec![NumericKeyframe {
            time_ns: 2_000_000_000,
            value: 0.55,
            interpolation: KeyframeInterpolation::Linear,
            bezier_controls: None,
        }];
        track.add_clip(clip.clone());
        p.tracks.push(track);

        let json = crate::otio::writer::write_otio(&p).unwrap();
        let p2 = parse_otio(&json).unwrap();

        let clip2 = &p2.tracks[0].clips[0];
        assert_eq!(clip2.scale, clip.scale);
        assert_eq!(clip2.position_x, clip.position_x);
        assert_eq!(clip2.position_y, clip.position_y);
        assert_eq!(clip2.rotate, clip.rotate);
        assert_eq!(clip2.flip_h, clip.flip_h);
        assert_eq!(clip2.flip_v, clip.flip_v);
        assert_eq!(clip2.crop_left, clip.crop_left);
        assert_eq!(clip2.crop_right, clip.crop_right);
        assert_eq!(clip2.crop_top, clip.crop_top);
        assert_eq!(clip2.crop_bottom, clip.crop_bottom);
        assert_eq!(clip2.blend_mode, clip.blend_mode);
        assert_eq!(clip2.scale_keyframes, clip.scale_keyframes);
        assert_eq!(clip2.position_x_keyframes, clip.position_x_keyframes);
        assert_eq!(clip2.position_y_keyframes, clip.position_y_keyframes);
        assert_eq!(clip2.rotate_keyframes, clip.rotate_keyframes);
        assert_eq!(clip2.opacity_keyframes, clip.opacity_keyframes);
    }

    #[test]
    fn test_roundtrip_batch_a_color_grading_and_keyframes() {
        use crate::model::clip::{
            AudioChannelMode, KeyframeInterpolation, NumericKeyframe, SlowMotionInterp,
        };
        use crate::model::track::Track;

        let mut p = Project::new("Batch A Roundtrip");
        p.frame_rate = FrameRate {
            numerator: 30,
            denominator: 1,
        };
        p.tracks.clear();

        let mut track = Track::new_video("V1");
        let mut clip = Clip::new(
            "/footage/test.mov",
            5_000_000_000,
            0,
            ClipKind::Video,
        );

        // Color correction
        clip.temperature = 5200.0;
        clip.tint = 0.15;
        // Image filters
        clip.denoise = 0.4;
        clip.sharpness = 0.25;
        clip.blur = 0.1;
        // Color grading (10 sliders)
        clip.shadows = -0.2;
        clip.midtones = 0.1;
        clip.highlights = 0.3;
        clip.exposure = -0.15;
        clip.black_point = 0.05;
        clip.highlights_warmth = 0.2;
        clip.highlights_tint = -0.1;
        clip.midtones_warmth = -0.05;
        clip.midtones_tint = 0.08;
        clip.shadows_warmth = 0.12;
        clip.shadows_tint = -0.18;
        // Pitch
        clip.pitch_shift_semitones = 2.5;
        clip.pitch_preserve = true;
        // Audio routing
        clip.audio_channel_mode = AudioChannelMode::Left;
        // Slow-motion interp
        clip.slow_motion_interp = SlowMotionInterp::OpticalFlow;
        // LUTs
        clip.lut_paths = vec![
            "/luts/film_warm.cube".to_string(),
            "/luts/contrast_pop.cube".to_string(),
        ];
        // A small set of keyframes covering each lane
        let kf = |t, v| NumericKeyframe {
            time_ns: t,
            value: v,
            interpolation: KeyframeInterpolation::Linear,
            bezier_controls: None,
        };
        clip.brightness_keyframes = vec![kf(0, 0.0), kf(2_000_000_000, 0.3)];
        clip.contrast_keyframes = vec![kf(0, 1.0), kf(2_000_000_000, 1.4)];
        clip.saturation_keyframes = vec![kf(1_000_000_000, 1.2)];
        clip.temperature_keyframes = vec![kf(0, 5500.0), kf(3_000_000_000, 6500.0)];
        clip.tint_keyframes = vec![kf(500_000_000, 0.05)];
        clip.blur_keyframes = vec![kf(0, 0.0), kf(1_500_000_000, 0.5)];
        clip.volume_keyframes = vec![kf(0, 1.0), kf(4_000_000_000, 0.6)];
        clip.pan_keyframes = vec![kf(2_500_000_000, -0.4)];
        clip.eq_low_gain_keyframes = vec![kf(0, -3.0), kf(2_000_000_000, 1.5)];
        clip.eq_mid_gain_keyframes = vec![kf(1_000_000_000, 0.5)];
        clip.eq_high_gain_keyframes = vec![kf(0, 2.0)];
        clip.crop_left_keyframes = vec![kf(0, 0.0), kf(1_000_000_000, 20.0)];
        clip.crop_right_keyframes = vec![kf(500_000_000, 5.0)];
        clip.crop_top_keyframes = vec![kf(0, 10.0)];
        clip.crop_bottom_keyframes = vec![kf(2_000_000_000, 15.0)];
        clip.speed_keyframes = vec![kf(0, 1.0), kf(2_000_000_000, 0.5)];

        track.add_clip(clip.clone());
        p.tracks.push(track);

        let json = crate::otio::writer::write_otio(&p).unwrap();
        let p2 = parse_otio(&json).unwrap();
        let clip2 = &p2.tracks[0].clips[0];

        // Color correction
        assert_eq!(clip2.temperature, clip.temperature);
        assert_eq!(clip2.tint, clip.tint);
        // Image filters
        assert_eq!(clip2.denoise, clip.denoise);
        assert_eq!(clip2.sharpness, clip.sharpness);
        assert_eq!(clip2.blur, clip.blur);
        // Color grading
        assert_eq!(clip2.shadows, clip.shadows);
        assert_eq!(clip2.midtones, clip.midtones);
        assert_eq!(clip2.highlights, clip.highlights);
        assert_eq!(clip2.exposure, clip.exposure);
        assert_eq!(clip2.black_point, clip.black_point);
        assert_eq!(clip2.highlights_warmth, clip.highlights_warmth);
        assert_eq!(clip2.highlights_tint, clip.highlights_tint);
        assert_eq!(clip2.midtones_warmth, clip.midtones_warmth);
        assert_eq!(clip2.midtones_tint, clip.midtones_tint);
        assert_eq!(clip2.shadows_warmth, clip.shadows_warmth);
        assert_eq!(clip2.shadows_tint, clip.shadows_tint);
        // Pitch + audio routing
        assert_eq!(clip2.pitch_shift_semitones, clip.pitch_shift_semitones);
        assert_eq!(clip2.pitch_preserve, clip.pitch_preserve);
        assert_eq!(clip2.audio_channel_mode, clip.audio_channel_mode);
        // Slow-motion interp
        assert_eq!(clip2.slow_motion_interp, clip.slow_motion_interp);
        // LUTs
        assert_eq!(clip2.lut_paths, clip.lut_paths);
        // Color/image keyframes
        assert_eq!(clip2.brightness_keyframes, clip.brightness_keyframes);
        assert_eq!(clip2.contrast_keyframes, clip.contrast_keyframes);
        assert_eq!(clip2.saturation_keyframes, clip.saturation_keyframes);
        assert_eq!(clip2.temperature_keyframes, clip.temperature_keyframes);
        assert_eq!(clip2.tint_keyframes, clip.tint_keyframes);
        assert_eq!(clip2.blur_keyframes, clip.blur_keyframes);
        // Audio keyframes
        assert_eq!(clip2.volume_keyframes, clip.volume_keyframes);
        assert_eq!(clip2.pan_keyframes, clip.pan_keyframes);
        assert_eq!(clip2.eq_low_gain_keyframes, clip.eq_low_gain_keyframes);
        assert_eq!(clip2.eq_mid_gain_keyframes, clip.eq_mid_gain_keyframes);
        assert_eq!(clip2.eq_high_gain_keyframes, clip.eq_high_gain_keyframes);
        // Crop keyframes
        assert_eq!(clip2.crop_left_keyframes, clip.crop_left_keyframes);
        assert_eq!(clip2.crop_right_keyframes, clip.crop_right_keyframes);
        assert_eq!(clip2.crop_top_keyframes, clip.crop_top_keyframes);
        assert_eq!(clip2.crop_bottom_keyframes, clip.crop_bottom_keyframes);
        // Speed keyframes
        assert_eq!(clip2.speed_keyframes, clip.speed_keyframes);
    }

    #[test]
    fn test_roundtrip_batch_b_voice_iso_chroma_freeze_misc() {
        use crate::model::clip::{ClipColorLabel, VoiceIsolationSource};
        use crate::model::track::Track;

        let mut p = Project::new("Batch B Roundtrip");
        p.frame_rate = FrameRate {
            numerator: 30,
            denominator: 1,
        };
        p.tracks.clear();

        let mut track = Track::new_video("V1");
        let mut clip = Clip::new(
            "/footage/test.mov",
            5_000_000_000,
            0,
            ClipKind::Video,
        );

        // Voice isolation (6 fields + base)
        clip.voice_isolation = 0.6;
        clip.voice_isolation_pad_ms = 120.0;
        clip.voice_isolation_fade_ms = 35.0;
        clip.voice_isolation_floor = 0.15;
        clip.voice_isolation_source = VoiceIsolationSource::Silence;
        clip.voice_isolation_silence_threshold_db = -28.0;
        clip.voice_isolation_silence_min_ms = 250;
        clip.measured_loudness_lufs = Some(-19.5);

        // Chroma key
        clip.chroma_key_enabled = true;
        clip.chroma_key_color = 0x00FF80;
        clip.chroma_key_tolerance = 0.42;
        clip.chroma_key_softness = 0.18;

        // BG removal
        clip.bg_removal_enabled = true;
        clip.bg_removal_threshold = 0.65;

        // Freeze frame
        clip.freeze_frame = true;
        clip.freeze_frame_source_ns = Some(1_500_000_000);
        clip.freeze_frame_hold_duration_ns = Some(3_000_000_000);

        // Stabilization
        clip.vidstab_enabled = true;
        clip.vidstab_smoothing = 0.7;

        // Misc
        clip.color_label = ClipColorLabel::Teal;
        clip.anamorphic_desqueeze = 1.33;
        clip.group_id = Some("group-A".to_string());
        clip.link_group_id = Some("link-42".to_string());
        clip.source_timecode_base_ns = Some(86_400_000_000_000);
        clip.animated_svg = true;

        track.add_clip(clip.clone());
        p.tracks.push(track);

        let json = crate::otio::writer::write_otio(&p).unwrap();
        let p2 = parse_otio(&json).unwrap();
        let clip2 = &p2.tracks[0].clips[0];

        // Voice isolation
        assert_eq!(clip2.voice_isolation, clip.voice_isolation);
        assert_eq!(clip2.voice_isolation_pad_ms, clip.voice_isolation_pad_ms);
        assert_eq!(clip2.voice_isolation_fade_ms, clip.voice_isolation_fade_ms);
        assert_eq!(clip2.voice_isolation_floor, clip.voice_isolation_floor);
        assert_eq!(clip2.voice_isolation_source, clip.voice_isolation_source);
        assert_eq!(
            clip2.voice_isolation_silence_threshold_db,
            clip.voice_isolation_silence_threshold_db
        );
        assert_eq!(
            clip2.voice_isolation_silence_min_ms,
            clip.voice_isolation_silence_min_ms
        );
        assert_eq!(clip2.measured_loudness_lufs, clip.measured_loudness_lufs);

        // Chroma key
        assert_eq!(clip2.chroma_key_enabled, clip.chroma_key_enabled);
        assert_eq!(clip2.chroma_key_color, clip.chroma_key_color);
        assert_eq!(clip2.chroma_key_tolerance, clip.chroma_key_tolerance);
        assert_eq!(clip2.chroma_key_softness, clip.chroma_key_softness);

        // BG removal
        assert_eq!(clip2.bg_removal_enabled, clip.bg_removal_enabled);
        assert_eq!(clip2.bg_removal_threshold, clip.bg_removal_threshold);

        // Freeze frame
        assert_eq!(clip2.freeze_frame, clip.freeze_frame);
        assert_eq!(clip2.freeze_frame_source_ns, clip.freeze_frame_source_ns);
        assert_eq!(
            clip2.freeze_frame_hold_duration_ns,
            clip.freeze_frame_hold_duration_ns
        );

        // Stabilization
        assert_eq!(clip2.vidstab_enabled, clip.vidstab_enabled);
        assert_eq!(clip2.vidstab_smoothing, clip.vidstab_smoothing);

        // Misc
        assert_eq!(clip2.color_label, clip.color_label);
        assert_eq!(clip2.anamorphic_desqueeze, clip.anamorphic_desqueeze);
        assert_eq!(clip2.group_id, clip.group_id);
        assert_eq!(clip2.link_group_id, clip.link_group_id);
        assert_eq!(
            clip2.source_timecode_base_ns,
            clip.source_timecode_base_ns
        );
        assert_eq!(clip2.animated_svg, clip.animated_svg);
    }

    #[test]
    fn test_url_to_path() {
        assert_eq!(
            url_to_path("file:///home/user/file.mp4", None),
            "/home/user/file.mp4"
        );
        assert_eq!(
            url_to_path("file:///home/user/my%20file.mp4", None),
            "/home/user/my file.mp4"
        );
        assert_eq!(url_to_path("/direct/path.mp4", None), "/direct/path.mp4");
    }

    #[test]
    fn test_parse_relative_media_reference_resolves_against_otio_file() {
        let json = r#"{
            "OTIO_SCHEMA": "Timeline.1",
            "name": "Relative Paths",
            "global_start_time": { "OTIO_SCHEMA": "RationalTime.1", "value": 0.0, "rate": 24.0 },
            "tracks": {
                "OTIO_SCHEMA": "Stack.1",
                "name": "tracks",
                "children": [{
                    "OTIO_SCHEMA": "Track.1",
                    "name": "V1",
                    "kind": "Video",
                    "children": [{
                        "OTIO_SCHEMA": "Clip.1",
                        "name": "shot_01",
                        "source_range": {
                            "OTIO_SCHEMA": "TimeRange.1",
                            "start_time": { "OTIO_SCHEMA": "RationalTime.1", "value": 0.0, "rate": 24.0 },
                            "duration": { "OTIO_SCHEMA": "RationalTime.1", "value": 48.0, "rate": 24.0 }
                        },
                        "media_reference": {
                            "OTIO_SCHEMA": "ExternalReference.1",
                            "target_url": "../media/clip%201.mp4"
                        }
                    }]
                }]
            }
        }"#;

        let project =
            parse_otio_with_path(json, Some(Path::new("/show/interchange/timeline.otio"))).unwrap();
        assert_eq!(
            project.tracks[0].clips[0].source_path,
            "/show/media/clip 1.mp4"
        );
    }

    #[test]
    fn test_rate_to_frame_rate() {
        let fr = rate_to_frame_rate(23.976);
        assert_eq!(fr.numerator, 24000);
        assert_eq!(fr.denominator, 1001);

        let fr = rate_to_frame_rate(24.0);
        assert_eq!(fr.numerator, 24);
        assert_eq!(fr.denominator, 1);

        let fr = rate_to_frame_rate(29.97);
        assert_eq!(fr.numerator, 30000);
        assert_eq!(fr.denominator, 1001);
    }
}
