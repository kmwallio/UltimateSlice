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
}

fn default_width() -> i32 { 960 }
fn default_height() -> i32 { 540 }

impl Default for ProgramMonitorState {
    fn default() -> Self {
        Self { popped: false, width: default_width(), height: default_height() }
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

#[derive(Clone, Debug, Serialize, Deserialize)]
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
}

impl Default for PreferencesState {
    fn default() -> Self {
        Self {
            hardware_acceleration_enabled: false,
            playback_priority: PlaybackPriority::default(),
            proxy_mode: ProxyMode::default(),
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
