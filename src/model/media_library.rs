use uuid::Uuid;

/// A media item in the project library — not yet placed on the timeline.
#[derive(Debug, Clone)]
pub struct MediaItem {
    #[allow(dead_code)]
    pub id: String,
    pub source_path: String,
    pub duration_ns: u64,
    pub label: String,
    /// True when the file has no video streams (audio-only).
    /// Set asynchronously after background probe completes.
    pub is_audio_only: bool,
    /// True when the file has at least one audio stream.
    /// Set asynchronously after background probe completes.
    pub has_audio: bool,
    /// True when the file is a still image (PNG, JPEG, etc.).
    /// Set asynchronously after background probe completes.
    pub is_image: bool,
    /// Optional absolute source time reference for the start of the media.
    pub source_timecode_base_ns: Option<u64>,
    /// True when the source file path cannot be resolved on disk.
    pub is_missing: bool,
}

impl MediaItem {
    pub fn new(source_path: impl Into<String>, duration_ns: u64) -> Self {
        let source_path = source_path.into();
        let is_missing = !source_path_exists(&source_path);
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
            is_audio_only: false,
            has_audio: false,
            is_image: false,
            source_timecode_base_ns: None,
            is_missing,
        }
    }
}

pub fn source_path_exists(source_path: &str) -> bool {
    std::fs::metadata(source_path).is_ok()
}

/// In/out marks and current source for the source preview monitor.
#[derive(Debug, Clone)]
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
    /// Frame duration in nanoseconds (default 24 fps ≈ 41_666_667 ns).
    /// Used for frame-accurate jog/shuttle stepping.
    pub frame_ns: u64,
    /// True when the loaded source has no video streams (audio file).
    pub is_audio_only: bool,
    /// True when the loaded source has at least one audio stream.
    pub has_audio: bool,
    /// True when the loaded source is a still image.
    pub is_image: bool,
    /// Optional absolute source time reference for the start of the loaded media.
    pub source_timecode_base_ns: Option<u64>,
}

impl Default for SourceMarks {
    fn default() -> Self {
        Self {
            path: String::new(),
            duration_ns: 0,
            in_ns: 0,
            out_ns: 0,
            display_pos_ns: 0,
            frame_ns: 41_666_667, // 24 fps default
            is_audio_only: false,
            has_audio: false,
            is_image: false,
            source_timecode_base_ns: None,
        }
    }
}
