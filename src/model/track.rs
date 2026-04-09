use super::clip::Clip;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

fn default_duck_amount_db() -> f64 {
    -6.0
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub enum TrackKind {
    Video,
    Audio,
}

/// Audio role for a track — determines submix routing and FCPXML role metadata.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum AudioRole {
    /// No role assigned (mixed into master).
    #[default]
    None,
    /// Dialogue / voice-over.
    Dialogue,
    /// Sound effects / foley.
    Effects,
    /// Music / score.
    Music,
}

impl AudioRole {
    pub fn label(self) -> &'static str {
        match self {
            Self::None => "None",
            Self::Dialogue => "Dialogue",
            Self::Effects => "Effects",
            Self::Music => "Music",
        }
    }

    pub fn short_label(self) -> &'static str {
        match self {
            Self::None => "",
            Self::Dialogue => "DLG",
            Self::Effects => "SFX",
            Self::Music => "MUS",
        }
    }

    pub fn from_str(s: &str) -> Self {
        match s.to_ascii_lowercase().as_str() {
            "dialogue" => Self::Dialogue,
            "effects" => Self::Effects,
            "music" => Self::Music,
            _ => Self::None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Dialogue => "dialogue",
            Self::Effects => "effects",
            Self::Music => "music",
        }
    }

    /// FCPXML role attribute value.
    pub fn fcpxml_role(self) -> &'static str {
        match self {
            Self::None => "dialogue",
            Self::Dialogue => "dialogue",
            Self::Effects => "effects",
            Self::Music => "music",
        }
    }

    pub const ALL: [AudioRole; 4] = [Self::None, Self::Dialogue, Self::Effects, Self::Music];
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TrackHeightPreset {
    Small,
    Medium,
    Large,
}

impl Default for TrackHeightPreset {
    fn default() -> Self {
        Self::Medium
    }
}

/// A single horizontal lane in the timeline
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Track {
    pub id: String,
    pub kind: TrackKind,
    pub label: String,
    pub clips: Vec<Clip>,
    pub muted: bool,
    pub locked: bool,
    #[serde(default)]
    pub soloed: bool,
    #[serde(default)]
    pub height_preset: TrackHeightPreset,
    /// Audio role for submix routing and FCPXML metadata.
    #[serde(default)]
    pub audio_role: AudioRole,
    /// When true, this track's volume is automatically reduced (ducked) when
    /// audio is present on any non-ducked track at the same timeline position.
    /// Typically enabled on music/effects tracks so dialogue comes through clearly.
    #[serde(default)]
    pub duck: bool,
    /// Ducking volume reduction in dB (negative). Default −6.0.
    /// Only applied when `duck` is true.
    #[serde(default = "default_duck_amount_db")]
    pub duck_amount_db: f64,
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
            soloed: false,
            height_preset: TrackHeightPreset::Medium,
            audio_role: AudioRole::default(),
            duck: false,
            duck_amount_db: default_duck_amount_db(),
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
            soloed: false,
            height_preset: TrackHeightPreset::Medium,
            audio_role: AudioRole::default(),
            duck: false,
            duck_amount_db: default_duck_amount_db(),
        }
    }

    /// True if this is a video track.
    pub fn is_video(&self) -> bool {
        self.kind == TrackKind::Video
    }

    /// True if this is an audio track.
    pub fn is_audio(&self) -> bool {
        self.kind == TrackKind::Audio
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
        self.clips
            .iter()
            .map(|c| c.timeline_end())
            .max()
            .unwrap_or(0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::clip::{Clip, ClipKind};

    fn make_test_clip(id: &str, source_out: u64, timeline_start: u64) -> Clip {
        let mut c = Clip::new("file.mp4", source_out, timeline_start, ClipKind::Video);
        c.id = id.to_string();
        c
    }

    #[test]
    fn test_new_video_track() {
        let track = Track::new_video("V1");
        assert_eq!(track.label, "V1");
        assert_eq!(track.kind, TrackKind::Video);
        assert!(track.clips.is_empty());
        assert!(!track.muted);
        assert!(!track.locked);
        assert!(!track.soloed);
        assert_eq!(track.height_preset, TrackHeightPreset::Medium);
    }

    #[test]
    fn test_new_audio_track() {
        let track = Track::new_audio("A1");
        assert_eq!(track.label, "A1");
        assert_eq!(track.kind, TrackKind::Audio);
        assert!(!track.soloed);
        assert_eq!(track.height_preset, TrackHeightPreset::Medium);
    }

    #[test]
    fn test_track_deserialize_backwards_compatible_height_default() {
        let json = r#"{
            "id":"track-1",
            "kind":"Video",
            "label":"V1",
            "clips":[],
            "muted":false,
            "locked":false,
            "soloed":false
        }"#;
        let track: Track = serde_json::from_str(json).expect("track should deserialize");
        assert_eq!(track.height_preset, TrackHeightPreset::Medium);
    }

    #[test]
    fn test_add_clip_sorted() {
        let mut track = Track::new_video("V1");
        track.add_clip(make_test_clip("B", 5, 10));
        track.add_clip(make_test_clip("A", 5, 0));
        assert_eq!(track.clips[0].id, "A");
        assert_eq!(track.clips[1].id, "B");
    }

    #[test]
    fn test_remove_clip() {
        let mut track = Track::new_video("V1");
        track.add_clip(make_test_clip("A", 5, 0));
        track.add_clip(make_test_clip("B", 5, 10));
        track.remove_clip("A");
        assert_eq!(track.clips.len(), 1);
        assert_eq!(track.clips[0].id, "B");
    }

    #[test]
    fn test_remove_nonexistent_clip() {
        let mut track = Track::new_video("V1");
        track.add_clip(make_test_clip("A", 5, 0));
        track.remove_clip("Z");
        assert_eq!(track.clips.len(), 1);
    }

    #[test]
    fn test_track_duration_empty() {
        let track = Track::new_video("V1");
        assert_eq!(track.duration(), 0);
    }

    #[test]
    fn test_track_duration_with_clips() {
        let mut track = Track::new_video("V1");
        // clip at 0..5, clip at 10..20
        track.add_clip(make_test_clip("A", 5, 0));
        track.add_clip(make_test_clip("B", 10, 10));
        assert_eq!(track.duration(), 20);
    }

    #[test]
    fn test_compact_gap_free() {
        let mut track = Track::new_video("V1");
        // clip at 0..5, gap, clip at 20..30 (duration 10)
        track.add_clip(make_test_clip("A", 5, 0));
        track.add_clip(make_test_clip("B", 10, 20));
        track.compact_gap_free();
        let a = track.clips.iter().find(|c| c.id == "A").unwrap();
        let b = track.clips.iter().find(|c| c.id == "B").unwrap();
        assert_eq!(a.timeline_start, 0);
        assert_eq!(b.timeline_start, 5);
    }

    #[test]
    fn test_push_unsorted_and_sort() {
        let mut track = Track::new_video("V1");
        track.push_unsorted(make_test_clip("B", 5, 10));
        track.push_unsorted(make_test_clip("A", 5, 0));
        track.sort_clips();
        assert_eq!(track.clips[0].id, "A");
        assert_eq!(track.clips[1].id, "B");
    }
}
