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

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
struct UiState {
    #[serde(default)]
    program_monitor: ProgramMonitorState,
}

fn config_path() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))?;
    Some(base.join("ultimateslice").join("ui-state.json"))
}

pub fn load_program_monitor_state() -> ProgramMonitorState {
    let path = match config_path() { Some(p) => p, None => return ProgramMonitorState::default() };
    let text = match std::fs::read_to_string(path) { Ok(t) => t, Err(_) => return ProgramMonitorState::default() };
    serde_json::from_str::<UiState>(&text)
        .map(|s| s.program_monitor)
        .unwrap_or_default()
}

pub fn save_program_monitor_state(state: &ProgramMonitorState) {
    let path = match config_path() { Some(p) => p, None => return };
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let ui = UiState { program_monitor: state.clone() };
    if let Ok(json) = serde_json::to_string_pretty(&ui) {
        let _ = std::fs::write(path, json);
    }
}
