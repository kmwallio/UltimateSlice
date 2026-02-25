use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Type of media a clip contains
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum ClipKind {
    Video,
    Audio,
    Image,
}

fn default_contrast() -> f32 { 1.0 }
fn default_saturation() -> f32 { 1.0 }
fn default_volume() -> f32 { 1.0 }
fn default_title_font() -> String { "Sans Bold 36".to_string() }
fn default_title_color() -> u32 { 0xFFFFFFFF }
fn default_title_x() -> f64 { 0.5 }
fn default_title_y() -> f64 { 0.9 }

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
    /// Brightness adjustment: -1.0 (darkest) to 1.0 (brightest), default 0.0
    #[serde(default)]
    pub brightness: f32,
    /// Contrast multiplier: 0.0 to 2.0, default 1.0
    #[serde(default = "default_contrast")]
    pub contrast: f32,
    /// Saturation multiplier: 0.0 (greyscale) to 2.0 (vivid), default 1.0
    #[serde(default = "default_saturation")]
    pub saturation: f32,
    /// Denoise strength: 0.0 (off) to 1.0 (heavy), default 0.0
    #[serde(default)]
    pub denoise: f32,
    /// Sharpness: -1.0 (soften) to 1.0 (sharpen), default 0.0
    #[serde(default)]
    pub sharpness: f32,
    /// Audio volume multiplier: 0.0 (silent) to 2.0 (double), default 1.0
    #[serde(default = "default_volume")]
    pub volume: f32,
    /// Audio pan: -1.0 (full left) to 1.0 (full right), default 0.0
    #[serde(default)]
    pub pan: f32,
    #[serde(default)]
    pub crop_left: i32,
    #[serde(default)]
    pub crop_right: i32,
    #[serde(default)]
    pub crop_top: i32,
    #[serde(default)]
    pub crop_bottom: i32,
    /// Rotation in degrees: 0, 90, 180, or 270
    #[serde(default)]
    pub rotate: i32,
    #[serde(default)]
    pub flip_h: bool,
    #[serde(default)]
    pub flip_v: bool,
    // Title / text overlay
    #[serde(default)]
    pub title_text: String,
    #[serde(default = "default_title_font")]
    pub title_font: String,
    #[serde(default = "default_title_color")]
    pub title_color: u32,   // 0xRRGGBBAA
    #[serde(default = "default_title_x")]
    pub title_x: f64,       // 0.0–1.0 relative horizontal position
    #[serde(default = "default_title_y")]
    pub title_y: f64,       // 0.0–1.0 relative vertical position
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
            brightness: 0.0,
            contrast: 1.0,
            saturation: 1.0,
            denoise: 0.0,
            sharpness: 0.0,
            volume: 1.0,
            pan: 0.0,
            crop_left: 0,
            crop_right: 0,
            crop_top: 0,
            crop_bottom: 0,
            rotate: 0,
            flip_h: false,
            flip_v: false,
            title_text: String::new(),
            title_font: default_title_font(),
            title_color: default_title_color(),
            title_x: default_title_x(),
            title_y: default_title_y(),
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
