//! Export a UltimateSlice `Project` to OpenTimelineIO JSON.

use anyhow::Result;
use serde_json::json;
use std::path::{Component, Path, PathBuf};

use super::metadata::{
    wrap_clip_metadata, wrap_marker_metadata, wrap_project_metadata, wrap_track_metadata,
    wrap_transition_metadata, UltimateSliceClipOtioMetadata, UltimateSliceMarkerOtioMetadata,
    UltimateSliceProjectOtioMetadata, UltimateSliceTrackOtioMetadata,
    UltimateSliceTransitionOtioMetadata,
};
use super::schema::*;
use crate::model::clip::ClipKind;
use crate::model::project::Project;
use crate::model::track::TrackKind;
use crate::model::transition::transition_label_for_kind;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum OtioMediaPathMode {
    #[default]
    Absolute,
    Relative,
}

impl OtioMediaPathMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Absolute => "absolute",
            Self::Relative => "relative",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Absolute => "Absolute media file paths",
            Self::Relative => "Relative to the exported .otio file",
        }
    }

    pub fn from_str(value: &str) -> Option<Self> {
        match value {
            "absolute" => Some(Self::Absolute),
            "relative" => Some(Self::Relative),
            _ => None,
        }
    }
}

/// Map an internal transition name to an OTIO transition type string.
fn otio_transition_type(name: &str) -> &'static str {
    match name {
        "cross_dissolve" => "SMPTE_Dissolve",
        _ => "Custom_Transition",
    }
}

fn encode_path_for_otio(path: &Path) -> String {
    path.to_string_lossy()
        .replace('\\', "/")
        .replace(' ', "%20")
}

fn relativize_path(target: &Path, base_dir: &Path) -> Option<PathBuf> {
    if !target.is_absolute() || !base_dir.is_absolute() {
        return None;
    }

    let target_components: Vec<_> = target.components().collect();
    let base_components: Vec<_> = base_dir.components().collect();
    let mut common_len = 0usize;
    while common_len < target_components.len()
        && common_len < base_components.len()
        && target_components[common_len] == base_components[common_len]
    {
        common_len += 1;
    }

    if common_len == 0 {
        return None;
    }

    let mut relative = PathBuf::new();
    for comp in &base_components[common_len..] {
        match comp {
            Component::Normal(_) | Component::CurDir | Component::ParentDir => relative.push(".."),
            Component::RootDir | Component::Prefix(_) => return None,
        }
    }
    for comp in &target_components[common_len..] {
        match comp {
            Component::Normal(part) => relative.push(part),
            Component::CurDir => {}
            Component::ParentDir => relative.push(".."),
            Component::RootDir | Component::Prefix(_) => return None,
        }
    }

    Some(relative)
}

/// Convert a source path to an OTIO media reference URL/path.
fn path_to_url(path: &str, output_path: Option<&Path>, path_mode: OtioMediaPathMode) -> String {
    if path.is_empty() {
        return String::new();
    }

    let raw_path = path.strip_prefix("file://").unwrap_or(path);
    let raw_path = Path::new(raw_path);

    if path_mode == OtioMediaPathMode::Relative {
        if let Some(base_dir) = output_path.and_then(Path::parent) {
            if raw_path.is_absolute() {
                if let Some(relative) = relativize_path(raw_path, base_dir) {
                    return encode_path_for_otio(&relative);
                }
            } else {
                return encode_path_for_otio(raw_path);
            }
        }
    }

    format!("file://{}", encode_path_for_otio(raw_path))
}

/// Serialize a `Project` to an OpenTimelineIO JSON string.
pub fn write_otio(project: &Project) -> Result<String> {
    write_otio_with_mode(project, None, OtioMediaPathMode::Absolute)
}

pub fn write_otio_to_path(
    project: &Project,
    output_path: &Path,
    path_mode: OtioMediaPathMode,
) -> Result<String> {
    write_otio_with_mode(project, Some(output_path), path_mode)
}

fn write_otio_with_mode(
    project: &Project,
    output_path: Option<&Path>,
    path_mode: OtioMediaPathMode,
) -> Result<String> {
    let rate = project.frame_rate.as_f64();

    // -- Build tracks -------------------------------------------------------
    let mut otio_tracks: Vec<OtioTrack> = Vec::new();

    for track in &project.tracks {
        let kind_str = match track.kind {
            TrackKind::Video => "Video",
            TrackKind::Audio => "Audio",
        };

        let mut children: Vec<OtioTrackChild> = Vec::new();

        // Walk clips, emitting explicit Gap items for dead space.
        let mut cursor_ns: u64 = 0;

        for clip in &track.clips {
            // Gap before this clip?
            if clip.timeline_start > cursor_ns {
                let gap_ns = clip.timeline_start - cursor_ns;
                children.push(OtioTrackChild::Gap(OtioGap {
                    schema: "Gap.1".into(),
                    name: "".into(),
                    source_range: Some(OtioTimeRange {
                        schema: "TimeRange.1".into(),
                        start_time: rational_time(0.0, rate),
                        duration: ns_to_rational_time(gap_ns, rate),
                    }),
                    effects: vec![],
                    markers: vec![],
                    metadata: serde_json::Value::Null,
                }));
            }

            // The clip itself.
            let clip_duration_ns = clip.duration();

            let source_range = OtioTimeRange {
                schema: "TimeRange.1".into(),
                start_time: ns_to_rational_time(clip.source_in, rate),
                duration: ns_to_rational_time(clip_duration_ns, rate),
            };

            let is_sourceless = matches!(clip.kind, ClipKind::Title | ClipKind::Adjustment);

            let media_reference = if is_sourceless || clip.source_path.is_empty() {
                Some(OtioMediaReference::Missing(OtioMissingReference {
                    schema: "MissingReference.1".into(),
                    metadata: serde_json::Value::Null,
                }))
            } else {
                let available_range = clip.media_duration_ns.map(|dur| OtioTimeRange {
                    schema: "TimeRange.1".into(),
                    start_time: rational_time(0.0, rate),
                    duration: ns_to_rational_time(dur, rate),
                });
                Some(OtioMediaReference::External(OtioExternalReference {
                    schema: "ExternalReference.1".into(),
                    target_url: path_to_url(&clip.source_path, output_path, path_mode),
                    available_range,
                    metadata: serde_json::Value::Null,
                }))
            };

            // Build per-clip effects list for basic interop.
            let mut effects: Vec<OtioEffect> = Vec::new();
            if (clip.speed - 1.0).abs() > 0.001 {
                effects.push(OtioEffect {
                    schema: "Effect.1".into(),
                    name: "LinearTimeWarp".into(),
                    metadata: json!({ "time_scalar": clip.speed }),
                });
            }

            // UltimateSlice-specific metadata for lossless round-trip.
            let metadata = wrap_clip_metadata(&UltimateSliceClipOtioMetadata {
                kind: Some(format!("{:?}", clip.kind)),
                speed: Some(clip.speed),
                reverse: Some(clip.reverse),
                volume: Some(clip.volume as f64),
                pan: Some(clip.pan as f64),
                eq_bands: Some(clip.eq_bands),
                match_eq_bands: if clip.match_eq_bands.is_empty() {
                    None
                } else {
                    Some(clip.match_eq_bands.clone())
                },
                voice_enhance: Some(clip.voice_enhance),
                voice_enhance_strength: Some(clip.voice_enhance_strength as f64),
                voice_isolation: Some(clip.voice_isolation as f64),
                voice_isolation_pad_ms: Some(clip.voice_isolation_pad_ms as f64),
                voice_isolation_fade_ms: Some(clip.voice_isolation_fade_ms as f64),
                voice_isolation_floor: Some(clip.voice_isolation_floor as f64),
                voice_isolation_source: Some(clip.voice_isolation_source),
                voice_isolation_silence_threshold_db: Some(
                    clip.voice_isolation_silence_threshold_db as f64,
                ),
                voice_isolation_silence_min_ms: Some(clip.voice_isolation_silence_min_ms),
                measured_loudness_lufs: clip.measured_loudness_lufs,
                chroma_key_enabled: Some(clip.chroma_key_enabled),
                chroma_key_color: Some(clip.chroma_key_color),
                chroma_key_tolerance: Some(clip.chroma_key_tolerance as f64),
                chroma_key_softness: Some(clip.chroma_key_softness as f64),
                bg_removal_enabled: Some(clip.bg_removal_enabled),
                bg_removal_threshold: Some(clip.bg_removal_threshold),
                freeze_frame: Some(clip.freeze_frame),
                freeze_frame_source_ns: clip.freeze_frame_source_ns,
                freeze_frame_hold_duration_ns: clip.freeze_frame_hold_duration_ns,
                vidstab_enabled: Some(clip.vidstab_enabled),
                vidstab_smoothing: Some(clip.vidstab_smoothing as f64),
                motion_blur_enabled: Some(clip.motion_blur_enabled),
                motion_blur_shutter_angle: Some(clip.motion_blur_shutter_angle),
                color_label: Some(clip.color_label),
                anamorphic_desqueeze: Some(clip.anamorphic_desqueeze),
                group_id: clip.group_id.clone(),
                link_group_id: clip.link_group_id.clone(),
                source_timecode_base_ns: clip.source_timecode_base_ns,
                animated_svg: Some(clip.animated_svg),
                frei0r_effects: if clip.frei0r_effects.is_empty() {
                    None
                } else {
                    Some(clip.frei0r_effects.clone())
                },
                ladspa_effects: if clip.ladspa_effects.is_empty() {
                    None
                } else {
                    Some(clip.ladspa_effects.clone())
                },
                masks: if clip.masks.is_empty() {
                    None
                } else {
                    Some(clip.masks.clone())
                },
                motion_trackers: if clip.motion_trackers.is_empty() {
                    None
                } else {
                    Some(clip.motion_trackers.clone())
                },
                tracking_binding: clip.tracking_binding.clone(),
                compound_tracks: clip.compound_tracks.clone(),
                multicam_angles: clip.multicam_angles.clone(),
                multicam_switches: clip.multicam_switches.clone(),
                audition_takes: clip.audition_takes.clone(),
                audition_active_take_index: if clip.audition_takes.is_some() {
                    Some(clip.audition_active_take_index)
                } else {
                    None
                },
                brightness: Some(clip.brightness as f64),
                contrast: Some(clip.contrast as f64),
                saturation: Some(clip.saturation as f64),
                temperature: Some(clip.temperature as f64),
                tint: Some(clip.tint as f64),
                denoise: Some(clip.denoise as f64),
                sharpness: Some(clip.sharpness as f64),
                blur: Some(clip.blur as f64),
                shadows: Some(clip.shadows as f64),
                midtones: Some(clip.midtones as f64),
                highlights: Some(clip.highlights as f64),
                exposure: Some(clip.exposure as f64),
                black_point: Some(clip.black_point as f64),
                highlights_warmth: Some(clip.highlights_warmth as f64),
                highlights_tint: Some(clip.highlights_tint as f64),
                midtones_warmth: Some(clip.midtones_warmth as f64),
                midtones_tint: Some(clip.midtones_tint as f64),
                shadows_warmth: Some(clip.shadows_warmth as f64),
                shadows_tint: Some(clip.shadows_tint as f64),
                hsl_qualifier: clip.hsl_qualifier.clone(),
                pitch_shift_semitones: Some(clip.pitch_shift_semitones),
                pitch_preserve: Some(clip.pitch_preserve),
                audio_channel_mode: Some(clip.audio_channel_mode),
                speed_keyframes: if clip.speed_keyframes.is_empty() {
                    None
                } else {
                    Some(clip.speed_keyframes.clone())
                },
                slow_motion_interp: Some(clip.slow_motion_interp),
                lut_paths: if clip.lut_paths.is_empty() {
                    None
                } else {
                    Some(clip.lut_paths.clone())
                },
                brightness_keyframes: if clip.brightness_keyframes.is_empty() {
                    None
                } else {
                    Some(clip.brightness_keyframes.clone())
                },
                contrast_keyframes: if clip.contrast_keyframes.is_empty() {
                    None
                } else {
                    Some(clip.contrast_keyframes.clone())
                },
                saturation_keyframes: if clip.saturation_keyframes.is_empty() {
                    None
                } else {
                    Some(clip.saturation_keyframes.clone())
                },
                temperature_keyframes: if clip.temperature_keyframes.is_empty() {
                    None
                } else {
                    Some(clip.temperature_keyframes.clone())
                },
                tint_keyframes: if clip.tint_keyframes.is_empty() {
                    None
                } else {
                    Some(clip.tint_keyframes.clone())
                },
                blur_keyframes: if clip.blur_keyframes.is_empty() {
                    None
                } else {
                    Some(clip.blur_keyframes.clone())
                },
                volume_keyframes: if clip.volume_keyframes.is_empty() {
                    None
                } else {
                    Some(clip.volume_keyframes.clone())
                },
                pan_keyframes: if clip.pan_keyframes.is_empty() {
                    None
                } else {
                    Some(clip.pan_keyframes.clone())
                },
                eq_low_gain_keyframes: if clip.eq_low_gain_keyframes.is_empty() {
                    None
                } else {
                    Some(clip.eq_low_gain_keyframes.clone())
                },
                eq_mid_gain_keyframes: if clip.eq_mid_gain_keyframes.is_empty() {
                    None
                } else {
                    Some(clip.eq_mid_gain_keyframes.clone())
                },
                eq_high_gain_keyframes: if clip.eq_high_gain_keyframes.is_empty() {
                    None
                } else {
                    Some(clip.eq_high_gain_keyframes.clone())
                },
                crop_left_keyframes: if clip.crop_left_keyframes.is_empty() {
                    None
                } else {
                    Some(clip.crop_left_keyframes.clone())
                },
                crop_right_keyframes: if clip.crop_right_keyframes.is_empty() {
                    None
                } else {
                    Some(clip.crop_right_keyframes.clone())
                },
                crop_top_keyframes: if clip.crop_top_keyframes.is_empty() {
                    None
                } else {
                    Some(clip.crop_top_keyframes.clone())
                },
                crop_bottom_keyframes: if clip.crop_bottom_keyframes.is_empty() {
                    None
                } else {
                    Some(clip.crop_bottom_keyframes.clone())
                },
                opacity: Some(clip.opacity),
                scale: Some(clip.scale),
                position_x: Some(clip.position_x),
                position_y: Some(clip.position_y),
                rotate: Some(clip.rotate),
                flip_h: Some(clip.flip_h),
                flip_v: Some(clip.flip_v),
                crop_left: Some(clip.crop_left),
                crop_right: Some(clip.crop_right),
                crop_top: Some(clip.crop_top),
                crop_bottom: Some(clip.crop_bottom),
                blend_mode: Some(clip.blend_mode),
                opacity_keyframes: Some(clip.opacity_keyframes.clone()),
                scale_keyframes: Some(clip.scale_keyframes.clone()),
                position_x_keyframes: Some(clip.position_x_keyframes.clone()),
                position_y_keyframes: Some(clip.position_y_keyframes.clone()),
                rotate_keyframes: Some(clip.rotate_keyframes.clone()),
                title_text: Some(clip.title_text.clone()),
                title_font: Some(clip.title_font.clone()),
                title_color: Some(clip.title_color),
                title_x: Some(clip.title_x),
                title_y: Some(clip.title_y),
                title_template: Some(clip.title_template.clone()),
                title_outline_color: Some(clip.title_outline_color),
                title_outline_width: Some(clip.title_outline_width),
                title_shadow: Some(clip.title_shadow),
                title_shadow_color: Some(clip.title_shadow_color),
                title_shadow_offset_x: Some(clip.title_shadow_offset_x),
                title_shadow_offset_y: Some(clip.title_shadow_offset_y),
                title_bg_box: Some(clip.title_bg_box),
                title_bg_box_color: Some(clip.title_bg_box_color),
                title_bg_box_padding: Some(clip.title_bg_box_padding),
                title_clip_bg_color: Some(clip.title_clip_bg_color),
                title_secondary_text: Some(clip.title_secondary_text.clone()),
                subtitle_segments: Some(clip.subtitle_segments.clone()),
                subtitles_language: Some(clip.subtitles_language.clone()),
                subtitle_font: Some(clip.subtitle_font.clone()),
                subtitle_bold: Some(clip.subtitle_bold),
                subtitle_italic: Some(clip.subtitle_italic),
                subtitle_underline: Some(clip.subtitle_underline),
                subtitle_shadow: Some(clip.subtitle_shadow),
                subtitle_visible: Some(clip.subtitle_visible),
                subtitle_shadow_color: Some(clip.subtitle_shadow_color),
                subtitle_shadow_offset_x: Some(clip.subtitle_shadow_offset_x),
                subtitle_shadow_offset_y: Some(clip.subtitle_shadow_offset_y),
                subtitle_highlight_flags: if clip.subtitle_highlight_flags.is_none() {
                    None
                } else {
                    Some(clip.subtitle_highlight_flags)
                },
                subtitle_bg_highlight_color: Some(clip.subtitle_bg_highlight_color),
                subtitle_color: Some(clip.subtitle_color),
                subtitle_outline_color: Some(clip.subtitle_outline_color),
                subtitle_outline_width: Some(clip.subtitle_outline_width),
                subtitle_bg_box: Some(clip.subtitle_bg_box),
                subtitle_bg_box_color: Some(clip.subtitle_bg_box_color),
                subtitle_highlight_mode: Some(clip.subtitle_highlight_mode),
                subtitle_highlight_color: Some(clip.subtitle_highlight_color),
                subtitle_highlight_stroke_color: Some(clip.subtitle_highlight_stroke_color),
                subtitle_word_window_secs: Some(clip.subtitle_word_window_secs),
                subtitle_position_y: Some(clip.subtitle_position_y),
                scene_id: clip.scene_id.clone(),
                script_confidence: clip.script_confidence,
            });

            children.push(OtioTrackChild::Clip(OtioClip {
                schema: "Clip.1".into(),
                name: clip.label.clone(),
                source_range: Some(source_range),
                media_reference,
                effects,
                markers: vec![],
                metadata,
            }));

            cursor_ns = clip.timeline_start + clip_duration_ns;

            // Transition after this clip?
            if clip.outgoing_transition.is_active() {
                let split = clip.outgoing_transition.cut_split();
                children.push(OtioTrackChild::Transition(OtioTransition {
                    schema: "Transition.1".into(),
                    name: transition_label_for_kind(clip.outgoing_transition.kind_trimmed())
                        .unwrap_or(clip.outgoing_transition.kind.as_str())
                        .to_string(),
                    transition_type: otio_transition_type(&clip.outgoing_transition.kind).into(),
                    in_offset: ns_to_rational_time(split.before_cut_ns, rate),
                    out_offset: ns_to_rational_time(split.after_cut_ns, rate),
                    metadata: wrap_transition_metadata(&UltimateSliceTransitionOtioMetadata {
                        transition_kind: Some(clip.outgoing_transition.kind.clone()),
                        transition_alignment: Some(
                            clip.outgoing_transition.alignment.as_str().to_string(),
                        ),
                    }),
                }));
            }
        }

        // Track-level metadata.
        let track_meta = wrap_track_metadata(&UltimateSliceTrackOtioMetadata {
            muted: Some(track.muted),
            locked: Some(track.locked),
            soloed: Some(track.soloed),
            audio_role: Some(track.audio_role.as_str().to_string()),
            duck: Some(track.duck),
            duck_amount_db: Some(track.duck_amount_db),
            // Only emit when non-default so legacy OTIO consumers don't see
            // a noisy new key on every track.
            surround_position: if track.surround_position
                != crate::model::track::SurroundPositionOverride::Auto
            {
                Some(track.surround_position.as_str().to_string())
            } else {
                None
            },
            height_preset: if track.height_preset
                != crate::model::track::TrackHeightPreset::default()
            {
                Some(format!("{:?}", track.height_preset).to_lowercase())
            } else {
                None
            },
            gain_db: if track.gain_db != 0.0 {
                Some(track.gain_db)
            } else {
                None
            },
            pan: if track.pan != 0.0 {
                Some(track.pan)
            } else {
                None
            },
        });

        otio_tracks.push(OtioTrack {
            schema: "Track.1".into(),
            name: track.label.clone(),
            kind: kind_str.into(),
            children,
            effects: vec![],
            markers: vec![],
            metadata: track_meta,
        });
    }

    // -- Project-level markers → first video track markers ------------------
    if !project.markers.is_empty() {
        // Find first video track (or first track if none).
        let target_idx = project
            .tracks
            .iter()
            .position(|t| t.is_video())
            .unwrap_or(0);
        if target_idx < otio_tracks.len() {
            for marker in &project.markers {
                let color = marker_color_name(marker.color);
                otio_tracks[target_idx].markers.push(OtioMarker {
                    schema: "Marker.1".into(),
                    name: marker.label.clone(),
                    marked_range: OtioTimeRange {
                        schema: "TimeRange.1".into(),
                        start_time: ns_to_rational_time(marker.position_ns, rate),
                        duration: rational_time(0.0, rate),
                    },
                    color,
                    metadata: wrap_marker_metadata(&UltimateSliceMarkerOtioMetadata {
                        color_rgba: Some(format!("{:08X}", marker.color)),
                        notes: if marker.notes.is_empty() {
                            None
                        } else {
                            Some(marker.notes.clone())
                        },
                    }),
                });
            }
        }
    }

    // -- Assemble timeline --------------------------------------------------
    let timeline = OtioTimeline {
        schema: "Timeline.1".into(),
        name: project.title.clone(),
        global_start_time: Some(rational_time(0.0, rate)),
        tracks: OtioStack {
            schema: "Stack.1".into(),
            name: "tracks".into(),
            children: otio_tracks,
            metadata: serde_json::Value::Null,
        },
        metadata: wrap_project_metadata(&UltimateSliceProjectOtioMetadata {
            width: Some(project.width),
            height: Some(project.height),
            master_gain_db: if project.master_gain_db.abs() > 1e-9 {
                Some(project.master_gain_db)
            } else {
                None
            },
            dialogue_bus_gain_db: if project.dialogue_bus.gain_db.abs() > 1e-9 {
                Some(project.dialogue_bus.gain_db)
            } else {
                None
            },
            dialogue_bus_muted: if project.dialogue_bus.muted {
                Some(true)
            } else {
                None
            },
            dialogue_bus_soloed: if project.dialogue_bus.soloed {
                Some(true)
            } else {
                None
            },
            effects_bus_gain_db: if project.effects_bus.gain_db.abs() > 1e-9 {
                Some(project.effects_bus.gain_db)
            } else {
                None
            },
            effects_bus_muted: if project.effects_bus.muted {
                Some(true)
            } else {
                None
            },
            effects_bus_soloed: if project.effects_bus.soloed {
                Some(true)
            } else {
                None
            },
            music_bus_gain_db: if project.music_bus.gain_db.abs() > 1e-9 {
                Some(project.music_bus.gain_db)
            } else {
                None
            },
            music_bus_muted: if project.music_bus.muted {
                Some(true)
            } else {
                None
            },
            music_bus_soloed: if project.music_bus.soloed {
                Some(true)
            } else {
                None
            },
            reference_stills: project.reference_stills.clone(),
        }),
    };

    serde_json::to_string_pretty(&timeline).map_err(Into::into)
}

/// Map a packed RGBA colour to a human-readable OTIO marker colour name.
fn marker_color_name(rgba: u32) -> String {
    // Extract rough hue from the RGB bytes.
    let (r, g, b, _a) = crate::ui::colors::rgba_u32_to_u8(rgba);
    let max = r.max(g).max(b);
    if max < 40 {
        return "BLACK".into();
    }
    if r > 200 && g < 80 && b < 80 {
        return "RED".into();
    }
    if g > 200 && r < 80 && b < 80 {
        return "GREEN".into();
    }
    if b > 200 && r < 80 && g < 80 {
        return "BLUE".into();
    }
    if r > 200 && g > 200 && b < 80 {
        return "YELLOW".into();
    }
    if r > 200 && g > 100 && g < 180 {
        return "ORANGE".into();
    }
    if r > 200 && g > 200 && b > 200 {
        return "WHITE".into();
    }
    if r > 150 && b > 150 && g < 100 {
        return "MAGENTA".into();
    }
    if g > 150 && b > 150 && r < 100 {
        return "CYAN".into();
    }
    "ORANGE".into() // default
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::clip::Clip;
    use crate::model::project::FrameRate;
    use crate::model::track::Track;

    fn make_project() -> Project {
        let mut p = Project::new("OTIO Test");
        p.frame_rate = FrameRate {
            numerator: 24,
            denominator: 1,
        };
        p.tracks.clear();
        p
    }

    #[test]
    fn test_write_empty_project() {
        let p = make_project();
        let json = write_otio(&p).unwrap();
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(v["OTIO_SCHEMA"], "Timeline.1");
        assert_eq!(v["name"], "OTIO Test");
        assert_eq!(v["tracks"]["OTIO_SCHEMA"], "Stack.1");
    }

    #[test]
    fn test_write_single_clip() {
        let mut p = make_project();
        let mut track = Track::new_video("V1");
        track.add_clip(Clip::new(
            "/footage/clip1.mp4",
            5_000_000_000,
            0,
            ClipKind::Video,
        ));
        p.tracks.push(track);

        let json = write_otio(&p).unwrap();
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        let children = &v["tracks"]["children"][0]["children"];
        assert_eq!(children.as_array().unwrap().len(), 1);
        assert_eq!(children[0]["OTIO_SCHEMA"], "Clip.1");
        assert!(children[0]["media_reference"]["target_url"]
            .as_str()
            .unwrap()
            .contains("clip1.mp4"));
    }

    #[test]
    fn test_write_single_clip_with_relative_media_path() {
        let mut p = make_project();
        let mut track = Track::new_video("V1");
        track.add_clip(Clip::new(
            "/project/media/clip1.mp4",
            5_000_000_000,
            0,
            ClipKind::Video,
        ));
        p.tracks.push(track);

        let json = write_otio_to_path(
            &p,
            Path::new("/project/interchange/timeline.otio"),
            OtioMediaPathMode::Relative,
        )
        .unwrap();
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        let children = &v["tracks"]["children"][0]["children"];
        assert_eq!(
            children[0]["media_reference"]["target_url"].as_str(),
            Some("../media/clip1.mp4")
        );
    }

    #[test]
    fn test_write_gap_generation() {
        let mut p = make_project();
        let mut track = Track::new_video("V1");
        // Clip at 0..2s
        track.add_clip(Clip::new(
            "/footage/a.mp4",
            2_000_000_000,
            0,
            ClipKind::Video,
        ));
        // Clip at 5s..8s (gap from 2s to 5s)
        track.add_clip(Clip::new(
            "/footage/b.mp4",
            3_000_000_000,
            5_000_000_000,
            ClipKind::Video,
        ));
        p.tracks.push(track);

        let json = write_otio(&p).unwrap();
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        let children = v["tracks"]["children"][0]["children"].as_array().unwrap();
        // Expect: Clip, Gap, Clip
        assert_eq!(children.len(), 3);
        assert_eq!(children[0]["OTIO_SCHEMA"], "Clip.1");
        assert_eq!(children[1]["OTIO_SCHEMA"], "Gap.1");
        assert_eq!(children[2]["OTIO_SCHEMA"], "Clip.1");
    }

    #[test]
    fn test_write_transition() {
        let mut p = make_project();
        let mut track = Track::new_video("V1");
        let mut c1 = Clip::new("/footage/a.mp4", 3_000_000_000, 0, ClipKind::Video);
        c1.outgoing_transition = crate::model::transition::OutgoingTransition::new(
            "cross_dissolve",
            1_000_000_000,
            crate::model::transition::TransitionAlignment::EndOnCut,
        ); // 1 second
        track.add_clip(c1);
        track.add_clip(Clip::new(
            "/footage/b.mp4",
            3_000_000_000,
            3_000_000_000,
            ClipKind::Video,
        ));
        p.tracks.push(track);

        let json = write_otio(&p).unwrap();
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        let children = v["tracks"]["children"][0]["children"].as_array().unwrap();
        // Clip, Transition, Clip
        assert_eq!(children.len(), 3);
        assert_eq!(children[1]["OTIO_SCHEMA"], "Transition.1");
        assert_eq!(children[1]["transition_type"], "SMPTE_Dissolve");
        assert_eq!(
            children[1]["metadata"]["ultimateslice"]["transition"]["transition_kind"],
            "cross_dissolve"
        );
    }

    #[test]
    fn test_write_markers() {
        use crate::model::project::Marker;
        let mut p = make_project();
        let mut track = Track::new_video("V1");
        track.add_clip(Clip::new(
            "/footage/a.mp4",
            5_000_000_000,
            0,
            ClipKind::Video,
        ));
        p.tracks.push(track);
        p.markers.push(Marker {
            id: "m1".into(),
            position_ns: 2_000_000_000,
            label: "Chapter 1".into(),
            color: 0xFF0000FF, // red
            notes: String::new(),
        });

        let json = write_otio(&p).unwrap();
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        let markers = v["tracks"]["children"][0]["markers"].as_array().unwrap();
        assert_eq!(markers.len(), 1);
        assert_eq!(markers[0]["name"], "Chapter 1");
        assert_eq!(markers[0]["color"], "RED");
    }

    #[test]
    fn test_write_title_clip_uses_missing_reference() {
        let mut p = make_project();
        let mut track = Track::new_video("V1");
        let mut title = Clip::new("", 2_000_000_000, 0, ClipKind::Title);
        title.label = "My Title".into();
        track.add_clip(title);
        p.tracks.push(track);

        let json = write_otio(&p).unwrap();
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        let clip = &v["tracks"]["children"][0]["children"][0];
        assert_eq!(clip["name"], "My Title");
        assert_eq!(clip["media_reference"]["OTIO_SCHEMA"], "MissingReference.1");
        assert_eq!(clip["metadata"]["ultimateslice"]["version"], 1);
        assert_eq!(clip["metadata"]["ultimateslice"]["clip"]["kind"], "Title");
        assert_eq!(clip["metadata"]["ultimateslice"]["clip"]["title_text"], "");
    }

    #[test]
    fn test_write_track_metadata_uses_versioned_section() {
        let mut p = make_project();
        let mut track = Track::new_audio("A1");
        track.audio_role = crate::model::track::AudioRole::Dialogue;
        p.tracks.push(track);

        let json = write_otio(&p).unwrap();
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        let track_meta = &v["tracks"]["children"][0]["metadata"]["ultimateslice"];

        assert_eq!(track_meta["version"], 1);
        assert_eq!(track_meta["track"]["audio_role"], "dialogue");
    }

    #[test]
    fn test_write_transform_and_keyframe_metadata() {
        use crate::model::clip::{BezierControls, KeyframeInterpolation, NumericKeyframe};

        let mut p = make_project();
        let mut track = Track::new_video("V1");
        let mut clip = Clip::new("/footage/clip1.mp4", 5_000_000_000, 0, ClipKind::Video);
        clip.scale = 1.25;
        clip.position_x = 0.2;
        clip.position_y = -0.15;
        clip.rotate = 18;
        clip.flip_h = true;
        clip.crop_left = 12;
        clip.crop_top = 8;
        clip.blend_mode = crate::model::clip::BlendMode::Screen;
        clip.scale_keyframes = vec![NumericKeyframe {
            time_ns: 1_000_000_000,
            value: 1.4,
            interpolation: KeyframeInterpolation::EaseInOut,
            bezier_controls: Some(BezierControls {
                x1: 0.25,
                y1: 0.0,
                x2: 0.75,
                y2: 1.0,
            }),
        }];
        clip.opacity_keyframes = vec![NumericKeyframe {
            time_ns: 2_000_000_000,
            value: 0.65,
            interpolation: KeyframeInterpolation::EaseOut,
            bezier_controls: None,
        }];
        track.add_clip(clip);
        p.tracks.push(track);

        let json = write_otio(&p).unwrap();
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        let clip_meta =
            &v["tracks"]["children"][0]["children"][0]["metadata"]["ultimateslice"]["clip"];

        assert_eq!(clip_meta["scale"], 1.25);
        assert_eq!(clip_meta["position_x"], 0.2);
        assert_eq!(clip_meta["position_y"], -0.15);
        assert_eq!(clip_meta["rotate"], 18);
        assert_eq!(clip_meta["flip_h"], true);
        assert_eq!(clip_meta["crop_left"], 12);
        assert_eq!(clip_meta["crop_top"], 8);
        assert_eq!(clip_meta["blend_mode"], "screen");
        assert_eq!(clip_meta["scale_keyframes"][0]["value"], 1.4);
        assert_eq!(clip_meta["opacity_keyframes"][0]["value"], 0.65);
    }

    #[test]
    fn test_path_to_url_absolute_mode() {
        assert_eq!(
            path_to_url(
                "/home/user/my file.mp4",
                Some(Path::new("/exports/timeline.otio")),
                OtioMediaPathMode::Absolute
            ),
            "file:///home/user/my%20file.mp4"
        );
        assert_eq!(
            path_to_url(
                "",
                Some(Path::new("/exports/timeline.otio")),
                OtioMediaPathMode::Absolute
            ),
            ""
        );
    }

    #[test]
    fn test_path_to_url_relative_mode() {
        assert_eq!(
            path_to_url(
                "/project/media/my file.mp4",
                Some(Path::new("/project/interchange/timeline.otio")),
                OtioMediaPathMode::Relative
            ),
            "../media/my%20file.mp4"
        );
    }

    #[test]
    fn test_reference_stills_round_trip_through_otio() {
        let mut p = make_project();
        let mut still = crate::model::project::ReferenceStill::new("Ref A");
        still.width = 1280;
        still.height = 720;
        still.captured_at_ns = 10_000_000_000;
        still.filename = format!("{}.png", still.id);
        p.reference_stills.push(still.clone());

        let json = write_otio(&p).unwrap();
        let parsed = crate::otio::parser::parse_otio(&json).expect("parse");
        assert_eq!(parsed.reference_stills.len(), 1);
        assert_eq!(parsed.reference_stills[0].id, still.id);
        assert_eq!(parsed.reference_stills[0].label, "Ref A");
        assert_eq!(parsed.reference_stills[0].width, 1280);
        assert_eq!(parsed.reference_stills[0].height, 720);
        assert_eq!(parsed.reference_stills[0].filename, still.filename);
    }
}
