use std::collections::{HashMap, HashSet};
use std::sync::mpsc;

/// Result of a background media probe.
pub struct ProbeResult {
    pub path: String,
    pub duration_ns: u64,
    pub is_audio_only: bool,
    #[allow(dead_code)]
    pub has_audio: bool,
}

/// Asynchronous media probe cache.
///
/// Uses a **single** background worker thread with a queue to serialise
/// GStreamer Discoverer calls, avoiding resource contention with the
/// playback pipeline and thumbnail extraction threads.
///
/// 1. Call `request(path)` to enqueue a probe.
/// 2. Call `poll()` periodically to drain completed results.
/// 3. Call `get(path)` to retrieve a finished probe result.
pub struct MediaProbeCache {
    pub results: HashMap<String, ProbeResult>,
    pending: HashSet<String>,
    result_rx: mpsc::Receiver<ProbeResult>,
    work_tx: Option<mpsc::Sender<String>>,
}

impl MediaProbeCache {
    pub fn new() -> Self {
        let (result_tx, result_rx) = mpsc::sync_channel(32);
        let (work_tx, work_rx) = mpsc::channel::<String>();

        // Single dedicated worker thread processes probes one at a time.
        let tx = result_tx.clone();
        std::thread::spawn(move || {
            while let Ok(path) = work_rx.recv() {
                let uri = format!("file://{path}");
                let (duration_ns, is_audio_only, has_audio) = probe_media_bg(&uri);
                if tx
                    .send(ProbeResult {
                        path,
                        duration_ns,
                        is_audio_only,
                        has_audio,
                    })
                    .is_err()
                {
                    break;
                }
            }
        });

        Self {
            results: HashMap::new(),
            pending: HashSet::new(),
            result_rx,
            work_tx: Some(work_tx),
        }
    }

    /// Enqueue a background probe for `source_path`. No-op if already cached or pending.
    pub fn request(&mut self, source_path: &str) {
        if self.results.contains_key(source_path) || self.pending.contains(source_path) {
            return;
        }
        self.pending.insert(source_path.to_string());
        if let Some(ref tx) = self.work_tx {
            let _ = tx.send(source_path.to_string());
        }
    }

    /// Drain completed background probes. Returns paths that were just resolved.
    pub fn poll(&mut self) -> Vec<String> {
        let mut resolved = Vec::new();
        while let Ok(result) = self.result_rx.try_recv() {
            self.pending.remove(&result.path);
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

/// Single Discoverer call that returns duration, audio-only flag, and has-audio flag.
fn probe_media_bg(uri: &str) -> (u64, bool, bool) {
    use gstreamer_pbutils::Discoverer;
    let fallback = (10 * 1_000_000_000, false, true);
    let Ok(()) = gstreamer::init() else {
        return fallback;
    };
    let Ok(discoverer) = Discoverer::new(gstreamer::ClockTime::from_seconds(5)) else {
        return fallback;
    };
    let Ok(info) = discoverer.discover_uri(uri) else {
        return fallback;
    };
    let duration_ns = info.duration().map(|d| d.nseconds()).unwrap_or(fallback.0);
    let is_audio_only = info.video_streams().is_empty();
    let has_audio = !info.audio_streams().is_empty();
    (duration_ns, is_audio_only, has_audio)
}
