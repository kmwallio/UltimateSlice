use crate::model::clip::Clip;
use crate::model::track::Track;
use serde::{Deserialize, Serialize};
use thiserror::Error;

pub const DEFAULT_TRANSITION_DURATION_NS: u64 = 500_000_000;
pub const MIN_TRANSITION_DURATION_NS: u64 = 10_000_000;

pub const SUPPORTED_TRANSITION_KINDS: &[&str] =
    &["cross_dissolve", "fade_to_black", "wipe_right", "wipe_left"];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum TransitionAlignment {
    StartOnCut,
    CenterOnCut,
    #[default]
    EndOnCut,
}

impl TransitionAlignment {
    pub const ALL: [TransitionAlignment; 3] = [
        TransitionAlignment::EndOnCut,
        TransitionAlignment::CenterOnCut,
        TransitionAlignment::StartOnCut,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::EndOnCut => "End on cut",
            Self::CenterOnCut => "Center on cut",
            Self::StartOnCut => "Start on cut",
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::EndOnCut => "end_on_cut",
            Self::CenterOnCut => "center_on_cut",
            Self::StartOnCut => "start_on_cut",
        }
    }

    pub fn from_str(value: &str) -> Option<Self> {
        match value.trim() {
            "end_on_cut" | "End on cut" => Some(Self::EndOnCut),
            "center_on_cut" | "Center on cut" => Some(Self::CenterOnCut),
            "start_on_cut" | "Start on cut" => Some(Self::StartOnCut),
            _ => None,
        }
    }

    pub fn split_duration(self, duration_ns: u64) -> TransitionCutSplit {
        let before_cut_ns = match self {
            Self::EndOnCut => duration_ns,
            Self::CenterOnCut => duration_ns / 2,
            Self::StartOnCut => 0,
        };
        TransitionCutSplit {
            before_cut_ns,
            after_cut_ns: duration_ns.saturating_sub(before_cut_ns),
        }
    }

    pub fn from_before_cut_duration(before_cut_ns: u64, duration_ns: u64) -> Self {
        if duration_ns == 0 || before_cut_ns == 0 {
            Self::StartOnCut
        } else if before_cut_ns >= duration_ns {
            Self::EndOnCut
        } else {
            Self::CenterOnCut
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TransitionCutSplit {
    pub before_cut_ns: u64,
    pub after_cut_ns: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TransitionOverlapWindow {
    pub start_ns: u64,
    pub end_ns: u64,
    pub before_cut_ns: u64,
    pub after_cut_ns: u64,
}

impl TransitionCutSplit {
    pub fn duration_ns(self) -> u64 {
        self.before_cut_ns.saturating_add(self.after_cut_ns)
    }

    pub fn overlap_window(self, cut_ns: u64) -> TransitionOverlapWindow {
        TransitionOverlapWindow {
            start_ns: cut_ns.saturating_sub(self.before_cut_ns),
            end_ns: cut_ns.saturating_add(self.after_cut_ns),
            before_cut_ns: self.before_cut_ns,
            after_cut_ns: self.after_cut_ns,
        }
    }
}

impl TransitionOverlapWindow {
    pub fn duration_ns(self) -> u64 {
        self.before_cut_ns.saturating_add(self.after_cut_ns)
    }

    pub fn contains(self, timeline_pos_ns: u64) -> bool {
        self.duration_ns() > 0 && timeline_pos_ns >= self.start_ns && timeline_pos_ns < self.end_ns
    }

    pub fn progress_at(self, timeline_pos_ns: u64) -> Option<f64> {
        if !self.contains(timeline_pos_ns) {
            return None;
        }
        Some(
            (timeline_pos_ns.saturating_sub(self.start_ns) as f64 / self.duration_ns() as f64)
                .clamp(0.0, 1.0),
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct OutgoingTransition {
    #[serde(default, rename = "transition_after")]
    pub kind: String,
    #[serde(default, rename = "transition_after_ns")]
    pub duration_ns: u64,
    #[serde(default, rename = "transition_after_alignment")]
    pub alignment: TransitionAlignment,
}

impl Default for OutgoingTransition {
    fn default() -> Self {
        Self {
            kind: String::new(),
            duration_ns: 0,
            alignment: TransitionAlignment::default(),
        }
    }
}

impl OutgoingTransition {
    pub fn new(kind: impl Into<String>, duration_ns: u64, alignment: TransitionAlignment) -> Self {
        let kind = kind.into().trim().to_string();
        if kind.is_empty() || duration_ns == 0 {
            Self::default()
        } else {
            Self {
                kind,
                duration_ns,
                alignment,
            }
        }
    }

    pub fn is_active(&self) -> bool {
        !self.kind.trim().is_empty() && self.duration_ns > 0
    }

    pub fn clear(&mut self) {
        *self = Self::default();
    }

    pub fn kind_trimmed(&self) -> &str {
        self.kind.trim()
    }

    pub fn cut_split(&self) -> TransitionCutSplit {
        self.alignment.split_duration(self.duration_ns)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValidatedTransitionEdit {
    pub transition: OutgoingTransition,
    pub max_duration_ns: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum TransitionValidationError {
    #[error("clip_index must reference a clip with a following clip")]
    MissingFollowingClip,
    #[error("unsupported transition kind '{kind}'")]
    UnsupportedKind { kind: String },
    #[error("duration_ns must be greater than 0 when kind is set")]
    MissingDuration,
    #[error("transition duration exceeds boundary capacity (max {max_duration_ns} ns)")]
    BoundaryTooShort { max_duration_ns: u64 },
}

pub fn is_supported_transition_kind(kind: &str) -> bool {
    SUPPORTED_TRANSITION_KINDS.contains(&kind)
}

pub fn max_transition_duration_ns(outgoing: &Clip, incoming: &Clip) -> u64 {
    outgoing
        .duration()
        .min(incoming.duration())
        .saturating_sub(1_000_000)
}

pub fn validate_track_transition_request(
    track: &Track,
    clip_index: usize,
    kind: &str,
    duration_ns: u64,
    alignment: TransitionAlignment,
) -> Result<ValidatedTransitionEdit, TransitionValidationError> {
    let Some(outgoing) = track.clips.get(clip_index) else {
        return Err(TransitionValidationError::MissingFollowingClip);
    };
    let trimmed_kind = kind.trim();
    if trimmed_kind.is_empty() {
        return Ok(ValidatedTransitionEdit {
            transition: OutgoingTransition::default(),
            max_duration_ns: track
                .clips
                .get(clip_index + 1)
                .map(|incoming| max_transition_duration_ns(outgoing, incoming))
                .unwrap_or(0),
        });
    }
    let Some(incoming) = track.clips.get(clip_index + 1) else {
        return Err(TransitionValidationError::MissingFollowingClip);
    };
    let max_duration_ns = max_transition_duration_ns(outgoing, incoming);
    if !is_supported_transition_kind(trimmed_kind) {
        return Err(TransitionValidationError::UnsupportedKind {
            kind: trimmed_kind.to_string(),
        });
    }
    if duration_ns == 0 {
        return Err(TransitionValidationError::MissingDuration);
    }
    if max_duration_ns < MIN_TRANSITION_DURATION_NS {
        return Err(TransitionValidationError::BoundaryTooShort { max_duration_ns });
    }
    Ok(ValidatedTransitionEdit {
        transition: OutgoingTransition::new(
            trimmed_kind,
            duration_ns.clamp(MIN_TRANSITION_DURATION_NS, max_duration_ns),
            alignment,
        ),
        max_duration_ns,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::clip::{Clip, ClipKind};
    use crate::model::track::Track;

    fn make_clip(id: &str, duration_ns: u64, timeline_start: u64) -> Clip {
        let mut clip = Clip::new(
            format!("{id}.mov"),
            duration_ns,
            timeline_start,
            ClipKind::Video,
        );
        clip.id = id.to_string();
        clip
    }

    #[test]
    fn transition_alignment_splits_duration() {
        assert_eq!(
            TransitionAlignment::EndOnCut.split_duration(900),
            TransitionCutSplit {
                before_cut_ns: 900,
                after_cut_ns: 0,
            }
        );
        assert_eq!(
            TransitionAlignment::CenterOnCut.split_duration(901),
            TransitionCutSplit {
                before_cut_ns: 450,
                after_cut_ns: 451,
            }
        );
        assert_eq!(
            TransitionAlignment::StartOnCut.split_duration(900),
            TransitionCutSplit {
                before_cut_ns: 0,
                after_cut_ns: 900,
            }
        );
    }

    #[test]
    fn transition_overlap_window_tracks_cut_position() {
        let window = TransitionAlignment::CenterOnCut
            .split_duration(900)
            .overlap_window(10_000);
        assert_eq!(
            window,
            TransitionOverlapWindow {
                start_ns: 9_550,
                end_ns: 10_450,
                before_cut_ns: 450,
                after_cut_ns: 450,
            }
        );
        assert_eq!(window.progress_at(9_550), Some(0.0));
        assert_eq!(window.progress_at(10_000), Some(0.5));
        assert_eq!(window.progress_at(10_449), Some(899.0 / 900.0));
        assert_eq!(window.progress_at(10_450), None);
    }

    #[test]
    fn validate_track_transition_clamps_duration() {
        let mut track = Track::new_video("V1");
        track.add_clip(make_clip("a", 1_000_000_000, 0));
        track.add_clip(make_clip("b", 600_000_000, 1_000_000_000));
        let validated = validate_track_transition_request(
            &track,
            0,
            "cross_dissolve",
            900_000_000,
            TransitionAlignment::CenterOnCut,
        )
        .unwrap();
        assert_eq!(validated.max_duration_ns, 599_000_000);
        assert_eq!(
            validated.transition,
            OutgoingTransition::new(
                "cross_dissolve",
                599_000_000,
                TransitionAlignment::CenterOnCut
            )
        );
    }

    #[test]
    fn validate_track_transition_rejects_unknown_kind() {
        let mut track = Track::new_video("V1");
        track.add_clip(make_clip("a", 1_000_000_000, 0));
        track.add_clip(make_clip("b", 1_000_000_000, 1_000_000_000));
        let err = validate_track_transition_request(
            &track,
            0,
            "iris",
            500_000_000,
            TransitionAlignment::EndOnCut,
        )
        .unwrap_err();
        assert!(matches!(
            err,
            TransitionValidationError::UnsupportedKind { .. }
        ));
    }

    #[test]
    fn validate_track_transition_allows_clearing_without_following_clip() {
        let mut track = Track::new_video("V1");
        track.add_clip(make_clip("a", 1_000_000_000, 0));
        let validated = validate_track_transition_request(
            &track,
            0,
            "",
            DEFAULT_TRANSITION_DURATION_NS,
            TransitionAlignment::EndOnCut,
        )
        .expect("clearing should succeed");
        assert_eq!(validated.transition, OutgoingTransition::default());
        assert_eq!(validated.max_duration_ns, 0);
    }
}
