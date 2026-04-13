// SPDX-License-Identifier: GPL-3.0-or-later
//! Script-to-timeline assembly logic.
//!
//! Takes an [`AlignmentResult`] and a [`Script`] and builds an
//! [`AssemblyPlan`] that specifies exactly which clips to create,
//! where to place them on the timeline, and which scene-heading
//! titles to generate.

use serde::{Deserialize, Serialize};

use crate::model::clip::{AuditionTake, Clip, ClipKind};
use crate::model::media_library::{MediaBin, MediaLibrary};
use crate::model::project::Project;
use crate::model::track::Track;

use super::script::Script;
use super::script_align::AlignmentResult;

// ── Data types ──────────────────────────────────────────────────────────

/// A clip to be created during timeline assembly.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlannedClip {
    pub source_path: String,
    pub source_in: u64,
    pub source_out: u64,
    pub timeline_start: u64,
    pub scene_id: Option<String>,
    pub scene_heading: Option<String>,
    pub confidence: Option<f64>,
    pub kind: ClipKind,
    /// Alternate takes for this scene (used when multiple source files
    /// match the same scene). When non-empty, the clip becomes an
    /// Audition with the primary fields as the active take.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub alternate_takes: Vec<PlannedTake>,
}

/// An alternate take within an audition clip.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlannedTake {
    pub source_path: String,
    pub source_in: u64,
    pub source_out: u64,
    pub confidence: f64,
}

/// Complete specification for timeline assembly.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AssemblyPlan {
    pub video_clips: Vec<PlannedClip>,
    pub title_clips: Vec<PlannedClip>,
    pub unmatched_paths: Vec<String>,
    pub script_path: String,
}

// ── Build plan ──────────────────────────────────────────────────────────

/// Build an assembly plan from alignment results and script scene order.
///
/// * `gap_ns` — inter-clip gap (e.g. 0 for butt-edit).
/// * `title_duration_ns` — duration for scene heading title cards.
/// * `include_titles` — whether to generate title cards for scene headings.
pub fn build_assembly_plan(
    script: &Script,
    alignment: &AlignmentResult,
    gap_ns: u64,
    title_duration_ns: u64,
    include_titles: bool,
) -> AssemblyPlan {
    let mut video_clips = Vec::new();
    let mut title_clips = Vec::new();
    let mut cursor_ns: u64 = 0;

    // Walk scenes in script order. For each scene, find matching clips.
    for scene in &script.scenes {
        let mut scene_mappings: Vec<_> = alignment
            .mappings
            .iter()
            .filter(|m| m.scene_id == scene.id)
            .collect();

        if scene_mappings.is_empty() {
            continue;
        }

        // Insert a title card for this scene heading.
        if include_titles {
            title_clips.push(PlannedClip {
                source_path: String::new(),
                source_in: 0,
                source_out: title_duration_ns,
                timeline_start: cursor_ns,
                scene_id: Some(scene.id.clone()),
                scene_heading: Some(scene.heading.clone()),
                confidence: None,
                kind: ClipKind::Title,
                alternate_takes: Vec::new(),
            });
            cursor_ns += title_duration_ns + gap_ns;
        }

        // Group mappings from different source files for the same scene.
        // Multiple mappings from the *same* source file (sub-clip splits)
        // are placed sequentially. Multiple mappings from *different* source
        // files are grouped into an audition (best confidence = active take).
        //
        // Sort by confidence descending so the best match is first.
        scene_mappings.sort_by(|a, b| {
            b.confidence
                .partial_cmp(&a.confidence)
                .unwrap_or(std::cmp::Ordering::Equal)
        });

        // Partition into groups by source_path. Same-source mappings are
        // separate clips (sub-clip trimming); different-source mappings for
        // the same scene become audition takes.
        //
        // Collect unique source paths in confidence order (best first).
        let mut seen_sources: Vec<String> = Vec::new();
        for m in &scene_mappings {
            if !seen_sources.contains(&m.clip_source_path) {
                seen_sources.push(m.clip_source_path.clone());
            }
        }

        if seen_sources.len() <= 1 {
            // Single source (possibly split into sub-clips) — no audition needed.
            // Re-sort by source_in for timeline order.
            let mut ordered: Vec<_> = scene_mappings.clone();
            ordered.sort_by_key(|m| m.source_in_ns);
            for mapping in &ordered {
                let duration = mapping.source_out_ns.saturating_sub(mapping.source_in_ns);
                if duration == 0 {
                    continue;
                }
                video_clips.push(PlannedClip {
                    source_path: mapping.clip_source_path.clone(),
                    source_in: mapping.source_in_ns,
                    source_out: mapping.source_out_ns,
                    timeline_start: cursor_ns,
                    scene_id: Some(scene.id.clone()),
                    scene_heading: None,
                    confidence: Some(mapping.confidence),
                    kind: ClipKind::Video,
                    alternate_takes: Vec::new(),
                });
                cursor_ns += duration + gap_ns;
            }
        } else {
            // Multiple source files match this scene — create an audition.
            // Best-confidence mapping is the active take; others are alternates.
            let best = &scene_mappings[0];
            let duration = best.source_out_ns.saturating_sub(best.source_in_ns);
            if duration == 0 {
                continue;
            }

            let alternates: Vec<PlannedTake> = scene_mappings[1..]
                .iter()
                .filter(|m| m.clip_source_path != best.clip_source_path)
                .map(|m| PlannedTake {
                    source_path: m.clip_source_path.clone(),
                    source_in: m.source_in_ns,
                    source_out: m.source_out_ns,
                    confidence: m.confidence,
                })
                .collect();

            video_clips.push(PlannedClip {
                source_path: best.clip_source_path.clone(),
                source_in: best.source_in_ns,
                source_out: best.source_out_ns,
                timeline_start: cursor_ns,
                scene_id: Some(scene.id.clone()),
                scene_heading: None,
                confidence: Some(best.confidence),
                kind: if alternates.is_empty() {
                    ClipKind::Video
                } else {
                    ClipKind::Audition
                },
                alternate_takes: alternates,
            });
            cursor_ns += duration + gap_ns;
        }
    }

    AssemblyPlan {
        video_clips,
        title_clips,
        unmatched_paths: alignment.unmatched_clips.clone(),
        script_path: script.path.clone(),
    }
}

// ── Apply plan to project ───────────────────────────────────────────────

/// Apply an assembly plan to a project, creating tracks and clips.
///
/// Returns the old tracks for undo.
pub fn apply_assembly_plan(
    project: &mut Project,
    library: &mut MediaLibrary,
    plan: &AssemblyPlan,
) -> Vec<Track> {
    let old_tracks = project.tracks.clone();

    // Find or create video track.
    let video_track_id = project
        .tracks
        .iter()
        .find(|t| t.kind == crate::model::track::TrackKind::Video)
        .map(|t| t.id.clone())
        .unwrap_or_else(|| {
            let t = Track::new_video("Video 1");
            let id = t.id.clone();
            project.tracks.push(t);
            id
        });

    // Create a separate track for title overlays if needed.
    let title_track_id = if !plan.title_clips.is_empty() {
        let t = Track::new_video("Scene Titles");
        let id = t.id.clone();
        project.tracks.insert(0, t); // Above the main video track.
        Some(id)
    } else {
        None
    };

    // Add video clips (including auditions for multi-take scenes).
    for pc in &plan.video_clips {
        let clip = if pc.alternate_takes.is_empty() {
            // Regular video clip.
            let mut c = Clip::new(
                &pc.source_path,
                pc.source_out,
                pc.timeline_start,
                pc.kind.clone(),
            );
            c.source_in = pc.source_in;
            c.scene_id = pc.scene_id.clone();
            c.script_confidence = pc.confidence;
            c
        } else {
            // Audition clip: active take + alternates.
            let active_label = std::path::Path::new(&pc.source_path)
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("Take 1")
                .to_string();

            let mut takes = vec![AuditionTake {
                id: uuid::Uuid::new_v4().to_string(),
                label: active_label,
                source_path: pc.source_path.clone(),
                source_in: pc.source_in,
                source_out: pc.source_out,
                source_timecode_base_ns: None,
                media_duration_ns: None,
            }];
            for (i, alt) in pc.alternate_takes.iter().enumerate() {
                let alt_label = std::path::Path::new(&alt.source_path)
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .unwrap_or(&format!("Take {}", i + 2))
                    .to_string();
                takes.push(AuditionTake {
                    id: uuid::Uuid::new_v4().to_string(),
                    label: alt_label,
                    source_path: alt.source_path.clone(),
                    source_in: alt.source_in,
                    source_out: alt.source_out,
                    source_timecode_base_ns: None,
                    media_duration_ns: None,
                });
            }

            let mut c = Clip::new_audition(pc.timeline_start, takes, 0);
            c.scene_id = pc.scene_id.clone();
            c.script_confidence = pc.confidence;
            c
        };

        if let Some(track) = project.track_mut(&video_track_id) {
            track.add_clip(clip);
        }
    }

    // Add title clips.
    if let Some(ref title_tid) = title_track_id {
        for pc in &plan.title_clips {
            let mut clip = Clip::new("", pc.source_out, pc.timeline_start, ClipKind::Title);
            clip.source_in = 0;
            clip.scene_id = pc.scene_id.clone();
            if let Some(ref heading) = pc.scene_heading {
                clip.title_text = heading.clone();
            }
            clip.title_font = "Sans Bold 48".to_string();

            if let Some(track) = project.track_mut(title_tid) {
                track.add_clip(clip);
            }
        }
    }

    // Create "Unassigned" bin for unmatched clips.
    if !plan.unmatched_paths.is_empty() {
        let bin = MediaBin {
            id: uuid::Uuid::new_v4().to_string(),
            name: "Unassigned".to_string(),
            parent_id: None,
        };
        let bin_id = bin.id.clone();
        library.bins.push(bin);

        for path in &plan.unmatched_paths {
            if let Some(item) = library.items.iter_mut().find(|i| i.source_path == *path) {
                item.bin_id = Some(bin_id.clone());
            }
        }
    }

    project.dirty = true;
    old_tracks
}

// ── Tests ───────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::media::script::{Scene, Script, ScriptElement, ScriptElementKind};
    use crate::media::script_align::{AlignmentResult, SceneMapping};

    fn make_script() -> Script {
        Script {
            path: "test.fountain".to_string(),
            title: Some("Test Movie".to_string()),
            scenes: vec![
                Scene {
                    id: "s1".to_string(),
                    scene_number: None,
                    heading: "INT. OFFICE - DAY".to_string(),
                    elements: vec![ScriptElement {
                        kind: ScriptElementKind::Dialogue,
                        text: "Hello".to_string(),
                        character: Some("JOHN".to_string()),
                    }],
                    full_text: "int. office - day hello".to_string(),
                },
                Scene {
                    id: "s2".to_string(),
                    scene_number: None,
                    heading: "EXT. PARK - NIGHT".to_string(),
                    elements: vec![ScriptElement {
                        kind: ScriptElementKind::Action,
                        text: "Birds sing".to_string(),
                        character: None,
                    }],
                    full_text: "ext. park - night birds sing".to_string(),
                },
            ],
        }
    }

    #[test]
    fn test_build_assembly_plan_basic() {
        let script = make_script();
        let alignment = AlignmentResult {
            mappings: vec![
                SceneMapping {
                    clip_source_path: "clip1.mp4".to_string(),
                    scene_id: "s1".to_string(),
                    confidence: 0.9,
                    source_in_ns: 0,
                    source_out_ns: 3_000_000_000,
                    transcript_excerpt: "hello".to_string(),
                },
                SceneMapping {
                    clip_source_path: "clip2.mp4".to_string(),
                    scene_id: "s2".to_string(),
                    confidence: 0.85,
                    source_in_ns: 0,
                    source_out_ns: 4_000_000_000,
                    transcript_excerpt: "birds sing".to_string(),
                },
            ],
            unmatched_clips: vec!["extra.mp4".to_string()],
        };

        let plan = build_assembly_plan(&script, &alignment, 0, 3_000_000_000, true);

        assert_eq!(plan.video_clips.len(), 2);
        assert_eq!(plan.title_clips.len(), 2);
        assert_eq!(plan.unmatched_paths.len(), 1);

        // First title at 0, first clip at 3s, second title at 6s, second clip at 9s.
        assert_eq!(plan.title_clips[0].timeline_start, 0);
        assert_eq!(plan.video_clips[0].timeline_start, 3_000_000_000);
        assert_eq!(plan.title_clips[1].timeline_start, 6_000_000_000);
        assert_eq!(plan.video_clips[1].timeline_start, 9_000_000_000);
    }

    #[test]
    fn test_build_assembly_plan_no_titles() {
        let script = make_script();
        let alignment = AlignmentResult {
            mappings: vec![SceneMapping {
                clip_source_path: "clip1.mp4".to_string(),
                scene_id: "s1".to_string(),
                confidence: 0.9,
                source_in_ns: 0,
                source_out_ns: 3_000_000_000,
                transcript_excerpt: "hello".to_string(),
            }],
            unmatched_clips: Vec::new(),
        };

        let plan = build_assembly_plan(&script, &alignment, 0, 3_000_000_000, false);

        assert!(plan.title_clips.is_empty());
        assert_eq!(plan.video_clips.len(), 1);
        assert_eq!(plan.video_clips[0].timeline_start, 0);
    }

    #[test]
    fn test_multi_take_scene_creates_audition() {
        let script = make_script();
        // Two different source files match the same scene (s1).
        let alignment = AlignmentResult {
            mappings: vec![
                SceneMapping {
                    clip_source_path: "take_a.mp4".to_string(),
                    scene_id: "s1".to_string(),
                    confidence: 0.9,
                    source_in_ns: 0,
                    source_out_ns: 3_000_000_000,
                    transcript_excerpt: "hello".to_string(),
                },
                SceneMapping {
                    clip_source_path: "take_b.mp4".to_string(),
                    scene_id: "s1".to_string(),
                    confidence: 0.7,
                    source_in_ns: 0,
                    source_out_ns: 3_500_000_000,
                    transcript_excerpt: "hello".to_string(),
                },
            ],
            unmatched_clips: Vec::new(),
        };

        let plan = build_assembly_plan(&script, &alignment, 0, 3_000_000_000, false);

        // Should produce a single audition clip, not two separate clips.
        assert_eq!(
            plan.video_clips.len(),
            1,
            "Multiple takes => single audition"
        );
        let clip = &plan.video_clips[0];
        assert_eq!(clip.kind, ClipKind::Audition);
        assert_eq!(
            clip.source_path, "take_a.mp4",
            "Best confidence is active take"
        );
        assert_eq!(clip.alternate_takes.len(), 1);
        assert_eq!(clip.alternate_takes[0].source_path, "take_b.mp4");
    }

    #[test]
    fn test_multi_take_apply_creates_audition_clip() {
        let script = make_script();
        let alignment = AlignmentResult {
            mappings: vec![
                SceneMapping {
                    clip_source_path: "/tmp/take_a.mp4".to_string(),
                    scene_id: "s1".to_string(),
                    confidence: 0.95,
                    source_in_ns: 0,
                    source_out_ns: 2_000_000_000,
                    transcript_excerpt: "hello".to_string(),
                },
                SceneMapping {
                    clip_source_path: "/tmp/take_b.mp4".to_string(),
                    scene_id: "s1".to_string(),
                    confidence: 0.6,
                    source_in_ns: 0,
                    source_out_ns: 2_500_000_000,
                    transcript_excerpt: "hello".to_string(),
                },
            ],
            unmatched_clips: Vec::new(),
        };

        let plan = build_assembly_plan(&script, &alignment, 0, 0, false);
        let mut project = Project::new("Test");
        let mut library = MediaLibrary::new();
        apply_assembly_plan(&mut project, &mut library, &plan);

        // Find the audition clip on the video track.
        let video_track = project
            .tracks
            .iter()
            .find(|t| t.kind == crate::model::track::TrackKind::Video)
            .expect("video track");
        assert_eq!(video_track.clips.len(), 1);
        let clip = &video_track.clips[0];
        assert_eq!(clip.kind, ClipKind::Audition);
        assert!(clip.audition_takes.is_some());
        let takes = clip.audition_takes.as_ref().unwrap();
        assert_eq!(takes.len(), 2, "Active + 1 alternate = 2 takes");
        assert_eq!(takes[0].source_path, "/tmp/take_a.mp4");
        assert_eq!(takes[1].source_path, "/tmp/take_b.mp4");
        assert_eq!(clip.audition_active_take_index, 0);
        assert_eq!(clip.scene_id.as_deref(), Some("s1"));
    }

    #[test]
    fn test_same_source_subclips_not_grouped_as_audition() {
        let script = make_script();
        // Two mappings from the SAME source file for the same scene
        // (sub-clip splits) — should NOT become an audition.
        let alignment = AlignmentResult {
            mappings: vec![
                SceneMapping {
                    clip_source_path: "long_take.mp4".to_string(),
                    scene_id: "s1".to_string(),
                    confidence: 0.9,
                    source_in_ns: 0,
                    source_out_ns: 2_000_000_000,
                    transcript_excerpt: "first part".to_string(),
                },
                SceneMapping {
                    clip_source_path: "long_take.mp4".to_string(),
                    scene_id: "s1".to_string(),
                    confidence: 0.8,
                    source_in_ns: 2_000_000_000,
                    source_out_ns: 4_000_000_000,
                    transcript_excerpt: "second part".to_string(),
                },
            ],
            unmatched_clips: Vec::new(),
        };

        let plan = build_assembly_plan(&script, &alignment, 0, 0, false);

        // Should produce two separate video clips, not an audition.
        assert_eq!(plan.video_clips.len(), 2);
        assert_eq!(plan.video_clips[0].kind, ClipKind::Video);
        assert_eq!(plan.video_clips[1].kind, ClipKind::Video);
        assert!(plan.video_clips[0].alternate_takes.is_empty());
        assert!(plan.video_clips[1].alternate_takes.is_empty());
    }
}
