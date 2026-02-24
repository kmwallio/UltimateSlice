use uuid::Uuid;

/// A media item in the project library — not yet placed on the timeline.
#[derive(Debug, Clone)]
pub struct MediaItem {
    pub id: String,
    pub source_path: String,
    pub duration_ns: u64,
    pub label: String,
}

impl MediaItem {
    pub fn new(source_path: impl Into<String>, duration_ns: u64) -> Self {
        let source_path = source_path.into();
        let label = std::path::Path::new(&source_path)
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("media")
            .to_string();
        Self {
            id: Uuid::new_v4().to_string(),
            source_path,
            duration_ns,
            label,
        }
    }
}

/// In/out marks and current source for the source preview monitor.
#[derive(Debug, Default)]
pub struct SourceMarks {
    /// Filesystem path of the currently loaded source clip
    pub path: String,
    /// Total duration of the loaded source
    pub duration_ns: u64,
    /// In-point (start of selection), in nanoseconds
    pub in_ns: u64,
    /// Out-point (end of selection), in nanoseconds
    pub out_ns: u64,
    /// Last position explicitly seeked to — used as display position while
    /// GStreamer is still pre-rolling (query_position returns 0 during that time).
    pub display_pos_ns: u64,
}
