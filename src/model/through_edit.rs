#![allow(dead_code)]

use super::{clip::Clip, project::Project, track::Track};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ThroughEditDetectionOptions {
    pub source_contiguity_tolerance_ns: u64,
    pub timeline_contiguity_tolerance_ns: u64,
}

impl Default for ThroughEditDetectionOptions {
    fn default() -> Self {
        Self {
            source_contiguity_tolerance_ns: 0,
            timeline_contiguity_tolerance_ns: 0,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ThroughEditBoundary {
    pub track_id: String,
    pub left_clip_id: String,
    pub right_clip_id: String,
    pub left_clip_index: usize,
    pub right_clip_index: usize,
    pub boundary_ns: u64,
}

pub fn detect_project_through_edit_boundaries(project: &Project) -> Vec<ThroughEditBoundary> {
    detect_project_through_edit_boundaries_with_options(
        project,
        ThroughEditDetectionOptions::default(),
    )
}

pub fn detect_project_through_edit_boundaries_with_options(
    project: &Project,
    options: ThroughEditDetectionOptions,
) -> Vec<ThroughEditBoundary> {
    project
        .tracks
        .iter()
        .flat_map(|track| detect_track_through_edit_boundaries_with_options(track, options))
        .collect()
}

pub fn detect_track_through_edit_boundaries(track: &Track) -> Vec<ThroughEditBoundary> {
    detect_track_through_edit_boundaries_with_options(track, ThroughEditDetectionOptions::default())
}

pub fn detect_track_through_edit_boundaries_with_options(
    track: &Track,
    options: ThroughEditDetectionOptions,
) -> Vec<ThroughEditBoundary> {
    let mut ordered_indices: Vec<usize> = (0..track.clips.len()).collect();
    ordered_indices.sort_by(|left_idx, right_idx| {
        let left = &track.clips[*left_idx];
        let right = &track.clips[*right_idx];
        left.timeline_start
            .cmp(&right.timeline_start)
            .then_with(|| left.timeline_end().cmp(&right.timeline_end()))
            .then_with(|| left.id.cmp(&right.id))
    });

    ordered_indices
        .windows(2)
        .filter_map(|window| {
            let left_index = window[0];
            let right_index = window[1];
            let left = &track.clips[left_index];
            let right = &track.clips[right_index];
            if !is_track_pair_through_edit_candidate(left, right, options) {
                return None;
            }

            Some(ThroughEditBoundary {
                track_id: track.id.clone(),
                left_clip_id: left.id.clone(),
                right_clip_id: right.id.clone(),
                left_clip_index: left_index,
                right_clip_index: right_index,
                boundary_ns: right.timeline_start,
            })
        })
        .collect()
}

pub fn is_track_through_edit_boundary(
    track: &Track,
    left_clip_id: &str,
    right_clip_id: &str,
) -> bool {
    is_track_through_edit_boundary_with_options(
        track,
        left_clip_id,
        right_clip_id,
        ThroughEditDetectionOptions::default(),
    )
}

pub fn is_track_through_edit_boundary_with_options(
    track: &Track,
    left_clip_id: &str,
    right_clip_id: &str,
    options: ThroughEditDetectionOptions,
) -> bool {
    detect_track_through_edit_boundaries_with_options(track, options)
        .iter()
        .any(|boundary| {
            boundary.left_clip_id == left_clip_id && boundary.right_clip_id == right_clip_id
        })
}

fn is_track_pair_through_edit_candidate(
    left: &Clip,
    right: &Clip,
    options: ThroughEditDetectionOptions,
) -> bool {
    !left.is_compound()
        && !right.is_compound()
        && !left.is_multicam()
        && !right.is_multicam()
        && left.source_path == right.source_path
        && left.kind == right.kind
        && left.source_out.abs_diff(right.source_in) <= options.source_contiguity_tolerance_ns
        && left.timeline_end().abs_diff(right.timeline_start)
            <= options.timeline_contiguity_tolerance_ns
        && !clip_has_outgoing_transition(left)
}

fn clip_has_outgoing_transition(clip: &Clip) -> bool {
    clip.outgoing_transition.is_active()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::clip::{Clip, ClipKind};
    use crate::model::project::Project;
    use crate::model::track::Track;
    use crate::model::transition::{OutgoingTransition, TransitionAlignment};

    fn make_clip(
        id: &str,
        source_path: &str,
        source_in: u64,
        source_out: u64,
        timeline_start: u64,
        kind: ClipKind,
    ) -> Clip {
        let mut clip = Clip::new(source_path, source_out, timeline_start, kind);
        clip.id = id.to_string();
        clip.source_in = source_in;
        clip.source_out = source_out;
        clip
    }

    #[test]
    fn detects_join_safe_through_edit_boundary() {
        let mut track = Track::new_video("V1");
        track.id = "track-1".to_string();
        track.add_clip(make_clip("left", "a.mov", 0, 10, 0, ClipKind::Video));
        track.add_clip(make_clip("right", "a.mov", 10, 20, 10, ClipKind::Video));

        let boundaries = detect_track_through_edit_boundaries(&track);
        assert_eq!(boundaries.len(), 1);
        assert_eq!(boundaries[0].track_id, "track-1");
        assert_eq!(boundaries[0].left_clip_id, "left");
        assert_eq!(boundaries[0].right_clip_id, "right");
        assert_eq!(boundaries[0].boundary_ns, 10);
    }

    #[test]
    fn rejects_boundary_when_source_is_not_contiguous() {
        let mut track = Track::new_video("V1");
        track.add_clip(make_clip("left", "a.mov", 0, 10, 0, ClipKind::Video));
        track.add_clip(make_clip("right", "a.mov", 11, 21, 10, ClipKind::Video));

        assert!(detect_track_through_edit_boundaries(&track).is_empty());
    }

    #[test]
    fn rejects_boundary_when_timeline_is_not_contiguous() {
        let mut track = Track::new_video("V1");
        track.add_clip(make_clip("left", "a.mov", 0, 10, 0, ClipKind::Video));
        track.add_clip(make_clip("right", "a.mov", 10, 20, 12, ClipKind::Video));

        assert!(detect_track_through_edit_boundaries(&track).is_empty());
    }

    #[test]
    fn rejects_boundary_when_transition_exists_at_cut() {
        let mut track = Track::new_video("V1");
        let mut left = make_clip("left", "a.mov", 0, 10, 0, ClipKind::Video);
        left.outgoing_transition =
            OutgoingTransition::new("cross_dissolve", 500_000_000, TransitionAlignment::EndOnCut);
        track.add_clip(left);
        track.add_clip(make_clip("right", "a.mov", 10, 20, 10, ClipKind::Video));

        assert!(detect_track_through_edit_boundaries(&track).is_empty());
    }

    #[test]
    fn rejects_boundary_when_transition_duration_exists_even_without_kind() {
        let mut track = Track::new_video("V1");
        let mut left = make_clip("left", "a.mov", 0, 10, 0, ClipKind::Video);
        left.outgoing_transition =
            OutgoingTransition::new("cross_dissolve", 250_000_000, TransitionAlignment::EndOnCut);
        track.add_clip(left);
        track.add_clip(make_clip("right", "a.mov", 10, 20, 10, ClipKind::Video));

        assert!(detect_track_through_edit_boundaries(&track).is_empty());
    }

    #[test]
    fn allows_boundary_when_right_clip_has_outgoing_transition() {
        let mut track = Track::new_video("V1");
        track.add_clip(make_clip("left", "a.mov", 0, 10, 0, ClipKind::Video));
        let mut right = make_clip("right", "a.mov", 10, 20, 10, ClipKind::Video);
        right.outgoing_transition =
            OutgoingTransition::new("cross_dissolve", 250_000_000, TransitionAlignment::EndOnCut);
        track.add_clip(right);

        let boundaries = detect_track_through_edit_boundaries(&track);
        assert_eq!(boundaries.len(), 1);
        assert_eq!(boundaries[0].left_clip_id, "left");
        assert_eq!(boundaries[0].right_clip_id, "right");
    }

    #[test]
    fn ignores_whitespace_transition_kind_when_duration_is_zero() {
        let mut track = Track::new_video("V1");
        let mut left = make_clip("left", "a.mov", 0, 10, 0, ClipKind::Video);
        left.outgoing_transition.kind = "   ".to_string();
        track.add_clip(left);
        track.add_clip(make_clip("right", "a.mov", 10, 20, 10, ClipKind::Video));

        let boundaries = detect_track_through_edit_boundaries(&track);
        assert_eq!(boundaries.len(), 1);
    }

    #[test]
    fn rejects_incompatible_clip_kinds() {
        let mut track = Track::new_video("V1");
        track.add_clip(make_clip("left", "a.mov", 0, 10, 0, ClipKind::Video));
        track.add_clip(make_clip("right", "a.mov", 10, 20, 10, ClipKind::Image));

        assert!(detect_track_through_edit_boundaries(&track).is_empty());
    }

    #[test]
    fn uses_tolerance_for_near_contiguous_ranges() {
        let mut track = Track::new_video("V1");
        track.add_clip(make_clip("left", "a.mov", 0, 10_000, 0, ClipKind::Video));
        track.add_clip(make_clip(
            "right",
            "a.mov",
            10_040,
            20_040,
            10_030,
            ClipKind::Video,
        ));

        assert!(detect_track_through_edit_boundaries(&track).is_empty());

        let boundaries = detect_track_through_edit_boundaries_with_options(
            &track,
            ThroughEditDetectionOptions {
                source_contiguity_tolerance_ns: 50,
                timeline_contiguity_tolerance_ns: 30,
            },
        );
        assert_eq!(boundaries.len(), 1);
    }

    #[test]
    fn detection_order_is_deterministic_for_unsorted_clips() {
        let mut track = Track::new_video("V1");
        let clip_b = make_clip("b", "a.mov", 10, 20, 10, ClipKind::Video);
        let clip_a = make_clip("a", "a.mov", 0, 10, 0, ClipKind::Video);
        let clip_c = make_clip("c", "a.mov", 20, 30, 20, ClipKind::Video);

        track.push_unsorted(clip_b);
        track.push_unsorted(clip_a);
        track.push_unsorted(clip_c);

        let boundaries = detect_track_through_edit_boundaries(&track);
        assert_eq!(boundaries.len(), 2);
        assert_eq!(
            boundaries
                .iter()
                .map(|b| (b.left_clip_id.as_str(), b.right_clip_id.as_str()))
                .collect::<Vec<_>>(),
            vec![("a", "b"), ("b", "c")]
        );
        assert!(is_track_through_edit_boundary(&track, "a", "b"));
        assert!(!is_track_through_edit_boundary(&track, "a", "c"));
    }

    #[test]
    fn project_detection_returns_boundaries_per_track() {
        let mut project = Project::new("Through Edit Test");
        let video_track_id = {
            let video_track = &mut project.tracks[0];
            video_track.clips.clear();
            video_track.add_clip(make_clip("v1", "video.mov", 0, 10, 0, ClipKind::Video));
            video_track.add_clip(make_clip("v2", "video.mov", 10, 20, 10, ClipKind::Video));
            video_track.id.clone()
        };

        let audio_track_id = {
            let audio_track = &mut project.tracks[1];
            audio_track.clips.clear();
            audio_track.add_clip(make_clip("a1", "audio.wav", 0, 10, 0, ClipKind::Audio));
            audio_track.add_clip(make_clip("a2", "audio.wav", 10, 20, 10, ClipKind::Audio));
            audio_track.id.clone()
        };

        let boundaries = detect_project_through_edit_boundaries(&project);
        assert_eq!(boundaries.len(), 2);
        assert_eq!(
            boundaries
                .iter()
                .map(|b| b.track_id.as_str())
                .collect::<Vec<_>>(),
            vec![video_track_id.as_str(), audio_track_id.as_str()]
        );
    }
}
