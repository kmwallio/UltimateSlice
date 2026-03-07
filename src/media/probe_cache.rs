use std::collections::{HashMap, HashSet};
use std::sync::mpsc;

/// Result of a background media probe.
pub struct ProbeResult {
    pub path: String,
    pub duration_ns: u64,
    pub is_audio_only: bool,
    #[allow(dead_code)]
    pub has_audio: bool,
    pub source_timecode_base_ns: Option<u64>,
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
                let (duration_ns, is_audio_only, has_audio, source_timecode_base_ns) =
                    probe_media_bg(&uri);
                if tx
                    .send(ProbeResult {
                        path,
                        duration_ns,
                        is_audio_only,
                        has_audio,
                        source_timecode_base_ns,
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

/// Single Discoverer call that returns duration, audio-only flag, has-audio flag, and timecode.
fn probe_media_bg(uri: &str) -> (u64, bool, bool, Option<u64>) {
    use gstreamer_pbutils::Discoverer;
    let fallback = (10 * 1_000_000_000, false, true, None);
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
    let source_timecode_base_ns = extract_timecode_ns(&info);
    (duration_ns, is_audio_only, has_audio, source_timecode_base_ns)
}

/// Extract time-of-day nanoseconds from GStreamer Discoverer tags.
/// Checks GST_TAG_DATE_TIME first, which contains creation date/time for
/// most camera files. Returns time-of-day (not epoch) for multi-cam sync.
fn extract_timecode_ns(info: &gstreamer_pbutils::DiscovererInfo) -> Option<u64> {
    let tags = info.tags()?;
    // Try GST_TAG_DATE_TIME (most camera files have this)
    if let Some(dt) = tags.get::<gstreamer::tags::DateTime>().map(|v| v.get()) {
        let hour = dt.hour()? as u64;
        let minute = dt.minute()? as u64;
        let second = dt.second()? as u64;
        let microsecond = dt.microsecond().unwrap_or(0) as u64;
        let ns = hour * 3_600_000_000_000
            + minute * 60_000_000_000
            + second * 1_000_000_000
            + microsecond * 1_000;
        return Some(ns);
    }
    None
}
