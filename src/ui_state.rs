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
}

fn default_width() -> i32 { 960 }
fn default_height() -> i32 { 540 }
fn default_docked_split_pos() -> i32 { 420 }

impl Default for ProgramMonitorState {
    fn default() -> Self {
        Self {
            popped: false,
            width: default_width(),
            height: default_height(),
            docked_split_pos: default_docked_split_pos(),
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
    fn default() -> Self { Self::Smooth }
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
    fn default() -> Self { Self::Auto }
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
    fn default() -> Self { Self::Full }
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
    fn default() -> Self { Self::Off }
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

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PreferencesState {
    #[serde(default)]
    pub hardware_acceleration_enabled: bool,
    #[serde(default)]
    pub playback_priority: PlaybackPriority,
    #[serde(default)]
    pub proxy_mode: ProxyMode,
    /// Show audio waveforms overlaid on video clips in the timeline.
    #[serde(default)]
    pub show_waveform_on_video: bool,
    /// Enable the MCP Unix-domain-socket server so agents can connect to this instance.
    #[serde(default)]
    pub mcp_socket_enabled: bool,
    /// GTK renderer backend (requires restart to take effect).
    #[serde(default)]
    pub gsk_renderer: GskRenderer,
    /// Compositor output quality for preview playback.
    #[serde(default)]
    pub preview_quality: PreviewQuality,
}

impl Default for PreferencesState {
    fn default() -> Self {
        Self {
            hardware_acceleration_enabled: false,
            playback_priority: PlaybackPriority::default(),
            proxy_mode: ProxyMode::default(),
            show_waveform_on_video: false,
            mcp_socket_enabled: false,
            gsk_renderer: GskRenderer::default(),
            preview_quality: PreviewQuality::default(),
        }
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
struct UiState {
    #[serde(default)]
    program_monitor: ProgramMonitorState,
    #[serde(default)]
    preferences: PreferencesState,
}

fn config_path() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))?;
    Some(base.join("ultimateslice").join("ui-state.json"))
}

fn load_ui_state() -> UiState {
    let path = match config_path() { Some(p) => p, None => return UiState::default() };
    let text = match std::fs::read_to_string(path) { Ok(t) => t, Err(_) => return UiState::default() };
    serde_json::from_str::<UiState>(&text).unwrap_or_default()
}

fn save_ui_state(ui: &UiState) {
    let path = match config_path() { Some(p) => p, None => return };
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
