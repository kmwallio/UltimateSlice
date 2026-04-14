//! Prerender FFmpeg filter-string builders.
//!
//! These pure functions translate `ProgramClip` properties into lavfi filter
//! fragments used by the background prerender pipeline. Extracted from
//! `program_player.rs` to reduce its size.

use super::program_player::ProgramClip;
use crate::media::color_math::ThreePointParabola;
use std::path::Path;

/// Transition-related metadata passed through the prerender pipeline.
#[derive(Clone)]
pub(crate) struct TransitionPrerenderSpec {
    pub(crate) outgoing_input: usize,
    pub(crate) incoming_input: usize,
    pub(crate) xfade_transition: String,
    pub(crate) duration_ns: u64,
    pub(crate) before_cut_ns: u64,
    pub(crate) after_cut_ns: u64,
}

pub(crate) fn prerender_build_transition_tpad_filter(
    transition_spec: Option<&TransitionPrerenderSpec>,
    transition_offset_ns: u64,
    input_index: usize,
) -> String {
    let Some(spec) = transition_spec else {
        return String::new();
    };
    let mut filter = String::new();
    let offset_s = transition_offset_ns as f64 / 1_000_000_000.0;
    if input_index == spec.incoming_input && offset_s > 0.0 {
        // Keep incoming transition source parked on its first frame until the
        // overlap boundary, so pre-padding does not advance incoming content.
        filter.push_str(&format!(
            ",tpad=start_duration={offset_s:.6}:start_mode=clone"
        ));
    }
    if input_index == spec.outgoing_input && spec.after_cut_ns > 0 {
        // For after-cut overlap, hold the outgoing clip's final frame long
        // enough for xfade to cover the tail that extends past the cut.
        filter.push_str(&format!(
            ",tpad=stop_mode=clone:stop_duration={:.6}",
            spec.after_cut_ns as f64 / 1_000_000_000.0
        ));
    }
    filter
}

pub(crate) fn prerender_build_transition_adelay_filter(
    transition_spec: Option<&TransitionPrerenderSpec>,
    transition_offset_ns: u64,
    input_index: usize,
) -> String {
    let Some(spec) = transition_spec else {
        return String::new();
    };
    if input_index != spec.incoming_input {
        return String::new();
    }
    if transition_offset_ns == 0 {
        return String::new();
    }
    // Keep incoming transition audio silent until overlap boundary so
    // prerender pre-padding does not introduce early incoming-audio bleed.
    let delay_ms = ((transition_offset_ns + 999_999) / 1_000_000).max(1);
    format!(",adelay={delay_ms}:all=1")
}

pub(crate) fn prerender_build_color_filter(clip: &ProgramClip) -> String {
    let has_color_keyframes = !clip.brightness_keyframes.is_empty()
        || !clip.contrast_keyframes.is_empty()
        || !clip.saturation_keyframes.is_empty();
    let has_color = clip.brightness != 0.0 || clip.contrast != 1.0 || clip.saturation != 1.0;
    let has_exposure = clip.exposure.abs() > f64::EPSILON;
    if has_color_keyframes {
        // Brightness is bounded ±1.0 (a normalized value range, not a
        // transform property — kept as a literal here because it's not
        // shared with anything else).
        let brightness_expr = crate::media::export::build_keyframed_property_expression(
            &clip.brightness_keyframes,
            clip.brightness,
            -1.0,
            1.0,
            "t",
        );
        let contrast_expr = crate::media::export::build_keyframed_property_expression(
            &clip.contrast_keyframes,
            clip.contrast,
            0.0,
            2.0,
            "t",
        );
        let saturation_expr = crate::media::export::build_keyframed_property_expression(
            &clip.saturation_keyframes,
            clip.saturation,
            0.0,
            2.0,
            "t",
        );
        let brightness_expr = if has_exposure {
            let exposure_brightness_delta = clip.exposure.clamp(-1.0, 1.0) * 0.55;
            format!("({brightness_expr})+{exposure_brightness_delta:.6}")
        } else {
            brightness_expr
        };
        let contrast_expr = if has_exposure {
            let exposure_contrast_delta = clip.exposure.clamp(-1.0, 1.0) * 0.12;
            format!("({contrast_expr})+{exposure_contrast_delta:.6}")
        } else {
            contrast_expr
        };
        format!(
            ",eq=brightness='{brightness_expr}':contrast='{contrast_expr}':saturation='{saturation_expr}':eval=frame"
        )
    } else if has_color || has_exposure {
        // Use the same calibrated videobalance mapping as export so that
        // proxy-mode preview matches the final render.
        let preview_params =
            crate::media::program_player::ProgramPlayer::compute_videobalance_params(
                clip.brightness,
                clip.contrast,
                clip.saturation,
                6500.0, // temperature handled by separate filter
                0.0,    // tint handled by separate filter
                0.0,    // shadows handled by grading filter
                0.0,    // midtones handled by grading filter
                0.0,    // highlights handled by grading filter
                clip.exposure,
                0.0, // black_point handled by grading filter
                0.0, // warmth/tint handled by grading filter
                0.0,
                0.0,
                0.0,
                0.0,
                0.0,
                true,
                true,
            );
        let contrast_t = clip.contrast.clamp(0.0, 2.0);
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

pub(crate) fn prerender_build_temperature_tint_filter(
    clip: &ProgramClip,
    caps: &crate::media::export::ColorFilterCapabilities,
) -> String {
    let has_temp = (clip.temperature - 6500.0).abs() > 1.0;
    let has_tint = clip.tint.abs() > 0.001;
    let has_temp_keyframes = !clip.temperature_keyframes.is_empty();
    let has_tint_keyframes = !clip.tint_keyframes.is_empty();
    // Use frei0r coloradj_RGB when available — same calibrated path as export.
    if caps.use_coloradj_frei0r
        && (has_temp || has_tint)
        && !has_temp_keyframes
        && !has_tint_keyframes
    {
        let cp = crate::media::export::compute_export_coloradj_params(clip.temperature, clip.tint);
        return format!(
            ",frei0r=filter_name=coloradj_RGB:filter_params={:.6}|{:.6}|{:.6}|0.333",
            cp.r, cp.g, cp.b
        );
    }
    // Fallback when frei0r is unavailable.
    let mut f = String::new();
    if has_temp_keyframes {
        let temp_expr = crate::media::export::build_keyframed_property_expression(
            &clip.temperature_keyframes,
            clip.temperature,
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
        let tint_expr = crate::media::export::build_keyframed_property_expression(
            &clip.tint_keyframes,
            clip.tint,
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
        let t = clip.tint.clamp(-1.0, 1.0);
        let gm = -t * 0.5;
        let rm = t * 0.25;
        let bm = t * 0.25;
        f.push_str(&format!(",colorbalance=rm={rm:.4}:gm={gm:.4}:bm={bm:.4}"));
    }
    f
}

pub(crate) fn prerender_build_denoise_filter(clip: &ProgramClip) -> String {
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

pub(crate) fn prerender_build_sharpen_filter(clip: &ProgramClip) -> String {
    if clip.sharpness != 0.0 {
        let la = (clip.sharpness * 3.0).clamp(-2.0, 5.0);
        format!(",unsharp=lx=5:ly=5:la={la:.4}:cx=5:cy=5:ca={la:.4}")
    } else {
        String::new()
    }
}

pub(crate) fn prerender_build_blur_filter(clip: &ProgramClip) -> String {
    if clip.blur > f64::EPSILON {
        let radius = (clip.blur * 10.0).clamp(0.0, 10.0);
        format!(",boxblur={radius:.0}:{radius:.0}")
    } else {
        String::new()
    }
}

pub(crate) fn prerender_build_anamorphic_filter(clip: &ProgramClip) -> String {
    if (clip.anamorphic_desqueeze - 1.0).abs() > 0.001 {
        // Physically desqueeze the source pixels horizontally and reset SAR to 1.
        format!(",scale=iw*{}:ih,setsar=1", clip.anamorphic_desqueeze)
    } else {
        String::new()
    }
}

pub(crate) fn prerender_build_lut_filter(clip: &ProgramClip, source_is_proxy: bool) -> String {
    if source_is_proxy {
        return String::new();
    }
    let mut result = String::new();
    for path in &clip.lut_paths {
        if !path.is_empty() && Path::new(path).exists() {
            let escaped = path.replace('\\', "\\\\").replace(':', "\\:");
            result.push_str(&format!(",lut3d={escaped}"));
        }
    }
    result
}

pub(crate) fn prerender_build_chroma_key_filter(clip: &ProgramClip) -> String {
    if clip.chroma_key_enabled {
        let color = format!(
            "0x{:02X}{:02X}{:02X}",
            (clip.chroma_key_color >> 16) & 0xFF,
            (clip.chroma_key_color >> 8) & 0xFF,
            clip.chroma_key_color & 0xFF
        );
        let similarity = (clip.chroma_key_tolerance * 0.5).clamp(0.01, 0.5);
        let blend = (clip.chroma_key_softness * 0.5).clamp(0.0, 0.5);
        format!(",colorkey={color}:{similarity:.4}:{blend:.4}")
    } else {
        String::new()
    }
}

pub(crate) fn prerender_build_grading_filter(clip: &ProgramClip) -> String {
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
        // Use the same parabola-matched lutrgb as export for proxy parity.
        let p = crate::media::program_player::ProgramPlayer::compute_export_3point_params(
            clip.shadows,
            clip.midtones,
            clip.highlights,
            clip.black_point,
            clip.highlights_warmth,
            clip.highlights_tint,
            clip.midtones_warmth,
            clip.midtones_tint,
            clip.shadows_warmth,
            clip.shadows_tint,
        );
        let parabola = ThreePointParabola::from_params(&p);
        parabola.to_lutrgb_filter()
    } else {
        String::new()
    }
}

pub(crate) fn prerender_build_crop_filter(
    clip: &ProgramClip,
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

pub(crate) fn prerender_build_rotation_filter(clip: &ProgramClip, transparent_pad: bool) -> String {
    // Keyframed rotation: emit per-frame ffmpeg expression mirroring
    // export's `build_rotation_filter` keyframed branch.
    if !clip.rotate_keyframes.is_empty() {
        let fill = if transparent_pad { "black@0" } else { "black" };
        let angle_expr = crate::media::export::build_keyframed_property_expression(
            &clip.rotate_keyframes,
            clip.rotate as f64,
            -180.0,
            180.0,
            "t",
        );
        return format!(",rotate='-({angle_expr})*PI/180':fillcolor={fill}");
    }
    if clip.rotate == 0 {
        return String::new();
    }
    let fill = if transparent_pad { "black@0" } else { "black" };
    format!(
        ",rotate={:.10}:fillcolor={fill}",
        -(clip.rotate as f64).to_radians()
    )
}

/// Returns `true` when this prerender clip has any keyframe lane that
/// affects per-frame geometry (scale, position, or crop). Rotation is
/// handled inline by `prerender_build_rotation_filter`, so it's NOT in
/// this gate — it doesn't need the multi-stream overlay path.
///
/// When this returns `true`, the prerender format string for this clip
/// uses the keyframed multi-stream overlay chain instead of the existing
/// static `prerender_build_scale_position_filter`.
pub(crate) fn prerender_clip_has_keyframed_overlay(clip: &ProgramClip) -> bool {
    !clip.scale_keyframes.is_empty()
        || !clip.position_x_keyframes.is_empty()
        || !clip.position_y_keyframes.is_empty()
        || !clip.crop_left_keyframes.is_empty()
        || !clip.crop_right_keyframes.is_empty()
        || !clip.crop_top_keyframes.is_empty()
        || !clip.crop_bottom_keyframes.is_empty()
        || !clip.opacity_keyframes.is_empty()
}

/// Build the multi-step keyframed transform tail for a prerender clip
/// chain. Mirrors the keyframed branch in `src/media/export.rs:543-605`:
///
/// 1. `,scale=w='max(1,{out_w}*({scale_expr}))':h=...:eval=frame[fg]`
/// 2. `color=c={bg}:size={out_w}x{out_h}:r={fps}:d={dur}[bg]`
/// 3. `[bg][fg]overlay=x='...':y='...':eval=frame,geq=...alpha*({opacity_expr})[raw]`
/// 4. `[raw]format=yuv420p[output_label]` (or yuva420p for transparent)
///
/// Returns a multi-step filter graph fragment with internal chains
/// separated by `;`, ending in `[{output_label}]`. The caller pushes
/// this as one entry into `nodes` (which is later joined by `;`),
/// AFTER pushing a separate node that produces the `[{pre_chain_label}]`
/// label containing all the static effects.
pub(crate) fn prerender_build_keyframed_overlay_tail(
    clip: &ProgramClip,
    pre_chain_label: &str,
    output_label: &str,
    out_w: u32,
    out_h: u32,
    fps_n: u32,
    fps_d: u32,
    transparent_bg: bool,
    final_format_yuv420p: bool,
    post_tail: &str,
) -> String {
    use crate::media::export::build_keyframed_property_expression;
    use crate::model::transform_bounds::{POSITION_MAX, POSITION_MIN, SCALE_MAX, SCALE_MIN};
    let scale_expr = build_keyframed_property_expression(
        &clip.scale_keyframes,
        clip.scale,
        SCALE_MIN,
        SCALE_MAX,
        "t",
    );
    let pos_x_expr = build_keyframed_property_expression(
        &clip.position_x_keyframes,
        clip.position_x,
        POSITION_MIN,
        POSITION_MAX,
        "t",
    );
    let pos_y_expr = build_keyframed_property_expression(
        &clip.position_y_keyframes,
        clip.position_y,
        POSITION_MIN,
        POSITION_MAX,
        "t",
    );
    let opacity_expr =
        build_keyframed_property_expression(&clip.opacity_keyframes, clip.opacity, 0.0, 1.0, "T");
    let clip_duration_s = clip.duration_ns() as f64 / 1_000_000_000.0;
    let bg_color = if transparent_bg { "black@0" } else { "black" };
    let (overlay_x_expr, overlay_y_expr) =
        if crate::media::program_player::ProgramPlayer::clip_uses_direct_canvas_translation(clip) {
            (
                format!("(W*(1+({pos_x_expr}))-w)/2"),
                format!("(H*(1+({pos_y_expr}))-h)/2"),
            )
        } else {
            (
                format!("(W-w)*(1+({pos_x_expr}))/2"),
                format!("(H-h)*(1+({pos_y_expr}))/2"),
            )
        };
    // Build the tail filter chain that consumes [raw] and produces
    // [output_label]. ffmpeg syntax requires at least one filter
    // between labels, and a leading comma after a `]` is invalid, so
    // strip any leading comma from `post_tail` and fall back to a
    // no-op `null` filter when nothing else is being applied.
    let post_tail_clean = post_tail.strip_prefix(',').unwrap_or(post_tail);
    let raw_to_output_chain = if final_format_yuv420p {
        if post_tail_clean.is_empty() {
            "format=yuv420p".to_string()
        } else {
            format!("format=yuv420p,{post_tail_clean}")
        }
    } else if post_tail_clean.is_empty() {
        "null".to_string()
    } else {
        post_tail_clean.to_string()
    };
    let fg_label = format!("{output_label}fg");
    let bg_label = format!("{output_label}bg");
    let raw_label = format!("{output_label}raw");
    format!(
        "[{pre_chain_label}]scale=w='max(1,{out_w}*({scale_expr}))':h='max(1,{out_h}*({scale_expr}))':eval=frame[{fg_label}];color=c={bg_color}:size={out_w}x{out_h}:r={fps_n}/{fps_d}:d={clip_duration_s:.6}[{bg_label}];[{bg_label}][{fg_label}]overlay=x='{overlay_x_expr}':y='{overlay_y_expr}':eval=frame,geq=lum='lum(X,Y)':cb='cb(X,Y)':cr='cr(X,Y)':a='alpha(X,Y)*({opacity_expr})'[{raw_label}];[{raw_label}]{raw_to_output_chain}[{output_label}]"
    )
}

pub(crate) fn prerender_build_flip_filter(clip: &ProgramClip) -> String {
    match (clip.flip_h, clip.flip_v) {
        (false, false) => String::new(),
        (true, false) => ",hflip".to_string(),
        (false, true) => ",vflip".to_string(),
        (true, true) => ",hflip,vflip".to_string(),
    }
}

pub(crate) fn prerender_build_scale_position_filter(
    clip: &ProgramClip,
    out_w: u32,
    out_h: u32,
    transparent_pad: bool,
) -> String {
    let scale = clip.scale.clamp(
        crate::model::transform_bounds::SCALE_MIN,
        crate::model::transform_bounds::SCALE_MAX,
    );
    if (scale - 1.0).abs() < 0.001 && clip.position_x.abs() < 0.001 && clip.position_y.abs() < 0.001
    {
        return String::new();
    }
    let pw = out_w as f64;
    let ph = out_h as f64;
    let pos_x = clip.position_x;
    let pos_y = clip.position_y;
    let sw = (pw * scale).round() as u32;
    let sh = (ph * scale).round() as u32;

    if crate::media::program_player::ProgramPlayer::clip_uses_direct_canvas_translation(clip) {
        let raw_x =
            crate::media::program_player::ProgramPlayer::direct_canvas_origin(pw, sw as f64, pos_x)
                as i64;
        let raw_y =
            crate::media::program_player::ProgramPlayer::direct_canvas_origin(ph, sh as f64, pos_y)
                as i64;
        return prerender_build_scale_translate_filter(
            sw,
            sh,
            raw_x,
            raw_y,
            out_w,
            out_h,
            transparent_pad,
        );
    }

    if scale >= 1.0 {
        let total_x = pw * (scale - 1.0);
        let total_y = ph * (scale - 1.0);
        let cx = (total_x * (1.0 + pos_x) / 2.0).round() as i64;
        let cy = (total_y * (1.0 + pos_y) / 2.0).round() as i64;
        format!(",scale={sw}:{sh},crop={out_w}:{out_h}:{cx}:{cy}")
    } else {
        let total_x = pw * (1.0 - scale);
        let total_y = ph * (1.0 - scale);
        let raw_pad_x = (total_x * (1.0 + pos_x) / 2.0).round() as i64;
        let raw_pad_y = (total_y * (1.0 + pos_y) / 2.0).round() as i64;
        prerender_build_scale_translate_filter(
            sw,
            sh,
            raw_pad_x,
            raw_pad_y,
            out_w,
            out_h,
            transparent_pad,
        )
    }
}

pub(crate) fn prerender_build_scale_translate_filter(
    sw: u32,
    sh: u32,
    raw_x: i64,
    raw_y: i64,
    out_w: u32,
    out_h: u32,
    transparent_pad: bool,
) -> String {
    let crop_left = if raw_x < 0 { (-raw_x) as u32 } else { 0 };
    let crop_top = if raw_y < 0 { (-raw_y) as u32 } else { 0 };
    let crop_right = if raw_x + sw as i64 > out_w as i64 {
        (raw_x + sw as i64 - out_w as i64) as u32
    } else {
        0
    };
    let crop_bottom = if raw_y + sh as i64 > out_h as i64 {
        (raw_y + sh as i64 - out_h as i64) as u32
    } else {
        0
    };
    let pad_x = raw_x.max(0) as u32;
    let pad_y = raw_y.max(0) as u32;
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

/// Build a chain of ffmpeg frei0r filters for user-applied effects.
/// Mirrors the export-pipeline `build_frei0r_effects_filter()` logic
/// but operates on `ProgramClip` instead of `Clip`.
pub(crate) fn prerender_build_frei0r_effects_filter(clip: &ProgramClip) -> String {
    use crate::media::frei0r_registry::{Frei0rNativeType, Frei0rRegistry};

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
        let params_str = if let Some(info) = plugin {
            if !info.native_params.is_empty() {
                info.native_params
                    .iter()
                    .map(|np| match np.native_type {
                        Frei0rNativeType::Color => {
                            let r = np
                                .gst_properties
                                .first()
                                .and_then(|k| effect.params.get(k))
                                .copied()
                                .unwrap_or(0.0);
                            let g = np
                                .gst_properties
                                .get(1)
                                .and_then(|k| effect.params.get(k))
                                .copied()
                                .unwrap_or(0.0);
                            let b = np
                                .gst_properties
                                .get(2)
                                .and_then(|k| effect.params.get(k))
                                .copied()
                                .unwrap_or(0.0);
                            format!("{r:.6}/{g:.6}/{b:.6}")
                        }
                        Frei0rNativeType::Position => {
                            let x = np
                                .gst_properties
                                .first()
                                .and_then(|k| effect.params.get(k))
                                .copied()
                                .unwrap_or(0.0);
                            let y = np
                                .gst_properties
                                .get(1)
                                .and_then(|k| effect.params.get(k))
                                .copied()
                                .unwrap_or(0.0);
                            format!("{x:.6}/{y:.6}")
                        }
                        Frei0rNativeType::NativeString => {
                            let prop = np.gst_properties.first().map(|s| s.as_str()).unwrap_or("");
                            effect.string_params.get(prop).cloned().unwrap_or_default()
                        }
                        _ => {
                            let prop = np.gst_properties.first().map(|s| s.as_str()).unwrap_or("");
                            if np.native_type == Frei0rNativeType::Bool {
                                let val = effect.params.get(prop).copied().unwrap_or(0.0);
                                if val > 0.5 {
                                    "y".to_string()
                                } else {
                                    "n".to_string()
                                }
                            } else {
                                let val = effect.params.get(prop).copied().unwrap_or(0.0);
                                format!("{val:.6}")
                            }
                        }
                    })
                    .collect::<Vec<_>>()
                    .join("|")
            } else {
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
        } else {
            effect
                .params
                .values()
                .map(|v| format!("{v:.6}"))
                .collect::<Vec<_>>()
                .join("|")
        };
        let ffmpeg_name = plugin
            .map(|p| p.ffmpeg_name.as_str())
            .unwrap_or(&effect.plugin_name);
        if params_str.is_empty() {
            result.push_str(&format!(",frei0r=filter_name={ffmpeg_name}"));
        } else {
            result.push_str(&format!(
                ",frei0r=filter_name={ffmpeg_name}:filter_params={params_str}"
            ));
        }
    }
    result
}

/// Build an ffmpeg `drawtext` filter for a title clip's text overlay.
/// Mirrors the export-pipeline `build_title_filter()` logic but operates
/// on `ProgramClip` instead of `Clip`.
pub(crate) fn prerender_build_title_filter(clip: &ProgramClip, out_h: u32) -> String {
    if clip.title_text.trim().is_empty() {
        return String::new();
    }
    use crate::model::clip::TitleAnimation;
    const REF_H: f64 = 1080.0;
    let text =
        crate::media::title_font::escape_drawtext_value(&clip.title_text).replace('\n', "\\n");
    let font_size = crate::media::title_font::parse_title_font(&clip.title_font).size_points();
    let font_option = crate::media::title_font::build_drawtext_font_option(&clip.title_font);
    let rel_x = clip.title_x.clamp(0.0, 1.0);
    let rel_y = clip.title_y.clamp(0.0, 1.0);
    let scale_factor = out_h as f64 / REF_H;
    let scaled_size = font_size * (4.0 / 3.0) * scale_factor;
    let (r, g, b, a) = crate::ui::colors::rgba_u32_to_u8(clip.title_color);
    let alpha = (a as f64 / 255.0).clamp(0.0, 1.0);

    let mut style = String::new();
    if clip.title_outline_width > 0.0 {
        let bw = (clip.title_outline_width * scale_factor).max(0.5);
        let (or_, og, ob, oa) = crate::ui::colors::rgba_u32_to_u8(clip.title_outline_color);
        let o_alpha = (oa as f64 / 255.0).clamp(0.0, 1.0);
        style.push_str(&format!(
            ":borderw={bw:.1}:bordercolor={or_:02x}{og:02x}{ob:02x}@{o_alpha:.4}"
        ));
    }
    if clip.title_shadow {
        let sx = (clip.title_shadow_offset_x * scale_factor).round() as i32;
        let sy = (clip.title_shadow_offset_y * scale_factor).round() as i32;
        let (sr, sg, sb, sa) = crate::ui::colors::rgba_u32_to_u8(clip.title_shadow_color);
        let s_alpha = (sa as f64 / 255.0).clamp(0.0, 1.0);
        style.push_str(&format!(
            ":shadowx={sx}:shadowy={sy}:shadowcolor={sr:02x}{sg:02x}{sb:02x}@{s_alpha:.4}"
        ));
    }
    if clip.title_bg_box {
        let pad = (clip.title_bg_box_padding * scale_factor).round() as i32;
        let (br, bgg, bb, ba) = crate::ui::colors::rgba_u32_to_u8(clip.title_bg_box_color);
        let b_alpha = (ba as f64 / 255.0).clamp(0.0, 1.0);
        style.push_str(&format!(
            ":box=1:boxcolor={br:02x}{bgg:02x}{bb:02x}@{b_alpha:.4}:boxborderw={pad}"
        ));
    }

    let dur_s = (clip.title_animation_duration_ns as f64 / 1_000_000_000.0).max(1e-6);
    let pos_x = format!("({rel_x:.6})*w-text_w/2");
    let pos_y = format!("({rel_y:.6})*h-text_h/2");
    let base_color = format!("{r:02x}{g:02x}{b:02x}@{alpha:.4}");

    let mut filter = String::new();
    match clip.title_animation {
        TitleAnimation::None | TitleAnimation::Pop => {
            filter.push_str(&format!(
                ",drawtext={font_option}:text='{text}':fontsize={scaled_size:.2}:fontcolor={base_color}:x='{pos_x}':y='{pos_y}'{style}"
            ));
        }
        TitleAnimation::Fade => {
            let alpha_expr = format!("min(1,max(0,t/{dur_s:.4}))*{alpha:.4}");
            filter.push_str(&format!(
                ",drawtext={font_option}:text='{text}':fontsize={scaled_size:.2}:fontcolor={r:02x}{g:02x}{b:02x}:x='{pos_x}':y='{pos_y}':alpha='{alpha_expr}'{style}"
            ));
        }
        TitleAnimation::Typewriter => {
            let char_count = clip.title_text.chars().count();
            if char_count == 0 {
                filter.push_str(&format!(
                    ",drawtext={font_option}:text='{text}':fontsize={scaled_size:.2}:fontcolor={base_color}:x='{pos_x}':y='{pos_y}'{style}"
                ));
            } else {
                let step = dur_s / char_count as f64;
                for i in 0..char_count {
                    let prefix: String = clip.title_text.chars().take(i + 1).collect();
                    let prefix_esc = crate::media::title_font::escape_drawtext_value(&prefix)
                        .replace('\n', "\\n");
                    let t0 = i as f64 * step;
                    let enable = if i + 1 == char_count {
                        format!("gte(t\\,{t0:.4})")
                    } else {
                        let t1 = (i + 1) as f64 * step;
                        format!("between(t\\,{t0:.4}\\,{t1:.4})")
                    };
                    filter.push_str(&format!(
                        ",drawtext={font_option}:text='{prefix_esc}':fontsize={scaled_size:.2}:fontcolor={base_color}:x='{pos_x}':y='{pos_y}':enable='{enable}'{style}"
                    ));
                }
            }
        }
    }
    filter
}

/// Background-prerender variant of `build_motion_blur_filter` (export.rs).
/// Uses the same math: `tmix=frames=2` at 360°, otherwise oversample by
/// 4× via `minterpolate`, average the appropriate sub-frame count, and
/// decimate back to project rate. Returns empty when motion blur is
/// disabled or the clip has no per-frame motion (animated transform or
/// speed > 1).
pub(crate) fn prerender_build_motion_blur_filter(clip: &ProgramClip, fps: u32) -> String {
    if !clip.motion_blur_enabled || clip.motion_blur_shutter_angle <= 0.5 {
        return String::new();
    }
    let has_animated_transform = !clip.scale_keyframes.is_empty()
        || !clip.position_x_keyframes.is_empty()
        || !clip.position_y_keyframes.is_empty()
        || !clip.rotate_keyframes.is_empty()
        || !clip.crop_left_keyframes.is_empty()
        || !clip.crop_right_keyframes.is_empty()
        || !clip.crop_top_keyframes.is_empty()
        || !clip.crop_bottom_keyframes.is_empty();
    let has_fast_speed = clip.speed > 1.001;
    if !has_animated_transform && !has_fast_speed {
        return String::new();
    }
    let shutter = clip.motion_blur_shutter_angle.clamp(0.0, 720.0);
    if (shutter - 360.0).abs() < 0.5 {
        return ",tmix=frames=2:weights='1 1'".to_string();
    }
    const K: u32 = 4;
    let raw_frames = (K as f64 * shutter / 360.0).round() as i32;
    let frames = raw_frames.max(1).min((K * 2) as i32) as u32;
    let weights = std::iter::repeat("1")
        .take(frames as usize)
        .collect::<Vec<_>>()
        .join(" ");
    let over_fps = fps.saturating_mul(K).max(1);
    format!(
        ",minterpolate=fps={over_fps}:mi_mode=blend,tmix=frames={frames}:weights='{weights}',fps={fps}"
    )
}

pub(crate) fn prerender_build_minterpolate_filter(clip: &ProgramClip, fps: u32) -> String {
    use crate::model::clip::SlowMotionInterp;
    if clip.slow_motion_interp == SlowMotionInterp::Off {
        return String::new();
    }
    // Only apply for slow-motion clips
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
        // AI mode is realized via a precomputed sidecar consumed at the
        // input level — do not also apply ffmpeg minterpolate here.
        SlowMotionInterp::Ai => return String::new(),
        SlowMotionInterp::Off => unreachable!(),
    };
    format!(",minterpolate=fps={fps}:mi_mode={mi_mode}")
}

pub(crate) fn prerender_build_mask_alpha(
    clip: &ProgramClip,
    out_w: u32,
    out_h: u32,
) -> Option<crate::media::mask_alpha::FfmpegMaskAlphaResult> {
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

pub(crate) fn prerender_append_mask_filter(
    nodes: &mut Vec<String>,
    input_label: &str,
    output_label: &str,
    clip: &ProgramClip,
    out_w: u32,
    out_h: u32,
    mask_temp_files: &mut Vec<tempfile::NamedTempFile>,
) -> bool {
    match prerender_build_mask_alpha(clip, out_w, out_h) {
        Some(crate::media::mask_alpha::FfmpegMaskAlphaResult::GeqExpression(expr)) => {
            nodes.push(format!(
                "[{input_label}]geq=lum='lum(X,Y)':cb='cb(X,Y)':cr='cr(X,Y)':a='alpha(X,Y)*({expr})'[{output_label}]"
            ));
            true
        }
        Some(crate::media::mask_alpha::FfmpegMaskAlphaResult::RasterFile(mask_file)) => {
            let mask_path = mask_file
                .path()
                .display()
                .to_string()
                .replace('\\', "\\\\")
                .replace(':', "\\:");
            let mask_label = format!("{output_label}_mask");
            nodes.push(format!(
                "movie='{mask_path}',format=gray,scale={out_w}:{out_h}[{mask_label}]"
            ));
            nodes.push(format!(
                "[{input_label}][{mask_label}]alphamerge[{output_label}]"
            ));
            mask_temp_files.push(mask_file);
            true
        }
        None => false,
    }
}

pub(crate) fn apply_export_tonal_parity_gains(
    shadows: f64,
    midtones: f64,
    highlights: f64,
    shadows_pos_gain: f64,
    midtones_neg_gain: f64,
    highlights_neg_gain: f64,
) -> (f64, f64, f64) {
    crate::media::color_math::apply_export_tonal_parity_gains(
        shadows,
        midtones,
        highlights,
        shadows_pos_gain,
        midtones_neg_gain,
        highlights_neg_gain,
    )
}
