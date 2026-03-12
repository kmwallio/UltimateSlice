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

    // Get video frame rate for timecode conversion.
    let video_fps = info
        .video_streams()
        .first()
        .map(|vs| {
            let fr = vs.framerate();
            (fr.numer() as u32, fr.denom() as u32)
        })
        .unwrap_or((24, 1));

    // Prefer the embedded timecode track (required by FCP) over creation
    // date/time which is only useful for multi-cam sync.
    let file_path = uri.strip_prefix("file://").unwrap_or(uri);
    let source_timecode_base_ns = extract_embedded_timecode(file_path, video_fps.0, video_fps.1)
        .or_else(|| extract_creation_time_ns(&info));

    (
        duration_ns,
        is_audio_only,
        has_audio,
        source_timecode_base_ns,
    )
}

/// Extract the embedded timecode from a media file's timecode track.
///
/// Uses ffprobe to read the timecode tag from the video stream, then converts
/// the HH:MM:SS:FF string to nanoseconds using the video frame rate.
/// FCP requires the asset `start` to match this embedded timecode.
fn extract_embedded_timecode(path: &str, fps_num: u32, fps_den: u32) -> Option<u64> {
    use std::process::Command;

    let output = Command::new("ffprobe")
        .args([
            "-v",
            "quiet",
            "-select_streams",
            "v:0",
            "-show_entries",
            "stream_tags=timecode",
            "-of",
            "csv=p=0",
            path,
        ])
        .output()
        .ok()?;

    if !output.status.success() {
        return None;
    }

    let tc_string = String::from_utf8_lossy(&output.stdout);
    let tc_string = tc_string.trim();
    if tc_string.is_empty() {
        return None;
    }

    parse_timecode_to_ns(tc_string, fps_num, fps_den)
}

/// Parse a timecode string (HH:MM:SS:FF or HH:MM:SS;FF) to nanoseconds.
fn parse_timecode_to_ns(tc: &str, fps_num: u32, fps_den: u32) -> Option<u64> {
    let tc = tc.replace(';', ":");
    let parts: Vec<&str> = tc.split(':').collect();
    if parts.len() != 4 {
        return None;
    }

    let hours: u64 = parts[0].parse().ok()?;
    let minutes: u64 = parts[1].parse().ok()?;
    let seconds: u64 = parts[2].parse().ok()?;
    let frames: u64 = parts[3].parse().ok()?;

    // Nominal fps for timecode frame counting (e.g. 24 for 23.976fps).
    let nominal_fps = (fps_num as u64 + fps_den as u64 - 1) / fps_den as u64;

    let total_frames =
        hours * 3600 * nominal_fps + minutes * 60 * nominal_fps + seconds * nominal_fps + frames;

    // Convert frames to nanoseconds: frames × fps_den × 1e9 / fps_num
    Some(total_frames * fps_den as u64 * 1_000_000_000 / fps_num as u64)
}

/// Extract time-of-day nanoseconds from GStreamer creation date/time tags.
/// Fallback when no embedded timecode track is present.
fn extract_creation_time_ns(info: &gstreamer_pbutils::DiscovererInfo) -> Option<u64> {
    let tags = info.tags()?;
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_timecode_to_ns_ntsc_23976() {
        // GoPro timecode 20:13:33:07 at 23.976fps (24000/1001)
        let ns = parse_timecode_to_ns("20:13:33:07", 24000, 1001).unwrap();
        // Expected frames: 20*3600*24 + 13*60*24 + 33*24 + 7 = 1,747,519
        // Expected ns: 1747519 * 1001 * 1e9 / 24000 = 72886104958333
        assert_eq!(ns, 72_886_104_958_333);
    }

    #[test]
    fn test_parse_timecode_to_ns_integer_24fps() {
        let ns = parse_timecode_to_ns("01:00:00:00", 24, 1).unwrap();
        // 1 hour = 3600 * 24 = 86400 frames at 24fps = 3600 seconds
        assert_eq!(ns, 3_600_000_000_000);
    }

    #[test]
    fn test_parse_timecode_drop_frame_separator() {
        // DF timecodes use ; separator — should still parse
        let ns = parse_timecode_to_ns("00:00:01;00", 30000, 1001).unwrap();
        // 30 frames at 29.97fps = 30 * 1001 * 1e9 / 30000 = 1001000000
        assert_eq!(ns, 1_001_000_000);
    }

    #[test]
    fn test_parse_timecode_invalid() {
        assert!(parse_timecode_to_ns("invalid", 24000, 1001).is_none());
        assert!(parse_timecode_to_ns("00:00:00", 24000, 1001).is_none());
    }
}
