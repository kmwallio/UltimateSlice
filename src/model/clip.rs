use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Type of media a clip contains
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum ClipKind {
    Video,
    Audio,
    Image,
}

fn default_contrast() -> f32 {
    1.0
}
fn default_saturation() -> f32 {
    1.0
}
fn default_volume() -> f32 {
    1.0
}
fn default_speed() -> f64 {
    1.0
}
fn default_scale() -> f64 {
    1.0
}
fn default_opacity() -> f64 {
    1.0
}
fn default_title_font() -> String {
    "Sans Bold 36".to_string()
}
fn default_title_color() -> u32 {
    0xFFFFFFFF
}
fn default_title_x() -> f64 {
    0.5
}
fn default_title_y() -> f64 {
    0.9
}

/// A single clip placed on the timeline
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
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
    /// Playback speed multiplier: 0.25 (slow) to 4.0 (fast), default 1.0.
    /// Values > 1.0 speed up (clip takes less time on timeline); < 1.0 slow down.
    #[serde(default = "default_speed")]
    pub speed: f64,
    #[serde(default)]
    pub crop_left: i32,
    #[serde(default)]
    pub crop_right: i32,
    #[serde(default)]
    pub crop_top: i32,
    #[serde(default)]
    pub crop_bottom: i32,
    /// Rotation in degrees (arbitrary angle, normalized by consumers as needed).
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
    pub title_color: u32, // 0xRRGGBBAA
    #[serde(default = "default_title_x")]
    pub title_x: f64, // 0.0–1.0 relative horizontal position
    #[serde(default = "default_title_y")]
    pub title_y: f64, // 0.0–1.0 relative vertical position
    /// Transition to the next clip on the same track (e.g. "cross_dissolve").
    #[serde(default)]
    pub transition_after: String,
    /// Transition duration in nanoseconds for `transition_after` (0 = none).
    #[serde(default)]
    pub transition_after_ns: u64,
    /// Absolute path to a .cube LUT file for color grading (applied on export via ffmpeg lut3d).
    /// None means no LUT is assigned.
    #[serde(default)]
    pub lut_path: Option<String>,
    /// Scale multiplier for the clip within the frame: 1.0 = fill frame, 2.0 = zoom in 2×,
    /// 0.5 = half-size with black borders. Range 0.1–4.0, default 1.0.
    #[serde(default = "default_scale")]
    pub scale: f64,
    /// Clip opacity for compositing: 0.0 = fully transparent, 1.0 = fully opaque.
    #[serde(default = "default_opacity")]
    pub opacity: f64,
    /// Horizontal position offset: −1.0 (clip anchored to left edge) to 1.0 (right edge).
    /// Meaningful when scale ≠ 1.0. Default 0.0 (centered).
    #[serde(default)]
    pub position_x: f64,
    /// Vertical position offset: −1.0 (top edge) to 1.0 (bottom edge). Default 0.0 (centered).
    #[serde(default)]
    pub position_y: f64,
    /// Shadow grading: −1.0 (crush shadows) to 1.0 (lift shadows). Default 0.0.
    #[serde(default)]
    pub shadows: f32,
    /// Midtone grading: −1.0 (darken midtones) to 1.0 (brighten midtones). Default 0.0.
    #[serde(default)]
    pub midtones: f32,
    /// Highlight grading: −1.0 (pull down highlights) to 1.0 (boost highlights). Default 0.0.
    #[serde(default)]
    pub highlights: f32,
    /// Play the clip in reverse (backwards). Default false.
    /// Applied as `reverse`/`areverse` filters on export; preview shows reversed playback
    /// indicator on the timeline clip.
    #[serde(default)]
    pub reverse: bool,
    /// Optional clip-group identifier. Clips with the same group id are edited as a unit.
    #[serde(default)]
    pub group_id: Option<String>,
    /// Unsupported FCPXML asset-clip attributes preserved for round-trip export.
    #[serde(default)]
    pub fcpxml_unknown_attrs: Vec<(String, String)>,
    /// Unsupported FCPXML child tags under asset-clip preserved for round-trip export.
    #[serde(default)]
    pub fcpxml_unknown_children: Vec<String>,
    /// Original imported FCPXML source path (before any runtime remapping).
    #[serde(default)]
    pub fcpxml_original_source_path: Option<String>,
    /// Original imported FCPXML asset ref id from `asset-clip@ref`.
    #[serde(default)]
    pub fcpxml_asset_ref: Option<String>,
    /// Unsupported FCPXML asset attributes preserved for round-trip export.
    #[serde(default)]
    pub fcpxml_unknown_asset_attrs: Vec<(String, String)>,
    /// Unsupported FCPXML child tags under asset preserved for round-trip export.
    #[serde(default)]
    pub fcpxml_unknown_asset_children: Vec<String>,
}

impl Clip {
    pub fn new(
        source_path: impl Into<String>,
        source_out: u64,
        timeline_start: u64,
        kind: ClipKind,
    ) -> Self {
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
            speed: 1.0,
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
            transition_after: String::new(),
            transition_after_ns: 0,
            lut_path: None,
            scale: 1.0,
            opacity: 1.0,
            position_x: 0.0,
            position_y: 0.0,
            shadows: 0.0,
            midtones: 0.0,
            highlights: 0.0,
            reverse: false,
            group_id: None,
            fcpxml_unknown_attrs: Vec::new(),
            fcpxml_unknown_children: Vec::new(),
            fcpxml_original_source_path: None,
            fcpxml_asset_ref: None,
            fcpxml_unknown_asset_attrs: Vec::new(),
            fcpxml_unknown_asset_children: Vec::new(),
        }
    }

    /// Raw source material duration (source_out − source_in), unaffected by speed.
    pub fn source_duration(&self) -> u64 {
        self.source_out.saturating_sub(self.source_in)
    }

    /// Duration of the clip on the **timeline**, in nanoseconds.
    /// A 2× speed clip occupies half the wall-clock time; 0.5× occupies double.
    pub fn duration(&self) -> u64 {
        let src = self.source_duration();
        if self.speed > 0.0 {
            (src as f64 / self.speed) as u64
        } else {
            src
        }
    }

    /// Exclusive end position on the timeline, in nanoseconds
    pub fn timeline_end(&self) -> u64 {
        self.timeline_start + self.duration()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_test_clip(source_out: u64, timeline_start: u64) -> Clip {
        Clip::new(
            "/path/to/video.mp4",
            source_out,
            timeline_start,
            ClipKind::Video,
        )
    }

    #[test]
    fn test_clip_new_defaults() {
        let clip = make_test_clip(10_000_000_000, 0);
        assert_eq!(clip.source_in, 0);
        assert_eq!(clip.source_out, 10_000_000_000);
        assert_eq!(clip.timeline_start, 0);
        assert_eq!(clip.brightness, 0.0);
        assert_eq!(clip.contrast, 1.0);
        assert_eq!(clip.saturation, 1.0);
        assert_eq!(clip.volume, 1.0);
        assert_eq!(clip.speed, 1.0);
        assert!(!clip.reverse);
        assert_eq!(clip.scale, 1.0);
        assert_eq!(clip.opacity, 1.0);
        assert!(!clip.flip_h);
        assert!(!clip.flip_v);
        assert!(clip.lut_path.is_none());
        assert!(clip.group_id.is_none());
        assert!(clip.transition_after.is_empty());
        assert!(clip.fcpxml_unknown_attrs.is_empty());
        assert!(clip.fcpxml_unknown_children.is_empty());
        assert!(clip.fcpxml_original_source_path.is_none());
    }

    #[test]
    fn test_clip_label_from_filename() {
        let clip = make_test_clip(5_000_000_000, 0);
        assert_eq!(clip.label, "video");
    }

    #[test]
    fn test_clip_label_fallback() {
        let clip = Clip::new("/", 5_000_000_000, 0, ClipKind::Audio);
        assert_eq!(clip.label, "clip");
    }

    #[test]
    fn test_source_duration() {
        let mut clip = make_test_clip(10_000_000_000, 0);
        clip.source_in = 2_000_000_000;
        assert_eq!(clip.source_duration(), 8_000_000_000);
    }

    #[test]
    fn test_source_duration_zero_when_empty() {
        let mut clip = make_test_clip(5_000_000_000, 0);
        clip.source_in = 5_000_000_000;
        assert_eq!(clip.source_duration(), 0);
    }

    #[test]
    fn test_duration_normal_speed() {
        let clip = make_test_clip(10_000_000_000, 0);
        assert_eq!(clip.duration(), 10_000_000_000);
    }

    #[test]
    fn test_duration_double_speed() {
        let mut clip = make_test_clip(10_000_000_000, 0);
        clip.speed = 2.0;
        assert_eq!(clip.duration(), 5_000_000_000);
    }

    #[test]
    fn test_duration_half_speed() {
        let mut clip = make_test_clip(10_000_000_000, 0);
        clip.speed = 0.5;
        assert_eq!(clip.duration(), 20_000_000_000);
    }

    #[test]
    fn test_timeline_end() {
        let clip = make_test_clip(5_000_000_000, 3_000_000_000);
        assert_eq!(clip.timeline_end(), 8_000_000_000);
    }

    #[test]
    fn test_clip_kind_variants() {
        let video = Clip::new("a.mp4", 1, 0, ClipKind::Video);
        let audio = Clip::new("b.mp3", 1, 0, ClipKind::Audio);
        let image = Clip::new("c.png", 1, 0, ClipKind::Image);
        assert_eq!(video.kind, ClipKind::Video);
        assert_eq!(audio.kind, ClipKind::Audio);
        assert_eq!(image.kind, ClipKind::Image);
    }

    #[test]
    fn test_reverse_default_false() {
        let clip = make_test_clip(10_000_000_000, 0);
        assert!(!clip.reverse);
    }

    #[test]
    fn test_reverse_does_not_affect_duration() {
        let mut clip = make_test_clip(10_000_000_000, 0);
        clip.speed = 2.0;
        clip.reverse = true;
        // Reverse does not change the timeline duration — only playback direction on export
        assert_eq!(clip.duration(), 5_000_000_000);
    }
}
