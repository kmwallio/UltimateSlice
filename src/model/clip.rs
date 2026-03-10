use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Type of media a clip contains
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum ClipKind {
    Video,
    Audio,
    Image,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClipColorLabel {
    None,
    Red,
    Orange,
    Yellow,
    Green,
    Teal,
    Blue,
    Purple,
    Magenta,
}

impl Default for ClipColorLabel {
    fn default() -> Self {
        Self::None
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum KeyframeInterpolation {
    #[default]
    Linear,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NumericKeyframe {
    pub time_ns: u64,
    pub value: f64,
    #[serde(default)]
    pub interpolation: KeyframeInterpolation,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Phase1KeyframeProperty {
    PositionX,
    PositionY,
    Scale,
    Opacity,
    Volume,
}

impl Phase1KeyframeProperty {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::PositionX => "position_x",
            Self::PositionY => "position_y",
            Self::Scale => "scale",
            Self::Opacity => "opacity",
            Self::Volume => "volume",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "position_x" | "position-x" => Some(Self::PositionX),
            "position_y" | "position-y" => Some(Self::PositionY),
            "scale" => Some(Self::Scale),
            "opacity" => Some(Self::Opacity),
            "volume" => Some(Self::Volume),
            _ => None,
        }
    }
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
fn default_chroma_key_color() -> u32 {
    0x00FF00
}
fn default_chroma_key_tolerance() -> f32 {
    0.3
}
fn default_chroma_key_softness() -> f32 {
    0.1
}
fn default_bg_removal_threshold() -> f64 {
    0.5
}
fn default_temperature() -> f32 {
    6500.0
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
    /// Semantic clip color label used for timeline tinting.
    #[serde(default)]
    pub color_label: ClipColorLabel,
    /// Brightness adjustment: -1.0 (darkest) to 1.0 (brightest), default 0.0
    #[serde(default)]
    pub brightness: f32,
    /// Contrast multiplier: 0.0 to 2.0, default 1.0
    #[serde(default = "default_contrast")]
    pub contrast: f32,
    /// Saturation multiplier: 0.0 (greyscale) to 2.0 (vivid), default 1.0
    #[serde(default = "default_saturation")]
    pub saturation: f32,
    /// Color temperature in Kelvin: 2000 (warm/amber) to 10000 (cool/blue), default 6500 (daylight neutral).
    #[serde(default = "default_temperature")]
    pub temperature: f32,
    /// Tint shift on the green–magenta axis: −1.0 (green) to 1.0 (magenta), default 0.0.
    #[serde(default)]
    pub tint: f32,
    /// Denoise strength: 0.0 (off) to 1.0 (heavy), default 0.0
    #[serde(default)]
    pub denoise: f32,
    /// Sharpness: -1.0 (soften) to 1.0 (sharpen), default 0.0
    #[serde(default)]
    pub sharpness: f32,
    /// Audio volume multiplier: 0.0 (silent) to 2.0 (double), default 1.0
    #[serde(default = "default_volume")]
    pub volume: f32,
    /// Optional volume keyframes over clip-local timeline.
    #[serde(default)]
    pub volume_keyframes: Vec<NumericKeyframe>,
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
    /// Optional scale keyframes over clip-local timeline.
    #[serde(default)]
    pub scale_keyframes: Vec<NumericKeyframe>,
    /// Clip opacity for compositing: 0.0 = fully transparent, 1.0 = fully opaque.
    #[serde(default = "default_opacity")]
    pub opacity: f64,
    /// Optional opacity keyframes over clip-local timeline.
    #[serde(default)]
    pub opacity_keyframes: Vec<NumericKeyframe>,
    /// Horizontal position offset: −1.0 (clip anchored to left edge) to 1.0 (right edge).
    /// Meaningful when scale ≠ 1.0. Default 0.0 (centered).
    #[serde(default)]
    pub position_x: f64,
    /// Optional horizontal position keyframes over clip-local timeline.
    #[serde(default)]
    pub position_x_keyframes: Vec<NumericKeyframe>,
    /// Vertical position offset: −1.0 (top edge) to 1.0 (bottom edge). Default 0.0 (centered).
    #[serde(default)]
    pub position_y: f64,
    /// Optional vertical position keyframes over clip-local timeline.
    #[serde(default)]
    pub position_y_keyframes: Vec<NumericKeyframe>,
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
    /// Freeze-frame enabled flag. When enabled for video clips, one source frame is held
    /// for the timeline duration and audio is suppressed.
    #[serde(default)]
    pub freeze_frame: bool,
    /// Optional source timestamp (ns) used for freeze-frame sampling.
    /// When unset, `source_in` is used.
    #[serde(default)]
    pub freeze_frame_source_ns: Option<u64>,
    /// Optional explicit timeline hold duration (ns) for freeze-frames.
    /// When unset, normal speed-based duration semantics are used.
    #[serde(default)]
    pub freeze_frame_hold_duration_ns: Option<u64>,
    /// Chroma key enabled flag. Default false.
    #[serde(default)]
    pub chroma_key_enabled: bool,
    /// Chroma key target color as 0xRRGGBB. Default 0x00FF00 (green).
    #[serde(default = "default_chroma_key_color")]
    pub chroma_key_color: u32,
    /// Chroma key tolerance (angle): 0.0 (tight key) to 1.0 (wide key). Default 0.3.
    #[serde(default = "default_chroma_key_tolerance")]
    pub chroma_key_tolerance: f32,
    /// Chroma key edge softness (noise level): 0.0 (hard edge) to 1.0 (soft edge). Default 0.1.
    #[serde(default = "default_chroma_key_softness")]
    pub chroma_key_softness: f32,
    /// AI background removal enabled flag. Default false.
    #[serde(default)]
    pub bg_removal_enabled: bool,
    /// AI background removal matte threshold: 0.0 (aggressive) to 1.0 (conservative). Default 0.5.
    #[serde(default = "default_bg_removal_threshold")]
    pub bg_removal_threshold: f64,
    /// Optional clip-group identifier. Clips with the same group id are edited as a unit.
    #[serde(default)]
    pub group_id: Option<String>,
    /// Optional strict link-group identifier used for synchronized A/V-linked edits.
    #[serde(default)]
    pub link_group_id: Option<String>,
    /// Optional absolute source time reference for the start of the underlying media.
    /// When present, source clip start timecode is `source_timecode_base_ns + source_in`.
    #[serde(default)]
    pub source_timecode_base_ns: Option<u64>,
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
            color_label: ClipColorLabel::None,
            brightness: 0.0,
            contrast: 1.0,
            saturation: 1.0,
            temperature: 6500.0,
            tint: 0.0,
            denoise: 0.0,
            sharpness: 0.0,
            volume: 1.0,
            volume_keyframes: Vec::new(),
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
            scale_keyframes: Vec::new(),
            opacity: 1.0,
            opacity_keyframes: Vec::new(),
            position_x: 0.0,
            position_x_keyframes: Vec::new(),
            position_y: 0.0,
            position_y_keyframes: Vec::new(),
            shadows: 0.0,
            midtones: 0.0,
            highlights: 0.0,
            reverse: false,
            freeze_frame: false,
            freeze_frame_source_ns: None,
            freeze_frame_hold_duration_ns: None,
            chroma_key_enabled: false,
            chroma_key_color: default_chroma_key_color(),
            chroma_key_tolerance: default_chroma_key_tolerance(),
            chroma_key_softness: default_chroma_key_softness(),
            bg_removal_enabled: false,
            bg_removal_threshold: default_bg_removal_threshold(),
            group_id: None,
            link_group_id: None,
            source_timecode_base_ns: None,
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

    pub fn evaluate_keyframed_value(
        keyframes: &[NumericKeyframe],
        local_timeline_ns: u64,
        default_value: f64,
    ) -> f64 {
        if keyframes.is_empty() {
            return default_value;
        }
        let mut sorted: Vec<&NumericKeyframe> = keyframes.iter().collect();
        sorted.sort_by_key(|kf| kf.time_ns);
        let first = sorted[0];
        if local_timeline_ns < first.time_ns {
            return first.value;
        }
        let mut prev = first;
        for next in sorted.iter().skip(1) {
            if local_timeline_ns < next.time_ns {
                let span = next.time_ns.saturating_sub(prev.time_ns);
                if span == 0 {
                    return next.value;
                }
                let t = (local_timeline_ns.saturating_sub(prev.time_ns)) as f64 / span as f64;
                return prev.value + (next.value - prev.value) * t;
            }
            prev = next;
        }
        prev.value
    }

    pub fn local_timeline_position_ns(&self, timeline_pos_ns: u64) -> u64 {
        timeline_pos_ns
            .saturating_sub(self.timeline_start)
            .min(self.duration())
    }

    pub fn keyframes_for_phase1_property(
        &self,
        property: Phase1KeyframeProperty,
    ) -> &[NumericKeyframe] {
        match property {
            Phase1KeyframeProperty::PositionX => &self.position_x_keyframes,
            Phase1KeyframeProperty::PositionY => &self.position_y_keyframes,
            Phase1KeyframeProperty::Scale => &self.scale_keyframes,
            Phase1KeyframeProperty::Opacity => &self.opacity_keyframes,
            Phase1KeyframeProperty::Volume => &self.volume_keyframes,
        }
    }

    pub fn keyframes_for_phase1_property_mut(
        &mut self,
        property: Phase1KeyframeProperty,
    ) -> &mut Vec<NumericKeyframe> {
        match property {
            Phase1KeyframeProperty::PositionX => &mut self.position_x_keyframes,
            Phase1KeyframeProperty::PositionY => &mut self.position_y_keyframes,
            Phase1KeyframeProperty::Scale => &mut self.scale_keyframes,
            Phase1KeyframeProperty::Opacity => &mut self.opacity_keyframes,
            Phase1KeyframeProperty::Volume => &mut self.volume_keyframes,
        }
    }

    pub fn default_value_for_phase1_property(&self, property: Phase1KeyframeProperty) -> f64 {
        match property {
            Phase1KeyframeProperty::PositionX => self.position_x,
            Phase1KeyframeProperty::PositionY => self.position_y,
            Phase1KeyframeProperty::Scale => self.scale,
            Phase1KeyframeProperty::Opacity => self.opacity,
            Phase1KeyframeProperty::Volume => self.volume as f64,
        }
    }

    pub fn clamp_phase1_property_value(property: Phase1KeyframeProperty, value: f64) -> f64 {
        match property {
            Phase1KeyframeProperty::PositionX | Phase1KeyframeProperty::PositionY => {
                value.clamp(-1.0, 1.0)
            }
            Phase1KeyframeProperty::Scale => value.clamp(0.1, 4.0),
            Phase1KeyframeProperty::Opacity => value.clamp(0.0, 1.0),
            Phase1KeyframeProperty::Volume => value.clamp(0.0, 4.0),
        }
    }

    pub fn value_for_phase1_property_at_timeline_ns(
        &self,
        property: Phase1KeyframeProperty,
        timeline_pos_ns: u64,
    ) -> f64 {
        Self::evaluate_keyframed_value(
            self.keyframes_for_phase1_property(property),
            self.local_timeline_position_ns(timeline_pos_ns),
            self.default_value_for_phase1_property(property),
        )
    }

    pub fn upsert_phase1_keyframe_at_timeline_ns(
        &mut self,
        property: Phase1KeyframeProperty,
        timeline_pos_ns: u64,
        value: f64,
    ) -> u64 {
        let local_time_ns = self.local_timeline_position_ns(timeline_pos_ns);
        let clamped_value = Self::clamp_phase1_property_value(property, value);
        let keyframes = self.keyframes_for_phase1_property_mut(property);
        if let Some(existing) = keyframes.iter_mut().find(|kf| kf.time_ns == local_time_ns) {
            existing.value = clamped_value;
            existing.interpolation = KeyframeInterpolation::Linear;
        } else {
            keyframes.push(NumericKeyframe {
                time_ns: local_time_ns,
                value: clamped_value,
                interpolation: KeyframeInterpolation::Linear,
            });
            keyframes.sort_by_key(|kf| kf.time_ns);
        }
        local_time_ns
    }

    pub fn remove_phase1_keyframe_at_timeline_ns(
        &mut self,
        property: Phase1KeyframeProperty,
        timeline_pos_ns: u64,
    ) -> bool {
        let local_time_ns = self.local_timeline_position_ns(timeline_pos_ns);
        let keyframes = self.keyframes_for_phase1_property_mut(property);
        let before = keyframes.len();
        keyframes.retain(|kf| kf.time_ns != local_time_ns);
        keyframes.len() != before
    }

    pub fn source_timecode_start_ns(&self) -> Option<u64> {
        self.source_timecode_base_ns
            .map(|base| base.saturating_add(self.source_in))
    }

    pub fn suppresses_embedded_audio_for_linked_peer(&self, peer: &Clip) -> bool {
        self.kind == ClipKind::Video
            && peer.kind == ClipKind::Audio
            && self.link_group_id.is_some()
            && self.link_group_id == peer.link_group_id
            && self.source_path == peer.source_path
    }

    pub fn is_freeze_frame(&self) -> bool {
        self.freeze_frame && self.kind == ClipKind::Video
    }

    pub fn freeze_frame_source_time_ns(&self) -> Option<u64> {
        if !self.is_freeze_frame() {
            return None;
        }
        let requested = self.freeze_frame_source_ns.unwrap_or(self.source_in);
        if self.source_out > self.source_in {
            Some(requested.clamp(self.source_in, self.source_out.saturating_sub(1)))
        } else {
            Some(self.source_in)
        }
    }

    pub fn freeze_frame_duration_ns(&self) -> Option<u64> {
        if !self.is_freeze_frame() {
            return None;
        }
        let _source_time_ns = self.freeze_frame_source_time_ns()?;
        Some(
            self.freeze_frame_hold_duration_ns
                .filter(|duration| *duration > 0)
                .unwrap_or_else(|| self.playback_duration_from_speed()),
        )
    }

    fn playback_duration_from_speed(&self) -> u64 {
        let src = self.source_duration();
        if self.speed > 0.0 {
            (src as f64 / self.speed) as u64
        } else {
            src
        }
    }

    /// Duration of the clip on the **timeline**, in nanoseconds.
    /// A 2× speed clip occupies half the wall-clock time; 0.5× occupies double.
    pub fn duration(&self) -> u64 {
        self.freeze_frame_duration_ns()
            .unwrap_or_else(|| self.playback_duration_from_speed())
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
        assert_eq!(clip.color_label, ClipColorLabel::None);
        assert_eq!(clip.volume, 1.0);
        assert!(clip.volume_keyframes.is_empty());
        assert_eq!(clip.speed, 1.0);
        assert!(!clip.reverse);
        assert!(!clip.chroma_key_enabled);
        assert_eq!(clip.chroma_key_color, 0x00FF00);
        assert!((clip.chroma_key_tolerance - 0.3).abs() < f32::EPSILON);
        assert!((clip.chroma_key_softness - 0.1).abs() < f32::EPSILON);
        assert_eq!(clip.scale, 1.0);
        assert!(clip.scale_keyframes.is_empty());
        assert_eq!(clip.opacity, 1.0);
        assert!(clip.opacity_keyframes.is_empty());
        assert!(clip.position_x_keyframes.is_empty());
        assert!(clip.position_y_keyframes.is_empty());
        assert!(!clip.flip_h);
        assert!(!clip.flip_v);
        assert!(!clip.freeze_frame);
        assert!(clip.freeze_frame_source_ns.is_none());
        assert!(clip.freeze_frame_hold_duration_ns.is_none());
        assert!(clip.lut_path.is_none());
        assert!(clip.group_id.is_none());
        assert!(clip.link_group_id.is_none());
        assert!(clip.source_timecode_base_ns.is_none());
        assert!(clip.transition_after.is_empty());
        assert!(clip.fcpxml_unknown_attrs.is_empty());
        assert!(clip.fcpxml_unknown_children.is_empty());
        assert!(clip.fcpxml_original_source_path.is_none());
    }

    #[test]
    fn test_source_timecode_start_uses_base_and_source_in() {
        let mut clip = make_test_clip(5_000_000_000, 0);
        clip.source_in = 2_000_000_000;
        clip.source_timecode_base_ns = Some(10_000_000_000);
        assert_eq!(clip.source_timecode_start_ns(), Some(12_000_000_000));
    }

    #[test]
    fn test_linked_audio_peer_suppresses_embedded_audio() {
        let mut video = make_test_clip(5_000_000_000, 0);
        video.link_group_id = Some("link-1".to_string());

        let mut audio = Clip::new("/path/to/video.mp4", 5_000_000_000, 0, ClipKind::Audio);
        audio.link_group_id = Some("link-1".to_string());

        assert!(video.suppresses_embedded_audio_for_linked_peer(&audio));
    }

    #[test]
    fn test_unlinked_or_different_source_peer_does_not_suppress_embedded_audio() {
        let mut video = make_test_clip(5_000_000_000, 0);
        video.link_group_id = Some("link-1".to_string());

        let mut audio = Clip::new("/path/to/other.wav", 5_000_000_000, 0, ClipKind::Audio);
        audio.link_group_id = Some("link-1".to_string());

        assert!(!video.suppresses_embedded_audio_for_linked_peer(&audio));
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

    #[test]
    fn test_freeze_frame_requires_video_clip_kind() {
        let mut clip = Clip::new("a.wav", 10, 0, ClipKind::Audio);
        clip.freeze_frame = true;
        clip.freeze_frame_source_ns = Some(4);
        clip.freeze_frame_hold_duration_ns = Some(7);
        assert!(!clip.is_freeze_frame());
        assert_eq!(clip.freeze_frame_source_time_ns(), None);
        assert_eq!(clip.freeze_frame_duration_ns(), None);
    }

    #[test]
    fn test_freeze_frame_source_defaults_to_source_in() {
        let mut clip = make_test_clip(10_000_000_000, 0);
        clip.source_in = 2_000_000_000;
        clip.freeze_frame = true;
        assert_eq!(clip.freeze_frame_source_time_ns(), Some(2_000_000_000));
    }

    #[test]
    fn test_freeze_frame_source_is_clamped_to_source_bounds() {
        let mut clip = make_test_clip(10_000_000_000, 0);
        clip.source_in = 2_000_000_000;
        clip.source_out = 4_000_000_000;
        clip.freeze_frame = true;

        clip.freeze_frame_source_ns = Some(1_000_000_000);
        assert_eq!(clip.freeze_frame_source_time_ns(), Some(2_000_000_000));

        clip.freeze_frame_source_ns = Some(9_000_000_000);
        assert_eq!(clip.freeze_frame_source_time_ns(), Some(3_999_999_999));
    }

    #[test]
    fn test_freeze_frame_duration_uses_explicit_hold_duration() {
        let mut clip = make_test_clip(10_000_000_000, 0);
        clip.speed = 4.0;
        clip.freeze_frame = true;
        clip.freeze_frame_hold_duration_ns = Some(3_000_000_000);
        assert_eq!(clip.freeze_frame_duration_ns(), Some(3_000_000_000));
        assert_eq!(clip.duration(), 3_000_000_000);
    }

    #[test]
    fn test_freeze_frame_duration_defaults_to_speed_based_duration_when_unset() {
        let mut clip = make_test_clip(10_000_000_000, 0);
        clip.speed = 2.0;
        clip.freeze_frame = true;
        assert_eq!(clip.freeze_frame_duration_ns(), Some(5_000_000_000));
        assert_eq!(clip.duration(), 5_000_000_000);
    }

    #[test]
    fn test_evaluate_keyframed_value_linear() {
        let keyframes = vec![
            NumericKeyframe {
                time_ns: 0,
                value: 0.0,
                interpolation: KeyframeInterpolation::Linear,
            },
            NumericKeyframe {
                time_ns: 1_000_000_000,
                value: 1.0,
                interpolation: KeyframeInterpolation::Linear,
            },
        ];
        let v = Clip::evaluate_keyframed_value(&keyframes, 500_000_000, 9.0);
        assert!((v - 0.5).abs() < 1e-9);
    }

    #[test]
    fn test_evaluate_keyframed_value_clamps_before_after_range() {
        let keyframes = vec![
            NumericKeyframe {
                time_ns: 100,
                value: 2.0,
                interpolation: KeyframeInterpolation::Linear,
            },
            NumericKeyframe {
                time_ns: 300,
                value: 6.0,
                interpolation: KeyframeInterpolation::Linear,
            },
        ];
        assert!((Clip::evaluate_keyframed_value(&keyframes, 50, 9.0) - 2.0).abs() < 1e-9);
        assert!((Clip::evaluate_keyframed_value(&keyframes, 500, 9.0) - 6.0).abs() < 1e-9);
    }

    #[test]
    fn test_evaluate_keyframed_value_duplicate_time_last_wins() {
        let keyframes = vec![
            NumericKeyframe {
                time_ns: 0,
                value: 1.0,
                interpolation: KeyframeInterpolation::Linear,
            },
            NumericKeyframe {
                time_ns: 0,
                value: 3.0,
                interpolation: KeyframeInterpolation::Linear,
            },
            NumericKeyframe {
                time_ns: 10,
                value: 5.0,
                interpolation: KeyframeInterpolation::Linear,
            },
        ];
        assert!((Clip::evaluate_keyframed_value(&keyframes, 0, 0.0) - 3.0).abs() < 1e-9);
    }

    #[test]
    fn test_keyframed_accessors_use_clip_timeline_space() {
        let mut clip = make_test_clip(10_000_000_000, 2_000_000_000);
        clip.position_x_keyframes = vec![
            NumericKeyframe {
                time_ns: 0,
                value: -1.0,
                interpolation: KeyframeInterpolation::Linear,
            },
            NumericKeyframe {
                time_ns: 1_000_000_000,
                value: 1.0,
                interpolation: KeyframeInterpolation::Linear,
            },
        ];
        let local_ns = 2_500_000_000_u64.saturating_sub(clip.timeline_start);
        let v =
            Clip::evaluate_keyframed_value(&clip.position_x_keyframes, local_ns, clip.position_x);
        assert!((v - 0.0).abs() < 1e-9);
    }

    #[test]
    fn test_upsert_phase1_keyframe_overwrites_at_same_time() {
        let mut clip = make_test_clip(10_000_000_000, 0);
        clip.upsert_phase1_keyframe_at_timeline_ns(Phase1KeyframeProperty::Opacity, 2, 0.25);
        clip.upsert_phase1_keyframe_at_timeline_ns(Phase1KeyframeProperty::Opacity, 2, 0.8);
        assert_eq!(clip.opacity_keyframes.len(), 1);
        assert!((clip.opacity_keyframes[0].value - 0.8).abs() < 1e-9);
    }

    #[test]
    fn test_remove_phase1_keyframe_at_timeline_time() {
        let mut clip = make_test_clip(10_000_000_000, 1_000);
        clip.upsert_phase1_keyframe_at_timeline_ns(Phase1KeyframeProperty::Scale, 1_250, 2.0);
        assert!(clip.remove_phase1_keyframe_at_timeline_ns(Phase1KeyframeProperty::Scale, 1_250));
        assert!(clip.scale_keyframes.is_empty());
    }

    #[test]
    fn test_clip_deserialize_backwards_compatible_freeze_frame_defaults() {
        let json = r#"{
            "id":"clip-1",
            "source_path":"/tmp/source.mp4",
            "source_in":0,
            "source_out":100,
            "timeline_start":0,
            "label":"source",
            "kind":"Video"
        }"#;
        let clip: Clip = serde_json::from_str(json).expect("clip should deserialize");
        assert!(!clip.freeze_frame);
        assert_eq!(clip.freeze_frame_source_ns, None);
        assert_eq!(clip.freeze_frame_hold_duration_ns, None);
        assert_eq!(clip.color_label, ClipColorLabel::None);
        assert!(clip.scale_keyframes.is_empty());
        assert!(clip.opacity_keyframes.is_empty());
        assert!(clip.position_x_keyframes.is_empty());
        assert!(clip.position_y_keyframes.is_empty());
        assert!(clip.volume_keyframes.is_empty());
    }
}
