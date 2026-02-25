use anyhow::{anyhow, Result};
use std::io::{BufRead, BufReader};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use crate::model::clip::ClipKind;
use crate::model::project::Project;

/// Progress updates sent back to the UI thread
#[derive(Debug)]
pub enum ExportProgress {
    Progress(f64),   // 0.0 – 1.0
    Done,
    Error(String),
}

/// Export the project to an MP4 file at `output_path`.
/// Sends progress to `tx`. Call this from a background thread.
pub fn export_project(
    project: &Project,
    output_path: &str,
    tx: mpsc::Sender<ExportProgress>,
) -> Result<()> {
    let mut video_clips: Vec<_> = project.video_tracks()
        .flat_map(|t| t.clips.iter())
        .collect();
    video_clips.sort_by_key(|c| c.timeline_start);

    if video_clips.is_empty() {
        return Err(anyhow!("No video clips to export"));
    }

    // Collect audio-only clips from non-muted audio tracks
    let mut audio_clips: Vec<_> = project.audio_tracks()
        .filter(|t| !t.muted)
        .flat_map(|t| t.clips.iter())
        .collect();
    audio_clips.sort_by_key(|c| c.timeline_start);

    let total_duration_us = project.duration().max(1) / 1_000;
    let _ = tx.send(ExportProgress::Progress(0.0));

    let mut cmd = Command::new("ffmpeg");
    cmd.arg("-y")
        .arg("-hide_banner")
        .arg("-loglevel").arg("error")
        .arg("-progress").arg("pipe:2")
        .arg("-nostats");

    // Video clip inputs: indices 0..video_clips.len()
    for clip in &video_clips {
        let in_s = clip.source_in as f64 / 1_000_000_000.0;
        let dur_s = clip.duration() as f64 / 1_000_000_000.0;
        cmd.arg("-ss").arg(format!("{in_s:.6}"))
            .arg("-t").arg(format!("{dur_s:.6}"))
            .arg("-i").arg(&clip.source_path);
    }

    // Audio-only clip inputs: indices video_clips.len()..
    for clip in &audio_clips {
        let in_s = clip.source_in as f64 / 1_000_000_000.0;
        let dur_s = clip.duration() as f64 / 1_000_000_000.0;
        cmd.arg("-ss").arg(format!("{in_s:.6}"))
            .arg("-t").arg(format!("{dur_s:.6}"))
            .arg("-i").arg(&clip.source_path);
    }

    let mut filter = String::new();

    // === Video pipeline: scale/pad each clip then concatenate ===
    for (i, _) in video_clips.iter().enumerate() {
        filter.push_str(&format!(
            "[{i}:v]scale={}:{}:force_original_aspect_ratio=decrease,pad={}:{}:(ow-iw)/2:(oh-ih)/2,setsar=1,fps={}/{},format=yuv420p[v{i}];",
            project.width, project.height, project.width, project.height,
            project.frame_rate.numerator, project.frame_rate.denominator
        ));
    }
    for i in 0..video_clips.len() {
        filter.push_str(&format!("[v{i}]"));
    }
    filter.push_str(&format!("concat=n={}:v=1:a=0[vout]", video_clips.len()));

    // === Audio pipeline: delay each stream to its timeline position then mix ===
    let mut audio_labels: Vec<String> = Vec::new();

    // Extract embedded audio from video clips (only ClipKind::Video, not images)
    for (i, clip) in video_clips.iter().enumerate() {
        if clip.kind == ClipKind::Video {
            let delay_ms = clip.timeline_start / 1_000_000;
            let label = format!("va{i}");
            filter.push_str(&format!(";[{i}:a]adelay={delay_ms}:all=1[{label}]"));
            audio_labels.push(label);
        }
    }

    // Extract audio from audio-only clips
    let audio_base = video_clips.len();
    for (j, clip) in audio_clips.iter().enumerate() {
        let delay_ms = clip.timeline_start / 1_000_000;
        let label = format!("aa{j}");
        filter.push_str(&format!(";[{}:a]adelay={delay_ms}:all=1[{label}]", audio_base + j));
        audio_labels.push(label);
    }

    // Mix all audio streams into one output
    let has_audio = !audio_labels.is_empty();
    if has_audio {
        let n = audio_labels.len();
        for label in &audio_labels {
            filter.push_str(&format!("[{label}]"));
        }
        filter.push_str(&format!("amix=inputs={n}:normalize=0[aout]"));
    }

    cmd.arg("-filter_complex").arg(&filter)
        .arg("-map").arg("[vout]");

    if has_audio {
        cmd.arg("-map").arg("[aout]")
            .arg("-c:a").arg("aac")
            .arg("-b:a").arg("192k");
    }

    cmd.arg("-c:v").arg("libx264")
        .arg("-pix_fmt").arg("yuv420p")
        .arg("-movflags").arg("+faststart")
        .arg(output_path)
        .stdout(Stdio::null())
        .stderr(Stdio::piped());

    let mut child = cmd.spawn().map_err(|e| anyhow!("Failed to start ffmpeg: {e}"))?;
    let stderr = child.stderr.take().ok_or_else(|| anyhow!("Failed to capture ffmpeg stderr"))?;
    let reader = BufReader::new(stderr);

    for line in reader.lines().map_while(|r| r.ok()) {
        if let Some(v) = line.strip_prefix("out_time_us=") {
            if let Ok(us) = v.parse::<u64>() {
                let p = (us as f64 / total_duration_us as f64).clamp(0.0, 1.0);
                let _ = tx.send(ExportProgress::Progress(p));
            }
        } else if let Some(v) = line.strip_prefix("out_time_ms=") {
            if let Ok(ms) = v.parse::<u64>() {
                let us = ms.saturating_mul(1000);
                let p = (us as f64 / total_duration_us as f64).clamp(0.0, 1.0);
                let _ = tx.send(ExportProgress::Progress(p));
            }
        }
    }

    let status = child.wait().map_err(|e| anyhow!("Failed waiting for ffmpeg: {e}"))?;
    if !status.success() {
        let _ = tx.send(ExportProgress::Error("ffmpeg export failed".to_string()));
        return Err(anyhow!("ffmpeg export failed with status {status}"));
    }

    let _ = tx.send(ExportProgress::Done);
    Ok(())
}
