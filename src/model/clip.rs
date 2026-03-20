use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use uuid::Uuid;

/// Common image file extensions (lowercase).
const IMAGE_EXTENSIONS: &[&str] = &[
    "png", "jpg", "jpeg", "gif", "bmp", "tiff", "tif", "webp", "svg", "heic", "heif",
];

/// Returns `true` when `path` has a file extension matching a known still-image
/// format.  Used across import, placement, playback, and export to distinguish
/// image clips from video/audio.
pub fn is_image_file(path: &str) -> bool {
    let lower = path.to_ascii_lowercase();
    IMAGE_EXTENSIONS
        .iter()
        .any(|ext| lower.ends_with(&format!(".{ext}")))
}

/// Type of media a clip contains
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum ClipKind {
    Video,
    Audio,
    Image,
    Title,
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

/// Slow-motion frame interpolation mode (export-only).
#[derive(Default, Clone, Copy, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SlowMotionInterp {
    #[default]
    Off,
    /// Temporal frame blending (minterpolate mi_mode=blend). Fast.
    Blend,
    /// Motion-compensated interpolation (minterpolate mi_mode=mci). Slow but smooth.
    OpticalFlow,
}

/// Compositing blend mode for a clip.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum BlendMode {
    #[default]
    Normal,
    Multiply,
    Screen,
    Overlay,
    Add,
    Difference,
    SoftLight,
}

impl BlendMode {
    pub const ALL: &'static [BlendMode] = &[
        BlendMode::Normal,
        BlendMode::Multiply,
        BlendMode::Screen,
        BlendMode::Overlay,
        BlendMode::Add,
        BlendMode::Difference,
        BlendMode::SoftLight,
    ];

    pub fn label(&self) -> &'static str {
        match self {
            BlendMode::Normal => "Normal",
            BlendMode::Multiply => "Multiply",
            BlendMode::Screen => "Screen",
            BlendMode::Overlay => "Overlay",
            BlendMode::Add => "Add",
            BlendMode::Difference => "Difference",
            BlendMode::SoftLight => "Soft Light",
        }
    }

    pub fn ffmpeg_mode(&self) -> &'static str {
        match self {
            BlendMode::Normal => "normal",
            BlendMode::Multiply => "multiply",
            BlendMode::Screen => "screen",
            BlendMode::Overlay => "overlay",
            BlendMode::Add => "addition",
            BlendMode::Difference => "difference",
            BlendMode::SoftLight => "softlight",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum KeyframeInterpolation {
    #[default]
    Linear,
    EaseIn,
    EaseOut,
    EaseInOut,
}

impl KeyframeInterpolation {
    /// Human-readable label for UI dropdowns.
    pub fn label(self) -> &'static str {
        match self {
            Self::Linear => "Linear",
            Self::EaseIn => "Ease In",
            Self::EaseOut => "Ease Out",
            Self::EaseInOut => "Ease In/Out",
        }
    }

    /// All variants in display order.
    pub const ALL: [KeyframeInterpolation; 4] =
        [Self::Linear, Self::EaseIn, Self::EaseOut, Self::EaseInOut];

    /// Parse from FCPXML `interp` attribute value.
    pub fn from_fcpxml(s: &str) -> Self {
        match s {
            "easeIn" => Self::EaseIn,
            "easeOut" => Self::EaseOut,
            "ease" => Self::EaseInOut,
            _ => Self::Linear,
        }
    }

    /// Emit as FCPXML `interp` attribute value.
    pub fn to_fcpxml(self) -> &'static str {
        match self {
            Self::Linear => "linear",
            Self::EaseIn => "easeIn",
            Self::EaseOut => "easeOut",
            Self::EaseInOut => "ease",
        }
    }

    /// Map a linear `t` (0..1) through this interpolation's easing curve.
    pub fn ease(self, t: f64) -> f64 {
        match self {
            Self::Linear => t,
            Self::EaseIn => cubic_bezier_ease(0.42, 0.0, 1.0, 1.0, t),
            Self::EaseOut => cubic_bezier_ease(0.0, 0.0, 0.58, 1.0, t),
            Self::EaseInOut => cubic_bezier_ease(0.42, 0.0, 0.58, 1.0, t),
        }
    }

    pub fn control_points(self) -> (f64, f64, f64, f64) {
        match self {
            Self::Linear => (1.0 / 3.0, 1.0 / 3.0, 2.0 / 3.0, 2.0 / 3.0),
            Self::EaseIn => (0.42, 0.0, 1.0, 1.0),
            Self::EaseOut => (0.0, 0.0, 0.58, 1.0),
            Self::EaseInOut => (0.42, 0.0, 0.58, 1.0),
        }
    }
}

/// Evaluate a cubic bezier curve with control points (x1,y1) and (x2,y2)
/// at the given linear time `t` (0..1). Returns the eased output value.
///
/// Uses Newton-Raphson iteration to invert the X(s) → t mapping, then
/// evaluates Y(s) for the eased value. This is the standard algorithm
/// used by CSS transitions / web animation engines.
fn cubic_bezier_ease(x1: f64, y1: f64, x2: f64, y2: f64, t: f64) -> f64 {
    if t <= 0.0 {
        return 0.0;
    }
    if t >= 1.0 {
        return 1.0;
    }

    // Find s such that bezier_x(s) = t using Newton-Raphson.
    let mut s = t; // initial guess
    for _ in 0..8 {
        let bx = bezier_component(x1, x2, s);
        let dx = bezier_component_derivative(x1, x2, s);
        if dx.abs() < 1e-12 {
            break;
        }
        s -= (bx - t) / dx;
        s = s.clamp(0.0, 1.0);
    }
    bezier_component(y1, y2, s)
}

/// Evaluate one component of a cubic bezier: B(s) = 3(1-s)²·s·p1 + 3(1-s)·s²·p2 + s³
#[inline]
fn bezier_component(p1: f64, p2: f64, s: f64) -> f64 {
    let s2 = s * s;
    let s3 = s2 * s;
    let inv = 1.0 - s;
    3.0 * inv * inv * s * p1 + 3.0 * inv * s2 * p2 + s3
}

/// Derivative of bezier_component w.r.t. s.
#[inline]
fn bezier_component_derivative(p1: f64, p2: f64, s: f64) -> f64 {
    let s2 = s * s;
    3.0 * (1.0 - s) * (1.0 - s) * p1 + 6.0 * (1.0 - s) * s * (p2 - p1) + 3.0 * s2 * (1.0 - p2)
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct BezierControls {
    pub x1: f64,
    pub y1: f64,
    pub x2: f64,
    pub y2: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NumericKeyframe {
    pub time_ns: u64,
    pub value: f64,
    #[serde(default)]
    pub interpolation: KeyframeInterpolation,
    /// Optional outgoing custom cubic-bezier controls for the segment from
    /// this keyframe to the next keyframe in the same lane.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bezier_controls: Option<BezierControls>,
}

impl NumericKeyframe {
    pub fn segment_control_points(&self) -> (f64, f64, f64, f64) {
        if let Some(ref bezier) = self.bezier_controls {
            (
                bezier.x1.clamp(0.0, 1.0),
                bezier.y1.clamp(0.0, 1.0),
                bezier.x2.clamp(0.0, 1.0),
                bezier.y2.clamp(0.0, 1.0),
            )
        } else {
            self.interpolation.control_points()
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Phase1KeyframeProperty {
    PositionX,
    PositionY,
    Scale,
    Opacity,
    Brightness,
    Contrast,
    Saturation,
    Temperature,
    Tint,
    Volume,
    Pan,
    Speed,
    Rotate,
    CropLeft,
    CropRight,
    CropTop,
    CropBottom,
}

impl Phase1KeyframeProperty {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::PositionX => "position_x",
            Self::PositionY => "position_y",
            Self::Scale => "scale",
            Self::Opacity => "opacity",
            Self::Brightness => "brightness",
            Self::Contrast => "contrast",
            Self::Saturation => "saturation",
            Self::Temperature => "temperature",
            Self::Tint => "tint",
            Self::Volume => "volume",
            Self::Pan => "pan",
            Self::Speed => "speed",
            Self::Rotate => "rotate",
            Self::CropLeft => "crop_left",
            Self::CropRight => "crop_right",
            Self::CropTop => "crop_top",
            Self::CropBottom => "crop_bottom",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "position_x" | "position-x" => Some(Self::PositionX),
            "position_y" | "position-y" => Some(Self::PositionY),
            "scale" => Some(Self::Scale),
            "opacity" => Some(Self::Opacity),
            "brightness" => Some(Self::Brightness),
            "contrast" => Some(Self::Contrast),
            "saturation" => Some(Self::Saturation),
            "temperature" => Some(Self::Temperature),
            "tint" => Some(Self::Tint),
            "volume" => Some(Self::Volume),
            "pan" => Some(Self::Pan),
            "speed" => Some(Self::Speed),
            "rotate" => Some(Self::Rotate),
            "crop_left" | "crop-left" => Some(Self::CropLeft),
            "crop_right" | "crop-right" => Some(Self::CropRight),
            "crop_top" | "crop-top" => Some(Self::CropTop),
            "crop_bottom" | "crop-bottom" => Some(Self::CropBottom),
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
fn default_title_outline_color() -> u32 {
    0x000000FF
}
fn default_title_shadow_color() -> u32 {
    0x000000AA
}
fn default_title_shadow_offset() -> f64 {
    2.0
}
fn default_title_bg_box_color() -> u32 {
    0x00000088
}
fn default_title_bg_box_padding() -> f64 {
    8.0
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

/// An instance of a frei0r filter effect applied to a clip.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Frei0rEffect {
    /// Unique instance id (UUID v4).
    pub id: String,
    /// Short frei0r plugin name (e.g. `"cartoon"`), matching
    /// [`crate::media::frei0r_registry::Frei0rPluginInfo::frei0r_name`].
    pub plugin_name: String,
    /// Whether the effect is currently active in the filter chain.
    #[serde(default = "default_effect_enabled")]
    pub enabled: bool,
    /// Numeric parameter values keyed by GStreamer property name.
    #[serde(default)]
    pub params: HashMap<String, f64>,
    /// String parameter values keyed by GStreamer property name
    /// (e.g. blend-mode → "normal").
    #[serde(default)]
    pub string_params: HashMap<String, String>,
}

fn default_effect_enabled() -> bool {
    true
}

impl Frei0rEffect {
    /// Create a new effect instance with default parameters.
    pub fn new(plugin_name: &str) -> Self {
        Self {
            id: Uuid::new_v4().to_string(),
            plugin_name: plugin_name.to_string(),
            enabled: true,
            params: HashMap::new(),
            string_params: HashMap::new(),
        }
    }

    /// Create a new effect instance with the given parameters.
    pub fn with_params(plugin_name: &str, params: HashMap<String, f64>) -> Self {
        Self {
            id: Uuid::new_v4().to_string(),
            plugin_name: plugin_name.to_string(),
            enabled: true,
            params,
            string_params: HashMap::new(),
        }
    }

    /// Create a new effect instance with both numeric and string parameters.
    pub fn with_all_params(
        plugin_name: &str,
        params: HashMap<String, f64>,
        string_params: HashMap<String, String>,
    ) -> Self {
        Self {
            id: Uuid::new_v4().to_string(),
            plugin_name: plugin_name.to_string(),
            enabled: true,
            params,
            string_params,
        }
    }
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
    /// Optional brightness keyframes over clip-local timeline.
    #[serde(default)]
    pub brightness_keyframes: Vec<NumericKeyframe>,
    /// Optional contrast keyframes over clip-local timeline.
    #[serde(default)]
    pub contrast_keyframes: Vec<NumericKeyframe>,
    /// Optional saturation keyframes over clip-local timeline.
    #[serde(default)]
    pub saturation_keyframes: Vec<NumericKeyframe>,
    /// Optional color-temperature keyframes over clip-local timeline.
    #[serde(default)]
    pub temperature_keyframes: Vec<NumericKeyframe>,
    /// Optional tint keyframes over clip-local timeline.
    #[serde(default)]
    pub tint_keyframes: Vec<NumericKeyframe>,
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
    /// Optional pan keyframes over clip-local timeline.
    #[serde(default)]
    pub pan_keyframes: Vec<NumericKeyframe>,
    /// Optional rotation keyframes over clip-local timeline.
    #[serde(default)]
    pub rotate_keyframes: Vec<NumericKeyframe>,
    /// Optional crop edge keyframes over clip-local timeline.
    #[serde(default)]
    pub crop_left_keyframes: Vec<NumericKeyframe>,
    #[serde(default)]
    pub crop_right_keyframes: Vec<NumericKeyframe>,
    #[serde(default)]
    pub crop_top_keyframes: Vec<NumericKeyframe>,
    #[serde(default)]
    pub crop_bottom_keyframes: Vec<NumericKeyframe>,
    /// Playback speed multiplier: 0.25 (slow) to 4.0 (fast), default 1.0.
    /// Values > 1.0 speed up (clip takes less time on timeline); < 1.0 slow down.
    #[serde(default = "default_speed")]
    pub speed: f64,
    /// Optional variable speed keyframes over clip-local timeline.
    #[serde(default)]
    pub speed_keyframes: Vec<NumericKeyframe>,
    /// Slow-motion frame interpolation mode (export-only).
    /// Applies minterpolate filter when speed < 1.0.
    #[serde(default)]
    pub slow_motion_interp: SlowMotionInterp,
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
    /// Template ID (e.g. "lower_third") for title clips.
    #[serde(default)]
    pub title_template: String,
    /// Outline stroke color (RRGGBBAA).
    #[serde(default = "default_title_outline_color")]
    pub title_outline_color: u32,
    /// Outline width in pts (0 = none).
    #[serde(default)]
    pub title_outline_width: f64,
    /// Drop shadow enabled.
    #[serde(default)]
    pub title_shadow: bool,
    /// Shadow color (RRGGBBAA).
    #[serde(default = "default_title_shadow_color")]
    pub title_shadow_color: u32,
    /// Shadow X offset in pts.
    #[serde(default = "default_title_shadow_offset")]
    pub title_shadow_offset_x: f64,
    /// Shadow Y offset in pts.
    #[serde(default = "default_title_shadow_offset")]
    pub title_shadow_offset_y: f64,
    /// Background box enabled.
    #[serde(default)]
    pub title_bg_box: bool,
    /// Background box color (RRGGBBAA).
    #[serde(default = "default_title_bg_box_color")]
    pub title_bg_box_color: u32,
    /// Background box padding in pts.
    #[serde(default = "default_title_bg_box_padding")]
    pub title_bg_box_padding: f64,
    /// Title clip background color (0 = transparent).
    #[serde(default)]
    pub title_clip_bg_color: u32,
    /// Secondary line of text (used by some templates).
    #[serde(default)]
    pub title_secondary_text: String,
    /// Transition to the next clip on the same track (e.g. "cross_dissolve").
    #[serde(default)]
    pub transition_after: String,
    /// Transition duration in nanoseconds for `transition_after` (0 = none).
    #[serde(default)]
    pub transition_after_ns: u64,
    /// Ordered list of .cube LUT file paths for color grading (applied sequentially on export via ffmpeg lut3d).
    /// Empty means no LUTs are assigned.
    #[serde(default)]
    pub lut_paths: Vec<String>,
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
    /// Compositing blend mode: Normal, Multiply, Screen, Overlay, Add, Difference, SoftLight.
    #[serde(default)]
    pub blend_mode: BlendMode,
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
    /// Exposure adjustment: −1.0 (darken) to 1.0 (brighten). Default 0.0.
    /// Maps to FCP "Exposure" param (key 3, range −100..100).
    #[serde(default)]
    pub exposure: f32,
    /// Black point adjustment: −1.0 (lower) to 1.0 (raise). Default 0.0.
    /// Maps to FCP "Black Point" param (key 1, range −100..100).
    #[serde(default)]
    pub black_point: f32,
    /// Highlights warmth (orange–blue): −1.0 to 1.0. Default 0.0.
    /// Maps to FCP "Highlights Warmth" param (key 10, range −100..100).
    #[serde(default)]
    pub highlights_warmth: f32,
    /// Highlights tint (green–magenta): −1.0 to 1.0. Default 0.0.
    /// Maps to FCP "Highlights Tint" param (key 11, range −100..100).
    #[serde(default)]
    pub highlights_tint: f32,
    /// Midtones warmth (orange–blue): −1.0 to 1.0. Default 0.0.
    /// Maps to FCP "Midtones Warmth" param (key 12, range −100..100).
    #[serde(default)]
    pub midtones_warmth: f32,
    /// Midtones tint (green–magenta): −1.0 to 1.0. Default 0.0.
    /// Maps to FCP "Midtones Tint" param (key 13, range −100..100).
    #[serde(default)]
    pub midtones_tint: f32,
    /// Shadows warmth (orange–blue): −1.0 to 1.0. Default 0.0.
    /// Maps to FCP "Shadows Warmth" param (key 14, range −100..100).
    #[serde(default)]
    pub shadows_warmth: f32,
    /// Shadows tint (green–magenta): −1.0 to 1.0. Default 0.0.
    /// Maps to FCP "Shadows Tint" param (key 15, range −100..100).
    #[serde(default)]
    pub shadows_tint: f32,
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
    /// Full probed duration of the source media file, in nanoseconds.
    /// Used to clamp `source_out` during trim operations so clips cannot
    /// be expanded beyond their actual running time.
    #[serde(default)]
    pub media_duration_ns: Option<u64>,
    /// Applied frei0r filter effects, ordered from first to last in the chain.
    #[serde(default)]
    pub frei0r_effects: Vec<Frei0rEffect>,
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
    /// Compute composite cache key for all assigned LUTs.
    /// Returns `None` if no LUTs are assigned.
    pub fn lut_key(&self) -> Option<String> {
        if self.lut_paths.is_empty() {
            None
        } else {
            Some(self.lut_paths.join("|"))
        }
    }

    fn remove_keyframes_at_local_time(
        keyframes: &mut Vec<NumericKeyframe>,
        local_time_ns: u64,
    ) -> usize {
        let before = keyframes.len();
        keyframes.retain(|kf| kf.time_ns != local_time_ns);
        before.saturating_sub(keyframes.len())
    }

    fn set_keyframe_interpolation_at_local_time(
        keyframes: &mut [NumericKeyframe],
        local_time_ns: u64,
        interpolation: KeyframeInterpolation,
    ) -> usize {
        let mut updated = 0usize;
        for kf in keyframes.iter_mut() {
            if kf.time_ns == local_time_ns {
                kf.interpolation = interpolation;
                kf.bezier_controls = None;
                updated += 1;
            }
        }
        updated
    }

    fn move_keyframes_by_time_map(
        keyframes: &mut Vec<NumericKeyframe>,
        move_map: &[(u64, u64)],
    ) -> usize {
        if keyframes.is_empty() || move_map.is_empty() {
            return 0;
        }
        let mut changed = 0usize;
        let original = keyframes.clone();
        let mut rebuilt = Vec::with_capacity(original.len());
        let mut destination_times = Vec::with_capacity(move_map.len());
        for (_, to) in move_map {
            destination_times.push(*to);
        }

        for kf in &original {
            let is_moved_source = move_map.iter().any(|(from, _)| kf.time_ns == *from);
            let is_moved_destination = destination_times.contains(&kf.time_ns);
            if !is_moved_source && !is_moved_destination {
                rebuilt.push(kf.clone());
            }
        }

        for (from, to) in move_map {
            let mut moved_from = original
                .iter()
                .filter(|kf| kf.time_ns == *from)
                .cloned()
                .collect::<Vec<_>>();
            if moved_from.is_empty() {
                continue;
            }
            changed += moved_from.len();
            for kf in &mut moved_from {
                kf.time_ns = *to;
            }
            rebuilt.extend(moved_from);
        }

        rebuilt.sort_by_key(|kf| kf.time_ns);
        *keyframes = rebuilt;
        changed
    }

    /// Retain only keyframes in a single vec that fall within `[start_ns, end_ns)`,
    /// then rebase their `time_ns` so `start_ns` maps to 0.
    fn retain_and_rebase_keyframes(kfs: &mut Vec<NumericKeyframe>, start_ns: u64, end_ns: u64) {
        kfs.retain(|kf| kf.time_ns >= start_ns && kf.time_ns < end_ns);
        for kf in kfs.iter_mut() {
            kf.time_ns = kf.time_ns.saturating_sub(start_ns);
        }
    }

    /// Filter all 17 keyframe vectors to the clip-local time range `[start_ns, end_ns)`,
    /// rebasing retained keyframes so `start_ns` maps to time 0.
    pub fn retain_keyframes_in_local_range(&mut self, start_ns: u64, end_ns: u64) {
        Self::retain_and_rebase_keyframes(&mut self.brightness_keyframes, start_ns, end_ns);
        Self::retain_and_rebase_keyframes(&mut self.contrast_keyframes, start_ns, end_ns);
        Self::retain_and_rebase_keyframes(&mut self.saturation_keyframes, start_ns, end_ns);
        Self::retain_and_rebase_keyframes(&mut self.temperature_keyframes, start_ns, end_ns);
        Self::retain_and_rebase_keyframes(&mut self.tint_keyframes, start_ns, end_ns);
        Self::retain_and_rebase_keyframes(&mut self.volume_keyframes, start_ns, end_ns);
        Self::retain_and_rebase_keyframes(&mut self.pan_keyframes, start_ns, end_ns);
        Self::retain_and_rebase_keyframes(&mut self.rotate_keyframes, start_ns, end_ns);
        Self::retain_and_rebase_keyframes(&mut self.crop_left_keyframes, start_ns, end_ns);
        Self::retain_and_rebase_keyframes(&mut self.crop_right_keyframes, start_ns, end_ns);
        Self::retain_and_rebase_keyframes(&mut self.crop_top_keyframes, start_ns, end_ns);
        Self::retain_and_rebase_keyframes(&mut self.crop_bottom_keyframes, start_ns, end_ns);
        Self::retain_and_rebase_keyframes(&mut self.speed_keyframes, start_ns, end_ns);
        Self::retain_and_rebase_keyframes(&mut self.scale_keyframes, start_ns, end_ns);
        Self::retain_and_rebase_keyframes(&mut self.opacity_keyframes, start_ns, end_ns);
        Self::retain_and_rebase_keyframes(&mut self.position_x_keyframes, start_ns, end_ns);
        Self::retain_and_rebase_keyframes(&mut self.position_y_keyframes, start_ns, end_ns);
    }

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
            brightness_keyframes: Vec::new(),
            contrast_keyframes: Vec::new(),
            saturation_keyframes: Vec::new(),
            temperature_keyframes: Vec::new(),
            tint_keyframes: Vec::new(),
            denoise: 0.0,
            sharpness: 0.0,
            volume: 1.0,
            volume_keyframes: Vec::new(),
            pan: 0.0,
            pan_keyframes: Vec::new(),
            rotate_keyframes: Vec::new(),
            crop_left_keyframes: Vec::new(),
            crop_right_keyframes: Vec::new(),
            crop_top_keyframes: Vec::new(),
            crop_bottom_keyframes: Vec::new(),
            speed: 1.0,
            speed_keyframes: Vec::new(),
            slow_motion_interp: SlowMotionInterp::Off,
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
            title_template: String::new(),
            title_outline_color: default_title_outline_color(),
            title_outline_width: 0.0,
            title_shadow: false,
            title_shadow_color: default_title_shadow_color(),
            title_shadow_offset_x: default_title_shadow_offset(),
            title_shadow_offset_y: default_title_shadow_offset(),
            title_bg_box: false,
            title_bg_box_color: default_title_bg_box_color(),
            title_bg_box_padding: default_title_bg_box_padding(),
            title_clip_bg_color: 0,
            title_secondary_text: String::new(),
            transition_after: String::new(),
            transition_after_ns: 0,
            lut_paths: Vec::new(),
            scale: 1.0,
            scale_keyframes: Vec::new(),
            opacity: 1.0,
            opacity_keyframes: Vec::new(),
            blend_mode: BlendMode::Normal,
            position_x: 0.0,
            position_x_keyframes: Vec::new(),
            position_y: 0.0,
            position_y_keyframes: Vec::new(),
            shadows: 0.0,
            midtones: 0.0,
            highlights: 0.0,
            exposure: 0.0,
            black_point: 0.0,
            highlights_warmth: 0.0,
            highlights_tint: 0.0,
            midtones_warmth: 0.0,
            midtones_tint: 0.0,
            shadows_warmth: 0.0,
            shadows_tint: 0.0,
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
            media_duration_ns: None,
            frei0r_effects: Vec::new(),
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

    /// Returns `true` when the clip has one or more frei0r effects applied.
    pub fn has_frei0r_effects(&self) -> bool {
        !self.frei0r_effects.is_empty()
    }

    /// Maximum allowed `source_out` value, derived from the probed media
    /// duration.  Returns `None` when the media duration is unknown (legacy
    /// clips or FCPXML imports without a probe result), or when the clip is
    /// a still image (images can be extended to any timeline length).
    pub fn max_source_out(&self) -> Option<u64> {
        if self.kind == ClipKind::Image || self.kind == ClipKind::Title {
            return None;
        }
        self.media_duration_ns
    }

    /// Clamp `source_out` so it does not exceed the source media duration.
    pub fn clamp_source_out(&mut self) {
        if let Some(max) = self.max_source_out() {
            if self.source_out > max {
                self.source_out = max;
            }
        }
    }

    /// Convert a timeline-space delta to source-space using the clip's speed.
    /// When speed keyframes are present, uses the mean speed over the clip duration.
    pub fn timeline_to_source_delta(&self, timeline_delta_ns: i64) -> i64 {
        let speed = if !self.speed_keyframes.is_empty() {
            let dur = self.duration();
            if dur > 0 {
                self.integrated_source_distance_for_local_timeline_ns(dur) / dur as f64
            } else {
                self.speed.max(0.01)
            }
        } else {
            self.speed.max(0.01)
        };
        (timeline_delta_ns as f64 * speed) as i64
    }

    /// Convert a timeline-space duration to source-space using the clip's speed.
    /// When speed keyframes are present, uses the mean speed over the clip duration.
    pub fn timeline_to_source_dur(&self, timeline_dur_ns: u64) -> u64 {
        let speed = if !self.speed_keyframes.is_empty() {
            let dur = self.duration();
            if dur > 0 {
                self.integrated_source_distance_for_local_timeline_ns(dur) / dur as f64
            } else {
                self.speed.max(0.01)
            }
        } else {
            self.speed.max(0.01)
        };
        (timeline_dur_ns as f64 * speed) as u64
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
                let (x1, y1, x2, y2) = prev.segment_control_points();
                let eased_t = cubic_bezier_ease(x1, y1, x2, y2, t);
                return prev.value + (next.value - prev.value) * eased_t;
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
            Phase1KeyframeProperty::Brightness => &self.brightness_keyframes,
            Phase1KeyframeProperty::Contrast => &self.contrast_keyframes,
            Phase1KeyframeProperty::Saturation => &self.saturation_keyframes,
            Phase1KeyframeProperty::Temperature => &self.temperature_keyframes,
            Phase1KeyframeProperty::Tint => &self.tint_keyframes,
            Phase1KeyframeProperty::Volume => &self.volume_keyframes,
            Phase1KeyframeProperty::Pan => &self.pan_keyframes,
            Phase1KeyframeProperty::Speed => &self.speed_keyframes,
            Phase1KeyframeProperty::Rotate => &self.rotate_keyframes,
            Phase1KeyframeProperty::CropLeft => &self.crop_left_keyframes,
            Phase1KeyframeProperty::CropRight => &self.crop_right_keyframes,
            Phase1KeyframeProperty::CropTop => &self.crop_top_keyframes,
            Phase1KeyframeProperty::CropBottom => &self.crop_bottom_keyframes,
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
            Phase1KeyframeProperty::Brightness => &mut self.brightness_keyframes,
            Phase1KeyframeProperty::Contrast => &mut self.contrast_keyframes,
            Phase1KeyframeProperty::Saturation => &mut self.saturation_keyframes,
            Phase1KeyframeProperty::Temperature => &mut self.temperature_keyframes,
            Phase1KeyframeProperty::Tint => &mut self.tint_keyframes,
            Phase1KeyframeProperty::Volume => &mut self.volume_keyframes,
            Phase1KeyframeProperty::Pan => &mut self.pan_keyframes,
            Phase1KeyframeProperty::Speed => &mut self.speed_keyframes,
            Phase1KeyframeProperty::Rotate => &mut self.rotate_keyframes,
            Phase1KeyframeProperty::CropLeft => &mut self.crop_left_keyframes,
            Phase1KeyframeProperty::CropRight => &mut self.crop_right_keyframes,
            Phase1KeyframeProperty::CropTop => &mut self.crop_top_keyframes,
            Phase1KeyframeProperty::CropBottom => &mut self.crop_bottom_keyframes,
        }
    }

    pub fn default_value_for_phase1_property(&self, property: Phase1KeyframeProperty) -> f64 {
        match property {
            Phase1KeyframeProperty::PositionX => self.position_x,
            Phase1KeyframeProperty::PositionY => self.position_y,
            Phase1KeyframeProperty::Scale => self.scale,
            Phase1KeyframeProperty::Opacity => self.opacity,
            Phase1KeyframeProperty::Brightness => self.brightness as f64,
            Phase1KeyframeProperty::Contrast => self.contrast as f64,
            Phase1KeyframeProperty::Saturation => self.saturation as f64,
            Phase1KeyframeProperty::Temperature => self.temperature as f64,
            Phase1KeyframeProperty::Tint => self.tint as f64,
            Phase1KeyframeProperty::Volume => self.volume as f64,
            Phase1KeyframeProperty::Pan => self.pan as f64,
            Phase1KeyframeProperty::Speed => self.speed,
            Phase1KeyframeProperty::Rotate => self.rotate as f64,
            Phase1KeyframeProperty::CropLeft => self.crop_left as f64,
            Phase1KeyframeProperty::CropRight => self.crop_right as f64,
            Phase1KeyframeProperty::CropTop => self.crop_top as f64,
            Phase1KeyframeProperty::CropBottom => self.crop_bottom as f64,
        }
    }

    pub fn clamp_phase1_property_value(property: Phase1KeyframeProperty, value: f64) -> f64 {
        match property {
            Phase1KeyframeProperty::PositionX | Phase1KeyframeProperty::PositionY => {
                value.clamp(-1.0, 1.0)
            }
            Phase1KeyframeProperty::Scale => value.clamp(0.1, 4.0),
            Phase1KeyframeProperty::Opacity => value.clamp(0.0, 1.0),
            Phase1KeyframeProperty::Brightness => value.clamp(-1.0, 1.0),
            Phase1KeyframeProperty::Contrast => value.clamp(0.0, 2.0),
            Phase1KeyframeProperty::Saturation => value.clamp(0.0, 2.0),
            Phase1KeyframeProperty::Temperature => value.clamp(2000.0, 10000.0),
            Phase1KeyframeProperty::Tint => value.clamp(-1.0, 1.0),
            Phase1KeyframeProperty::Volume => value.clamp(0.0, 4.0),
            Phase1KeyframeProperty::Pan => value.clamp(-1.0, 1.0),
            Phase1KeyframeProperty::Speed => value.clamp(0.05, 16.0),
            Phase1KeyframeProperty::Rotate => value.clamp(-180.0, 180.0),
            Phase1KeyframeProperty::CropLeft
            | Phase1KeyframeProperty::CropRight
            | Phase1KeyframeProperty::CropTop
            | Phase1KeyframeProperty::CropBottom => value.clamp(0.0, 500.0),
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
        self.upsert_phase1_keyframe_at_timeline_ns_with_interp(
            property,
            timeline_pos_ns,
            value,
            KeyframeInterpolation::Linear,
        )
    }

    pub fn upsert_phase1_keyframe_at_timeline_ns_with_interp(
        &mut self,
        property: Phase1KeyframeProperty,
        timeline_pos_ns: u64,
        value: f64,
        interpolation: KeyframeInterpolation,
    ) -> u64 {
        // For speed keyframes, don't clamp to duration() — speed keyframes
        // define the speed curve that *determines* the duration, so they must
        // be positioned independently of it. The clip duration adjusts to
        // encompass whatever speed curve the keyframes define.
        let local_time_ns = if property == Phase1KeyframeProperty::Speed {
            timeline_pos_ns.saturating_sub(self.timeline_start)
        } else {
            self.local_timeline_position_ns(timeline_pos_ns)
        };
        let clamped_value = Self::clamp_phase1_property_value(property, value);
        // Capture duration before the change so we can rescale siblings.
        let old_dur = if property == Phase1KeyframeProperty::Speed
            && !self.speed_keyframes.is_empty()
        {
            Some(self.duration())
        } else {
            None
        };
        let keyframes = self.keyframes_for_phase1_property_mut(property);
        // Snap to an existing keyframe within half a frame (~20ms) to avoid
        // creating near-duplicates when the playhead is close but not exact.
        const SNAP_TOLERANCE_NS: u64 = 20_000_000;
        let mut changed_time_ns: Option<u64> = None;
        if let Some(existing) = keyframes
            .iter_mut()
            .find(|kf| kf.time_ns.abs_diff(local_time_ns) <= SNAP_TOLERANCE_NS)
        {
            changed_time_ns = Some(existing.time_ns);
            existing.value = clamped_value;
            existing.interpolation = interpolation;
            existing.bezier_controls = None;
        } else {
            keyframes.push(NumericKeyframe {
                time_ns: local_time_ns,
                value: clamped_value,
                interpolation,
                bezier_controls: None,
            });
            keyframes.sort_by_key(|kf| kf.time_ns);
        }
        // When a speed keyframe's VALUE changes (not a new insertion),
        // proportionally rescale all OTHER keyframes so they maintain
        // their relative position within the clip.
        if let (Some(old_d), Some(anchor)) = (old_dur, changed_time_ns) {
            let new_d = self.duration();
            if old_d > 0 && new_d > 0 && old_d != new_d {
                let ratio = new_d as f64 / old_d as f64;
                for kf in &mut self.speed_keyframes {
                    if kf.time_ns != anchor {
                        kf.time_ns = (kf.time_ns as f64 * ratio).round() as u64;
                    }
                }
                self.speed_keyframes.sort_by_key(|kf| kf.time_ns);
            }
        }
        local_time_ns
    }

    pub fn remove_phase1_keyframe_at_timeline_ns(
        &mut self,
        property: Phase1KeyframeProperty,
        timeline_pos_ns: u64,
    ) -> bool {
        let local_time_ns = if property == Phase1KeyframeProperty::Speed {
            timeline_pos_ns.saturating_sub(self.timeline_start)
        } else {
            self.local_timeline_position_ns(timeline_pos_ns)
        };
        let keyframes = self.keyframes_for_phase1_property_mut(property);
        let before = keyframes.len();
        const SNAP_TOLERANCE_NS: u64 = 20_000_000;
        keyframes.retain(|kf| kf.time_ns.abs_diff(local_time_ns) > SNAP_TOLERANCE_NS);
        keyframes.len() != before
    }

    pub fn remove_all_phase1_keyframes_at_timeline_ns(&mut self, timeline_pos_ns: u64) -> usize {
        let local_time_ns = self.local_timeline_position_ns(timeline_pos_ns);
        self.remove_all_phase1_keyframes_at_local_ns(local_time_ns)
    }

    pub fn remove_all_phase1_keyframes_at_local_ns(&mut self, local_time_ns: u64) -> usize {
        let mut removed = 0usize;
        removed += Self::remove_keyframes_at_local_time(&mut self.scale_keyframes, local_time_ns);
        removed += Self::remove_keyframes_at_local_time(&mut self.opacity_keyframes, local_time_ns);
        removed +=
            Self::remove_keyframes_at_local_time(&mut self.brightness_keyframes, local_time_ns);
        removed +=
            Self::remove_keyframes_at_local_time(&mut self.contrast_keyframes, local_time_ns);
        removed +=
            Self::remove_keyframes_at_local_time(&mut self.saturation_keyframes, local_time_ns);
        removed +=
            Self::remove_keyframes_at_local_time(&mut self.temperature_keyframes, local_time_ns);
        removed += Self::remove_keyframes_at_local_time(&mut self.tint_keyframes, local_time_ns);
        removed +=
            Self::remove_keyframes_at_local_time(&mut self.position_x_keyframes, local_time_ns);
        removed +=
            Self::remove_keyframes_at_local_time(&mut self.position_y_keyframes, local_time_ns);
        removed += Self::remove_keyframes_at_local_time(&mut self.volume_keyframes, local_time_ns);
        removed += Self::remove_keyframes_at_local_time(&mut self.pan_keyframes, local_time_ns);
        removed += Self::remove_keyframes_at_local_time(&mut self.speed_keyframes, local_time_ns);
        removed += Self::remove_keyframes_at_local_time(&mut self.rotate_keyframes, local_time_ns);
        removed +=
            Self::remove_keyframes_at_local_time(&mut self.crop_left_keyframes, local_time_ns);
        removed +=
            Self::remove_keyframes_at_local_time(&mut self.crop_right_keyframes, local_time_ns);
        removed +=
            Self::remove_keyframes_at_local_time(&mut self.crop_top_keyframes, local_time_ns);
        removed +=
            Self::remove_keyframes_at_local_time(&mut self.crop_bottom_keyframes, local_time_ns);
        removed
    }

    pub fn set_phase1_keyframe_interpolation_at_local_ns(
        &mut self,
        local_time_ns: u64,
        interpolation: KeyframeInterpolation,
    ) -> usize {
        let mut updated = 0usize;
        updated += Self::set_keyframe_interpolation_at_local_time(
            &mut self.scale_keyframes,
            local_time_ns,
            interpolation,
        );
        updated += Self::set_keyframe_interpolation_at_local_time(
            &mut self.opacity_keyframes,
            local_time_ns,
            interpolation,
        );
        updated += Self::set_keyframe_interpolation_at_local_time(
            &mut self.brightness_keyframes,
            local_time_ns,
            interpolation,
        );
        updated += Self::set_keyframe_interpolation_at_local_time(
            &mut self.contrast_keyframes,
            local_time_ns,
            interpolation,
        );
        updated += Self::set_keyframe_interpolation_at_local_time(
            &mut self.saturation_keyframes,
            local_time_ns,
            interpolation,
        );
        updated += Self::set_keyframe_interpolation_at_local_time(
            &mut self.temperature_keyframes,
            local_time_ns,
            interpolation,
        );
        updated += Self::set_keyframe_interpolation_at_local_time(
            &mut self.tint_keyframes,
            local_time_ns,
            interpolation,
        );
        updated += Self::set_keyframe_interpolation_at_local_time(
            &mut self.position_x_keyframes,
            local_time_ns,
            interpolation,
        );
        updated += Self::set_keyframe_interpolation_at_local_time(
            &mut self.position_y_keyframes,
            local_time_ns,
            interpolation,
        );
        updated += Self::set_keyframe_interpolation_at_local_time(
            &mut self.volume_keyframes,
            local_time_ns,
            interpolation,
        );
        updated += Self::set_keyframe_interpolation_at_local_time(
            &mut self.pan_keyframes,
            local_time_ns,
            interpolation,
        );
        updated += Self::set_keyframe_interpolation_at_local_time(
            &mut self.speed_keyframes,
            local_time_ns,
            interpolation,
        );
        updated += Self::set_keyframe_interpolation_at_local_time(
            &mut self.rotate_keyframes,
            local_time_ns,
            interpolation,
        );
        updated += Self::set_keyframe_interpolation_at_local_time(
            &mut self.crop_left_keyframes,
            local_time_ns,
            interpolation,
        );
        updated += Self::set_keyframe_interpolation_at_local_time(
            &mut self.crop_right_keyframes,
            local_time_ns,
            interpolation,
        );
        updated += Self::set_keyframe_interpolation_at_local_time(
            &mut self.crop_top_keyframes,
            local_time_ns,
            interpolation,
        );
        updated += Self::set_keyframe_interpolation_at_local_time(
            &mut self.crop_bottom_keyframes,
            local_time_ns,
            interpolation,
        );
        updated
    }

    pub fn move_all_phase1_keyframes_local_ns(&mut self, move_map: &[(u64, u64)]) -> usize {
        let mut changed = 0usize;
        changed += Self::move_keyframes_by_time_map(&mut self.scale_keyframes, move_map);
        changed += Self::move_keyframes_by_time_map(&mut self.opacity_keyframes, move_map);
        changed += Self::move_keyframes_by_time_map(&mut self.brightness_keyframes, move_map);
        changed += Self::move_keyframes_by_time_map(&mut self.contrast_keyframes, move_map);
        changed += Self::move_keyframes_by_time_map(&mut self.saturation_keyframes, move_map);
        changed += Self::move_keyframes_by_time_map(&mut self.temperature_keyframes, move_map);
        changed += Self::move_keyframes_by_time_map(&mut self.tint_keyframes, move_map);
        changed += Self::move_keyframes_by_time_map(&mut self.position_x_keyframes, move_map);
        changed += Self::move_keyframes_by_time_map(&mut self.position_y_keyframes, move_map);
        changed += Self::move_keyframes_by_time_map(&mut self.volume_keyframes, move_map);
        changed += Self::move_keyframes_by_time_map(&mut self.pan_keyframes, move_map);
        changed += Self::move_keyframes_by_time_map(&mut self.speed_keyframes, move_map);
        changed += Self::move_keyframes_by_time_map(&mut self.rotate_keyframes, move_map);
        changed += Self::move_keyframes_by_time_map(&mut self.crop_left_keyframes, move_map);
        changed += Self::move_keyframes_by_time_map(&mut self.crop_right_keyframes, move_map);
        changed += Self::move_keyframes_by_time_map(&mut self.crop_top_keyframes, move_map);
        changed += Self::move_keyframes_by_time_map(&mut self.crop_bottom_keyframes, move_map);
        changed
    }

    /// Collect all unique keyframe times across every phase-1 property, sorted.
    fn all_keyframe_local_times_ns(&self) -> Vec<u64> {
        let mut times: Vec<u64> = self
            .scale_keyframes
            .iter()
            .chain(&self.opacity_keyframes)
            .chain(&self.brightness_keyframes)
            .chain(&self.contrast_keyframes)
            .chain(&self.saturation_keyframes)
            .chain(&self.temperature_keyframes)
            .chain(&self.tint_keyframes)
            .chain(&self.position_x_keyframes)
            .chain(&self.position_y_keyframes)
            .chain(&self.volume_keyframes)
            .chain(&self.pan_keyframes)
            .chain(&self.speed_keyframes)
            .chain(&self.rotate_keyframes)
            .chain(&self.crop_left_keyframes)
            .chain(&self.crop_right_keyframes)
            .chain(&self.crop_top_keyframes)
            .chain(&self.crop_bottom_keyframes)
            .map(|kf| kf.time_ns)
            .collect();
        times.sort_unstable();
        times.dedup();
        times
    }

    /// Return the next keyframe local time strictly after `local_ns`, across all properties.
    pub fn next_keyframe_local_ns(&self, local_ns: u64) -> Option<u64> {
        self.all_keyframe_local_times_ns()
            .into_iter()
            .find(|&t| t > local_ns)
    }

    /// Return the previous keyframe local time strictly before `local_ns`, across all properties.
    pub fn prev_keyframe_local_ns(&self, local_ns: u64) -> Option<u64> {
        self.all_keyframe_local_times_ns()
            .into_iter()
            .rev()
            .find(|&t| t < local_ns)
    }

    /// Return true if any phase-1 property has a keyframe within `tolerance_ns` of `local_ns`.
    pub fn has_keyframe_at_local_ns(&self, local_ns: u64, tolerance_ns: u64) -> bool {
        self.all_keyframe_local_times_ns().iter().any(|&t| {
            let diff = if t >= local_ns {
                t - local_ns
            } else {
                local_ns - t
            };
            diff <= tolerance_ns
        })
    }

    /// Return the next keyframe local time strictly after `local_ns` for a single property.
    pub fn next_keyframe_local_ns_for_property(
        &self,
        property: Phase1KeyframeProperty,
        local_ns: u64,
    ) -> Option<u64> {
        let kfs = self.keyframes_for_phase1_property(property);
        kfs.iter().map(|kf| kf.time_ns).find(|&t| t > local_ns)
    }

    /// Return the previous keyframe local time strictly before `local_ns` for a single property.
    pub fn prev_keyframe_local_ns_for_property(
        &self,
        property: Phase1KeyframeProperty,
        local_ns: u64,
    ) -> Option<u64> {
        let kfs = self.keyframes_for_phase1_property(property);
        kfs.iter()
            .map(|kf| kf.time_ns)
            .rev()
            .find(|&t| t < local_ns)
    }

    /// Return true if a specific property has a keyframe within `tolerance_ns` of `local_ns`.
    pub fn has_keyframe_at_local_ns_for_property(
        &self,
        property: Phase1KeyframeProperty,
        local_ns: u64,
        tolerance_ns: u64,
    ) -> bool {
        let kfs = self.keyframes_for_phase1_property(property);
        kfs.iter()
            .any(|kf| (kf.time_ns as i64 - local_ns as i64).unsigned_abs() <= tolerance_ns)
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

    pub fn speed_at_local_timeline_ns(&self, local_timeline_ns: u64) -> f64 {
        if !self.speed_keyframes.is_empty() {
            // For speed, use clip.speed (the base value) before the first keyframe
            // and after the last keyframe, rather than holding the nearest keyframe
            // value. This lets users set a base speed and add ramp keyframes at
            // specific points without the base being overridden.
            let first_ns = self
                .speed_keyframes
                .iter()
                .map(|kf| kf.time_ns)
                .min()
                .unwrap_or(0);
            let last_ns = self
                .speed_keyframes
                .iter()
                .map(|kf| kf.time_ns)
                .max()
                .unwrap_or(0);
            if local_timeline_ns < first_ns || local_timeline_ns > last_ns {
                return self.speed.clamp(0.05, 16.0);
            }
        }
        Self::evaluate_keyframed_value(&self.speed_keyframes, local_timeline_ns, self.speed)
            .clamp(0.05, 16.0)
    }

    pub fn speed_at_timeline_ns(&self, timeline_pos_ns: u64) -> f64 {
        let local_ns = timeline_pos_ns
            .saturating_sub(self.timeline_start)
            .min(self.duration());
        self.speed_at_local_timeline_ns(local_ns)
    }

    pub(crate) fn integrated_source_distance_for_local_timeline_ns(&self, local_timeline_ns: u64) -> f64 {
        if local_timeline_ns == 0 {
            return 0.0;
        }
        if self.speed_keyframes.is_empty() {
            return local_timeline_ns as f64 * self.speed.clamp(0.05, 16.0);
        }
        // Adaptive sampling: place samples at keyframe boundaries plus a fixed
        // number of intermediate points per segment. This gives accurate results
        // for piecewise-linear speed curves with far fewer evaluations than the
        // previous fixed 4096-sample approach.
        let mut breakpoints: Vec<u64> = Vec::with_capacity(self.speed_keyframes.len() + 2);
        breakpoints.push(0);
        for kf in &self.speed_keyframes {
            if kf.time_ns > 0 && kf.time_ns < local_timeline_ns {
                breakpoints.push(kf.time_ns);
            }
        }
        breakpoints.push(local_timeline_ns);
        breakpoints.sort_unstable();
        breakpoints.dedup();

        const SAMPLES_PER_SEGMENT: u64 = 8;
        let mut integrated = 0.0f64;
        for win in breakpoints.windows(2) {
            let seg_start = win[0];
            let seg_end = win[1];
            let seg_len = seg_end - seg_start;
            if seg_len == 0 {
                continue;
            }
            let n = SAMPLES_PER_SEGMENT.min(seg_len / 1_000_000).max(1);
            for j in 0..n {
                let t0 = seg_start + (u128::from(seg_len) * u128::from(j) / u128::from(n)) as u64;
                let t1 = seg_start + (u128::from(seg_len) * u128::from(j + 1) / u128::from(n)) as u64;
                let dt = t1 - t0;
                let mid = t0 + dt / 2;
                integrated += self.speed_at_local_timeline_ns(mid) * dt as f64;
            }
        }
        integrated
    }

    fn min_effective_speed_hint(&self) -> f64 {
        let mut min_speed = self.speed.clamp(0.05, 16.0);
        for kf in &self.speed_keyframes {
            min_speed = min_speed.min(kf.value.clamp(0.05, 16.0));
        }
        min_speed
    }

    fn playback_duration_from_speed(&self) -> u64 {
        let src = self.source_duration();
        if !self.speed_keyframes.is_empty() {
            // Bisect to find timeline duration T where
            // integrated_source_distance(T) == source_duration.
            let min_speed = self.min_effective_speed_hint();
            let upper = (src as f64 / min_speed) as u64 + 1_000_000_000; // +1s headroom
            let (mut lo, mut hi): (u64, u64) = (0, upper);
            for _ in 0..40 {
                let mid = lo + (hi - lo) / 2;
                if self.integrated_source_distance_for_local_timeline_ns(mid) < src as f64 {
                    lo = mid + 1;
                } else {
                    hi = mid;
                }
            }
            hi.max(1)
        } else if self.speed > 0.0 {
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
        assert_eq!(clip.temperature, 6500.0);
        assert_eq!(clip.tint, 0.0);
        assert!(clip.brightness_keyframes.is_empty());
        assert!(clip.contrast_keyframes.is_empty());
        assert!(clip.saturation_keyframes.is_empty());
        assert!(clip.temperature_keyframes.is_empty());
        assert!(clip.tint_keyframes.is_empty());
        assert_eq!(clip.color_label, ClipColorLabel::None);
        assert_eq!(clip.volume, 1.0);
        assert!(clip.volume_keyframes.is_empty());
        assert!(clip.rotate_keyframes.is_empty());
        assert!(clip.crop_left_keyframes.is_empty());
        assert!(clip.crop_right_keyframes.is_empty());
        assert!(clip.crop_top_keyframes.is_empty());
        assert!(clip.crop_bottom_keyframes.is_empty());
        assert_eq!(clip.speed, 1.0);
        assert!(clip.speed_keyframes.is_empty());
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
        assert!(clip.lut_paths.is_empty());
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
                bezier_controls: None,
            },
            NumericKeyframe {
                time_ns: 1_000_000_000,
                value: 1.0,
                interpolation: KeyframeInterpolation::Linear,
                bezier_controls: None,
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
                bezier_controls: None,
            },
            NumericKeyframe {
                time_ns: 300,
                value: 6.0,
                interpolation: KeyframeInterpolation::Linear,
                bezier_controls: None,
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
                bezier_controls: None,
            },
            NumericKeyframe {
                time_ns: 0,
                value: 3.0,
                interpolation: KeyframeInterpolation::Linear,
                bezier_controls: None,
            },
            NumericKeyframe {
                time_ns: 10,
                value: 5.0,
                interpolation: KeyframeInterpolation::Linear,
                bezier_controls: None,
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
                bezier_controls: None,
            },
            NumericKeyframe {
                time_ns: 1_000_000_000,
                value: 1.0,
                interpolation: KeyframeInterpolation::Linear,
                bezier_controls: None,
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
        assert!(clip.brightness_keyframes.is_empty());
        assert!(clip.contrast_keyframes.is_empty());
        assert!(clip.saturation_keyframes.is_empty());
        assert!(clip.temperature_keyframes.is_empty());
        assert!(clip.tint_keyframes.is_empty());
        assert!(clip.volume_keyframes.is_empty());
        assert!(clip.pan_keyframes.is_empty());
        assert!(clip.rotate_keyframes.is_empty());
        assert!(clip.crop_left_keyframes.is_empty());
        assert!(clip.crop_right_keyframes.is_empty());
        assert!(clip.crop_top_keyframes.is_empty());
        assert!(clip.crop_bottom_keyframes.is_empty());
    }

    #[test]
    fn test_next_prev_keyframe_across_properties() {
        let mut clip = make_test_clip(5_000_000_000, 0);
        // Scale keyframe at 1s, opacity at 2s, position_x at 3s
        clip.scale_keyframes.push(NumericKeyframe {
            time_ns: 1_000_000_000,
            value: 1.5,
            interpolation: KeyframeInterpolation::Linear,
            bezier_controls: None,
        });
        clip.opacity_keyframes.push(NumericKeyframe {
            time_ns: 2_000_000_000,
            value: 0.5,
            interpolation: KeyframeInterpolation::Linear,
            bezier_controls: None,
        });
        clip.position_x_keyframes.push(NumericKeyframe {
            time_ns: 3_000_000_000,
            value: 0.2,
            interpolation: KeyframeInterpolation::Linear,
            bezier_controls: None,
        });

        // next from 0 -> 1s (scale)
        assert_eq!(clip.next_keyframe_local_ns(0), Some(1_000_000_000));
        // next from 1s -> 2s (opacity)
        assert_eq!(
            clip.next_keyframe_local_ns(1_000_000_000),
            Some(2_000_000_000)
        );
        // next from 2.5s -> 3s (position_x)
        assert_eq!(
            clip.next_keyframe_local_ns(2_500_000_000),
            Some(3_000_000_000)
        );
        // next from 3s -> None
        assert_eq!(clip.next_keyframe_local_ns(3_000_000_000), None);

        // prev from 4s -> 3s
        assert_eq!(
            clip.prev_keyframe_local_ns(4_000_000_000),
            Some(3_000_000_000)
        );
        // prev from 2s -> 1s
        assert_eq!(
            clip.prev_keyframe_local_ns(2_000_000_000),
            Some(1_000_000_000)
        );
        // prev from 1s -> None
        assert_eq!(clip.prev_keyframe_local_ns(1_000_000_000), None);
        // prev from 0 -> None
        assert_eq!(clip.prev_keyframe_local_ns(0), None);
    }

    #[test]
    fn test_has_keyframe_at_local_ns_with_tolerance() {
        let mut clip = make_test_clip(5_000_000_000, 0);
        clip.volume_keyframes.push(NumericKeyframe {
            time_ns: 1_000_000_000,
            value: 0.8,
            interpolation: KeyframeInterpolation::Linear,
            bezier_controls: None,
        });

        let one_frame_24fps = 1_000_000_000 / 24; // ~41.6ms
                                                  // Exact match
        assert!(clip.has_keyframe_at_local_ns(1_000_000_000, one_frame_24fps));
        // Within tolerance
        assert!(clip.has_keyframe_at_local_ns(1_000_000_000 + one_frame_24fps / 2, one_frame_24fps));
        // Outside tolerance
        assert!(
            !clip.has_keyframe_at_local_ns(1_000_000_000 + one_frame_24fps * 2, one_frame_24fps)
        );
        // No keyframes at 0
        assert!(!clip.has_keyframe_at_local_ns(0, one_frame_24fps));
    }

    #[test]
    fn test_next_prev_with_duplicate_times_across_properties() {
        let mut clip = make_test_clip(5_000_000_000, 0);
        // Both scale and opacity have keyframes at 1s
        clip.scale_keyframes.push(NumericKeyframe {
            time_ns: 1_000_000_000,
            value: 2.0,
            interpolation: KeyframeInterpolation::Linear,
            bezier_controls: None,
        });
        clip.opacity_keyframes.push(NumericKeyframe {
            time_ns: 1_000_000_000,
            value: 0.5,
            interpolation: KeyframeInterpolation::Linear,
            bezier_controls: None,
        });
        clip.position_y_keyframes.push(NumericKeyframe {
            time_ns: 2_000_000_000,
            value: 0.3,
            interpolation: KeyframeInterpolation::Linear,
            bezier_controls: None,
        });

        // next from 0 -> 1s (deduplicated)
        assert_eq!(clip.next_keyframe_local_ns(0), Some(1_000_000_000));
        // next from 1s -> 2s
        assert_eq!(
            clip.next_keyframe_local_ns(1_000_000_000),
            Some(2_000_000_000)
        );
        // prev from 2s -> 1s (deduplicated)
        assert_eq!(
            clip.prev_keyframe_local_ns(2_000_000_000),
            Some(1_000_000_000)
        );
    }

    #[test]
    fn test_property_specific_keyframe_navigation() {
        let mut clip = make_test_clip(5_000_000_000, 0);
        clip.volume_keyframes.push(NumericKeyframe {
            time_ns: 500_000_000,
            value: 0.5,
            interpolation: KeyframeInterpolation::Linear,
            bezier_controls: None,
        });
        clip.volume_keyframes.push(NumericKeyframe {
            time_ns: 2_000_000_000,
            value: 0.8,
            interpolation: KeyframeInterpolation::Linear,
            bezier_controls: None,
        });
        clip.scale_keyframes.push(NumericKeyframe {
            time_ns: 1_000_000_000,
            value: 1.5,
            interpolation: KeyframeInterpolation::Linear,
            bezier_controls: None,
        });

        // Volume nav: 0 → 500ms → 2s
        assert_eq!(
            clip.next_keyframe_local_ns_for_property(Phase1KeyframeProperty::Volume, 0),
            Some(500_000_000)
        );
        assert_eq!(
            clip.next_keyframe_local_ns_for_property(Phase1KeyframeProperty::Volume, 500_000_000),
            Some(2_000_000_000)
        );
        assert_eq!(
            clip.next_keyframe_local_ns_for_property(Phase1KeyframeProperty::Volume, 2_000_000_000),
            None
        );
        // Volume prev
        assert_eq!(
            clip.prev_keyframe_local_ns_for_property(Phase1KeyframeProperty::Volume, 2_000_000_000),
            Some(500_000_000)
        );
        assert_eq!(
            clip.prev_keyframe_local_ns_for_property(Phase1KeyframeProperty::Volume, 500_000_000),
            None
        );
        // Scale nav should NOT see volume keyframes
        assert_eq!(
            clip.next_keyframe_local_ns_for_property(Phase1KeyframeProperty::Scale, 0),
            Some(1_000_000_000)
        );
        assert_eq!(
            clip.next_keyframe_local_ns_for_property(Phase1KeyframeProperty::Scale, 1_000_000_000),
            None
        );
        // has_keyframe_at_local_ns_for_property
        assert!(clip.has_keyframe_at_local_ns_for_property(
            Phase1KeyframeProperty::Volume,
            500_000_000,
            100
        ));
        assert!(!clip.has_keyframe_at_local_ns_for_property(
            Phase1KeyframeProperty::Scale,
            500_000_000,
            100
        ));
    }

    #[test]
    fn test_pan_phase1_clamp_and_eval() {
        let mut clip = make_test_clip(5_000_000_000, 0);
        clip.pan = 0.0;
        clip.pan_keyframes = vec![
            NumericKeyframe {
                time_ns: 0,
                value: -2.0, // clamp to -1.0
                interpolation: KeyframeInterpolation::Linear,
                bezier_controls: None,
            },
            NumericKeyframe {
                time_ns: 1_000_000_000,
                value: 2.0, // clamp to 1.0
                interpolation: KeyframeInterpolation::Linear,
                bezier_controls: None,
            },
        ];
        let local = 500_000_000;
        let v = Clip::evaluate_keyframed_value(
            &clip.pan_keyframes,
            local,
            clip.default_value_for_phase1_property(Phase1KeyframeProperty::Pan),
        );
        // evaluate_keyframed_value uses raw values; clamp happens on upsert, so validate clamp helper too.
        assert_eq!(
            Clip::clamp_phase1_property_value(Phase1KeyframeProperty::Pan, -2.0),
            -1.0
        );
        assert_eq!(
            Clip::clamp_phase1_property_value(Phase1KeyframeProperty::Pan, 2.0),
            1.0
        );
        assert!(v.is_finite());
    }

    #[test]
    fn test_rotate_crop_phase1_clamp_ranges() {
        assert_eq!(
            Clip::clamp_phase1_property_value(Phase1KeyframeProperty::Brightness, 5.0),
            1.0
        );
        assert_eq!(
            Clip::clamp_phase1_property_value(Phase1KeyframeProperty::Contrast, -3.0),
            0.0
        );
        assert_eq!(
            Clip::clamp_phase1_property_value(Phase1KeyframeProperty::Saturation, 5.0),
            2.0
        );
        assert_eq!(
            Clip::clamp_phase1_property_value(Phase1KeyframeProperty::Temperature, 1200.0),
            2000.0
        );
        assert_eq!(
            Clip::clamp_phase1_property_value(Phase1KeyframeProperty::Tint, -4.0),
            -1.0
        );
        assert_eq!(
            Clip::clamp_phase1_property_value(Phase1KeyframeProperty::Rotate, 500.0),
            180.0
        );
        assert_eq!(
            Clip::clamp_phase1_property_value(Phase1KeyframeProperty::Rotate, -500.0),
            -180.0
        );
        assert_eq!(
            Clip::clamp_phase1_property_value(Phase1KeyframeProperty::CropLeft, -10.0),
            0.0
        );
        assert_eq!(
            Clip::clamp_phase1_property_value(Phase1KeyframeProperty::CropBottom, 999.0),
            500.0
        );
    }

    #[test]
    fn test_cubic_bezier_ease_boundaries() {
        // All easing curves must pass through (0,0) and (1,1)
        for interp in KeyframeInterpolation::ALL {
            let v0 = interp.ease(0.0);
            let v1 = interp.ease(1.0);
            assert!((v0 - 0.0).abs() < 1e-9, "{:?} ease(0) = {}", interp, v0);
            assert!((v1 - 1.0).abs() < 1e-9, "{:?} ease(1) = {}", interp, v1);
        }
    }

    #[test]
    fn test_cubic_bezier_ease_monotonic() {
        // Easing curves should be monotonically increasing for t in [0,1]
        for interp in KeyframeInterpolation::ALL {
            let mut prev = 0.0;
            for i in 1..=100 {
                let t = i as f64 / 100.0;
                let v = interp.ease(t);
                assert!(
                    v >= prev - 1e-9,
                    "{:?} not monotonic at t={}: {} < {}",
                    interp,
                    t,
                    v,
                    prev
                );
                prev = v;
            }
        }
    }

    #[test]
    fn test_ease_in_slow_start() {
        // EaseIn: at t=0.25 the output should be less than 0.25 (slow start)
        let v = KeyframeInterpolation::EaseIn.ease(0.25);
        assert!(v < 0.25, "EaseIn(0.25) = {} should be < 0.25", v);
    }

    #[test]
    fn test_ease_out_fast_start() {
        // EaseOut: at t=0.25 the output should be greater than 0.25 (fast start)
        let v = KeyframeInterpolation::EaseOut.ease(0.25);
        assert!(v > 0.25, "EaseOut(0.25) = {} should be > 0.25", v);
    }

    #[test]
    fn test_ease_in_out_symmetric() {
        // EaseInOut should be roughly symmetric around (0.5, 0.5)
        let v = KeyframeInterpolation::EaseInOut.ease(0.5);
        assert!(
            (v - 0.5).abs() < 0.01,
            "EaseInOut(0.5) = {} should be ~0.5",
            v
        );
    }

    #[test]
    fn test_evaluate_keyframed_value_ease_in() {
        // Two keyframes: 0→100 with EaseIn interpolation
        let kfs = vec![
            NumericKeyframe {
                time_ns: 0,
                value: 0.0,
                interpolation: KeyframeInterpolation::EaseIn,
                bezier_controls: None,
            },
            NumericKeyframe {
                time_ns: 1_000_000_000,
                value: 100.0,
                interpolation: KeyframeInterpolation::Linear,
                bezier_controls: None,
            },
        ];
        // At t=0.5 (500ms), EaseIn should give a value less than 50 (slow start)
        let v = Clip::evaluate_keyframed_value(&kfs, 500_000_000, 0.0);
        assert!(v < 50.0, "EaseIn at midpoint should be < 50, got {}", v);
        assert!(v > 0.0, "EaseIn at midpoint should be > 0, got {}", v);
    }

    #[test]
    fn test_evaluate_keyframed_value_ease_out() {
        let kfs = vec![
            NumericKeyframe {
                time_ns: 0,
                value: 0.0,
                interpolation: KeyframeInterpolation::EaseOut,
                bezier_controls: None,
            },
            NumericKeyframe {
                time_ns: 1_000_000_000,
                value: 100.0,
                interpolation: KeyframeInterpolation::Linear,
                bezier_controls: None,
            },
        ];
        // At t=0.5, EaseOut should give a value greater than 50 (fast start)
        let v = Clip::evaluate_keyframed_value(&kfs, 500_000_000, 0.0);
        assert!(v > 50.0, "EaseOut at midpoint should be > 50, got {}", v);
        assert!(v < 100.0, "EaseOut at midpoint should be < 100, got {}", v);
    }

    #[test]
    fn test_linear_interpolation_unchanged() {
        // Confirm linear still works as before (no regression)
        let kfs = vec![
            NumericKeyframe {
                time_ns: 0,
                value: 0.0,
                interpolation: KeyframeInterpolation::Linear,
                bezier_controls: None,
            },
            NumericKeyframe {
                time_ns: 1_000_000_000,
                value: 100.0,
                interpolation: KeyframeInterpolation::Linear,
                bezier_controls: None,
            },
        ];
        let v = Clip::evaluate_keyframed_value(&kfs, 500_000_000, 0.0);
        assert!(
            (v - 50.0).abs() < 0.01,
            "Linear at midpoint should be 50, got {}",
            v
        );
    }

    #[test]
    fn test_interp_serde_round_trip() {
        // Verify serde rename_all = "snake_case" for new variants
        let json = serde_json::to_string(&KeyframeInterpolation::EaseIn).unwrap();
        assert_eq!(json, "\"ease_in\"");
        let json = serde_json::to_string(&KeyframeInterpolation::EaseOut).unwrap();
        assert_eq!(json, "\"ease_out\"");
        let json = serde_json::to_string(&KeyframeInterpolation::EaseInOut).unwrap();
        assert_eq!(json, "\"ease_in_out\"");

        // Round-trip
        let de: KeyframeInterpolation = serde_json::from_str("\"ease_in_out\"").unwrap();
        assert_eq!(de, KeyframeInterpolation::EaseInOut);
    }

    #[test]
    fn test_remove_all_phase1_keyframes_at_local_ns() {
        let mut clip = make_test_clip(4_000_000_000, 0);
        clip.scale_keyframes.push(NumericKeyframe {
            time_ns: 1_000_000_000,
            value: 1.2,
            interpolation: KeyframeInterpolation::Linear,
            bezier_controls: None,
        });
        clip.opacity_keyframes.push(NumericKeyframe {
            time_ns: 1_000_000_000,
            value: 0.8,
            interpolation: KeyframeInterpolation::EaseOut,
            bezier_controls: None,
        });
        clip.position_x_keyframes.push(NumericKeyframe {
            time_ns: 2_000_000_000,
            value: 0.3,
            interpolation: KeyframeInterpolation::EaseIn,
            bezier_controls: None,
        });

        let removed = clip.remove_all_phase1_keyframes_at_local_ns(1_000_000_000);
        assert_eq!(removed, 2);
        assert!(clip.scale_keyframes.is_empty());
        assert!(clip.opacity_keyframes.is_empty());
        assert_eq!(clip.position_x_keyframes.len(), 1);
    }

    #[test]
    fn test_set_phase1_keyframe_interpolation_at_local_ns() {
        let mut clip = make_test_clip(4_000_000_000, 0);
        clip.scale_keyframes.push(NumericKeyframe {
            time_ns: 500_000_000,
            value: 1.1,
            interpolation: KeyframeInterpolation::Linear,
            bezier_controls: None,
        });
        clip.volume_keyframes.push(NumericKeyframe {
            time_ns: 500_000_000,
            value: 0.6,
            interpolation: KeyframeInterpolation::Linear,
            bezier_controls: None,
        });

        let updated = clip.set_phase1_keyframe_interpolation_at_local_ns(
            500_000_000,
            KeyframeInterpolation::EaseInOut,
        );
        assert_eq!(updated, 2);
        assert_eq!(
            clip.scale_keyframes[0].interpolation,
            KeyframeInterpolation::EaseInOut
        );
        assert_eq!(
            clip.volume_keyframes[0].interpolation,
            KeyframeInterpolation::EaseInOut
        );
    }

    #[test]
    fn test_move_all_phase1_keyframes_local_ns_overwrites_destination() {
        let mut clip = make_test_clip(5_000_000_000, 0);
        clip.scale_keyframes.push(NumericKeyframe {
            time_ns: 1_000_000_000,
            value: 1.4,
            interpolation: KeyframeInterpolation::EaseOut,
            bezier_controls: None,
        });
        clip.scale_keyframes.push(NumericKeyframe {
            time_ns: 2_000_000_000,
            value: 2.2,
            interpolation: KeyframeInterpolation::Linear,
            bezier_controls: None,
        });
        clip.opacity_keyframes.push(NumericKeyframe {
            time_ns: 1_000_000_000,
            value: 0.75,
            interpolation: KeyframeInterpolation::EaseIn,
            bezier_controls: None,
        });

        let moved = clip.move_all_phase1_keyframes_local_ns(&[(1_000_000_000, 2_000_000_000)]);
        assert_eq!(moved, 2);
        assert_eq!(clip.scale_keyframes.len(), 1);
        assert_eq!(clip.scale_keyframes[0].time_ns, 2_000_000_000);
        assert!((clip.scale_keyframes[0].value - 1.4).abs() < 1e-9);
        assert_eq!(clip.opacity_keyframes[0].time_ns, 2_000_000_000);
    }

    #[test]
    fn test_timeline_to_source_delta_speed_1() {
        let clip = make_test_clip(10_000_000_000, 0);
        assert_eq!(clip.timeline_to_source_delta(1_000_000_000), 1_000_000_000);
        assert_eq!(clip.timeline_to_source_delta(-500_000_000), -500_000_000);
    }

    #[test]
    fn test_timeline_to_source_delta_speed_2() {
        let mut clip = make_test_clip(10_000_000_000, 0);
        clip.speed = 2.0;
        // At 2x speed, 1s on timeline = 2s in source
        assert_eq!(clip.timeline_to_source_delta(1_000_000_000), 2_000_000_000);
        assert_eq!(clip.timeline_to_source_delta(-1_000_000_000), -2_000_000_000);
    }

    #[test]
    fn test_timeline_to_source_dur_half_speed() {
        let mut clip = make_test_clip(10_000_000_000, 0);
        clip.speed = 0.5;
        // At 0.5x speed, 2s on timeline = 1s in source
        assert_eq!(clip.timeline_to_source_dur(2_000_000_000), 1_000_000_000);
    }

    #[test]
    fn test_clamp_source_out_with_media_duration() {
        let mut clip = make_test_clip(10_000_000_000, 0);
        clip.media_duration_ns = Some(5_000_000_000);
        clip.clamp_source_out();
        assert_eq!(clip.source_out, 5_000_000_000);
    }

    #[test]
    fn test_clamp_source_out_without_media_duration() {
        let mut clip = make_test_clip(10_000_000_000, 0);
        // No media_duration_ns — clamp is a no-op
        clip.clamp_source_out();
        assert_eq!(clip.source_out, 10_000_000_000);
    }

    #[test]
    fn test_clamp_source_out_already_within_bounds() {
        let mut clip = make_test_clip(3_000_000_000, 0);
        clip.media_duration_ns = Some(5_000_000_000);
        clip.clamp_source_out();
        assert_eq!(clip.source_out, 3_000_000_000);
    }

    #[test]
    fn test_max_source_out() {
        let mut clip = make_test_clip(10_000_000_000, 0);
        assert_eq!(clip.max_source_out(), None);
        clip.media_duration_ns = Some(8_000_000_000);
        assert_eq!(clip.max_source_out(), Some(8_000_000_000));
    }

    #[test]
    fn test_is_image_file() {
        assert!(is_image_file("photo.png"));
        assert!(is_image_file("/path/to/PHOTO.PNG"));
        assert!(is_image_file("image.jpg"));
        assert!(is_image_file("image.jpeg"));
        assert!(is_image_file("image.gif"));
        assert!(is_image_file("image.bmp"));
        assert!(is_image_file("image.tiff"));
        assert!(is_image_file("image.tif"));
        assert!(is_image_file("image.webp"));
        assert!(is_image_file("image.heic"));
        assert!(is_image_file("image.heif"));
        assert!(is_image_file("image.svg"));
        assert!(!is_image_file("video.mp4"));
        assert!(!is_image_file("audio.mp3"));
        assert!(!is_image_file("file.txt"));
        assert!(!is_image_file(""));
    }

    #[test]
    fn test_image_clip_max_source_out_always_none() {
        let mut clip = Clip::new("photo.png", 4_000_000_000, 0, ClipKind::Image);
        clip.media_duration_ns = Some(4_000_000_000);
        // Image clips should always return None for max_source_out
        // so they can be extended to any length.
        assert_eq!(clip.max_source_out(), None);
    }

    #[test]
    fn test_image_clip_duration_uses_source_range() {
        let mut clip = Clip::new("photo.png", 4_000_000_000, 0, ClipKind::Image);
        clip.source_in = 0;
        clip.source_out = 4_000_000_000;
        assert_eq!(clip.duration(), 4_000_000_000);

        // Extending the clip by moving source_out increases the duration
        clip.source_out = 10_000_000_000;
        assert_eq!(clip.duration(), 10_000_000_000);
    }

    #[test]
    fn test_speed_keyframes_constant_matches_scalar() {
        // A clip with speed_keyframes all at 2× should produce the same
        // duration as clip.speed = 2.0.
        let src_dur: u64 = 10_000_000_000; // 10s
        let mut clip = make_test_clip(src_dur, 0);
        clip.speed = 2.0;
        let expected = clip.duration(); // 5s

        let mut kf_clip = make_test_clip(src_dur, 0);
        kf_clip.speed = 2.0;
        kf_clip.speed_keyframes = vec![
            NumericKeyframe {
                time_ns: 0,
                value: 2.0,
                interpolation: KeyframeInterpolation::Linear,
                bezier_controls: None,
            },
            NumericKeyframe {
                time_ns: src_dur, // far enough out
                value: 2.0,
                interpolation: KeyframeInterpolation::Linear,
                bezier_controls: None,
            },
        ];
        let actual = kf_clip.duration();
        // Allow 1% tolerance due to numerical integration
        let tolerance = (expected as f64 * 0.01) as u64;
        assert!(
            actual.abs_diff(expected) <= tolerance,
            "constant 2× keyframes: expected ~{expected}, got {actual}"
        );
    }

    #[test]
    fn test_speed_keyframes_ramp_duration_satisfies_integral() {
        // A 1× → 2× linear ramp over a 10s source clip.
        // Mean speed ≈ 1.5×, so timeline duration ≈ 10/1.5 ≈ 6.67s.
        let src_dur: u64 = 10_000_000_000;
        let mut clip = make_test_clip(src_dur, 0);
        clip.speed = 1.0;
        clip.speed_keyframes = vec![
            NumericKeyframe {
                time_ns: 0,
                value: 1.0,
                interpolation: KeyframeInterpolation::Linear,
                bezier_controls: None,
            },
            NumericKeyframe {
                time_ns: 20_000_000_000, // place second KF far out so ramp spans the clip
                value: 2.0,
                interpolation: KeyframeInterpolation::Linear,
                bezier_controls: None,
            },
        ];
        let dur = clip.duration();
        // Verify: integrating speed over [0, dur] should ≈ source_duration
        let integrated = clip.integrated_source_distance_for_local_timeline_ns(dur);
        let src = src_dur as f64;
        let error = (integrated - src).abs() / src;
        assert!(
            error < 0.02,
            "integral over computed duration should ≈ source_duration, got {integrated} vs {src} (error {error:.4})"
        );
    }

    #[test]
    fn test_speed_keyframes_slow_ramp_longer_than_1x() {
        // A 1× → 0.5× ramp should produce a longer timeline duration than 1×.
        let src_dur: u64 = 10_000_000_000;
        let mut clip = make_test_clip(src_dur, 0);
        clip.speed = 1.0;
        clip.speed_keyframes = vec![
            NumericKeyframe {
                time_ns: 0,
                value: 1.0,
                interpolation: KeyframeInterpolation::Linear,
                bezier_controls: None,
            },
            NumericKeyframe {
                time_ns: 20_000_000_000,
                value: 0.5,
                interpolation: KeyframeInterpolation::Linear,
                bezier_controls: None,
            },
        ];
        let dur = clip.duration();
        assert!(
            dur > src_dur,
            "slow ramp should produce longer duration: {dur} vs {src_dur}"
        );
    }

    #[test]
    fn test_speed_keyframes_empty_uses_constant_speed() {
        // No speed keyframes → should use clip.speed as before
        let src_dur: u64 = 10_000_000_000;
        let mut clip = make_test_clip(src_dur, 0);
        clip.speed = 2.0;
        assert_eq!(clip.duration(), 5_000_000_000);
    }

    #[test]
    fn test_speed_keyframes_determine_duration() {
        // 120s of source material, base speed 1.0 → clip duration = 120s
        let src_dur: u64 = 120_000_000_000;
        let mut clip = make_test_clip(src_dur, 0);
        clip.speed = 1.0;
        assert_eq!(clip.duration(), 120_000_000_000);

        // Adding a 4x keyframe should shorten the clip (source consumed faster).
        clip.upsert_phase1_keyframe_at_timeline_ns_with_interp(
            Phase1KeyframeProperty::Speed,
            10_000_000_000,
            1.0,
            KeyframeInterpolation::Linear,
        );
        clip.upsert_phase1_keyframe_at_timeline_ns_with_interp(
            Phase1KeyframeProperty::Speed,
            20_000_000_000,
            4.0,
            KeyframeInterpolation::Linear,
        );
        let dur_with_ramp = clip.duration();
        assert!(dur_with_ramp < 120_000_000_000, "4x ramp should shorten clip");

        // Adding a third keyframe that ramps back to 1x changes the curve
        // and thus the duration. The keyframe is placed wherever the user
        // puts the playhead — it is NOT clamped or pruned.
        clip.upsert_phase1_keyframe_at_timeline_ns_with_interp(
            Phase1KeyframeProperty::Speed,
            90_000_000_000,
            1.0,
            KeyframeInterpolation::Linear,
        );
        assert_eq!(clip.speed_keyframes.len(), 3, "all 3 keyframes should be kept");
        // Duration changes because the speed curve changed.
        let dur_with_three = clip.duration();
        assert!(dur_with_three > 0);
        assert_ne!(dur_with_three, dur_with_ramp, "3rd KF should change duration");
    }
}
