use crate::model::clip::{Clip, ClipKind};
use crate::model::project::Project;
use anyhow::{anyhow, Result};
use std::io::{BufRead, BufReader};
use std::process::{Command, Stdio};
use std::sync::mpsc;

/// Progress updates sent back to the UI thread
#[derive(Debug)]
pub enum ExportProgress {
    Progress(f64), // 0.0 – 1.0
    Done,
    Error(String),
}

#[derive(Debug, Clone, PartialEq)]
pub enum VideoCodec {
    H264,
    H265,
    Vp9,
    ProRes,
    Av1,
}
#[derive(Debug, Clone, PartialEq)]
pub enum AudioCodec {
    Aac,
    Opus,
    Flac,
    Pcm,
}
#[derive(Debug, Clone, PartialEq)]
pub enum Container {
    Mp4,
    Mov,
    WebM,
    Mkv,
}

impl Container {
    pub fn extension(&self) -> &'static str {
        match self {
            Container::Mp4 => "mp4",
            Container::Mov => "mov",
            Container::WebM => "webm",
            Container::Mkv => "mkv",
        }
    }
}

/// Options for a single export operation.
#[derive(Debug, Clone)]
pub struct ExportOptions {
    pub video_codec: VideoCodec,
    pub container: Container,
    /// 0 = use project resolution
    pub output_width: u32,
    pub output_height: u32,
    /// CRF quality value (lower = better quality / larger file)
    pub crf: u32,
    pub audio_codec: AudioCodec,
    pub audio_bitrate_kbps: u32,
}

impl Default for ExportOptions {
    fn default() -> Self {
        Self {
            video_codec: VideoCodec::H264,
            container: Container::Mp4,
            output_width: 0,
            output_height: 0,
            crf: 23,
            audio_codec: AudioCodec::Aac,
            audio_bitrate_kbps: 192,
        }
    }
}

/// Export the project to a file at `output_path` using `options`.
/// Sends progress to `tx`. Call this from a background thread.
pub fn export_project(
    project: &Project,
    output_path: &str,
    options: ExportOptions,
    estimated_size_bytes: Option<u64>,
    tx: mpsc::Sender<ExportProgress>,
) -> Result<()> {
    let out_w = if options.output_width == 0 {
        project.width
    } else {
        options.output_width
    };
    let out_h = if options.output_height == 0 {
        project.height
    } else {
        options.output_height
    };

    // Primary video track (first video track) — forms the base concat sequence.
    // Secondary video tracks are composited on top with overlay.
    let mut video_tracks_iter = project.video_tracks();
    let primary_clips: Vec<&crate::model::clip::Clip> = video_tracks_iter
        .next()
        .map(|t| t.clips.iter().collect())
        .unwrap_or_default();

    // Remaining video tracks: each is a list of (overlay) clips
    let secondary_track_clips: Vec<Vec<&crate::model::clip::Clip>> = project
        .video_tracks()
        .skip(1)
        .filter(|t| !t.muted)
        .map(|t| t.clips.iter().collect())
        .collect();

    if primary_clips.is_empty() {
        return Err(anyhow!("No video clips to export"));
    }

    // Collect audio-only clips from non-muted audio tracks
    let mut audio_clips: Vec<_> = project
        .audio_tracks()
        .filter(|t| !t.muted)
        .flat_map(|t| t.clips.iter())
        .collect();
    audio_clips.sort_by_key(|c| c.timeline_start);

    // Flatten secondary clips for indexing
    let secondary_clips_flat: Vec<_> = secondary_track_clips.iter().flatten().copied().collect();

    let total_duration_us = project.duration().max(1) / 1_000;
    let estimated_size_bytes = estimated_size_bytes
        .filter(|v| *v > 0)
        .or_else(|| estimate_export_size_bytes(project, &options, out_w, out_h));
    let _ = tx.send(ExportProgress::Progress(0.0));

    let ffmpeg = find_ffmpeg()?;
    let mut cmd = Command::new(&ffmpeg);
    cmd.arg("-y")
        .arg("-hide_banner")
        .arg("-loglevel")
        .arg("error")
        .arg("-progress")
        .arg("pipe:2")
        .arg("-nostats");

    // Inputs: primary video clips (0..primary_clips.len())
    for clip in &primary_clips {
        let in_s = clip.source_in as f64 / 1_000_000_000.0;
        let src_dur_s = clip.source_duration() as f64 / 1_000_000_000.0;
        cmd.arg("-ss")
            .arg(format!("{in_s:.6}"))
            .arg("-t")
            .arg(format!("{src_dur_s:.6}"))
            .arg("-i")
            .arg(&clip.source_path);
    }

    // Inputs: secondary video clips (primary_clips.len()..primary_clips.len()+secondary_clips_flat.len())
    for clip in &secondary_clips_flat {
        let in_s = clip.source_in as f64 / 1_000_000_000.0;
        let src_dur_s = clip.source_duration() as f64 / 1_000_000_000.0;
        cmd.arg("-ss")
            .arg(format!("{in_s:.6}"))
            .arg("-t")
            .arg(format!("{src_dur_s:.6}"))
            .arg("-i")
            .arg(&clip.source_path);
    }

    let sec_base = primary_clips.len();

    // Audio-only clip inputs
    let audio_base = sec_base + secondary_clips_flat.len();
    for clip in &audio_clips {
        let in_s = clip.source_in as f64 / 1_000_000_000.0;
        let src_dur_s = clip.source_duration() as f64 / 1_000_000_000.0;
        cmd.arg("-ss")
            .arg(format!("{in_s:.6}"))
            .arg("-t")
            .arg(format!("{src_dur_s:.6}"))
            .arg("-i")
            .arg(&clip.source_path);
    }

    let mut filter = String::new();

    // === Primary video track: scale/correct each clip then concatenate ===
    for (i, clip) in primary_clips.iter().enumerate() {
        let color_filter = build_color_filter(clip);
        let temp_tint_filter = build_temperature_tint_filter(clip);
        let grading_filter = build_grading_filter(clip);
        let denoise_filter = build_denoise_filter(clip);
        let sharpen_filter = build_sharpen_filter(clip);
        let chroma_key_filter = build_chroma_key_filter(clip);
        let speed_filter = build_speed_filter(clip);
        let lut_filter = build_lut_filter(clip);
        let crop_filter = build_crop_filter(clip, out_w, out_h, false);
        let rotate_filter = build_rotation_filter(clip, false);
        let scale_pos_filter = build_scale_position_filter(clip, out_w, out_h, false);
        if clip.chroma_key_enabled {
            filter.push_str(&format!(
                "[{i}:v]format=yuva420p,scale={out_w}:{out_h}:force_original_aspect_ratio=decrease,pad={out_w}:{out_h}:(ow-iw)/2:(oh-ih)/2:color=black@0,setsar=1{crop_filter}{scale_pos_filter}{rotate_filter},fps={}/{}{lut_filter}{color_filter}{temp_tint_filter}{grading_filter}{denoise_filter}{sharpen_filter}{chroma_key_filter}{speed_filter}[pv{i}];",
                project.frame_rate.numerator, project.frame_rate.denominator
            ));
        } else {
            filter.push_str(&format!(
                "[{i}:v]scale={out_w}:{out_h}:force_original_aspect_ratio=decrease,pad={out_w}:{out_h}:(ow-iw)/2:(oh-ih)/2,setsar=1{crop_filter}{scale_pos_filter}{rotate_filter},fps={}/{},format=yuv420p{lut_filter}{color_filter}{temp_tint_filter}{grading_filter}{denoise_filter}{sharpen_filter}{speed_filter}[pv{i}];",
                project.frame_rate.numerator, project.frame_rate.denominator
            ));
        }
    }
    // Check for xfade and tpad support
    let has_xfade = check_filter_support(&ffmpeg, "xfade");
    let has_tpad = check_filter_support(&ffmpeg, "tpad");

    // Map transition kind string to the ffmpeg xfade transition name.
    let transition_xfade_name = |kind: &str| -> &'static str {
        match kind {
            "cross_dissolve" => "fade",
            "fade_to_black" => "fadeblack",
            "wipe_right" => "wiperight",
            "wipe_left" => "wipeleft",
            _ => "fade", // safe fallback
        }
    };

    // Build primary-track sequence:
    // - If transitions exist AND filters are supported, chain xfade filters
    // - Otherwise use concat (original behavior).
    let has_primary_transitions = primary_clips
        .iter()
        .take(primary_clips.len().saturating_sub(1))
        .any(|c| !c.transition_after.is_empty() && c.transition_after_ns > 0);

    if primary_clips.len() == 1 {
        filter.push_str("[pv0]copy[vbase]");
    } else if has_primary_transitions && has_xfade && has_tpad {
        let mut prev_label = "pv0".to_string();
        let mut running_s = primary_clips[0].duration() as f64 / 1_000_000_000.0;
        let mut total_overlap_s = 0.0_f64;
        for i in 0..(primary_clips.len() - 1) {
            let next_label = format!("pv{}", i + 1);
            let out_label = format!("vxd{}", i + 1);
            let clip = &primary_clips[i];
            let mut d_s = if !clip.transition_after.is_empty() && clip.transition_after_ns > 0 {
                clip.transition_after_ns as f64 / 1_000_000_000.0
            } else {
                0.0
            };
            let max_d = (primary_clips[i]
                .duration()
                .min(primary_clips[i + 1].duration()) as f64
                / 1_000_000_000.0)
                - 0.001;
            d_s = d_s.clamp(0.0, max_d.max(0.0));
            if d_s <= 0.0 {
                d_s = 0.001;
            }
            let offset_s = (running_s - d_s).max(0.0);
            let sep = if i == 0 { "" } else { ";" };
            let xfade = transition_xfade_name(&clip.transition_after);
            filter.push_str(&format!(
                "{sep}[{prev_label}][{next_label}]xfade=transition={xfade}:duration={d_s:.6}:offset={offset_s:.6}[{out_label}]"
            ));
            running_s += primary_clips[i + 1].duration() as f64 / 1_000_000_000.0 - d_s;
            total_overlap_s += d_s;
            prev_label = out_label;
        }
        if total_overlap_s > 0.0 {
            filter.push_str(&format!(
                ";[{prev_label}]tpad=stop_mode=clone:stop_duration={total_overlap_s:.6}[vbase]"
            ));
        } else {
            filter.push_str(&format!(";[{prev_label}]copy[vbase]"));
        }
    } else {
        for i in 0..primary_clips.len() {
            filter.push_str(&format!("[pv{i}]"));
        }
        filter.push_str(&format!("concat=n={}:v=1:a=0[vbase]", primary_clips.len()));
    }

    // === Secondary video tracks: overlay each clip at its timeline position ===
    // Chain overlays: [vbase] → overlay clip 0 → [vcomp0] → overlay clip 1 → [vcomp1] → ...
    let mut prev_label = "vbase".to_string();
    for (k, clip) in secondary_clips_flat.iter().enumerate() {
        let in_idx = sec_base + k;
        let color_filter = build_color_filter(clip);
        let temp_tint_filter = build_temperature_tint_filter(clip);
        let grading_filter = build_grading_filter(clip);
        let denoise_filter = build_denoise_filter(clip);
        let sharpen_filter = build_sharpen_filter(clip);
        let chroma_key_filter = build_chroma_key_filter(clip);
        let speed_filter = build_speed_filter(clip);
        let lut_filter = build_lut_filter(clip);
        let crop_filter = build_crop_filter(clip, out_w, out_h, true);
        let rotate_filter = build_rotation_filter(clip, true);
        let scale_pos_filter = build_scale_position_filter(clip, out_w, out_h, true);
        let opacity = clip.opacity.clamp(0.0, 1.0);
        // Scale the overlay clip to output size (keeps aspect ratio, pads transparent)
        let ov_label = format!("ov{k}");
        filter.push_str(&format!(
            ";[{in_idx}:v]format=yuva420p,scale={out_w}:{out_h}:force_original_aspect_ratio=decrease,pad={out_w}:{out_h}:(ow-iw)/2:(oh-ih)/2:color=black@0,setsar=1{lut_filter}{color_filter}{temp_tint_filter}{grading_filter}{denoise_filter}{sharpen_filter}{chroma_key_filter}{crop_filter}{scale_pos_filter}{rotate_filter},colorchannelmixer=aa={opacity:.4}{speed_filter}[{ov_label}raw]"
        ));
        // Delay PTS to timeline position so the overlay lands at the right time
        let start_s = clip.timeline_start as f64 / 1_000_000_000.0;
        filter.push_str(&format!(
            ";[{ov_label}raw]setpts=PTS+{start_s:.6}/TB[{ov_label}]"
        ));
        let next_label = format!("vcomp{k}");
        let end_s = (clip.timeline_start + clip.duration()) as f64 / 1_000_000_000.0;
        filter.push_str(&format!(
            ";[{prev_label}][{ov_label}]overlay=x=0:y=0:enable='between(t,{start_s:.6},{end_s:.6})'[{next_label}]"
        ));
        prev_label = next_label;
    }
    // Final output video label — use the last composited label directly
    let vout_label = prev_label;

    // === Audio pipeline ===
    let mut audio_labels: Vec<String> = Vec::new();

    // Embedded audio from primary video clips, with per-clip volume scaling
    for (i, clip) in primary_clips.iter().enumerate() {
        if clip.kind == ClipKind::Video
            && !has_linked_audio_peer(clip, &audio_clips)
            && probe_has_audio(&ffmpeg, &clip.source_path)
        {
            let delay_ms = clip.timeline_start / 1_000_000;
            let label = format!("va{i}");
            let areverse = if clip.reverse { "areverse," } else { "" };
            let atempo = build_atempo(clip.speed);
            let vol = clip.volume;
            filter.push_str(&format!(
                ";[{i}:a]{areverse}{atempo}volume={vol:.4},adelay={delay_ms}:all=1[{label}]"
            ));
            audio_labels.push(label);
        }
    }

    // Embedded audio from secondary video clips (with their volume)
    for (k, clip) in secondary_clips_flat.iter().enumerate() {
        let in_idx = sec_base + k;
        if clip.kind == ClipKind::Video
            && !has_linked_audio_peer(clip, &audio_clips)
            && probe_has_audio(&ffmpeg, &clip.source_path)
        {
            let delay_ms = clip.timeline_start / 1_000_000;
            let label = format!("sva{k}");
            let areverse = if clip.reverse { "areverse," } else { "" };
            let atempo = build_atempo(clip.speed);
            let vol = clip.volume;
            filter.push_str(&format!(
                ";[{in_idx}:a]{areverse}{atempo}volume={vol:.4},adelay={delay_ms}:all=1[{label}]"
            ));
            audio_labels.push(label);
        }
    }

    // Audio-only track clips
    for (j, clip) in audio_clips.iter().enumerate() {
        let delay_ms = clip.timeline_start / 1_000_000;
        let label = format!("aa{j}");
        let areverse = if clip.reverse { "areverse," } else { "" };
        let atempo = build_atempo(clip.speed);
        let vol = clip.volume;
        filter.push_str(&format!(
            ";[{}:a]{areverse}{atempo}volume={vol:.4},adelay={delay_ms}:all=1[{label}]",
            audio_base + j
        ));
        audio_labels.push(label);
    }

    // Mix all audio streams
    let has_audio = !audio_labels.is_empty();
    if has_audio {
        let n = audio_labels.len();
        filter.push(';');
        for label in &audio_labels {
            filter.push_str(&format!("[{label}]"));
        }
        filter.push_str(&format!("amix=inputs={n}:normalize=0[aout]"));
    }

    cmd.arg("-filter_complex")
        .arg(&filter)
        .arg("-map")
        .arg(format!("[{vout_label}]"));

    if has_audio {
        cmd.arg("-map").arg("[aout]");
        match options.audio_codec {
            AudioCodec::Aac => {
                cmd.arg("-c:a")
                    .arg("aac")
                    .arg("-b:a")
                    .arg(format!("{}k", options.audio_bitrate_kbps));
            }
            AudioCodec::Opus => {
                cmd.arg("-c:a")
                    .arg("libopus")
                    .arg("-b:a")
                    .arg(format!("{}k", options.audio_bitrate_kbps));
            }
            AudioCodec::Flac => {
                cmd.arg("-c:a").arg("flac");
            }
            AudioCodec::Pcm => {
                cmd.arg("-c:a").arg("pcm_s24le");
            }
        }
    }

    match options.video_codec {
        VideoCodec::H264 => {
            cmd.arg("-c:v")
                .arg("libx264")
                .arg("-crf")
                .arg(options.crf.to_string())
                .arg("-pix_fmt")
                .arg("yuv420p");
        }
        VideoCodec::H265 => {
            cmd.arg("-c:v")
                .arg("libx265")
                .arg("-crf")
                .arg(options.crf.to_string())
                .arg("-pix_fmt")
                .arg("yuv420p");
        }
        VideoCodec::Vp9 => {
            cmd.arg("-c:v")
                .arg("libvpx-vp9")
                .arg("-crf")
                .arg(options.crf.to_string())
                .arg("-b:v")
                .arg("0")
                .arg("-pix_fmt")
                .arg("yuv420p");
        }
        VideoCodec::ProRes => {
            cmd.arg("-c:v").arg("prores_ks").arg("-profile:v").arg("3");
        }
        VideoCodec::Av1 => {
            cmd.arg("-c:v")
                .arg("libaom-av1")
                .arg("-crf")
                .arg(options.crf.to_string())
                .arg("-b:v")
                .arg("0")
                .arg("-pix_fmt")
                .arg("yuv420p");
        }
    }

    // Container-specific flags
    if matches!(options.container, Container::Mp4 | Container::Mov) {
        cmd.arg("-movflags").arg("+faststart");
    }

    cmd.arg(output_path)
        .stdout(Stdio::null())
        .stderr(Stdio::piped());

    eprintln!(
        "[export] ffmpeg args: {:?}",
        cmd.get_args().collect::<Vec<_>>()
    );

    let mut child = cmd
        .spawn()
        .map_err(|e| anyhow!("Failed to start ffmpeg: {e}"))?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| anyhow!("Failed to capture ffmpeg stderr"))?;
    let reader = BufReader::new(stderr);

    let mut error_lines: Vec<String> = Vec::new();
    for line in reader.lines().map_while(|r| r.ok()) {
        if let Some(p) = parse_progress_line(&line, total_duration_us, estimated_size_bytes) {
            let _ = tx.send(ExportProgress::Progress(p));
        } else if !line.starts_with("frame=")
            && !line.starts_with("fps=")
            && !line.starts_with("progress=")
            && !line.starts_with("speed=")
            && !line.starts_with("bitrate=")
            && !line.starts_with("size=")
            && !line.starts_with("total_size=")
            && !line.starts_with("out_")
            && !line.starts_with("dup_")
            && !line.starts_with("drop_")
            && !line.starts_with("stream_")
        {
            eprintln!("[export] ffmpeg: {line}");
            error_lines.push(line);
        }
    }

    let status = child
        .wait()
        .map_err(|e| anyhow!("Failed waiting for ffmpeg: {e}"))?;
    if !status.success() {
        let detail = error_lines.join("; ");
        let msg = format!("ffmpeg export failed: {detail}");
        let _ = tx.send(ExportProgress::Error(msg.clone()));
        return Err(anyhow!("{msg}"));
    }

    let _ = tx.send(ExportProgress::Done);
    Ok(())
}

fn build_color_filter(clip: &crate::model::clip::Clip) -> String {
    if clip.brightness != 0.0 || clip.contrast != 1.0 || clip.saturation != 1.0 {
        format!(
            ",eq=brightness={:.4}:contrast={:.4}:saturation={:.4}",
            clip.brightness.clamp(-1.0, 1.0),
            clip.contrast.clamp(0.0, 2.0),
            clip.saturation.clamp(0.0, 2.0)
        )
    } else {
        String::new()
    }
}

fn build_temperature_tint_filter(clip: &crate::model::clip::Clip) -> String {
    let has_temp = (clip.temperature - 6500.0).abs() > 1.0;
    let has_tint = clip.tint.abs() > 0.001;
    let mut f = String::new();
    if has_temp {
        f.push_str(&format!(
            ",colortemperature=temperature={:.0}",
            clip.temperature.clamp(2000.0, 10000.0)
        ));
    }
    if has_tint {
        // Map tint to green channel offset via colorbalance midtones.
        // Negative tint = boost green (positive gm), positive tint = cut green (negative gm)
        // and complementary red+blue boost.
        let t = clip.tint.clamp(-1.0, 1.0);
        let gm = -t * 0.5;
        let rm = t * 0.25;
        let bm = t * 0.25;
        f.push_str(&format!(
            ",colorbalance=rm={rm:.4}:gm={gm:.4}:bm={bm:.4}"
        ));
    }
    f
}

fn build_denoise_filter(clip: &crate::model::clip::Clip) -> String {
    if clip.denoise > 0.0 {
        let d = clip.denoise.clamp(0.0, 1.0);
        format!(
            ",hqdn3d={:.4}:{:.4}:{:.4}:{:.4}",
            d * 4.0,
            d * 3.0,
            d * 6.0,
            d * 4.5
        )
    } else {
        String::new()
    }
}

fn build_sharpen_filter(clip: &crate::model::clip::Clip) -> String {
    if clip.sharpness != 0.0 {
        let la = (clip.sharpness * 3.0).clamp(-2.0, 5.0);
        format!(",unsharp=lx=5:ly=5:la={la:.4}:cx=5:cy=5:ca={la:.4}")
    } else {
        String::new()
    }
}

fn build_lut_filter(clip: &crate::model::clip::Clip) -> String {
    if let Some(ref path) = clip.lut_path {
        if !path.is_empty() && std::path::Path::new(path).exists() {
            // Escape path for ffmpeg filter syntax (colons and backslashes need escaping)
            let escaped = path.replace('\\', "\\\\").replace(':', "\\:");
            return format!(",lut3d={escaped}");
        }
    }
    String::new()
}

fn build_grading_filter(clip: &crate::model::clip::Clip) -> String {
    if clip.shadows != 0.0 || clip.midtones != 0.0 || clip.highlights != 0.0 {
        let s = clip.shadows.clamp(-1.0, 1.0);
        let m = clip.midtones.clamp(-1.0, 1.0);
        let h = clip.highlights.clamp(-1.0, 1.0);
        format!(
            ",colorbalance=rs={s:.4}:gs={s:.4}:bs={s:.4}:rm={m:.4}:gm={m:.4}:bm={m:.4}:rh={h:.4}:gh={h:.4}:bh={h:.4}"
        )
    } else {
        String::new()
    }
}

fn build_chroma_key_filter(clip: &crate::model::clip::Clip) -> String {
    if clip.chroma_key_enabled {
        let color = format!("{:06x}", clip.chroma_key_color & 0xFFFFFF);
        let similarity = (clip.chroma_key_tolerance * 0.5).clamp(0.01, 0.5);
        let blend = (clip.chroma_key_softness * 0.5).clamp(0.0, 0.5);
        format!(",colorkey=0x{color}:{similarity:.4}:{blend:.4}")
    } else {
        String::new()
    }
}

fn build_speed_filter(clip: &crate::model::clip::Clip) -> String {
    let has_speed = (clip.speed - 1.0).abs() > 0.001;
    match (clip.reverse, has_speed) {
        (false, false) => String::new(),
        (false, true) => format!(",setpts=PTS/{:.6}", clip.speed),
        // `reverse` already emits a valid timeline in ffmpeg; STARTPTS-PTS here can
        // cause non-monotonic DTS and near-empty video output.
        (true, false) => ",reverse".to_string(),
        (true, true) => format!(",reverse,setpts=PTS/{:.6}", clip.speed),
    }
}

/// Build an ffmpeg crop + re-pad filter for user-controlled crop.
/// `transparent_pad`: when true, pads with transparent black (for overlay clips);
/// otherwise pads with opaque black (primary track).
fn build_crop_filter(
    clip: &crate::model::clip::Clip,
    out_w: u32,
    out_h: u32,
    transparent_pad: bool,
) -> String {
    let cl = clip.crop_left.max(0) as u32;
    let cr = clip.crop_right.max(0) as u32;
    let ct = clip.crop_top.max(0) as u32;
    let cb = clip.crop_bottom.max(0) as u32;
    if cl == 0 && cr == 0 && ct == 0 && cb == 0 {
        return String::new();
    }
    let cw = out_w.saturating_sub(cl + cr).max(1);
    let ch = out_h.saturating_sub(ct + cb).max(1);
    let pad_color = if transparent_pad {
        "black@0.0"
    } else {
        "black"
    };
    format!(",crop={cw}:{ch}:{cl}:{ct},pad={out_w}:{out_h}:{cl}:{ct}:{pad_color}")
}

/// Build a rotation filter for arbitrary-angle clip rotation.
fn build_rotation_filter(clip: &crate::model::clip::Clip, transparent_pad: bool) -> String {
    let rot = clip.rotate;
    if rot == 0 {
        return String::new();
    }
    let fill = if transparent_pad { "black@0" } else { "black" };
    format!(",rotate={:.10}:fillcolor={fill}", (rot as f64).to_radians())
}

/// Build a scale + crop/pad filter for user-controlled scale and position.
/// Inserts BEFORE the output pad/crop so the final result is still `out_w × out_h`.
fn build_scale_position_filter(
    clip: &crate::model::clip::Clip,
    out_w: u32,
    out_h: u32,
    transparent_pad: bool,
) -> String {
    let scale = clip.scale.clamp(0.1, 4.0);
    if (scale - 1.0).abs() < 0.001 && clip.position_x.abs() < 0.001 && clip.position_y.abs() < 0.001
    {
        return String::new(); // passthrough when scale=1 and position=0
    }
    let pw = out_w as f64;
    let ph = out_h as f64;
    let pos_x = clip.position_x;
    let pos_y = clip.position_y;

    if scale >= 1.0 {
        // Zoom in: scale UP then crop to output size.
        let sw = (pw * scale).round() as u32;
        let sh = (ph * scale).round() as u32;
        let total_x = pw * (scale - 1.0);
        let total_y = ph * (scale - 1.0);
        let cx = (total_x * (1.0 + pos_x) / 2.0).round() as i64;
        let cy = (total_y * (1.0 + pos_y) / 2.0).round() as i64;
        // ffmpeg: scale then crop (x=cx, y=cy from top-left of the scaled frame)
        format!(",scale={sw}:{sh},crop={out_w}:{out_h}:{cx}:{cy}")
    } else {
        // Zoom out: scale DOWN then pad to output size.
        // When the PIP extends beyond the frame edge (position > 1.0 or < -1.0),
        // crop the overflow before padding — matching the preview's videobox
        // behavior which clips content past the frame boundary.
        let sw = (pw * scale).round() as u32;
        let sh = (ph * scale).round() as u32;
        let total_x = pw * (1.0 - scale);
        let total_y = ph * (1.0 - scale);
        let raw_pad_x = (total_x * (1.0 + pos_x) / 2.0).round() as i64;
        let raw_pad_y = (total_y * (1.0 + pos_y) / 2.0).round() as i64;

        // Compute overflow on each edge
        let crop_left = if raw_pad_x < 0 {
            (-raw_pad_x) as u32
        } else {
            0
        };
        let crop_top = if raw_pad_y < 0 {
            (-raw_pad_y) as u32
        } else {
            0
        };
        let crop_right = if raw_pad_x as i64 + sw as i64 > out_w as i64 {
            (raw_pad_x as i64 + sw as i64 - out_w as i64) as u32
        } else {
            0
        };
        let crop_bottom = if raw_pad_y as i64 + sh as i64 > out_h as i64 {
            (raw_pad_y as i64 + sh as i64 - out_h as i64) as u32
        } else {
            0
        };

        let pad_x = raw_pad_x.max(0) as u32;
        let pad_y = raw_pad_y.max(0) as u32;
        let pad_color = if transparent_pad { "black@0" } else { "black" };

        let needs_crop = crop_left > 0 || crop_top > 0 || crop_right > 0 || crop_bottom > 0;
        if needs_crop {
            let vis_w = sw.saturating_sub(crop_left + crop_right).max(1);
            let vis_h = sh.saturating_sub(crop_top + crop_bottom).max(1);
            format!(
                ",scale={sw}:{sh},crop={vis_w}:{vis_h}:{crop_left}:{crop_top},pad={out_w}:{out_h}:{pad_x}:{pad_y}:{pad_color}"
            )
        } else {
            format!(",scale={sw}:{sh},pad={out_w}:{out_h}:{pad_x}:{pad_y}:{pad_color}")
        }
    }
}

/// Build atempo filter chain for audio speed change.
/// atempo is limited to 0.5–2.0 per filter, so chain multiple for extremes.
/// Returns a string like "atempo=2.0," (with trailing comma) or "" for 1.0x.
fn build_atempo(speed: f64) -> String {
    if (speed - 1.0).abs() < 0.001 {
        return String::new();
    }
    let mut remaining = speed;
    let mut filters = String::new();
    // Chain atempo in [0.5, 2.0] steps
    while remaining > 2.0 {
        filters.push_str("atempo=2.0,");
        remaining /= 2.0;
    }
    while remaining < 0.5 {
        filters.push_str("atempo=0.5,");
        remaining /= 0.5;
    }
    filters.push_str(&format!("atempo={remaining:.6},"));
    filters
}

fn parse_progress_line(
    line: &str,
    total_duration_us: u64,
    estimated_size_bytes: Option<u64>,
) -> Option<f64> {
    if let Some(estimate) = estimated_size_bytes {
        if let Some(v) = line.strip_prefix("total_size=") {
            if let Ok(bytes) = v.parse::<u64>() {
                return Some((bytes as f64 / estimate as f64).clamp(0.0, 0.99));
            }
        }
        return None;
    }

    if let Some(v) = line.strip_prefix("out_time_us=") {
        if let Ok(us) = v.parse::<u64>() {
            return Some((us as f64 / total_duration_us as f64).clamp(0.0, 0.99));
        }
    } else if let Some(v) = line.strip_prefix("out_time_ms=") {
        if let Ok(us) = v.parse::<u64>() {
            return Some((us as f64 / total_duration_us as f64).clamp(0.0, 0.99));
        }
    }
    None
}

fn estimate_export_size_bytes(
    project: &Project,
    options: &ExportOptions,
    out_w: u32,
    out_h: u32,
) -> Option<u64> {
    let duration_secs = project.duration() as f64 / 1_000_000_000.0;
    if duration_secs <= 0.0 {
        return None;
    }
    let total_bitrate_bps = estimate_export_total_bitrate_bps(project, options, out_w, out_h);
    if total_bitrate_bps == 0 {
        return None;
    }
    Some(((total_bitrate_bps as f64 * duration_secs) / 8.0).round() as u64)
}

fn estimate_export_total_bitrate_bps(
    project: &Project,
    options: &ExportOptions,
    out_w: u32,
    out_h: u32,
) -> u64 {
    let fps = project.frame_rate.as_f64().clamp(1.0, 120.0);
    let pixel_scale = ((out_w.max(1) as f64 * out_h.max(1) as f64) / (1920.0 * 1080.0)).max(0.1);
    let fps_scale = (fps / 30.0).max(0.5);
    let crf_scale = (1.0 + ((23.0 - options.crf as f64) * 0.04)).clamp(0.4, 2.0);

    let base_video_kbps = match options.video_codec {
        VideoCodec::H264 => 6_000.0,
        VideoCodec::H265 => 4_200.0,
        VideoCodec::Vp9 => 3_600.0,
        VideoCodec::Av1 => 3_200.0,
        VideoCodec::ProRes => 95_000.0,
    };
    let mut video_kbps = base_video_kbps * pixel_scale * fps_scale * crf_scale;
    if matches!(options.video_codec, VideoCodec::ProRes) {
        video_kbps = video_kbps.max(40_000.0);
    } else {
        video_kbps = video_kbps.clamp(700.0, 40_000.0);
    }

    let audio_kbps = match options.audio_codec {
        AudioCodec::Aac | AudioCodec::Opus => options.audio_bitrate_kbps.max(64) as f64,
        AudioCodec::Flac => 1_000.0,
        AudioCodec::Pcm => 4_608.0, // 48kHz * 24-bit * 2ch
    };

    ((video_kbps + audio_kbps) * 1_000.0) as u64
}

fn check_filter_support(ffmpeg: &str, filter_name: &str) -> bool {
    let output = Command::new(ffmpeg)
        .arg("-filters")
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).to_string())
        .unwrap_or_default();
    output.lines().any(|line| {
        let parts: Vec<&str> = line.split_whitespace().collect();
        if parts.len() >= 2 {
            parts[1] == filter_name
        } else {
            false
        }
    })
}

fn has_linked_audio_peer(clip: &Clip, audio_clips: &[&Clip]) -> bool {
    audio_clips
        .iter()
        .any(|audio_clip| clip.suppresses_embedded_audio_for_linked_peer(audio_clip))
}

fn probe_has_audio(ffmpeg: &str, path: &str) -> bool {
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

#[cfg(test)]
mod tests {
    use super::{
        estimate_export_size_bytes, has_linked_audio_peer, parse_progress_line, AudioCodec,
        ExportOptions, VideoCodec,
    };
    use crate::model::clip::{Clip, ClipKind};
    use crate::model::project::Project;

    #[test]
    fn total_size_progress_uses_estimate_and_caps() {
        let p = parse_progress_line("total_size=500", 1_000_000, Some(1_000)).unwrap();
        assert!((p - 0.5).abs() < 1e-6);

        let capped = parse_progress_line("total_size=2000", 1_000_000, Some(1_000)).unwrap();
        assert!((capped - 0.99).abs() < 1e-6);
    }

    #[test]
    fn total_size_mode_ignores_out_time_lines() {
        assert!(parse_progress_line("out_time_us=500000", 1_000_000, Some(1_000)).is_none());
    }

    #[test]
    fn time_mode_out_time_ms_treated_as_microseconds() {
        let p = parse_progress_line("out_time_ms=500000", 1_000_000, None).unwrap();
        assert!((p - 0.5).abs() < 1e-6);
    }

    #[test]
    fn export_size_estimate_returns_positive_value() {
        let mut project = Project::new("Test");
        project.tracks[0].add_clip(Clip::new(
            "/tmp/test.mp4",
            5_000_000_000,
            0,
            ClipKind::Video,
        ));
        let options = ExportOptions {
            video_codec: VideoCodec::H264,
            audio_codec: AudioCodec::Aac,
            ..ExportOptions::default()
        };
        let est = estimate_export_size_bytes(&project, &options, project.width, project.height);
        assert!(est.unwrap_or(0) > 0);
    }

    #[test]
    fn linked_audio_peer_suppresses_embedded_video_audio() {
        let mut video = Clip::new("/tmp/test.mp4", 5_000_000_000, 0, ClipKind::Video);
        video.link_group_id = Some("link-1".to_string());

        let mut audio = Clip::new("/tmp/test.mp4", 5_000_000_000, 0, ClipKind::Audio);
        audio.link_group_id = Some("link-1".to_string());

        assert!(has_linked_audio_peer(&video, std::slice::from_ref(&&audio)));
    }
}
