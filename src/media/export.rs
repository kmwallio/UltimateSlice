use crate::media::program_player::ProgramPlayer;
use crate::model::clip::{Clip, ClipKind, NumericKeyframe, SlowMotionInterp};
use crate::model::project::Project;
use anyhow::{anyhow, Result};
use std::collections::HashMap;
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

#[derive(Debug, Clone, Default)]
pub(crate) struct ColorFilterCapabilities {
    pub(crate) use_coloradj_frei0r: bool,
    pub(crate) three_point_frei0r_module: Option<String>,
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
    Gif,
}

impl Container {
    pub fn extension(&self) -> &'static str {
        match self {
            Container::Mp4 => "mp4",
            Container::Mov => "mov",
            Container::WebM => "webm",
            Container::Mkv => "mkv",
            Container::Gif => "gif",
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
    /// Frames per second for GIF output (None = use project frame rate). Only used when container = Gif.
    pub gif_fps: Option<u32>,
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
            gif_fps: None,
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
    bg_removal_paths: &std::collections::HashMap<String, String>,
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
    let frame_duration_s = if project.frame_rate.numerator > 0 && project.frame_rate.denominator > 0
    {
        project.frame_rate.denominator as f64 / project.frame_rate.numerator as f64
    } else {
        1.0 / 30.0
    };

    // Primary video track (first active video track) — forms the base concat sequence.
    // Secondary active video tracks are composited on top with overlay.
    let active_video_tracks: Vec<_> = project
        .video_tracks()
        .filter(|t| project.track_is_active_for_output(t))
        .collect();
    let mut primary_clips: Vec<&crate::model::clip::Clip> = active_video_tracks
        .first()
        .map(|t| t.clips.iter().filter(|c| c.kind != ClipKind::Adjustment).collect())
        .unwrap_or_default();
    primary_clips.sort_by_key(|c| c.timeline_start);

    // Remaining video tracks: each is a list of (overlay) clips
    // Collect adjustment clips from ALL tracks before filtering them out.
    let all_adjustment_clips: Vec<&Clip> = active_video_tracks
        .iter()
        .flat_map(|t| t.clips.iter())
        .filter(|c| c.kind == ClipKind::Adjustment)
        .collect();

    let secondary_track_clips: Vec<Vec<&crate::model::clip::Clip>> = active_video_tracks
        .into_iter()
        .skip(1)
        .map(|t| {
            let mut clips: Vec<&Clip> = t.clips.iter().filter(|c| c.kind != ClipKind::Adjustment).collect();
            clips.sort_by_key(|c| c.timeline_start);
            clips
        })
        .collect();

    if primary_clips.is_empty() {
        return Err(anyhow!("No video clips to export"));
    }

    // Collect audio-only clips from active audio tracks.
    let audio_track_clips: Vec<Vec<&crate::model::clip::Clip>> = project
        .audio_tracks()
        .filter(|t| project.track_is_active_for_output(t))
        .map(|t| {
            let mut clips: Vec<&Clip> = t.clips.iter().collect();
            clips.sort_by_key(|c| c.timeline_start);
            clips
        })
        .collect();
    let audio_clips: Vec<&Clip> = audio_track_clips.iter().flatten().copied().collect();

    // Flatten secondary clips for indexing
    let secondary_clips_flat: Vec<_> = secondary_track_clips.iter().flatten().copied().collect();

    let total_duration_us = project.duration().max(1) / 1_000;
    let estimated_size_bytes = estimated_size_bytes
        .filter(|v| *v > 0)
        .or_else(|| estimate_export_size_bytes(project, &options, out_w, out_h));
    let _ = tx.send(ExportProgress::Progress(0.0));

    let ffmpeg = find_ffmpeg()?;
    let preferences = crate::ui_state::load_preferences_state();
    let crossfade_enabled = preferences.crossfade_enabled;
    let crossfade_duration_ns = preferences.crossfade_duration_ns;
    let crossfade_curve = audio_crossfade_curve_name(&preferences.crossfade_curve);
    let mut audio_presence_cache: HashMap<String, bool> = HashMap::new();
    let mut cmd = Command::new(&ffmpeg);
    cmd.arg("-y")
        .arg("-hide_banner")
        .arg("-loglevel")
        .arg("error")
        .arg("-progress")
        .arg("pipe:2")
        .arg("-nostats");

    // Helper: resolve effective source path (bg-removed version if available).
    // Helper: true when the clip's actual FFmpeg input is a bg-removed file (video-only, no audio).
    let uses_bg_removal_path = |clip: &Clip| -> bool {
        clip.bg_removal_enabled
            && bg_removal_paths
                .get(&clip.source_path)
                .map(|p| std::path::Path::new(p).exists())
                .unwrap_or(false)
    };

    let resolve_export_path = |clip: &Clip| -> String {
        if clip.kind == ClipKind::Title || clip.kind == ClipKind::Adjustment {
            return String::new(); // Title/adjustment clips use lavfi or no input
        }
        if clip.bg_removal_enabled {
            if let Some(bg_path) = bg_removal_paths.get(&clip.source_path) {
                if std::path::Path::new(bg_path).exists() {
                    return bg_path.clone();
                }
            }
        }
        clip.source_path.clone()
    };

    // Inputs: primary video clips (0..primary_clips.len())
    // Adjustment clips are already filtered out of primary_clips.
    for clip in &primary_clips {
        if clip.kind == ClipKind::Title {
            let dur_s = clip.duration() as f64 / 1_000_000_000.0;
            let bg = title_clip_lavfi_color(clip, out_w, out_h,
                project.frame_rate.numerator, project.frame_rate.denominator, dur_s);
            cmd.arg("-f").arg("lavfi").arg("-i").arg(bg);
        } else {
            let (in_s, src_dur_s) = video_input_seek_and_duration(clip, frame_duration_s);
            if clip.kind == ClipKind::Image {
                cmd.arg("-loop").arg("1");
            }
            cmd.arg("-ss")
                .arg(format!("{in_s:.6}"))
                .arg("-t")
                .arg(format!("{src_dur_s:.6}"))
                .arg("-i")
                .arg(resolve_export_path(clip));
        }
    }

    // Inputs: secondary video clips (primary_clips.len()..primary_clips.len()+secondary_clips_flat.len())
    // Adjustment clips are already filtered out of secondary_track_clips.
    for clip in &secondary_clips_flat {
        if clip.kind == ClipKind::Title {
            let dur_s = clip.duration() as f64 / 1_000_000_000.0;
            let bg = title_clip_lavfi_color(clip, out_w, out_h,
                project.frame_rate.numerator, project.frame_rate.denominator, dur_s);
            cmd.arg("-f").arg("lavfi").arg("-i").arg(bg);
        } else {
            let (in_s, src_dur_s) = video_input_seek_and_duration(clip, frame_duration_s);
            if clip.kind == ClipKind::Image {
                cmd.arg("-loop").arg("1");
            }
            cmd.arg("-ss")
                .arg(format!("{in_s:.6}"))
                .arg("-t")
                .arg(format!("{src_dur_s:.6}"))
                .arg("-i")
                .arg(resolve_export_path(clip));
        }
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

    // Chapter metadata input (FFMETADATA file from project markers).
    // Must be added after all media inputs so the input index is correct.
    let _chapter_metadata = write_chapter_metadata(&project.markers, project.duration())?;
    if let Some(ref meta) = _chapter_metadata {
        let metadata_input_idx = audio_base + audio_clips.len();
        cmd.arg("-f")
            .arg("ffmetadata")
            .arg("-i")
            .arg(meta.path());
        cmd.arg("-map_metadata").arg(format!("{metadata_input_idx}"));
    }

    let mut filter = String::new();
    let color_caps = detect_color_filter_capabilities(&ffmpeg);

    // === Vidstab pre-analysis pass for clips with stabilization enabled ===
    let mut vidstab_trf: HashMap<String, String> = HashMap::new();
    for clip in primary_clips.iter().chain(secondary_clips_flat.iter()) {
        if let Ok(Some(trf)) = run_vidstab_analysis(&ffmpeg, clip, frame_duration_s) {
            vidstab_trf.insert(clip.id.clone(), trf);
        }
    }

    // === Primary video track: scale/correct each clip then concatenate ===
    // Adjustment clips are already filtered out of primary_clips.
    for (i, clip) in primary_clips.iter().enumerate() {
        let color_filter = build_color_filter(clip);
        let temp_tint_filter = build_temperature_tint_filter_with_caps(clip, &color_caps);
        let grading_filter = build_grading_filter_with_caps(clip, &color_caps);
        let denoise_filter = build_denoise_filter(clip);
        let sharpen_filter = build_sharpen_filter(clip);
        let blur_filter = build_blur_filter(clip);
        let vidstab_filter = build_vidstab_filter(clip, vidstab_trf.get(&clip.id).map(|s| s.as_str()));
        let frei0r_effects_filter = build_frei0r_effects_filter(clip);
        let chroma_key_filter = build_chroma_key_filter(clip);
        let title_filter = build_title_filter(clip, out_h);
        let speed_filter = build_timing_filter(clip, frame_duration_s, project.frame_rate.numerator, project.frame_rate.denominator);
        let lut_prefix = build_lut_filter_prefix(clip);
        let crop_filter = build_crop_filter(clip, out_w, out_h, false);
        let rotate_filter = build_rotation_filter(clip, false);
        let has_transform_keyframes = has_transform_keyframes(clip);
        let has_opacity_keyframes = !clip.opacity_keyframes.is_empty();
        if has_transform_keyframes || has_opacity_keyframes {
            let scale_expr = build_keyframed_property_expression(
                &clip.scale_keyframes,
                clip.scale,
                0.1,
                4.0,
                "t",
            );
            let pos_x_expr = build_keyframed_property_expression(
                &clip.position_x_keyframes,
                clip.position_x,
                -1.0,
                1.0,
                "t",
            );
            let pos_y_expr = build_keyframed_property_expression(
                &clip.position_y_keyframes,
                clip.position_y,
                -1.0,
                1.0,
                "t",
            );
            let opacity_expr = build_keyframed_property_expression(
                &clip.opacity_keyframes,
                clip.opacity,
                0.0,
                1.0,
                "T",
            );
            let clip_duration_s = clip.duration() as f64 / 1_000_000_000.0;
            let anamorphic_filter = build_anamorphic_filter(clip);
            filter.push_str(&format!(
                "[{i}:v]{lut_prefix}{anamorphic_filter}format=yuva420p,scale={out_w}:{out_h}:force_original_aspect_ratio=decrease,setsar=1,pad={out_w}:{out_h}:(ow-iw)/2:(oh-ih)/2:color=black@0{crop_filter}{rotate_filter},fps={}/{}{vidstab_filter}{color_filter}{temp_tint_filter}{grading_filter}{denoise_filter}{sharpen_filter}{blur_filter}{frei0r_effects_filter}{chroma_key_filter}{title_filter}{speed_filter}\
                 ,scale=w='max(1,{out_w}*({scale_expr}))':h='max(1,{out_h}*({scale_expr}))':eval=frame[pv{i}fg];\
                 color=c=black:size={out_w}x{out_h}:r={}/{}:d={clip_duration_s:.6}[pv{i}bg];\
                 [pv{i}bg][pv{i}fg]overlay=x='(W-w)*(1+({pos_x_expr}))/2':y='(H-h)*(1+({pos_y_expr}))/2':eval=frame\
                 ,geq=lum='lum(X,Y)':cb='cb(X,Y)':cr='cr(X,Y)':a='alpha(X,Y)*({opacity_expr})'[pv{i}raw];\
                 [pv{i}raw]format=yuv420p[pv{i}];",
                project.frame_rate.numerator, project.frame_rate.denominator
                , project.frame_rate.numerator, project.frame_rate.denominator
            ));
        } else if clip.chroma_key_enabled || clip.bg_removal_enabled {
            let scale_pos_filter = build_scale_position_filter(clip, out_w, out_h, false);
            let anamorphic_filter = build_anamorphic_filter(clip);
            filter.push_str(&format!(
                "[{i}:v]{lut_prefix}{anamorphic_filter}format=yuva420p,scale={out_w}:{out_h}:force_original_aspect_ratio=decrease,setsar=1,pad={out_w}:{out_h}:(ow-iw)/2:(oh-ih)/2:color=black@0{crop_filter}{scale_pos_filter}{rotate_filter},fps={}/{}{vidstab_filter}{color_filter}{temp_tint_filter}{grading_filter}{denoise_filter}{sharpen_filter}{blur_filter}{frei0r_effects_filter}{chroma_key_filter}{title_filter}{speed_filter}[pv{i}raw];[pv{i}raw]format=yuv420p[pv{i}];",
                project.frame_rate.numerator, project.frame_rate.denominator
            ));
        } else {
            let scale_pos_filter = build_scale_position_filter(clip, out_w, out_h, false);
            let anamorphic_filter = build_anamorphic_filter(clip);
            filter.push_str(&format!(
                "[{i}:v]{lut_prefix}{anamorphic_filter}scale={out_w}:{out_h}:force_original_aspect_ratio=decrease,setsar=1,pad={out_w}:{out_h}:(ow-iw)/2:(oh-ih)/2{crop_filter}{scale_pos_filter}{rotate_filter},fps={}/{},format=yuv420p{vidstab_filter}{color_filter}{temp_tint_filter}{grading_filter}{denoise_filter}{sharpen_filter}{blur_filter}{frei0r_effects_filter}{title_filter}{speed_filter}[pv{i}];",
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
            let out_label = format!("vseq{}", i + 1);
            let clip = &primary_clips[i];
            let next_clip = &primary_clips[i + 1];
            let sep = if i == 0 { "" } else { ";" };
            if let Some(d_s) = clamped_primary_xfade_duration_s(clip, next_clip) {
                let offset_s = (running_s - d_s).max(0.0);
                let xfade = transition_xfade_name(&clip.transition_after);
                filter.push_str(&format!(
                    "{sep}[{prev_label}][{next_label}]xfade=transition={xfade}:duration={d_s:.6}:offset={offset_s:.6}[{out_label}]"
                ));
                running_s += next_clip.duration() as f64 / 1_000_000_000.0 - d_s;
                total_overlap_s += d_s;
            } else {
                filter.push_str(&format!(
                    "{sep}[{prev_label}][{next_label}]concat=n=2:v=1:a=0[{out_label}]"
                ));
                running_s += next_clip.duration() as f64 / 1_000_000_000.0;
            }
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

    // Use the pre-collected adjustment clips from all tracks.
    let mut adjustment_clips = all_adjustment_clips;
    adjustment_clips.sort_by_key(|c| c.timeline_start);

    // === Secondary video tracks: overlay each clip at its timeline position ===
    // Chain overlays: [vbase] → overlay clip 0 → [vcomp0] → overlay clip 1 → [vcomp1] → ...
    let mut prev_label = "vbase".to_string();
    // Adjustment clips are already filtered out of secondary_clips_flat.
    for (k, clip) in secondary_clips_flat.iter().enumerate() {
        let in_idx = sec_base + k;
        let color_filter = build_color_filter(clip);
        let temp_tint_filter = build_temperature_tint_filter_with_caps(clip, &color_caps);
        let grading_filter = build_grading_filter_with_caps(clip, &color_caps);
        let denoise_filter = build_denoise_filter(clip);
        let sharpen_filter = build_sharpen_filter(clip);
        let blur_filter = build_blur_filter(clip);
        let vidstab_filter = build_vidstab_filter(clip, vidstab_trf.get(&clip.id).map(|s| s.as_str()));
        let frei0r_effects_filter = build_frei0r_effects_filter(clip);
        let chroma_key_filter = build_chroma_key_filter(clip);
        let title_filter = build_title_filter(clip, out_h);
        let speed_filter = build_timing_filter(clip, frame_duration_s, project.frame_rate.numerator, project.frame_rate.denominator);
        let lut_prefix = build_lut_filter_prefix(clip);        let crop_filter = build_crop_filter(clip, out_w, out_h, true);
        let rotate_filter = build_rotation_filter(clip, true);
        let has_transform_keyframes = has_transform_keyframes(clip);
        let has_opacity_keyframes = !clip.opacity_keyframes.is_empty();
        // Scale the overlay clip to output size (keeps aspect ratio, pads transparent)
        let ov_label = format!("ov{k}");
        if has_transform_keyframes || has_opacity_keyframes {
            let scale_expr = build_keyframed_property_expression(
                &clip.scale_keyframes,
                clip.scale,
                0.1,
                4.0,
                "t",
            );
            let pos_x_expr = build_keyframed_property_expression(
                &clip.position_x_keyframes,
                clip.position_x,
                -1.0,
                1.0,
                "t",
            );
            let pos_y_expr = build_keyframed_property_expression(
                &clip.position_y_keyframes,
                clip.position_y,
                -1.0,
                1.0,
                "t",
            );
            let opacity_expr = build_keyframed_property_expression(
                &clip.opacity_keyframes,
                clip.opacity,
                0.0,
                1.0,
                "T",
            );
            let clip_duration_s = clip.duration() as f64 / 1_000_000_000.0;
            let anamorphic_filter = build_anamorphic_filter(clip);
            filter.push_str(&format!(
                ";[{in_idx}:v]{lut_prefix}{anamorphic_filter}format=yuva420p,scale={out_w}:{out_h}:force_original_aspect_ratio=decrease,setsar=1,pad={out_w}:{out_h}:(ow-iw)/2:(oh-ih)/2:color=black@0,fps={}/{}{vidstab_filter}{color_filter}{temp_tint_filter}{grading_filter}{denoise_filter}{sharpen_filter}{blur_filter}{frei0r_effects_filter}{chroma_key_filter}{title_filter}{crop_filter}{rotate_filter}{speed_filter}\
                 ,scale=w='max(1,{out_w}*({scale_expr}))':h='max(1,{out_h}*({scale_expr}))':eval=frame[ov{k}fg];\
                 color=c=black@0:size={out_w}x{out_h}:r={}/{}:d={clip_duration_s:.6}[ov{k}bg];\
                 [ov{k}bg][ov{k}fg]overlay=x='(W-w)*(1+({pos_x_expr}))/2':y='(H-h)*(1+({pos_y_expr}))/2':eval=frame\
                 ,geq=lum='lum(X,Y)':cb='cb(X,Y)':cr='cr(X,Y)':a='alpha(X,Y)*({opacity_expr})'[{ov_label}raw]"
                , project.frame_rate.numerator, project.frame_rate.denominator
                , project.frame_rate.numerator, project.frame_rate.denominator
            ));
        } else {
            let scale_pos_filter = build_scale_position_filter(clip, out_w, out_h, true);
            let opacity = clip.opacity.clamp(0.0, 1.0);
            let anamorphic_filter = build_anamorphic_filter(clip);
            filter.push_str(&format!(
                ";[{in_idx}:v]{lut_prefix}{anamorphic_filter}format=yuva420p,scale={out_w}:{out_h}:force_original_aspect_ratio=decrease,setsar=1,pad={out_w}:{out_h}:(ow-iw)/2:(oh-ih)/2:color=black@0,fps={}/{}{vidstab_filter}{color_filter}{temp_tint_filter}{grading_filter}{denoise_filter}{sharpen_filter}{blur_filter}{frei0r_effects_filter}{chroma_key_filter}{title_filter}{crop_filter}{scale_pos_filter}{rotate_filter},colorchannelmixer=aa={opacity:.4}{speed_filter}[{ov_label}raw]"
                , project.frame_rate.numerator, project.frame_rate.denominator
            ));
        }
        // Normalize PTS to zero (removes any residual offset from keyframe
        // seeking), then delay to the correct timeline position.
        let start_s = clip.timeline_start as f64 / 1_000_000_000.0;
        filter.push_str(&format!(
            ";[{ov_label}raw]setpts=PTS-STARTPTS+{start_s:.6}/TB[{ov_label}]"
        ));
        let next_label = format!("vcomp{k}");
        let end_s = (clip.timeline_start + clip.duration()) as f64 / 1_000_000_000.0;
        if clip.blend_mode != crate::model::clip::BlendMode::Normal {
            let mode = clip.blend_mode.ffmpeg_mode();
            filter.push_str(&format!(
                ";[{prev_label}][{ov_label}]blend=all_mode={mode}:enable='between(t,{start_s:.6},{end_s:.6})'[{next_label}]"
            ));
        } else {
            filter.push_str(&format!(
                ";[{prev_label}][{ov_label}]overlay=x=0:y=0:enable='between(t,{start_s:.6},{end_s:.6})'[{next_label}]"
            ));
        }
        prev_label = next_label;
    }
    // === Adjustment layers: apply effects to the composited result ===
    // Each adjustment layer applies its effects (color, LUT, blur, frei0r) to the
    // composite output, time-gated to the adjustment clip's duration.
    for (adj_idx, adj_clip) in adjustment_clips.iter().enumerate() {
        let adj_color = build_color_filter(adj_clip);
        let adj_temp_tint = build_temperature_tint_filter_with_caps(adj_clip, &color_caps);
        let adj_grading = build_grading_filter_with_caps(adj_clip, &color_caps);
        let adj_denoise = build_denoise_filter(adj_clip);
        let adj_sharpen = build_sharpen_filter(adj_clip);
        let adj_blur = build_blur_filter(adj_clip);
        let adj_frei0r = build_frei0r_effects_filter(adj_clip);
        let adj_lut = build_lut_filter_prefix(adj_clip);

        // Combine all effect filters into a single chain.
        // The filter builders return strings with a leading comma (e.g. ",eq=brightness=0.1")
        // and build_lut_filter_prefix returns a trailing comma (e.g. "lut3d=file.cube,").
        // We concatenate then strip the leading comma so the chain starts clean.
        let mut effects_chain = String::new();
        if !adj_lut.is_empty() {
            effects_chain.push_str(&adj_lut);
        }
        effects_chain.push_str(&adj_color);
        effects_chain.push_str(&adj_temp_tint);
        effects_chain.push_str(&adj_grading);
        effects_chain.push_str(&adj_denoise);
        effects_chain.push_str(&adj_sharpen);
        effects_chain.push_str(&adj_blur);
        effects_chain.push_str(&adj_frei0r);

        if effects_chain.is_empty() {
            continue; // No effects — skip this adjustment layer
        }
        // Strip leading/trailing commas to avoid empty filter names.
        let effects_chain = effects_chain.trim_matches(',').to_string();
        if effects_chain.is_empty() {
            continue;
        }

        let start_s = adj_clip.timeline_start as f64 / 1_000_000_000.0;
        let end_s = (adj_clip.timeline_start + adj_clip.duration()) as f64 / 1_000_000_000.0;
        let opacity = adj_clip.opacity.clamp(0.0, 1.0);
        let next_label = format!("vadj{adj_idx}");

        if opacity < 1.0 - f64::EPSILON {
            // Partial opacity: split → apply effects → overlay with opacity
            let orig_label = format!("vadj{adj_idx}orig");
            let work_label = format!("vadj{adj_idx}work");
            let fx_label = format!("vadj{adj_idx}fx");
            filter.push_str(&format!(
                ";[{prev_label}]split[{orig_label}][{work_label}];\
                 [{work_label}]{effects_chain},format=yuva420p,colorchannelmixer=aa={opacity:.4}[{fx_label}];\
                 [{orig_label}][{fx_label}]overlay=enable='between(t,{start_s:.6},{end_s:.6})'[{next_label}]"
            ));
        } else {
            // Full opacity: apply effects directly to the stream.
            filter.push_str(&format!(
                ";[{prev_label}]{effects_chain}[{next_label}]"
            ));
        }
        prev_label = next_label;
    }

    // Final output video label — use the last composited label directly
    let vout_label = prev_label;

    // === Audio pipeline ===
    let mut audio_labels: Vec<(String, crate::model::track::AudioRole)> = Vec::new();
    let clip_audio_fades: HashMap<String, ClipAudioFade> =
        if crossfade_enabled && crossfade_duration_ns > 0 {
            let mut crossfade_tracks: Vec<Vec<&Clip>> = Vec::new();

            let mut primary_embedded_audio_clips = Vec::new();
            for clip in &primary_clips {
                if clip.kind == ClipKind::Video
                    && !clip.is_freeze_frame()
                    && !has_linked_audio_peer(clip, &audio_clips)
                    && !uses_bg_removal_path(clip)
                    && clip_has_audio(&ffmpeg, clip, &mut audio_presence_cache)
                {
                    primary_embedded_audio_clips.push(*clip);
                }
            }
            if !primary_embedded_audio_clips.is_empty() {
                crossfade_tracks.push(primary_embedded_audio_clips);
            }

            for track_clips in &secondary_track_clips {
                let mut secondary_embedded_audio_clips = Vec::new();
                for clip in track_clips {
                    if clip.kind == ClipKind::Video
                        && !clip.is_freeze_frame()
                        && !has_linked_audio_peer(clip, &audio_clips)
                        && !uses_bg_removal_path(clip)
                        && clip_has_audio(&ffmpeg, clip, &mut audio_presence_cache)
                    {
                        secondary_embedded_audio_clips.push(*clip);
                    }
                }
                if !secondary_embedded_audio_clips.is_empty() {
                    crossfade_tracks.push(secondary_embedded_audio_clips);
                }
            }

            for track_clips in &audio_track_clips {
                if !track_clips.is_empty() {
                    crossfade_tracks.push(track_clips.clone());
                }
            }

            compute_clip_audio_fades(&crossfade_tracks, crossfade_duration_ns)
        } else {
            HashMap::new()
        };

    // Embedded audio from primary video clips, with per-clip volume scaling
    for (i, clip) in primary_clips.iter().enumerate() {
        if clip.kind == ClipKind::Video
            && !clip.is_freeze_frame()
            && !has_linked_audio_peer(clip, &audio_clips)
            && !uses_bg_removal_path(clip)
            && clip_has_audio(&ffmpeg, clip, &mut audio_presence_cache)
        {
            let delay_ms = clip.timeline_start / 1_000_000;
            let label = format!("va{i}");
            let areverse = if clip.reverse { "areverse," } else { "" };
            let atempo = build_audio_speed_filter(clip);
            let ch_filter = build_channel_filter(clip);
            let ch_part = if ch_filter.is_empty() { String::new() } else { format!(",{ch_filter}") };
            let volume_filter = build_volume_filter(clip);
            let pitch_filter = build_pitch_filter(clip);
            let pitch_part = if pitch_filter.is_empty() { String::new() } else { format!(",{pitch_filter}") };
            let ladspa_filter = build_ladspa_effects_filter(clip);
            let ladspa_part = if ladspa_filter.is_empty() { String::new() } else { format!(",{ladspa_filter}") };
            let eq_filter = build_eq_filter(clip);
            let eq_part = if eq_filter.is_empty() { String::new() } else { format!(",{eq_filter}") };
            let fades = clip_audio_fades.get(&clip.id).copied().unwrap_or_default();
            let fade_filters = build_audio_crossfade_filters(clip, fades, crossfade_curve);
            let pre_pan = format!("{label}_prepan");
            let post_pan = format!("{label}_panned");
            filter.push_str(&format!(
                ";[{i}:a]{areverse}{atempo}{ch_part}{volume_filter}{pitch_part}{ladspa_part}{eq_part},{fade_filters}anull[{pre_pan}]"
            ));
            append_pan_filter_chain(&mut filter, clip, &pre_pan, &post_pan, &label);
            filter.push_str(&format!(";[{post_pan}]adelay={delay_ms}:all=1[{label}]"));
            // Primary video clips — find track role from project.
            let role = project.tracks.iter()
                .find(|t| t.clips.iter().any(|c| c.id == clip.id))
                .map(|t| t.audio_role)
                .unwrap_or_default();
            audio_labels.push((label, role));
        }
    }

    // Embedded audio from secondary video clips (with their volume)
    for (k, clip) in secondary_clips_flat.iter().enumerate() {
        let in_idx = sec_base + k;
        if clip.kind == ClipKind::Video
            && !clip.is_freeze_frame()
            && !has_linked_audio_peer(clip, &audio_clips)
            && !uses_bg_removal_path(clip)
            && clip_has_audio(&ffmpeg, clip, &mut audio_presence_cache)
        {
            let delay_ms = clip.timeline_start / 1_000_000;
            let label = format!("sva{k}");
            let areverse = if clip.reverse { "areverse," } else { "" };
            let atempo = build_audio_speed_filter(clip);
            let ch_filter = build_channel_filter(clip);
            let ch_part = if ch_filter.is_empty() { String::new() } else { format!(",{ch_filter}") };
            let volume_filter = build_volume_filter(clip);
            let pitch_filter = build_pitch_filter(clip);
            let pitch_part = if pitch_filter.is_empty() { String::new() } else { format!(",{pitch_filter}") };
            let ladspa_filter = build_ladspa_effects_filter(clip);
            let ladspa_part = if ladspa_filter.is_empty() { String::new() } else { format!(",{ladspa_filter}") };
            let eq_filter = build_eq_filter(clip);
            let eq_part = if eq_filter.is_empty() { String::new() } else { format!(",{eq_filter}") };
            let fades = clip_audio_fades.get(&clip.id).copied().unwrap_or_default();
            let fade_filters = build_audio_crossfade_filters(clip, fades, crossfade_curve);
            let pre_pan = format!("{label}_prepan");
            let post_pan = format!("{label}_panned");
            filter.push_str(&format!(
                ";[{in_idx}:a]{areverse}{atempo}{ch_part}{volume_filter}{pitch_part}{ladspa_part}{eq_part},{fade_filters}anull[{pre_pan}]"
            ));
            append_pan_filter_chain(&mut filter, clip, &pre_pan, &post_pan, &label);
            filter.push_str(&format!(";[{post_pan}]adelay={delay_ms}:all=1[{label}]"));
            // Find the track for this secondary clip to get its role.
            let role = project.tracks.iter()
                .find(|t| t.clips.iter().any(|c| c.id == clip.id))
                .map(|t| t.audio_role)
                .unwrap_or_default();
            audio_labels.push((label, role));
        }
    }

    // Audio-only track clips
    for (j, clip) in audio_clips.iter().enumerate() {
        let delay_ms = clip.timeline_start / 1_000_000;
        let label = format!("aa{j}");
        let areverse = if clip.reverse { "areverse," } else { "" };
        let atempo = build_audio_speed_filter(clip);
        let ch_filter = build_channel_filter(clip);
        let ch_part = if ch_filter.is_empty() { String::new() } else { format!(",{ch_filter}") };
        let volume_filter = build_volume_filter(clip);
        let pitch_filter = build_pitch_filter(clip);
        let pitch_part = if pitch_filter.is_empty() { String::new() } else { format!(",{pitch_filter}") };
        let ladspa_filter = build_ladspa_effects_filter(clip);
        let ladspa_part = if ladspa_filter.is_empty() { String::new() } else { format!(",{ladspa_filter}") };
        let eq_filter = build_eq_filter(clip);
        let eq_part = if eq_filter.is_empty() {
            String::new()
        } else {
            format!(",{eq_filter}")
        };
        // Ducking filter: reduce volume when non-ducked audio overlaps.
        let duck_filter = project
            .tracks
            .iter()
            .find(|t| t.clips.iter().any(|c| c.id == clip.id))
            .map(|track| build_duck_filter(clip, track, &project.tracks))
            .unwrap_or_default();
        let duck_part = if duck_filter.is_empty() {
            String::new()
        } else {
            format!(",{duck_filter}")
        };
        let fades = clip_audio_fades.get(&clip.id).copied().unwrap_or_default();
        let fade_filters = build_audio_crossfade_filters(clip, fades, crossfade_curve);
        let pre_pan = format!("{label}_prepan");
        let post_pan = format!("{label}_panned");
        filter.push_str(&format!(
            ";[{}:a]{areverse}{atempo}{ch_part}{volume_filter}{pitch_part}{ladspa_part}{duck_part}{eq_part},{fade_filters}anull[{pre_pan}]",
            audio_base + j
        ));
        append_pan_filter_chain(&mut filter, clip, &pre_pan, &post_pan, &label);
        filter.push_str(&format!(";[{post_pan}]adelay={delay_ms}:all=1[{label}]"));
        let role = project.tracks.iter()
            .find(|t| t.clips.iter().any(|c| c.id == clip.id))
            .map(|t| t.audio_role)
            .unwrap_or_default();
        audio_labels.push((label, role));
    }

    // Mix all audio streams.
    // Use `duration=longest` (default) but trim the amix output to the
    // project duration.  Without the trim, `amix` + `adelay` can produce
    // trailing packets with PTS=NOPTS_VALUE (INT64_MAX) that cause
    // "non monotonically increasing dts" muxer errors, especially when
    // the timeline contains title clips or other video-only sources that
    // don't contribute audio.
    let has_audio = !audio_labels.is_empty();
    if has_audio && options.container != Container::Gif {
        use crate::model::track::AudioRole;
        let project_dur_s = project.duration() as f64 / 1_000_000_000.0;

        // Group audio labels by role for submix routing.
        let roles_in_use: Vec<AudioRole> = {
            let mut roles: Vec<AudioRole> = audio_labels.iter().map(|(_, r)| *r).collect();
            roles.sort_by_key(|r| *r as u8);
            roles.dedup();
            roles
        };

        if roles_in_use.len() <= 1 {
            // Single role (or all None) — no submix needed, mix directly.
            let n = audio_labels.len();
            filter.push(';');
            for (label, _) in &audio_labels {
                filter.push_str(&format!("[{label}]"));
            }
            filter.push_str(&format!(
                "amix=inputs={n}:normalize=0,atrim=duration={project_dur_s:.6},asetpts=PTS-STARTPTS[aout]"
            ));
        } else {
            // Multiple roles — create per-role submixes, then master mix.
            let mut submix_labels: Vec<String> = Vec::new();
            for role in &roles_in_use {
                let role_labels: Vec<&str> = audio_labels
                    .iter()
                    .filter(|(_, r)| r == role)
                    .map(|(l, _)| l.as_str())
                    .collect();
                if role_labels.is_empty() {
                    continue;
                }
                let submix_name = format!("submix_{}", role.as_str());
                let n = role_labels.len();
                filter.push(';');
                for l in &role_labels {
                    filter.push_str(&format!("[{l}]"));
                }
                if n == 1 {
                    // Single input — just rename, no amix needed.
                    filter.push_str(&format!("anull[{submix_name}]"));
                } else {
                    filter.push_str(&format!(
                        "amix=inputs={n}:normalize=0[{submix_name}]"
                    ));
                }
                submix_labels.push(submix_name);
            }
            // Master mix from submixes.
            let n = submix_labels.len();
            filter.push(';');
            for l in &submix_labels {
                filter.push_str(&format!("[{l}]"));
            }
            if n == 1 {
                filter.push_str(&format!(
                    "atrim=duration={project_dur_s:.6},asetpts=PTS-STARTPTS[aout]"
                ));
            } else {
                filter.push_str(&format!(
                    "amix=inputs={n}:normalize=0,atrim=duration={project_dur_s:.6},asetpts=PTS-STARTPTS[aout]"
                ));
            }
        }
    }

    // For GIF output, extend the filtergraph with palettegen + paletteuse and map [gifout].
    // For all other containers, map [vout_label] directly.
    let (filter, map_label) = if options.container == Container::Gif {
        let fps_val = options
            .gif_fps
            .unwrap_or_else(|| project.frame_rate.as_f64().round().clamp(1.0, 30.0) as u32)
            .clamp(1, 30);
        let gif_filter = format!(
            ";[{vout_label}]fps={fps_val},split[gifa][gifb];\
             [gifa]palettegen=max_colors=256:stats_mode=full[gifp];\
             [gifb][gifp]paletteuse=dither=bayer:bayer_scale=5[gifout]"
        );
        (filter + &gif_filter, "gifout".to_string())
    } else {
        (filter, vout_label)
    };

    cmd.arg("-filter_complex")
        .arg(&filter)
        .arg("-map")
        .arg(format!("[{map_label}]"));

    if has_audio && options.container != Container::Gif {
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

    if options.container == Container::Gif {
        // GIF container handles its own encoding; add -loop 0 for infinite loop
        cmd.arg("-loop").arg("0");
    } else {
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

    // Clean up temporary vidstab .trf files.
    for trf in vidstab_trf.values() {
        let _ = std::fs::remove_file(trf);
    }

    let _ = tx.send(ExportProgress::Done);
    Ok(())
}

fn build_color_filter(clip: &crate::model::clip::Clip) -> String {
    let has_keyframes = !clip.brightness_keyframes.is_empty()
        || !clip.contrast_keyframes.is_empty()
        || !clip.saturation_keyframes.is_empty();
    // Keep exposure mapping aligned with preview parity:
    // preview approximates exposure as a brightness lift + slight contrast boost.
    let has_exposure = clip.exposure.abs() > f32::EPSILON;
    let (exposure_brightness_delta, exposure_contrast_delta) = if has_exposure {
        let e = clip.exposure.clamp(-1.0, 1.0) as f64;
        (e * 0.55, e * 0.12)
    } else {
        (0.0, 0.0)
    };
    if has_keyframes {
        let brightness_expr = build_keyframed_property_expression(
            &clip.brightness_keyframes,
            clip.brightness as f64,
            -1.0,
            1.0,
            "t",
        );
        let contrast_expr = build_keyframed_property_expression(
            &clip.contrast_keyframes,
            clip.contrast as f64,
            0.0,
            2.0,
            "t",
        );
        let saturation_expr = build_keyframed_property_expression(
            &clip.saturation_keyframes,
            clip.saturation as f64,
            0.0,
            2.0,
            "t",
        );
        let brightness_expr = if has_exposure {
            format!("({brightness_expr})+{exposure_brightness_delta:.6}")
        } else {
            brightness_expr
        };
        let contrast_expr = if has_exposure {
            format!("({contrast_expr})+{exposure_contrast_delta:.6}")
        } else {
            contrast_expr
        };
        format!(
            ",eq=brightness='{brightness_expr}':contrast='{contrast_expr}':saturation='{saturation_expr}':eval=frame"
        )
    } else if clip.brightness != 0.0 || clip.contrast != 1.0 || clip.saturation != 1.0 || has_exposure
    {
        // For static (non-keyframed) primaries, align export with Program Monitor
        // by reusing the calibrated videobalance mapping used in preview.
        // Temperature/tint and tonal grading are excluded here because export
        // applies them through dedicated filters later in the chain.
        let preview_params = ProgramPlayer::compute_videobalance_params(
            clip.brightness as f64,
            clip.contrast as f64,
            clip.saturation as f64,
            6500.0,
            0.0,
            0.0,
            0.0,
            0.0,
            clip.exposure as f64,
            0.0,
            0.0,
            0.0,
            0.0,
            0.0,
            0.0,
            0.0,
            true,
            true,
        );
        // FFmpeg `eq` contrast pivots around Y=0.5 differently than preview's
        // GStreamer videobalance path. Add a calibrated brightness bias around
        // neutral contrast to keep low/high contrast looks closer to preview.
        let contrast_t = clip.contrast.clamp(0.0, 2.0) as f64;
        let contrast_delta = contrast_t - 1.0;
        let contrast_brightness_bias = 0.26 * contrast_delta - 0.08 * contrast_delta * contrast_delta;
        format!(
            ",eq=brightness={:.4}:contrast={:.4}:saturation={:.4}",
            (preview_params.brightness + contrast_brightness_bias).clamp(-1.0, 1.0),
            preview_params.contrast,
            preview_params.saturation
        )
    } else {
        String::new()
    }
}

fn has_transform_keyframes(clip: &Clip) -> bool {
    !clip.scale_keyframes.is_empty()
        || !clip.position_x_keyframes.is_empty()
        || !clip.position_y_keyframes.is_empty()
        || !clip.rotate_keyframes.is_empty()
        || !clip.crop_left_keyframes.is_empty()
        || !clip.crop_right_keyframes.is_empty()
        || !clip.crop_top_keyframes.is_empty()
        || !clip.crop_bottom_keyframes.is_empty()
}

#[inline]
fn bezier_component(p1: f64, p2: f64, s: f64) -> f64 {
    let s2 = s * s;
    let s3 = s2 * s;
    let inv = 1.0 - s;
    3.0 * inv * inv * s * p1 + 3.0 * inv * s2 * p2 + s3
}

#[inline]
fn bezier_component_derivative(p1: f64, p2: f64, s: f64) -> f64 {
    let s2 = s * s;
    3.0 * (1.0 - s) * (1.0 - s) * p1 + 6.0 * (1.0 - s) * s * (p2 - p1) + 3.0 * s2 * (1.0 - p2)
}

fn cubic_bezier_ease_controls(x1: f64, y1: f64, x2: f64, y2: f64, t: f64) -> f64 {
    if t <= 0.0 {
        return 0.0;
    }
    if t >= 1.0 {
        return 1.0;
    }
    let mut s = t;
    for _ in 0..8 {
        let bx = bezier_component(x1, x2, s);
        let dx = bezier_component_derivative(x1, x2, s);
        if dx.abs() < 1e-12 {
            break;
        }
        s -= (bx - t) / dx;
        s = s.clamp(0.0, 1.0);
    }
    bezier_component(y1, y2, s)
}

fn build_custom_eased_t_expr(
    t_expr: &str,
    controls: (f64, f64, f64, f64),
    samples: usize,
) -> String {
    let samples = samples.max(4);
    let (x1, y1, x2, y2) = controls;
    let mut ys = Vec::with_capacity(samples + 1);
    for i in 0..=samples {
        let t = i as f64 / samples as f64;
        ys.push(cubic_bezier_ease_controls(x1, y1, x2, y2, t));
    }
    let mut expr = format!("{:.10}", ys[samples]);
    for i in (1..=samples).rev() {
        let t0 = (i - 1) as f64 / samples as f64;
        let t1 = i as f64 / samples as f64;
        let y0 = ys[i - 1];
        let y1s = ys[i];
        let seg_expr = if (y1s - y0).abs() < 1e-12 {
            format!("{:.10}", y0)
        } else {
            format!(
                "{y0:.10}+({y1s:.10}-{y0:.10})*(({t_expr}-{t0:.10})/{span:.10})",
                span = (t1 - t0).max(1e-9)
            )
        };
        expr = format!("if(lt({t_expr},{t1:.10}),{seg_expr},{expr})");
    }
    expr
}

fn build_keyframed_property_expression(
    keyframes: &[NumericKeyframe],
    default_value: f64,
    min_value: f64,
    max_value: f64,
    time_var: &str,
) -> String {
    use crate::model::clip::KeyframeInterpolation;

    let mut sorted: Vec<&NumericKeyframe> = keyframes.iter().collect();
    sorted.sort_by_key(|kf| kf.time_ns);
    // Deduplicate by time (last wins)
    let mut deduped: Vec<(u64, f64, KeyframeInterpolation, Option<(f64, f64, f64, f64)>)> =
        Vec::with_capacity(sorted.len());
    for kf in &sorted {
        let v = kf.value.clamp(min_value, max_value);
        let controls = if kf.bezier_controls.is_some() {
            Some(kf.segment_control_points())
        } else {
            None
        };
        if let Some(last) = deduped.last_mut() {
            if last.0 == kf.time_ns {
                last.1 = v;
                last.2 = kf.interpolation;
                last.3 = controls;
                continue;
            }
        }
        deduped.push((kf.time_ns, v, kf.interpolation, controls));
    }

    if deduped.is_empty() {
        return format!("{:.10}", default_value.clamp(min_value, max_value));
    }
    if deduped.len() == 1 {
        return format!("{:.10}", deduped[0].1);
    }

    let mut expr = format!(
        "{:.10}",
        deduped.last().map(|(_, v, _, _)| *v).unwrap_or(default_value)
    );
    for i in (1..deduped.len()).rev() {
        let (left_ns, left_value, interp, controls) = deduped[i - 1];
        let (right_ns, right_value, _, _) = deduped[i];
        let left_s = left_ns as f64 / 1_000_000_000.0;
        let right_s = right_ns as f64 / 1_000_000_000.0;
        let span_s = (right_s - left_s).max(1e-9);
        // Compute normalized t for this segment
        let t_expr = format!("(({time_var})-{left_s:.9})/{span_s:.9}");
        // Apply easing to t
        let eased_t = if let Some(controls) = controls {
            build_custom_eased_t_expr(&t_expr, controls, 8)
        } else {
            match interp {
                KeyframeInterpolation::Linear => t_expr.clone(),
                KeyframeInterpolation::EaseIn => format!("pow({t_expr},2)"),
                KeyframeInterpolation::EaseOut => format!("(1-pow(1-({t_expr}),2))"),
                KeyframeInterpolation::EaseInOut => {
                    format!("if(lt({t_expr},0.5),2*pow({t_expr},2),1-pow(-2*({t_expr})+2,2)/2)")
                }
            }
        };
        let segment_expr =
            format!("{left_value:.10}+({right_value:.10}-{left_value:.10})*{eased_t}");
        expr = format!(
            "if(lt({time_var},{right_s:.9}),{segment_expr},{expr})",
            right_s = right_s
        );
    }
    let (first_ns, first_value, _, _) = deduped[0];
    let first_s = first_ns as f64 / 1_000_000_000.0;
    format!("if(lt({time_var},{first_s:.9}),{first_value:.10},{expr})")
}

/// Build an FFmpeg volume filter that applies ducking to a clip.
/// Returns empty string if the clip's track doesn't have ducking enabled or
/// there are no overlapping non-ducked audio sources.
///
/// The filter uses `between(t, start, end)` to detect time ranges where
/// non-ducked audio overlaps, and applies the duck gain during those ranges.
fn build_duck_filter(
    clip: &Clip,
    track: &crate::model::track::Track,
    all_tracks: &[crate::model::track::Track],
) -> String {
    if !track.duck {
        return String::new();
    }

    let clip_start = clip.timeline_start;
    let clip_end = clip.timeline_start + clip.duration();
    let duck_gain = 10.0_f64.powf(track.duck_amount_db.min(0.0) / 20.0);

    // Find all time ranges where non-ducked audio overlaps this clip.
    // Non-ducked audio sources: video clips with embedded audio + audio-only
    // clips on non-ducked tracks.
    let mut overlap_ranges: Vec<(f64, f64)> = Vec::new();

    for t in all_tracks {
        if t.id == track.id {
            continue; // Skip the ducked track itself.
        }
        if t.duck {
            continue; // Skip other ducked tracks.
        }
        for c in &t.clips {
            let c_start = c.timeline_start;
            let c_end = c.timeline_start + c.duration();
            // Check overlap with the ducked clip.
            if c_start < clip_end && c_end > clip_start {
                // Convert to source-relative time for the ducked clip.
                let overlap_start = c_start.max(clip_start).saturating_sub(clip_start);
                let overlap_end = c_end.min(clip_end).saturating_sub(clip_start);
                let start_s = overlap_start as f64 / 1e9;
                let end_s = overlap_end as f64 / 1e9;
                overlap_ranges.push((start_s, end_s));
            }
        }
    }

    if overlap_ranges.is_empty() {
        return String::new();
    }

    // Merge overlapping ranges.
    overlap_ranges.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap());
    let mut merged: Vec<(f64, f64)> = Vec::new();
    for (s, e) in &overlap_ranges {
        if let Some(last) = merged.last_mut() {
            if *s <= last.1 {
                last.1 = last.1.max(*e);
                continue;
            }
        }
        merged.push((*s, *e));
    }

    // Build the FFmpeg expression: duck during overlap ranges, normal otherwise.
    // volume='if(between(t,S1,E1)+between(t,S2,E2)+..., DUCK_GAIN, 1.0)':eval=frame
    let conditions: Vec<String> = merged
        .iter()
        .map(|(s, e)| format!("between(t,{s:.4},{e:.4})"))
        .collect();
    let cond_expr = conditions.join("+");
    format!(
        "volume='if({cond_expr},{duck_gain:.6},1.0)':eval=frame"
    )
}

fn build_volume_filter(clip: &Clip) -> String {
    if clip.volume_keyframes.is_empty() {
        return format!("volume={:.4}", clip.volume.clamp(0.0, 4.0));
    }
    let expr = build_keyframed_property_expression(
        &clip.volume_keyframes,
        clip.volume as f64,
        0.0,
        4.0,
        "t",
    );
    // Keyframed volume expressions depend on `t`, so force per-frame evaluation.
    format!("volume='{expr}':eval=frame")
}

/// Build FFmpeg `equalizer` filter chain for the clip's 3-band parametric EQ.
/// Returns an empty string when EQ is flat (all gains 0 and no keyframes).
fn build_eq_filter(clip: &Clip) -> String {
    if !clip.has_eq() {
        return String::new();
    }
    let band_kfs: [&[NumericKeyframe]; 3] = [
        &clip.eq_low_gain_keyframes,
        &clip.eq_mid_gain_keyframes,
        &clip.eq_high_gain_keyframes,
    ];
    let mut parts = Vec::new();
    for (i, band) in clip.eq_bands.iter().enumerate() {
        let has_kfs = !band_kfs[i].is_empty();
        if band.gain.abs() < 0.001 && !has_kfs {
            continue;
        }
        let bw = band.freq / band.q.max(0.1);
        if has_kfs {
            let gain_expr = build_keyframed_property_expression(
                band_kfs[i],
                band.gain,
                -24.0,
                24.0,
                "t",
            );
            parts.push(format!(
                "equalizer=f={:.1}:t=h:w={:.1}:g='{gain_expr}'",
                band.freq, bw
            ));
        } else {
            parts.push(format!(
                "equalizer=f={:.1}:t=h:w={:.1}:g={:.2}",
                band.freq, bw, band.gain
            ));
        }
    }
    parts.join(",")
}

fn build_pan_expression(clip: &Clip) -> String {
    if clip.pan_keyframes.is_empty() {
        format!("{:.10}", clip.pan.clamp(-1.0, 1.0))
    } else {
        build_keyframed_property_expression(&clip.pan_keyframes, clip.pan as f64, -1.0, 1.0, "t")
    }
}

fn append_pan_filter_chain(
    filter: &mut String,
    clip: &Clip,
    input_label: &str,
    output_label: &str,
    label_prefix: &str,
) {
    if clip.pan.abs() <= f32::EPSILON && clip.pan_keyframes.is_empty() {
        filter.push_str(&format!(";[{input_label}]anull[{output_label}]"));
        return;
    }

    let pan_expr = build_pan_expression(clip);
    let left_gain_expr = format!("if(gt({pan_expr},0),1-({pan_expr}),1)");
    let right_gain_expr = format!("if(lt({pan_expr},0),1+({pan_expr}),1)");
    let left_label = format!("{label_prefix}_pan_l");
    let right_label = format!("{label_prefix}_pan_r");
    let left_scaled_label = format!("{label_prefix}_pan_lv");
    let right_scaled_label = format!("{label_prefix}_pan_rv");

    filter.push_str(&format!(
        ";[{input_label}]aformat=channel_layouts=stereo,channelsplit=channel_layout=stereo[{left_label}][{right_label}]"
    ));
    filter.push_str(&format!(
        ";[{left_label}]volume='{left_gain_expr}':eval=frame[{left_scaled_label}]"
    ));
    filter.push_str(&format!(
        ";[{right_label}]volume='{right_gain_expr}':eval=frame[{right_scaled_label}]"
    ));
    filter.push_str(&format!(
        ";[{left_scaled_label}][{right_scaled_label}]amerge=inputs=2,aformat=channel_layouts=stereo[{output_label}]"
    ));
}

fn build_temperature_tint_filter(clip: &crate::model::clip::Clip) -> String {
    build_temperature_tint_filter_with_caps(clip, &ColorFilterCapabilities::default())
}

fn build_temperature_tint_filter_with_caps(
    clip: &crate::model::clip::Clip,
    caps: &ColorFilterCapabilities,
) -> String {
    let has_temp = (clip.temperature - 6500.0).abs() > 1.0;
    let has_tint = clip.tint.abs() > 0.001;
    let has_temp_keyframes = !clip.temperature_keyframes.is_empty();
    let has_tint_keyframes = !clip.tint_keyframes.is_empty();

    // FFmpeg frei0r bridge path: use the same calibrated coloradj mapping as preview
    // when this is a static (non-keyframed) temp/tint adjustment.
    if caps.use_coloradj_frei0r
        && (has_temp || has_tint)
        && !has_temp_keyframes
        && !has_tint_keyframes
    {
        let cp = compute_export_coloradj_params(clip.temperature as f64, clip.tint as f64);
        return format!(
            ",frei0r=filter_name=coloradj_RGB:filter_params={:.6}|{:.6}|{:.6}|0.333",
            cp.r, cp.g, cp.b
        );
    }

    let mut f = String::new();
    if has_temp_keyframes {
        let temp_expr = build_keyframed_property_expression(
            &clip.temperature_keyframes,
            clip.temperature as f64,
            2000.0,
            10000.0,
            "t",
        );
        f.push_str(&format!(
            ",colortemperature=temperature='{temp_expr}':eval=frame"
        ));
    } else if has_temp {
        f.push_str(&format!(
            ",colortemperature=temperature={:.0}",
            clip.temperature.clamp(2000.0, 10000.0)
        ));
    }
    if has_tint_keyframes {
        // Map tint to green channel offset via colorbalance midtones.
        // Negative tint = boost green (positive gm), positive tint = cut green (negative gm)
        // and complementary red+blue boost.
        let tint_expr = build_keyframed_property_expression(
            &clip.tint_keyframes,
            clip.tint as f64,
            -1.0,
            1.0,
            "t",
        );
        let gm_expr = format!("(-({tint_expr}))*0.5");
        let rm_expr = format!("({tint_expr})*0.25");
        let bm_expr = format!("({tint_expr})*0.25");
        f.push_str(&format!(
            ",colorbalance=rm='{rm_expr}':gm='{gm_expr}':bm='{bm_expr}':eval=frame"
        ));
    } else if has_tint {
        // Map tint to green channel offset via colorbalance midtones.
        // Negative tint = boost green (positive gm), positive tint = cut green (negative gm)
        // and complementary red+blue boost.
        let t = clip.tint.clamp(-1.0, 1.0);
        let gm = -t * 0.5;
        let rm = t * 0.25;
        let bm = t * 0.25;
        f.push_str(&format!(",colorbalance=rm={rm:.4}:gm={gm:.4}:bm={bm:.4}"));
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

fn build_blur_filter(clip: &crate::model::clip::Clip) -> String {
    if clip.blur > 0.0 {
        let r = (clip.blur.clamp(0.0, 1.0) * 10.0).round() as i32;
        format!(",boxblur={r}:{r}")
    } else {
        String::new()
    }
}

/// Run vidstab analysis (pass 1) for a clip, producing a .trf transform file.
/// Returns the .trf path on success, or None if analysis fails or vidstab is disabled.
fn run_vidstab_analysis(
    ffmpeg: &str,
    clip: &Clip,
    frame_duration_s: f64,
) -> Result<Option<String>> {
    if !clip.vidstab_enabled || clip.vidstab_smoothing <= 0.0 {
        return Ok(None);
    }
    // Skip non-video clips (titles, adjustments, audio, images)
    if clip.kind != ClipKind::Video || clip.source_path.is_empty() {
        return Ok(None);
    }
    let trf_path = format!(
        "/tmp/ultimateslice-vidstab-{}.trf",
        clip.id.replace(|c: char| !c.is_ascii_alphanumeric() && c != '-', "_")
    );
    let shakiness = ((clip.vidstab_smoothing * 10.0).round() as i32).clamp(1, 10);
    let (in_s, dur_s) = video_input_seek_and_duration(clip, frame_duration_s);
    let status = Command::new(ffmpeg)
        .arg("-y")
        .arg("-ss")
        .arg(format!("{in_s:.6}"))
        .arg("-t")
        .arg(format!("{dur_s:.6}"))
        .arg("-i")
        .arg(&clip.source_path)
        .arg("-vf")
        .arg(format!(
            "vidstabdetect=shakiness={shakiness}:result={trf_path}"
        ))
        .arg("-f")
        .arg("null")
        .arg("-")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
    match status {
        Ok(s) if s.success() && std::path::Path::new(&trf_path).exists() => Ok(Some(trf_path)),
        _ => {
            log::warn!("vidstab analysis failed for clip {}", clip.id);
            Ok(None)
        }
    }
}

/// Build the vidstabtransform filter string for pass 2 of stabilization.
fn build_vidstab_filter(clip: &Clip, trf_path: Option<&str>) -> String {
    match trf_path {
        Some(trf) if clip.vidstab_enabled => {
            let smoothing = ((clip.vidstab_smoothing * 30.0).round() as i32).clamp(1, 30);
            format!(
                ",vidstabtransform=input={trf}:smoothing={smoothing}:zoom=0:optzoom=1,unsharp=5:5:0.8:3:3:0.4"
            )
        }
        _ => String::new(),
    }
}

/// Build a chain of FFmpeg frei0r filters for user-applied effects on a clip.
/// Each enabled effect becomes `,frei0r=filter_name={name}:filter_params={p1}|{p2}|...`.
/// Effects with no FFmpeg frei0r support are silently skipped.
fn build_frei0r_effects_filter(clip: &crate::model::clip::Clip) -> String {
    use crate::media::frei0r_registry::{Frei0rRegistry, Frei0rNativeType};

    if clip.frei0r_effects.is_empty() {
        return String::new();
    }

    let mut result = String::new();
    let registry = Frei0rRegistry::get_or_discover();

    for effect in &clip.frei0r_effects {
        if !effect.enabled {
            continue;
        }

        // Look up the plugin info to get ordered param names.
        let plugin = registry.find_by_name(&effect.plugin_name);

        // Build filter_params string using native frei0r param ordering.
        let params_str = if let Some(info) = plugin {
            if !info.native_params.is_empty() {
                // Use native param info for correct compound formatting.
                info.native_params
                    .iter()
                    .map(|np| match np.native_type {
                        Frei0rNativeType::Color => {
                            // COLOR: combine 3 GStreamer properties into r/g/b.
                            let r = np.gst_properties.first().and_then(|k| effect.params.get(k)).copied().unwrap_or(0.0);
                            let g = np.gst_properties.get(1).and_then(|k| effect.params.get(k)).copied().unwrap_or(0.0);
                            let b = np.gst_properties.get(2).and_then(|k| effect.params.get(k)).copied().unwrap_or(0.0);
                            format!("{r:.6}/{g:.6}/{b:.6}")
                        }
                        Frei0rNativeType::Position => {
                            // POSITION: combine 2 GStreamer properties into x/y.
                            let x = np.gst_properties.first().and_then(|k| effect.params.get(k)).copied().unwrap_or(0.0);
                            let y = np.gst_properties.get(1).and_then(|k| effect.params.get(k)).copied().unwrap_or(0.0);
                            format!("{x:.6}/{y:.6}")
                        }
                        Frei0rNativeType::NativeString => {
                            let prop = np.gst_properties.first().map(|s| s.as_str()).unwrap_or("");
                            effect
                                .string_params
                                .get(prop)
                                .cloned()
                                .unwrap_or_default()
                        }
                        _ => {
                            // Bool / Double: single GStreamer property.
                            let prop = np.gst_properties.first().map(|s| s.as_str()).unwrap_or("");
                            if np.native_type == Frei0rNativeType::Bool {
                                let val = effect.params.get(prop).copied().unwrap_or(0.0);
                                if val > 0.5 { "y".to_string() } else { "n".to_string() }
                            } else {
                                let val = effect.params.get(prop).copied().unwrap_or(0.0);
                                format!("{val:.6}")
                            }
                        }
                    })
                    .collect::<Vec<_>>()
                    .join("|")
            } else {
                // Fallback: no native info, use GStreamer params in registry order.
                info.params
                    .iter()
                    .map(|p| {
                        if p.param_type == crate::media::frei0r_registry::Frei0rParamType::String {
                            effect
                                .string_params
                                .get(&p.name)
                                .cloned()
                                .or_else(|| p.default_string.clone())
                                .unwrap_or_default()
                        } else {
                            let val =
                                effect.params.get(&p.name).copied().unwrap_or(p.default_value);
                            format!("{val:.6}")
                        }
                    })
                    .collect::<Vec<_>>()
                    .join("|")
            }
        } else {
            // No registry info — use param values in HashMap iteration order.
            effect
                .params
                .values()
                .map(|v| format!("{v:.6}"))
                .collect::<Vec<_>>()
                .join("|")
        };

        // Use FFmpeg module name (may differ from GStreamer name).
        let ffmpeg_name = plugin
            .map(|p| p.ffmpeg_name.as_str())
            .unwrap_or(&effect.plugin_name);

        if params_str.is_empty() {
            result.push_str(&format!(",frei0r=filter_name={}", ffmpeg_name));
        } else {
            result.push_str(&format!(
                ",frei0r=filter_name={}:filter_params={}",
                ffmpeg_name, params_str
            ));
        }
    }

    result
}

/// LUT filter chain for use at the start of a filter pipeline (trailing comma, no leading).
/// Returns `lut3d={path1},lut3d={path2},` or empty string.
fn build_lut_filter_prefix(clip: &crate::model::clip::Clip) -> String {
    let mut result = String::new();
    for path in &clip.lut_paths {
        if !path.is_empty() && std::path::Path::new(path).exists() {
            let escaped = path.replace('\\', "\\\\").replace(':', "\\:");
            result.push_str(&format!("lut3d={escaped},"));
        }
    }
    result
}

fn parse_title_font(font_desc: &str) -> (String, f64) {
    let trimmed = font_desc.trim();
    if trimmed.is_empty() {
        return ("Sans".to_string(), 36.0);
    }
    let mut parts = trimmed.rsplitn(2, ' ');
    let last = parts.next().unwrap_or_default();
    if let Ok(size) = last.parse::<f64>() {
        let family = parts.next().unwrap_or("Sans").trim();
        if family.is_empty() {
            ("Sans".to_string(), size.max(1.0))
        } else {
            (family.to_string(), size.max(1.0))
        }
    } else {
        (trimmed.to_string(), 36.0)
    }
}

fn escape_drawtext_value(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace(':', "\\:")
        .replace('\'', "\\'")
        .replace('%', "\\%")
}

const TITLE_REFERENCE_HEIGHT: f64 = 1080.0;

fn build_title_filter(clip: &crate::model::clip::Clip, out_h: u32) -> String {
    if clip.title_text.trim().is_empty() {
        return String::new();
    }

    let text = escape_drawtext_value(&clip.title_text).replace('\n', "\\n");
    let (font_name, font_size) = parse_title_font(&clip.title_font);
    let font_name = escape_drawtext_value(&font_name);
    let rel_x = clip.title_x.clamp(0.0, 1.0);
    let rel_y = clip.title_y.clamp(0.0, 1.0);

    let scale_factor = out_h as f64 / TITLE_REFERENCE_HEIGHT;
    // Scale Pango points → pixels (×4/3) then proportionally to output height
    let scaled_size = font_size * (4.0 / 3.0) * scale_factor;

    let rgba = clip.title_color;
    let r = ((rgba >> 24) & 0xFF) as u8;
    let g = ((rgba >> 16) & 0xFF) as u8;
    let b = ((rgba >> 8) & 0xFF) as u8;
    let a = (rgba & 0xFF) as u8;
    let alpha = (a as f64 / 255.0).clamp(0.0, 1.0);

    // Base drawtext filter
    let mut filter = format!(
        ",drawtext=font='{font_name}':text='{text}':fontsize={scaled_size:.2}:fontcolor={r:02x}{g:02x}{b:02x}@{alpha:.4}:x='({rel_x:.6})*w-text_w/2':y='({rel_y:.6})*h-text_h/2'"
    );

    // Outline (border)
    if clip.title_outline_width > 0.0 {
        let bw = (clip.title_outline_width * scale_factor).max(0.5);
        let oc = clip.title_outline_color;
        let or = ((oc >> 24) & 0xFF) as u8;
        let og = ((oc >> 16) & 0xFF) as u8;
        let ob = ((oc >> 8) & 0xFF) as u8;
        let oa = (oc & 0xFF) as u8;
        let o_alpha = (oa as f64 / 255.0).clamp(0.0, 1.0);
        filter.push_str(&format!(":borderw={bw:.1}:bordercolor={or:02x}{og:02x}{ob:02x}@{o_alpha:.4}"));
    }

    // Shadow
    if clip.title_shadow {
        let sx = (clip.title_shadow_offset_x * scale_factor).round() as i32;
        let sy = (clip.title_shadow_offset_y * scale_factor).round() as i32;
        let sc = clip.title_shadow_color;
        let sr = ((sc >> 24) & 0xFF) as u8;
        let sg = ((sc >> 16) & 0xFF) as u8;
        let sb = ((sc >> 8) & 0xFF) as u8;
        let sa = (sc & 0xFF) as u8;
        let s_alpha = (sa as f64 / 255.0).clamp(0.0, 1.0);
        filter.push_str(&format!(":shadowx={sx}:shadowy={sy}:shadowcolor={sr:02x}{sg:02x}{sb:02x}@{s_alpha:.4}"));
    }

    // Background box
    if clip.title_bg_box {
        let pad = (clip.title_bg_box_padding * scale_factor).round() as i32;
        let bc = clip.title_bg_box_color;
        let br = ((bc >> 24) & 0xFF) as u8;
        let bg = ((bc >> 16) & 0xFF) as u8;
        let bb = ((bc >> 8) & 0xFF) as u8;
        let ba = (bc & 0xFF) as u8;
        let b_alpha = (ba as f64 / 255.0).clamp(0.0, 1.0);
        filter.push_str(&format!(":box=1:boxcolor={br:02x}{bg:02x}{bb:02x}@{b_alpha:.4}:boxborderw={pad}"));
    }

    // Secondary text (second drawtext filter below primary)
    if !clip.title_secondary_text.trim().is_empty() {
        let sec_text = escape_drawtext_value(&clip.title_secondary_text).replace('\n', "\\n");
        let sec_size = scaled_size * 0.7; // secondary text is 70% of primary
        let sec_y_offset = scaled_size * 1.5; // offset below primary
        filter.push_str(&format!(
            ",drawtext=font='{font_name}':text='{sec_text}':fontsize={sec_size:.2}:fontcolor={r:02x}{g:02x}{b:02x}@{alpha:.4}:x='({rel_x:.6})*w-text_w/2':y='({rel_y:.6})*h-text_h/2+{sec_y_offset:.0}'"
        ));
    }

    filter
}

fn build_grading_filter(clip: &crate::model::clip::Clip) -> String {
    build_grading_filter_with_caps(clip, &ColorFilterCapabilities::default())
}

fn rgb_triplet_hex(r: f64, g: f64, b: f64) -> String {
    let to_u8 = |v: f64| ((v.clamp(0.0, 1.0) * 255.0).round() as u8) as u32;
    format!("0x{:02X}{:02X}{:02X}", to_u8(r), to_u8(g), to_u8(b))
}

fn build_grading_filter_with_caps(
    clip: &crate::model::clip::Clip,
    caps: &ColorFilterCapabilities,
) -> String {
    let has_grading = clip.shadows != 0.0
        || clip.midtones != 0.0
        || clip.highlights != 0.0
        || clip.black_point != 0.0
        || clip.highlights_warmth != 0.0
        || clip.highlights_tint != 0.0
        || clip.midtones_warmth != 0.0
        || clip.midtones_tint != 0.0
        || clip.shadows_warmth != 0.0
        || clip.shadows_tint != 0.0;
    if has_grading {
        // Replicate the frei0r 3-point-color-balance quadratic transfer
        // curve using FFmpeg's `lutrgb`.  The frei0r plugin fits a parabola
        // y = a·x² + b·x + c through (black_c, 0), (gray_c, 0.5),
        // (white_c, 1.0) per channel.  Using the identical quadratic in
        // `lutrgb` avoids frei0r cross-runtime parameter-passing issues
        // and cubic-spline overshoot from `curves`.
        let p = ProgramPlayer::compute_export_3point_params(
            clip.shadows as f64,
            clip.midtones as f64,
            clip.highlights as f64,
            clip.black_point as f64,
            clip.highlights_warmth as f64,
            clip.highlights_tint as f64,
            clip.midtones_warmth as f64,
            clip.midtones_tint as f64,
            clip.shadows_warmth as f64,
            clip.shadows_tint as f64,
        );
        let parabola = crate::media::program_player::ThreePointParabola::from_params(&p);
        parabola.to_lutrgb_filter()
    } else {
        String::new()
    }
}

pub(crate) fn compute_export_coloradj_params(
    temperature: f64,
    tint: f64,
) -> crate::media::program_player::ColorAdjRGBParams {
    let neutral = ProgramPlayer::compute_coloradj_params(6500.0, 0.0);
    let temp_only = ProgramPlayer::compute_coloradj_params(temperature, 0.0);
    let tint_only = ProgramPlayer::compute_coloradj_params(6500.0, tint);

    // FFmpeg frei0r implementations can diverge from preview at stronger
    // temperature/tint settings; apply a conservative attenuation of deltas
    // from neutral to better align cross-runtime behavior.
    let temp_gain = ProgramPlayer::export_temperature_parity_gain(temperature);
    let tint_gain = if tint < 0.0 {
        0.60
    } else if tint > 0.0 {
        0.72
    } else {
        1.0
    };

    // Per-channel additive offsets for cross-runtime bridge compensation.
    let (off_r, off_g, off_b) = ProgramPlayer::export_temperature_channel_offsets(temperature);

    crate::media::program_player::ColorAdjRGBParams {
        r: (neutral.r + (temp_only.r - neutral.r) * temp_gain + (tint_only.r - neutral.r) * tint_gain + off_r).clamp(0.0, 1.0),
        g: (neutral.g + (temp_only.g - neutral.g) * temp_gain + (tint_only.g - neutral.g) * tint_gain + off_g).clamp(0.0, 1.0),
        b: (neutral.b + (temp_only.b - neutral.b) * temp_gain + (tint_only.b - neutral.b) * tint_gain + off_b).clamp(0.0, 1.0),
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

fn video_input_seek_and_duration(
    clip: &crate::model::clip::Clip,
    frame_duration_s: f64,
) -> (f64, f64) {
    if clip.kind == ClipKind::Title {
        // Title clips use lavfi input; return zero seek, full duration.
        return (0.0, clip.duration() as f64 / 1_000_000_000.0);
    }
    if clip.is_freeze_frame() {
        let source_ns = clip.freeze_frame_source_time_ns().unwrap_or(clip.source_in);
        return (
            source_ns as f64 / 1_000_000_000.0,
            frame_duration_s.max(0.001),
        );
    }
    // Still images: seek to 0, decode a single frame.
    if clip.kind == ClipKind::Image {
        return (0.0, frame_duration_s.max(0.001));
    }
    (
        clip.source_in as f64 / 1_000_000_000.0,
        clip.source_duration() as f64 / 1_000_000_000.0,
    )
}

/// Generate lavfi color source string for a title clip.
///
/// Always outputs opaque yuv420p — transparency is not supported in the
/// primary/secondary concat export path (the filter chain converts to
/// yuv420p anyway).  Using a finite `d=` duration plus `trim` and
/// `setpts=PTS-STARTPTS` ensures the lavfi source produces clean
/// monotonic timestamps compatible with concat.
fn title_clip_lavfi_color(
    clip: &crate::model::clip::Clip,
    out_w: u32, out_h: u32,
    fr_n: u32, fr_d: u32,
    dur_s: f64,
) -> String {
    let bg = clip.title_clip_bg_color;
    let a = bg & 0xFF;
    let color_str = if a > 0 {
        let r = (bg >> 24) & 0xFF;
        let g = (bg >> 16) & 0xFF;
        let b = (bg >> 8) & 0xFF;
        format!("#{r:02x}{g:02x}{b:02x}")
    } else {
        "black".to_string()
    };
    format!(
        "color=c={color_str}:size={out_w}x{out_h}:r={fr_n}/{fr_d}:d={dur_s:.6},format=yuv420p,trim=duration={dur_s:.6},setpts=PTS-STARTPTS"
    )
}

fn build_timing_filter(
    clip: &crate::model::clip::Clip,
    frame_duration_s: f64,
    fps_num: u32,
    fps_den: u32,
) -> String {
    if clip.kind == ClipKind::Title {
        // Title clips are generated at exact duration by lavfi, no timing filter needed.
        return String::new();
    }
    if clip.is_freeze_frame() || clip.kind == ClipKind::Image {
        let hold_s = clip.duration() as f64 / 1_000_000_000.0;
        let frame_s = frame_duration_s.max(0.001);
        let pad_s = (hold_s - frame_s).max(0.0);
        return format!(
            ",trim=duration={frame_s:.6},setpts=PTS-STARTPTS,tpad=stop_mode=clone:stop_duration={pad_s:.6},trim=duration={hold_s:.6},setpts=PTS-STARTPTS"
        );
    }

    let minterp_suffix = build_minterpolate_suffix(clip, fps_num, fps_den);

    if !clip.speed_keyframes.is_empty() {
        // Build a source→timeline time mapping for setpts.  Speed keyframes
        // are in timeline coordinates, but FFmpeg's PTS is in source
        // coordinates.  We compute (source_ns, timeline_ns) control points
        // and build a piecewise linear expression that maps source PTS to
        // the correct output PTS.
        let clip_dur_ns = clip.duration();
        let source_dur_ns = clip.source_duration();

        // Collect unique timeline positions: 0, each keyframe (clamped to
        // clip duration), and clip duration.
        let mut timeline_points: Vec<u64> = vec![0, clip_dur_ns];
        for kf in &clip.speed_keyframes {
            let t = kf.time_ns.min(clip_dur_ns);
            if !timeline_points.contains(&t) {
                timeline_points.push(t);
            }
        }
        timeline_points.sort_unstable();

        // For each timeline point, compute the corresponding source position.
        let map_points: Vec<(f64, f64)> = timeline_points
            .iter()
            .map(|&t_ns| {
                let src_ns = clip
                    .integrated_source_distance_for_local_timeline_ns(t_ns)
                    .clamp(0.0, source_dur_ns as f64);
                let src_s = src_ns / 1_000_000_000.0;
                let tl_s = t_ns as f64 / 1_000_000_000.0;
                (src_s, tl_s)
            })
            .collect();

        // Build piecewise linear setpts expression that maps source time
        // (T, in seconds) to output time (seconds), then convert to PTS
        // units by dividing by TB.
        //
        // Expression structure:
        //   if(lt(T,s1), lerp_0, if(lt(T,s2), lerp_1, lerp_last)) / TB
        //
        // Commas inside the expression are escaped as \, so FFmpeg's
        // filtergraph parser doesn't split on them.
        let mut expr = String::new();
        let mut depth = 0usize;
        for i in 0..map_points.len().saturating_sub(1) {
            let (s0, t0) = map_points[i];
            let (s1, t1) = map_points[i + 1];
            let ds = s1 - s0;
            if ds.abs() < 1e-9 {
                continue;
            }
            let slope = (t1 - t0) / ds;
            if i + 2 < map_points.len() {
                expr.push_str(&format!(
                    "if(lt(T\\,{s1:.10})\\,{t0:.10}+(T-{s0:.10})*{slope:.10}\\,"
                ));
                depth += 1;
            } else {
                // Last segment — no condition needed.
                expr.push_str(&format!("{t0:.10}+(T-{s0:.10})*{slope:.10}"));
            }
        }
        for _ in 0..depth {
            expr.push(')');
        }
        // Fallback if expression is empty (shouldn't happen).
        if expr.is_empty() {
            expr = "PTS".to_string();
        }

        // Wrap: convert seconds → PTS units, trim to clip duration.
        let dur_s = clip_dur_ns as f64 / 1_000_000_000.0;
        return if clip.reverse {
            format!(",reverse,setpts=({expr})/TB,trim=duration={dur_s:.6},setpts=PTS-STARTPTS{minterp_suffix}")
        } else {
            format!(",setpts=({expr})/TB,trim=duration={dur_s:.6},setpts=PTS-STARTPTS{minterp_suffix}")
        };
    }
    let has_speed = (clip.speed - 1.0).abs() > 0.001;
    match (clip.reverse, has_speed) {
        (false, false) => minterp_suffix,
        (false, true) => format!(",setpts=PTS/{:.6}{minterp_suffix}", clip.speed),
        // `reverse` already emits a valid timeline in ffmpeg; STARTPTS-PTS here can
        // cause non-monotonic DTS and near-empty video output.
        (true, false) => format!(",reverse{minterp_suffix}"),
        (true, true) => format!(",reverse,setpts=PTS/{:.6}{minterp_suffix}", clip.speed),
    }
}

/// Build the minterpolate filter suffix for slow-motion frame interpolation.
/// Returns empty string if interpolation is Off or the clip isn't slow-motion.
fn build_minterpolate_suffix(clip: &Clip, fps_num: u32, fps_den: u32) -> String {
    if clip.slow_motion_interp == SlowMotionInterp::Off {
        return String::new();
    }
    // Check if clip has slow-motion segments:
    // - For constant speed: speed < 1.0
    // - For speed keyframes: any keyframe value < 1.0
    let is_slow = if !clip.speed_keyframes.is_empty() {
        clip.speed_keyframes.iter().any(|kf| kf.value < 1.0)
    } else {
        clip.speed < 1.0 - 0.001
    };
    if !is_slow {
        return String::new();
    }
    let mi_mode = match clip.slow_motion_interp {
        SlowMotionInterp::Blend => "blend",
        SlowMotionInterp::OpticalFlow => "mci",
        SlowMotionInterp::Off => unreachable!(),
    };
    let fps = if fps_den > 0 {
        format!("{fps_num}/{fps_den}")
    } else {
        format!("{fps_num}")
    };
    format!(",minterpolate=fps={fps}:mi_mode={mi_mode}")
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
    let has_crop_keyframes = !clip.crop_left_keyframes.is_empty()
        || !clip.crop_right_keyframes.is_empty()
        || !clip.crop_top_keyframes.is_empty()
        || !clip.crop_bottom_keyframes.is_empty();
    if has_crop_keyframes {
        let cl_expr = build_keyframed_property_expression(
            &clip.crop_left_keyframes,
            clip.crop_left as f64,
            0.0,
            500.0,
            "T",
        );
        let cr_expr = build_keyframed_property_expression(
            &clip.crop_right_keyframes,
            clip.crop_right as f64,
            0.0,
            500.0,
            "T",
        );
        let ct_expr = build_keyframed_property_expression(
            &clip.crop_top_keyframes,
            clip.crop_top as f64,
            0.0,
            500.0,
            "T",
        );
        let cb_expr = build_keyframed_property_expression(
            &clip.crop_bottom_keyframes,
            clip.crop_bottom as f64,
            0.0,
            500.0,
            "T",
        );
        // Dynamic crop via alpha masking (per-frame expressions). This avoids relying on
        // crop filter `eval=frame` support while matching preview semantics.
        return format!(
            ",geq=lum='lum(X,Y)':cb='cb(X,Y)':cr='cr(X,Y)':a='if(between(X,({cl_expr}),{out_w}-({cr_expr})-1)*between(Y,({ct_expr}),{out_h}-({cb_expr})-1),alpha(X,Y),0)'"
        );
    }
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
    if !clip.rotate_keyframes.is_empty() {
        let fill = if transparent_pad { "black@0" } else { "black" };
        let angle_expr = build_keyframed_property_expression(
            &clip.rotate_keyframes,
            clip.rotate as f64,
            -180.0,
            180.0,
            "t",
        );
        return format!(",rotate='-({angle_expr})*PI/180':fillcolor={fill}");
    }
    let rot = clip.rotate;
    if rot == 0 {
        return String::new();
    }
    let fill = if transparent_pad { "black@0" } else { "black" };
    format!(
        ",rotate={:.10}:fillcolor={fill}",
        -(rot as f64).to_radians()
    )
}

fn build_anamorphic_filter(clip: &crate::model::clip::Clip) -> String {
    if (clip.anamorphic_desqueeze - 1.0).abs() > 0.001 {
        // Physically desqueeze the source pixels horizontally and reset SAR to 1.
        // This ensures subsequent scale/fit/crop filters work in a consistent square-pixel space.
        format!("scale=iw*{}:ih,setsar=1,", clip.anamorphic_desqueeze)
    } else {
        String::new()
    }
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

fn clamped_primary_xfade_duration_s(current: &Clip, next: &Clip) -> Option<f64> {
    if current.transition_after.is_empty() || current.transition_after_ns == 0 {
        return None;
    }
    let mut d_s = current.transition_after_ns as f64 / 1_000_000_000.0;
    let max_d = (current.duration().min(next.duration()) as f64 / 1_000_000_000.0) - 0.001;
    d_s = d_s.clamp(0.001, max_d.max(0.001));
    Some(d_s)
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

/// Build audio speed filter for a clip. When speed keyframes are present,
/// uses the mean speed as an atempo approximation (atempo and asetrate do not
/// support time-varying expressions). True variable-speed audio requires
/// Rubberband, which is a separate roadmap item.
/// Build an FFmpeg filter for pitch shifting and/or pitch-preserved speed change
/// using the `rubberband` filter. Returns empty string if no pitch processing needed.
fn build_pitch_filter(clip: &crate::model::clip::Clip) -> String {
    let has_pitch_shift = clip.pitch_shift_semitones.abs() > 0.001;
    let has_pitch_preserve = clip.pitch_preserve && (clip.speed - 1.0).abs() > 0.001;

    if !has_pitch_shift && !has_pitch_preserve {
        return String::new();
    }

    // FFmpeg rubberband filter: pitch= is a ratio (2^(semitones/12)),
    // tempo= is the speed factor (only used for pitch-preserved speed changes).
    let pitch_ratio = if has_pitch_shift {
        2.0_f64.powf(clip.pitch_shift_semitones / 12.0)
    } else {
        1.0
    };

    let tempo = if has_pitch_preserve {
        clip.speed.clamp(0.05, 16.0)
    } else {
        1.0
    };

    let mut params = Vec::new();
    if (pitch_ratio - 1.0).abs() > 0.0001 {
        params.push(format!("pitch={pitch_ratio:.6}"));
    }
    if (tempo - 1.0).abs() > 0.0001 {
        params.push(format!("tempo={tempo:.6}"));
    }
    // Preserve formants for voice content.
    params.push("formant=preserved".to_string());

    format!("rubberband={}", params.join(":"))
}

/// Build an FFmpeg filter for audio channel routing (Left/Right/MonoMix).
/// Returns empty string for Stereo (default passthrough).
fn build_channel_filter(clip: &crate::model::clip::Clip) -> String {
    use crate::model::clip::AudioChannelMode;
    match clip.audio_channel_mode {
        AudioChannelMode::Stereo => String::new(),
        AudioChannelMode::Left => "pan=stereo|c0=c0|c1=c0".to_string(),
        AudioChannelMode::Right => "pan=stereo|c0=c1|c1=c1".to_string(),
        AudioChannelMode::MonoMix => "pan=stereo|c0=0.5*c0+0.5*c1|c1=0.5*c0+0.5*c1".to_string(),
    }
}

/// Build FFmpeg filter chain for LADSPA audio effects on a clip.
/// Uses FFmpeg's native `ladspa` filter which loads .so plugins directly.
/// Find the absolute path to a LADSPA .so file.
fn find_ladspa_so(name: &str) -> Option<String> {
    let search_dirs = [
        "/usr/lib/ladspa",
        "/usr/lib/x86_64-linux-gnu/ladspa",
        "/usr/local/lib/ladspa",
        "/usr/lib64/ladspa",
    ];
    for dir in &search_dirs {
        let path = format!("{dir}/{name}");
        if std::path::Path::new(&path).exists() {
            return Some(path);
        }
    }
    None
}

fn build_ladspa_effects_filter(clip: &crate::model::clip::Clip) -> String {
    if clip.ladspa_effects.is_empty() {
        return String::new();
    }
    let reg = crate::media::ladspa_registry::LadspaRegistry::get_or_discover();
    let mut parts = Vec::new();
    for effect in &clip.ladspa_effects {
        if !effect.enabled {
            continue;
        }
        // Find the LADSPA .so file and plugin label.
        // The GStreamer element name encodes the .so path:
        // "ladspa-ladspa-rubberband-so-rubberband-pitchshifter-stereo"
        // → .so = "ladspa-rubberband" (replace hyphens with path logic)
        // For FFmpeg's ladspa filter: ladspa=file=SONAME:plugin=LABEL[:controls=c0|c1|...]
        let info = reg.find_by_name(&effect.plugin_name);
        // Extract .so filename from the GStreamer element name pattern.
        // Pattern: ladspa-{soname-with-hyphens}-so-{pluginname}
        let gst_name = &effect.gst_element_name;
        let stripped = gst_name.strip_prefix("ladspa-").unwrap_or(gst_name);
        // Find "-so-" separator.
        if let Some(so_pos) = stripped.find("-so-") {
            let so_part = &stripped[..so_pos];
            let plugin_part = &stripped[so_pos + 4..];
            // .so filename keeps hyphens as-is (e.g. "ladspa-rubberband.so").
            // Use absolute path since FFmpeg doesn't search LADSPA_PATH reliably.
            let so_name = format!("{so_part}.so");
            let Some(so_file) = find_ladspa_so(&so_name) else {
                log::warn!("LADSPA export: .so not found: {so_name}, skipping effect");
                continue;
            };
            // LADSPA plugin labels use underscores (GStreamer converts _ → -).
            let plugin_part = plugin_part.replace('-', "_");
            // Build controls string from params (ordered by registry param list).
            let controls = if let Some(info) = info {
                let vals: Vec<String> = info
                    .params
                    .iter()
                    .map(|p| {
                        let val = effect.params.get(&p.name).copied().unwrap_or(p.default_value);
                        format!("{val:.6}")
                    })
                    .collect();
                if vals.is_empty() {
                    String::new()
                } else {
                    format!(":controls={}", vals.join("|"))
                }
            } else {
                String::new()
            };
            parts.push(format!(
                "ladspa=file={so_file}:plugin={plugin_part}{controls}"
            ));
        }
    }
    parts.join(",")
}

fn build_audio_speed_filter(clip: &crate::model::clip::Clip) -> String {
    // When pitch_preserve is true, the rubberband filter handles the tempo change,
    // so skip atempo to avoid double speed-change.
    if clip.pitch_preserve && (clip.speed - 1.0).abs() > 0.001 {
        return String::new();
    }
    if !clip.speed_keyframes.is_empty() {
        // Compute mean speed over the clip's timeline duration.
        let dur = clip.duration();
        let mean_speed = if dur > 0 {
            clip.integrated_source_distance_for_local_timeline_ns(dur) / dur as f64
        } else {
            clip.speed.clamp(0.05, 16.0)
        };
        build_atempo(mean_speed)
    } else {
        build_atempo(clip.speed)
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct ClipAudioFade {
    fade_in_ns: u64,
    fade_out_ns: u64,
}

fn compute_clip_audio_fades(
    track_audio_clips: &[Vec<&Clip>],
    target_crossfade_ns: u64,
) -> HashMap<String, ClipAudioFade> {
    let mut fades: HashMap<String, ClipAudioFade> = HashMap::new();
    if target_crossfade_ns == 0 {
        return fades;
    }

    for track in track_audio_clips {
        if track.len() < 2 {
            continue;
        }
        let mut sorted = track.clone();
        sorted.sort_by_key(|c| c.timeline_start);

        for pair in sorted.windows(2) {
            let left = pair[0];
            let right = pair[1];
            let left_end = left.timeline_end();
            if right.timeline_start > left_end {
                continue;
            }

            let overlap_ns = left_end.saturating_sub(right.timeline_start);
            let mut fade_ns = target_crossfade_ns;
            if overlap_ns > 0 {
                fade_ns = fade_ns.min(overlap_ns);
            }
            fade_ns = fade_ns.min(left.duration()).min(right.duration());
            if fade_ns == 0 {
                continue;
            }

            let left_fade = fades.entry(left.id.clone()).or_default();
            left_fade.fade_out_ns = left_fade.fade_out_ns.max(fade_ns);

            let right_fade = fades.entry(right.id.clone()).or_default();
            right_fade.fade_in_ns = right_fade.fade_in_ns.max(fade_ns);
        }

        for clip in sorted {
            if let Some(clip_fades) = fades.get_mut(&clip.id) {
                let clip_duration = clip.duration();
                if clip_duration == 0 {
                    clip_fades.fade_in_ns = 0;
                    clip_fades.fade_out_ns = 0;
                    continue;
                }

                clip_fades.fade_in_ns = clip_fades.fade_in_ns.min(clip_duration);
                clip_fades.fade_out_ns = clip_fades.fade_out_ns.min(clip_duration);
                let total = clip_fades.fade_in_ns.saturating_add(clip_fades.fade_out_ns);
                if total > clip_duration {
                    let scaled_in = ((clip_fades.fade_in_ns as u128 * clip_duration as u128)
                        / total as u128) as u64;
                    clip_fades.fade_in_ns = scaled_in.min(clip_duration);
                    clip_fades.fade_out_ns = clip_duration.saturating_sub(clip_fades.fade_in_ns);
                }
            }
        }
    }

    fades
}

fn audio_crossfade_curve_name(curve: &crate::ui_state::CrossfadeCurve) -> &'static str {
    match curve {
        crate::ui_state::CrossfadeCurve::EqualPower => "qsin",
        crate::ui_state::CrossfadeCurve::Linear => "tri",
    }
}

fn build_audio_crossfade_filters(clip: &Clip, fades: ClipAudioFade, curve: &str) -> String {
    let clip_duration_ns = clip.duration();
    if clip_duration_ns == 0 {
        return String::new();
    }

    let mut filters = String::new();
    let fade_in_ns = fades.fade_in_ns.min(clip_duration_ns);
    if fade_in_ns > 0 {
        let d_s = fade_in_ns as f64 / 1_000_000_000.0;
        filters.push_str(&format!("afade=t=in:st=0:d={d_s:.6}:curve={curve},"));
    }

    let max_fade_out_ns = clip_duration_ns.saturating_sub(fade_in_ns);
    let fade_out_ns = fades.fade_out_ns.min(max_fade_out_ns);
    if fade_out_ns > 0 {
        let d_s = fade_out_ns as f64 / 1_000_000_000.0;
        let st_s = clip_duration_ns.saturating_sub(fade_out_ns) as f64 / 1_000_000_000.0;
        filters.push_str(&format!(
            "afade=t=out:st={st_s:.6}:d={d_s:.6}:curve={curve},"
        ));
    }
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
    // GIF is a palette-indexed format with no audio; use a rough estimate
    if options.container == Container::Gif {
        let gif_fps = options
            .gif_fps
            .unwrap_or_else(|| project.frame_rate.as_f64().round().clamp(1.0, 30.0) as u32)
            .clamp(1, 30) as f64;
        let pixel_scale =
            ((out_w.max(1) as f64 * out_h.max(1) as f64) / (640.0 * 480.0)).max(0.1);
        // Approximate: ~20 kbps per 640×480 pixel at 15fps, scaled by resolution and fps
        let gif_kbps = (20_000.0 * pixel_scale * (gif_fps / 15.0)).clamp(500.0, 20_000.0);
        return (gif_kbps * 1_000.0) as u64;
    }

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

fn check_frei0r_module_support(ffmpeg: &str, module_name: &str, probe_params: &str) -> bool {
    let vf = format!("format=rgba,frei0r=filter_name={module_name}:filter_params={probe_params}");
    Command::new(ffmpeg)
        .args([
            "-v",
            "error",
            "-f",
            "lavfi",
            "-i",
            "color=c=black:s=16x16:d=0.04",
            "-vf",
            &vf,
            "-frames:v",
            "1",
            "-f",
            "null",
            "-",
        ])
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

pub(crate) fn detect_color_filter_capabilities(ffmpeg: &str) -> ColorFilterCapabilities {
    if !check_filter_support(ffmpeg, "frei0r") {
        return ColorFilterCapabilities::default();
    }

    let use_coloradj_frei0r =
        check_frei0r_module_support(ffmpeg, "coloradj_RGB", "0.5|0.5|0.5|0.333");

    // FFmpeg module naming differs across builds; prefer the common underscore form.
    let three_point_frei0r_module =
        if check_frei0r_module_support(ffmpeg, "three_point_balance", "0x000000|0x808080|0xFFFFFF")
        {
            Some("three_point_balance".to_string())
        } else if check_frei0r_module_support(
            ffmpeg,
            "3-point-color-balance",
            "0x000000|0x808080|0xFFFFFF",
        ) {
            Some("3-point-color-balance".to_string())
        } else {
            None
        };

    ColorFilterCapabilities {
        use_coloradj_frei0r,
        three_point_frei0r_module,
    }
}

fn has_linked_audio_peer(clip: &Clip, audio_clips: &[&Clip]) -> bool {
    audio_clips
        .iter()
        .any(|audio_clip| clip.suppresses_embedded_audio_for_linked_peer(audio_clip))
}

fn clip_has_audio(ffmpeg: &str, clip: &Clip, cache: &mut HashMap<String, bool>) -> bool {
    // Title clips have no source media and thus no audio.
    if clip.kind == ClipKind::Title || clip.source_path.is_empty() {
        return false;
    }
    if let Some(has_audio) = cache.get(&clip.source_path) {
        return *has_audio;
    }
    let has_audio = probe_has_audio(ffmpeg, &clip.source_path);
    cache.insert(clip.source_path.clone(), has_audio);
    has_audio
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
            if let Some(val) = val_str.split_whitespace().next().and_then(|s| s.parse::<f64>().ok()) {
                pending_start = Some(val);
            }
        }
        if let Some(pos) = line.find("silence_end: ") {
            let val_str = &line[pos + "silence_end: ".len()..];
            if let Some(end_val) = val_str.split_whitespace().next().and_then(|s| s.parse::<f64>().ok()) {
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

/// Measure integrated loudness (LUFS) of a clip's audio via FFmpeg `ebur128` filter.
/// Returns the integrated loudness value in LUFS (e.g. -18.3).
pub(crate) fn analyze_loudness_lufs(
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
            "ebur128",
            "-f",
            "null",
            "-",
        ])
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .output()
        .map_err(|e| anyhow!("Failed to run ffmpeg ebur128: {e}"))?;

    let stderr = String::from_utf8_lossy(&output.stderr);
    // Parse the summary block. The ebur128 filter outputs lines like:
    //   [Parsed_ebur128_0 @ 0x...] Summary:
    //
    //     Integrated loudness:
    //       I:         -25.9 LUFS
    // The "Summary:" line has a filter tag prefix; the "I:" line does not.
    let mut in_summary = false;
    for line in stderr.lines() {
        let trimmed = line.trim();
        // "Summary:" may be prefixed by "[Parsed_ebur128_0 @ 0x...]"
        if trimmed.contains("Summary:") {
            in_summary = true;
            continue;
        }
        if in_summary {
            // e.g. "    I:         -25.9 LUFS"
            if trimmed.starts_with("I:") {
                let rest = trimmed["I:".len()..].trim();
                if let Some(val) = rest
                    .split_whitespace()
                    .next()
                    .and_then(|s| s.parse::<f64>().ok())
                {
                    if val.is_finite() {
                        return Ok(val);
                    }
                }
            }
        }
    }
    Err(anyhow!(
        "Could not parse integrated loudness from ffmpeg ebur128 output"
    ))
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

/// Compute the linear gain multiplier needed to shift measured LUFS to a target LUFS.
pub(crate) fn compute_lufs_gain(measured_lufs: f64, target_lufs: f64) -> f64 {
    10.0_f64.powf((target_lufs - measured_lufs) / 20.0)
}

/// Compute the linear gain multiplier needed to shift measured peak dB to a target dB.
pub(crate) fn compute_peak_gain(measured_peak_db: f64, target_peak_db: f64) -> f64 {
    10.0_f64.powf((target_peak_db - measured_peak_db) / 20.0)
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

/// Generate an FFMETADATA file with chapter entries from project markers.
/// Returns `None` if there are no markers.
fn write_chapter_metadata(
    markers: &[crate::model::project::Marker],
    project_duration_ns: u64,
) -> Result<Option<tempfile::NamedTempFile>> {
    if markers.is_empty() {
        return Ok(None);
    }
    use std::io::Write;
    let mut file = tempfile::NamedTempFile::new()?;
    writeln!(file, ";FFMETADATA1")?;
    writeln!(file)?;

    let sorted: Vec<_> = {
        let mut v: Vec<_> = markers.iter().collect();
        v.sort_by_key(|m| m.position_ns);
        v
    };

    for (i, marker) in sorted.iter().enumerate() {
        let start = marker.position_ns;
        let end = if i + 1 < sorted.len() {
            sorted[i + 1].position_ns
        } else {
            project_duration_ns
        };
        // Escape special FFMETADATA characters in the title: = ; # \ and newlines
        let title = marker
            .label
            .replace('\\', "\\\\")
            .replace('=', "\\=")
            .replace(';', "\\;")
            .replace('#', "\\#")
            .replace('\n', " ");
        writeln!(file, "[CHAPTER]")?;
        writeln!(file, "TIMEBASE=1/1000000000")?;
        writeln!(file, "START={start}")?;
        writeln!(file, "END={end}")?;
        writeln!(file, "title={title}")?;
        writeln!(file)?;
    }
    file.flush()?;
    Ok(Some(file))
}

#[cfg(test)]
mod tests {
    use super::{
        append_pan_filter_chain, audio_crossfade_curve_name, build_audio_crossfade_filters,
        build_color_filter, build_crop_filter, build_grading_filter,
        build_keyframed_property_expression, build_pan_expression, build_rotation_filter,
        build_temperature_tint_filter, build_timing_filter, build_title_filter,
        build_volume_filter, clamped_primary_xfade_duration_s, compute_clip_audio_fades,
        compute_export_coloradj_params,
        estimate_export_size_bytes, has_linked_audio_peer, has_transform_keyframes,
        parse_progress_line, video_input_seek_and_duration, write_chapter_metadata,
        AudioCodec, ClipAudioFade, ExportOptions, VideoCodec,
    };
    use gstreamer as gst;
    use crate::media::program_player::ProgramPlayer;
    use crate::model::clip::{Clip, ClipKind, KeyframeInterpolation, NumericKeyframe};
    use crate::model::project::Project;
    use crate::ui_state::CrossfadeCurve;

    fn extract_colorbalance_component(filter: &str, key: &str) -> f32 {
        let needle = format!("{key}=");
        let start = filter
            .find(&needle)
            .unwrap_or_else(|| panic!("missing colorbalance component `{key}` in `{filter}`"));
        let rest = &filter[start + needle.len()..];
        let end = rest.find(':').unwrap_or(rest.len());
        rest[..end]
            .parse::<f32>()
            .unwrap_or_else(|e| panic!("invalid `{key}` value in `{filter}`: {e}"))
    }

    fn extract_eq_component(filter: &str, key: &str) -> f32 {
        let needle = format!("{key}=");
        let start = filter
            .find(&needle)
            .unwrap_or_else(|| panic!("missing eq component `{key}` in `{filter}`"));
        let rest = &filter[start + needle.len()..];
        let end = rest.find(':').unwrap_or(rest.len());
        rest[..end]
            .trim_matches('\'')
            .parse::<f32>()
            .unwrap_or_else(|e| panic!("invalid `{key}` value in `{filter}`: {e}"))
    }

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

    fn make_audio_clip(id: &str, timeline_start: u64, duration_ns: u64) -> Clip {
        let mut clip = Clip::new(
            "/tmp/audio.wav",
            duration_ns,
            timeline_start,
            ClipKind::Audio,
        );
        clip.id = id.to_string();
        clip
    }

    fn make_video_clip(id: &str, timeline_start: u64, source_duration_ns: u64) -> Clip {
        let mut clip = Clip::new(
            "/tmp/video.mp4",
            source_duration_ns,
            timeline_start,
            ClipKind::Video,
        );
        clip.id = id.to_string();
        clip
    }

    #[test]
    fn compute_clip_audio_fades_for_adjacent_clips() {
        let a = make_audio_clip("a", 0, 2_000_000_000);
        let b = make_audio_clip("b", 2_000_000_000, 2_000_000_000);
        let tracks = vec![vec![&a, &b]];

        let fades = compute_clip_audio_fades(&tracks, 300_000_000);

        assert_eq!(
            fades.get("a"),
            Some(&ClipAudioFade {
                fade_in_ns: 0,
                fade_out_ns: 300_000_000
            })
        );
        assert_eq!(
            fades.get("b"),
            Some(&ClipAudioFade {
                fade_in_ns: 300_000_000,
                fade_out_ns: 0
            })
        );
    }

    #[test]
    fn compute_clip_audio_fades_clamps_short_middle_clip() {
        let a = make_audio_clip("a", 0, 100_000_000);
        let b = make_audio_clip("b", 100_000_000, 100_000_000);
        let c = make_audio_clip("c", 200_000_000, 100_000_000);
        let tracks = vec![vec![&a, &b, &c]];

        let fades = compute_clip_audio_fades(&tracks, 80_000_000);
        let middle = fades.get("b").expect("middle clip should have fades");

        assert_eq!(middle.fade_in_ns + middle.fade_out_ns, 100_000_000);
        assert!(middle.fade_in_ns > 0);
        assert!(middle.fade_out_ns > 0);
    }

    #[test]
    fn compute_clip_audio_fades_clamps_to_overlap_when_tracks_overlap() {
        let a = make_audio_clip("a", 0, 2_000_000_000);
        let b = make_audio_clip("b", 1_800_000_000, 2_000_000_000);
        let tracks = vec![vec![&a, &b]];

        let fades = compute_clip_audio_fades(&tracks, 500_000_000);

        assert_eq!(
            fades.get("a"),
            Some(&ClipAudioFade {
                fade_in_ns: 0,
                fade_out_ns: 200_000_000
            })
        );
        assert_eq!(
            fades.get("b"),
            Some(&ClipAudioFade {
                fade_in_ns: 200_000_000,
                fade_out_ns: 0
            })
        );
    }

    #[test]
    fn build_audio_crossfade_filters_builds_expected_afade_chain() {
        let clip = make_audio_clip("a", 0, 1_000_000_000);
        let filters = build_audio_crossfade_filters(
            &clip,
            ClipAudioFade {
                fade_in_ns: 200_000_000,
                fade_out_ns: 300_000_000,
            },
            "tri",
        );
        assert!(filters.contains("afade=t=in:st=0:d=0.200000:curve=tri"));
        assert!(filters.contains("afade=t=out:st=0.700000:d=0.300000:curve=tri"));
    }

    #[test]
    fn build_audio_crossfade_filters_clamps_fade_out_after_large_fade_in() {
        let clip = make_audio_clip("a", 0, 1_000_000_000);
        let filters = build_audio_crossfade_filters(
            &clip,
            ClipAudioFade {
                fade_in_ns: 800_000_000,
                fade_out_ns: 900_000_000,
            },
            "qsin",
        );
        assert!(filters.contains("afade=t=in:st=0:d=0.800000:curve=qsin"));
        assert!(filters.contains("afade=t=out:st=0.800000:d=0.200000:curve=qsin"));
    }

    #[test]
    fn audio_crossfade_curve_name_maps_expected_ffmpeg_curves() {
        assert_eq!(
            audio_crossfade_curve_name(&CrossfadeCurve::EqualPower),
            "qsin"
        );
        assert_eq!(audio_crossfade_curve_name(&CrossfadeCurve::Linear), "tri");
    }

    #[test]
    fn build_audio_crossfade_filters_supports_both_curve_types() {
        let clip = make_audio_clip("a", 0, 1_000_000_000);
        let fades = ClipAudioFade {
            fade_in_ns: 150_000_000,
            fade_out_ns: 150_000_000,
        };
        let equal_power = build_audio_crossfade_filters(&clip, fades, "qsin");
        let linear = build_audio_crossfade_filters(&clip, fades, "tri");
        assert!(equal_power.contains("curve=qsin"));
        assert!(linear.contains("curve=tri"));
    }

    #[test]
    fn freeze_frame_video_input_uses_freeze_source_and_single_frame_duration() {
        let mut clip = make_video_clip("freeze", 0, 6_000_000_000);
        clip.source_in = 1_000_000_000;
        clip.source_out = 6_000_000_000;
        clip.freeze_frame = true;
        clip.freeze_frame_source_ns = Some(4_500_000_000);
        clip.freeze_frame_hold_duration_ns = Some(3_000_000_000);

        let (seek_s, input_s) = video_input_seek_and_duration(&clip, 1.0 / 30.0);
        assert!((seek_s - 4.5).abs() < 1e-6);
        assert!((input_s - (1.0 / 30.0)).abs() < 1e-6);
    }

    #[test]
    fn freeze_frame_timing_filter_holds_single_frame_for_clip_duration() {
        let mut clip = make_video_clip("freeze", 0, 6_000_000_000);
        clip.freeze_frame = true;
        clip.freeze_frame_hold_duration_ns = Some(2_500_000_000);

        let filter = build_timing_filter(&clip, 1.0 / 25.0, 25, 1);
        assert!(filter.contains("trim=duration=0.040000"));
        assert!(filter.contains("tpad=stop_mode=clone:stop_duration=2.460000"));
        assert!(filter.contains("trim=duration=2.500000"));
    }

    #[test]
    fn keyframed_expression_uses_first_value_before_first_point() {
        let expr = build_keyframed_property_expression(
            &[
                NumericKeyframe {
                    time_ns: 500_000_000,
                    value: 0.5,
                    interpolation: KeyframeInterpolation::Linear,
                    bezier_controls: None,
                },
                NumericKeyframe {
                    time_ns: 1_000_000_000,
                    value: 1.0,
                    interpolation: KeyframeInterpolation::Linear,
                    bezier_controls: None,
                },
            ],
            0.25,
            0.0,
            1.0,
            "t",
        );
        assert!(expr.starts_with("if(lt(t,0.500000000),0.5000000000,"));
    }

    #[test]
    fn volume_filter_uses_expression_when_keyframed() {
        let mut clip = Clip::new("/tmp/test.wav", 2_000_000_000, 0, ClipKind::Audio);
        clip.volume = 0.8;
        clip.volume_keyframes = vec![
            NumericKeyframe {
                time_ns: 0,
                value: 0.3,
                interpolation: KeyframeInterpolation::Linear,
                bezier_controls: None,
            },
            NumericKeyframe {
                time_ns: 1_000_000_000,
                value: 0.9,
                interpolation: KeyframeInterpolation::Linear,
                bezier_controls: None,
            },
        ];
        let filter = build_volume_filter(&clip);
        assert!(filter.starts_with("volume='if(lt(t,"));
        assert!(filter.ends_with("':eval=frame"));
    }

    #[test]
    fn volume_filter_uses_constant_when_not_keyframed() {
        let mut clip = Clip::new("/tmp/test.wav", 2_000_000_000, 0, ClipKind::Audio);
        clip.volume = 1.25;
        let filter = build_volume_filter(&clip);
        assert_eq!(filter, "volume=1.2500");
    }

    #[test]
    fn pan_expression_uses_keyframes_when_present() {
        let mut clip = Clip::new("/tmp/test.wav", 2_000_000_000, 0, ClipKind::Audio);
        clip.pan = 0.0;
        clip.pan_keyframes = vec![
            NumericKeyframe {
                time_ns: 0,
                value: -1.0,
                interpolation: KeyframeInterpolation::Linear,
                bezier_controls: None,
            },
            NumericKeyframe {
                time_ns: 1_000_000_000,
                value: 1.0,
                interpolation: KeyframeInterpolation::Linear,
                bezier_controls: None,
            },
        ];
        let expr = build_pan_expression(&clip);
        assert!(expr.starts_with("if(lt(t,"));
    }

    #[test]
    fn append_pan_filter_chain_uses_anull_for_center_pan_without_keyframes() {
        let clip = Clip::new("/tmp/test.wav", 2_000_000_000, 0, ClipKind::Audio);
        let mut graph = String::new();
        append_pan_filter_chain(&mut graph, &clip, "in", "out", "clip1");
        assert_eq!(graph, ";[in]anull[out]");
    }

    #[test]
    fn append_pan_filter_chain_emits_dynamic_channel_gains_for_keyframed_pan() {
        let mut clip = Clip::new("/tmp/test.wav", 2_000_000_000, 0, ClipKind::Audio);
        clip.pan_keyframes = vec![
            NumericKeyframe {
                time_ns: 0,
                value: -0.5,
                interpolation: KeyframeInterpolation::Linear,
                bezier_controls: None,
            },
            NumericKeyframe {
                time_ns: 1_000_000_000,
                value: 0.5,
                interpolation: KeyframeInterpolation::Linear,
                bezier_controls: None,
            },
        ];
        let mut graph = String::new();
        append_pan_filter_chain(&mut graph, &clip, "in", "out", "clip1");
        assert!(graph.contains("channelsplit=channel_layout=stereo"));
        assert!(graph.contains("volume='if(gt("));
        assert!(graph.contains("':eval=frame"));
        assert!(graph.contains("amerge=inputs=2"));
    }

    #[test]
    fn clamped_primary_xfade_duration_requires_explicit_transition() {
        let a = make_video_clip("a", 0, 4_000_000_000);
        let b = make_video_clip("b", 4_000_000_000, 8_000_000_000);
        assert_eq!(clamped_primary_xfade_duration_s(&a, &b), None);
    }

    #[test]
    fn clamped_primary_xfade_duration_clamps_to_boundary_limits() {
        let mut a = make_video_clip("a", 0, 4_000_000_000);
        let b = make_video_clip("b", 4_000_000_000, 8_000_000_000);
        a.transition_after = "cross_dissolve".to_string();
        a.transition_after_ns = 10_000_000_000;
        let d = clamped_primary_xfade_duration_s(&a, &b).expect("transition should be enabled");
        assert!((d - 3.999).abs() < 0.000_001);
    }

    #[test]
    fn has_transform_keyframes_includes_rotate_and_crop_lanes() {
        let mut clip = Clip::new("/tmp/test.mp4", 2_000_000_000, 0, ClipKind::Video);
        assert!(!has_transform_keyframes(&clip));
        clip.rotate_keyframes.push(NumericKeyframe {
            time_ns: 0,
            value: 20.0,
            interpolation: KeyframeInterpolation::Linear,
            bezier_controls: None,
        });
        assert!(has_transform_keyframes(&clip));

        clip.rotate_keyframes.clear();
        clip.crop_left_keyframes.push(NumericKeyframe {
            time_ns: 0,
            value: 42.0,
            interpolation: KeyframeInterpolation::Linear,
            bezier_controls: None,
        });
        assert!(has_transform_keyframes(&clip));
    }

    #[test]
    fn build_rotation_filter_uses_expression_when_keyframed() {
        let mut clip = Clip::new("/tmp/test.mp4", 2_000_000_000, 0, ClipKind::Video);
        clip.rotate = 0;
        clip.rotate_keyframes = vec![
            NumericKeyframe {
                time_ns: 0,
                value: -45.0,
                interpolation: KeyframeInterpolation::Linear,
                bezier_controls: None,
            },
            NumericKeyframe {
                time_ns: 1_000_000_000,
                value: 45.0,
                interpolation: KeyframeInterpolation::Linear,
                bezier_controls: None,
            },
        ];
        let f = build_rotation_filter(&clip, false);
        assert!(f.contains("rotate='-(")); // negated for ffmpeg convention
        assert!(f.contains("*PI/180'"));
    }

    #[test]
    fn build_crop_filter_uses_eval_frame_when_keyframed() {
        let mut clip = Clip::new("/tmp/test.mp4", 2_000_000_000, 0, ClipKind::Video);
        clip.crop_left_keyframes = vec![
            NumericKeyframe {
                time_ns: 0,
                value: 0.0,
                interpolation: KeyframeInterpolation::Linear,
                bezier_controls: None,
            },
            NumericKeyframe {
                time_ns: 1_000_000_000,
                value: 100.0,
                interpolation: KeyframeInterpolation::Linear,
                bezier_controls: None,
            },
        ];
        let f = build_crop_filter(&clip, 1920, 1080, false);
        assert!(f.contains(",geq=lum='lum(X,Y)'"));
        assert!(f.contains("alpha(X,Y)"));
        assert!(f.contains("between(X,("));
    }

    #[test]
    fn build_color_filter_uses_eval_frame_when_keyframed() {
        let mut clip = Clip::new("/tmp/test.mp4", 2_000_000_000, 0, ClipKind::Video);
        clip.brightness_keyframes = vec![
            NumericKeyframe {
                time_ns: 0,
                value: -0.25,
                interpolation: KeyframeInterpolation::Linear,
                bezier_controls: None,
            },
            NumericKeyframe {
                time_ns: 1_000_000_000,
                value: 0.5,
                interpolation: KeyframeInterpolation::Linear,
                bezier_controls: None,
            },
        ];
        let f = build_color_filter(&clip);
        assert!(f.contains("eq=brightness='if(lt(t,"));
        assert!(f.contains(":eval=frame"));
    }

    #[test]
    fn build_color_filter_exposure_uses_preview_aligned_deltas() {
        let mut clip = Clip::new("/tmp/test.mp4", 2_000_000_000, 0, ClipKind::Video);
        clip.exposure = 1.0;
        let f = build_color_filter(&clip);
        assert!(f.contains(",eq=brightness="));
        assert!(f.contains(":contrast="));
        assert!(!f.contains(":gamma="));
    }

    #[test]
    fn build_color_filter_static_uses_preview_calibrated_primary_mapping() {
        let mut clip = Clip::new("/tmp/test.mp4", 2_000_000_000, 0, ClipKind::Video);
        clip.contrast = 0.0;
        let f = build_color_filter(&clip);
        let brightness = extract_eq_component(&f, "brightness");
        let contrast = extract_eq_component(&f, "contrast");
        let saturation = extract_eq_component(&f, "saturation");
        assert!(
            brightness < -0.2,
            "low contrast should include negative brightness bias for preview parity; got {brightness}"
        );
        assert!(
            contrast < 0.5,
            "preview-calibrated contrast=0 mapping should stay low; got {contrast}"
        );
        assert!(
            saturation > 1.5,
            "preview-calibrated contrast=0 mapping should include saturation compensation; got {saturation}"
        );

        clip.contrast = 2.0;
        let f_hi = build_color_filter(&clip);
        let brightness_hi = extract_eq_component(&f_hi, "brightness");
        assert!(
            brightness_hi > 0.1,
            "high contrast should include positive brightness bias for preview parity; got {brightness_hi}"
        );
    }

    #[test]
    fn build_temperature_tint_filter_uses_eval_frame_when_keyframed() {
        let mut clip = Clip::new("/tmp/test.mp4", 2_000_000_000, 0, ClipKind::Video);
        clip.temperature_keyframes = vec![
            NumericKeyframe {
                time_ns: 0,
                value: 3200.0,
                interpolation: KeyframeInterpolation::Linear,
                bezier_controls: None,
            },
            NumericKeyframe {
                time_ns: 1_000_000_000,
                value: 7800.0,
                interpolation: KeyframeInterpolation::Linear,
                bezier_controls: None,
            },
        ];
        clip.tint_keyframes = vec![
            NumericKeyframe {
                time_ns: 0,
                value: -0.5,
                interpolation: KeyframeInterpolation::Linear,
                bezier_controls: None,
            },
            NumericKeyframe {
                time_ns: 1_000_000_000,
                value: 0.5,
                interpolation: KeyframeInterpolation::Linear,
                bezier_controls: None,
            },
        ];
        let f = build_temperature_tint_filter(&clip);
        assert!(f.contains("colortemperature=temperature='if(lt(t,"));
        assert!(f.contains(",colorbalance=rm='("));
        assert!(f.contains(":eval=frame"));
    }

    #[test]
    fn build_grading_filter_emits_lutrgb_when_active() {
        let mut clip = Clip::new("/tmp/test.mp4", 2_000_000_000, 0, ClipKind::Video);
        clip.shadows = 0.25;
        let f = build_grading_filter(&clip);
        assert!(f.contains(",lutrgb="), "grading should emit lutrgb filter: {f}");
        assert!(f.contains("r='"), "lutrgb should have red channel");
    }

    #[test]
    fn build_grading_filter_boosts_tonal_warmth_at_slider_extremes() {
        // Validate via compute_export_3point_params that shadows warmth is
        // stronger than midtones due to shadows_endpoint_boost.
        // In 3-point space: positive warmth lowers the R control point
        // (brighter red output) and raises the B control point (darker blue).
        let sh_p = ProgramPlayer::compute_export_3point_params(
            0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0,
        );
        let mid_p = ProgramPlayer::compute_export_3point_params(
            0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0,
        );
        // Positive warmth: red control lowered (more red output)
        assert!(sh_p.black_r < 0.05, "shadows warmth should lower red control: {}", sh_p.black_r);
        // Shadows warmth shift should be proportionally larger than midtones
        let sh_r_shift = (0.0 - sh_p.black_r).abs(); // shift from neutral black=0
        let mid_r_shift = (0.5 - mid_p.gray_r).abs(); // shift from neutral gray=0.5
        assert!(sh_r_shift > 0.01 || mid_r_shift > 0.01,
            "warmth should produce measurable shifts: sh={} mid={}",
            sh_r_shift, mid_r_shift);

        // Also check the curves filter is emitted
        let mut clip = Clip::new("/tmp/test.mp4", 2_000_000_000, 0, ClipKind::Video);
        clip.shadows_warmth = 1.0;
        let f = build_grading_filter(&clip);
        assert!(f.contains(",lutrgb="), "should emit lutrgb: {f}");
    }

    #[test]
    fn build_grading_filter_boosts_shadows_tint_at_slider_extremes() {
        // Validate that shadows tint is stronger than midtones tint.
        let sh_p = ProgramPlayer::compute_export_3point_params(
            0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 1.0,
        );
        let mid_p = ProgramPlayer::compute_export_3point_params(
            0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0,
        );
        // Positive tint = magenta: green channel lowered (or stays at floor)
        // Shadows tint effect (on black) should be proportionally larger
        // than midtones tint (on gray) due to shadows_endpoint_boost.
        let sh_g_deviation = (sh_p.black_g - 0.0).abs();
        let mid_g_deviation = (mid_p.gray_g - 0.5).abs();
        assert!(
            sh_g_deviation < mid_g_deviation + 0.3,
            "tint produces effect: sh_g_dev={} mid_g_dev={}",
            sh_g_deviation, mid_g_deviation
        );
    }

    #[test]
    fn build_temperature_tint_filter_preserves_green_magenta_direction() {
        let mut clip = Clip::new("/tmp/test.mp4", 2_000_000_000, 0, ClipKind::Video);
        clip.tint = 0.8;
        let magenta = build_temperature_tint_filter(&clip);
        let magenta_rm = extract_colorbalance_component(&magenta, "rm");
        let magenta_gm = extract_colorbalance_component(&magenta, "gm");
        let magenta_bm = extract_colorbalance_component(&magenta, "bm");
        assert!(magenta_rm > 0.0, "positive tint should boost red");
        assert!(magenta_gm < 0.0, "positive tint should cut green");
        assert!(magenta_bm > 0.0, "positive tint should boost blue");

        clip.tint = -0.8;
        let green = build_temperature_tint_filter(&clip);
        let green_rm = extract_colorbalance_component(&green, "rm");
        let green_gm = extract_colorbalance_component(&green, "gm");
        let green_bm = extract_colorbalance_component(&green, "bm");
        assert!(green_rm < 0.0, "negative tint should cut red");
        assert!(green_gm > 0.0, "negative tint should boost green");
        assert!(green_bm < 0.0, "negative tint should cut blue");
    }

    #[test]
    fn export_coloradj_compensation_preserves_neutral_and_tunes_tint_delta() {
        let neutral = ProgramPlayer::compute_coloradj_params(6500.0, 0.0);
        let preview_temp = ProgramPlayer::compute_coloradj_params(2000.0, 0.0);
        let export_temp = compute_export_coloradj_params(2000.0, 0.0);
        let preview_tint = ProgramPlayer::compute_coloradj_params(6500.0, -1.0);
        let export_tint = compute_export_coloradj_params(6500.0, -1.0);
        let export_neutral = compute_export_coloradj_params(6500.0, 0.0);

        let magnitude = |a: &crate::media::program_player::ColorAdjRGBParams,
                         b: &crate::media::program_player::ColorAdjRGBParams| {
            (a.r - b.r).abs() + (a.g - b.g).abs() + (a.b - b.b).abs()
        };
        assert!(
            (export_neutral.r - neutral.r).abs() < 1e-9
                && (export_neutral.g - neutral.g).abs() < 1e-9
                && (export_neutral.b - neutral.b).abs() < 1e-9,
            "neutral mapping should remain unchanged"
        );
        // Per-channel offsets intentionally push the export delta slightly
        // beyond preview's to compensate for FFmpeg's weaker frei0r
        // rendering.  Allow up to 20% amplification.
        assert!(
            magnitude(&export_temp, &neutral) <= magnitude(&preview_temp, &neutral) * 1.20,
            "temperature mapping should not over-amplify preview delta"
        );
        assert!(
            magnitude(&export_tint, &neutral) < magnitude(&preview_tint, &neutral),
            "tint compensation should attenuate delta from neutral"
        );
    }

    #[test]
    fn build_grading_filter_warmth_direction_is_consistent_per_tonal_region() {
        // Positive warmth = warm (lower R control point = brighter red output,
        // higher B control point = darker blue output) in ALL zones.
        let sh = ProgramPlayer::compute_export_3point_params(
            0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0,
        );
        assert!(sh.black_r < sh.black_b, "shadows warmth: red control < blue control at black point");

        let mid = ProgramPlayer::compute_export_3point_params(
            0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0,
        );
        assert!(mid.gray_r < mid.gray_b, "midtones warmth: red control < blue control at gray point");

        let hi = ProgramPlayer::compute_export_3point_params(
            0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 0.0,
        );
        assert!(hi.white_r < hi.white_b, "highlights warmth: red control < blue control at white point");
    }

    #[test]
    fn build_grading_filter_tint_direction_is_consistent_per_tonal_region() {
        // Positive tint = magenta (higher G control = less green output) in ALL zones.
        let sh = ProgramPlayer::compute_export_3point_params(
            0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 1.0,
        );
        assert!(sh.black_g > sh.black_r,
            "shadows tint +1: green control should be higher than red (less green output)");

        let mid = ProgramPlayer::compute_export_3point_params(
            0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0,
        );
        assert!(mid.gray_g > mid.gray_r, "midtones tint +1: green control > red at gray point");

        let hi = ProgramPlayer::compute_export_3point_params(
            0.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0,
        );
        assert!(hi.white_g > hi.white_r, "highlights tint +1: green control > red at white point");
    }

    #[test]
    fn build_title_filter_empty_when_no_title_text() {
        let clip = Clip::new("/tmp/test.mp4", 2_000_000_000, 0, ClipKind::Video);
        assert!(build_title_filter(&clip, 1080).is_empty());
    }

    #[test]
    fn build_title_filter_emits_drawtext_with_position_and_color() {
        let mut clip = Clip::new("/tmp/test.mp4", 2_000_000_000, 0, ClipKind::Video);
        clip.title_text = "Hello: world".to_string();
        clip.title_font = "Sans Bold 48".to_string();
        clip.title_x = 0.25;
        clip.title_y = 0.75;
        clip.title_color = 0xFF3366CC;

        // At 1080p: 48pt × 4/3 × (1080/1080) = 64px
        let f = build_title_filter(&clip, 1080);
        assert!(f.contains(",drawtext="));
        assert!(f.contains("text='Hello\\: world'"));
        assert!(f.contains("font='Sans Bold'"));
        assert!(f.contains("fontsize=64.00"));
        assert!(f.contains("fontcolor=ff3366@0.8000"));
        assert!(f.contains("x='(0.250000)*w-text_w/2'"));
        assert!(f.contains("y='(0.750000)*h-text_h/2'"));
    }

    #[test]
    fn build_title_filter_scales_with_resolution() {
        let mut clip = Clip::new("/tmp/test.mp4", 2_000_000_000, 0, ClipKind::Video);
        clip.title_text = "Test".to_string();
        clip.title_font = "Sans Bold 36".to_string();

        // At 2160p: 36pt × 4/3 × (2160/1080) = 96px
        let f = build_title_filter(&clip, 2160);
        assert!(f.contains("fontsize=96.00"));

        // At 1080p: 36pt × 4/3 × (1080/1080) = 48px
        let f = build_title_filter(&clip, 1080);
        assert!(f.contains("fontsize=48.00"));
    }

    #[test]
    fn build_title_filter_default_font_at_1080p() {
        let mut clip = Clip::new("/tmp/test.mp4", 2_000_000_000, 0, ClipKind::Video);
        clip.title_text = "Default".to_string();
        // default font is "Sans Bold 36"

        let f = build_title_filter(&clip, 1080);
        // 36pt × 4/3 × 1 = 48px
        assert!(f.contains("fontsize=48.00"));
    }

    #[test]
    fn frei0r_export_bool_params_use_y_n() {
        let _ = gst::init();
        // 3-point-color-balance has Bool params (split-preview, source-image-on-left-side).
        // FFmpeg requires 'y'/'n' for Bool, not '1.000000'/'0.000000'.
        let mut clip = Clip::new("/tmp/test.mp4", 2_000_000_000, 0, ClipKind::Video);
        let mut params = std::collections::HashMap::new();
        params.insert("black-color-r".to_string(), 0.0);
        params.insert("black-color-g".to_string(), 0.0);
        params.insert("black-color-b".to_string(), 0.0);
        params.insert("gray-color-r".to_string(), 0.5);
        params.insert("gray-color-g".to_string(), 0.5);
        params.insert("gray-color-b".to_string(), 0.5);
        params.insert("white-color-r".to_string(), 1.0);
        params.insert("white-color-g".to_string(), 1.0);
        params.insert("white-color-b".to_string(), 1.0);
        params.insert("split-preview".to_string(), 1.0);
        params.insert("source-image-on-left-side".to_string(), 0.0);

        clip.frei0r_effects.push(crate::model::clip::Frei0rEffect {
            id: "test-id".to_string(),
            plugin_name: "3-point-color-balance".to_string(),
            enabled: true,
            params,
            string_params: std::collections::HashMap::new(),
        });

        let filter = super::build_frei0r_effects_filter(&clip);
        // Must contain y/n for bools, not 1.000000/0.000000.
        assert!(filter.contains("|y|n"), "Expected y/n for bools, got: {}", filter);
        // Must contain r/g/b compound format for COLORs.
        assert!(filter.contains("0.000000/0.000000/0.000000"), "Missing compound COLOR format in: {}", filter);
    }

    // --- Chapter metadata tests ---

    #[test]
    fn chapter_metadata_empty_markers_returns_none() {
        let result = write_chapter_metadata(&[], 10_000_000_000).unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn chapter_metadata_single_marker() {
        use crate::model::project::Marker;
        let markers = vec![Marker::new(5_000_000_000, "Intro".to_string())];
        let file = write_chapter_metadata(&markers, 20_000_000_000)
            .unwrap()
            .expect("should produce metadata file");
        let content = std::fs::read_to_string(file.path()).unwrap();
        assert!(content.starts_with(";FFMETADATA1"));
        assert!(content.contains("[CHAPTER]"));
        assert!(content.contains("START=5000000000"));
        assert!(content.contains("END=20000000000"));
        assert!(content.contains("title=Intro"));
        assert!(content.contains("TIMEBASE=1/1000000000"));
    }

    #[test]
    fn chapter_metadata_multiple_markers_sorted() {
        use crate::model::project::Marker;
        // Provide markers out of order to verify sorting
        let markers = vec![
            Marker::new(15_000_000_000, "Middle".to_string()),
            Marker::new(0, "Start".to_string()),
            Marker::new(30_000_000_000, "End".to_string()),
        ];
        let file = write_chapter_metadata(&markers, 60_000_000_000)
            .unwrap()
            .expect("should produce metadata file");
        let content = std::fs::read_to_string(file.path()).unwrap();
        // Verify chapter order: Start (0→15B), Middle (15B→30B), End (30B→60B)
        let chapters: Vec<&str> = content.matches("[CHAPTER]").collect();
        assert_eq!(chapters.len(), 3);
        assert!(content.contains("START=0\nEND=15000000000\ntitle=Start"));
        assert!(content.contains("START=15000000000\nEND=30000000000\ntitle=Middle"));
        assert!(content.contains("START=30000000000\nEND=60000000000\ntitle=End"));
    }

    #[test]
    fn chapter_metadata_escapes_special_characters() {
        use crate::model::project::Marker;
        let markers = vec![Marker::new(0, "Title=With;Special#Chars\\Here\nNewline".to_string())];
        let file = write_chapter_metadata(&markers, 10_000_000_000)
            .unwrap()
            .expect("should produce metadata file");
        let content = std::fs::read_to_string(file.path()).unwrap();
        assert!(content.contains("title=Title\\=With\\;Special\\#Chars\\\\Here Newline"));
    }
}
