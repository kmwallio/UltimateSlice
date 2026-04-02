//! Import an OpenTimelineIO JSON file into an UltimateSlice `Project`.

use anyhow::{Context, Result};
use serde_json::Value;

use crate::model::clip::{Clip, ClipKind};
use crate::model::project::{FrameRate, Marker, Project};
use crate::model::track::{Track, TrackKind};

use super::schema::*;

/// Parse an OTIO JSON string into a `Project`.
pub fn parse_otio(json: &str) -> Result<Project> {
    let timeline: OtioTimeline = serde_json::from_str(json).context("failed to parse OTIO JSON")?;

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
        if let Some(us) = otio_track.metadata.get("ultimateslice") {
            track.muted = us.get("muted").and_then(|v| v.as_bool()).unwrap_or(false);
            track.locked = us.get("locked").and_then(|v| v.as_bool()).unwrap_or(false);
            track.soloed = us.get("soloed").and_then(|v| v.as_bool()).unwrap_or(false);
            track.duck = us.get("duck").and_then(|v| v.as_bool()).unwrap_or(false);
            if let Some(db) = us.get("duck_amount_db").and_then(|v| v.as_f64()) {
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
                    let clip = otio_clip_to_clip(otio_clip, cursor_ns, kind, rate);
                    let dur = clip.duration();
                    track.clips.push(clip);
                    cursor_ns += dur;
                }

                OtioTrackChild::Transition(trans) => {
                    // Attach transition info to the preceding clip.
                    if let Some(prev) = track.clips.last_mut() {
                        let kind_name = trans
                            .metadata
                            .get("ultimateslice")
                            .and_then(|us| us.get("transition_kind"))
                            .and_then(|v| v.as_str())
                            .map(String::from);
                        let transition_name =
                            kind_name.unwrap_or_else(|| match trans.transition_type.as_str() {
                                "SMPTE_Dissolve" => "cross_dissolve".into(),
                                _ => trans.transition_type.clone(),
                            });
                        let in_ns = rational_time_to_ns(&trans.in_offset);
                        let out_ns = rational_time_to_ns(&trans.out_offset);
                        prev.transition_after = transition_name;
                        prev.transition_after_ns = in_ns + out_ns;
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
    rate: f64,
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
        Some(OtioMediaReference::External(ext)) => url_to_path(&ext.target_url),
        _ => String::new(),
    };

    // Clip kind — check UltimateSlice metadata first, else derive from track.
    let us_meta = otio_clip.metadata.get("ultimateslice");
    let kind = us_meta
        .and_then(|us| us.get("kind"))
        .and_then(|v| v.as_str())
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
    if let Some(us) = us_meta {
        if let Some(v) = us.get("speed").and_then(|v| v.as_f64()) {
            clip.speed = v;
        }
        if let Some(v) = us.get("reverse").and_then(|v| v.as_bool()) {
            clip.reverse = v;
        }
        if let Some(v) = us.get("volume").and_then(|v| v.as_f64()) {
            clip.volume = v as f32;
        }
        if let Some(v) = us.get("pan").and_then(|v| v.as_f64()) {
            clip.pan = v as f32;
        }
        if let Some(v) = us.get("brightness").and_then(|v| v.as_f64()) {
            clip.brightness = v as f32;
        }
        if let Some(v) = us.get("contrast").and_then(|v| v.as_f64()) {
            clip.contrast = v as f32;
        }
        if let Some(v) = us.get("saturation").and_then(|v| v.as_f64()) {
            clip.saturation = v as f32;
        }
        if let Some(v) = us.get("opacity").and_then(|v| v.as_f64()) {
            clip.opacity = v;
        }
    } else {
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

/// Convert a `file://` URL to a local path.
fn url_to_path(url: &str) -> String {
    let stripped = url.strip_prefix("file://").unwrap_or(url);
    // Decode percent-encoded characters.
    percent_decode(stripped)
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
    let w = metadata
        .get("ultimateslice")
        .and_then(|us| us.get("width"))
        .and_then(|v| v.as_u64())
        .unwrap_or(1920) as u32;
    let h = metadata
        .get("ultimateslice")
        .and_then(|us| us.get("height"))
        .and_then(|v| v.as_u64())
        .unwrap_or(1080) as u32;
    (w, h)
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
    if let Some(hex) = m
        .metadata
        .get("ultimateslice")
        .and_then(|us| us.get("color_rgba"))
        .and_then(|v| v.as_str())
    {
        if let Ok(rgba) = u32::from_str_radix(hex, 16) {
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
        assert_eq!(a.transition_after, "cross_dissolve");
        assert!(a.transition_after_ns > 0);
    }

    #[test]
    fn test_roundtrip() {
        use crate::model::clip::Clip;
        use crate::model::track::Track;

        let mut p = Project::new("Roundtrip");
        p.frame_rate = FrameRate {
            numerator: 24,
            denominator: 1,
        };
        p.tracks.clear();
        let mut track = Track::new_video("V1");
        track.add_clip(Clip::new(
            "/footage/a.mp4",
            2_000_000_000,
            0,
            ClipKind::Video,
        ));
        track.add_clip(Clip::new(
            "/footage/b.mp4",
            3_000_000_000,
            5_000_000_000,
            ClipKind::Video,
        ));
        p.tracks.push(track);

        let json = crate::otio::writer::write_otio(&p).unwrap();
        let p2 = parse_otio(&json).unwrap();

        assert_eq!(p2.title, "Roundtrip");
        assert_eq!(p2.tracks.len(), 1);
        assert_eq!(p2.tracks[0].clips.len(), 2);
        // First clip at 0, second at 5s.
        assert_eq!(p2.tracks[0].clips[0].timeline_start, 0);
        let diff = (p2.tracks[0].clips[1].timeline_start as i64 - 5_000_000_000i64).unsigned_abs();
        assert!(diff < 42_000_000, "second clip start off by {diff} ns");
    }

    #[test]
    fn test_url_to_path() {
        assert_eq!(
            url_to_path("file:///home/user/file.mp4"),
            "/home/user/file.mp4"
        );
        assert_eq!(
            url_to_path("file:///home/user/my%20file.mp4"),
            "/home/user/my file.mp4"
        );
        assert_eq!(url_to_path("/direct/path.mp4"), "/direct/path.mp4");
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
