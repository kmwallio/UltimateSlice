use crate::model::clip::ClipKind;
use crate::model::project::{FrameRate, Project};
use crate::model::track::TrackKind;

/// Convert nanoseconds to a frame count at the given frame rate.
pub fn ns_to_frames(ns: u64, fps: &FrameRate) -> u64 {
    let num = fps.numerator as u64;
    let den = fps.denominator as u64;
    // Round to nearest frame.
    (ns * num + den * 500_000_000) / (den * 1_000_000_000)
}

/// True for NTSC 29.97fps (30000/1001) which uses drop-frame timecode.
pub fn is_drop_frame(fps: &FrameRate) -> bool {
    fps.numerator == 30000 && fps.denominator == 1001
}

/// Convert a frame count to SMPTE timecode string.
/// Non-drop: `HH:MM:SS:FF`, drop-frame: `HH:MM:SS;FF`.
pub fn frames_to_timecode(total_frames: u64, fps: &FrameRate, drop_frame: bool) -> String {
    let fps_int = if drop_frame {
        30u64
    } else {
        fps.numerator as u64 / fps.denominator.max(1) as u64
    };
    if fps_int == 0 {
        return "00:00:00:00".to_string();
    }

    let mut frames = total_frames;

    if drop_frame {
        // Drop-frame timecode for 29.97fps.
        // 2 frames are dropped at the start of each minute except every 10th minute.
        // Total frames in 10 minutes = 17982 (not 18000).
        let d = frames / 17982;
        let m = frames % 17982;
        // After the first minute in each 10-min block, each minute has 1798 frames.
        let extra_drops = if m < 2 { 0 } else { (m - 2) / 1798 };
        frames += 18 * d + 2 * extra_drops;
    }

    let ff = frames % fps_int;
    let ss = (frames / fps_int) % 60;
    let mm = (frames / (fps_int * 60)) % 60;
    let hh = frames / (fps_int * 3600);

    let sep = if drop_frame { ';' } else { ':' };
    format!("{:02}:{:02}:{:02}{}{:02}", hh, mm, ss, sep, ff)
}

/// Convert nanoseconds to SMPTE timecode.
pub fn ns_to_timecode(ns: u64, fps: &FrameRate) -> String {
    let drop = is_drop_frame(fps);
    let frames = ns_to_frames(ns, fps);
    frames_to_timecode(frames, fps, drop)
}

/// Derive a reel identifier from a source file path.
/// Uses the filename stem, truncated/padded to 8 characters (CMX 3600 convention).
/// Falls back to "AX" for empty paths.
fn reel_id(source_path: &str) -> String {
    if source_path.is_empty() {
        return "AX      ".to_string();
    }
    let stem = std::path::Path::new(source_path)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("AX");
    // Keep only alphanumeric + underscore, truncate to 8 chars, pad with spaces.
    let clean: String = stem
        .chars()
        .filter(|c| c.is_alphanumeric() || *c == '_')
        .take(8)
        .collect();
    if clean.is_empty() {
        "AX      ".to_string()
    } else {
        format!("{:<8}", clean)
    }
}

/// Map an internal transition name to EDL transition type and code.
/// Returns `(type_char, edl_code)` where type_char is used in the EDL line
/// and edl_code is the wipe/effect number (0 for dissolve/cut).
fn edl_transition(transition: &str) -> (&'static str, u16) {
    match transition {
        "cross_dissolve" => ("D", 0),
        "fade_to_black" => ("D", 0), // treated as dissolve to black
        "wipe_right" => ("W", 1),
        "wipe_left" => ("W", 2),
        _ => ("D", 0),
    }
}

/// Generate a CMX 3600 EDL string from a project.
///
/// The EDL contains one event per clip on each track. Record timecodes
/// start at 01:00:00:00 (standard broadcast convention). Source timecodes
/// reflect the clip's in/out points in the source media, adjusted for
/// speed if applicable.
///
/// Title and Adjustment clips are skipped (no source media).
pub fn write_edl(project: &Project) -> String {
    let fps = &project.frame_rate;
    let drop = is_drop_frame(fps);
    // Record timecode offset: 01:00:00:00 in frames.
    let fps_int = if drop {
        30u64
    } else {
        fps.numerator as u64 / fps.denominator.max(1) as u64
    };
    let rec_offset_frames = fps_int * 3600; // 1 hour

    let mut out = String::new();
    out.push_str(&format!("TITLE: {}\n", project.title));
    if drop {
        out.push_str("FCM: DROP FRAME\n");
    } else {
        out.push_str("FCM: NON-DROP FRAME\n");
    }
    out.push('\n');

    let mut event_num = 1u32;

    for track in &project.tracks {
        let track_label = match track.kind {
            TrackKind::Video => "V".to_string(),
            TrackKind::Audio => {
                // Number audio tracks: A, A2, A3, etc.
                let audio_idx = project
                    .tracks
                    .iter()
                    .filter(|t| t.kind == TrackKind::Audio)
                    .position(|t| t.id == track.id)
                    .unwrap_or(0);
                if audio_idx == 0 {
                    "A".to_string()
                } else {
                    format!("A{}", audio_idx + 1)
                }
            }
        };

        for (clip_idx, clip) in track.clips.iter().enumerate() {
            // Skip clips without source media.
            if clip.kind == ClipKind::Title
                || clip.kind == ClipKind::Adjustment
                || clip.kind == ClipKind::Compound
                || clip.kind == ClipKind::Multicam
            {
                continue;
            }

            let reel = reel_id(&clip.source_path);

            // Source timecodes (adjusted for speed).
            let src_in_ns = clip.source_in;
            let src_out_ns = clip.source_out;
            // Add source timecode base if available (e.g., for timecode-based media).
            let tc_base = clip.source_timecode_base_ns.unwrap_or(0);
            let src_in_tc = ns_to_timecode(tc_base + src_in_ns, fps);
            let src_out_tc = ns_to_timecode(tc_base + src_out_ns, fps);

            // Record timecodes (timeline position + 01:00:00:00 offset).
            let rec_in_frames = ns_to_frames(clip.timeline_start, fps) + rec_offset_frames;
            let rec_out_frames =
                ns_to_frames(clip.timeline_start + clip.duration(), fps) + rec_offset_frames;
            let rec_in_tc = frames_to_timecode(rec_in_frames, fps, drop);
            let rec_out_tc = frames_to_timecode(rec_out_frames, fps, drop);

            // Determine transition type from the PREVIOUS clip's outgoing transition.
            let (trans_type, trans_code) = if clip_idx > 0 {
                let prev = &track.clips[clip_idx - 1];
                if prev.outgoing_transition.is_active() {
                    edl_transition(prev.outgoing_transition.kind_trimmed())
                } else {
                    ("C", 0)
                }
            } else {
                ("C", 0)
            };

            // Format transition field.
            let trans_field = if trans_type == "C" {
                "C        ".to_string()
            } else {
                let dur_frames = if clip_idx > 0 {
                    ns_to_frames(
                        track.clips[clip_idx - 1].outgoing_transition.duration_ns,
                        fps,
                    )
                } else {
                    0
                };
                format!("{} {:03}    ", trans_type, dur_frames)
            };

            // EDL event line.
            out.push_str(&format!(
                "{:03}  {}  {:<5} {}  {} {} {} {}\n",
                event_num,
                reel,
                track_label,
                trans_field,
                src_in_tc,
                src_out_tc,
                rec_in_tc,
                rec_out_tc,
            ));

            // Speed effect comment (if not 1.0).
            if (clip.speed - 1.0).abs() > 0.001 {
                let effective_fps = fps.as_f64() * clip.speed;
                out.push_str(&format!(
                    "M2   {}  {:.1}  {}\n",
                    reel, effective_fps, src_in_tc
                ));
            }

            // Comment lines with clip metadata.
            out.push_str(&format!("* FROM CLIP NAME: {}\n", clip.label));
            if !clip.source_path.is_empty() {
                out.push_str(&format!("* SOURCE FILE: {}\n", clip.source_path));
            }
            out.push('\n');

            event_num += 1;
        }
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::clip::Clip;
    use crate::model::project::FrameRate;

    #[test]
    fn test_ns_to_frames_24fps() {
        let fps = FrameRate {
            numerator: 24,
            denominator: 1,
        };
        assert_eq!(ns_to_frames(0, &fps), 0);
        assert_eq!(ns_to_frames(1_000_000_000, &fps), 24); // 1 second = 24 frames
        assert_eq!(ns_to_frames(500_000_000, &fps), 12); // 0.5s = 12 frames
    }

    #[test]
    fn test_ns_to_frames_2997fps() {
        let fps = FrameRate {
            numerator: 30000,
            denominator: 1001,
        };
        // 1 second ≈ 29.97 frames → rounds to 30
        assert_eq!(ns_to_frames(1_000_000_000, &fps), 30);
    }

    #[test]
    fn test_frames_to_timecode_nondrop() {
        let fps = FrameRate {
            numerator: 24,
            denominator: 1,
        };
        assert_eq!(frames_to_timecode(0, &fps, false), "00:00:00:00");
        assert_eq!(frames_to_timecode(24, &fps, false), "00:00:01:00");
        assert_eq!(frames_to_timecode(48, &fps, false), "00:00:02:00");
        assert_eq!(frames_to_timecode(24 * 60, &fps, false), "00:01:00:00");
        assert_eq!(frames_to_timecode(24 * 3600, &fps, false), "01:00:00:00");
        assert_eq!(frames_to_timecode(10, &fps, false), "00:00:00:10");
    }

    #[test]
    fn test_frames_to_timecode_drop() {
        let fps = FrameRate {
            numerator: 30000,
            denominator: 1001,
        };
        assert_eq!(frames_to_timecode(0, &fps, true), "00:00:00;00");
        // At 29.97 DF, 1 second = 30 frames
        assert_eq!(frames_to_timecode(30, &fps, true), "00:00:01;00");
    }

    #[test]
    fn test_is_drop_frame() {
        assert!(is_drop_frame(&FrameRate {
            numerator: 30000,
            denominator: 1001
        }));
        assert!(!is_drop_frame(&FrameRate {
            numerator: 24,
            denominator: 1
        }));
        assert!(!is_drop_frame(&FrameRate {
            numerator: 30,
            denominator: 1
        }));
    }

    #[test]
    fn test_reel_id() {
        assert_eq!(reel_id("/home/user/footage.mp4"), "footage ");
        assert_eq!(reel_id("/path/to/VERY_LONG_REEL_NAME.mov"), "VERY_LON");
        assert_eq!(reel_id(""), "AX      ");
    }

    #[test]
    fn test_write_edl_basic() {
        use crate::model::track::Track;
        let mut project = Project::new("Test EDL");
        project.frame_rate = FrameRate {
            numerator: 24,
            denominator: 1,
        };
        let mut track = Track::new_video("V1");
        track.add_clip(Clip::new(
            "/footage/clip1.mp4",
            5_000_000_000,
            0,
            ClipKind::Video,
        ));
        track.add_clip(Clip::new(
            "/footage/clip2.mp4",
            3_000_000_000,
            5_000_000_000,
            ClipKind::Video,
        ));
        project.tracks.push(track);

        let edl = write_edl(&project);
        assert!(edl.contains("TITLE: Test EDL"));
        assert!(edl.contains("FCM: NON-DROP FRAME"));
        assert!(edl.contains("001"));
        assert!(edl.contains("002"));
        assert!(edl.contains("clip1"));
        assert!(edl.contains("clip2"));
        // Record timecodes should start at 01:00:00:00
        assert!(edl.contains("01:00:00:00"));
    }

    #[test]
    fn test_write_edl_skips_titles() {
        use crate::model::track::Track;
        let mut project = Project::new("Test");
        project.frame_rate = FrameRate {
            numerator: 24,
            denominator: 1,
        };
        let mut track = Track::new_video("V1");
        track.add_clip(Clip::new(
            "/footage/real.mp4",
            5_000_000_000,
            0,
            ClipKind::Video,
        ));
        track.add_clip(Clip::new("", 2_000_000_000, 5_000_000_000, ClipKind::Title));
        project.tracks.push(track);

        let edl = write_edl(&project);
        // Should have only 1 event (title skipped).
        assert!(edl.contains("001"));
        assert!(!edl.contains("002"));
    }
}
