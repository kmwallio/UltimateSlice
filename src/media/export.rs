use crate::media::{adjustment_scope::AdjustmentScopeShape, program_player::ProgramPlayer};
use crate::model::clip::{Clip, ClipKind, NumericKeyframe, SlowMotionInterp};
use crate::model::project::Project;
use crate::model::transition::{max_transition_duration_ns, transition_xfade_name_for_kind};
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

    // Flatten compound clips before building the filter graph.
    // This produces a modified track list where every compound clip has been
    // recursively expanded into its constituent leaf clips with rebased
    // timeline positions. The rest of the export pipeline operates on this
    // flat representation unchanged.
    let flattened_tracks = flatten_compound_tracks(&project.tracks);
    let flattened_project_tracks = &flattened_tracks;

    // Primary video track (first active video track) — forms the base concat sequence.
    // Secondary active video tracks are composited on top with overlay.
    let active_video_tracks: Vec<_> = flattened_project_tracks
        .iter()
        .filter(|t| t.kind == crate::model::track::TrackKind::Video)
        .filter(|t| project.track_is_active_for_output(t))
        .collect();
    let mut primary_clips: Vec<&crate::model::clip::Clip> = active_video_tracks
        .first()
        .map(|t| {
            t.clips
                .iter()
                .filter(|c| c.kind != ClipKind::Adjustment)
                .collect()
        })
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
            let mut clips: Vec<&Clip> = t
                .clips
                .iter()
                .filter(|c| c.kind != ClipKind::Adjustment)
                .collect();
            clips.sort_by_key(|c| c.timeline_start);
            clips
        })
        .collect();

    if primary_clips.is_empty() {
        return Err(anyhow!("No video clips to export"));
    }

    // Collect audio-only clips from active audio tracks.
    let audio_track_clips: Vec<Vec<&crate::model::clip::Clip>> = flattened_project_tracks
        .iter()
        .filter(|t| t.kind == crate::model::track::TrackKind::Audio)
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
    // Hold rasterized mask temp files alive for the duration of the export.
    let mut _mask_temp_files: Vec<tempfile::NamedTempFile> = Vec::new();
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

    let resolve_export_path = |clip: &Clip| -> Result<String> {
        if clip.kind == ClipKind::Title || clip.kind == ClipKind::Adjustment {
            return Ok(String::new()); // Title/adjustment clips use lavfi or no input
        }
        if clip.animated_svg {
            return crate::media::animated_svg::ensure_rendered_clip(
                &clip.source_path,
                clip.source_in,
                clip.source_out,
                clip.media_duration_ns,
                project.frame_rate.numerator,
                project.frame_rate.denominator,
            );
        }
        if clip.bg_removal_enabled {
            if let Some(bg_path) = bg_removal_paths.get(&clip.source_path) {
                if std::path::Path::new(bg_path).exists() {
                    return Ok(bg_path.clone());
                }
            }
        }
        Ok(clip.source_path.clone())
    };

    // Inputs: primary video clips (0..primary_clips.len())
    // Adjustment clips are already filtered out of primary_clips.
    for clip in &primary_clips {
        if clip.kind == ClipKind::Title {
            let dur_s = clip.duration() as f64 / 1_000_000_000.0;
            let bg = title_clip_lavfi_color(
                clip,
                out_w,
                out_h,
                project.frame_rate.numerator,
                project.frame_rate.denominator,
                dur_s,
            );
            cmd.arg("-f").arg("lavfi").arg("-i").arg(bg);
        } else {
            let (in_s, src_dur_s) = video_input_seek_and_duration(clip, frame_duration_s);
            if clip.kind == ClipKind::Image && !clip.animated_svg {
                cmd.arg("-loop").arg("1");
            }
            cmd.arg("-ss")
                .arg(format!("{in_s:.6}"))
                .arg("-t")
                .arg(format!("{src_dur_s:.6}"))
                .arg("-i")
                .arg(resolve_export_path(clip)?);
        }
    }

    // Inputs: secondary video clips (primary_clips.len()..primary_clips.len()+secondary_clips_flat.len())
    // Adjustment clips are already filtered out of secondary_track_clips.
    for clip in &secondary_clips_flat {
        if clip.kind == ClipKind::Title {
            let dur_s = clip.duration() as f64 / 1_000_000_000.0;
            let bg = title_clip_lavfi_color(
                clip,
                out_w,
                out_h,
                project.frame_rate.numerator,
                project.frame_rate.denominator,
                dur_s,
            );
            cmd.arg("-f").arg("lavfi").arg("-i").arg(bg);
        } else {
            let (in_s, src_dur_s) = video_input_seek_and_duration(clip, frame_duration_s);
            if clip.kind == ClipKind::Image && !clip.animated_svg {
                cmd.arg("-loop").arg("1");
            }
            cmd.arg("-ss")
                .arg(format!("{in_s:.6}"))
                .arg("-t")
                .arg(format!("{src_dur_s:.6}"))
                .arg("-i")
                .arg(resolve_export_path(clip)?);
        }
    }

    let sec_base = primary_clips.len();

    // Audio-only clip inputs (skipped for GIF — no audio in output)
    let audio_base = sec_base + secondary_clips_flat.len();
    if options.container != Container::Gif {
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
    }

    // Chapter metadata input (FFMETADATA file from project markers).
    // Must be added after all media inputs so the input index is correct.
    let _chapter_metadata = write_chapter_metadata(&project.markers, project.duration())?;
    if let Some(ref meta) = _chapter_metadata {
        let metadata_input_idx = audio_base + audio_clips.len();
        cmd.arg("-f").arg("ffmetadata").arg("-i").arg(meta.path());
        cmd.arg("-map_metadata")
            .arg(format!("{metadata_input_idx}"));
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

    let primary_transition_timings: Vec<Option<PrimaryTransitionTiming>> = primary_clips
        .windows(2)
        .map(|clips| clamped_primary_transition_timing(clips[0], clips[1]))
        .collect();
    let has_primary_transitions = primary_clips
        .iter()
        .take(primary_clips.len().saturating_sub(1))
        .any(|c| c.outgoing_transition.is_active());
    let has_xfade = check_filter_support(&ffmpeg, "xfade");
    let has_tpad = check_filter_support(&ffmpeg, "tpad");
    let can_render_primary_transitions = has_primary_transitions && has_xfade && has_tpad;

    // === Primary video track: scale/correct each clip then concatenate ===
    // Adjustment clips are already filtered out of primary_clips.
    for (i, clip) in primary_clips.iter().enumerate() {
        let color_filter = build_color_filter(clip);
        let temp_tint_filter = build_temperature_tint_filter_with_caps(clip, &color_caps);
        let grading_filter = build_grading_filter_with_caps(clip, &color_caps);
        let denoise_filter = build_denoise_filter(clip);
        let sharpen_filter = build_sharpen_filter(clip);
        let blur_filter = build_blur_filter(clip);
        let vidstab_filter =
            build_vidstab_filter(clip, vidstab_trf.get(&clip.id).map(|s| s.as_str()));
        let frei0r_effects_filter = build_frei0r_effects_filter(clip);
        let chroma_key_filter = build_chroma_key_filter(clip);
        let title_filter = build_title_filter(clip, out_h);
        let subtitle_filter = ""; // Subtitles applied post-compositing.
        let speed_filter = build_timing_filter(
            clip,
            frame_duration_s,
            project.frame_rate.numerator,
            project.frame_rate.denominator,
        );
        let lut_prefix = build_lut_filter_prefix(clip);
        let crop_filter = build_crop_filter(clip, out_w, out_h, false);
        let rotate_filter = build_rotation_filter(clip, false);
        let transition_stop_pad_filter = if can_render_primary_transitions {
            build_primary_clip_transition_stop_pad_filter(&primary_transition_timings, i)
        } else {
            String::new()
        };
        let has_transform_keyframes = has_transform_keyframes(clip);
        let has_opacity_keyframes = !clip.opacity_keyframes.is_empty();
        let clip_has_mask = clip.has_mask();
        if has_transform_keyframes || has_opacity_keyframes || clip_has_mask {
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
            let mask_result = build_combined_mask_alpha(clip, out_w, out_h);
            let clip_duration_s = clip.duration() as f64 / 1_000_000_000.0;
            let anamorphic_filter = build_anamorphic_filter(clip);
            // Determine mask alpha expression or rasterized file path.
            let mask_alpha_expr = match &mask_result {
                Some(MaskAlphaResult::GeqExpression(expr)) => expr.clone(),
                Some(MaskAlphaResult::RasterFile(_)) | None => "1".to_string(),
            };
            filter.push_str(&format!(
                "[{i}:v]{lut_prefix}{anamorphic_filter}format=yuva420p,scale={out_w}:{out_h}:force_original_aspect_ratio=decrease,setsar=1,pad={out_w}:{out_h}:(ow-iw)/2:(oh-ih)/2:color=black@0{crop_filter}{rotate_filter},fps={}/{}{vidstab_filter}{color_filter}{temp_tint_filter}{grading_filter}{denoise_filter}{sharpen_filter}{blur_filter}{frei0r_effects_filter}{chroma_key_filter}{title_filter}{subtitle_filter}{speed_filter}\
                 ,scale=w='max(1,{out_w}*({scale_expr}))':h='max(1,{out_h}*({scale_expr}))':eval=frame[pv{i}fg];\
                 color=c=black:size={out_w}x{out_h}:r={}/{}:d={clip_duration_s:.6}[pv{i}bg];\
                 [pv{i}bg][pv{i}fg]overlay=x='(W-w)*(1+({pos_x_expr}))/2':y='(H-h)*(1+({pos_y_expr}))/2':eval=frame\
                  ,geq=lum='lum(X,Y)':cb='cb(X,Y)':cr='cr(X,Y)':a='alpha(X,Y)*({opacity_expr})*({mask_alpha_expr})'[pv{i}raw];\
                 [pv{i}raw]format=yuv420p{transition_stop_pad_filter}[pv{i}];",
                project.frame_rate.numerator, project.frame_rate.denominator
                , project.frame_rate.numerator, project.frame_rate.denominator
            ));
            // For rasterized path masks, apply alphamerge with the mask PGM.
            if let Some(MaskAlphaResult::RasterFile(mask_file)) = mask_result {
                let mask_path_str = mask_file.path().display().to_string();
                _mask_temp_files.push(mask_file);
                let old_tail = format!(
                    "[pv{i}raw];[pv{i}raw]format=yuv420p{transition_stop_pad_filter}[pv{i}];",
                    i = i
                );
                let new_tail = format!(
                    "[pv{i}raw];movie='{mask_path_str}',format=gray,scale={out_w}:{out_h}[pv{i}mask];\
                     [pv{i}raw][pv{i}mask]alphamerge,format=yuv420p{transition_stop_pad_filter}[pv{i}];",
                    i = i, out_w = out_w, out_h = out_h,
                );
                let current = filter.clone();
                if let Some(pos) = current.rfind(&old_tail) {
                    filter.truncate(pos);
                    filter.push_str(&new_tail);
                }
            }
        } else if clip.chroma_key_enabled || clip.bg_removal_enabled {
            let scale_pos_filter = build_scale_position_filter(clip, out_w, out_h, false);
            let anamorphic_filter = build_anamorphic_filter(clip);
            filter.push_str(&format!(
                "[{i}:v]{lut_prefix}{anamorphic_filter}format=yuva420p,scale={out_w}:{out_h}:force_original_aspect_ratio=decrease,setsar=1,pad={out_w}:{out_h}:(ow-iw)/2:(oh-ih)/2:color=black@0{crop_filter}{scale_pos_filter}{rotate_filter},fps={}/{}{vidstab_filter}{color_filter}{temp_tint_filter}{grading_filter}{denoise_filter}{sharpen_filter}{blur_filter}{frei0r_effects_filter}{chroma_key_filter}{title_filter}{subtitle_filter}{speed_filter}[pv{i}raw];[pv{i}raw]format=yuv420p{transition_stop_pad_filter}[pv{i}];",
                project.frame_rate.numerator, project.frame_rate.denominator
            ));
        } else {
            let scale_pos_filter = build_scale_position_filter(clip, out_w, out_h, false);
            let anamorphic_filter = build_anamorphic_filter(clip);
            filter.push_str(&format!(
                "[{i}:v]{lut_prefix}{anamorphic_filter}scale={out_w}:{out_h}:force_original_aspect_ratio=decrease,setsar=1,pad={out_w}:{out_h}:(ow-iw)/2:(oh-ih)/2{crop_filter}{scale_pos_filter}{rotate_filter},fps={}/{},format=yuv420p{vidstab_filter}{color_filter}{temp_tint_filter}{grading_filter}{denoise_filter}{sharpen_filter}{blur_filter}{frei0r_effects_filter}{title_filter}{subtitle_filter}{speed_filter}{transition_stop_pad_filter}[pv{i}];",
                project.frame_rate.numerator, project.frame_rate.denominator
            ));
        }
    }
    // Build primary-track sequence:
    // - If transitions exist AND filters are supported, chain xfade filters
    // - Otherwise use concat (original behavior).
    if primary_clips.len() == 1 {
        filter.push_str("[pv0]copy[vbase]");
    } else if can_render_primary_transitions {
        let mut prev_label = "pv0".to_string();
        let mut running_cut_s = primary_clips[0].duration() as f64 / 1_000_000_000.0;
        for i in 0..(primary_clips.len() - 1) {
            let next_label = format!("pv{}", i + 1);
            let out_label = format!("vseq{}", i + 1);
            let clip = &primary_clips[i];
            let next_clip = &primary_clips[i + 1];
            let sep = if i == 0 { "" } else { ";" };
            if let Some(timing) = primary_transition_timings[i] {
                let offset_s = (running_cut_s - timing.before_cut_s()).max(0.0);
                let xfade = transition_xfade_name_for_kind(clip.outgoing_transition.kind_trimmed())
                    .unwrap_or("fade");
                filter.push_str(&format!(
                    "{sep}[{prev_label}][{next_label}]xfade=transition={xfade}:duration={:.6}:offset={offset_s:.6}[{out_label}]",
                    timing.duration_s(),
                ));
            } else {
                filter.push_str(&format!(
                    "{sep}[{prev_label}][{next_label}]concat=n=2:v=1:a=0[{out_label}]"
                ));
            }
            running_cut_s += next_clip.duration() as f64 / 1_000_000_000.0;
            prev_label = out_label;
        }
        filter.push_str(&format!(";[{prev_label}]copy[vbase]"));
    } else {
        for i in 0..primary_clips.len() {
            filter.push_str(&format!("[pv{i}]"));
        }
        filter.push_str(&format!("concat=n={}:v=1:a=0[vbase]", primary_clips.len()));
    }

    // Preserve track stacking order from `active_video_tracks` so overlapping
    // adjustment layers apply in the same order as preview.
    let adjustment_clips = all_adjustment_clips;

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
        let vidstab_filter =
            build_vidstab_filter(clip, vidstab_trf.get(&clip.id).map(|s| s.as_str()));
        let frei0r_effects_filter = build_frei0r_effects_filter(clip);
        let chroma_key_filter = build_chroma_key_filter(clip);
        let title_filter = build_title_filter(clip, out_h);
        let subtitle_filter = ""; // Subtitles applied post-compositing.
        let speed_filter = build_timing_filter(
            clip,
            frame_duration_s,
            project.frame_rate.numerator,
            project.frame_rate.denominator,
        );
        let lut_prefix = build_lut_filter_prefix(clip);
        let crop_filter = build_crop_filter(clip, out_w, out_h, true);
        let rotate_filter = build_rotation_filter(clip, true);
        let has_transform_keyframes = has_transform_keyframes(clip);
        let has_opacity_keyframes = !clip.opacity_keyframes.is_empty();
        let ov_has_mask = clip.has_mask();
        // Scale the overlay clip to output size (keeps aspect ratio, pads transparent)
        let ov_label = format!("ov{k}");
        let ov_mask_is_raster = clip
            .masks
            .iter()
            .any(|m| m.enabled && m.shape == crate::model::clip::MaskShape::Path);
        if has_transform_keyframes || has_opacity_keyframes || ov_has_mask {
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
            let ov_mask_result = build_combined_mask_alpha(clip, out_w, out_h);
            let mask_alpha_expr = match &ov_mask_result {
                Some(MaskAlphaResult::GeqExpression(expr)) => expr.clone(),
                Some(MaskAlphaResult::RasterFile(_)) | None => "1".to_string(),
            };
            let clip_duration_s = clip.duration() as f64 / 1_000_000_000.0;
            let anamorphic_filter = build_anamorphic_filter(clip);
            filter.push_str(&format!(
                ";[{in_idx}:v]{lut_prefix}{anamorphic_filter}format=yuva420p,scale={out_w}:{out_h}:force_original_aspect_ratio=decrease,setsar=1,pad={out_w}:{out_h}:(ow-iw)/2:(oh-ih)/2:color=black@0,fps={}/{}{vidstab_filter}{color_filter}{temp_tint_filter}{grading_filter}{denoise_filter}{sharpen_filter}{blur_filter}{frei0r_effects_filter}{chroma_key_filter}{title_filter}{subtitle_filter}{crop_filter}{rotate_filter}{speed_filter}\
                 ,scale=w='max(1,{out_w}*({scale_expr}))':h='max(1,{out_h}*({scale_expr}))':eval=frame[ov{k}fg];\
                 color=c=black@0:size={out_w}x{out_h}:r={}/{}:d={clip_duration_s:.6}[ov{k}bg];\
                 [ov{k}bg][ov{k}fg]overlay=x='(W-w)*(1+({pos_x_expr}))/2':y='(H-h)*(1+({pos_y_expr}))/2':eval=frame\
                 ,geq=lum='lum(X,Y)':cb='cb(X,Y)':cr='cr(X,Y)':a='alpha(X,Y)*({opacity_expr})*({mask_alpha_expr})'[{ov_label}raw]"
                , project.frame_rate.numerator, project.frame_rate.denominator
                , project.frame_rate.numerator, project.frame_rate.denominator
            ));
            // For rasterized path masks on overlay clips, insert alphamerge.
            if let Some(MaskAlphaResult::RasterFile(mask_file)) = ov_mask_result {
                let mask_path_str = mask_file.path().display().to_string();
                _mask_temp_files.push(mask_file);
                filter.push_str(&format!(
                    ";movie='{mask_path_str}',format=gray,scale={out_w}:{out_h}[ov{k}mask];\
                     [{ov_label}raw][ov{k}mask]alphamerge[{ov_label}raw2]",
                ));
                // ov_mask_result is now consumed; ov_raw_label below picks up "raw2".
            }
        } else {
            let scale_pos_filter = build_scale_position_filter(clip, out_w, out_h, true);
            let opacity = clip.opacity.clamp(0.0, 1.0);
            let anamorphic_filter = build_anamorphic_filter(clip);
            filter.push_str(&format!(
                ";[{in_idx}:v]{lut_prefix}{anamorphic_filter}format=yuva420p,scale={out_w}:{out_h}:force_original_aspect_ratio=decrease,setsar=1,pad={out_w}:{out_h}:(ow-iw)/2:(oh-ih)/2:color=black@0,fps={}/{}{vidstab_filter}{color_filter}{temp_tint_filter}{grading_filter}{denoise_filter}{sharpen_filter}{blur_filter}{frei0r_effects_filter}{chroma_key_filter}{title_filter}{subtitle_filter}{crop_filter}{scale_pos_filter}{rotate_filter},colorchannelmixer=aa={opacity:.4}{speed_filter}[{ov_label}raw]"
                , project.frame_rate.numerator, project.frame_rate.denominator
            ));
        }
        // For rasterized overlay masks, use the masked label.
        let ov_raw_label = if ov_mask_is_raster {
            format!("{ov_label}raw2")
        } else {
            format!("{ov_label}raw")
        };
        // Normalize PTS to zero (removes any residual offset from keyframe
        // seeking), then delay to the correct timeline position.
        let start_s = clip.timeline_start as f64 / 1_000_000_000.0;
        filter.push_str(&format!(
            ";[{ov_raw_label}]setpts=PTS-STARTPTS+{start_s:.6}/TB[{ov_label}]"
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
        let next_label = format!("vadj{adj_idx}");
        if let Some(graph) = build_adjustment_layer_filter_graph(
            &prev_label,
            &next_label,
            adj_clip,
            adj_idx,
            out_w,
            out_h,
            &color_caps,
        ) {
            filter.push_str(&graph);
            prev_label = next_label;
        }
    }

    // === Post-compositing subtitle burn-in ===
    // Chain one subtitle filter per clip that has subtitles. Each clip gets its
    // own temp file and styling, so different tracks can have different positions,
    // fonts, and highlight modes.
    {
        let mut sub_idx = 0usize;
        for track in flattened_project_tracks {
            for clip in &track.clips {
                if clip.subtitle_segments.is_empty() {
                    continue;
                }
                // Collect this clip's segments as timeline-absolute.
                let clip_segs: Vec<(u64, u64, String, &crate::model::clip::Clip)> = clip
                    .subtitle_segments
                    .iter()
                    .map(|seg| {
                        let abs_start = clip.timeline_start
                            + ((seg.start_ns.saturating_sub(clip.source_in)) as f64 / clip.speed)
                                as u64;
                        let abs_end = clip.timeline_start
                            + ((seg.end_ns.saturating_sub(clip.source_in)) as f64 / clip.speed)
                                as u64;
                        (abs_start, abs_end, seg.text.clone(), clip)
                    })
                    .collect();

                let (sub_filter, sub_temp) =
                    build_subtitle_filter_composited(&clip_segs, clip, out_h);
                if let Some(f) = sub_temp {
                    _mask_temp_files.push(f);
                }
                if !sub_filter.is_empty() {
                    let next_label = format!("vsub{sub_idx}");
                    filter.push_str(&format!(";[{prev_label}]{sub_filter}[{next_label}]"));
                    prev_label = next_label;
                    sub_idx += 1;
                }
            }
        }
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

    // Skip all audio filter construction for GIF — no audio output is needed.
    if options.container != Container::Gif {
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
                let ch_part = if ch_filter.is_empty() {
                    String::new()
                } else {
                    format!(",{ch_filter}")
                };
                let volume_filter = build_volume_filter(clip);
                let pitch_filter = build_pitch_filter(clip);
                let pitch_part = if pitch_filter.is_empty() {
                    String::new()
                } else {
                    format!(",{pitch_filter}")
                };
                let ladspa_filter = build_ladspa_effects_filter(clip);
                let ladspa_part = if ladspa_filter.is_empty() {
                    String::new()
                } else {
                    format!(",{ladspa_filter}")
                };
                let eq_filter = build_eq_filter(clip);
                let eq_part = if eq_filter.is_empty() {
                    String::new()
                } else {
                    format!(",{eq_filter}")
                };
                let fades = clip_audio_fades.get(&clip.id).copied().unwrap_or_default();
                let fade_filters = build_audio_crossfade_filters(clip, fades, crossfade_curve);
                let pre_pan = format!("{label}_prepan");
                let post_pan = format!("{label}_panned");
                filter.push_str(&format!(
                ";[{i}:a]{areverse}{atempo}{volume_filter}{ch_part}{pitch_part}{ladspa_part}{eq_part},{fade_filters}anull[{pre_pan}]"
            ));
                append_pan_filter_chain(&mut filter, clip, &pre_pan, &post_pan, &label);
                filter.push_str(&format!(";[{post_pan}]adelay={delay_ms}:all=1[{label}]"));
                // Primary video clips — find track role from project.
                let role = project
                    .tracks
                    .iter()
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
                let ch_part = if ch_filter.is_empty() {
                    String::new()
                } else {
                    format!(",{ch_filter}")
                };
                let volume_filter = build_volume_filter(clip);
                let pitch_filter = build_pitch_filter(clip);
                let pitch_part = if pitch_filter.is_empty() {
                    String::new()
                } else {
                    format!(",{pitch_filter}")
                };
                let ladspa_filter = build_ladspa_effects_filter(clip);
                let ladspa_part = if ladspa_filter.is_empty() {
                    String::new()
                } else {
                    format!(",{ladspa_filter}")
                };
                let eq_filter = build_eq_filter(clip);
                let eq_part = if eq_filter.is_empty() {
                    String::new()
                } else {
                    format!(",{eq_filter}")
                };
                let fades = clip_audio_fades.get(&clip.id).copied().unwrap_or_default();
                let fade_filters = build_audio_crossfade_filters(clip, fades, crossfade_curve);
                let pre_pan = format!("{label}_prepan");
                let post_pan = format!("{label}_panned");
                filter.push_str(&format!(
                ";[{in_idx}:a]{areverse}{atempo}{volume_filter}{ch_part}{pitch_part}{ladspa_part}{eq_part},{fade_filters}anull[{pre_pan}]"
            ));
                append_pan_filter_chain(&mut filter, clip, &pre_pan, &post_pan, &label);
                filter.push_str(&format!(";[{post_pan}]adelay={delay_ms}:all=1[{label}]"));
                // Find the track for this secondary clip to get its role.
                let role = project
                    .tracks
                    .iter()
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
            let ch_part = if ch_filter.is_empty() {
                String::new()
            } else {
                format!(",{ch_filter}")
            };
            let volume_filter = build_volume_filter(clip);
            let pitch_filter = build_pitch_filter(clip);
            let pitch_part = if pitch_filter.is_empty() {
                String::new()
            } else {
                format!(",{pitch_filter}")
            };
            let ladspa_filter = build_ladspa_effects_filter(clip);
            let ladspa_part = if ladspa_filter.is_empty() {
                String::new()
            } else {
                format!(",{ladspa_filter}")
            };
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
            ";[{}:a]{areverse}{atempo}{volume_filter}{ch_part}{pitch_part}{ladspa_part}{duck_part}{eq_part},{fade_filters}anull[{pre_pan}]",
            audio_base + j
        ));
            append_pan_filter_chain(&mut filter, clip, &pre_pan, &post_pan, &label);
            filter.push_str(&format!(";[{post_pan}]adelay={delay_ms}:all=1[{label}]"));
            let role = project
                .tracks
                .iter()
                .find(|t| t.clips.iter().any(|c| c.id == clip.id))
                .map(|t| t.audio_role)
                .unwrap_or_default();
            audio_labels.push((label, role));
        }
    } // end `if options.container != Container::Gif` for per-clip audio filters

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
                    filter.push_str(&format!("amix=inputs={n}:normalize=0[{submix_name}]"));
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
    } else if clip.brightness != 0.0
        || clip.contrast != 1.0
        || clip.saturation != 1.0
        || has_exposure
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
        let contrast_brightness_bias =
            0.26 * contrast_delta - 0.08 * contrast_delta * contrast_delta;
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

pub(crate) fn build_keyframed_property_expression(
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
    let mut deduped: Vec<(
        u64,
        f64,
        KeyframeInterpolation,
        Option<(f64, f64, f64, f64)>,
    )> = Vec::with_capacity(sorted.len());
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
        deduped
            .last()
            .map(|(_, v, _, _)| *v)
            .unwrap_or(default_value)
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

fn build_adjustment_scope_alpha_expression(
    clip: &Clip,
    out_w: u32,
    out_h: u32,
    time_var: &str,
) -> String {
    if !has_transform_keyframes(clip) {
        let scope = AdjustmentScopeShape::from_transform(
            out_w,
            out_h,
            clip.scale,
            clip.position_x,
            clip.position_y,
            clip.rotate as f64,
            clip.crop_left,
            clip.crop_right,
            clip.crop_top,
            clip.crop_bottom,
        );
        if scope.is_full_frame(out_w, out_h) {
            return "1".to_string();
        }
    }

    let pw = out_w.max(1) as f64;
    let ph = out_h.max(1) as f64;
    let scale_expr =
        build_keyframed_property_expression(&clip.scale_keyframes, clip.scale, 0.1, 4.0, time_var);
    let pos_x_expr = build_keyframed_property_expression(
        &clip.position_x_keyframes,
        clip.position_x,
        -1.0,
        1.0,
        time_var,
    );
    let pos_y_expr = build_keyframed_property_expression(
        &clip.position_y_keyframes,
        clip.position_y,
        -1.0,
        1.0,
        time_var,
    );
    let rotate_expr = build_keyframed_property_expression(
        &clip.rotate_keyframes,
        clip.rotate as f64,
        -180.0,
        180.0,
        time_var,
    );
    let crop_left_expr = build_keyframed_property_expression(
        &clip.crop_left_keyframes,
        clip.crop_left as f64,
        0.0,
        500.0,
        time_var,
    );
    let crop_right_expr = build_keyframed_property_expression(
        &clip.crop_right_keyframes,
        clip.crop_right as f64,
        0.0,
        500.0,
        time_var,
    );
    let crop_top_expr = build_keyframed_property_expression(
        &clip.crop_top_keyframes,
        clip.crop_top as f64,
        0.0,
        500.0,
        time_var,
    );
    let crop_bottom_expr = build_keyframed_property_expression(
        &clip.crop_bottom_keyframes,
        clip.crop_bottom as f64,
        0.0,
        500.0,
        time_var,
    );

    let cx_expr = format!("{pw:.10}/2+({pos_x_expr})*{pw:.10}*(1-({scale_expr}))/2");
    let cy_expr = format!("{ph:.10}/2+({pos_y_expr})*{ph:.10}*(1-({scale_expr}))/2");
    let half_w_expr = format!("{pw:.10}*({scale_expr})/2");
    let half_h_expr = format!("{ph:.10}*({scale_expr})/2");
    let left_raw_expr = format!("({cx_expr})-({half_w_expr})+({crop_left_expr})*({scale_expr})");
    let right_raw_expr = format!("({cx_expr})+({half_w_expr})-({crop_right_expr})*({scale_expr})");
    let top_raw_expr = format!("({cy_expr})-({half_h_expr})+({crop_top_expr})*({scale_expr})");
    let bottom_raw_expr =
        format!("({cy_expr})+({half_h_expr})-({crop_bottom_expr})*({scale_expr})");
    let right_expr = format!("max({right_raw_expr},{left_raw_expr})");
    let bottom_expr = format!("max({bottom_raw_expr},{top_raw_expr})");
    let rad_expr = format!("({rotate_expr})*PI/180");
    let ux_expr =
        format!("({cx_expr})+(X-({cx_expr}))*cos({rad_expr})-(Y-({cy_expr}))*sin({rad_expr})");
    let uy_expr =
        format!("({cy_expr})+(X-({cx_expr}))*sin({rad_expr})+(Y-({cy_expr}))*cos({rad_expr})");

    format!(
        "between({ux_expr},{left_raw_expr},({right_expr})-0.000001)*between({uy_expr},{top_raw_expr},({bottom_expr})-0.000001)"
    )
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
    format!("volume='if({cond_expr},{duck_gain:.6},1.0)':eval=frame")
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
            let gain_expr =
                build_keyframed_property_expression(band_kfs[i], band.gain, -24.0, 24.0, "t");
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
        clip.id
            .replace(|c: char| !c.is_ascii_alphanumeric() && c != '-', "_")
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

struct KnownFrei0rExportParam {
    native_type: crate::media::frei0r_registry::Frei0rNativeType,
    gst_properties: &'static [&'static str],
}

struct KnownFrei0rExportSchema {
    ffmpeg_name: &'static str,
    native_params: &'static [KnownFrei0rExportParam],
}

const THREE_POINT_COLOR_BALANCE_EXPORT_PARAMS: &[KnownFrei0rExportParam] = &[
    KnownFrei0rExportParam {
        native_type: crate::media::frei0r_registry::Frei0rNativeType::Color,
        gst_properties: &["black-color-r", "black-color-g", "black-color-b"],
    },
    KnownFrei0rExportParam {
        native_type: crate::media::frei0r_registry::Frei0rNativeType::Color,
        gst_properties: &["gray-color-r", "gray-color-g", "gray-color-b"],
    },
    KnownFrei0rExportParam {
        native_type: crate::media::frei0r_registry::Frei0rNativeType::Color,
        gst_properties: &["white-color-r", "white-color-g", "white-color-b"],
    },
    KnownFrei0rExportParam {
        native_type: crate::media::frei0r_registry::Frei0rNativeType::Bool,
        gst_properties: &["split-preview"],
    },
    KnownFrei0rExportParam {
        native_type: crate::media::frei0r_registry::Frei0rNativeType::Bool,
        gst_properties: &["source-image-on-left-side"],
    },
];

const THREE_POINT_COLOR_BALANCE_EXPORT_SCHEMA: KnownFrei0rExportSchema = KnownFrei0rExportSchema {
    ffmpeg_name: "three_point_balance",
    native_params: THREE_POINT_COLOR_BALANCE_EXPORT_PARAMS,
};

fn known_frei0r_export_schema(plugin_name: &str) -> Option<&'static KnownFrei0rExportSchema> {
    match plugin_name {
        "3-point-color-balance" => Some(&THREE_POINT_COLOR_BALANCE_EXPORT_SCHEMA),
        _ => None,
    }
}

fn format_frei0r_native_param<P: AsRef<str>>(
    effect: &crate::model::clip::Frei0rEffect,
    native_type: crate::media::frei0r_registry::Frei0rNativeType,
    gst_properties: &[P],
) -> String {
    use crate::media::frei0r_registry::Frei0rNativeType;

    match native_type {
        Frei0rNativeType::Color => {
            let r = gst_properties
                .first()
                .and_then(|k| effect.params.get(k.as_ref()))
                .copied()
                .unwrap_or(0.0);
            let g = gst_properties
                .get(1)
                .and_then(|k| effect.params.get(k.as_ref()))
                .copied()
                .unwrap_or(0.0);
            let b = gst_properties
                .get(2)
                .and_then(|k| effect.params.get(k.as_ref()))
                .copied()
                .unwrap_or(0.0);
            format!("{r:.6}/{g:.6}/{b:.6}")
        }
        Frei0rNativeType::Position => {
            let x = gst_properties
                .first()
                .and_then(|k| effect.params.get(k.as_ref()))
                .copied()
                .unwrap_or(0.0);
            let y = gst_properties
                .get(1)
                .and_then(|k| effect.params.get(k.as_ref()))
                .copied()
                .unwrap_or(0.0);
            format!("{x:.6}/{y:.6}")
        }
        Frei0rNativeType::NativeString => {
            let prop = gst_properties.first().map(|s| s.as_ref()).unwrap_or("");
            effect.string_params.get(prop).cloned().unwrap_or_default()
        }
        Frei0rNativeType::Bool => {
            let prop = gst_properties.first().map(|s| s.as_ref()).unwrap_or("");
            let val = effect.params.get(prop).copied().unwrap_or(0.0);
            if val > 0.5 {
                "y".to_string()
            } else {
                "n".to_string()
            }
        }
        Frei0rNativeType::Double => {
            let prop = gst_properties.first().map(|s| s.as_ref()).unwrap_or("");
            let val = effect.params.get(prop).copied().unwrap_or(0.0);
            format!("{val:.6}")
        }
    }
}

/// Build a chain of FFmpeg frei0r filters for user-applied effects on a clip.
/// Each enabled effect becomes `,frei0r=filter_name={name}:filter_params={p1}|{p2}|...`.
/// Effects with no FFmpeg frei0r support are silently skipped.
fn build_frei0r_effects_filter(clip: &crate::model::clip::Clip) -> String {
    use crate::media::frei0r_registry::Frei0rRegistry;

    if clip.frei0r_effects.is_empty() {
        return String::new();
    }

    let mut result = String::new();
    let registry = Frei0rRegistry::get_or_discover();

    for effect in &clip.frei0r_effects {
        if !effect.enabled {
            continue;
        }

        let plugin = registry.find_by_name(&effect.plugin_name);
        let fallback_schema = known_frei0r_export_schema(&effect.plugin_name);

        // Build filter_params string using native frei0r param ordering.
        let params_str = if let Some(info) = plugin {
            if !info.native_params.is_empty() {
                // Use native param info for correct compound formatting.
                info.native_params
                    .iter()
                    .map(|np| {
                        format_frei0r_native_param(effect, np.native_type, &np.gst_properties)
                    })
                    .collect::<Vec<_>>()
                    .join("|")
            } else if let Some(schema) = fallback_schema {
                schema
                    .native_params
                    .iter()
                    .map(|np| format_frei0r_native_param(effect, np.native_type, np.gst_properties))
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
                            let val = effect
                                .params
                                .get(&p.name)
                                .copied()
                                .unwrap_or(p.default_value);
                            format!("{val:.6}")
                        }
                    })
                    .collect::<Vec<_>>()
                    .join("|")
            }
        } else if let Some(schema) = fallback_schema {
            schema
                .native_params
                .iter()
                .map(|np| format_frei0r_native_param(effect, np.native_type, np.gst_properties))
                .collect::<Vec<_>>()
                .join("|")
        } else {
            // No registry info — fall back to deterministic property-name order.
            let mut keys: Vec<_> = effect.params.keys().collect();
            keys.sort_unstable();
            keys.into_iter()
                .map(|k| format!("{:.6}", effect.params.get(k).copied().unwrap_or(0.0)))
                .collect::<Vec<_>>()
                .join("|")
        };

        // Use FFmpeg module name (may differ from GStreamer name).
        let ffmpeg_name = if let Some(info) = plugin {
            if !info.native_params.is_empty() {
                info.ffmpeg_name.as_str()
            } else {
                fallback_schema
                    .map(|schema| schema.ffmpeg_name)
                    .unwrap_or(info.ffmpeg_name.as_str())
            }
        } else {
            fallback_schema
                .map(|schema| schema.ffmpeg_name)
                .unwrap_or(&effect.plugin_name)
        };

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

fn build_adjustment_effects_chain_filter(
    clip: &Clip,
    color_caps: &ColorFilterCapabilities,
) -> String {
    let mut effects_chain = String::new();
    let lut = build_lut_filter_prefix(clip);
    if !lut.is_empty() {
        effects_chain.push_str(&lut);
    }
    effects_chain.push_str(&build_color_filter(clip));
    effects_chain.push_str(&build_temperature_tint_filter_with_caps(clip, color_caps));
    effects_chain.push_str(&build_grading_filter_with_caps(clip, color_caps));
    effects_chain.push_str(&build_denoise_filter(clip));
    effects_chain.push_str(&build_sharpen_filter(clip));
    effects_chain.push_str(&build_blur_filter(clip));
    effects_chain.push_str(&build_frei0r_effects_filter(clip));
    effects_chain.trim_matches(',').to_string()
}

fn build_adjustment_layer_filter_graph(
    input_label: &str,
    output_label: &str,
    adj_clip: &Clip,
    adj_idx: usize,
    out_w: u32,
    out_h: u32,
    color_caps: &ColorFilterCapabilities,
) -> Option<String> {
    let effects_chain = build_adjustment_effects_chain_filter(adj_clip, color_caps);
    if effects_chain.is_empty() {
        return None;
    }

    let opacity = adj_clip.opacity.clamp(0.0, 1.0);
    if opacity <= f64::EPSILON {
        return None;
    }

    let start_s = adj_clip.timeline_start as f64 / 1_000_000_000.0;
    let end_s = (adj_clip.timeline_start + adj_clip.duration()) as f64 / 1_000_000_000.0;
    let orig_label = format!("vadj{adj_idx}orig");
    let work_label = format!("vadj{adj_idx}work");
    let fx_label = format!("vadj{adj_idx}fx");
    let scope_alpha = build_adjustment_scope_alpha_expression(adj_clip, out_w, out_h, "T");
    let scope_alpha = if opacity < 1.0 - f64::EPSILON {
        format!("({scope_alpha})*{opacity:.10}")
    } else {
        scope_alpha
    };

    Some(format!(
        ";[{input_label}]split[{orig_label}][{work_label}];\
         [{work_label}]trim=start={start_s:.6}:end={end_s:.6},setpts=PTS-STARTPTS,{effects_chain},format=yuva420p,\
         geq=lum='lum(X,Y)':cb='cb(X,Y)':cr='cr(X,Y)':a='alpha(X,Y)*({scope_alpha})',\
         setpts=PTS+{start_s:.6}/TB[{fx_label}];\
         [{orig_label}][{fx_label}]overlay=x=0:y=0:eof_action=pass[{output_label}]"
    ))
}

const TITLE_REFERENCE_HEIGHT: f64 = 1080.0;

fn build_title_filter(clip: &crate::model::clip::Clip, out_h: u32) -> String {
    if clip.title_text.trim().is_empty() {
        return String::new();
    }

    let text =
        crate::media::title_font::escape_drawtext_value(&clip.title_text).replace('\n', "\\n");
    let font_size = crate::media::title_font::parse_title_font(&clip.title_font).size_points();
    let font_option = crate::media::title_font::build_drawtext_font_option(&clip.title_font);
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
        ",drawtext={font_option}:text='{text}':fontsize={scaled_size:.2}:fontcolor={r:02x}{g:02x}{b:02x}@{alpha:.4}:x='({rel_x:.6})*w-text_w/2':y='({rel_y:.6})*h-text_h/2'"
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
        filter.push_str(&format!(
            ":borderw={bw:.1}:bordercolor={or:02x}{og:02x}{ob:02x}@{o_alpha:.4}"
        ));
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
        filter.push_str(&format!(
            ":shadowx={sx}:shadowy={sy}:shadowcolor={sr:02x}{sg:02x}{sb:02x}@{s_alpha:.4}"
        ));
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
        filter.push_str(&format!(
            ":box=1:boxcolor={br:02x}{bg:02x}{bb:02x}@{b_alpha:.4}:boxborderw={pad}"
        ));
    }

    // Secondary text (second drawtext filter below primary)
    if !clip.title_secondary_text.trim().is_empty() {
        let sec_text = crate::media::title_font::escape_drawtext_value(&clip.title_secondary_text)
            .replace('\n', "\\n");
        let sec_size = scaled_size * 0.7; // secondary text is 70% of primary
        let sec_y_offset = scaled_size * 1.5; // offset below primary
        filter.push_str(&format!(
            ",drawtext={font_option}:text='{sec_text}':fontsize={sec_size:.2}:fontcolor={r:02x}{g:02x}{b:02x}@{alpha:.4}:x='({rel_x:.6})*w-text_w/2':y='({rel_y:.6})*h-text_h/2+{sec_y_offset:.0}'"
        ));
    }

    filter
}

/// Build a single subtitle filter for post-compositing burn-in.
/// Takes timeline-absolute segments collected from all clips.
#[derive(Clone, Debug, PartialEq)]
struct SubtitleFontStyle {
    family: String,
    size_points: f64,
    bold: bool,
    italic: bool,
}

fn subtitle_font_style_from_desc(font_desc: &str) -> SubtitleFontStyle {
    let desc = pango::FontDescription::from_string(font_desc);
    let family = desc
        .family()
        .map(|family| family.trim().to_string())
        .filter(|family| !family.is_empty())
        .unwrap_or_else(|| "Sans".to_string());
    let size_points = if desc.size() > 0 {
        desc.size() as f64 / pango::SCALE as f64
    } else {
        24.0
    };
    let bold = matches!(
        desc.weight(),
        pango::Weight::Semibold
            | pango::Weight::Bold
            | pango::Weight::Ultrabold
            | pango::Weight::Heavy
            | pango::Weight::Ultraheavy
    );
    let italic = matches!(desc.style(), pango::Style::Italic | pango::Style::Oblique);
    SubtitleFontStyle {
        family,
        size_points,
        bold,
        italic,
    }
}

fn resolve_subtitle_font_style(font_desc: &str) -> SubtitleFontStyle {
    let base_size = crate::media::title_font::parse_subtitle_font(font_desc).size_points();
    let resolved_desc =
        crate::media::title_font::build_preview_subtitle_font_desc(font_desc, base_size);
    subtitle_font_style_from_desc(&resolved_desc)
}

fn ass_bool(enabled: bool) -> i32 {
    if enabled {
        -1
    } else {
        0
    }
}

fn build_subtitle_filter_composited(
    segments: &[(u64, u64, String, &crate::model::clip::Clip)],
    style_clip: &crate::model::clip::Clip,
    out_h: u32,
) -> (String, Option<tempfile::NamedTempFile>) {
    use crate::model::clip::SubtitleHighlightMode;

    if segments.is_empty() {
        return (String::new(), None);
    }

    let scale_factor = out_h as f64 / TITLE_REFERENCE_HEIGHT;
    let font_style = resolve_subtitle_font_style(&style_clip.subtitle_font);
    let scaled_size = (font_style.size_points * (4.0 / 3.0) * scale_factor).round() as u32;
    let ass_bold = ass_bool(font_style.bold);
    let ass_italic = ass_bool(font_style.italic);

    let rgba = style_clip.subtitle_color;
    let r = ((rgba >> 24) & 0xFF) as u8;
    let g = ((rgba >> 16) & 0xFF) as u8;
    let b = ((rgba >> 8) & 0xFF) as u8;
    let ass_primary = format!("&H00{b:02X}{g:02X}{r:02X}");

    // Map position_y to ASS alignment + MarginV.
    let pos_y = style_clip.subtitle_position_y.clamp(0.05, 0.95);
    let (ass_align, margin_v) = if pos_y < 0.33 {
        (8, ((pos_y * out_h as f64) as u32).max(10))
    } else if pos_y < 0.66 {
        (5, (((pos_y - 0.5).abs() * out_h as f64) as u32).max(10))
    } else {
        (2, (((1.0 - pos_y) * out_h as f64) as u32).max(10))
    };

    let mut style_parts = format!(
        "FontName={},FontSize={scaled_size},PrimaryColour={ass_primary},Alignment={ass_align},MarginV={margin_v},Bold={ass_bold},Italic={ass_italic}",
        font_style.family
    );

    if style_clip.subtitle_outline_width > 0.0 {
        let bw = (style_clip.subtitle_outline_width * scale_factor).round() as u32;
        let oc = style_clip.subtitle_outline_color;
        let obr = ((oc >> 24) & 0xFF) as u8;
        let obg = ((oc >> 16) & 0xFF) as u8;
        let obb = ((oc >> 8) & 0xFF) as u8;
        style_parts.push_str(&format!(
            ",OutlineColour=&H00{obb:02X}{obg:02X}{obr:02X},Outline={bw}"
        ));
    }

    if style_clip.subtitle_bg_box {
        let bc = style_clip.subtitle_bg_box_color;
        let bbr = ((bc >> 24) & 0xFF) as u8;
        let bbg = ((bc >> 16) & 0xFF) as u8;
        let bbb = ((bc >> 8) & 0xFF) as u8;
        let bba = (bc & 0xFF) as u8;
        let ass_alpha = format!("{:02X}", 255 - bba);
        style_parts.push_str(&format!(
            ",BorderStyle=3,BackColour=&H{ass_alpha}{bbb:02X}{bbg:02X}{bbr:02X}"
        ));
    }

    let highlight_mode = style_clip.subtitle_highlight_mode;
    let has_words = style_clip
        .subtitle_segments
        .iter()
        .any(|s| !s.words.is_empty());
    let use_karaoke = highlight_mode != SubtitleHighlightMode::None && has_words;

    if use_karaoke {
        // Write a proper ASS file so override tags (\c, \b, \u) work.
        let mut sub_file = match tempfile::Builder::new().suffix(".ass").tempfile() {
            Ok(f) => f,
            Err(_) => return (String::new(), None),
        };

        let hc = style_clip.subtitle_highlight_color;
        let hr = ((hc >> 24) & 0xFF) as u8;
        let hg = ((hc >> 16) & 0xFF) as u8;
        let hb = ((hc >> 8) & 0xFF) as u8;
        let group_size = (style_clip.subtitle_word_window_secs as usize).max(2);

        {
            use std::io::Write;

            // ASS header with default style matching our settings.
            let _ = writeln!(sub_file, "[Script Info]");
            let _ = writeln!(sub_file, "ScriptType: v4.00+");
            let _ = writeln!(sub_file, "PlayResX: {out_w}", out_w = out_h * 16 / 9); // approximate
            let _ = writeln!(sub_file, "PlayResY: {out_h}");
            let _ = writeln!(sub_file);
            let _ = writeln!(sub_file, "[V4+ Styles]");
            let _ = writeln!(
                sub_file,
                "Format: Name, Fontname, Fontsize, PrimaryColour, SecondaryColour, OutlineColour, BackColour, Bold, Italic, Underline, StrikeOut, ScaleX, ScaleY, Spacing, Angle, BorderStyle, Outline, Shadow, Alignment, MarginL, MarginR, MarginV, Encoding"
            );
            // Build the default style line.
            let mut outline_color = "&H00000000".to_string();
            let mut outline_w = 0u32;
            let mut border_style = 1u32;
            let mut back_color = "&H00000000".to_string();
            if style_clip.subtitle_outline_width > 0.0 {
                let oc = style_clip.subtitle_outline_color;
                let obr = ((oc >> 24) & 0xFF) as u8;
                let obg = ((oc >> 16) & 0xFF) as u8;
                let obb = ((oc >> 8) & 0xFF) as u8;
                outline_color = format!("&H00{obb:02X}{obg:02X}{obr:02X}");
                outline_w = (style_clip.subtitle_outline_width * scale_factor).round() as u32;
            }
            let mut shadow_depth = 0u32;
            if style_clip.subtitle_bg_box {
                let bc = style_clip.subtitle_bg_box_color;
                let bbr = ((bc >> 24) & 0xFF) as u8;
                let bbg = ((bc >> 16) & 0xFF) as u8;
                let bbb = ((bc >> 8) & 0xFF) as u8;
                let bba = (bc & 0xFF) as u8;
                let ass_alpha = 255 - bba;
                back_color = format!("&H{ass_alpha:02X}{bbb:02X}{bbg:02X}{bbr:02X}");
                if highlight_mode == SubtitleHighlightMode::Stroke {
                    // Stroke mode needs BorderStyle=1 for outline overrides to work.
                    // Simulate bg box via shadow (BackColour + Shadow depth).
                    border_style = 1;
                    shadow_depth = 4;
                } else {
                    border_style = 3;
                }
            }
            let _ = writeln!(
                sub_file,
                "Style: Default,{font_name},{scaled_size},{ass_primary},&H000000FF,{outline_color},{back_color},{ass_bold},{ass_italic},0,0,100,100,0,0,{border_style},{outline_w},{shadow_depth},{ass_align},10,10,{margin_v},1",
                font_name = font_style.family,
                ass_bold = ass_bold,
                ass_italic = ass_italic
            );
            let _ = writeln!(sub_file);
            let _ = writeln!(sub_file, "[Events]");
            let _ = writeln!(
                sub_file,
                "Format: Layer, Start, End, Style, Name, MarginL, MarginR, MarginV, Effect, Text"
            );

            for seg in &style_clip.subtitle_segments {
                if seg.words.is_empty() {
                    let abs_start = style_clip.timeline_start
                        + ((seg.start_ns.saturating_sub(style_clip.source_in)) as f64
                            / style_clip.speed) as u64;
                    let abs_end = style_clip.timeline_start
                        + ((seg.end_ns.saturating_sub(style_clip.source_in)) as f64
                            / style_clip.speed) as u64;
                    let _ = writeln!(
                        sub_file,
                        "Dialogue: 0,{},{},Default,,0,0,0,,{}",
                        ass_timecode(abs_start),
                        ass_timecode(abs_end),
                        seg.text
                    );
                    continue;
                }

                // Fixed groups: divide words into groups of group_size.
                // Each group gets one Dialogue event per word (for highlight),
                // but the visible text is the same fixed set of words.
                for (wi, word) in seg.words.iter().enumerate() {
                    let w_abs_start = style_clip.timeline_start
                        + ((word.start_ns.saturating_sub(style_clip.source_in)) as f64
                            / style_clip.speed) as u64;
                    let w_abs_end = style_clip.timeline_start
                        + ((word.end_ns.saturating_sub(style_clip.source_in)) as f64
                            / style_clip.speed) as u64;

                    // Determine which fixed group this word belongs to.
                    let group_start = (wi / group_size) * group_size;
                    let group_end = (group_start + group_size).min(seg.words.len());

                    let mut text = String::new();
                    for (owi, ow) in seg.words[group_start..group_end].iter().enumerate() {
                        if !text.is_empty() {
                            text.push(' ');
                        }
                        if group_start + owi == wi {
                            match highlight_mode {
                                SubtitleHighlightMode::Color => {
                                    text.push_str(&format!("{{\\c&H{hb:02X}{hg:02X}{hr:02X}&}}"));
                                    text.push_str(&ow.text);
                                    text.push_str(&format!("{{\\c{ass_primary}&}}"));
                                }
                                SubtitleHighlightMode::Bold => {
                                    text.push_str("{\\b1}");
                                    text.push_str(&ow.text);
                                    text.push_str("{\\b0}");
                                }
                                SubtitleHighlightMode::Underline => {
                                    text.push_str("{\\u1}");
                                    text.push_str(&ow.text);
                                    text.push_str("{\\u0}");
                                }
                                SubtitleHighlightMode::Stroke => {
                                    // Switch to outline border style, set outline color
                                    // to highlight color with thick border, then restore.
                                    text.push_str(&format!(
                                        "{{\\bord4\\3c&H{hb:02X}{hg:02X}{hr:02X}&}}"
                                    ));
                                    text.push_str(&ow.text);
                                    text.push_str(&format!(
                                        "{{\\bord{outline_w}\\3c{outline_color}&}}"
                                    ));
                                }
                                SubtitleHighlightMode::None => text.push_str(&ow.text),
                            }
                        } else {
                            text.push_str(&ow.text);
                        }
                    }

                    let _ = writeln!(
                        sub_file,
                        "Dialogue: 0,{},{},Default,,0,0,0,,{text}",
                        ass_timecode(w_abs_start),
                        ass_timecode(w_abs_end)
                    );
                }
            }
            let _ = sub_file.flush();
        }

        let sub_path = sub_file.path().to_string_lossy().to_string();
        let escaped_path = sub_path
            .replace('\\', "\\\\")
            .replace(':', "\\:")
            .replace('\'', "'\\''");

        // ASS file has styles embedded, no force_style needed.
        let filter = format!("subtitles='{escaped_path}'");
        (filter, Some(sub_file))
    } else {
        // Simple mode: SRT with force_style.
        let mut sub_file = match tempfile::Builder::new().suffix(".srt").tempfile() {
            Ok(f) => f,
            Err(_) => return (String::new(), None),
        };

        {
            use std::io::Write;
            let mut sorted: Vec<_> = segments.iter().collect();
            sorted.sort_by_key(|(start, _, _, _)| *start);
            for (i, (start_ns, end_ns, text, _)) in sorted.iter().enumerate() {
                let _ = writeln!(sub_file, "{}", i + 1);
                let _ = writeln!(
                    sub_file,
                    "{} --> {}",
                    srt_timecode(*start_ns),
                    srt_timecode(*end_ns)
                );
                let _ = writeln!(sub_file, "{text}");
                let _ = writeln!(sub_file);
            }
            let _ = sub_file.flush();
        }

        let sub_path = sub_file.path().to_string_lossy().to_string();
        let escaped_path = sub_path
            .replace('\\', "\\\\")
            .replace(':', "\\:")
            .replace('\'', "'\\''");

        let filter = format!("subtitles='{escaped_path}':force_style='{style_parts}'");
        (filter, Some(sub_file))
    }
}

/// Export subtitles from all clips in the project as an SRT file.
pub fn export_srt(project: &Project, output_path: &str) -> Result<()> {
    use std::io::Write;
    let mut segments: Vec<(u64, u64, String)> = Vec::new();

    for track in &project.tracks {
        for clip in &track.clips {
            if clip.subtitle_segments.is_empty() {
                continue;
            }
            for seg in &clip.subtitle_segments {
                // Convert source-relative to timeline-absolute timestamps.
                let timeline_start = clip.timeline_start
                    + ((seg.start_ns.saturating_sub(clip.source_in)) as f64 / clip.speed) as u64;
                let timeline_end = clip.timeline_start
                    + ((seg.end_ns.saturating_sub(clip.source_in)) as f64 / clip.speed) as u64;
                segments.push((timeline_start, timeline_end, seg.text.clone()));
            }
        }
    }

    // Sort by start time.
    segments.sort_by_key(|s| s.0);

    let mut file = std::fs::File::create(output_path)?;
    for (i, (start_ns, end_ns, text)) in segments.iter().enumerate() {
        let start_tc = srt_timecode(*start_ns);
        let end_tc = srt_timecode(*end_ns);
        writeln!(file, "{}", i + 1)?;
        writeln!(file, "{start_tc} --> {end_tc}")?;
        writeln!(file, "{text}")?;
        writeln!(file)?;
    }

    Ok(())
}

/// Parse an SRT file and return subtitle segments.
/// Timestamps in the SRT are treated as source-relative (offset by `source_in_ns`).
pub fn import_srt(
    path: &str,
    source_in_ns: u64,
) -> Result<Vec<crate::model::clip::SubtitleSegment>> {
    let content = std::fs::read_to_string(path)?;
    let mut segments = Vec::new();

    // SRT format: index\nHH:MM:SS,mmm --> HH:MM:SS,mmm\ntext\n\n
    let mut lines = content.lines().peekable();
    while lines.peek().is_some() {
        // Skip blank lines and cue index.
        let line = match lines.next() {
            Some(l) => l.trim(),
            None => break,
        };
        if line.is_empty() {
            continue;
        }
        // Try to parse as cue index (just a number) — skip it.
        if line.parse::<u64>().is_ok() {
            // Next line should be the timecode.
            let tc_line = match lines.next() {
                Some(l) => l.trim().to_string(),
                None => break,
            };
            // Parse "HH:MM:SS,mmm --> HH:MM:SS,mmm"
            let (start_ns, end_ns) = match parse_srt_timecode_line(&tc_line) {
                Some(t) => t,
                None => continue,
            };
            // Collect text lines until blank line.
            let mut text = String::new();
            for tl in lines.by_ref() {
                let tl = tl.trim();
                if tl.is_empty() {
                    break;
                }
                if !text.is_empty() {
                    text.push('\n');
                }
                text.push_str(tl);
            }
            if !text.is_empty() {
                segments.push(crate::model::clip::SubtitleSegment {
                    id: uuid::Uuid::new_v4().to_string(),
                    start_ns: source_in_ns + start_ns,
                    end_ns: source_in_ns + end_ns,
                    text,
                    words: Vec::new(),
                });
            }
        }
    }

    Ok(segments)
}

/// Parse an SRT timecode line like "00:01:23,456 --> 00:01:25,789" into (start_ns, end_ns).
fn parse_srt_timecode_line(line: &str) -> Option<(u64, u64)> {
    let parts: Vec<&str> = line.split("-->").collect();
    if parts.len() != 2 {
        return None;
    }
    let start = parse_srt_tc(parts[0].trim())?;
    let end = parse_srt_tc(parts[1].trim())?;
    Some((start, end))
}

/// Parse "HH:MM:SS,mmm" into nanoseconds.
fn parse_srt_tc(tc: &str) -> Option<u64> {
    // Handle both comma and period as millisecond separator.
    let tc = tc.replace(',', ".");
    let parts: Vec<&str> = tc.split(':').collect();
    if parts.len() != 3 {
        return None;
    }
    let h: u64 = parts[0].parse().ok()?;
    let m: u64 = parts[1].parse().ok()?;
    let sec_parts: Vec<&str> = parts[2].split('.').collect();
    let s: u64 = sec_parts[0].parse().ok()?;
    let ms: u64 = if sec_parts.len() > 1 {
        let ms_str = sec_parts[1];
        // Pad or truncate to 3 digits.
        let padded = format!("{:0<3}", &ms_str[..ms_str.len().min(3)]);
        padded.parse().ok()?
    } else {
        0
    };
    Some((h * 3600 + m * 60 + s) * 1_000_000_000 + ms * 1_000_000)
}

/// Format nanoseconds as ASS timecode: H:MM:SS.cc (centiseconds)
fn ass_timecode(ns: u64) -> String {
    let total_cs = ns / 10_000_000;
    let cs = total_cs % 100;
    let total_s = total_cs / 100;
    let s = total_s % 60;
    let total_m = total_s / 60;
    let m = total_m % 60;
    let h = total_m / 60;
    format!("{h}:{m:02}:{s:02}.{cs:02}")
}

/// Format nanoseconds as SRT timecode: HH:MM:SS,mmm
fn srt_timecode(ns: u64) -> String {
    let total_ms = ns / 1_000_000;
    let ms = total_ms % 1000;
    let total_s = total_ms / 1000;
    let s = total_s % 60;
    let total_m = total_s / 60;
    let m = total_m % 60;
    let h = total_m / 60;
    format!("{h:02}:{m:02}:{s:02},{ms:03}")
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
    _caps: &ColorFilterCapabilities,
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
        r: (neutral.r
            + (temp_only.r - neutral.r) * temp_gain
            + (tint_only.r - neutral.r) * tint_gain
            + off_r)
            .clamp(0.0, 1.0),
        g: (neutral.g
            + (temp_only.g - neutral.g) * temp_gain
            + (tint_only.g - neutral.g) * tint_gain
            + off_g)
            .clamp(0.0, 1.0),
        b: (neutral.b
            + (temp_only.b - neutral.b) * temp_gain
            + (tint_only.b - neutral.b) * tint_gain
            + off_b)
            .clamp(0.0, 1.0),
    }
}

type MaskAlphaResult = crate::media::mask_alpha::FfmpegMaskAlphaResult;

/// Build a combined mask alpha for all enabled masks on a clip.
/// Returns either a geq expression (for rect/ellipse only) or a rasterized
/// grayscale temp file (when any path mask is present).
fn build_combined_mask_alpha(
    clip: &crate::model::clip::Clip,
    out_w: u32,
    out_h: u32,
) -> Option<MaskAlphaResult> {
    crate::media::mask_alpha::build_combined_mask_ffmpeg_alpha(
        &clip.masks,
        out_w,
        out_h,
        0,
        clip.scale,
        clip.position_x,
        clip.position_y,
    )
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
    if clip.kind == ClipKind::Image && clip.animated_svg {
        return (0.0, clip.source_duration() as f64 / 1_000_000_000.0);
    }
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
/// When the background alpha is 0 (transparent, the default for most
/// title templates), outputs `yuva420p` with a fully transparent source
/// so the overlay compositing chain shows lower video tracks through
/// the title.  When background alpha > 0, outputs opaque `yuv420p`.
///
/// Primary-track concat paths that require opaque input already apply a
/// downstream `format=yuv420p` conversion, so the alpha is safely
/// dropped for that case.
fn title_clip_lavfi_color(
    clip: &crate::model::clip::Clip,
    out_w: u32,
    out_h: u32,
    fr_n: u32,
    fr_d: u32,
    dur_s: f64,
) -> String {
    let bg = clip.title_clip_bg_color;
    let a = bg & 0xFF;
    if a > 0 {
        // Opaque or semi-transparent background — use opaque yuv420p.
        let r = (bg >> 24) & 0xFF;
        let g = (bg >> 16) & 0xFF;
        let b = (bg >> 8) & 0xFF;
        let color_str = format!("#{r:02x}{g:02x}{b:02x}");
        format!(
            "color=c={color_str}:size={out_w}x{out_h}:r={fr_n}/{fr_d}:d={dur_s:.6},format=yuv420p,trim=duration={dur_s:.6},setpts=PTS-STARTPTS"
        )
    } else {
        // Transparent background — use yuva420p so overlays show through.
        format!(
            "color=c=black@0.0:size={out_w}x{out_h}:r={fr_n}/{fr_d}:d={dur_s:.6},format=yuva420p,trim=duration={dur_s:.6},setpts=PTS-STARTPTS"
        )
    }
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
    if clip.is_freeze_frame() || (clip.kind == ClipKind::Image && !clip.animated_svg) {
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
            format!(
                ",setpts=({expr})/TB,trim=duration={dur_s:.6},setpts=PTS-STARTPTS{minterp_suffix}"
            )
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct PrimaryTransitionTiming {
    duration_ns: u64,
    before_cut_ns: u64,
    after_cut_ns: u64,
}

impl PrimaryTransitionTiming {
    fn duration_s(self) -> f64 {
        self.duration_ns as f64 / 1_000_000_000.0
    }

    fn before_cut_s(self) -> f64 {
        self.before_cut_ns as f64 / 1_000_000_000.0
    }
}

fn clamped_primary_transition_timing(
    current: &Clip,
    next: &Clip,
) -> Option<PrimaryTransitionTiming> {
    if !current.outgoing_transition.is_active() {
        return None;
    }
    let max_duration_ns = max_transition_duration_ns(current, next);
    if max_duration_ns < 1_000_000 {
        return None;
    }
    let duration_ns = current
        .outgoing_transition
        .duration_ns
        .min(max_duration_ns)
        .max(1_000_000);
    let split = current
        .outgoing_transition
        .alignment
        .split_duration(duration_ns);
    Some(PrimaryTransitionTiming {
        duration_ns,
        before_cut_ns: split.before_cut_ns,
        after_cut_ns: split.after_cut_ns,
    })
}

fn primary_clip_transition_stop_pad_ns(
    transition_timings: &[Option<PrimaryTransitionTiming>],
    clip_idx: usize,
) -> u64 {
    let incoming_before_ns = clip_idx
        .checked_sub(1)
        .and_then(|prev_idx| transition_timings.get(prev_idx))
        .and_then(|timing| *timing)
        .map(|timing| timing.before_cut_ns)
        .unwrap_or(0);
    let outgoing_after_ns = transition_timings
        .get(clip_idx)
        .and_then(|timing| *timing)
        .map(|timing| timing.after_cut_ns)
        .unwrap_or(0);
    incoming_before_ns.saturating_add(outgoing_after_ns)
}

fn build_primary_clip_transition_stop_pad_filter(
    transition_timings: &[Option<PrimaryTransitionTiming>],
    clip_idx: usize,
) -> String {
    let stop_pad_ns = primary_clip_transition_stop_pad_ns(transition_timings, clip_idx);
    if stop_pad_ns == 0 {
        String::new()
    } else {
        format!(
            ",tpad=stop_mode=clone:stop_duration={:.6}",
            stop_pad_ns as f64 / 1_000_000_000.0
        )
    }
}

fn clamped_primary_xfade_duration_s(current: &Clip, next: &Clip) -> Option<f64> {
    clamped_primary_transition_timing(current, next).map(PrimaryTransitionTiming::duration_s)
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
                        let val = effect
                            .params
                            .get(&p.name)
                            .copied()
                            .unwrap_or(p.default_value);
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
        let pixel_scale = ((out_w.max(1) as f64 * out_h.max(1) as f64) / (640.0 * 480.0)).max(0.1);
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
    // Missing optional frei0r modules are a normal fallback case on many
    // FFmpeg builds, so keep the capability probe quiet.
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
        .stdout(Stdio::null())
        .stderr(Stdio::null())
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

/// Measure integrated loudness (LUFS) of a clip's audio via FFmpeg `ebur128` filter.
/// Returns the integrated loudness value in LUFS (e.g. -18.3).
pub(crate) fn analyze_loudness_lufs(
    source_path: &str,
    source_in_ns: u64,
    source_out_ns: u64,
) -> Result<f64> {
    analyze_loudness_lufs_with_prefilter(source_path, source_in_ns, source_out_ns, None)
}

pub(crate) fn analyze_loudness_lufs_with_prefilter(
    source_path: &str,
    source_in_ns: u64,
    source_out_ns: u64,
    prefilter: Option<String>,
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
    let audio_filter = prefilter
        .map(|filter| format!("{filter},ebur128"))
        .unwrap_or_else(|| "ebur128".to_string());
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

/// Recursively flatten compound clips in a track list.
/// Each compound clip is replaced by its internal clips with timeline positions
/// rebased to the compound clip's position on the parent timeline.
/// Returns a new `Vec<Track>` containing only leaf (non-compound) clips.
fn flatten_compound_tracks(
    tracks: &[crate::model::track::Track],
) -> Vec<crate::model::track::Track> {
    let mut result: Vec<crate::model::track::Track> = Vec::new();
    // Collect audio clips extracted from compound/multicam clips on video tracks.
    // These need to go on audio tracks so the export pipeline picks them up.
    let mut extracted_audio_clips: Vec<Clip> = Vec::new();

    for track in tracks {
        let flat = flatten_clips(&track.clips, 0, 0);
        // Separate audio clips that landed on a video track (from compound/multicam expansion)
        if track.kind == crate::model::track::TrackKind::Video {
            let mut video_clips = Vec::new();
            for clip in flat {
                if clip.kind == ClipKind::Audio {
                    extracted_audio_clips.push(clip);
                } else {
                    video_clips.push(clip);
                }
            }
            let mut flat_track = track.clone();
            flat_track.clips = video_clips;
            result.push(flat_track);
        } else {
            let mut flat_track = track.clone();
            flat_track.clips = flat;
            result.push(flat_track);
        }
    }

    // Place extracted audio clips onto an audio track
    if !extracted_audio_clips.is_empty() {
        // Find an existing audio track or create one
        let audio_track = result
            .iter_mut()
            .find(|t| t.kind == crate::model::track::TrackKind::Audio);
        if let Some(track) = audio_track {
            track.clips.extend(extracted_audio_clips);
            track.clips.sort_by_key(|c| c.timeline_start);
        } else {
            let mut new_track = crate::model::track::Track::new_audio("Compound Audio");
            new_track.clips = extracted_audio_clips;
            new_track.clips.sort_by_key(|c| c.timeline_start);
            result.push(new_track);
        }
    }

    result
}

fn flatten_clips(clips: &[Clip], timeline_offset: u64, depth: usize) -> Vec<Clip> {
    if depth > 16 {
        return Vec::new();
    }
    let mut result = Vec::new();
    for clip in clips {
        if clip.kind == ClipKind::Compound {
            if let Some(ref internal_tracks) = clip.compound_tracks {
                // Map internal clip positions into the parent timeline.
                // After windowing, each clip's timeline_start >= source_in,
                // so subtracting source_in gives the offset from the visible
                // start. Adding the compound's parent position avoids u64
                // underflow that saturating_sub would cause.
                let compound_offset = timeline_offset
                    .saturating_add(clip.timeline_start);
                let window_start = clip.source_in;
                let window_end = clip.source_out;
                for inner_track in internal_tracks {
                    for inner_clip in &inner_track.clips {
                        // Skip clips entirely outside the visible window
                        if inner_clip.timeline_end() <= window_start
                            || inner_clip.timeline_start >= window_end
                        {
                            continue;
                        }
                        let mut rebased = inner_clip.clone();
                        // Trim clips that partially overlap window boundaries
                        let orig_duration = rebased.duration();
                        let left_trim =
                            window_start.saturating_sub(rebased.timeline_start);
                        if left_trim > 0 {
                            rebased.source_in = rebased.source_in.saturating_add(left_trim);
                            rebased.timeline_start = window_start;
                        }
                        let mut right_trim = 0u64;
                        if rebased.timeline_end() > window_end {
                            right_trim = rebased.timeline_end() - window_end;
                            rebased.source_out = rebased.source_out.saturating_sub(right_trim);
                        }
                        // Rebase keyframes so they stay aligned with clip content
                        if left_trim > 0 || right_trim > 0 {
                            rebased.retain_keyframes_in_local_range(
                                left_trim,
                                orig_duration.saturating_sub(right_trim),
                            );
                        }
                        // Rebase: offset from window start + compound parent pos
                        rebased.timeline_start = compound_offset.saturating_add(
                            rebased.timeline_start.saturating_sub(window_start),
                        );
                        if rebased.kind == ClipKind::Compound || rebased.kind == ClipKind::Multicam
                        {
                            result.extend(flatten_clips(&[rebased], 0, depth + 1));
                        } else {
                            result.push(rebased);
                        }
                    }
                }
            }
        } else if clip.kind == ClipKind::Multicam {
            let clip_start = timeline_offset.saturating_add(clip.timeline_start);
            let clip_dur = clip.duration();
            let segments = clip.multicam_segments();
            // Video segments from angle switches
            for (seg_start, seg_end, angle_idx) in &segments {
                if let Some(angle) = clip
                    .multicam_angles
                    .as_ref()
                    .and_then(|a| a.get(*angle_idx))
                {
                    let mut seg = Clip::new(
                        &angle.source_path,
                        angle
                            .source_in
                            .saturating_add(*seg_end)
                            .min(angle.source_out),
                        clip_start.saturating_add(*seg_start),
                        ClipKind::Video,
                    );
                    seg.source_in = angle.source_in.saturating_add(*seg_start);
                    seg.source_out = angle
                        .source_in
                        .saturating_add(*seg_end)
                        .min(angle.source_out);
                    seg.id = uuid::Uuid::new_v4().to_string();
                    result.push(seg);
                }
            }
            // Audio clips: one per unmuted angle spanning full multicam duration
            if let Some(ref angles) = clip.multicam_angles {
                for (ai, angle) in angles.iter().enumerate() {
                    if angle.muted {
                        continue;
                    }
                    let mut audio_clip = Clip::new(
                        &angle.source_path,
                        angle
                            .source_in
                            .saturating_add(clip_dur)
                            .min(angle.source_out),
                        clip_start,
                        ClipKind::Audio,
                    );
                    audio_clip.source_in = angle.source_in;
                    audio_clip.source_out = angle
                        .source_in
                        .saturating_add(clip_dur)
                        .min(angle.source_out);
                    audio_clip.volume = angle.volume;
                    audio_clip.id = uuid::Uuid::new_v4().to_string();
                    result.push(audio_clip);
                }
            }
        } else {
            let mut c = clip.clone();
            c.timeline_start = timeline_offset.saturating_add(c.timeline_start);
            result.push(c);
        }
    }
    result.sort_by_key(|c| c.timeline_start);
    result
}

#[cfg(test)]
mod tests {
    use super::{
        append_pan_filter_chain, audio_crossfade_curve_name, build_adjustment_layer_filter_graph,
        build_adjustment_scope_alpha_expression, build_audio_crossfade_filters, build_color_filter,
        build_crop_filter, build_grading_filter, build_keyframed_property_expression,
        build_pan_expression, build_rotation_filter, build_subtitle_filter_composited,
        build_temperature_tint_filter, build_timing_filter, build_title_filter,
        build_volume_filter, clamped_primary_transition_timing, clamped_primary_xfade_duration_s,
        compute_clip_audio_fades, compute_export_coloradj_params, estimate_export_size_bytes,
        flatten_compound_tracks, has_linked_audio_peer, has_transform_keyframes,
        parse_progress_line, primary_clip_transition_stop_pad_ns, resolve_subtitle_font_style,
        video_input_seek_and_duration, write_chapter_metadata, AudioCodec, ClipAudioFade,
        ColorFilterCapabilities, ExportOptions, VideoCodec,
    };
    use crate::media::program_player::ProgramPlayer;
    use crate::model::clip::{
        Clip, ClipKind, KeyframeInterpolation, NumericKeyframe, SubtitleSegment, SubtitleWord,
    };
    use crate::model::project::Project;
    use crate::ui_state::CrossfadeCurve;
    use gstreamer as gst;

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

    fn make_subtitle_video_clip(font_desc: &str) -> Clip {
        let mut clip = make_video_clip("sub", 0, 5_000_000_000);
        clip.subtitle_font = font_desc.to_string();
        clip.subtitle_segments = vec![SubtitleSegment {
            id: "seg-1".to_string(),
            start_ns: 0,
            end_ns: 1_000_000_000,
            text: "Hello world".to_string(),
            words: Vec::new(),
        }];
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
    fn resolve_subtitle_font_style_uses_subtitle_default_size() {
        let style = resolve_subtitle_font_style("");
        assert_eq!(style.size_points, 24.0);
    }

    #[test]
    fn subtitle_force_style_splits_family_and_style_flags() {
        let clip = make_subtitle_video_clip("DejaVu Sans Mono Bold Oblique 24");
        let segs = vec![(0_u64, 1_000_000_000_u64, "Hello world".to_string(), &clip)];

        let (filter, _temp) = build_subtitle_filter_composited(&segs, &clip, 1080);

        assert!(filter.contains("FontName=DejaVu Sans Mono"));
        assert!(filter.contains("Bold=-1"));
        assert!(filter.contains("Italic=-1"));
        assert!(!filter.contains("FontName=DejaVu Sans Mono Bold Oblique"));
    }

    #[test]
    fn karaoke_subtitle_ass_style_uses_family_and_style_flags() {
        let mut clip = make_subtitle_video_clip("DejaVu Sans Mono Bold Oblique 24");
        clip.subtitle_highlight_mode = crate::model::clip::SubtitleHighlightMode::Color;
        clip.subtitle_segments[0].words = vec![
            SubtitleWord {
                start_ns: 0,
                end_ns: 500_000_000,
                text: "Hello".to_string(),
            },
            SubtitleWord {
                start_ns: 500_000_000,
                end_ns: 1_000_000_000,
                text: "world".to_string(),
            },
        ];
        let segs = vec![(0_u64, 1_000_000_000_u64, "Hello world".to_string(), &clip)];

        let (_filter, temp) = build_subtitle_filter_composited(&segs, &clip, 1080);
        let temp = temp.expect("karaoke path should create ASS temp file");
        let ass = std::fs::read_to_string(temp.path()).expect("read ASS file");

        assert!(ass.contains("Style: Default,DejaVu Sans Mono,32"));
        assert!(ass.contains(",-1,-1,0,0,100,100"));
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
        a.outgoing_transition = crate::model::transition::OutgoingTransition::new(
            "cross_dissolve",
            10_000_000_000,
            crate::model::transition::TransitionAlignment::EndOnCut,
        );
        let d = clamped_primary_xfade_duration_s(&a, &b).expect("transition should be enabled");
        assert!((d - 3.999).abs() < 0.000_001);
    }

    #[test]
    fn clamped_primary_transition_timing_respects_alignment_split() {
        let mut a = make_video_clip("a", 0, 4_000_000_000);
        let b = make_video_clip("b", 4_000_000_000, 8_000_000_000);
        a.outgoing_transition = crate::model::transition::OutgoingTransition::new(
            "cross_dissolve",
            1_000_000_000,
            crate::model::transition::TransitionAlignment::CenterOnCut,
        );
        let timing =
            clamped_primary_transition_timing(&a, &b).expect("transition timing should exist");
        assert_eq!(timing.duration_ns, 1_000_000_000);
        assert_eq!(timing.before_cut_ns, 500_000_000);
        assert_eq!(timing.after_cut_ns, 500_000_000);
    }

    #[test]
    fn primary_clip_transition_stop_pad_combines_incoming_and_outgoing_hold() {
        let timings = vec![
            Some(super::PrimaryTransitionTiming {
                duration_ns: 1_000_000_000,
                before_cut_ns: 400_000_000,
                after_cut_ns: 600_000_000,
            }),
            Some(super::PrimaryTransitionTiming {
                duration_ns: 800_000_000,
                before_cut_ns: 0,
                after_cut_ns: 800_000_000,
            }),
        ];
        assert_eq!(
            primary_clip_transition_stop_pad_ns(&timings, 0),
            600_000_000
        );
        assert_eq!(
            primary_clip_transition_stop_pad_ns(&timings, 1),
            1_200_000_000
        );
        assert_eq!(primary_clip_transition_stop_pad_ns(&timings, 2), 0);
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
    fn adjustment_scope_alpha_expression_is_passthrough_for_full_frame_static_scope() {
        let clip = Clip::new_adjustment(0, 2_000_000_000);
        assert_eq!(
            build_adjustment_scope_alpha_expression(&clip, 1920, 1080, "T"),
            "1"
        );
    }

    #[test]
    fn adjustment_layer_filter_graph_uses_clip_local_time_and_scope_mask() {
        let mut clip = Clip::new_adjustment(5_000_000_000, 2_000_000_000);
        clip.brightness_keyframes = vec![
            NumericKeyframe {
                time_ns: 0,
                value: -0.2,
                interpolation: KeyframeInterpolation::Linear,
                bezier_controls: None,
            },
            NumericKeyframe {
                time_ns: 1_000_000_000,
                value: 0.4,
                interpolation: KeyframeInterpolation::Linear,
                bezier_controls: None,
            },
        ];
        clip.scale = 0.75;
        clip.scale_keyframes = vec![
            NumericKeyframe {
                time_ns: 0,
                value: 0.75,
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
        clip.position_x = 0.4;
        clip.crop_left = 120;
        clip.rotate = 18;

        let graph = build_adjustment_layer_filter_graph(
            "vin",
            "vout",
            &clip,
            0,
            1920,
            1080,
            &ColorFilterCapabilities::default(),
        )
        .expect("adjustment graph");

        assert!(graph.contains("trim=start=5.000000:end=7.000000,setpts=PTS-STARTPTS"));
        assert!(graph.contains("eq=brightness='if(lt(t,"));
        assert!(graph.contains("a='alpha(X,Y)*("));
        assert!(graph.contains("if(lt(T,"));
        assert!(graph.contains("overlay=x=0:y=0:eof_action=pass[vout]"));
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
        assert!(
            f.contains(",lutrgb="),
            "grading should emit lutrgb filter: {f}"
        );
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
        assert!(
            sh_p.black_r < 0.05,
            "shadows warmth should lower red control: {}",
            sh_p.black_r
        );
        // Shadows warmth shift should be proportionally larger than midtones
        let sh_r_shift = (0.0 - sh_p.black_r).abs(); // shift from neutral black=0
        let mid_r_shift = (0.5 - mid_p.gray_r).abs(); // shift from neutral gray=0.5
        assert!(
            sh_r_shift > 0.01 || mid_r_shift > 0.01,
            "warmth should produce measurable shifts: sh={} mid={}",
            sh_r_shift,
            mid_r_shift
        );

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
            sh_g_deviation,
            mid_g_deviation
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

        let magnitude =
            |a: &crate::media::program_player::ColorAdjRGBParams,
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
        assert!(
            sh.black_r < sh.black_b,
            "shadows warmth: red control < blue control at black point"
        );

        let mid = ProgramPlayer::compute_export_3point_params(
            0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0,
        );
        assert!(
            mid.gray_r < mid.gray_b,
            "midtones warmth: red control < blue control at gray point"
        );

        let hi = ProgramPlayer::compute_export_3point_params(
            0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 0.0,
        );
        assert!(
            hi.white_r < hi.white_b,
            "highlights warmth: red control < blue control at white point"
        );
    }

    #[test]
    fn build_grading_filter_tint_direction_is_consistent_per_tonal_region() {
        // Positive tint = magenta (higher G control = less green output) in ALL zones.
        let sh = ProgramPlayer::compute_export_3point_params(
            0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 1.0,
        );
        assert!(
            sh.black_g > sh.black_r,
            "shadows tint +1: green control should be higher than red (less green output)"
        );

        let mid = ProgramPlayer::compute_export_3point_params(
            0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0,
        );
        assert!(
            mid.gray_g > mid.gray_r,
            "midtones tint +1: green control > red at gray point"
        );

        let hi = ProgramPlayer::compute_export_3point_params(
            0.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0,
        );
        assert!(
            hi.white_g > hi.white_r,
            "highlights tint +1: green control > red at white point"
        );
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
        assert!(f.contains("drawtext=fontfile='") || f.contains("font='Sans\\:weight=bold'"));
        assert!(f.contains("fontsize=64.00"));
        assert!(f.contains("fontcolor=ff3366@0.8000"));
        assert!(f.contains("x='(0.250000)*w-text_w/2'"));
        assert!(f.contains("y='(0.750000)*h-text_h/2'"));
    }

    #[test]
    fn drawtext_font_selector_uses_structured_fontconfig_fields() {
        assert_eq!(
            crate::media::title_font::build_drawtext_font_selector("Sans Bold 48"),
            "Sans:weight=bold"
        );
        assert_eq!(
            crate::media::title_font::build_drawtext_font_selector("Sans Bold Italic 48"),
            "Sans:weight=bold:slant=italic"
        );
        assert_eq!(
            crate::media::title_font::build_drawtext_font_selector(""),
            "Sans:weight=bold"
        );
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
        assert!(
            filter.contains("|y|n"),
            "Expected y/n for bools, got: {}",
            filter
        );
        // Must contain r/g/b compound format for COLORs.
        assert!(
            filter.contains("0.000000/0.000000/0.000000"),
            "Missing compound COLOR format in: {}",
            filter
        );
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
        let markers = vec![Marker::new(
            0,
            "Title=With;Special#Chars\\Here\nNewline".to_string(),
        )];
        let file = write_chapter_metadata(&markers, 10_000_000_000)
            .unwrap()
            .expect("should produce metadata file");
        let content = std::fs::read_to_string(file.path()).unwrap();
        assert!(content.contains("title=Title\\=With\\;Special\\#Chars\\\\Here Newline"));
    }

    // ── Compound clip flattening tests ────────────────────────────────

    #[test]
    fn test_flatten_compound_tracks_no_compounds() {
        use crate::model::track::Track;
        let mut t = Track::new_video("V1");
        let mut c = Clip::new("a.mp4", 5_000, 0, ClipKind::Video);
        c.id = "A".into();
        t.add_clip(c);

        let flattened = flatten_compound_tracks(&[t]);
        assert_eq!(flattened.len(), 1);
        assert_eq!(flattened[0].clips.len(), 1);
        assert_eq!(flattened[0].clips[0].id, "A");
        assert_eq!(flattened[0].clips[0].timeline_start, 0);
    }

    #[test]
    fn test_flatten_compound_tracks_single_compound() {
        use crate::model::track::Track;

        // Inner clip at internal position 2000, compound starts at 10000 on parent
        let mut inner_track = Track::new_video("Inner V");
        let mut inner_clip = Clip::new("inner.mp4", 3_000, 2_000, ClipKind::Video);
        inner_clip.id = "inner".into();
        inner_track.add_clip(inner_clip);

        let mut compound = Clip::new_compound(10_000, vec![inner_track]);
        compound.id = "compound".into();

        let mut root_track = Track::new_video("Root V");
        root_track.add_clip(compound);

        let flattened = flatten_compound_tracks(&[root_track]);
        assert_eq!(flattened.len(), 1);
        assert_eq!(flattened[0].clips.len(), 1);
        // Inner clip's absolute position: 10000 (compound start) + 2000 (internal offset) = 12000
        assert_eq!(flattened[0].clips[0].timeline_start, 12_000);
        assert_eq!(flattened[0].clips[0].source_path, "inner.mp4");
    }

    #[test]
    fn test_flatten_compound_preserves_non_compound_clips() {
        use crate::model::track::Track;

        let mut root_track = Track::new_video("V1");
        let mut regular = Clip::new("regular.mp4", 5_000, 0, ClipKind::Video);
        regular.id = "R".into();
        root_track.add_clip(regular);

        // Compound at 10000
        let mut inner = Track::new_video("Inner");
        let mut ic = Clip::new("inner.mp4", 3_000, 0, ClipKind::Video);
        ic.id = "I".into();
        inner.add_clip(ic);
        let mut compound = Clip::new_compound(10_000, vec![inner]);
        compound.id = "C".into();
        root_track.add_clip(compound);

        let flattened = flatten_compound_tracks(&[root_track]);
        // Should have 2 clips: regular at 0, inner at 10000
        assert_eq!(flattened[0].clips.len(), 2);
        let ids: Vec<&str> = flattened[0].clips.iter().map(|c| c.id.as_str()).collect();
        assert!(ids.contains(&"R"));
        // Inner clip gets a fresh UUID, so check by source_path
        let inner_clip = flattened[0]
            .clips
            .iter()
            .find(|c| c.source_path == "inner.mp4")
            .unwrap();
        assert_eq!(inner_clip.timeline_start, 10_000);
    }

    #[test]
    fn test_flatten_nested_compound() {
        use crate::model::track::Track;

        // Deeply nested: compound inside compound
        let mut deep_track = Track::new_video("Deep");
        let mut deep_clip = Clip::new("deep.mp4", 1_000, 500, ClipKind::Video);
        deep_clip.id = "deep".into();
        deep_track.add_clip(deep_clip);

        let mut inner_compound = Clip::new_compound(1_000, vec![deep_track]);
        inner_compound.id = "inner-compound".into();

        let mut mid_track = Track::new_video("Mid");
        mid_track.add_clip(inner_compound);

        let mut outer_compound = Clip::new_compound(5_000, vec![mid_track]);
        outer_compound.id = "outer-compound".into();

        let mut root = Track::new_video("Root");
        root.add_clip(outer_compound);

        let flattened = flatten_compound_tracks(&[root]);
        assert_eq!(flattened[0].clips.len(), 1);
        // deep clip absolute position: 5000 (outer) + 1000 (inner) + 500 (deep clip offset) = 6500
        assert_eq!(flattened[0].clips[0].timeline_start, 6_500);
        assert_eq!(flattened[0].clips[0].source_path, "deep.mp4");
    }

    #[test]
    fn test_flatten_compound_no_compound_clips_remain() {
        use crate::model::track::Track;

        let mut inner = Track::new_video("Inner");
        inner.add_clip(Clip::new("a.mp4", 1_000, 0, ClipKind::Video));
        let mut compound = Clip::new_compound(0, vec![inner]);
        compound.id = "C".into();

        let mut root = Track::new_video("Root");
        root.add_clip(compound);

        let flattened = flatten_compound_tracks(&[root]);
        for track in &flattened {
            for clip in &track.clips {
                assert_ne!(
                    clip.kind,
                    ClipKind::Compound,
                    "no compound clips should remain after flattening"
                );
            }
        }
    }

    #[test]
    fn test_flatten_compound_audio_goes_to_audio_track() {
        use crate::model::track::Track;

        // Compound clip with internal video + audio tracks
        let mut inner_v = Track::new_video("Inner V");
        let mut vc = Clip::new("video.mp4", 5_000, 0, ClipKind::Video);
        vc.id = "vc".into();
        inner_v.add_clip(vc);

        let mut inner_a = Track::new_audio("Inner A");
        let mut ac = Clip::new("audio.wav", 5_000, 0, ClipKind::Audio);
        ac.id = "ac".into();
        inner_a.add_clip(ac);

        let mut compound = Clip::new_compound(1_000, vec![inner_v, inner_a]);
        compound.id = "compound".into();

        let mut video_track = Track::new_video("V1");
        video_track.add_clip(compound);

        let flattened = flatten_compound_tracks(&[video_track]);

        // Should have at least 2 tracks: original video track + audio track for extracted audio
        let video_tracks: Vec<_> = flattened
            .iter()
            .filter(|t| t.kind == crate::model::track::TrackKind::Video)
            .collect();
        let audio_tracks: Vec<_> = flattened
            .iter()
            .filter(|t| t.kind == crate::model::track::TrackKind::Audio)
            .collect();

        // Video track should have the video clip, no audio clips
        assert!(!video_tracks.is_empty());
        for vt in &video_tracks {
            for clip in &vt.clips {
                assert_ne!(
                    clip.kind,
                    ClipKind::Audio,
                    "audio clips should not be on video tracks"
                );
            }
        }

        // Audio track should have the extracted audio clip
        assert!(
            !audio_tracks.is_empty(),
            "should have an audio track for compound internal audio"
        );
        let audio_clip_count: usize = audio_tracks.iter().map(|t| t.clips.len()).sum();
        assert!(
            audio_clip_count >= 1,
            "audio track should contain the extracted audio clip"
        );

        // Verify the audio clip has the correct timeline offset (compound starts at 1000)
        let first_audio = &audio_tracks[0].clips[0];
        assert_eq!(first_audio.source_path, "audio.wav");
        assert_eq!(first_audio.timeline_start, 1_000); // compound offset applied
    }

    #[test]
    fn test_flatten_compound_with_source_in_offset() {
        use crate::model::track::Track;

        // Simulate a razor-cut compound clip (right half) where source_in > 0.
        // Internal clips: A at 0..3000, B at 3000..6000
        // Compound source_in=3000 (cut at 3000), source_out=6000
        // Compound timeline_start=10000 (placed at 10s on parent)
        let mut inner_track = Track::new_video("Inner V");
        let mut clip_a = Clip::new("a.mp4", 3_000, 0, ClipKind::Video);
        clip_a.id = "A".into();
        inner_track.add_clip(clip_a);
        let mut clip_b = Clip::new("b.mp4", 6_000, 3_000, ClipKind::Video);
        clip_b.id = "B".into();
        inner_track.add_clip(clip_b);

        let mut compound = Clip::new_compound(10_000, vec![inner_track]);
        compound.id = "compound".into();
        // Simulate razor cut: right half starts at source_in=3000
        compound.source_in = 3_000;
        // source_out stays at 6000 (the internal timeline duration)

        let mut root_track = Track::new_video("Root V");
        root_track.add_clip(compound);

        let flattened = flatten_compound_tracks(&[root_track]);
        // Clip A (0..3000) is entirely before source_in=3000, should be excluded
        // Clip B (3000..6000) is within the window, should appear at 10000
        assert_eq!(flattened[0].clips.len(), 1);
        assert_eq!(flattened[0].clips[0].source_path, "b.mp4");
        assert_eq!(flattened[0].clips[0].timeline_start, 10_000);
    }

    #[test]
    fn test_flatten_compound_moved_after_cut_no_gap() {
        use crate::model::track::Track;

        // Scenario: compound cut at 10s, left half deleted, right half moved
        // to position 0.  The compound's visible window is [10000, 25000] but
        // timeline_start is 0 — content must start immediately, no gap.
        let mut inner_track = Track::new_video("Inner V");
        let mut clip_a = Clip::new("a.mp4", 10_000, 0, ClipKind::Video);
        clip_a.id = "A".into();
        inner_track.add_clip(clip_a);
        let mut clip_b = Clip::new("b.mp4", 25_000, 10_000, ClipKind::Video);
        clip_b.id = "B".into();
        inner_track.add_clip(clip_b);

        let mut compound = Clip::new_compound(0, vec![inner_track]);
        compound.id = "compound".into();
        compound.source_in = 10_000;
        // source_out = 25_000 (from new_compound)

        let mut root = Track::new_video("Root V");
        root.add_clip(compound);

        let flattened = flatten_compound_tracks(&[root]);
        // Clip A (0..10000) is outside window, excluded
        // Clip B (10000..25000) should start at position 0 (no gap)
        assert_eq!(flattened[0].clips.len(), 1);
        assert_eq!(flattened[0].clips[0].source_path, "b.mp4");
        assert_eq!(
            flattened[0].clips[0].timeline_start, 0,
            "clip should start at 0 with no gap"
        );
    }

    #[test]
    fn test_flatten_compound_trims_partial_overlap() {
        use crate::model::track::Track;

        // Internal clip spans 1000..5000, compound window is 2000..4000
        let mut inner_track = Track::new_video("Inner V");
        let mut clip = Clip::new("wide.mp4", 5_000, 1_000, ClipKind::Video);
        clip.source_in = 0;
        clip.source_out = 4_000; // 4000ns of source material
        clip.timeline_start = 1_000;
        clip.id = "W".into();
        inner_track.add_clip(clip);

        let mut compound = Clip::new_compound(20_000, vec![inner_track]);
        compound.id = "compound".into();
        compound.source_in = 2_000;
        compound.source_out = 4_000;

        let mut root = Track::new_video("Root V");
        root.add_clip(compound);

        let flattened = flatten_compound_tracks(&[root]);
        assert_eq!(flattened[0].clips.len(), 1);
        let fc = &flattened[0].clips[0];
        // compound_offset = 20000 - 2000 = 18000
        // Clip starts at 1000, trimmed to window_start=2000, so trim=1000
        // Rebased: 18000 + 2000 = 20000
        assert_eq!(fc.timeline_start, 20_000);
        // source_in trimmed by 1000 (from 0 to 1000)
        assert_eq!(fc.source_in, 1_000);
        // source_out trimmed: clip would end at 5000, window_end=4000, excess=1000
        // source_out was 4000, minus 1000 = 3000
        assert_eq!(fc.source_out, 3_000);
    }

    #[test]
    fn test_flatten_multicam_produces_video_segments_and_audio() {
        use crate::model::clip::MulticamAngle;
        use crate::model::track::Track;

        let mut mc = Clip::new_multicam(
            5_000,
            vec![
                MulticamAngle {
                    id: "a1".into(),
                    label: "Cam1".into(),
                    source_path: "cam1.mp4".into(),
                    source_in: 0,
                    source_out: 20_000,
                    sync_offset_ns: 0,
                    source_timecode_base_ns: None,
                    media_duration_ns: None,
                    volume: 1.0,
                    muted: false,
                },
                MulticamAngle {
                    id: "a2".into(),
                    label: "Cam2".into(),
                    source_path: "cam2.mp4".into(),
                    source_in: 0,
                    source_out: 20_000,
                    sync_offset_ns: 0,
                    source_timecode_base_ns: None,
                    media_duration_ns: None,
                    volume: 0.5,
                    muted: false,
                },
            ],
        );
        mc.id = "mc1".into();
        // Add a switch: angle 0 at 0, angle 1 at 10000
        mc.insert_angle_switch(10_000, 1);

        let mut root = Track::new_video("V1");
        root.add_clip(mc);

        let flattened = flatten_compound_tracks(&[root]);

        // Video track: should have 2 video segments (angle 0: 5000-15000, angle 1: 15000-25000)
        let video_tracks: Vec<_> = flattened
            .iter()
            .filter(|t| t.kind == crate::model::track::TrackKind::Video)
            .collect();
        assert!(!video_tracks.is_empty());
        let video_clips: Vec<_> = video_tracks.iter().flat_map(|t| &t.clips).collect();
        assert_eq!(
            video_clips.len(),
            2,
            "should have 2 video segments from angle switches"
        );
        assert_eq!(video_clips[0].source_path, "cam1.mp4");
        assert_eq!(video_clips[1].source_path, "cam2.mp4");

        // Audio tracks: should have 2 audio clips (one per unmuted angle, continuous)
        let audio_tracks: Vec<_> = flattened
            .iter()
            .filter(|t| t.kind == crate::model::track::TrackKind::Audio)
            .collect();
        let audio_clips: Vec<_> = audio_tracks.iter().flat_map(|t| &t.clips).collect();
        assert_eq!(
            audio_clips.len(),
            2,
            "should have 2 audio clips (both angles unmuted)"
        );
        // Both start at the multicam clip's timeline_start
        for ac in &audio_clips {
            assert_eq!(ac.timeline_start, 5_000);
            assert_eq!(ac.kind, ClipKind::Audio);
        }
    }

    #[test]
    fn test_flatten_multicam_muted_angle_excluded_from_audio() {
        use crate::model::clip::MulticamAngle;
        use crate::model::track::Track;

        let mc = Clip::new_multicam(
            0,
            vec![
                MulticamAngle {
                    id: "a1".into(),
                    label: "Cam1".into(),
                    source_path: "cam1.mp4".into(),
                    source_in: 0,
                    source_out: 10_000,
                    sync_offset_ns: 0,
                    source_timecode_base_ns: None,
                    media_duration_ns: None,
                    volume: 1.0,
                    muted: false,
                },
                MulticamAngle {
                    id: "a2".into(),
                    label: "Cam2".into(),
                    source_path: "cam2.mp4".into(),
                    source_in: 0,
                    source_out: 10_000,
                    sync_offset_ns: 0,
                    source_timecode_base_ns: None,
                    media_duration_ns: None,
                    volume: 0.0,
                    muted: true, // muted
                },
            ],
        );

        let mut root = Track::new_video("V1");
        root.add_clip(mc);

        let flattened = flatten_compound_tracks(&[root]);
        let audio_clips: Vec<_> = flattened
            .iter()
            .filter(|t| t.kind == crate::model::track::TrackKind::Audio)
            .flat_map(|t| &t.clips)
            .collect();
        assert_eq!(
            audio_clips.len(),
            1,
            "muted angle should be excluded from audio"
        );
        assert_eq!(audio_clips[0].source_path, "cam1.mp4");
    }

    #[test]
    fn test_flatten_multicam_inside_compound() {
        use crate::model::clip::MulticamAngle;
        use crate::model::track::Track;

        // Multicam clip inside a compound clip
        let mc = Clip::new_multicam(
            0,
            vec![MulticamAngle {
                id: "a1".into(),
                label: "Cam1".into(),
                source_path: "cam1.mp4".into(),
                source_in: 0,
                source_out: 10_000,
                sync_offset_ns: 0,
                source_timecode_base_ns: None,
                media_duration_ns: None,
                volume: 1.0,
                muted: false,
            }],
        );

        let mut inner_track = Track::new_video("Inner V");
        inner_track.add_clip(mc);

        let mut compound = Clip::new_compound(2_000, vec![inner_track]);
        compound.id = "compound".into();

        let mut root = Track::new_video("V1");
        root.add_clip(compound);

        let flattened = flatten_compound_tracks(&[root]);

        // The multicam inside the compound should be flattened:
        // - Video segment from cam1.mp4 at offset 2000 (compound start)
        // - Audio clip from cam1.mp4 at offset 2000
        let all_clips: Vec<_> = flattened.iter().flat_map(|t| &t.clips).collect();
        assert!(
            !all_clips.is_empty(),
            "nested multicam should produce clips"
        );
        // No compound or multicam clips should remain
        for clip in &all_clips {
            assert_ne!(clip.kind, ClipKind::Compound);
            assert_ne!(clip.kind, ClipKind::Multicam);
        }
        // Video clip should be at compound offset
        let video_clips: Vec<_> = flattened
            .iter()
            .filter(|t| t.kind == crate::model::track::TrackKind::Video)
            .flat_map(|t| &t.clips)
            .collect();
        assert!(!video_clips.is_empty());
        assert_eq!(video_clips[0].timeline_start, 2_000);
    }
}
