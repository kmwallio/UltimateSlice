use super::track::{Track, TrackKind};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// A timeline marker (chapter point / note) placed at a specific position.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Marker {
    pub id: String,
    /// Position on the timeline in nanoseconds
    pub position_ns: u64,
    /// Short label shown on the ruler
    pub label: String,
    /// RGBA colour packed as 0xRRGGBBAA (default orange = 0xFF8C00FF)
    #[serde(default = "default_marker_color")]
    pub color: u32,
}

fn default_marker_color() -> u32 {
    0xFF8C00FF
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct FcpxmlUnknownNode {
    #[serde(default)]
    pub attrs: Vec<(String, String)>,
    #[serde(default)]
    pub children: Vec<String>,
}

impl Marker {
    pub fn new(position_ns: u64, label: impl Into<String>) -> Self {
        Self {
            id: Uuid::new_v4().to_string(),
            position_ns,
            label: label.into(),
            color: default_marker_color(),
        }
    }
}

/// Frame rate as a rational number
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FrameRate {
    pub numerator: u32,
    pub denominator: u32,
}

impl FrameRate {
    pub fn fps_24() -> Self {
        Self {
            numerator: 24,
            denominator: 1,
        }
    }
    #[allow(dead_code)]
    pub fn fps_30() -> Self {
        Self {
            numerator: 30000,
            denominator: 1001,
        }
    }
    #[allow(dead_code)]
    pub fn fps_60() -> Self {
        Self {
            numerator: 60,
            denominator: 1,
        }
    }

    pub fn as_f64(&self) -> f64 {
        self.numerator as f64 / self.denominator as f64
    }

    pub fn frame_duration_ns(&self) -> u64 {
        if self.numerator == 0 {
            return 41_666_667; // fallback ~24fps
        }
        ((self.denominator as u64) * 1_000_000_000) / (self.numerator as u64)
    }
}

/// The top-level project, containing all tracks and sequence settings
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Project {
    pub title: String,
    pub width: u32,
    pub height: u32,
    pub frame_rate: FrameRate,
    pub tracks: Vec<Track>,
    /// Timeline markers / chapter points
    #[serde(default)]
    pub markers: Vec<Marker>,
    /// Dirty flag — true if there are unsaved changes
    #[serde(skip)]
    pub dirty: bool,
    /// Path to the saved project file, if any
    #[serde(skip)]
    pub file_path: Option<String>,
    /// Original FCPXML document captured at import time for lossless clean-save passthrough.
    #[serde(skip)]
    pub source_fcpxml: Option<String>,
    /// Unknown FCPXML root (`<fcpxml>`) attrs/children preserved for dirty-save regeneration.
    #[serde(skip)]
    pub fcpxml_unknown_root: FcpxmlUnknownNode,
    /// Unknown FCPXML `<resources>` attrs/children preserved for dirty-save regeneration.
    #[serde(skip)]
    pub fcpxml_unknown_resources: FcpxmlUnknownNode,
    /// Unknown attrs on the selected sequence's referenced `<format>` resource.
    #[serde(skip)]
    pub fcpxml_unknown_format: FcpxmlUnknownNode,
    /// Unknown FCPXML `<library>` attrs/children preserved for dirty-save regeneration.
    #[serde(skip)]
    pub fcpxml_unknown_library: FcpxmlUnknownNode,
    /// Unknown FCPXML selected `<event>` attrs/children preserved for dirty-save regeneration.
    #[serde(skip)]
    pub fcpxml_unknown_event: FcpxmlUnknownNode,
    /// Unknown FCPXML selected `<project>` attrs/children preserved for dirty-save regeneration.
    #[serde(skip)]
    pub fcpxml_unknown_project: FcpxmlUnknownNode,
    /// Unknown FCPXML selected `<sequence>` attrs/children preserved for dirty-save regeneration.
    #[serde(skip)]
    pub fcpxml_unknown_sequence: FcpxmlUnknownNode,
    /// Unknown FCPXML selected `<spine>` attrs/children preserved for dirty-save regeneration.
    #[serde(skip)]
    pub fcpxml_unknown_spine: FcpxmlUnknownNode,
}

impl Project {
    pub fn new(title: impl Into<String>) -> Self {
        let mut project = Self {
            title: title.into(),
            width: 1920,
            height: 1080,
            frame_rate: FrameRate::fps_24(),
            tracks: Vec::new(),
            markers: Vec::new(),
            dirty: false,
            file_path: None,
            source_fcpxml: None,
            fcpxml_unknown_root: FcpxmlUnknownNode::default(),
            fcpxml_unknown_resources: FcpxmlUnknownNode::default(),
            fcpxml_unknown_format: FcpxmlUnknownNode::default(),
            fcpxml_unknown_library: FcpxmlUnknownNode::default(),
            fcpxml_unknown_event: FcpxmlUnknownNode::default(),
            fcpxml_unknown_project: FcpxmlUnknownNode::default(),
            fcpxml_unknown_sequence: FcpxmlUnknownNode::default(),
            fcpxml_unknown_spine: FcpxmlUnknownNode::default(),
        };
        // Default tracks like FCP
        project.tracks.push(Track::new_video("Video 1"));
        project.tracks.push(Track::new_audio("Audio 1"));
        project
    }

    pub fn video_tracks(&self) -> impl Iterator<Item = &Track> {
        self.tracks.iter().filter(|t| t.kind == TrackKind::Video)
    }

    pub fn audio_tracks(&self) -> impl Iterator<Item = &Track> {
        self.tracks.iter().filter(|t| t.kind == TrackKind::Audio)
    }

    pub fn has_solo_tracks(&self) -> bool {
        self.tracks.iter().any(|t| t.soloed)
    }

    pub fn track_is_active_for_output(&self, track: &Track) -> bool {
        if track.muted {
            return false;
        }
        !self.has_solo_tracks() || track.soloed
    }

    /// Total sequence duration across all tracks, in nanoseconds
    pub fn duration(&self) -> u64 {
        self.tracks.iter().map(|t| t.duration()).max().unwrap_or(0)
    }

    #[allow(dead_code)]
    pub fn add_video_track(&mut self) {
        let n = self.video_tracks().count() + 1;
        self.tracks.push(Track::new_video(format!("Video {n}")));
        self.dirty = true;
    }

    #[allow(dead_code)]
    pub fn add_audio_track(&mut self) {
        let n = self.audio_tracks().count() + 1;
        self.tracks.push(Track::new_audio(format!("Audio {n}")));
        self.dirty = true;
    }

    pub fn track_mut(&mut self, track_id: &str) -> Option<&mut Track> {
        self.tracks.iter_mut().find(|t| t.id == track_id)
    }

    /// Add a marker at the given position. Returns the new marker's id.
    pub fn add_marker(&mut self, position_ns: u64, label: impl Into<String>) -> String {
        let m = Marker::new(position_ns, label);
        let id = m.id.clone();
        self.markers.push(m);
        self.markers.sort_by_key(|m| m.position_ns);
        self.dirty = true;
        id
    }

    /// Remove a marker by id.
    pub fn remove_marker(&mut self, id: &str) {
        self.markers.retain(|m| m.id != id);
        self.dirty = true;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::clip::{Clip, ClipKind};

    #[test]
    fn test_project_new_defaults() {
        let p = Project::new("My Project");
        assert_eq!(p.title, "My Project");
        assert_eq!(p.width, 1920);
        assert_eq!(p.height, 1080);
        assert!(!p.dirty);
        assert!(p.file_path.is_none());
    }

    #[test]
    fn test_project_new_has_default_tracks() {
        let p = Project::new("Test");
        assert_eq!(p.tracks.len(), 2);
        assert_eq!(p.video_tracks().count(), 1);
        assert_eq!(p.audio_tracks().count(), 1);
    }

    #[test]
    fn test_frame_rate_fps24() {
        let fps = FrameRate::fps_24();
        assert!((fps.as_f64() - 24.0).abs() < 1e-6);
    }

    #[test]
    fn test_frame_rate_fps30() {
        let fps = FrameRate::fps_30();
        assert!((fps.as_f64() - 29.97).abs() < 0.01);
    }

    #[test]
    fn test_frame_rate_fps60() {
        let fps = FrameRate::fps_60();
        assert!((fps.as_f64() - 60.0).abs() < 1e-6);
    }

    #[test]
    fn test_add_video_track() {
        let mut p = Project::new("Test");
        p.add_video_track();
        assert_eq!(p.video_tracks().count(), 2);
        assert!(p.dirty);
    }

    #[test]
    fn test_add_audio_track() {
        let mut p = Project::new("Test");
        p.add_audio_track();
        assert_eq!(p.audio_tracks().count(), 2);
        assert!(p.dirty);
    }

    #[test]
    fn test_track_is_active_for_output_without_solo() {
        let mut p = Project::new("Test");
        let v = p.video_tracks().next().unwrap().id.clone();
        let t = p.track_mut(&v).unwrap();
        t.muted = false;
        assert!(p.track_is_active_for_output(p.tracks.iter().find(|tr| tr.id == v).unwrap()));
    }

    #[test]
    fn test_track_is_active_for_output_with_solo() {
        let mut p = Project::new("Test");
        p.add_video_track();
        let ids: Vec<String> = p.video_tracks().map(|t| t.id.clone()).collect();
        p.track_mut(&ids[0]).unwrap().soloed = true;
        p.track_mut(&ids[1]).unwrap().soloed = false;
        let left = p.tracks.iter().find(|t| t.id == ids[0]).unwrap();
        let right = p.tracks.iter().find(|t| t.id == ids[1]).unwrap();
        assert!(p.track_is_active_for_output(left));
        assert!(!p.track_is_active_for_output(right));
    }

    #[test]
    fn test_project_duration_empty() {
        let p = Project::new("Test");
        assert_eq!(p.duration(), 0);
    }

    #[test]
    fn test_project_duration_with_clips() {
        let mut p = Project::new("Test");
        let mut clip = Clip::new("file.mp4", 10_000_000_000, 0, ClipKind::Video);
        clip.id = "c1".to_string();
        p.tracks[0].add_clip(clip);
        assert_eq!(p.duration(), 10_000_000_000);
    }

    #[test]
    fn test_add_marker_sorted() {
        let mut p = Project::new("Test");
        let _id1 = p.add_marker(5_000_000_000, "Late");
        let _id2 = p.add_marker(1_000_000_000, "Early");
        assert_eq!(p.markers[0].position_ns, 1_000_000_000);
        assert_eq!(p.markers[1].position_ns, 5_000_000_000);
    }

    #[test]
    fn test_remove_marker() {
        let mut p = Project::new("Test");
        let id = p.add_marker(1_000_000_000, "Mark");
        assert_eq!(p.markers.len(), 1);
        p.remove_marker(&id);
        assert!(p.markers.is_empty());
        assert!(p.dirty);
    }

    #[test]
    fn test_marker_default_color() {
        let m = Marker::new(0, "test");
        assert_eq!(m.color, 0xFF8C00FF);
    }

    #[test]
    fn test_track_mut_found() {
        let mut p = Project::new("Test");
        let id = p.tracks[0].id.clone();
        assert!(p.track_mut(&id).is_some());
    }

    #[test]
    fn test_track_mut_not_found() {
        let mut p = Project::new("Test");
        assert!(p.track_mut("nonexistent-id").is_none());
    }

    #[test]
    fn test_project_new_has_no_source_fcpxml() {
        let p = Project::new("Test");
        assert!(p.source_fcpxml.is_none());
    }
}
