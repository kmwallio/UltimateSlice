use serde::{Deserialize, Serialize};
use uuid::Uuid;
use super::clip::Clip;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum TrackKind {
    Video,
    Audio,
}

/// A single horizontal lane in the timeline
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Track {
    pub id: String,
    pub kind: TrackKind,
    pub label: String,
    pub clips: Vec<Clip>,
    pub muted: bool,
    pub locked: bool,
}

impl Track {
    pub fn new_video(label: impl Into<String>) -> Self {
        Self {
            id: Uuid::new_v4().to_string(),
            kind: TrackKind::Video,
            label: label.into(),
            clips: Vec::new(),
            muted: false,
            locked: false,
        }
    }

    pub fn new_audio(label: impl Into<String>) -> Self {
        Self {
            id: Uuid::new_v4().to_string(),
            kind: TrackKind::Audio,
            label: label.into(),
            clips: Vec::new(),
            muted: false,
            locked: false,
        }
    }

    /// Add a clip and keep clips sorted by timeline position
    pub fn add_clip(&mut self, clip: Clip) {
        self.clips.push(clip);
        self.clips.sort_by_key(|c| c.timeline_start);
    }

    /// Append a clip without sorting. Call `sort_clips()` once after bulk insertion.
    pub fn push_unsorted(&mut self, clip: Clip) {
        self.clips.push(clip);
    }

    /// Sort clips by timeline position. Use after one or more `push_unsorted()` calls.
    pub fn sort_clips(&mut self) {
        self.clips.sort_by_key(|c| c.timeline_start);
    }

    /// Remove timeline gaps by packing clips back-to-back in timeline order.
    pub fn compact_gap_free(&mut self) {
        self.clips.sort_by_key(|c| c.timeline_start);
        let mut cursor = 0_u64;
        for clip in &mut self.clips {
            clip.timeline_start = cursor;
            cursor = clip.timeline_end();
        }
    }

    pub fn remove_clip(&mut self, clip_id: &str) {
        self.clips.retain(|c| c.id != clip_id);
    }

    /// Total timeline duration covered by this track's clips
    pub fn duration(&self) -> u64 {
        self.clips.iter().map(|c| c.timeline_end()).max().unwrap_or(0)
    }
}
