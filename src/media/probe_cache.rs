use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::mpsc;

/// Shared media metadata resolved from a probe pass.
#[derive(Debug, Clone, Default)]
pub struct MediaProbeMetadata {
    pub duration_ns: Option<u64>,
    pub is_audio_only: bool,
    pub has_audio: bool,
    pub source_timecode_base_ns: Option<u64>,
    pub is_image: bool,
    pub is_animated_svg: bool,
    pub video_width: Option<u32>,
    pub video_height: Option<u32>,
    pub frame_rate_num: Option<u32>,
    pub frame_rate_den: Option<u32>,
    pub codec_summary: Option<String>,
    pub file_size_bytes: Option<u64>,
}

/// Result of a background media probe.
pub struct ProbeResult {
    pub path: String,
    pub duration_ns: u64,
    pub is_audio_only: bool,
    pub has_audio: bool,
    pub source_timecode_base_ns: Option<u64>,
    /// True when the file is a still image (PNG, JPEG, etc.).
    pub is_image: bool,
    /// True when the file is an animated SVG that should be treated as animated media.
    pub is_animated_svg: bool,
    pub video_width: Option<u32>,
    pub video_height: Option<u32>,
    pub frame_rate_num: Option<u32>,
    pub frame_rate_den: Option<u32>,
    pub codec_summary: Option<String>,
    pub file_size_bytes: Option<u64>,
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
                let metadata = probe_media_metadata(&path);
                if tx
                    .send(ProbeResult {
                        path,
                        duration_ns: metadata.duration_ns.unwrap_or(10 * 1_000_000_000),
                        is_audio_only: metadata.is_audio_only,
                        has_audio: metadata.has_audio,
                        source_timecode_base_ns: metadata.source_timecode_base_ns,
                        is_image: metadata.is_image,
                        is_animated_svg: metadata.is_animated_svg,
                        video_width: metadata.video_width,
                        video_height: metadata.video_height,
                        frame_rate_num: metadata.frame_rate_num,
                        frame_rate_den: metadata.frame_rate_den,
                        codec_summary: metadata.codec_summary,
                        file_size_bytes: metadata.file_size_bytes,
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

/// Default duration for still-image clips: 4 seconds.
const IMAGE_DEFAULT_DURATION_NS: u64 = 4_000_000_000;

/// Probe duration + stream characteristics in one pass.
pub fn probe_media_metadata(path: &str) -> MediaProbeMetadata {
    let uri = format!("file://{path}");
    let is_image = crate::model::clip::is_image_file(path);
    let file_size_bytes = std::fs::metadata(path).ok().map(|m| m.len());

    // For still images, skip the Discoverer entirely — images have no
    // meaningful duration or audio streams.
    if is_image {
        let (video_width, video_height) = ffprobe_dimensions(path);
        if crate::model::clip::is_svg_file(path) {
            if let Ok(analysis) = crate::media::animated_svg::analyze_svg_path(path) {
                if analysis.is_animated {
                    return MediaProbeMetadata {
                        duration_ns: Some(
                            analysis.duration_ns.unwrap_or(IMAGE_DEFAULT_DURATION_NS),
                        ),
                        is_image: true,
                        is_animated_svg: true,
                        video_width,
                        video_height,
                        codec_summary: Some("Animated SVG".to_string()),
                        file_size_bytes,
                        ..MediaProbeMetadata::default()
                    };
                }
            }
        }
        return MediaProbeMetadata {
            duration_ns: Some(IMAGE_DEFAULT_DURATION_NS),
            is_image: true,
            video_width,
            video_height,
            codec_summary: image_codec_summary(path),
            file_size_bytes,
            ..MediaProbeMetadata::default()
        };
    }

    use gstreamer_pbutils::Discoverer;
    let fallback = MediaProbeMetadata {
        duration_ns: Some(10 * 1_000_000_000),
        has_audio: true,
        file_size_bytes,
        ..MediaProbeMetadata::default()
    };
    let Ok(()) = gstreamer::init() else {
        return fallback;
    };
    let Ok(discoverer) = Discoverer::new(gstreamer::ClockTime::from_seconds(5)) else {
        return fallback;
    };
    let Ok(info) = discoverer.discover_uri(&uri) else {
        return fallback;
    };
    let duration_ns = info.duration().map(|d| d.nseconds());
    let video_streams = info.video_streams();
    let is_audio_only = video_streams.is_empty();
    let has_audio = !info.audio_streams().is_empty();
    let video_stream = video_streams.first();
    let (video_width, video_height) = video_stream
        .map(|vs| (Some(vs.width()), Some(vs.height())))
        .unwrap_or((None, None));

    // Get video frame rate for timecode conversion.
    let (frame_rate_num, frame_rate_den) = video_stream
        .map(|vs| {
            let fr = vs.framerate();
            (fr.numer() as u32, fr.denom() as u32)
        })
        .unwrap_or((24, 1));

    // Prefer the embedded timecode track (required by FCP) over creation
    // date/time which is only useful for multi-cam sync.
    let file_path = uri.strip_prefix("file://").unwrap_or(&uri);
    let source_timecode_base_ns =
        extract_embedded_timecode(file_path, frame_rate_num, frame_rate_den)
            .or_else(|| extract_creation_time_ns(&info));

    MediaProbeMetadata {
        duration_ns,
        is_audio_only,
        has_audio,
        source_timecode_base_ns,
        video_width,
        video_height,
        frame_rate_num: video_stream.map(|_| frame_rate_num),
        frame_rate_den: video_stream.map(|_| frame_rate_den),
        codec_summary: ffprobe_codec_summary(path),
        file_size_bytes,
        ..MediaProbeMetadata::default()
    }
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

fn image_codec_summary(path: &str) -> Option<String> {
    let ext = Path::new(path)
        .extension()
        .and_then(|ext| ext.to_str())
        .map(|ext| ext.to_ascii_lowercase())?;
    let label = match ext.as_str() {
        "jpg" | "jpeg" => "JPEG",
        "png" => "PNG",
        "gif" => "GIF",
        "bmp" => "BMP",
        "tif" | "tiff" => "TIFF",
        "webp" => "WebP",
        "heic" => "HEIC",
        "svg" => "SVG",
        _ => return None,
    };
    Some(label.to_string())
}

fn ffprobe_dimensions(path: &str) -> (Option<u32>, Option<u32>) {
    use std::process::Command;

    let output = match Command::new("ffprobe")
        .args([
            "-v",
            "quiet",
            "-show_entries",
            "stream=width,height",
            "-of",
            "csv=p=0",
            path,
        ])
        .output()
    {
        Ok(output) if output.status.success() => output,
        _ => return (None, None),
    };

    let stdout = String::from_utf8_lossy(&output.stdout);
    let Some(line) = stdout.lines().next() else {
        return (None, None);
    };
    let mut parts = line.split(',');
    let width = parts
        .next()
        .and_then(|part| part.trim().parse::<u32>().ok());
    let height = parts
        .next()
        .and_then(|part| part.trim().parse::<u32>().ok());
    (width, height)
}

fn ffprobe_codec_summary(path: &str) -> Option<String> {
    use std::process::Command;

    let output = Command::new("ffprobe")
        .args([
            "-v",
            "quiet",
            "-show_entries",
            "stream=codec_type,codec_name",
            "-of",
            "csv=p=0",
            path,
        ])
        .output()
        .ok()?;

    if !output.status.success() {
        return None;
    }

    let mut video_codec: Option<String> = None;
    let mut audio_codec: Option<String> = None;
    for line in String::from_utf8_lossy(&output.stdout).lines() {
        let mut parts = line.splitn(2, ',');
        let Some(codec_type) = parts.next() else {
            continue;
        };
        let Some(codec_name) = parts.next() else {
            continue;
        };
        let codec_name = codec_name.trim();
        if codec_name.is_empty() {
            continue;
        }
        match codec_type.trim() {
            "video" if video_codec.is_none() => {
                video_codec = Some(pretty_codec_name(codec_name));
            }
            "audio" if audio_codec.is_none() => {
                audio_codec = Some(pretty_codec_name(codec_name));
            }
            _ => {}
        }
    }

    match (video_codec, audio_codec) {
        (Some(video), Some(audio)) => Some(format!("{video} / {audio}")),
        (Some(video), None) => Some(video),
        (None, Some(audio)) => Some(audio),
        (None, None) => None,
    }
}

fn pretty_codec_name(codec: &str) -> String {
    match codec.to_ascii_lowercase().as_str() {
        "h264" => "H.264".to_string(),
        "hevc" | "h265" => "H.265/HEVC".to_string(),
        "prores" => "ProRes".to_string(),
        "mpeg4" => "MPEG-4".to_string(),
        "vp8" => "VP8".to_string(),
        "vp9" => "VP9".to_string(),
        "av1" => "AV1".to_string(),
        "aac" => "AAC".to_string(),
        "mp3" => "MP3".to_string(),
        "flac" => "FLAC".to_string(),
        "pcm_s16le" | "pcm_s24le" | "pcm_s32le" | "pcm_f32le" => "PCM".to_string(),
        "opus" => "Opus".to_string(),
        "vorbis" => "Vorbis".to_string(),
        "alac" => "ALAC".to_string(),
        other => other.to_ascii_uppercase(),
    }
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

    #[test]
    fn test_pretty_codec_name() {
        assert_eq!(pretty_codec_name("h264"), "H.264");
        assert_eq!(pretty_codec_name("aac"), "AAC");
        assert_eq!(pretty_codec_name("pcm_s24le"), "PCM");
    }

    #[test]
    fn test_image_codec_summary() {
        assert_eq!(
            image_codec_summary("/tmp/example.jpeg").as_deref(),
            Some("JPEG")
        );
        assert_eq!(
            image_codec_summary("/tmp/example.SVG").as_deref(),
            Some("SVG")
        );
        assert_eq!(image_codec_summary("/tmp/example.unknown"), None);
    }

    #[test]
    fn test_ffprobe_dimensions_empty_output() {
        assert_eq!(ffprobe_dimensions("/definitely/missing/file"), (None, None));
    }
}
