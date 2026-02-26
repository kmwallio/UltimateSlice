use std::collections::{HashMap, HashSet};
use std::sync::mpsc;

/// Result of a background media probe.
pub struct ProbeResult {
    pub path: String,
    pub duration_ns: u64,
    pub is_audio_only: bool,
}

/// Asynchronous media probe cache.
///
/// Follows the same pattern as `ThumbnailCache` / `WaveformCache`:
/// 1. Call `request(path)` to start a background GStreamer Discoverer probe.
/// 2. Call `poll()` periodically to drain completed results.
/// 3. Call `get(path)` to retrieve a finished probe result.
pub struct MediaProbeCache {
    pub results: HashMap<String, ProbeResult>,
    loading: HashSet<String>,
    tx: mpsc::SyncSender<ProbeResult>,
    rx: mpsc::Receiver<ProbeResult>,
}

impl MediaProbeCache {
    pub fn new() -> Self {
        let (tx, rx) = mpsc::sync_channel(32);
        Self {
            results: HashMap::new(),
            loading: HashSet::new(),
            tx,
            rx,
        }
    }

    /// Start a background probe for `source_path`. No-op if already cached or pending.
    pub fn request(&mut self, source_path: &str) {
        if self.results.contains_key(source_path) || self.loading.contains(source_path) {
            return;
        }
        self.loading.insert(source_path.to_string());
        let tx = self.tx.clone();
        let path = source_path.to_string();
        std::thread::spawn(move || {
            let uri = format!("file://{path}");
            let duration_ns = probe_duration_bg(&uri).unwrap_or(10 * 1_000_000_000);
            let is_audio_only = probe_is_audio_only_bg(&uri);
            let _ = tx.send(ProbeResult {
                path,
                duration_ns,
                is_audio_only,
            });
        });
    }

    /// Drain completed background probes. Returns paths that were just resolved.
    pub fn poll(&mut self) -> Vec<String> {
        let mut resolved = Vec::new();
        while let Ok(result) = self.rx.try_recv() {
            self.loading.remove(&result.path);
            let path = result.path.clone();
            self.results.insert(result.path.clone(), result);
            resolved.push(path);
        }
        resolved
    }

    /// Get a completed probe result, if available.
    pub fn get(&self, source_path: &str) -> Option<&ProbeResult> {
        self.results.get(source_path)
    }
}

fn probe_duration_bg(uri: &str) -> Option<u64> {
    use gstreamer_pbutils::Discoverer;
    gstreamer::init().ok()?;
    let discoverer = Discoverer::new(gstreamer::ClockTime::from_seconds(5)).ok()?;
    let info = discoverer.discover_uri(uri).ok()?;
    info.duration().map(|d| d.nseconds())
}

fn probe_is_audio_only_bg(uri: &str) -> bool {
    use gstreamer_pbutils::prelude::*;
    use gstreamer_pbutils::Discoverer;
    let Ok(()) = gstreamer::init() else { return false };
    let Ok(discoverer) = Discoverer::new(gstreamer::ClockTime::from_seconds(5)) else { return false };
    let Ok(info) = discoverer.discover_uri(uri) else { return false };
    info.video_streams().is_empty()
}
