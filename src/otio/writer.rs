//! Export a UltimateSlice `Project` to OpenTimelineIO JSON.

use anyhow::Result;
use serde_json::json;

use crate::model::clip::ClipKind;
use crate::model::project::Project;
use crate::model::track::TrackKind;

use super::schema::*;

/// Map an internal transition name to an OTIO transition type string.
fn otio_transition_type(name: &str) -> &'static str {
    match name {
        "cross_dissolve" => "SMPTE_Dissolve",
        _ => "Custom_Transition",
    }
}

/// Convert a source path to a `file://` URL.
fn path_to_url(path: &str) -> String {
    if path.is_empty() {
        return String::new();
    }
    // Percent-encode spaces (most common issue); leave other characters as-is
    // for readability.
    let encoded = path.replace(' ', "%20");
    format!("file://{encoded}")
}

/// Serialize a `Project` to an OpenTimelineIO JSON string.
pub fn write_otio(project: &Project) -> Result<String> {
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
                    target_url: path_to_url(&clip.source_path),
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
            let us_meta = json!({
                "kind": format!("{:?}", clip.kind),
                "speed": clip.speed,
                "reverse": clip.reverse,
                "volume": clip.volume,
                "pan": clip.pan,
                "brightness": clip.brightness,
                "contrast": clip.contrast,
                "saturation": clip.saturation,
                "opacity": clip.opacity,
                "transition_after": clip.transition_after,
                "transition_after_ns": clip.transition_after_ns,
            });

            let metadata = json!({ "ultimateslice": us_meta });

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
            if !clip.transition_after.is_empty() && clip.transition_after_ns > 0 {
                let half_ns = clip.transition_after_ns / 2;
                children.push(OtioTrackChild::Transition(OtioTransition {
                    schema: "Transition.1".into(),
                    name: clip.transition_after.replace('_', " "),
                    transition_type: otio_transition_type(&clip.transition_after).into(),
                    in_offset: ns_to_rational_time(half_ns, rate),
                    out_offset: ns_to_rational_time(clip.transition_after_ns - half_ns, rate),
                    metadata: json!({
                        "ultimateslice": {
                            "transition_kind": clip.transition_after,
                        }
                    }),
                }));
            }
        }

        // Track-level metadata.
        let track_meta = json!({
            "ultimateslice": {
                "muted": track.muted,
                "locked": track.locked,
                "soloed": track.soloed,
                "audio_role": format!("{:?}", track.audio_role),
                "duck": track.duck,
                "duck_amount_db": track.duck_amount_db,
            }
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
            .position(|t| t.kind == TrackKind::Video)
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
                    metadata: json!({
                        "ultimateslice": {
                            "color_rgba": format!("{:08X}", marker.color),
                        }
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
        metadata: json!({
            "ultimateslice": {
                "width": project.width,
                "height": project.height,
            }
        }),
    };

    serde_json::to_string_pretty(&timeline).map_err(Into::into)
}

/// Map a packed RGBA colour to a human-readable OTIO marker colour name.
fn marker_color_name(rgba: u32) -> String {
    // Extract rough hue from the RGB bytes.
    let r = ((rgba >> 24) & 0xFF) as u8;
    let g = ((rgba >> 16) & 0xFF) as u8;
    let b = ((rgba >> 8) & 0xFF) as u8;
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
        c1.transition_after = "cross_dissolve".into();
        c1.transition_after_ns = 1_000_000_000; // 1 second
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
    }

    #[test]
    fn test_path_to_url() {
        assert_eq!(
            path_to_url("/home/user/my file.mp4"),
            "file:///home/user/my%20file.mp4"
        );
        assert_eq!(path_to_url(""), "");
    }
}
