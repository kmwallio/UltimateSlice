use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Type of media a clip contains
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum ClipKind {
    Video,
    Audio,
    Image,
}

/// A single clip placed on the timeline
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Clip {
    /// Unique identifier
    pub id: String,
    /// Filesystem path to the source media file
    pub source_path: String,
    /// In-point in the source file, in nanoseconds (GstClockTime)
    pub source_in: u64,
    /// Out-point in the source file, in nanoseconds
    pub source_out: u64,
    /// Position on the timeline, in nanoseconds
    pub timeline_start: u64,
    /// Human-readable label (defaults to filename)
    pub label: String,
    pub kind: ClipKind,
}

impl Clip {
    pub fn new(source_path: impl Into<String>, source_out: u64, timeline_start: u64, kind: ClipKind) -> Self {
        let source_path = source_path.into();
        let label = std::path::Path::new(&source_path)
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("clip")
            .to_string();
        Self {
            id: Uuid::new_v4().to_string(),
            source_path,
            source_in: 0,
            source_out,
            timeline_start,
            label,
            kind,
        }
    }

    /// Duration of the clip on the timeline, in nanoseconds
    pub fn duration(&self) -> u64 {
        self.source_out.saturating_sub(self.source_in)
    }

    /// Exclusive end position on the timeline, in nanoseconds
    pub fn timeline_end(&self) -> u64 {
        self.timeline_start + self.duration()
    }
}
