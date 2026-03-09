use crate::media::export::{AudioCodec, Container, ExportOptions, VideoCodec};
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
    pub show_safe_areas: bool,
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

impl Default for ProgramMonitorState {
    fn default() -> Self {
        Self {
            popped: false,
            width: default_width(),
            height: default_height(),
            docked_split_pos: default_docked_split_pos(),
            show_safe_areas: false,
        }
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
        }
    }

    pub fn to_container(&self) -> Container {
        match self {
            Self::Mp4 => Container::Mp4,
            Self::Mov => Container::Mov,
            Self::WebM => Container::WebM,
            Self::Mkv => Container::Mkv,
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
        },
    ]
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
    /// Show audio waveforms overlaid on video clips in the timeline.
    #[serde(default)]
    pub show_waveform_on_video: bool,
    /// Show thumbnail preview strips on timeline video clips.
    #[serde(default = "default_show_timeline_preview")]
    pub show_timeline_preview: bool,
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
}

impl Default for PreferencesState {
    fn default() -> Self {
        Self {
            hardware_acceleration_enabled: false,
            playback_priority: PlaybackPriority::default(),
            source_playback_priority: PlaybackPriority::default(),
            proxy_mode: ProxyMode::default(),
            show_waveform_on_video: false,
            show_timeline_preview: default_show_timeline_preview(),
            show_track_audio_levels: default_show_track_audio_levels(),
            mcp_socket_enabled: false,
            gsk_renderer: GskRenderer::default(),
            preview_quality: PreviewQuality::default(),
            experimental_preview_optimizations: false,
            realtime_preview: false,
            background_prerender: false,
            preview_luts: false,
            crossfade_enabled: false,
            crossfade_curve: CrossfadeCurve::default(),
            crossfade_duration_ns: default_crossfade_duration_ns(),
        }
    }
}

fn default_show_timeline_preview() -> bool {
    true
}

fn default_show_track_audio_levels() -> bool {
    true
}

fn default_crossfade_duration_ns() -> u64 {
    200_000_000
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
struct UiState {
    #[serde(default)]
    program_monitor: ProgramMonitorState,
    #[serde(default)]
    preferences: PreferencesState,
    #[serde(default)]
    export_presets: ExportPresetsState,
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
        assert!(!parsed.preferences.crossfade_enabled);
        assert_eq!(
            parsed.preferences.crossfade_curve,
            CrossfadeCurve::EqualPower
        );
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
    fn preferences_crossfade_curve_serde_uses_snake_case_values() {
        let decoded: PreferencesState = serde_json::from_str(
            r#"{"crossfade_enabled":true,"crossfade_curve":"equal_power","crossfade_duration_ns":220000000}"#,
        )
        .unwrap();
        assert_eq!(decoded.crossfade_curve, CrossfadeCurve::EqualPower);
        let encoded = serde_json::to_string(&decoded).unwrap();
        assert!(encoded.contains(r#""crossfade_curve":"equal_power""#));
    }
}
