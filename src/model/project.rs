use serde::{Deserialize, Serialize};
use super::track::{Track, TrackKind};
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

fn default_marker_color() -> u32 { 0xFF8C00FF }

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
    pub fn fps_24() -> Self { Self { numerator: 24, denominator: 1 } }
    pub fn fps_30() -> Self { Self { numerator: 30000, denominator: 1001 } }
    pub fn fps_60() -> Self { Self { numerator: 60, denominator: 1 } }

    pub fn as_f64(&self) -> f64 {
        self.numerator as f64 / self.denominator as f64
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

    /// Total sequence duration across all tracks, in nanoseconds
    pub fn duration(&self) -> u64 {
        self.tracks.iter().map(|t| t.duration()).max().unwrap_or(0)
    }

    pub fn add_video_track(&mut self) {
        let n = self.video_tracks().count() + 1;
        self.tracks.push(Track::new_video(format!("Video {n}")));
        self.dirty = true;
    }

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
