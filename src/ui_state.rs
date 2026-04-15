use crate::media::export::{AudioChannelLayout, AudioCodec, Container, ExportOptions, VideoCodec};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ProgramMonitorState {
    #[serde(default)]
    pub popped: bool,
    #[serde(default = "default_width")]
    pub width: i32,
    #[serde(default = "default_height")]
    pub height: i32,
    #[serde(default = "default_docked_split_pos")]
    pub docked_split_pos: i32,
    #[serde(default)]
    pub scopes_visible: bool,
    #[serde(default)]
    pub show_safe_areas: bool,
    /// Show false-color luminance overlay on the Program Monitor.
    #[serde(default)]
    pub show_false_color: bool,
    /// Show zebra-stripe overexposure overlay on the Program Monitor.
    #[serde(default)]
    pub show_zebra: bool,
    /// Luminance threshold for zebra stripes (0.0–1.0, default 0.90 = 90 IRE).
    #[serde(default = "default_zebra_threshold")]
    pub zebra_threshold: f64,
}

fn default_zebra_threshold() -> f64 {
    0.90
}

fn default_width() -> i32 {
    960
}
fn default_height() -> i32 {
    540
}
fn default_docked_split_pos() -> i32 {
    420
}

fn default_root_hpaned_pos() -> i32 {
    1120
}

fn default_root_vpaned_pos() -> i32 {
    520
}

fn default_top_paned_pos() -> i32 {
    320
}

fn default_left_vpaned_pos() -> i32 {
    320
}

fn default_timeline_paned_pos() -> i32 {
    0
}

fn default_right_sidebar_paned_pos() -> i32 {
    580
}

const WORKSPACE_SPLIT_RATIO_SCALE: i32 = 1000;

fn default_workspace_panel_visible() -> bool {
    true
}

pub fn workspace_split_ratio_from_pixels(position: i32, total: i32) -> Option<u16> {
    if total <= 0 {
        return None;
    }
    let clamped = position.clamp(0, total) as i64;
    let scaled =
        ((clamped * WORKSPACE_SPLIT_RATIO_SCALE as i64) + (total as i64 / 2)) / total as i64;
    Some(scaled.clamp(0, WORKSPACE_SPLIT_RATIO_SCALE as i64) as u16)
}

pub fn workspace_split_position_from_ratio(
    ratio_permille: Option<u16>,
    total: i32,
    fallback_position: i32,
) -> i32 {
    if total <= 0 {
        return fallback_position.max(0);
    }
    match ratio_permille {
        Some(ratio) => {
            let scaled = ((ratio as i64 * total as i64) + (WORKSPACE_SPLIT_RATIO_SCALE as i64 / 2))
                / WORKSPACE_SPLIT_RATIO_SCALE as i64;
            scaled.clamp(0, total as i64) as i32
        }
        None => fallback_position.max(0),
    }
}

impl Default for ProgramMonitorState {
    fn default() -> Self {
        Self {
            popped: false,
            width: default_width(),
            height: default_height(),
            docked_split_pos: default_docked_split_pos(),
            scopes_visible: false,
            show_safe_areas: false,
            show_false_color: false,
            show_zebra: false,
            zebra_threshold: default_zebra_threshold(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkspaceLeftPanelTab {
    Media,
    Effects,
    AudioEffects,
    Titles,
}

impl Default for WorkspaceLeftPanelTab {
    fn default() -> Self {
        Self::Media
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ProgramMonitorWorkspaceState {
    #[serde(default)]
    pub popped: bool,
    #[serde(default = "default_width")]
    pub width: i32,
    #[serde(default = "default_height")]
    pub height: i32,
    #[serde(default = "default_docked_split_pos")]
    pub docked_split_pos: i32,
    #[serde(default)]
    pub scopes_visible: bool,
}

impl Default for ProgramMonitorWorkspaceState {
    fn default() -> Self {
        Self {
            popped: false,
            width: default_width(),
            height: default_height(),
            docked_split_pos: default_docked_split_pos(),
            scopes_visible: false,
        }
    }
}

impl ProgramMonitorWorkspaceState {
    pub fn from_program_monitor_state(state: &ProgramMonitorState) -> Self {
        Self {
            popped: state.popped,
            width: state.width,
            height: state.height,
            docked_split_pos: state.docked_split_pos,
            scopes_visible: state.scopes_visible,
        }
    }

    pub fn apply_to_program_monitor_state(&self, state: &mut ProgramMonitorState) {
        state.popped = self.popped;
        state.width = self.width;
        state.height = self.height;
        state.docked_split_pos = self.docked_split_pos;
        state.scopes_visible = self.scopes_visible;
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct WorkspaceArrangement {
    #[serde(default = "default_root_hpaned_pos")]
    pub root_hpaned_pos: i32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub root_hpaned_ratio_permille: Option<u16>,
    #[serde(default = "default_root_vpaned_pos")]
    pub root_vpaned_pos: i32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub root_vpaned_ratio_permille: Option<u16>,
    #[serde(default = "default_top_paned_pos")]
    pub top_paned_pos: i32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub top_paned_ratio_permille: Option<u16>,
    #[serde(default = "default_left_vpaned_pos")]
    pub left_vpaned_pos: i32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub left_vpaned_ratio_permille: Option<u16>,
    #[serde(default = "default_timeline_paned_pos")]
    pub timeline_paned_pos: i32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeline_paned_ratio_permille: Option<u16>,
    #[serde(default = "default_right_sidebar_paned_pos")]
    pub right_sidebar_paned_pos: i32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub right_sidebar_paned_ratio_permille: Option<u16>,
    #[serde(default = "default_workspace_panel_visible")]
    pub media_browser_visible: bool,
    #[serde(default = "default_workspace_panel_visible")]
    pub inspector_visible: bool,
    #[serde(default)]
    pub keyframe_editor_visible: bool,
    #[serde(default)]
    pub left_panel_tab: WorkspaceLeftPanelTab,
    #[serde(default)]
    pub minimap_visible: bool,
    #[serde(default)]
    pub program_monitor: ProgramMonitorWorkspaceState,
}

impl Default for WorkspaceArrangement {
    fn default() -> Self {
        Self {
            root_hpaned_pos: default_root_hpaned_pos(),
            root_hpaned_ratio_permille: None,
            root_vpaned_pos: default_root_vpaned_pos(),
            root_vpaned_ratio_permille: None,
            top_paned_pos: default_top_paned_pos(),
            top_paned_ratio_permille: None,
            left_vpaned_pos: default_left_vpaned_pos(),
            left_vpaned_ratio_permille: None,
            timeline_paned_pos: default_timeline_paned_pos(),
            timeline_paned_ratio_permille: None,
            right_sidebar_paned_pos: default_right_sidebar_paned_pos(),
            right_sidebar_paned_ratio_permille: None,
            media_browser_visible: default_workspace_panel_visible(),
            inspector_visible: default_workspace_panel_visible(),
            keyframe_editor_visible: false,
            left_panel_tab: WorkspaceLeftPanelTab::default(),
            minimap_visible: false,
            program_monitor: ProgramMonitorWorkspaceState::default(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct WorkspaceLayout {
    pub name: String,
    #[serde(default)]
    pub arrangement: WorkspaceArrangement,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct WorkspaceLayoutsState {
    #[serde(default)]
    pub current: WorkspaceArrangement,
    #[serde(default)]
    pub layouts: Vec<WorkspaceLayout>,
    #[serde(default)]
    pub active_layout: Option<String>,
}

impl Default for WorkspaceLayoutsState {
    fn default() -> Self {
        Self {
            current: WorkspaceArrangement::default(),
            layouts: Vec::new(),
            active_layout: None,
        }
    }
}

impl WorkspaceLayoutsState {
    fn normalize_name(name: &str) -> Option<String> {
        let trimmed = name.trim();
        if trimmed.is_empty() {
            None
        } else {
            Some(trimmed.to_string())
        }
    }

    fn is_reserved_name(name: &str) -> bool {
        matches!(
            name.to_ascii_lowercase().as_str(),
            "current" | "(current)" | "default" | "default layout"
        )
    }

    fn matching_layout_name_for(&self, arrangement: &WorkspaceArrangement) -> Option<String> {
        self.layouts
            .iter()
            .find(|layout| layout.arrangement == *arrangement)
            .map(|layout| layout.name.clone())
    }

    pub fn set_current_arrangement(&mut self, arrangement: WorkspaceArrangement) {
        self.current = arrangement;
        self.active_layout = self.matching_layout_name_for(&self.current);
    }

    pub fn upsert_layout(&mut self, mut layout: WorkspaceLayout) -> Result<(), String> {
        let normalized = Self::normalize_name(&layout.name)
            .ok_or_else(|| "Layout name cannot be empty".to_string())?;
        if Self::is_reserved_name(&normalized) {
            return Err(format!("Layout name is reserved: {normalized}"));
        }
        layout.name = normalized.clone();
        if let Some(existing) = self
            .layouts
            .iter_mut()
            .find(|entry| entry.name.eq_ignore_ascii_case(&normalized))
        {
            *existing = layout.clone();
        } else {
            self.layouts.push(layout.clone());
            self.layouts.sort_by(|a, b| {
                a.name
                    .to_ascii_lowercase()
                    .cmp(&b.name.to_ascii_lowercase())
            });
        }
        self.current = layout.arrangement;
        self.active_layout = Some(normalized);
        Ok(())
    }

    pub fn rename_layout(&mut self, old_name: &str, new_name: &str) -> Result<String, String> {
        let old_normalized = Self::normalize_name(old_name)
            .ok_or_else(|| "Existing layout name cannot be empty".to_string())?;
        let new_normalized = Self::normalize_name(new_name)
            .ok_or_else(|| "New layout name cannot be empty".to_string())?;
        if Self::is_reserved_name(&new_normalized) {
            return Err(format!("Layout name is reserved: {new_normalized}"));
        }
        if self.layouts.iter().any(|layout| {
            layout.name.eq_ignore_ascii_case(&new_normalized)
                && !layout.name.eq_ignore_ascii_case(&old_normalized)
        }) {
            return Err(format!("Layout already exists: {new_normalized}"));
        }
        let Some(existing) = self
            .layouts
            .iter_mut()
            .find(|layout| layout.name.eq_ignore_ascii_case(&old_normalized))
        else {
            return Err(format!("Workspace layout not found: {old_normalized}"));
        };
        existing.name = new_normalized.clone();
        self.layouts.sort_by(|a, b| {
            a.name
                .to_ascii_lowercase()
                .cmp(&b.name.to_ascii_lowercase())
        });
        if self
            .active_layout
            .as_deref()
            .is_some_and(|name| name.eq_ignore_ascii_case(&old_normalized))
        {
            self.active_layout = Some(new_normalized.clone());
        } else {
            self.active_layout = self.matching_layout_name_for(&self.current);
        }
        Ok(new_normalized)
    }

    pub fn delete_layout(&mut self, name: &str) -> bool {
        let Some(normalized) = Self::normalize_name(name) else {
            return false;
        };
        let before = self.layouts.len();
        self.layouts
            .retain(|layout| !layout.name.eq_ignore_ascii_case(&normalized));
        let removed = self.layouts.len() != before;
        if removed {
            self.active_layout = self.matching_layout_name_for(&self.current);
        }
        removed
    }

    pub fn get_layout(&self, name: &str) -> Option<&WorkspaceLayout> {
        let normalized = Self::normalize_name(name)?;
        self.layouts
            .iter()
            .find(|layout| layout.name.eq_ignore_ascii_case(&normalized))
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlaybackPriority {
    Smooth,
    Balanced,
    Accurate,
}

impl Default for PlaybackPriority {
    fn default() -> Self {
        Self::Smooth
    }
}

impl PlaybackPriority {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Smooth => "smooth",
            Self::Balanced => "balanced",
            Self::Accurate => "accurate",
        }
    }

    pub fn from_str(value: &str) -> Self {
        match value {
            "accurate" => Self::Accurate,
            "balanced" => Self::Balanced,
            _ => Self::Smooth,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GskRenderer {
    Auto,
    Cairo,
    Opengl,
    Vulkan,
}

impl Default for GskRenderer {
    fn default() -> Self {
        Self::Auto
    }
}

impl GskRenderer {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Cairo => "cairo",
            Self::Opengl => "opengl",
            Self::Vulkan => "vulkan",
        }
    }

    pub fn from_str(value: &str) -> Self {
        match value {
            "cairo" => Self::Cairo,
            "opengl" => Self::Opengl,
            "vulkan" => Self::Vulkan,
            _ => Self::Auto,
        }
    }

    /// Returns the value to set for the `GSK_RENDERER` env var, or `None` for Auto.
    pub fn env_value(&self) -> Option<&'static str> {
        match self {
            Self::Auto => None,
            Self::Cairo => Some("cairo"),
            Self::Opengl => Some("gl"),
            Self::Vulkan => Some("vulkan"),
        }
    }
}

/// Controls the compositor output resolution relative to project dimensions.
/// Lower quality reduces memory and CPU usage for smoother preview playback
/// on low-end hardware. Export always uses full project resolution.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PreviewQuality {
    Auto,
    Full,
    Half,
    Quarter,
}

impl Default for PreviewQuality {
    fn default() -> Self {
        Self::Full
    }
}

impl PreviewQuality {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Full => "full",
            Self::Half => "half",
            Self::Quarter => "quarter",
        }
    }

    pub fn from_str(value: &str) -> Self {
        match value {
            "auto" => Self::Auto,
            "half" => Self::Half,
            "quarter" => Self::Quarter,
            _ => Self::Full,
        }
    }

    /// Divisor applied to project width/height for the compositor output.
    pub fn divisor(&self) -> u32 {
        match self {
            Self::Auto => 1,
            Self::Full => 1,
            Self::Half => 2,
            Self::Quarter => 4,
        }
    }
}

/// Behavior for keeping the playhead visible in the timeline during playback.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum AutoScrollMode {
    /// Jump the view forward one page when the playhead passes the right edge.
    #[default]
    Page,
    /// Slide the view so the playhead stays near the right side of the viewport.
    Smooth,
    /// Do not move the view automatically.
    Off,
}

impl AutoScrollMode {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Page => "page",
            Self::Smooth => "smooth",
            Self::Off => "off",
        }
    }

    pub fn from_str(value: &str) -> Self {
        match value {
            "smooth" => Self::Smooth,
            "off" => Self::Off,
            _ => Self::Page,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PrerenderEncodingPreset {
    Ultrafast,
    Superfast,
    Veryfast,
    Faster,
    Fast,
    Medium,
}

impl Default for PrerenderEncodingPreset {
    fn default() -> Self {
        Self::Veryfast
    }
}

impl PrerenderEncodingPreset {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Ultrafast => "ultrafast",
            Self::Superfast => "superfast",
            Self::Veryfast => "veryfast",
            Self::Faster => "faster",
            Self::Fast => "fast",
            Self::Medium => "medium",
        }
    }

    pub fn from_str(value: &str) -> Self {
        match value {
            "ultrafast" => Self::Ultrafast,
            "superfast" => Self::Superfast,
            "faster" => Self::Faster,
            "fast" => Self::Fast,
            "medium" => Self::Medium,
            _ => Self::Veryfast,
        }
    }
}

pub const MIN_PRERENDER_CRF: u32 = 0;
pub const MAX_PRERENDER_CRF: u32 = 51;
pub const DEFAULT_PRERENDER_CRF: u32 = 20;

pub fn clamp_prerender_crf(value: u32) -> u32 {
    value.clamp(MIN_PRERENDER_CRF, MAX_PRERENDER_CRF)
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProxyMode {
    Off,
    HalfRes,
    QuarterRes,
}

impl Default for ProxyMode {
    fn default() -> Self {
        Self::Off
    }
}

impl ProxyMode {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::HalfRes => "half_res",
            Self::QuarterRes => "quarter_res",
        }
    }

    pub fn from_str(value: &str) -> Self {
        match value {
            "half_res" => Self::HalfRes,
            "quarter_res" => Self::QuarterRes,
            _ => Self::Off,
        }
    }

    pub fn is_enabled(&self) -> bool {
        !matches!(self, Self::Off)
    }
}

fn default_last_non_off_proxy_mode() -> ProxyMode {
    ProxyMode::HalfRes
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CrossfadeCurve {
    EqualPower,
    Linear,
}

impl Default for CrossfadeCurve {
    fn default() -> Self {
        Self::EqualPower
    }
}

impl CrossfadeCurve {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::EqualPower => "equal_power",
            Self::Linear => "linear",
        }
    }

    pub fn from_str(value: &str) -> Self {
        match value {
            "linear" => Self::Linear,
            _ => Self::EqualPower,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExportVideoCodec {
    H264,
    H265,
    Vp9,
    ProRes,
    Av1,
}

impl Default for ExportVideoCodec {
    fn default() -> Self {
        Self::H264
    }
}

impl ExportVideoCodec {
    pub fn from_video_codec(value: &VideoCodec) -> Self {
        match value {
            VideoCodec::H264 => Self::H264,
            VideoCodec::H265 => Self::H265,
            VideoCodec::Vp9 => Self::Vp9,
            VideoCodec::ProRes => Self::ProRes,
            VideoCodec::Av1 => Self::Av1,
        }
    }

    pub fn to_video_codec(&self) -> VideoCodec {
        match self {
            Self::H264 => VideoCodec::H264,
            Self::H265 => VideoCodec::H265,
            Self::Vp9 => VideoCodec::Vp9,
            Self::ProRes => VideoCodec::ProRes,
            Self::Av1 => VideoCodec::Av1,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExportContainer {
    Mp4,
    Mov,
    WebM,
    Mkv,
    Gif,
}

impl Default for ExportContainer {
    fn default() -> Self {
        Self::Mp4
    }
}

impl ExportContainer {
    pub fn from_container(value: &Container) -> Self {
        match value {
            Container::Mp4 => Self::Mp4,
            Container::Mov => Self::Mov,
            Container::WebM => Self::WebM,
            Container::Mkv => Self::Mkv,
            Container::Gif => Self::Gif,
        }
    }

    pub fn to_container(&self) -> Container {
        match self {
            Self::Mp4 => Container::Mp4,
            Self::Mov => Container::Mov,
            Self::WebM => Container::WebM,
            Self::Mkv => Container::Mkv,
            Self::Gif => Container::Gif,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExportAudioCodec {
    Aac,
    Opus,
    Flac,
    Pcm,
}

/// Serialized mirror of `AudioChannelLayout` for preset round-trip.
///
/// Has its own `Default` (Stereo) and `#[serde(default)]` so existing preset
/// JSON files (which lack this field) load unchanged.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum ExportAudioChannelLayout {
    #[default]
    Stereo,
    Surround51,
    Surround71,
}

impl ExportAudioChannelLayout {
    pub fn from_layout(layout: &AudioChannelLayout) -> Self {
        match layout {
            AudioChannelLayout::Stereo => Self::Stereo,
            AudioChannelLayout::Surround51 => Self::Surround51,
            AudioChannelLayout::Surround71 => Self::Surround71,
        }
    }

    pub fn to_layout(&self) -> AudioChannelLayout {
        match self {
            Self::Stereo => AudioChannelLayout::Stereo,
            Self::Surround51 => AudioChannelLayout::Surround51,
            Self::Surround71 => AudioChannelLayout::Surround71,
        }
    }
}

impl Default for ExportAudioCodec {
    fn default() -> Self {
        Self::Aac
    }
}

impl ExportAudioCodec {
    pub fn from_audio_codec(value: &AudioCodec) -> Self {
        match value {
            AudioCodec::Aac => Self::Aac,
            AudioCodec::Opus => Self::Opus,
            AudioCodec::Flac => Self::Flac,
            AudioCodec::Pcm => Self::Pcm,
        }
    }

    pub fn to_audio_codec(&self) -> AudioCodec {
        match self {
            Self::Aac => AudioCodec::Aac,
            Self::Opus => AudioCodec::Opus,
            Self::Flac => AudioCodec::Flac,
            Self::Pcm => AudioCodec::Pcm,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ExportPreset {
    pub name: String,
    #[serde(default)]
    pub video_codec: ExportVideoCodec,
    #[serde(default)]
    pub container: ExportContainer,
    /// 0 = use project resolution
    #[serde(default)]
    pub output_width: u32,
    /// 0 = use project resolution
    #[serde(default)]
    pub output_height: u32,
    #[serde(default = "default_export_crf")]
    pub crf: u32,
    #[serde(default)]
    pub audio_codec: ExportAudioCodec,
    #[serde(default = "default_export_audio_bitrate_kbps")]
    pub audio_bitrate_kbps: u32,
    /// Frames per second override for GIF output. None = use project frame rate.
    #[serde(default)]
    pub gif_fps: Option<u32>,
    /// Output audio channel layout. Defaults to Stereo so legacy preset JSON
    /// without this field continues to load unchanged.
    #[serde(default)]
    pub audio_channel_layout: ExportAudioChannelLayout,
}

impl ExportPreset {
    pub fn from_export_options(name: impl Into<String>, options: &ExportOptions) -> Self {
        Self {
            name: name.into(),
            video_codec: ExportVideoCodec::from_video_codec(&options.video_codec),
            container: ExportContainer::from_container(&options.container),
            output_width: options.output_width,
            output_height: options.output_height,
            crf: options.crf,
            audio_codec: ExportAudioCodec::from_audio_codec(&options.audio_codec),
            audio_bitrate_kbps: options.audio_bitrate_kbps,
            gif_fps: options.gif_fps,
            audio_channel_layout: ExportAudioChannelLayout::from_layout(
                &options.audio_channel_layout,
            ),
        }
    }

    pub fn to_export_options(&self) -> ExportOptions {
        ExportOptions {
            video_codec: self.video_codec.to_video_codec(),
            container: self.container.to_container(),
            output_width: self.output_width,
            output_height: self.output_height,
            crf: self.crf,
            audio_codec: self.audio_codec.to_audio_codec(),
            audio_bitrate_kbps: self.audio_bitrate_kbps,
            gif_fps: self.gif_fps,
            audio_channel_layout: self.audio_channel_layout.to_layout(),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ExportPresetsState {
    #[serde(default = "default_export_presets")]
    pub presets: Vec<ExportPreset>,
    #[serde(default)]
    pub last_used_preset: Option<String>,
}

impl Default for ExportPresetsState {
    fn default() -> Self {
        Self {
            presets: default_export_presets(),
            last_used_preset: None,
        }
    }
}

impl ExportPresetsState {
    fn normalize_name(name: &str) -> Option<String> {
        let trimmed = name.trim();
        if trimmed.is_empty() {
            None
        } else {
            Some(trimmed.to_string())
        }
    }

    pub fn upsert_preset(&mut self, mut preset: ExportPreset) -> Result<(), String> {
        let normalized = Self::normalize_name(&preset.name)
            .ok_or_else(|| "Preset name cannot be empty".to_string())?;
        preset.name = normalized.clone();
        if let Some(existing) = self
            .presets
            .iter_mut()
            .find(|p| p.name.eq_ignore_ascii_case(&normalized))
        {
            *existing = preset;
        } else {
            self.presets.push(preset);
            self.presets.sort_by(|a, b| {
                a.name
                    .to_ascii_lowercase()
                    .cmp(&b.name.to_ascii_lowercase())
            });
        }
        self.last_used_preset = Some(normalized);
        Ok(())
    }

    pub fn delete_preset(&mut self, name: &str) -> bool {
        let Some(normalized) = Self::normalize_name(name) else {
            return false;
        };
        let before = self.presets.len();
        self.presets
            .retain(|p| !p.name.eq_ignore_ascii_case(&normalized));
        if self
            .last_used_preset
            .as_deref()
            .is_some_and(|n| n.eq_ignore_ascii_case(&normalized))
        {
            self.last_used_preset = None;
        }
        self.presets.len() != before
    }

    pub fn get_preset(&self, name: &str) -> Option<&ExportPreset> {
        let normalized = Self::normalize_name(name)?;
        self.presets
            .iter()
            .find(|p| p.name.eq_ignore_ascii_case(&normalized))
    }
}

fn default_export_crf() -> u32 {
    23
}

fn default_export_audio_bitrate_kbps() -> u32 {
    192
}

fn default_export_presets() -> Vec<ExportPreset> {
    vec![
        ExportPreset {
            name: "Web H.264 1080p".to_string(),
            video_codec: ExportVideoCodec::H264,
            container: ExportContainer::Mp4,
            output_width: 1920,
            output_height: 1080,
            crf: 23,
            audio_codec: ExportAudioCodec::Aac,
            audio_bitrate_kbps: 192,
            gif_fps: None,
            audio_channel_layout: ExportAudioChannelLayout::Stereo,
        },
        ExportPreset {
            name: "High Quality H.264 4K".to_string(),
            video_codec: ExportVideoCodec::H264,
            container: ExportContainer::Mp4,
            output_width: 3840,
            output_height: 2160,
            crf: 18,
            audio_codec: ExportAudioCodec::Aac,
            audio_bitrate_kbps: 320,
            gif_fps: None,
            audio_channel_layout: ExportAudioChannelLayout::Stereo,
        },
        ExportPreset {
            name: "Archive ProRes 4K".to_string(),
            video_codec: ExportVideoCodec::ProRes,
            container: ExportContainer::Mov,
            output_width: 3840,
            output_height: 2160,
            crf: 18,
            audio_codec: ExportAudioCodec::Pcm,
            audio_bitrate_kbps: 320,
            gif_fps: None,
            audio_channel_layout: ExportAudioChannelLayout::Stereo,
        },
        ExportPreset {
            name: "WebM VP9 1080p".to_string(),
            video_codec: ExportVideoCodec::Vp9,
            container: ExportContainer::WebM,
            output_width: 1920,
            output_height: 1080,
            crf: 30,
            audio_codec: ExportAudioCodec::Opus,
            audio_bitrate_kbps: 160,
            gif_fps: None,
            audio_channel_layout: ExportAudioChannelLayout::Stereo,
        },
        ExportPreset {
            name: "Animated GIF".to_string(),
            video_codec: ExportVideoCodec::H264,
            container: ExportContainer::Gif,
            output_width: 640,
            output_height: 0,
            crf: 23,
            audio_codec: ExportAudioCodec::Aac,
            audio_bitrate_kbps: 128,
            gif_fps: Some(15),
            audio_channel_layout: ExportAudioChannelLayout::Stereo,
        },
        // Cinema-style 5.1 surround at 1080p / 448 kbps AAC. Auto-routes
        // dialogue to Front Center, music to Front L/R, and effects to
        // Front L/R + Surround L/R, with an automatic LFE bass tap.
        ExportPreset {
            name: "Cinema H.264 5.1 1080p".to_string(),
            video_codec: ExportVideoCodec::H264,
            container: ExportContainer::Mp4,
            output_width: 1920,
            output_height: 1080,
            crf: 20,
            audio_codec: ExportAudioCodec::Aac,
            audio_bitrate_kbps: 448,
            gif_fps: None,
            audio_channel_layout: ExportAudioChannelLayout::Surround51,
        },
    ]
}

// ── Batch Export Queue ─────────────────────────────────────────────────────

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExportQueueJobStatus {
    Pending,
    Running,
    Done,
    Error,
}

impl Default for ExportQueueJobStatus {
    fn default() -> Self {
        Self::Pending
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ExportQueueJob {
    /// Unique identifier for the job (random hex string).
    pub id: String,
    /// Human-readable label derived from the output file name.
    pub label: String,
    /// Absolute path for the output file.
    pub output_path: String,
    /// Export settings snapshot.
    pub options: ExportPreset,
    #[serde(default)]
    pub status: ExportQueueJobStatus,
    /// Error message when status == Error.
    #[serde(default)]
    pub error: Option<String>,
}

impl ExportQueueJob {
    /// Create a new pending job, deriving the label from the output file name.
    pub fn new(output_path: impl Into<String>, options: ExportPreset) -> Self {
        let output_path = output_path.into();
        let label = std::path::Path::new(&output_path)
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or(&output_path)
            .to_string();
        let id = format!(
            "{:x}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .subsec_nanos()
        );
        Self {
            id,
            label,
            output_path,
            options,
            status: ExportQueueJobStatus::Pending,
            error: None,
        }
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct ExportQueueState {
    #[serde(default)]
    pub jobs: Vec<ExportQueueJob>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PreferencesState {
    #[serde(default)]
    pub hardware_acceleration_enabled: bool,
    #[serde(default)]
    pub playback_priority: PlaybackPriority,
    #[serde(default)]
    pub source_playback_priority: PlaybackPriority,
    #[serde(default)]
    pub proxy_mode: ProxyMode,
    /// Last non-Off proxy mode, used by the quick proxy toggle to restore the
    /// user's preferred proxy quality.
    #[serde(default = "default_last_non_off_proxy_mode")]
    pub last_non_off_proxy_mode: ProxyMode,
    /// Mirror/preserve proxy files in `UltimateSlice.cache/` next to source media.
    #[serde(default = "default_persist_proxies_next_to_original_media")]
    pub persist_proxies_next_to_original_media: bool,
    /// Show audio waveforms overlaid on video clips in the timeline.
    #[serde(default)]
    pub show_waveform_on_video: bool,
    /// Show thumbnail preview strips on timeline video clips.
    #[serde(default = "default_show_timeline_preview")]
    pub show_timeline_preview: bool,
    /// How the timeline view follows the playhead during playback.
    #[serde(default)]
    pub timeline_autoscroll: AutoScrollMode,
    /// Auto-link source placements and timeline drops into paired video+audio clips when possible.
    #[serde(default = "default_source_monitor_auto_link_av")]
    pub source_monitor_auto_link_av: bool,
    /// Show per-track audio levels in timeline track labels.
    #[serde(default = "default_show_track_audio_levels")]
    pub show_track_audio_levels: bool,
    /// Enable the MCP Unix-domain-socket server so agents can connect to this instance.
    #[serde(default)]
    pub mcp_socket_enabled: bool,
    /// GTK renderer backend (requires restart to take effect).
    #[serde(default)]
    pub gsk_renderer: GskRenderer,
    /// Compositor output quality for preview playback.
    #[serde(default)]
    pub preview_quality: PreviewQuality,
    /// Enable experimental preview optimizations (e.g. skip video decode for occluded clips).
    #[serde(default)]
    pub experimental_preview_optimizations: bool,
    /// Pre-build upcoming decoder slots so clip transitions are near-instant.
    /// Uses more CPU and memory during playback.
    #[serde(default)]
    pub realtime_preview: bool,
    /// Prewarm upcoming playback boundaries earlier in the background.
    /// Uses more CPU and memory during playback.
    #[serde(default)]
    pub background_prerender: bool,
    /// Automatically build transcript search data for eligible library items
    /// in the background when speech-to-text support is available.
    #[serde(default)]
    pub background_ai_indexing: bool,
    /// FFmpeg x264 preset used for background prerender video segments.
    #[serde(default)]
    pub prerender_preset: PrerenderEncodingPreset,
    /// FFmpeg x264 CRF used for background prerender video segments.
    #[serde(default = "default_prerender_crf")]
    pub prerender_crf: u32,
    /// Preserve prerender cache files beside saved project files instead of using
    /// a temporary-only cache root.
    #[serde(default = "default_persist_prerenders_next_to_project_file")]
    pub persist_prerenders_next_to_project_file: bool,
    /// Pre-render LUT-assigned clips at project resolution for preview use when proxy mode is Off.
    #[serde(default)]
    pub preview_luts: bool,
    /// Enable automatic audio crossfades at timeline edit points.
    #[serde(default)]
    pub crossfade_enabled: bool,
    /// Audio crossfade curve shape used for automatic fades.
    #[serde(default)]
    pub crossfade_curve: CrossfadeCurve,
    /// Audio crossfade duration in nanoseconds.
    #[serde(default = "default_crossfade_duration_ns")]
    pub crossfade_duration_ns: u64,
    /// Enable automatic audio ducking (lower music when dialogue is present).
    #[serde(default)]
    pub duck_enabled: bool,
    /// Ducking volume reduction in dB (negative). Default −6.0.
    #[serde(default = "default_duck_amount_db")]
    pub duck_amount_db: f64,
    /// Enable versioned auto-backups to $XDG_DATA_HOME/ultimateslice/backups/.
    #[serde(default = "default_backup_enabled")]
    pub backup_enabled: bool,
    /// Maximum number of versioned backups per project title.
    #[serde(default = "default_backup_max_versions")]
    pub backup_max_versions: usize,
    /// Loudness Radar target preset id. One of: `"ebu_r128"` | `"atsc_a85"`
    /// | `"netflix"` | `"apple_pod"` | `"streaming"` | `"custom"`.
    #[serde(default = "default_loudness_target_preset")]
    pub loudness_target_preset: String,
    /// Loudness Radar target integrated LUFS value. Mirrors the preset
    /// when a preset is selected; carries the custom value otherwise.
    #[serde(default = "default_loudness_target_lufs")]
    pub loudness_target_lufs: f64,
    /// Soft cap on the per-user voice-enhance prerender cache disk
    /// usage in GiB. The cache evicts least-recently-modified files
    /// once this is exceeded. Default 2 GiB; raise it for projects
    /// with many long enhanced clips, lower it on small disks.
    #[serde(default = "default_voice_enhance_cache_cap_gib")]
    pub voice_enhance_cache_cap_gib: f64,
    /// ONNX Runtime execution provider used for all AI inference
    /// (background removal, frame interpolation, music generation,
    /// and — when enabled — SAM segmentation). Stored as a stable
    /// string id: `"auto"` | `"cuda"` | `"rocm"` | `"openvino"` |
    /// `"cpu"`. Unknown values load as `"auto"`. This is kept as a
    /// plain string rather than a typed enum so that `ui_state`
    /// remains independent of the `ai-inference` feature gate and
    /// older preference files load cleanly on newer builds without
    /// type-migration churn.
    #[serde(default = "default_ai_backend")]
    pub ai_backend: String,
    /// Show the timeline mini-map overview strip above the track canvas.
    #[serde(default)]
    pub show_timeline_minimap: bool,
}

fn default_ai_backend() -> String {
    "auto".to_string()
}

impl Default for PreferencesState {
    fn default() -> Self {
        Self {
            hardware_acceleration_enabled: false,
            playback_priority: PlaybackPriority::default(),
            source_playback_priority: PlaybackPriority::default(),
            proxy_mode: ProxyMode::default(),
            last_non_off_proxy_mode: default_last_non_off_proxy_mode(),
            persist_proxies_next_to_original_media: default_persist_proxies_next_to_original_media(
            ),
            show_waveform_on_video: false,
            show_timeline_preview: default_show_timeline_preview(),
            timeline_autoscroll: AutoScrollMode::default(),
            source_monitor_auto_link_av: default_source_monitor_auto_link_av(),
            show_track_audio_levels: default_show_track_audio_levels(),
            mcp_socket_enabled: false,
            gsk_renderer: GskRenderer::default(),
            preview_quality: PreviewQuality::default(),
            experimental_preview_optimizations: false,
            realtime_preview: true,
            background_prerender: false,
            background_ai_indexing: false,
            prerender_preset: PrerenderEncodingPreset::default(),
            prerender_crf: default_prerender_crf(),
            persist_prerenders_next_to_project_file:
                default_persist_prerenders_next_to_project_file(),
            preview_luts: false,
            crossfade_enabled: false,
            crossfade_curve: CrossfadeCurve::default(),
            crossfade_duration_ns: default_crossfade_duration_ns(),
            duck_enabled: false,
            duck_amount_db: default_duck_amount_db(),
            backup_enabled: default_backup_enabled(),
            backup_max_versions: default_backup_max_versions(),
            loudness_target_preset: default_loudness_target_preset(),
            loudness_target_lufs: default_loudness_target_lufs(),
            voice_enhance_cache_cap_gib: default_voice_enhance_cache_cap_gib(),
            ai_backend: default_ai_backend(),
            show_timeline_minimap: false,
        }
    }
}

impl PreferencesState {
    pub fn remembered_proxy_mode(&self) -> ProxyMode {
        if self.last_non_off_proxy_mode.is_enabled() {
            self.last_non_off_proxy_mode.clone()
        } else {
            default_last_non_off_proxy_mode()
        }
    }

    pub fn set_proxy_mode(&mut self, mode: ProxyMode) {
        if mode.is_enabled() {
            self.last_non_off_proxy_mode = mode.clone();
        } else if !self.last_non_off_proxy_mode.is_enabled() {
            self.last_non_off_proxy_mode = default_last_non_off_proxy_mode();
        }
        self.proxy_mode = mode;
    }

    pub fn set_proxy_enabled(&mut self, enabled: bool) {
        let mode = if enabled {
            self.remembered_proxy_mode()
        } else {
            ProxyMode::Off
        };
        self.set_proxy_mode(mode);
    }

    pub fn set_prerender_quality(&mut self, preset: PrerenderEncodingPreset, crf: u32) {
        self.prerender_preset = preset;
        self.prerender_crf = clamp_prerender_crf(crf);
    }
}

fn default_show_timeline_preview() -> bool {
    true
}

fn default_persist_proxies_next_to_original_media() -> bool {
    true
}

fn default_persist_prerenders_next_to_project_file() -> bool {
    true
}

fn default_prerender_crf() -> u32 {
    DEFAULT_PRERENDER_CRF
}

fn default_show_track_audio_levels() -> bool {
    true
}

fn default_source_monitor_auto_link_av() -> bool {
    false
}

fn default_crossfade_duration_ns() -> u64 {
    200_000_000
}
fn default_duck_amount_db() -> f64 {
    -6.0
}
fn default_voice_enhance_cache_cap_gib() -> f64 {
    2.0
}

fn default_loudness_target_preset() -> String {
    "ebu_r128".to_string()
}
fn default_loudness_target_lufs() -> f64 {
    -23.0
}

/// Resolve a Loudness Radar target preset id to its canonical LUFS value.
/// Returns `None` for unknown or `"custom"` ids so the caller can fall
/// back to the stored `loudness_target_lufs`.
pub fn loudness_target_preset_to_lufs(preset_id: &str) -> Option<f64> {
    match preset_id {
        "ebu_r128" => Some(-23.0),
        "atsc_a85" => Some(-24.0),
        "netflix" => Some(-27.0),
        "apple_pod" => Some(-16.0),
        "streaming" => Some(-14.0),
        _ => None,
    }
}

/// Human-readable label for a Loudness Radar target preset id.
pub fn loudness_target_preset_label(preset_id: &str) -> &'static str {
    match preset_id {
        "ebu_r128" => "EBU R128 (−23 LUFS)",
        "atsc_a85" => "ATSC A/85 (−24 LUFS)",
        "netflix" => "Netflix (−27 LUFS)",
        "apple_pod" => "Apple Podcasts (−16 LUFS)",
        "streaming" => "Streaming (−14 LUFS)",
        "custom" => "Custom",
        _ => "Custom",
    }
}
fn default_backup_enabled() -> bool {
    true
}
fn default_backup_max_versions() -> usize {
    20
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
struct UiState {
    #[serde(default)]
    program_monitor: ProgramMonitorState,
    #[serde(default)]
    preferences: PreferencesState,
    #[serde(default)]
    export_presets: ExportPresetsState,
    #[serde(default)]
    export_queue: ExportQueueState,
    #[serde(default)]
    workspace_layouts: WorkspaceLayoutsState,
}

fn config_path() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))?;
    Some(base.join("ultimateslice").join("ui-state.json"))
}

fn load_ui_state() -> UiState {
    let path = match config_path() {
        Some(p) => p,
        None => return UiState::default(),
    };
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(_) => return UiState::default(),
    };
    serde_json::from_str::<UiState>(&text).unwrap_or_default()
}

fn save_ui_state(ui: &UiState) {
    let path = match config_path() {
        Some(p) => p,
        None => return,
    };
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(json) = serde_json::to_string_pretty(ui) {
        let _ = std::fs::write(path, json);
    }
}

pub fn load_program_monitor_state() -> ProgramMonitorState {
    load_ui_state().program_monitor
}

pub fn save_program_monitor_state(state: &ProgramMonitorState) {
    let mut ui = load_ui_state();
    ui.program_monitor = state.clone();
    save_ui_state(&ui);
}

pub fn load_preferences_state() -> PreferencesState {
    load_ui_state().preferences
}

pub fn save_preferences_state(state: &PreferencesState) {
    let mut ui = load_ui_state();
    ui.preferences = state.clone();
    save_ui_state(&ui);
}

pub fn load_export_presets_state() -> ExportPresetsState {
    load_ui_state().export_presets
}

pub fn save_export_presets_state(state: &ExportPresetsState) {
    let mut ui = load_ui_state();
    ui.export_presets = state.clone();
    save_ui_state(&ui);
}

pub fn load_export_queue_state() -> ExportQueueState {
    load_ui_state().export_queue
}

pub fn save_export_queue_state(state: &ExportQueueState) {
    let mut ui = load_ui_state();
    ui.export_queue = state.clone();
    save_ui_state(&ui);
}

pub fn load_workspace_layouts_state() -> WorkspaceLayoutsState {
    let ui = load_ui_state();
    let mut state = ui.workspace_layouts;
    if state.layouts.is_empty()
        && state.active_layout.is_none()
        && state.current == WorkspaceArrangement::default()
    {
        state.current.program_monitor =
            ProgramMonitorWorkspaceState::from_program_monitor_state(&ui.program_monitor);
    }
    state
}

pub fn save_workspace_layouts_state(state: &WorkspaceLayoutsState) {
    let mut ui = load_ui_state();
    ui.workspace_layouts = state.clone();
    save_ui_state(&ui);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn export_preset_round_trip_to_export_options() {
        let options = ExportOptions {
            video_codec: VideoCodec::Av1,
            container: Container::Mkv,
            output_width: 1920,
            output_height: 1080,
            crf: 18,
            audio_codec: AudioCodec::Opus,
            audio_bitrate_kbps: 256,
            gif_fps: None,
            audio_channel_layout: AudioChannelLayout::Surround51,
        };
        let preset = ExportPreset::from_export_options("High Quality", &options);
        assert_eq!(preset.name, "High Quality");
        assert_eq!(preset.to_export_options().video_codec, VideoCodec::Av1);
        assert_eq!(preset.to_export_options().container, Container::Mkv);
        assert_eq!(preset.to_export_options().output_width, 1920);
        assert_eq!(preset.to_export_options().output_height, 1080);
        assert_eq!(preset.to_export_options().crf, 18);
        assert_eq!(preset.to_export_options().audio_codec, AudioCodec::Opus);
        assert_eq!(preset.to_export_options().audio_bitrate_kbps, 256);
        assert_eq!(
            preset.to_export_options().audio_channel_layout,
            AudioChannelLayout::Surround51
        );
    }

    /// Back-compat regression: existing JSON preset files predate the
    /// `audio_channel_layout` field. They must still load and default to Stereo.
    #[test]
    fn export_preset_deserializes_legacy_json_without_audio_channel_layout_as_stereo() {
        let legacy_json = r#"{
            "name": "Legacy",
            "video_codec": "h264",
            "container": "mp4",
            "output_width": 1920,
            "output_height": 1080,
            "crf": 23,
            "audio_codec": "aac",
            "audio_bitrate_kbps": 192,
            "gif_fps": null
        }"#;
        let preset: ExportPreset =
            serde_json::from_str(legacy_json).expect("legacy JSON should still load");
        assert_eq!(
            preset.audio_channel_layout,
            ExportAudioChannelLayout::Stereo
        );
        assert_eq!(
            preset.to_export_options().audio_channel_layout,
            AudioChannelLayout::Stereo
        );
    }

    #[test]
    fn export_preset_round_trip_preserves_surround_5_1() {
        let options = ExportOptions {
            audio_channel_layout: AudioChannelLayout::Surround51,
            ..ExportOptions::default()
        };
        let preset = ExportPreset::from_export_options("Cinema", &options);
        let json = serde_json::to_string(&preset).expect("serialize");
        let parsed: ExportPreset = serde_json::from_str(&json).expect("round-trip deserialize");
        assert_eq!(
            parsed.audio_channel_layout,
            ExportAudioChannelLayout::Surround51
        );
        assert_eq!(
            parsed.to_export_options().audio_channel_layout,
            AudioChannelLayout::Surround51
        );
    }

    #[test]
    fn export_presets_upsert_and_delete() {
        let mut state = ExportPresetsState {
            presets: Vec::new(),
            last_used_preset: None,
        };
        let preset = ExportPreset::from_export_options(" Social ", &ExportOptions::default());
        state.upsert_preset(preset).unwrap();
        assert_eq!(state.presets.len(), 1);
        assert_eq!(state.presets[0].name, "Social");
        let mut opts = ExportOptions::default();
        opts.crf = 16;
        state
            .upsert_preset(ExportPreset::from_export_options("social", &opts))
            .unwrap();
        assert_eq!(state.presets.len(), 1);
        assert_eq!(state.presets[0].crf, 16);
        assert!(state.delete_preset("SOCIAL"));
        assert!(state.presets.is_empty());
        assert!(state.last_used_preset.is_none());
    }

    #[test]
    fn ui_state_defaults_missing_export_presets_field() {
        let parsed: UiState =
            serde_json::from_str(r#"{"preferences":{"hardware_acceleration_enabled":true}}"#)
                .unwrap();
        let names: Vec<&str> = parsed
            .export_presets
            .presets
            .iter()
            .map(|preset| preset.name.as_str())
            .collect();
        assert_eq!(
            names,
            vec![
                "Web H.264 1080p",
                "High Quality H.264 4K",
                "Archive ProRes 4K",
                "WebM VP9 1080p",
                "Animated GIF",
                "Cinema H.264 5.1 1080p",
            ]
        );
        assert!(parsed.export_presets.last_used_preset.is_none());
    }

    #[test]
    fn ui_state_keeps_explicit_empty_export_presets_array() {
        let parsed: UiState =
            serde_json::from_str(r#"{"export_presets":{"presets":[],"last_used_preset":null}}"#)
                .unwrap();
        assert!(parsed.export_presets.presets.is_empty());
        assert!(parsed.export_presets.last_used_preset.is_none());
    }

    #[test]
    fn preferences_defaults_missing_crossfade_fields() {
        let parsed: UiState =
            serde_json::from_str(r#"{"preferences":{"hardware_acceleration_enabled":true}}"#)
                .unwrap();
        assert!(!parsed.preferences.source_monitor_auto_link_av);
        assert!(!parsed.preferences.crossfade_enabled);
        assert!(!parsed.preferences.background_ai_indexing);
        assert_eq!(
            parsed.preferences.crossfade_curve,
            CrossfadeCurve::EqualPower
        );
        assert!(parsed.preferences.persist_proxies_next_to_original_media);
        assert!(parsed.preferences.persist_prerenders_next_to_project_file);
        assert_eq!(
            parsed.preferences.prerender_preset,
            PrerenderEncodingPreset::Veryfast
        );
        assert_eq!(parsed.preferences.prerender_crf, DEFAULT_PRERENDER_CRF);
        assert_eq!(
            parsed.preferences.crossfade_duration_ns,
            default_crossfade_duration_ns()
        );
    }

    #[test]
    fn preferences_crossfade_round_trip() {
        let prefs = PreferencesState {
            crossfade_enabled: true,
            crossfade_curve: CrossfadeCurve::Linear,
            crossfade_duration_ns: 350_000_000,
            ..PreferencesState::default()
        };
        let json = serde_json::to_string(&prefs).unwrap();
        let decoded: PreferencesState = serde_json::from_str(&json).unwrap();
        assert!(decoded.crossfade_enabled);
        assert_eq!(decoded.crossfade_curve, CrossfadeCurve::Linear);
        assert_eq!(decoded.crossfade_duration_ns, 350_000_000);
    }

    #[test]
    fn preferences_source_monitor_auto_link_round_trip() {
        let prefs = PreferencesState {
            source_monitor_auto_link_av: false,
            ..PreferencesState::default()
        };
        let json = serde_json::to_string(&prefs).unwrap();
        let decoded: PreferencesState = serde_json::from_str(&json).unwrap();
        assert!(!decoded.source_monitor_auto_link_av);
    }

    #[test]
    fn preferences_background_ai_indexing_round_trip() {
        let prefs = PreferencesState {
            background_ai_indexing: true,
            ..PreferencesState::default()
        };
        let json = serde_json::to_string(&prefs).unwrap();
        let decoded: PreferencesState = serde_json::from_str(&json).unwrap();
        assert!(decoded.background_ai_indexing);
    }

    #[test]
    fn preferences_prerender_quality_round_trip() {
        let mut prefs = PreferencesState::default();
        prefs.set_prerender_quality(PrerenderEncodingPreset::Fast, 17);
        let json = serde_json::to_string(&prefs).unwrap();
        let decoded: PreferencesState = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded.prerender_preset, PrerenderEncodingPreset::Fast);
        assert_eq!(decoded.prerender_crf, 17);
    }

    #[test]
    fn preferences_set_prerender_quality_clamps_crf() {
        let mut prefs = PreferencesState::default();
        prefs.set_prerender_quality(
            PrerenderEncodingPreset::Ultrafast,
            MAX_PRERENDER_CRF.saturating_add(10),
        );
        assert_eq!(prefs.prerender_preset, PrerenderEncodingPreset::Ultrafast);
        assert_eq!(prefs.prerender_crf, MAX_PRERENDER_CRF);
    }

    #[test]
    fn prerender_preset_serde_uses_snake_case_values() {
        let decoded: PreferencesState =
            serde_json::from_str(r#"{"prerender_preset":"superfast","prerender_crf":21}"#).unwrap();
        assert_eq!(decoded.prerender_preset, PrerenderEncodingPreset::Superfast);
        let encoded = serde_json::to_string(&decoded).unwrap();
        assert!(encoded.contains(r#""prerender_preset":"superfast""#));
    }

    #[test]
    fn preferences_crossfade_curve_serde_uses_snake_case_values() {
        let decoded: PreferencesState = serde_json::from_str(
            r#"{"crossfade_enabled":true,"crossfade_curve":"equal_power","crossfade_duration_ns":220000000}"#,
        )
        .unwrap();
        assert_eq!(decoded.crossfade_curve, CrossfadeCurve::EqualPower);
        let encoded = serde_json::to_string(&decoded).unwrap();
        assert!(encoded.contains(r#""crossfade_curve":"equal_power""#));
    }

    #[test]
    fn preferences_default_missing_proxy_restore_mode_to_half_res() {
        let parsed: UiState =
            serde_json::from_str(r#"{"preferences":{"proxy_mode":"off"}}"#).unwrap();
        assert_eq!(parsed.preferences.proxy_mode, ProxyMode::Off);
        assert_eq!(
            parsed.preferences.remembered_proxy_mode(),
            ProxyMode::HalfRes
        );
    }

    #[test]
    fn preferences_set_proxy_mode_remembers_last_enabled_mode() {
        let mut prefs = PreferencesState::default();
        prefs.set_proxy_mode(ProxyMode::QuarterRes);
        prefs.set_proxy_mode(ProxyMode::Off);
        assert_eq!(prefs.proxy_mode, ProxyMode::Off);
        assert_eq!(prefs.remembered_proxy_mode(), ProxyMode::QuarterRes);
    }

    #[test]
    fn preferences_set_proxy_enabled_restores_last_enabled_mode() {
        let mut prefs = PreferencesState::default();
        prefs.set_proxy_mode(ProxyMode::QuarterRes);
        prefs.set_proxy_enabled(false);
        prefs.set_proxy_enabled(true);
        assert_eq!(prefs.proxy_mode, ProxyMode::QuarterRes);
        assert_eq!(prefs.remembered_proxy_mode(), ProxyMode::QuarterRes);
    }

    #[test]
    fn workspace_layouts_upsert_rename_delete_tracks_active_layout() {
        let arrangement = WorkspaceArrangement {
            root_hpaned_pos: 980,
            inspector_visible: false,
            left_panel_tab: WorkspaceLeftPanelTab::Effects,
            program_monitor: ProgramMonitorWorkspaceState {
                popped: true,
                width: 1280,
                height: 720,
                docked_split_pos: 480,
                scopes_visible: true,
            },
            ..WorkspaceArrangement::default()
        };
        let mut state = WorkspaceLayoutsState::default();
        state.set_current_arrangement(arrangement.clone());
        state
            .upsert_layout(WorkspaceLayout {
                name: " Color ".to_string(),
                arrangement: arrangement.clone(),
            })
            .unwrap();
        assert_eq!(state.layouts.len(), 1);
        assert_eq!(state.layouts[0].name, "Color");
        assert_eq!(state.current, arrangement);
        assert_eq!(state.active_layout.as_deref(), Some("Color"));

        let renamed = state.rename_layout("color", "Review").unwrap();
        assert_eq!(renamed, "Review");
        assert_eq!(state.layouts[0].name, "Review");
        assert_eq!(state.active_layout.as_deref(), Some("Review"));

        assert!(state.delete_layout("review"));
        assert!(state.layouts.is_empty());
        assert!(state.active_layout.is_none());
    }

    #[test]
    fn workspace_layouts_recompute_active_layout_from_current_arrangement() {
        let mut state = WorkspaceLayoutsState::default();
        state
            .upsert_layout(WorkspaceLayout {
                name: "Edit".to_string(),
                arrangement: WorkspaceArrangement::default(),
            })
            .unwrap();
        assert_eq!(state.active_layout.as_deref(), Some("Edit"));

        let mut changed = state.current.clone();
        changed.media_browser_visible = false;
        state.set_current_arrangement(changed);
        assert!(state.active_layout.is_none());

        state.set_current_arrangement(WorkspaceArrangement::default());
        assert_eq!(state.active_layout.as_deref(), Some("Edit"));
    }

    #[test]
    fn program_monitor_workspace_state_copies_geometry_only() {
        let monitor = ProgramMonitorState {
            popped: true,
            width: 1111,
            height: 777,
            docked_split_pos: 512,
            scopes_visible: true,
            show_safe_areas: true,
            show_false_color: true,
            show_zebra: true,
            zebra_threshold: 0.95,
        };
        let workspace = ProgramMonitorWorkspaceState::from_program_monitor_state(&monitor);
        assert!(workspace.popped);
        assert_eq!(workspace.width, 1111);
        assert_eq!(workspace.height, 777);
        assert_eq!(workspace.docked_split_pos, 512);
        assert!(workspace.scopes_visible);
    }

    #[test]
    fn workspace_left_panel_tab_serde_uses_snake_case_values() {
        let arrangement: WorkspaceArrangement =
            serde_json::from_str(r#"{"left_panel_tab":"audio_effects"}"#).unwrap();
        assert_eq!(
            arrangement.left_panel_tab,
            WorkspaceLeftPanelTab::AudioEffects
        );
        let encoded = serde_json::to_string(&arrangement).unwrap();
        assert!(encoded.contains(r#""left_panel_tab":"audio_effects""#));
    }

    #[test]
    fn workspace_split_ratio_scales_between_window_sizes() {
        let ratio = workspace_split_ratio_from_pixels(1596, 2200);
        assert_eq!(ratio, Some(725));
        let scaled = workspace_split_position_from_ratio(ratio, 1440, 1596);
        assert_eq!(scaled, 1044);
        assert_eq!(workspace_split_ratio_from_pixels(scaled, 1440), ratio);
    }

    #[test]
    fn workspace_arrangement_serde_keeps_split_ratio_fields_optional() {
        let arrangement = WorkspaceArrangement::default();
        let encoded = serde_json::to_string(&arrangement).unwrap();
        assert!(!encoded.contains("ratio_permille"));

        let decoded: WorkspaceArrangement =
            serde_json::from_str(r#"{"root_hpaned_pos":1596}"#).unwrap();
        assert_eq!(decoded.root_hpaned_pos, 1596);
        assert!(decoded.root_hpaned_ratio_permille.is_none());
        assert!(decoded.root_vpaned_ratio_permille.is_none());
        assert_eq!(
            decoded.right_sidebar_paned_pos,
            default_right_sidebar_paned_pos()
        );
        assert!(decoded.right_sidebar_paned_ratio_permille.is_none());
    }

    #[test]
    fn ui_state_defaults_missing_workspace_layouts_field() {
        let parsed: UiState = serde_json::from_str(
            r#"{"program_monitor":{"popped":true,"width":1111,"height":777,"docked_split_pos":512,"scopes_visible":true}}"#,
        )
        .unwrap();
        assert!(parsed.workspace_layouts.layouts.is_empty());
        assert!(parsed.workspace_layouts.active_layout.is_none());
        assert_eq!(
            parsed.workspace_layouts.current,
            WorkspaceArrangement::default()
        );
        assert!(parsed.program_monitor.popped);
        assert!(parsed.program_monitor.scopes_visible);
    }
}
