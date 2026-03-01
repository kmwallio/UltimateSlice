/// Persistent recent-projects list stored in
/// `~/.config/ultimateslice/recent.json` as a JSON array of absolute paths.
use std::path::PathBuf;

const MAX_RECENT: usize = 10;

fn config_path() -> Option<PathBuf> {
    // Honour $XDG_CONFIG_HOME, fall back to ~/.config
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))?;
    Some(base.join("ultimateslice").join("recent.json"))
}

/// Load the recent projects list (most-recent first). Returns an empty list on any error.
pub fn load() -> Vec<String> {
    let path = match config_path() {
        Some(p) => p,
        None => return Vec::new(),
    };
    let text = match std::fs::read_to_string(&path) {
        Ok(t) => t,
        Err(_) => return Vec::new(),
    };
    serde_json::from_str::<Vec<String>>(&text).unwrap_or_default()
}

/// Push `path` to the front of the recent list and persist it.
/// Duplicate entries are removed; the list is capped at MAX_RECENT.
pub fn push(path: &str) {
    let config_path = match config_path() {
        Some(p) => p,
        None => return,
    };
    let mut entries = load();
    entries.retain(|p| p != path);
    entries.insert(0, path.to_string());
    entries.truncate(MAX_RECENT);
    if let Some(parent) = config_path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(json) = serde_json::to_string_pretty(&entries) {
        let _ = std::fs::write(&config_path, json);
    }
}
