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
            let (duration_ns, is_audio_only) = probe_media_bg(&uri);
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

/// Single Discoverer call that returns both duration and audio-only flag.
fn probe_media_bg(uri: &str) -> (u64, bool) {
    use gstreamer_pbutils::prelude::*;
    use gstreamer_pbutils::Discoverer;
    let fallback = (10 * 1_000_000_000, false);
    let Ok(()) = gstreamer::init() else { return fallback };
    let Ok(discoverer) = Discoverer::new(gstreamer::ClockTime::from_seconds(5)) else { return fallback };
    let Ok(info) = discoverer.discover_uri(uri) else { return fallback };
    let duration_ns = info.duration().map(|d| d.nseconds()).unwrap_or(fallback.0);
    let is_audio_only = info.video_streams().is_empty();
    (duration_ns, is_audio_only)
}
