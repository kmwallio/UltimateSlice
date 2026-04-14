//! Audio analysis utilities: loudness measurement, silence/scene detection,
//! and FFmpeg binary resolution.
//!
//! These are pure subprocess-based utilities that spawn FFmpeg for audio
//! analysis tasks. They have no GStreamer or GTK dependencies and are shared
//! by the preview engine (`program_player.rs`), export pipeline (`export.rs`),
//! UI actions (`window.rs`, `loudness_popover.rs`), and MCP server.

use anyhow::{anyhow, Result};
use std::process::{Command, Stdio};

// ── FFmpeg binary resolution ────────────────────────────────────────────

/// Find the ffmpeg binary, checking PATH and common install locations.
pub(crate) fn find_ffmpeg() -> Result<String> {
    // First try the name directly (respects the process PATH)
    if Command::new("ffmpeg")
        .arg("-version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok()
    {
        return Ok("ffmpeg".to_string());
    }
    // Fall back to common absolute paths
    for path in &[
        "/usr/bin/ffmpeg",
        "/usr/local/bin/ffmpeg",
        "/opt/homebrew/bin/ffmpeg",
    ] {
        if std::path::Path::new(path).exists() {
            return Ok(path.to_string());
        }
    }
    Err(anyhow!("ffmpeg not found — please install ffmpeg"))
}

/// Check whether a source file has at least one audio stream.
pub(crate) fn probe_has_audio(ffmpeg: &str, path: &str) -> bool {
    // Derive ffprobe path from ffmpeg path (they live side-by-side)
    let ffprobe = ffmpeg.replace("ffmpeg", "ffprobe");
    Command::new(&ffprobe)
        .args([
            "-v",
            "error",
            "-select_streams",
            "a:0",
            "-show_entries",
            "stream=codec_type",
            "-of",
            "csv=p=0",
            path,
        ])
        .output()
        .map(|o| !o.stdout.is_empty())
        .unwrap_or(false)
}

// ── Silence detection ───────────────────────────────────────────────────

/// Detect silent intervals in a source clip's audio track using ffmpeg's `silencedetect` filter.
///
/// Returns `(silence_start_sec, silence_end_sec)` pairs relative to `source_in_ns`.
/// Returns an empty vec if the source has no audio stream.
pub(crate) fn detect_silence(
    source_path: &str,
    source_in_ns: u64,
    source_out_ns: u64,
    noise_db: f64,
    min_duration: f64,
) -> Result<Vec<(f64, f64)>> {
    let ffmpeg = find_ffmpeg()?;
    if !probe_has_audio(&ffmpeg, source_path) {
        return Ok(Vec::new());
    }
    let src_in_sec = source_in_ns as f64 / 1_000_000_000.0;
    // source_out_ns is an absolute position, not a duration — compute the duration
    let duration_sec = source_out_ns.saturating_sub(source_in_ns) as f64 / 1_000_000_000.0;
    if duration_sec <= 0.0 {
        return Ok(Vec::new());
    }
    let af = format!("silencedetect=noise={noise_db}dB:d={min_duration}");
    let output = Command::new(&ffmpeg)
        .args([
            "-ss",
            &format!("{src_in_sec}"),
            "-t",
            &format!("{duration_sec}"),
            "-i",
            source_path,
            "-af",
            &af,
            "-f",
            "null",
            "-",
        ])
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .output()
        .map_err(|e| anyhow!("Failed to run ffmpeg silencedetect: {e}"))?;

    let stderr = String::from_utf8_lossy(&output.stderr);
    let mut intervals = Vec::new();
    let mut pending_start: Option<f64> = None;
    for line in stderr.lines() {
        if let Some(pos) = line.find("silence_start: ") {
            let val_str = &line[pos + "silence_start: ".len()..];
            if let Some(val) = val_str
                .split_whitespace()
                .next()
                .and_then(|s| s.parse::<f64>().ok())
            {
                pending_start = Some(val);
            }
        }
        if let Some(pos) = line.find("silence_end: ") {
            let val_str = &line[pos + "silence_end: ".len()..];
            if let Some(end_val) = val_str
                .split_whitespace()
                .next()
                .and_then(|s| s.parse::<f64>().ok())
            {
                if let Some(start_val) = pending_start.take() {
                    intervals.push((start_val, end_val));
                }
            }
        }
    }
    // Handle trailing silence_start with no silence_end
    if let Some(start_val) = pending_start {
        intervals.push((start_val, duration_sec));
    }
    Ok(intervals)
}

/// Convert silent intervals (returned by `detect_silence`) into the inverse:
/// **speech intervals** in clip-local nanoseconds, suitable for storing in
/// `Clip::voice_isolation_speech_intervals`.
///
/// `silences` is a sorted list of `(start_sec, end_sec)` pairs relative to
/// `source_in`. `clip_duration_ns` is `source_out_ns - source_in_ns`. Speech
/// regions are everything between/around the silences.
pub(crate) fn invert_silences_to_speech(
    silences: &[(f64, f64)],
    clip_duration_ns: u64,
) -> Vec<(u64, u64)> {
    let mut speech: Vec<(u64, u64)> = Vec::new();
    let mut cursor_ns: u64 = 0;
    for (s, e) in silences {
        let s_ns = (s.max(0.0) * 1_000_000_000.0) as u64;
        let e_ns = (e.max(0.0) * 1_000_000_000.0) as u64;
        if s_ns > cursor_ns {
            speech.push((cursor_ns, s_ns.min(clip_duration_ns)));
        }
        cursor_ns = e_ns;
    }
    if cursor_ns < clip_duration_ns {
        speech.push((cursor_ns, clip_duration_ns));
    }
    // Drop zero-length intervals that can occur at clip boundaries.
    speech.retain(|(s, e)| e > s);
    speech
}

/// Analyze a clip's audio and suggest a silence-detection threshold (in dB)
/// based on the noise floor.
///
/// Uses FFmpeg's `astats` filter with windowed RMS measurements (0.5 s windows)
/// and computes the **5th percentile** of the windowed RMS levels — a robust
/// noise-floor estimate that ignores both intro/outro silences (which would
/// pull a naive mean toward `-inf`) and loud transients during speech.
///
/// Returns the suggested threshold = noise_floor + 6 dB headroom, clamped to
/// the inspector's slider range `[-60.0, -10.0]`.
///
/// Returns an error if ffmpeg is missing, the source has no audio, or no RMS
/// samples were measurable.
pub(crate) fn suggest_silence_threshold_db(
    source_path: &str,
    source_in_ns: u64,
    source_out_ns: u64,
) -> Result<f32> {
    let ffmpeg = find_ffmpeg()?;
    if !probe_has_audio(&ffmpeg, source_path) {
        return Err(anyhow!("source has no audio stream"));
    }
    let src_in_sec = source_in_ns as f64 / 1_000_000_000.0;
    let duration_sec = source_out_ns.saturating_sub(source_in_ns) as f64 / 1_000_000_000.0;
    if duration_sec <= 0.0 {
        return Err(anyhow!("clip duration is zero"));
    }
    // astats with reset=0.5 gives one measurement per 0.5 s window. ametadata=print
    // emits the RMS_level metadata to stderr (alongside other lavfi.astats keys).
    // We're only interested in lavfi.astats.Overall.RMS_level lines.
    let af = "astats=metadata=1:reset=0.5,ametadata=print:key=lavfi.astats.Overall.RMS_level"
        .to_string();
    let output = Command::new(&ffmpeg)
        .args([
            "-nostats",
            "-hide_banner",
            "-ss",
            &format!("{src_in_sec}"),
            "-t",
            &format!("{duration_sec}"),
            "-i",
            source_path,
            "-vn",
            "-af",
            &af,
            "-f",
            "null",
            "-",
        ])
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .output()
        .map_err(|e| anyhow!("Failed to run ffmpeg astats: {e}"))?;

    let stderr = String::from_utf8_lossy(&output.stderr);
    let mut rms_levels: Vec<f64> = Vec::new();
    for line in stderr.lines() {
        // ametadata=print emits lines like:
        //   [Parsed_ametadata_1 @ 0x...] lavfi.astats.Overall.RMS_level=-42.123456
        if let Some(pos) = line.find("lavfi.astats.Overall.RMS_level=") {
            let val_str = &line[pos + "lavfi.astats.Overall.RMS_level=".len()..];
            if let Some(val) = val_str
                .split_whitespace()
                .next()
                .and_then(|s| s.parse::<f64>().ok())
            {
                // -inf shows up as a very large negative — skip these (silent windows).
                if val.is_finite() && val > -200.0 {
                    rms_levels.push(val);
                }
            }
        }
    }

    if rms_levels.is_empty() {
        return Err(anyhow!("no RMS samples produced by astats"));
    }

    // 5th percentile of windowed RMS = robust noise-floor estimate.
    rms_levels.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let idx = ((rms_levels.len() as f64) * 0.05) as usize;
    let noise_floor = rms_levels[idx.min(rms_levels.len() - 1)];

    // Suggested threshold sits 6 dB above the noise floor.
    let suggested = (noise_floor + 6.0) as f32;
    Ok(suggested.clamp(-60.0, -10.0))
}

// ── Scene cut detection ─────────────────────────────────────────────────

/// Detect scene/shot changes in a video clip using ffmpeg's `scdet` filter.
///
/// Returns cut-point timestamps in seconds, relative to `source_in_ns`.
/// Returns an empty vec if the source has no video stream or no cuts are found.
pub(crate) fn detect_scene_cuts(
    source_path: &str,
    source_in_ns: u64,
    source_out_ns: u64,
    threshold: f64,
) -> Result<Vec<f64>> {
    let ffmpeg = find_ffmpeg()?;
    let src_in_sec = source_in_ns as f64 / 1_000_000_000.0;
    let duration_sec = source_out_ns.saturating_sub(source_in_ns) as f64 / 1_000_000_000.0;
    if duration_sec <= 0.0 {
        return Ok(Vec::new());
    }
    let vf = format!("scdet=threshold={threshold}:sc_pass=1");
    let output = Command::new(&ffmpeg)
        .args([
            "-ss",
            &format!("{src_in_sec}"),
            "-t",
            &format!("{duration_sec}"),
            "-i",
            source_path,
            "-vf",
            &vf,
            "-an",
            "-f",
            "null",
            "-",
        ])
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .output()
        .map_err(|e| anyhow!("Failed to run ffmpeg scdet: {e}"))?;

    let stderr = String::from_utf8_lossy(&output.stderr);
    let mut cuts = Vec::new();
    for line in stderr.lines() {
        if let Some(pos) = line.find("lavfi.scd.time:") {
            let val_str = &line[pos + "lavfi.scd.time:".len()..];
            if let Some(t) = val_str
                .split_whitespace()
                .next()
                .and_then(|s| s.parse::<f64>().ok())
            {
                // Skip cuts at the very start or end of the clip
                if t > 0.01 && t < duration_sec - 0.01 {
                    cuts.push(t);
                }
            }
        }
    }
    cuts.dedup_by(|a, b| (*a - *b).abs() < 0.01);
    Ok(cuts)
}

// ── Loudness measurement ────────────────────────────────────────────────

/// Full EBU R128 loudness report for a measured audio source.
///
/// All fields are in standard R128 units. `short_term_max_lufs` and
/// `momentary_max_lufs` track the running max across the per-frame log;
/// `true_peak_dbtp` is populated only when FFmpeg is invoked with
/// `ebur128=peak=true` (otherwise defaults to 0.0 on parse).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct LoudnessReport {
    /// Integrated loudness over the full duration (I:). LUFS.
    pub integrated_lufs: f64,
    /// Loudness Range (LRA:). LU.
    pub loudness_range_lu: f64,
    /// Integrated threshold (I Threshold:). LUFS.
    pub threshold_lufs: f64,
    /// Maximum short-term (3 s window) loudness observed. LUFS.
    pub short_term_max_lufs: f64,
    /// Maximum momentary (400 ms window) loudness observed. LUFS.
    pub momentary_max_lufs: f64,
    /// True-peak (dBTP) from the Summary block's Peak: line. dBFS equivalent.
    pub true_peak_dbtp: f64,
}

/// Parse a LoudnessReport out of an ffmpeg stderr dump that used
/// `ebur128=peak=true:framelog=verbose`.
///
/// Exposed (pub(crate)) so unit tests can feed canned strings without
/// spawning a subprocess.
pub(crate) fn parse_loudness_report(stderr: &str) -> Result<LoudnessReport> {
    let mut report = LoudnessReport::default();
    let mut saw_summary_i = false;
    let mut in_summary = false;

    // First pass: scan per-frame log lines of the form
    //   [Parsed_ebur128_0 @ 0x...] t: 0.4  M: -25.3 S:-inf     I: -25.3 LUFS  LRA:  0.0 LU
    // and track max(M) and max(S). `-inf` / `nan` are ignored.
    for line in stderr.lines() {
        if !line.contains("[Parsed_ebur128") {
            continue;
        }
        // `M: -25.3` and `S: -18.7` — spaces between key and value vary.
        if let Some(m) = extract_ebur128_metric(line, " M:") {
            if m > report.momentary_max_lufs || report.momentary_max_lufs == 0.0 {
                report.momentary_max_lufs = m;
            }
        }
        if let Some(s) = extract_ebur128_metric(line, " S:") {
            if s > report.short_term_max_lufs || report.short_term_max_lufs == 0.0 {
                report.short_term_max_lufs = s;
            }
        }
    }

    // Second pass: walk the trailing Summary block and extract
    // I:, I Threshold:, LRA:, Peak:.
    let mut in_integrated = false;
    let mut in_range = false;
    let mut in_true_peak = false;
    for line in stderr.lines() {
        let trimmed = line.trim_start_matches(|c: char| !c.is_alphabetic()).trim();
        if line.contains("Summary:") {
            in_summary = true;
            in_integrated = false;
            in_range = false;
            in_true_peak = false;
            continue;
        }
        if !in_summary {
            continue;
        }
        if trimmed.starts_with("Integrated loudness") {
            in_integrated = true;
            in_range = false;
            in_true_peak = false;
            continue;
        }
        if trimmed.starts_with("Loudness range") {
            in_integrated = false;
            in_range = true;
            in_true_peak = false;
            continue;
        }
        if trimmed.starts_with("True peak") {
            in_integrated = false;
            in_range = false;
            in_true_peak = true;
            continue;
        }
        if in_integrated && trimmed.starts_with("I:") {
            if let Some(val) = parse_leading_f64(&trimmed["I:".len()..]) {
                report.integrated_lufs = val;
                saw_summary_i = true;
            }
        } else if in_integrated && trimmed.starts_with("Threshold:") {
            if let Some(val) = parse_leading_f64(&trimmed["Threshold:".len()..]) {
                report.threshold_lufs = val;
            }
        } else if in_range && trimmed.starts_with("LRA:") {
            if let Some(val) = parse_leading_f64(&trimmed["LRA:".len()..]) {
                report.loudness_range_lu = val;
            }
        } else if in_true_peak && trimmed.starts_with("Peak:") {
            if let Some(val) = parse_leading_f64(&trimmed["Peak:".len()..]) {
                report.true_peak_dbtp = val;
            }
        }
    }

    if !saw_summary_i {
        return Err(anyhow!(
            "Could not parse integrated loudness from ffmpeg ebur128 output"
        ));
    }
    Ok(report)
}

/// Extract a numeric metric from a per-frame ebur128 log line.
/// Handles `-inf`, `nan`, and whitespace variations between the key and value.
fn extract_ebur128_metric(line: &str, key: &str) -> Option<f64> {
    let idx = line.find(key)?;
    let rest = &line[idx + key.len()..];
    let token = rest.trim_start().split_whitespace().next().unwrap_or("");
    if token.is_empty() || token.contains("inf") || token.contains("nan") {
        return None;
    }
    token.parse::<f64>().ok().filter(|v| v.is_finite())
}

/// Parse the leading f64 from a stringified metric value, ignoring the unit.
/// `"  -23.0 LUFS"` → `Some(-23.0)`. `"-inf LUFS"` → `None`.
fn parse_leading_f64(s: &str) -> Option<f64> {
    let token = s.trim().split_whitespace().next().unwrap_or("");
    if token.is_empty() || token.contains("inf") || token.contains("nan") {
        return None;
    }
    token.parse::<f64>().ok().filter(|v| v.is_finite())
}

/// Analyze a source file and return the full EBU R128 loudness report.
/// Invokes FFmpeg with `ebur128=peak=true:framelog=verbose` so the stderr
/// dump contains both per-frame M/S values and the Summary block with the
/// True Peak line.
pub(crate) fn analyze_loudness_full(
    source_path: &str,
    source_in_ns: u64,
    source_out_ns: u64,
) -> Result<LoudnessReport> {
    analyze_loudness_full_with_prefilter(source_path, source_in_ns, source_out_ns, None)
}

pub(crate) fn analyze_loudness_full_with_prefilter(
    source_path: &str,
    source_in_ns: u64,
    source_out_ns: u64,
    prefilter: Option<String>,
) -> Result<LoudnessReport> {
    let ffmpeg = find_ffmpeg()?;
    if !probe_has_audio(&ffmpeg, source_path) {
        return Err(anyhow!("Clip has no audio stream"));
    }
    let src_in_sec = source_in_ns as f64 / 1_000_000_000.0;
    let duration_sec = source_out_ns.saturating_sub(source_in_ns) as f64 / 1_000_000_000.0;
    if duration_sec <= 0.0 {
        return Err(anyhow!("Clip has zero duration"));
    }
    let ebur128_filter = "ebur128=peak=true:framelog=verbose";
    let audio_filter = prefilter
        .map(|filter| format!("{filter},{ebur128_filter}"))
        .unwrap_or_else(|| ebur128_filter.to_string());
    let output = Command::new(&ffmpeg)
        .args([
            "-nostats",
            "-hide_banner",
            "-ss",
            &format!("{src_in_sec}"),
            "-t",
            &format!("{duration_sec}"),
            "-i",
            source_path,
            "-vn", // skip video decode — audio-only analysis
            "-af",
            &audio_filter,
            "-f",
            "null",
            "-",
        ])
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .output()
        .map_err(|e| anyhow!("Failed to run ffmpeg ebur128: {e}"))?;

    let stderr = String::from_utf8_lossy(&output.stderr);
    parse_loudness_report(&stderr)
}

/// Scalar wrapper — returns only the integrated LUFS value. Preserved for
/// the existing per-clip Inspector **Normalize…** button, `normalize_clip_audio`
/// MCP tool, and `match_clip_audio` paths which don't need the full report.
pub(crate) fn analyze_loudness_lufs(
    source_path: &str,
    source_in_ns: u64,
    source_out_ns: u64,
) -> Result<f64> {
    Ok(analyze_loudness_full(source_path, source_in_ns, source_out_ns)?.integrated_lufs)
}

pub(crate) fn analyze_loudness_lufs_with_prefilter(
    source_path: &str,
    source_in_ns: u64,
    source_out_ns: u64,
    prefilter: Option<String>,
) -> Result<f64> {
    Ok(
        analyze_loudness_full_with_prefilter(source_path, source_in_ns, source_out_ns, prefilter)?
            .integrated_lufs,
    )
}

/// Measure peak amplitude (dB) of a clip's audio via FFmpeg `volumedetect` filter.
/// Returns the max volume in dBFS (e.g. -3.5, where 0.0 = full scale).
pub(crate) fn analyze_peak_db(
    source_path: &str,
    source_in_ns: u64,
    source_out_ns: u64,
) -> Result<f64> {
    let ffmpeg = find_ffmpeg()?;
    if !probe_has_audio(&ffmpeg, source_path) {
        return Err(anyhow!("Clip has no audio stream"));
    }
    let src_in_sec = source_in_ns as f64 / 1_000_000_000.0;
    let duration_sec = source_out_ns.saturating_sub(source_in_ns) as f64 / 1_000_000_000.0;
    if duration_sec <= 0.0 {
        return Err(anyhow!("Clip has zero duration"));
    }
    let output = Command::new(&ffmpeg)
        .args([
            "-ss",
            &format!("{src_in_sec}"),
            "-t",
            &format!("{duration_sec}"),
            "-i",
            source_path,
            "-vn", // skip video decode — audio-only analysis
            "-af",
            "volumedetect",
            "-f",
            "null",
            "-",
        ])
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .output()
        .map_err(|e| anyhow!("Failed to run ffmpeg volumedetect: {e}"))?;

    let stderr = String::from_utf8_lossy(&output.stderr);
    // Parse "max_volume: -X.X dB" from volumedetect output.
    for line in stderr.lines() {
        if let Some(pos) = line.find("max_volume:") {
            let rest = &line[pos + "max_volume:".len()..];
            if let Some(val) = rest
                .trim()
                .split_whitespace()
                .next()
                .and_then(|s| s.parse::<f64>().ok())
            {
                return Ok(val);
            }
        }
    }
    Err(anyhow!(
        "Could not parse max_volume from ffmpeg volumedetect output"
    ))
}

// ── Gain computation ────────────────────────────────────────────────────

/// Compute the linear gain multiplier needed to shift measured LUFS to a target LUFS.
pub(crate) fn compute_lufs_gain(measured_lufs: f64, target_lufs: f64) -> f64 {
    10.0_f64.powf((target_lufs - measured_lufs) / 20.0)
}

/// Compute the linear gain multiplier needed to shift measured peak dB to a target dB.
pub(crate) fn compute_peak_gain(measured_peak_db: f64, target_peak_db: f64) -> f64 {
    10.0_f64.powf((target_peak_db - measured_peak_db) / 20.0)
}
