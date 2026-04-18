//! MCP (Model Context Protocol) command dispatcher.
//!
//! This module contains the `handle_mcp_command` function, which was extracted
//! from `window.rs` to reduce its size. It dispatches incoming MCP commands
//! to the appropriate project/timeline/media/cache operations.

use crate::media::player::Player;
use crate::media::program_player::ProgramPlayer;
use crate::model::clip::{ClipKind, Phase1KeyframeProperty};
use crate::model::media_library::{
    MediaCollection, MediaItem, MediaKeywordRange, MediaLibrary, MediaRating,
};
use crate::model::project::Project;
use crate::model::transition::{
    supported_transition_kinds, validate_track_transition_request, TransitionAlignment,
};
use crate::ui::timecode;
use crate::ui::timeline::TimelineState;
use crate::ui::window::{
    cleanup_project_health_cache, current_project_health_snapshot, AutoCropOutcome,
    PreparedLtcConversion, AUTO_CROP_DEFAULT_PADDING,
};
use crate::undo::TrackClipsChange;
use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::rc::Rc;

use gtk4 as gtk;
use gtk4::prelude::*;

thread_local! {
    static MCP_LOADED_SCRIPT: RefCell<Option<crate::media::script::Script>> = RefCell::new(None);
    static MCP_ALIGNMENT_RESULT: RefCell<Option<crate::media::script_align::AlignmentResult>> = RefCell::new(None);
}

fn parse_project_health_cache_kind(
    cache: &str,
) -> Option<crate::media::project_health::ProjectHealthPathKind> {
    use crate::media::project_health::ProjectHealthPathKind;

    match cache {
        "proxy_local" => Some(ProjectHealthPathKind::ProxyLocal),
        "proxy_sidecars" => Some(ProjectHealthPathKind::ProxySidecars),
        "prerender" => Some(ProjectHealthPathKind::Prerender),
        "background_removal" => Some(ProjectHealthPathKind::BackgroundRemoval),
        "frame_interpolation" => Some(ProjectHealthPathKind::FrameInterpolation),
        "voice_enhancement" => Some(ProjectHealthPathKind::VoiceEnhancement),
        "clip_embeddings" => Some(ProjectHealthPathKind::ClipEmbeddings),
        "auto_tags" => Some(ProjectHealthPathKind::AutoTags),
        _ => None,
    }
}

pub(crate) fn handle_mcp_command(
    cmd: crate::mcp::McpCommand,
    window: &gtk::ApplicationWindow,
    main_stack: &gtk::Stack,
    project: &Rc<RefCell<Project>>,
    library: &Rc<RefCell<MediaLibrary>>,
    player: &Rc<RefCell<Player>>,
    prog_player: &Rc<RefCell<ProgramPlayer>>,
    timeline_state: &Rc<RefCell<TimelineState>>,
    preferences_state: &Rc<RefCell<crate::ui_state::PreferencesState>>,
    workspace_layouts_state: &Rc<RefCell<crate::ui_state::WorkspaceLayoutsState>>,
    proxy_cache: &Rc<RefCell<crate::media::proxy_cache::ProxyCache>>,
    bg_removal_cache: &Rc<RefCell<crate::media::bg_removal_cache::BgRemovalCache>>,
    frame_interp_cache: &Rc<RefCell<crate::media::frame_interp_cache::FrameInterpCache>>,
    voice_enhance_cache: &Rc<RefCell<crate::media::voice_enhance_cache::VoiceEnhanceCache>>,
    clip_embedding_cache: &Rc<RefCell<crate::media::clip_embedding_cache::ClipEmbeddingCache>>,
    auto_tag_cache: &Rc<RefCell<crate::media::auto_tag_cache::AutoTagCache>>,
    stt_cache: &Rc<RefCell<crate::media::stt_cache::SttCache>>,
    music_gen_cache: &Rc<RefCell<crate::media::music_gen::MusicGenCache>>,
    tracking_cache: &Rc<RefCell<crate::media::tracking::TrackingCache>>,
    tracking_job_owner_by_key: &Rc<RefCell<HashMap<String, String>>>,
    tracking_job_key_by_clip: &Rc<RefCell<HashMap<String, String>>>,
    on_close_preview: &Rc<dyn Fn()>,
    source_marks: &Rc<RefCell<crate::model::media_library::SourceMarks>>,
    on_source_selected: &Rc<dyn Fn(String, u64)>,
    on_project_changed: &Rc<dyn Fn()>,
    on_project_changed_full: &Rc<dyn Fn()>,
    capture_workspace_arrangement: &Rc<dyn Fn() -> crate::ui_state::WorkspaceArrangement>,
    apply_workspace_arrangement: &Rc<dyn Fn(crate::ui_state::WorkspaceArrangement)>,
    workspace_layout_pending_name: &Rc<RefCell<Option<String>>>,
    sync_workspace_layout_controls: &Rc<dyn Fn()>,
    apply_preferences_state: &Rc<dyn Fn(crate::ui_state::PreferencesState)>,
    suppress_resume_on_next_reload: &Rc<Cell<bool>>,
    clear_media_browser_on_next_reload: &Rc<Cell<bool>>,
) {
    use crate::mcp::McpCommand;
    use serde_json::{json, Value};

    let sync_library_change = || {
        {
            let lib = library.borrow();
            let mut proj = project.borrow_mut();
            crate::model::media_library::sync_bins_to_project(&lib, &mut proj);
            proj.dirty = true;
        }
        on_project_changed_full();
    };

    match cmd {
        McpCommand::GetProject { reply } => {
            let proj = project.borrow();
            let v = serde_json::to_value(&*proj).unwrap_or(json!(null));
            reply.send(v).ok();
        }

        McpCommand::ListTracks { compact, reply } => {
            let proj = project.borrow();
            let tracks: Vec<_> = proj
                .tracks
                .iter()
                .enumerate()
                .map(|(i, t)| {
                    if compact {
                        json!({
                            "index":      i,
                            "id":         t.id,
                            "kind":       format!("{:?}", t.kind),
                            "clip_count": t.clips.len(),
                        })
                    } else {
                        json!({
                            "index":      i,
                            "id":         t.id,
                            "label":      t.label,
                            "kind":       format!("{:?}", t.kind),
                            "clip_count": t.clips.len(),
                            "muted":      t.muted,
                            "locked":     t.locked,
                            "soloed":     t.soloed,
                            "height_preset": match t.height_preset {
                                crate::model::track::TrackHeightPreset::Small => "small",
                                crate::model::track::TrackHeightPreset::Medium => "medium",
                                crate::model::track::TrackHeightPreset::Large => "large",
                            },
                        })
                    }
                })
                .collect();
            reply.send(json!(tracks)).ok();
        }

        McpCommand::ListClips { compact, reply } => {
            let proj = project.borrow();
            let clips: Vec<_> = proj
                .tracks
                .iter()
                .enumerate()
                .flat_map(|(ti, track)| {
                    track.clips.iter().map(move |c| {
                        if compact {
                            json!({
                                "id":               c.id,
                                "source_path":      c.source_path,
                                "track_index":      ti,
                                "track_id":         track.id,
                                "timeline_start_ns": c.timeline_start,
                                "source_in_ns":     c.source_in,
                                "source_out_ns":    c.source_out,
                                "duration_ns":      c.duration(),
                            })
                        } else {
                            json!({
                                "id":               c.id,
                                "label":            c.label,
                                "source_path":      c.source_path,
                                "track_index":      ti,
                                "track_id":         track.id,
                                "group_id":         c.group_id,
                                "link_group_id":    c.link_group_id,
                                "source_timecode_base_ns": c.source_timecode_base_ns,
                                "source_timecode_start_ns": c.source_timecode_start_ns(),
                                "timeline_start_ns": c.timeline_start,
                                "source_in_ns":     c.source_in,
                                "source_out_ns":    c.source_out,
                                "duration_ns":      c.duration(),
                                "color_label":      match c.color_label {
                                    crate::model::clip::ClipColorLabel::None => "none",
                                    crate::model::clip::ClipColorLabel::Red => "red",
                                    crate::model::clip::ClipColorLabel::Orange => "orange",
                                    crate::model::clip::ClipColorLabel::Yellow => "yellow",
                                    crate::model::clip::ClipColorLabel::Green => "green",
                                    crate::model::clip::ClipColorLabel::Teal => "teal",
                                    crate::model::clip::ClipColorLabel::Blue => "blue",
                                    crate::model::clip::ClipColorLabel::Purple => "purple",
                                    crate::model::clip::ClipColorLabel::Magenta => "magenta",
                                },
                                "brightness":       c.brightness,
                                "contrast":         c.contrast,
                                "saturation":       c.saturation,
                                "temperature":      c.temperature,
                                "tint":             c.tint,
                                "denoise":          c.denoise,
                                "sharpness":        c.sharpness,
                                "shadows":          c.shadows,
                                "midtones":         c.midtones,
                                "highlights":       c.highlights,
                                "exposure":         c.exposure,
                                "black_point":      c.black_point,
                                "highlights_warmth": c.highlights_warmth,
                                "highlights_tint":  c.highlights_tint,
                                "midtones_warmth":  c.midtones_warmth,
                                "midtones_tint":    c.midtones_tint,
                                "shadows_warmth":   c.shadows_warmth,
                                "shadows_tint":     c.shadows_tint,
                                "volume":           c.volume,
                                "pan":              c.pan,
                                "scale":            c.scale,
                                "anamorphic_desqueeze": c.anamorphic_desqueeze,
                                "opacity":          c.opacity,
                                "blend_mode":       c.blend_mode.label(),
                                "position_x":       c.position_x,
                                "position_y":       c.position_y,
                                "speed":            c.speed,
                                "scale_keyframes":      c.scale_keyframes,
                                "opacity_keyframes":    c.opacity_keyframes,
                                "brightness_keyframes": c.brightness_keyframes,
                                "contrast_keyframes":   c.contrast_keyframes,
                                "saturation_keyframes": c.saturation_keyframes,
                                "temperature_keyframes": c.temperature_keyframes,
                                "tint_keyframes":       c.tint_keyframes,
                                "position_x_keyframes": c.position_x_keyframes,
                                "position_y_keyframes": c.position_y_keyframes,
                                "volume_keyframes":     c.volume_keyframes,
                                "pan_keyframes":        c.pan_keyframes,
                                "speed_keyframes":      c.speed_keyframes,
                                "rotate_keyframes":     c.rotate_keyframes,
                                "crop_left_keyframes":  c.crop_left_keyframes,
                                "crop_right_keyframes": c.crop_right_keyframes,
                                "crop_top_keyframes":   c.crop_top_keyframes,
                                "crop_bottom_keyframes": c.crop_bottom_keyframes,
                                "bg_removal_enabled":   c.bg_removal_enabled,
                                "bg_removal_threshold": c.bg_removal_threshold,
                            })
                        }
                    })
                })
                .collect();
            reply.send(json!(clips)).ok();
        }

        McpCommand::GetTimelineSettings { reply } => {
            let magnetic_mode = timeline_state.borrow().magnetic_mode;
            reply
                .send(json!({
                    "magnetic_mode": magnetic_mode
                }))
                .ok();
        }

        McpCommand::GetPlayheadPosition { reply } => {
            let timeline_pos_ns = prog_player.borrow().timeline_pos_ns;
            reply
                .send(json!({
                    "timeline_pos_ns": timeline_pos_ns
                }))
                .ok();
        }

        McpCommand::GetPerformanceSnapshot { reply } => {
            let snapshot = prog_player.borrow().performance_snapshot();
            let transition_metrics: Vec<_> = snapshot
                .transition_metrics
                .iter()
                .map(|metric| {
                    json!({
                        "kind": metric.kind.clone(),
                        "hit": metric.hit,
                        "miss": metric.miss,
                        "hit_rate_percent": metric.hit_rate_percent
                    })
                })
                .collect();
            reply
                .send(json!({
                    "player_state": snapshot.player_state,
                    "playback_priority": snapshot.playback_priority,
                    "timeline_pos_ns": snapshot.timeline_pos_ns,
                    "background_prerender_enabled": snapshot.background_prerender_enabled,
                    "prerender_total_requested": snapshot.prerender_total_requested,
                    "prerender_pending": snapshot.prerender_pending,
                    "prerender_ready": snapshot.prerender_ready,
                    "prerender_failed": snapshot.prerender_failed,
                    "prerender_cache_hits": snapshot.prerender_cache_hits,
                    "prerender_cache_misses": snapshot.prerender_cache_misses,
                    "prerender_cache_hit_rate_percent": snapshot.prerender_cache_hit_rate_percent,
                    "prewarmed_boundary_ns": snapshot.prewarmed_boundary_ns,
                    "active_prerender_segment_key": snapshot.active_prerender_segment_key,
                    "rebuild_history_samples": snapshot.rebuild_history_samples,
                    "rebuild_history_recent_ms": snapshot.rebuild_history_recent_ms,
                    "rebuild_latest_ms": snapshot.rebuild_latest_ms,
                    "rebuild_p50_ms": snapshot.rebuild_p50_ms,
                    "rebuild_p75_ms": snapshot.rebuild_p75_ms,
                    "transition_hits_total": snapshot.transition_hits_total,
                    "transition_misses_total": snapshot.transition_misses_total,
                    "transition_hit_rate_percent": snapshot.transition_hit_rate_percent,
                    "transition_low_hitrate_pressure": snapshot.transition_low_hitrate_pressure,
                    "prerender_queue_pressure": snapshot.prerender_queue_pressure,
                    "transition_metrics": transition_metrics
                }))
                .ok();
        }

        McpCommand::SetMagneticMode { enabled, reply } => {
            timeline_state.borrow_mut().magnetic_mode = enabled;
            reply
                .send(json!({"success": true, "magnetic_mode": enabled}))
                .ok();
            on_project_changed();
        }

        McpCommand::SetTrackSolo {
            track_id,
            solo,
            reply,
        } => {
            let result = {
                let mut proj = project.borrow_mut();
                if let Some(track) = proj.track_mut(&track_id) {
                    track.soloed = solo;
                    proj.dirty = true;
                    Ok(())
                } else {
                    Err(format!("Track id not found: {track_id}"))
                }
            };
            match result {
                Ok(()) => {
                    reply
                        .send(json!({"success": true, "track_id": track_id, "soloed": solo}))
                        .ok();
                    on_project_changed();
                }
                Err(message) => {
                    reply.send(json!({"success": false, "error": message})).ok();
                }
            }
        }

        McpCommand::ListLadspaPlugins { reply } => {
            let reg = crate::media::ladspa_registry::LadspaRegistry::get_or_discover();
            let plugins: Vec<serde_json::Value> = reg
                .plugins
                .iter()
                .map(|p| {
                    serde_json::json!({
                        "name": p.ladspa_name,
                        "display_name": p.display_name,
                        "description": p.description,
                        "category": p.category,
                        "gst_element_name": p.gst_element_name,
                        "params": p.params.iter().map(|param| serde_json::json!({
                            "name": param.name,
                            "display_name": param.display_name,
                            "default": param.default_value,
                            "min": param.min,
                            "max": param.max,
                        })).collect::<Vec<_>>(),
                    })
                })
                .collect();
            reply
                .send(serde_json::json!({"plugins": plugins, "count": plugins.len()}))
                .ok();
        }

        McpCommand::AddClipLadspaEffect {
            clip_id,
            plugin_name,
            reply,
        } => {
            let reg = crate::media::ladspa_registry::LadspaRegistry::get_or_discover();
            if let Some(info) = reg.find_by_name(&plugin_name) {
                let effect = crate::model::clip::LadspaEffect::new(
                    &info.ladspa_name,
                    &info.gst_element_name,
                );
                let effect_id = effect.id.clone();
                let mut proj = project.borrow_mut();
                let found = if let Some(clip) = proj.clip_mut(&clip_id) {
                    clip.ladspa_effects.push(effect);
                    true
                } else {
                    false
                };
                if found {
                    proj.dirty = true;
                }
                drop(proj);
                reply
                    .send(serde_json::json!({"success": found, "effect_id": effect_id}))
                    .ok();
                if found {
                    on_project_changed();
                }
            } else {
                reply
                    .send(serde_json::json!({"success": false, "error": format!("Plugin not found: {plugin_name}")}))
                    .ok();
            }
        }

        McpCommand::RemoveClipLadspaEffect {
            clip_id,
            effect_id,
            reply,
        } => {
            let mut proj = project.borrow_mut();
            let found = if let Some(clip) = proj.clip_mut(&clip_id) {
                let before = clip.ladspa_effects.len();
                clip.ladspa_effects.retain(|e| e.id != effect_id);
                clip.ladspa_effects.len() < before
            } else {
                false
            };
            if found {
                proj.dirty = true;
            }
            drop(proj);
            reply.send(serde_json::json!({"success": found})).ok();
            if found {
                on_project_changed();
            }
        }

        McpCommand::SetClipLadspaEffectParams {
            clip_id,
            effect_id,
            params,
            reply,
        } => {
            let mut proj = project.borrow_mut();
            let found = if let Some(clip) = proj.clip_mut(&clip_id) {
                if let Some(effect) = clip.ladspa_effects.iter_mut().find(|e| e.id == effect_id) {
                    for (k, v) in &params {
                        effect.params.insert(k.clone(), *v);
                    }
                    true
                } else {
                    false
                }
            } else {
                false
            };
            if found {
                proj.dirty = true;
            }
            drop(proj);
            reply.send(serde_json::json!({"success": found})).ok();
            if found {
                on_project_changed();
            }
        }

        McpCommand::SetTrackRole {
            track_id,
            role,
            reply,
        } => {
            let result = {
                let mut proj = project.borrow_mut();
                if let Some(track) = proj.track_mut(&track_id) {
                    let new_role = crate::model::track::AudioRole::from_str(&role);
                    track.audio_role = new_role;
                    let role_str = new_role.as_str().to_string();
                    drop(track);
                    proj.dirty = true;
                    Ok(role_str)
                } else {
                    Err(format!("Track id not found: {track_id}"))
                }
            };
            match result {
                Ok(applied_role) => {
                    reply
                        .send(serde_json::json!({"success": true, "track_id": track_id, "role": applied_role}))
                        .ok();
                    on_project_changed();
                }
                Err(message) => {
                    reply
                        .send(serde_json::json!({"success": false, "error": message}))
                        .ok();
                }
            }
        }

        McpCommand::SetTrackDuck {
            track_id,
            duck,
            reply,
        } => {
            let result = {
                let mut proj = project.borrow_mut();
                if let Some(track) = proj.track_mut(&track_id) {
                    track.duck = duck;
                    proj.dirty = true;
                    Ok(())
                } else {
                    Err(format!("Track id not found: {track_id}"))
                }
            };
            match result {
                Ok(()) => {
                    reply
                        .send(serde_json::json!({"success": true, "track_id": track_id, "duck": duck}))
                        .ok();
                    on_project_changed();
                }
                Err(message) => {
                    reply
                        .send(serde_json::json!({"success": false, "error": message}))
                        .ok();
                }
            }
        }

        McpCommand::SetTrackMuted {
            track_id,
            muted,
            reply,
        } => {
            let result = {
                let mut proj = project.borrow_mut();
                if let Some(track) = proj.track_mut(&track_id) {
                    track.muted = muted;
                    proj.dirty = true;
                    Ok(())
                } else {
                    Err(format!("Track id not found: {track_id}"))
                }
            };
            match result {
                Ok(()) => {
                    reply
                        .send(json!({"success": true, "track_id": track_id, "muted": muted}))
                        .ok();
                    on_project_changed();
                }
                Err(message) => {
                    reply.send(json!({"success": false, "error": message})).ok();
                }
            }
        }

        McpCommand::SetTrackGain {
            track_id,
            gain_db,
            reply,
        } => {
            let result = {
                let mut proj = project.borrow_mut();
                if let Some(track) = proj.track_mut(&track_id) {
                    track.gain_db = gain_db;
                    proj.dirty = true;
                    Ok(())
                } else {
                    Err(format!("Track id not found: {track_id}"))
                }
            };
            match result {
                Ok(()) => {
                    reply
                        .send(json!({"success": true, "track_id": track_id, "gain_db": gain_db}))
                        .ok();
                    on_project_changed();
                }
                Err(message) => {
                    reply.send(json!({"success": false, "error": message})).ok();
                }
            }
        }

        McpCommand::SetTrackPan {
            track_id,
            pan,
            reply,
        } => {
            let result = {
                let mut proj = project.borrow_mut();
                if let Some(track) = proj.track_mut(&track_id) {
                    track.pan = pan.clamp(-1.0, 1.0);
                    proj.dirty = true;
                    Ok(())
                } else {
                    Err(format!("Track id not found: {track_id}"))
                }
            };
            match result {
                Ok(()) => {
                    reply
                        .send(json!({"success": true, "track_id": track_id, "pan": pan}))
                        .ok();
                    on_project_changed();
                }
                Err(message) => {
                    reply.send(json!({"success": false, "error": message})).ok();
                }
            }
        }

        McpCommand::GetMixerState { reply } => {
            let proj = project.borrow();
            let tracks: Vec<serde_json::Value> = proj
                .tracks
                .iter()
                .map(|t| {
                    json!({
                        "track_id": t.id,
                        "label": t.label,
                        "kind": if t.is_video() { "video" } else { "audio" },
                        "gain_db": t.gain_db,
                        "pan": t.pan,
                        "muted": t.muted,
                        "soloed": t.soloed,
                        "audio_role": t.audio_role.label(),
                    })
                })
                .collect();
            let buses = json!({
                "dialogue": {
                    "gain_db": proj.dialogue_bus.gain_db,
                    "muted": proj.dialogue_bus.muted,
                    "soloed": proj.dialogue_bus.soloed,
                },
                "effects": {
                    "gain_db": proj.effects_bus.gain_db,
                    "muted": proj.effects_bus.muted,
                    "soloed": proj.effects_bus.soloed,
                },
                "music": {
                    "gain_db": proj.music_bus.gain_db,
                    "muted": proj.music_bus.muted,
                    "soloed": proj.music_bus.soloed,
                },
            });
            reply
                .send(json!({
                    "master_gain_db": proj.master_gain_db,
                    "tracks": tracks,
                    "buses": buses,
                }))
                .ok();
        }

        McpCommand::SetBusGain {
            role,
            gain_db,
            reply,
        } => {
            let result = {
                let mut proj = project.borrow_mut();
                match parse_audio_role(&role) {
                    Some(r) => {
                        if let Some(bus) = proj.bus_for_role_mut(&r) {
                            bus.gain_db = gain_db.clamp(-96.0, 24.0);
                            proj.dirty = true;
                            Ok(())
                        } else {
                            Err(format!("No bus for role: {role}"))
                        }
                    }
                    None => Err(format!(
                        "Unknown role: {role}. Use 'Dialogue', 'Effects', or 'Music'."
                    )),
                }
            };
            match result {
                Ok(()) => {
                    reply
                        .send(json!({"success": true, "role": role, "gain_db": gain_db}))
                        .ok();
                    on_project_changed();
                }
                Err(message) => {
                    reply.send(json!({"success": false, "error": message})).ok();
                }
            }
        }

        McpCommand::SetBusMuted { role, muted, reply } => {
            let result = {
                let mut proj = project.borrow_mut();
                match parse_audio_role(&role) {
                    Some(r) => {
                        if let Some(bus) = proj.bus_for_role_mut(&r) {
                            bus.muted = muted;
                            proj.dirty = true;
                            Ok(())
                        } else {
                            Err(format!("No bus for role: {role}"))
                        }
                    }
                    None => Err(format!(
                        "Unknown role: {role}. Use 'Dialogue', 'Effects', or 'Music'."
                    )),
                }
            };
            match result {
                Ok(()) => {
                    reply
                        .send(json!({"success": true, "role": role, "muted": muted}))
                        .ok();
                    on_project_changed();
                }
                Err(message) => {
                    reply.send(json!({"success": false, "error": message})).ok();
                }
            }
        }

        McpCommand::SetBusSoloed {
            role,
            soloed,
            reply,
        } => {
            let result = {
                let mut proj = project.borrow_mut();
                match parse_audio_role(&role) {
                    Some(r) => {
                        if let Some(bus) = proj.bus_for_role_mut(&r) {
                            bus.soloed = soloed;
                            proj.dirty = true;
                            Ok(())
                        } else {
                            Err(format!("No bus for role: {role}"))
                        }
                    }
                    None => Err(format!(
                        "Unknown role: {role}. Use 'Dialogue', 'Effects', or 'Music'."
                    )),
                }
            };
            match result {
                Ok(()) => {
                    reply
                        .send(json!({"success": true, "role": role, "soloed": soloed}))
                        .ok();
                    on_project_changed();
                }
                Err(message) => {
                    reply.send(json!({"success": false, "error": message})).ok();
                }
            }
        }

        McpCommand::SetTrackLocked {
            track_id,
            locked,
            reply,
        } => {
            let result = {
                let mut proj = project.borrow_mut();
                if let Some(track) = proj.track_mut(&track_id) {
                    track.locked = locked;
                    proj.dirty = true;
                    Ok(())
                } else {
                    Err(format!("Track id not found: {track_id}"))
                }
            };
            match result {
                Ok(()) => {
                    reply
                        .send(json!({"success": true, "track_id": track_id, "locked": locked}))
                        .ok();
                    on_project_changed();
                }
                Err(message) => {
                    reply.send(json!({"success": false, "error": message})).ok();
                }
            }
        }

        McpCommand::SetTrackColor {
            track_id,
            color,
            reply,
        } => {
            let parsed = crate::model::track::TrackColorLabel::from_str(&color);
            let result = {
                let mut proj = project.borrow_mut();
                if let Some(track) = proj.track_mut(&track_id) {
                    track.color_label = parsed;
                    proj.dirty = true;
                    Ok(())
                } else {
                    Err(format!("Track id not found: {track_id}"))
                }
            };
            match result {
                Ok(()) => {
                    reply
                        .send(json!({"success": true, "track_id": track_id, "color": color}))
                        .ok();
                    on_project_changed();
                }
                Err(message) => {
                    reply.send(json!({"success": false, "error": message})).ok();
                }
            }
        }

        McpCommand::SetTrackHeightPreset {
            track_id,
            height_preset,
            reply,
        } => {
            let parsed = match height_preset.as_str() {
                "small" => Some(crate::model::track::TrackHeightPreset::Small),
                "medium" => Some(crate::model::track::TrackHeightPreset::Medium),
                "large" => Some(crate::model::track::TrackHeightPreset::Large),
                _ => None,
            };
            let Some(parsed) = parsed else {
                reply
                    .send(json!({"success": false, "error": "height_preset must be one of: small, medium, large"}))
                    .ok();
                return;
            };
            let result = {
                let mut proj = project.borrow_mut();
                if let Some(track) = proj.track_mut(&track_id) {
                    track.height_preset = parsed;
                    proj.dirty = true;
                    Ok(())
                } else {
                    Err(format!("Track id not found: {track_id}"))
                }
            };
            match result {
                Ok(()) => {
                    reply
                        .send(json!({"success": true, "track_id": track_id, "height_preset": height_preset}))
                        .ok();
                    on_project_changed();
                }
                Err(message) => {
                    reply.send(json!({"success": false, "error": message})).ok();
                }
            }
        }

        McpCommand::CloseSourcePreview { reply } => {
            on_close_preview();
            reply.send(json!({"success": true})).ok();
        }

        McpCommand::GetPreferences { reply } => {
            let prefs = preferences_state.borrow().clone();
            reply
                .send(json!({
                    "hardware_acceleration_enabled": prefs.hardware_acceleration_enabled,
                    "playback_priority": prefs.playback_priority.as_str(),
                    "source_playback_priority": prefs.source_playback_priority.as_str(),
                    "proxy_mode": prefs.proxy_mode.as_str(),
                    "last_non_off_proxy_mode": prefs.remembered_proxy_mode().as_str(),
                    "persist_proxies_next_to_original_media": prefs.persist_proxies_next_to_original_media,
                    "show_timeline_preview": prefs.show_timeline_preview,
                    "timeline_autoscroll": prefs.timeline_autoscroll.as_str(),
                    "show_track_audio_levels": prefs.show_track_audio_levels,
                    "gsk_renderer": prefs.gsk_renderer.as_str(),
                    "preview_quality": prefs.preview_quality.as_str(),
                    "experimental_preview_optimizations": prefs.experimental_preview_optimizations,
                    "realtime_preview": prefs.realtime_preview,
                    "background_prerender": prefs.background_prerender,
                    "background_ai_indexing": prefs.background_ai_indexing,
                    "background_auto_tagging": prefs.background_auto_tagging,
                    "prerender_preset": prefs.prerender_preset.as_str(),
                    "prerender_crf": prefs.prerender_crf,
                    "persist_prerenders_next_to_project_file": prefs.persist_prerenders_next_to_project_file,
                    "preview_luts": prefs.preview_luts,
                    "crossfade_enabled": prefs.crossfade_enabled,
                    "crossfade_curve": prefs.crossfade_curve.as_str(),
                    "crossfade_duration_ns": prefs.crossfade_duration_ns
                }))
                .ok();
        }

        McpCommand::GetProjectHealth { reply } => {
            let snapshot = {
                let proj = project.borrow();
                let lib = library.borrow();
                let prog = prog_player.borrow();
                current_project_health_snapshot(&proj, &lib, &prog)
            };
            reply
                .send(serde_json::to_value(snapshot).unwrap_or(json!(null)))
                .ok();
        }

        McpCommand::CleanupProjectCache { cache, reply } => {
            let Some(kind) = parse_project_health_cache_kind(&cache) else {
                reply.send(json!({
                    "success": false,
                    "error": "cache must be one of: proxy_local, proxy_sidecars, prerender, background_removal, frame_interpolation, voice_enhancement, clip_embeddings, auto_tags"
                })).ok();
                return;
            };
            match cleanup_project_health_cache(
                kind,
                &project.borrow(),
                &library.borrow(),
                proxy_cache,
                bg_removal_cache,
                frame_interp_cache,
                voice_enhance_cache,
                clip_embedding_cache,
                auto_tag_cache,
                prog_player,
            ) {
                Ok(message) => {
                    reply
                        .send(json!({"success": true, "cache": cache, "message": message}))
                        .ok();
                }
                Err(message) => {
                    reply.send(json!({"success": false, "error": message})).ok();
                }
            }
        }

        McpCommand::SetHardwareAcceleration { enabled, reply } => {
            let new_state = {
                let mut prefs = preferences_state.borrow_mut();
                prefs.hardware_acceleration_enabled = enabled;
                prefs.clone()
            };
            crate::ui_state::save_preferences_state(&new_state);
            match player.borrow().set_hardware_acceleration(enabled) {
                Ok(()) => {
                    reply
                        .send(json!({
                            "success": true,
                            "hardware_acceleration_enabled": enabled,
                            "playback_priority": new_state.playback_priority.as_str(),
                            "source_playback_priority": new_state.source_playback_priority.as_str()
                        }))
                        .ok();
                }
                Err(e) => {
                    reply
                        .send(json!({
                            "success": false,
                            "hardware_acceleration_enabled": enabled,
                            "playback_priority": new_state.playback_priority.as_str(),
                            "source_playback_priority": new_state.source_playback_priority.as_str(),
                            "error": e.to_string()
                        }))
                        .ok();
                }
            }
        }

        McpCommand::SetPlaybackPriority { priority, reply } => {
            let parsed = crate::ui_state::PlaybackPriority::from_str(&priority);
            let new_state = {
                let mut prefs = preferences_state.borrow_mut();
                prefs.playback_priority = parsed.clone();
                prefs.clone()
            };
            crate::ui_state::save_preferences_state(&new_state);
            prog_player.borrow_mut().set_playback_priority(parsed);
            reply
                .send(json!({
                    "success": true,
                    "playback_priority": new_state.playback_priority.as_str()
                }))
                .ok();
        }

        McpCommand::SetSourcePlaybackPriority { priority, reply } => {
            let parsed = crate::ui_state::PlaybackPriority::from_str(&priority);
            let new_state = {
                let mut prefs = preferences_state.borrow_mut();
                prefs.source_playback_priority = parsed.clone();
                prefs.clone()
            };
            crate::ui_state::save_preferences_state(&new_state);
            player.borrow().set_source_playback_priority(parsed.clone());
            reply
                .send(json!({
                    "success": true,
                    "source_playback_priority": new_state.source_playback_priority.as_str()
                }))
                .ok();
        }

        McpCommand::SetCrossfadeSettings {
            enabled,
            curve,
            duration_ns,
            reply,
        } => {
            const MIN_CROSSFADE_DURATION_NS: u64 = 10_000_000;
            const MAX_CROSSFADE_DURATION_NS: u64 = 10_000_000_000;
            let parsed_curve = match curve.as_str() {
                "equal_power" => crate::ui_state::CrossfadeCurve::EqualPower,
                "linear" => crate::ui_state::CrossfadeCurve::Linear,
                _ => {
                    reply
                        .send(json!({"success": false, "error": "curve must be one of: equal_power, linear"}))
                        .ok();
                    return;
                }
            };
            if !(MIN_CROSSFADE_DURATION_NS..=MAX_CROSSFADE_DURATION_NS).contains(&duration_ns) {
                reply
                    .send(json!({
                        "success": false,
                        "error": "duration_ns must be between 10_000_000 and 10_000_000_000"
                    }))
                    .ok();
                return;
            }
            let new_state = {
                let mut prefs = preferences_state.borrow_mut();
                prefs.crossfade_enabled = enabled;
                prefs.crossfade_curve = parsed_curve;
                prefs.crossfade_duration_ns = duration_ns;
                prefs.clone()
            };
            crate::ui_state::save_preferences_state(&new_state);
            prog_player.borrow_mut().set_audio_crossfade_preview(
                new_state.crossfade_enabled,
                new_state.crossfade_curve.clone(),
                new_state.crossfade_duration_ns,
            );
            reply
                .send(json!({
                    "success": true,
                    "crossfade_enabled": new_state.crossfade_enabled,
                    "crossfade_curve": new_state.crossfade_curve.as_str(),
                    "crossfade_duration_ns": new_state.crossfade_duration_ns
                }))
                .ok();
        }

        McpCommand::SetProxyMode { mode, reply } => {
            let parsed = crate::ui_state::ProxyMode::from_str(&mode);
            let mut new_state = preferences_state.borrow().clone();
            new_state.set_proxy_mode(parsed);
            apply_preferences_state(new_state.clone());
            reply
                .send(json!({
                    "success": true,
                    "proxy_mode": new_state.proxy_mode.as_str(),
                    "last_non_off_proxy_mode": new_state.remembered_proxy_mode().as_str()
                }))
                .ok();
        }

        McpCommand::SetProxySidecarPersistence { enabled, reply } => {
            let mut new_state = preferences_state.borrow().clone();
            new_state.persist_proxies_next_to_original_media = enabled;
            apply_preferences_state(new_state.clone());
            reply
                .send(json!({
                    "success": true,
                    "persist_proxies_next_to_original_media": enabled
                }))
                .ok();
        }

        McpCommand::SetGskRenderer { renderer, reply } => {
            let parsed = crate::ui_state::GskRenderer::from_str(&renderer);
            let new_state = {
                let mut prefs = preferences_state.borrow_mut();
                prefs.gsk_renderer = parsed;
                prefs.clone()
            };
            crate::ui_state::save_preferences_state(&new_state);
            reply
                .send(json!({
                    "success": true,
                    "gsk_renderer": new_state.gsk_renderer.as_str(),
                    "note": "Restart the application for the new renderer to take effect."
                }))
                .ok();
        }

        McpCommand::SetPreviewQuality { quality, reply } => {
            let parsed = crate::ui_state::PreviewQuality::from_str(&quality);
            prog_player
                .borrow_mut()
                .set_preview_quality(parsed.divisor());
            let new_state = {
                let mut prefs = preferences_state.borrow_mut();
                prefs.preview_quality = parsed;
                prefs.clone()
            };
            crate::ui_state::save_preferences_state(&new_state);
            reply
                .send(json!({
                    "success": true,
                    "preview_quality": new_state.preview_quality.as_str()
                }))
                .ok();
        }

        McpCommand::SetRealtimePreview { enabled, reply } => {
            prog_player.borrow_mut().set_realtime_preview(enabled);
            let new_state = {
                let mut prefs = preferences_state.borrow_mut();
                prefs.realtime_preview = enabled;
                prefs.clone()
            };
            crate::ui_state::save_preferences_state(&new_state);
            reply
                .send(json!({
                    "success": true,
                    "realtime_preview": enabled
                }))
                .ok();
        }

        McpCommand::SetExperimentalPreviewOptimizations { enabled, reply } => {
            prog_player
                .borrow_mut()
                .set_experimental_preview_optimizations(enabled);
            let new_state = {
                let mut prefs = preferences_state.borrow_mut();
                prefs.experimental_preview_optimizations = enabled;
                prefs.clone()
            };
            crate::ui_state::save_preferences_state(&new_state);
            reply
                .send(json!({
                    "success": true,
                    "experimental_preview_optimizations": enabled
                }))
                .ok();
        }

        McpCommand::SetBackgroundPrerender { enabled, reply } => {
            let mut new_state = preferences_state.borrow().clone();
            new_state.background_prerender = enabled;
            apply_preferences_state(new_state.clone());
            reply
                .send(json!({
                    "success": true,
                    "background_prerender": enabled
                }))
                .ok();
        }

        McpCommand::SetBackgroundAiIndexing { enabled, reply } => {
            let mut new_state = preferences_state.borrow().clone();
            new_state.background_ai_indexing = enabled;
            apply_preferences_state(new_state.clone());
            reply
                .send(json!({
                    "success": true,
                    "background_ai_indexing": enabled
                }))
                .ok();
        }

        McpCommand::SetBackgroundAutoTagging { enabled, reply } => {
            let mut new_state = preferences_state.borrow().clone();
            new_state.background_auto_tagging = enabled;
            apply_preferences_state(new_state.clone());
            reply
                .send(json!({
                    "success": true,
                    "background_auto_tagging": enabled
                }))
                .ok();
        }

        McpCommand::SetPrerenderQuality { preset, crf, reply } => {
            let parsed_preset = match preset.as_str() {
                "ultrafast" => crate::ui_state::PrerenderEncodingPreset::Ultrafast,
                "superfast" => crate::ui_state::PrerenderEncodingPreset::Superfast,
                "veryfast" => crate::ui_state::PrerenderEncodingPreset::Veryfast,
                "faster" => crate::ui_state::PrerenderEncodingPreset::Faster,
                "fast" => crate::ui_state::PrerenderEncodingPreset::Fast,
                "medium" => crate::ui_state::PrerenderEncodingPreset::Medium,
                _ => {
                    reply
                        .send(json!({
                            "success": false,
                            "error": "preset must be one of: ultrafast, superfast, veryfast, faster, fast, medium"
                        }))
                        .ok();
                    return;
                }
            };
            if crf > crate::ui_state::MAX_PRERENDER_CRF {
                reply
                    .send(json!({
                        "success": false,
                        "error": format!(
                            "crf must be between {} and {}",
                            crate::ui_state::MIN_PRERENDER_CRF,
                            crate::ui_state::MAX_PRERENDER_CRF
                        )
                    }))
                    .ok();
                return;
            }
            prog_player
                .borrow_mut()
                .set_prerender_quality(parsed_preset.clone(), crf);
            let new_state = {
                let mut prefs = preferences_state.borrow_mut();
                prefs.set_prerender_quality(parsed_preset, crf);
                prefs.clone()
            };
            crate::ui_state::save_preferences_state(&new_state);
            reply
                .send(json!({
                    "success": true,
                    "prerender_preset": new_state.prerender_preset.as_str(),
                    "prerender_crf": new_state.prerender_crf
                }))
                .ok();
        }

        McpCommand::SetPrerenderProjectPersistence { enabled, reply } => {
            let mut new_state = preferences_state.borrow().clone();
            new_state.persist_prerenders_next_to_project_file = enabled;
            apply_preferences_state(new_state.clone());
            reply
                .send(json!({
                    "success": true,
                    "persist_prerenders_next_to_project_file": enabled
                }))
                .ok();
        }

        McpCommand::SetPreviewLuts { enabled, reply } => {
            let mut new_state = preferences_state.borrow().clone();
            new_state.preview_luts = enabled;
            apply_preferences_state(new_state.clone());
            reply
                .send(json!({
                    "success": true,
                    "preview_luts": enabled
                }))
                .ok();
        }

        McpCommand::AddClip {
            source_path,
            track_index,
            timeline_start_ns,
            source_in_ns,
            source_out_ns,
            reply,
        } => {
            let magnetic_mode = timeline_state.borrow().magnetic_mode;
            let source_monitor_auto_link_av =
                preferences_state.borrow().source_monitor_auto_link_av;
            let source_info = {
                let lib = library.borrow();
                let proj = project.borrow();
                crate::ui::window::lookup_source_placement_info(&lib.items, &proj, &source_path)
            };
            let created = {
                let mut proj = project.borrow_mut();
                let placement_plan = crate::ui::window::build_source_placement_plan_by_track_index(
                    &proj,
                    Some(track_index),
                    source_info,
                    source_monitor_auto_link_av,
                );
                if placement_plan.targets.is_empty() {
                    Err(format!("Track index {track_index} does not exist"))
                } else {
                    let magnetic_mode_for_placement =
                        magnetic_mode && !placement_plan.uses_linked_pair();
                    let mut created_clip_ids: Vec<String> = Vec::new();
                    for (target_track_idx, clip) in crate::ui::window::build_source_clips_for_plan(
                        &placement_plan,
                        &source_path,
                        source_in_ns,
                        source_out_ns,
                        timeline_start_ns,
                        source_info.source_timecode_base_ns,
                        source_info.audio_channel_mode,
                        None,
                        source_info.is_animated_svg,
                    ) {
                        created_clip_ids.push(clip.id.clone());
                        let _ = crate::ui::window::add_clip_to_track(
                            &mut proj.tracks[target_track_idx],
                            clip,
                            magnetic_mode_for_placement,
                        );
                    }
                    proj.dirty = true;
                    Ok((
                        created_clip_ids.first().cloned().unwrap_or_default(),
                        created_clip_ids.into_iter().skip(1).collect::<Vec<_>>(),
                        placement_plan.link_group_id.clone(),
                    ))
                }
            };
            match created {
                Ok((clip_id, linked_clip_ids, link_group_id)) => {
                    reply
                        .send(json!({
                            "success": true,
                            "clip_id": clip_id,
                            "linked_clip_ids": linked_clip_ids,
                            "link_group_id": link_group_id
                        }))
                        .ok();
                    on_project_changed_full();
                }
                Err(e) => {
                    reply.send(json!({"success": false, "error": e})).ok();
                }
            }
        }

        McpCommand::RemoveClip { clip_id, reply } => {
            let magnetic_mode = timeline_state.borrow().magnetic_mode;
            let mut proj = project.borrow_mut();
            let target_ids: HashSet<String> = proj
                .tracks
                .iter()
                .flat_map(|t| t.clips.iter())
                .find(|c| c.id == clip_id)
                .map(|clip| {
                    if let Some(link_group_id) = clip.link_group_id.clone() {
                        proj.tracks
                            .iter()
                            .flat_map(|t| t.clips.iter())
                            .filter(|c| c.link_group_id.as_deref() == Some(link_group_id.as_str()))
                            .map(|c| c.id.clone())
                            .collect()
                    } else {
                        std::iter::once(clip_id.clone()).collect()
                    }
                })
                .unwrap_or_default();
            let mut removed_count = 0usize;
            for track in proj.tracks.iter_mut() {
                let before = track.clips.len();
                track.clips.retain(|c| !target_ids.contains(&c.id));
                if before != track.clips.len() {
                    removed_count += before - track.clips.len();
                    if magnetic_mode {
                        track.compact_gap_free();
                    }
                }
            }
            let found = removed_count > 0;
            if found {
                proj.dirty = true;
            }
            drop(proj);
            reply
                .send(json!({"success": found, "removed_clip_count": removed_count}))
                .ok();
            if found {
                on_project_changed();
            }
        }

        McpCommand::MoveClip {
            clip_id,
            new_start_ns,
            reply,
        } => {
            let magnetic_mode = timeline_state.borrow().magnetic_mode;
            let mut proj = project.borrow_mut();
            let target = proj
                .tracks
                .iter()
                .flat_map(|t| t.clips.iter())
                .find(|c| c.id == clip_id)
                .map(|clip| (clip.timeline_start, clip.link_group_id.clone()));
            let mut moved_count = 0usize;
            if let Some((original_start_ns, link_group_id)) = target {
                if let Some(link_group_id) = link_group_id {
                    let delta = i128::from(new_start_ns) - i128::from(original_start_ns);
                    for track in proj.tracks.iter_mut() {
                        let mut changed = false;
                        for clip in &mut track.clips {
                            if clip.link_group_id.as_deref() == Some(link_group_id.as_str()) {
                                clip.timeline_start =
                                    (i128::from(clip.timeline_start) + delta).max(0) as u64;
                                moved_count += 1;
                                changed = true;
                            }
                        }
                        if changed {
                            track.sort_clips();
                            if magnetic_mode {
                                track.compact_gap_free();
                            }
                        }
                    }
                } else {
                    for track in proj.tracks.iter_mut() {
                        if let Some(idx) = track.clips.iter().position(|c| c.id == clip_id) {
                            track.clips[idx].timeline_start = new_start_ns;
                            if magnetic_mode {
                                track.compact_gap_free();
                            } else {
                                track.sort_clips();
                            }
                            moved_count = 1;
                            break;
                        }
                    }
                }
            }
            let found = moved_count > 0;
            if found {
                proj.dirty = true;
            }
            drop(proj);
            reply
                .send(json!({"success": found, "moved_clip_count": moved_count}))
                .ok();
            if found {
                on_project_changed();
            }
        }

        McpCommand::LinkClips { clip_ids, reply } => {
            if clip_ids.len() < 2 {
                reply
                    .send(json!({"success": false, "error": "clip_ids must contain at least two clip ids"}))
                    .ok();
                return;
            }
            let clip_id_set: HashSet<String> = clip_ids.into_iter().collect();
            let link_group_id = uuid::Uuid::new_v4().to_string();
            let mut proj = project.borrow_mut();
            let mut linked_count = 0usize;
            for track in proj.tracks.iter_mut() {
                for clip in &mut track.clips {
                    if clip_id_set.contains(&clip.id) {
                        clip.link_group_id = Some(link_group_id.clone());
                        linked_count += 1;
                    }
                }
            }
            let success = linked_count >= 2;
            if success {
                proj.dirty = true;
            }
            drop(proj);
            reply
                .send(json!({
                    "success": success,
                    "link_group_id": if success { Some(link_group_id) } else { None::<String> },
                    "linked_clip_count": linked_count
                }))
                .ok();
            if success {
                on_project_changed();
            }
        }

        McpCommand::UnlinkClips { clip_ids, reply } => {
            let clip_id_set: HashSet<String> = clip_ids.into_iter().collect();
            if clip_id_set.is_empty() {
                reply
                    .send(json!({"success": false, "error": "clip_ids must contain at least one clip id"}))
                    .ok();
                return;
            }
            let mut proj = project.borrow_mut();
            let target_link_groups: HashSet<String> = proj
                .tracks
                .iter()
                .flat_map(|t| t.clips.iter())
                .filter(|c| clip_id_set.contains(&c.id))
                .filter_map(|c| c.link_group_id.clone())
                .collect();
            if target_link_groups.is_empty() {
                drop(proj);
                reply
                    .send(json!({"success": false, "error": "No linked clips found for the provided clip_ids"}))
                    .ok();
                return;
            }
            let mut unlinked_count = 0usize;
            for track in proj.tracks.iter_mut() {
                for clip in &mut track.clips {
                    if clip
                        .link_group_id
                        .as_deref()
                        .is_some_and(|gid| target_link_groups.contains(gid))
                    {
                        clip.link_group_id = None;
                        unlinked_count += 1;
                    }
                }
            }
            let success = unlinked_count > 0;
            if success {
                proj.dirty = true;
            }
            drop(proj);
            reply
                .send(json!({"success": success, "unlinked_clip_count": unlinked_count}))
                .ok();
            if success {
                on_project_changed();
            }
        }

        McpCommand::AlignGroupedClipsByTimecode { clip_ids, reply } => {
            let result = {
                let mut proj = project.borrow_mut();
                let result = crate::ui::window::align_grouped_clips_by_timecode_in_project(
                    &mut proj, &clip_ids,
                );
                if result.is_ok() {
                    proj.dirty = true;
                }
                result
            };
            match result {
                Ok((aligned_group_count, aligned_clip_count)) => {
                    reply
                        .send(json!({
                            "success": true,
                            "aligned_group_count": aligned_group_count,
                            "aligned_clip_count": aligned_clip_count
                        }))
                        .ok();
                    on_project_changed();
                }
                Err(error) => {
                    reply.send(json!({"success": false, "error": error})).ok();
                }
            }
        }

        McpCommand::ConvertLtcAudioToTimecode {
            clip_id,
            ltc_channel,
            frame_rate,
            reply,
        } => {
            let context = {
                let proj = project.borrow();
                let lib = library.borrow();
                crate::ui::window::resolve_ltc_conversion_context(&proj, &lib, &clip_id, frame_rate)
            };
            match context {
                Ok(context) => {
                    let prepared = crate::media::ltc::decode_ltc_from_clip(
                        &context.source_path,
                        context.source_in,
                        context.source_out,
                        ltc_channel,
                        &context.frame_rate,
                    )
                    .map(|decode| PreparedLtcConversion { context, decode });
                    match prepared {
                        Ok(prepared) => {
                            let applied = {
                                let mut proj = project.borrow_mut();
                                let mut lib = library.borrow_mut();
                                let mut marks = source_marks.borrow_mut();
                                crate::ui::window::apply_prepared_ltc_conversion(
                                    &mut proj,
                                    &mut lib,
                                    Some(&mut *marks),
                                    prepared,
                                )
                            };
                            let timecode_label = timecode::format_ns_as_timecode(
                                applied.source_timecode_base_ns,
                                &applied.frame_rate,
                            );
                            reply
                                .send(json!({
                                    "success": true,
                                    "clip_id": clip_id,
                                    "source_path": applied.source_path,
                                    "source_timecode_base_ns": applied.source_timecode_base_ns,
                                    "timecode": timecode_label,
                                    "resolved_ltc_channel": applied.resolved_channel.as_str(),
                                    "applied_audio_channel_mode": applied.applied_audio_channel_mode.map(|mode| mode.as_str()),
                                    "muted": applied.muted_clip_count > 0,
                                    "updated_clip_count": applied.updated_clip_count,
                                    "message": crate::ui::window::format_ltc_conversion_status(&applied),
                                }))
                                .ok();
                            on_project_changed();
                        }
                        Err(error) => {
                            reply.send(json!({"success": false, "error": error})).ok();
                        }
                    }
                }
                Err(error) => {
                    reply.send(json!({"success": false, "error": error})).ok();
                }
            }
        }

        McpCommand::SyncClipsByAudio {
            clip_ids,
            replace_audio,
            reply,
        } => {
            if clip_ids.len() < 2 {
                reply
                    .send(json!({"success": false, "error": "Need at least 2 clip ids"}))
                    .ok();
            } else {
                // Collect clip info from project
                let clips: Vec<(String, String, u64, u64)> = {
                    let proj = project.borrow();
                    clip_ids
                        .iter()
                        .filter_map(|id| {
                            proj.tracks
                                .iter()
                                .flat_map(|t| t.clips.iter())
                                .find(|c| &c.id == id)
                                .map(|c| {
                                    (
                                        c.id.clone(),
                                        c.source_path.clone(),
                                        c.source_in,
                                        c.source_out,
                                    )
                                })
                        })
                        .collect()
                };
                if clips.len() < 2 {
                    reply
                        .send(json!({"success": false, "error": "Could not find 2+ clips with the provided ids"}))
                        .ok();
                } else {
                    let anchor_timeline_start = {
                        let proj = project.borrow();
                        proj.tracks
                            .iter()
                            .flat_map(|t| t.clips.iter())
                            .find(|c| c.id == clips[0].0)
                            .map(|c| c.timeline_start)
                            .unwrap_or(0)
                    };
                    let sync_results = crate::media::audio_sync::sync_clips_by_audio(&clips);
                    let mut result_json = Vec::new();
                    let mut assignments: HashMap<String, u64> = HashMap::new();
                    let mut drift_corrections: HashMap<String, f64> = HashMap::new();
                    let mut all_confident = true;
                    for r in &sync_results {
                        let new_start = (anchor_timeline_start as i64 + r.offset_ns).max(0) as u64;
                        result_json.push(json!({
                            "clip_id": r.clip_id,
                            "offset_ns": r.offset_ns,
                            "confidence": r.confidence,
                            "new_timeline_start_ns": new_start,
                            "drift_speed": r.drift_speed,
                        }));
                        if r.confidence < 3.0 {
                            all_confident = false;
                        } else {
                            assignments.insert(r.clip_id.clone(), new_start);
                            if let Some(drift) = r.drift_speed {
                                drift_corrections.insert(r.clip_id.clone(), drift);
                            }
                        }
                    }
                    if all_confident && !assignments.is_empty() {
                        let mut proj = project.borrow_mut();
                        // Apply timeline position assignments and drift speed corrections.
                        for track in &mut proj.tracks {
                            for clip in &mut track.clips {
                                if let Some(&new_start) = assignments.get(&clip.id) {
                                    clip.timeline_start = new_start;
                                }
                                if let Some(&drift) = drift_corrections.get(&clip.id) {
                                    if (drift - 1.0).abs() > 1e-9 {
                                        clip.speed *= drift;
                                    }
                                }
                            }
                        }
                        // When replace_audio is set, link all clips and mute anchor's embedded audio.
                        if replace_audio && clip_ids.len() >= 2 {
                            let link_id = uuid::Uuid::new_v4().to_string();
                            let anchor_id = &clip_ids[0];
                            for track in &mut proj.tracks {
                                for clip in &mut track.clips {
                                    if clip_ids.contains(&clip.id) {
                                        clip.link_group_id = Some(link_id.clone());
                                    }
                                    // Mute anchor clip's embedded audio so external audio replaces it.
                                    if &clip.id == anchor_id
                                        && clip.kind == crate::model::clip::ClipKind::Video
                                    {
                                        clip.volume = 0.0;
                                    }
                                }
                            }
                        }
                        proj.dirty = true;
                        drop(proj);
                        on_project_changed();
                    }
                    reply
                        .send(json!({
                            "success": all_confident,
                            "replace_audio_applied": replace_audio && all_confident,
                            "results": result_json,
                        }))
                        .ok();
                }
            }
        }

        McpCommand::CopyClipColorGrade { clip_id, reply } => {
            let mut ts = timeline_state.borrow_mut();
            // Temporarily set selected clip for the copy operation
            let prev_selected = ts.selected_clip_id.clone();
            ts.selected_clip_id = Some(clip_id.clone());
            let ok = ts.copy_color_grade();
            ts.selected_clip_id = prev_selected;
            drop(ts);
            reply.send(json!({"success": ok})).ok();
        }

        McpCommand::PasteClipColorGrade { clip_id, reply } => {
            let mut ts = timeline_state.borrow_mut();
            let prev_selected = ts.selected_clip_id.clone();
            ts.selected_clip_id = Some(clip_id.clone());
            let ok = ts.paste_color_grade();
            ts.selected_clip_id = prev_selected;
            drop(ts);
            if ok {
                on_project_changed_full();
            }
            reply.send(json!({"success": ok})).ok();
        }

        McpCommand::MatchClipColors {
            source_clip_id,
            reference_clip_id,
            generate_lut,
            reply,
        } => {
            // Collect clip info from project.
            let clip_info: Option<(
                String,
                u64,
                u64,
                String, // source: path, in, out, track_id
                String,
                u64,
                u64, // ref: path, in, out
                Option<crate::media::color_match::ReferenceGrading>,
            )> = {
                let proj = project.borrow();
                let find_clip = |id: &str| -> Option<(String, u64, u64, String)> {
                    for track in &proj.tracks {
                        if let Some(c) = track.clips.iter().find(|c| c.id == id) {
                            return Some((
                                c.source_path.clone(),
                                c.source_in,
                                c.source_out,
                                track.id.clone(),
                            ));
                        }
                    }
                    None
                };
                let ref_grading = proj
                    .tracks
                    .iter()
                    .flat_map(|t| t.clips.iter())
                    .find(|c| c.id == reference_clip_id)
                    .map(crate::media::color_match::ReferenceGrading::from_clip);
                let src = find_clip(&source_clip_id);
                let reff = find_clip(&reference_clip_id);
                match (src, reff) {
                    (Some(s), Some(r)) => Some((s.0, s.1, s.2, s.3, r.0, r.1, r.2, ref_grading)),
                    _ => None,
                }
            };

            let Some((
                src_path,
                src_in,
                src_out,
                src_track_id,
                ref_path,
                ref_in,
                ref_out,
                ref_grading,
            )) = clip_info
            else {
                reply
                    .send(json!({"success": false, "error": "Could not find source and/or reference clip"}))
                    .ok();
                return;
            };

            // Capture old values before modification.
            let old_values = {
                let proj = project.borrow();
                proj.tracks
                    .iter()
                    .flat_map(|t| t.clips.iter())
                    .find(|c| c.id == source_clip_id)
                    .map(|c| {
                        (
                            c.brightness,
                            c.contrast,
                            c.saturation,
                            c.temperature,
                            c.tint,
                            c.exposure,
                            c.black_point,
                            c.shadows,
                            c.midtones,
                            c.highlights,
                            c.highlights_warmth,
                            c.highlights_tint,
                            c.midtones_warmth,
                            c.midtones_tint,
                            c.shadows_warmth,
                            c.shadows_tint,
                            c.lut_paths.clone(),
                        )
                    })
            };
            let Some(old) = old_values else {
                reply
                    .send(json!({"success": false, "error": "Source clip not found"}))
                    .ok();
                return;
            };

            let params = crate::media::color_match::MatchColorParams {
                source_path: src_path,
                source_in_ns: src_in,
                source_out_ns: src_out,
                reference_path: ref_path,
                reference_in_ns: ref_in,
                reference_out_ns: ref_out,
                sample_count: 8,
                generate_lut,
                lut_output_dir: None,
                reference_grading: ref_grading,
            };

            match crate::media::color_match::run_match_color(&params) {
                Ok(outcome) => {
                    let r = &outcome.slider_result;

                    // Build and execute undo command.
                    let cmd = crate::undo::MatchColorCommand {
                        clip_id: source_clip_id.clone(),
                        track_id: src_track_id.clone(),
                        old_brightness: old.0,
                        old_contrast: old.1,
                        old_saturation: old.2,
                        old_temperature: old.3,
                        old_tint: old.4,
                        old_exposure: old.5,
                        old_black_point: old.6,
                        old_shadows: old.7,
                        old_midtones: old.8,
                        old_highlights: old.9,
                        old_highlights_warmth: old.10,
                        old_highlights_tint: old.11,
                        old_midtones_warmth: old.12,
                        old_midtones_tint: old.13,
                        old_shadows_warmth: old.14,
                        old_shadows_tint: old.15,
                        old_lut_paths: old.16.clone(),
                        new_brightness: r.brightness,
                        new_contrast: r.contrast,
                        new_saturation: r.saturation,
                        new_temperature: r.temperature,
                        new_tint: r.tint,
                        new_exposure: r.exposure,
                        new_black_point: r.black_point,
                        new_shadows: r.shadows,
                        new_midtones: r.midtones,
                        new_highlights: r.highlights,
                        new_highlights_warmth: r.highlights_warmth,
                        new_highlights_tint: r.highlights_tint,
                        new_midtones_warmth: r.midtones_warmth,
                        new_midtones_tint: r.midtones_tint,
                        new_shadows_warmth: r.shadows_warmth,
                        new_shadows_tint: r.shadows_tint,
                        new_lut_paths: {
                            let mut paths = old.16.clone();
                            if let Some(ref lp) = outcome.lut_path {
                                paths.push(lp.clone());
                            }
                            paths
                        },
                    };

                    {
                        let mut ts = timeline_state.borrow_mut();
                        let mut proj = project.borrow_mut();
                        ts.history.execute(Box::new(cmd), &mut proj);
                    }

                    // Also assign the generated LUT if any.
                    if let Some(ref lut_path) = outcome.lut_path {
                        let mut proj = project.borrow_mut();
                        if let Some(clip) = proj.clip_mut(&source_clip_id) {
                            clip.lut_paths.push(lut_path.clone());
                        }
                        proj.dirty = true;
                    }

                    on_project_changed_full();

                    reply
                        .send(json!({
                            "success": true,
                            "applied": {
                                "brightness": r.brightness,
                                "contrast": r.contrast,
                                "saturation": r.saturation,
                                "temperature": r.temperature,
                                "tint": r.tint,
                                "exposure": r.exposure,
                            },
                            "lut_path": outcome.lut_path,
                            "source_stats": {
                                "mean_l": outcome.source_stats.mean_l,
                                "std_l": outcome.source_stats.std_l,
                                "mean_a": outcome.source_stats.mean_a,
                                "mean_b": outcome.source_stats.mean_b,
                            },
                            "reference_stats": {
                                "mean_l": outcome.reference_stats.mean_l,
                                "std_l": outcome.reference_stats.std_l,
                                "mean_a": outcome.reference_stats.mean_a,
                                "mean_b": outcome.reference_stats.mean_b,
                            },
                        }))
                        .ok();
                }
                Err(e) => {
                    reply
                        .send(json!({"success": false, "error": format!("{e}")}))
                        .ok();
                }
            }
        }

        McpCommand::TrimClip {
            clip_id,
            source_in_ns,
            source_out_ns,
            reply,
        } => {
            let magnetic_mode = timeline_state.borrow().magnetic_mode;
            let mut proj = project.borrow_mut();
            let mut found = false;
            for track in proj.tracks.iter_mut() {
                if let Some(idx) = track.clips.iter().position(|c| c.id == clip_id) {
                    track.clips[idx].source_in = source_in_ns;
                    track.clips[idx].source_out = source_out_ns;
                    track.clips[idx].clamp_source_out();
                    if magnetic_mode {
                        track.compact_gap_free();
                    }
                    proj.dirty = true;
                    found = true;
                    break;
                }
            }
            drop(proj);
            reply.send(json!({"success": found})).ok();
            if found {
                on_project_changed();
            }
        }

        McpCommand::SetClipSpeed {
            clip_id,
            speed,
            slow_motion_interp,
            reply,
        } => {
            let speed = speed.clamp(0.05, 16.0);
            let mut found = false;
            let mut requested_ai = false;
            {
                let mut proj = project.borrow_mut();
                if let Some(clip) = proj.clip_mut(&clip_id) {
                    clip.speed = speed;
                    if let Some(ref mode) = slow_motion_interp {
                        clip.slow_motion_interp =
                            crate::model::clip::SlowMotionInterp::from_xml_str(mode);
                    }
                    requested_ai = clip.slow_motion_interp
                        == crate::model::clip::SlowMotionInterp::Ai
                        && clip.has_slow_motion();
                    proj.dirty = true;
                    found = true;
                }
            }
            if found && requested_ai {
                let proj = project.borrow();
                let clip = proj.clip_ref(&clip_id).cloned();
                drop(proj);
                if let Some(clip) = clip {
                    frame_interp_cache.borrow_mut().request_for_clip(&clip);
                }
            }
            reply
                .send(json!({
                    "success": found,
                    "ai_queued": requested_ai,
                }))
                .ok();
            if found {
                on_project_changed();
            }
        }

        McpCommand::SlipClip {
            clip_id,
            delta_ns,
            reply,
        } => {
            let mut proj = project.borrow_mut();
            let found = if let Some(clip) = proj.clip_mut(&clip_id) {
                let new_in = (clip.source_in as i64 + delta_ns).max(0) as u64;
                let mut new_out =
                    (clip.source_out as i64 + delta_ns).max(new_in as i64 + 1_000_000) as u64;
                if let Some(max) = clip.max_source_out() {
                    if new_out > max {
                        new_out = max;
                    }
                }
                clip.source_in = new_in;
                clip.source_out = new_out;
                true
            } else {
                false
            };
            if found {
                proj.dirty = true;
            }
            drop(proj);
            reply.send(json!({"success": found})).ok();
            if found {
                on_project_changed();
            }
        }

        McpCommand::SlideClip {
            clip_id,
            delta_ns,
            reply,
        } => {
            let mut proj = project.borrow_mut();
            let mut found = false;
            for track in proj.tracks.iter_mut() {
                let clip_idx = track.clips.iter().position(|c| c.id == clip_id);
                if let Some(idx) = clip_idx {
                    let mut sorted_indices: Vec<usize> = (0..track.clips.len()).collect();
                    sorted_indices.sort_by_key(|&i| track.clips[i].timeline_start);
                    let sorted_pos = sorted_indices.iter().position(|&i| i == idx);
                    let left_idx = sorted_pos.and_then(|p| {
                        if p > 0 {
                            Some(sorted_indices[p - 1])
                        } else {
                            None
                        }
                    });
                    let right_idx = sorted_pos.and_then(|p| sorted_indices.get(p + 1).copied());
                    // Validate neighbors
                    let left_ok = left_idx
                        .map(|li| {
                            let new_out =
                                (track.clips[li].source_out as i64 + delta_ns).max(0) as u64;
                            new_out > track.clips[li].source_in + 1_000_000
                        })
                        .unwrap_or(true);
                    let right_ok = right_idx
                        .map(|ri| {
                            let new_in =
                                (track.clips[ri].source_in as i64 + delta_ns).max(0) as u64;
                            new_in + 1_000_000 < track.clips[ri].source_out
                        })
                        .unwrap_or(true);
                    if left_ok && right_ok {
                        track.clips[idx].timeline_start =
                            (track.clips[idx].timeline_start as i64 + delta_ns).max(0) as u64;
                        if let Some(li) = left_idx {
                            track.clips[li].source_out =
                                (track.clips[li].source_out as i64 + delta_ns).max(0) as u64;
                            track.clips[li].clamp_source_out();
                        }
                        if let Some(ri) = right_idx {
                            track.clips[ri].source_in =
                                (track.clips[ri].source_in as i64 + delta_ns).max(0) as u64;
                            track.clips[ri].timeline_start =
                                (track.clips[ri].timeline_start as i64 + delta_ns).max(0) as u64;
                        }
                        proj.dirty = true;
                        found = true;
                    }
                    break;
                }
            }
            drop(proj);
            reply.send(json!({"success": found})).ok();
            if found {
                on_project_changed_full();
            }
        }

        McpCommand::SetClipColor {
            clip_id,
            brightness,
            contrast,
            saturation,
            temperature,
            tint,
            denoise,
            sharpness,
            blur,
            shadows,
            midtones,
            highlights,
            exposure,
            black_point,
            highlights_warmth,
            highlights_tint,
            midtones_warmth,
            midtones_tint,
            shadows_warmth,
            shadows_tint,
            reply,
        } => {
            let mut proj = project.borrow_mut();
            let found = if let Some(clip) = proj.clip_mut(&clip_id) {
                clip.brightness = brightness as f32;
                clip.contrast = contrast as f32;
                clip.saturation = saturation as f32;
                clip.temperature = temperature as f32;
                clip.tint = tint as f32;
                clip.denoise = denoise as f32;
                clip.sharpness = sharpness as f32;
                clip.blur = blur as f32;
                clip.shadows = shadows as f32;
                clip.midtones = midtones as f32;
                clip.highlights = highlights as f32;
                clip.exposure = exposure as f32;
                clip.black_point = black_point as f32;
                clip.highlights_warmth = highlights_warmth as f32;
                clip.highlights_tint = highlights_tint as f32;
                clip.midtones_warmth = midtones_warmth as f32;
                clip.midtones_tint = midtones_tint as f32;
                clip.shadows_warmth = shadows_warmth as f32;
                clip.shadows_tint = shadows_tint as f32;
                proj.dirty = true;
                true
            } else {
                false
            };
            drop(proj);
            reply.send(json!({"success": found})).ok();
            if found {
                on_project_changed_full();
            }
        }

        McpCommand::SetClipColorLabel {
            clip_id,
            color_label,
            reply,
        } => {
            let parsed = match color_label.as_str() {
                "none" => Some(crate::model::clip::ClipColorLabel::None),
                "red" => Some(crate::model::clip::ClipColorLabel::Red),
                "orange" => Some(crate::model::clip::ClipColorLabel::Orange),
                "yellow" => Some(crate::model::clip::ClipColorLabel::Yellow),
                "green" => Some(crate::model::clip::ClipColorLabel::Green),
                "teal" => Some(crate::model::clip::ClipColorLabel::Teal),
                "blue" => Some(crate::model::clip::ClipColorLabel::Blue),
                "purple" => Some(crate::model::clip::ClipColorLabel::Purple),
                "magenta" => Some(crate::model::clip::ClipColorLabel::Magenta),
                _ => None,
            };
            let Some(parsed) = parsed else {
                reply
                    .send(json!({"success": false, "error": "color_label must be one of: none, red, orange, yellow, green, teal, blue, purple, magenta"}))
                    .ok();
                return;
            };
            let mut proj = project.borrow_mut();
            let found = if let Some(clip) = proj.clip_mut(&clip_id) {
                clip.color_label = parsed;
                proj.dirty = true;
                true
            } else {
                false
            };
            drop(proj);
            reply
                .send(json!({"success": found, "clip_id": clip_id, "color_label": color_label}))
                .ok();
            if found {
                on_project_changed();
            }
        }

        McpCommand::SetClipChromaKey {
            clip_id,
            enabled,
            color,
            tolerance,
            softness,
            reply,
        } => {
            let mut proj = project.borrow_mut();
            let found = if let Some(clip) = proj.clip_mut(&clip_id) {
                if let Some(v) = enabled {
                    clip.chroma_key_enabled = v;
                }
                if let Some(v) = color {
                    clip.chroma_key_color = v;
                }
                if let Some(v) = tolerance {
                    clip.chroma_key_tolerance = v as f32;
                }
                if let Some(v) = softness {
                    clip.chroma_key_softness = v as f32;
                }
                proj.dirty = true;
                true
            } else {
                false
            };
            drop(proj);
            reply.send(json!({"success": found})).ok();
            if found {
                on_project_changed();
            }
        }

        #[cfg(feature = "ai-inference")]
        McpCommand::GenerateSamMask {
            clip_id,
            frame_ns,
            box_x1,
            box_y1,
            box_x2,
            box_y2,
            point_x,
            point_y,
            tolerance_px,
            reply,
        } => {
            // Step 1 — resolve the clip.
            let clip_info = {
                let proj = project.borrow();
                proj.tracks.iter().find_map(|t| {
                    t.clips
                        .iter()
                        .find(|c| c.id == clip_id)
                        .map(|c| (c.source_path.clone(), c.source_in, c.source_out))
                })
            };
            let Some((source_path, source_in, source_out)) = clip_info else {
                reply
                    .send(serde_json::json!({"success": false, "error": "Clip not found"}))
                    .ok();
                return;
            };

            // Step 2 — build prompt in normalized coords.
            let target_frame_ns = frame_ns.unwrap_or(source_in).clamp(source_in, source_out);
            use crate::media::sam_cache::BoxPrompt;
            let placeholder = BoxPrompt::from_corners(0.0, 0.0, 1.0, 1.0);
            let normalized_box: Result<Option<(f32, f32, f32, f32)>, &'static str> =
                match (box_x1, box_y1, box_x2, box_y2) {
                    (Some(x1), Some(y1), Some(x2), Some(y2)) => {
                        Ok(Some((x1 as f32, y1 as f32, x2 as f32, y2 as f32)))
                    }
                    _ => match (point_x, point_y) {
                        (Some(px), Some(py)) => {
                            // Point prompt → small normalized box
                            // (~8 px at 1000 px source resolution).
                            let h = 0.004;
                            Ok(Some((
                                (px - h).max(0.0) as f32,
                                (py - h).max(0.0) as f32,
                                (px + h).min(1.0) as f32,
                                (py + h).min(1.0) as f32,
                            )))
                        }
                        _ => Err("Missing prompt: provide either all four box_{x1,y1,x2,y2} or both point_x/point_y"),
                    },
                };
            let normalized_box = match normalized_box {
                Ok(b) => b,
                Err(msg) => {
                    reply
                        .send(serde_json::json!({"success": false, "error": msg}))
                        .ok();
                    return;
                }
            };

            // Step 3 — run the full pipeline synchronously (MCP is
            // automation traffic — blocking the main thread is OK).
            let input = crate::media::sam_job::SamJobInput {
                source_path: std::path::PathBuf::from(&source_path),
                frame_ns: target_frame_ns,
                prompt: placeholder,
                normalized_box,
                tolerance_px: tolerance_px.unwrap_or(2.0).max(0.0),
            };
            let result = crate::media::sam_job::run_sam_pipeline(input);

            // Step 4 — on success, append the mask to the clip.
            match result {
                crate::media::sam_job::SamJobResult::Success { mask_points, score } => {
                    let point_count = mask_points.len();
                    let appended: Option<String> = {
                        let mut proj = project.borrow_mut();
                        let mut out: Option<String> = None;
                        for track in proj.tracks.iter_mut() {
                            if let Some(clip) = track.clips.iter_mut().find(|c| c.id == clip_id) {
                                let mask =
                                    crate::model::clip::ClipMask::new_path(mask_points.clone());
                                let mask_id = mask.id.clone();
                                clip.masks.push(mask);
                                out = Some(mask_id);
                                break;
                            }
                        }
                        if out.is_some() {
                            proj.dirty = true;
                        }
                        out
                    };
                    match appended {
                        Some(mask_id) => {
                            on_project_changed();
                            reply
                                .send(serde_json::json!({
                                    "success": true,
                                    "mask_id": mask_id,
                                    "score": score,
                                    "point_count": point_count,
                                }))
                                .ok();
                        }
                        None => {
                            reply
                                .send(serde_json::json!({
                                    "success": false,
                                    "error": "Clip disappeared during SAM inference"
                                }))
                                .ok();
                        }
                    }
                }
                crate::media::sam_job::SamJobResult::Error(msg) => {
                    reply
                        .send(serde_json::json!({"success": false, "error": msg}))
                        .ok();
                }
            }
        }

        #[cfg(not(feature = "ai-inference"))]
        McpCommand::GenerateSamMask { reply, .. } => {
            reply
                .send(serde_json::json!({
                    "success": false,
                    "error": "generate_sam_mask requires the ai-inference Cargo feature"
                }))
                .ok();
        }

        McpCommand::SetClipMask {
            clip_id,
            enabled,
            shape,
            center_x,
            center_y,
            width,
            height,
            rotation,
            feather,
            expansion,
            invert,
            path,
            reply,
        } => {
            let mut proj = project.borrow_mut();
            let found = if let Some(clip) = proj.clip_mut(&clip_id) {
                // Create mask[0] if absent
                if clip.masks.is_empty() {
                    clip.masks.push(crate::model::clip::ClipMask::new(
                        crate::model::clip::MaskShape::Rectangle,
                    ));
                }
                if let Some(mask) = clip.masks.first_mut() {
                    if let Some(v) = enabled {
                        mask.enabled = v;
                    }
                    if let Some(ref s) = shape {
                        mask.shape = match s.as_str() {
                            "ellipse" => crate::model::clip::MaskShape::Ellipse,
                            "path" => crate::model::clip::MaskShape::Path,
                            _ => crate::model::clip::MaskShape::Rectangle,
                        };
                    }
                    // Handle path data for bezier path masks
                    if let Some(ref s) = shape {
                        if s == "path" {
                            if let Some(ref path_val) = path {
                                if let Ok(points) =
                                    serde_json::from_value::<Vec<crate::model::clip::BezierPoint>>(
                                        path_val.clone(),
                                    )
                                {
                                    mask.path = Some(crate::model::clip::MaskPath { points });
                                }
                            }
                            if mask.path.is_none() {
                                mask.path = Some(crate::model::clip::default_diamond_path());
                            }
                        }
                    }
                    if let Some(v) = center_x {
                        mask.center_x = v.clamp(0.0, 1.0);
                    }
                    if let Some(v) = center_y {
                        mask.center_y = v.clamp(0.0, 1.0);
                    }
                    if let Some(v) = width {
                        mask.width = v.clamp(0.01, 0.5);
                    }
                    if let Some(v) = height {
                        mask.height = v.clamp(0.01, 0.5);
                    }
                    if let Some(v) = rotation {
                        mask.rotation = v.clamp(
                            crate::model::transform_bounds::ROTATE_MIN_DEG,
                            crate::model::transform_bounds::ROTATE_MAX_DEG,
                        );
                    }
                    if let Some(v) = feather {
                        mask.feather = v.clamp(0.0, 0.5);
                    }
                    if let Some(v) = expansion {
                        mask.expansion = v.clamp(-0.5, 0.5);
                    }
                    if let Some(v) = invert {
                        mask.invert = v;
                    }
                }
                proj.dirty = true;
                true
            } else {
                false
            };
            drop(proj);
            reply.send(json!({"success": found})).ok();
            if found {
                on_project_changed();
            }
        }

        McpCommand::SetClipBgRemoval {
            clip_id,
            enabled,
            threshold,
            reply,
        } => {
            let mut proj = project.borrow_mut();
            let found = if let Some(clip) = proj.clip_mut(&clip_id) {
                if let Some(v) = enabled {
                    clip.bg_removal_enabled = v;
                }
                if let Some(v) = threshold {
                    clip.bg_removal_threshold = v;
                }
                proj.dirty = true;
                true
            } else {
                false
            };
            drop(proj);
            reply.send(json!({"success": found})).ok();
            if found {
                on_project_changed();
            }
        }

        McpCommand::SetClipHslQualifier {
            clip_id,
            qualifier,
            reply,
        } => {
            let mut proj = project.borrow_mut();
            let found = if let Some(clip) = proj.clip_mut(&clip_id) {
                clip.hsl_qualifier = qualifier.clone();
                proj.dirty = true;
                true
            } else {
                false
            };
            drop(proj);
            reply.send(json!({"success": found})).ok();
            if found {
                on_project_changed();
            }
        }

        McpCommand::SetClipMotionBlur {
            clip_id,
            enabled,
            shutter_angle,
            reply,
        } => {
            let mut proj = project.borrow_mut();
            let result: Option<(bool, f64)> = if let Some(clip) = proj.clip_mut(&clip_id) {
                if let Some(v) = enabled {
                    clip.motion_blur_enabled = v;
                }
                if let Some(v) = shutter_angle {
                    clip.motion_blur_shutter_angle = v.clamp(0.0, 720.0);
                }
                Some((clip.motion_blur_enabled, clip.motion_blur_shutter_angle))
            } else {
                None
            };
            if result.is_some() {
                proj.dirty = true;
            }
            drop(proj);
            let response = match result {
                Some((en, sh)) => json!({
                    "success": true,
                    "motion_blur_enabled": en,
                    "motion_blur_shutter_angle": sh,
                }),
                None => json!({"success": false}),
            };
            let found = result.is_some();
            reply.send(response).ok();
            if found {
                on_project_changed();
            }
        }

        McpCommand::SetClipLut {
            clip_id,
            lut_paths,
            reply,
        } => {
            let mut proj = project.borrow_mut();
            let found = if let Some(clip) = proj.clip_mut(&clip_id) {
                clip.lut_paths = lut_paths.clone();
                proj.dirty = true;
                true
            } else {
                false
            };
            drop(proj);
            reply.send(json!({"success": found})).ok();
            if found {
                on_project_changed();
            }
        }

        McpCommand::SetClipTransform {
            clip_id,
            scale,
            position_x,
            position_y,
            rotate,
            anamorphic_desqueeze,
            reply,
        } => {
            use crate::model::transform_bounds::{
                POSITION_MAX, POSITION_MIN, ROTATE_MAX_DEG_I32, ROTATE_MIN_DEG_I32, SCALE_MAX,
                SCALE_MIN,
            };
            let mut proj = project.borrow_mut();
            let found = if let Some(clip) = proj.clip_mut(&clip_id) {
                clip.scale = scale.clamp(SCALE_MIN, SCALE_MAX);
                clip.position_x = position_x.clamp(POSITION_MIN, POSITION_MAX);
                clip.position_y = position_y.clamp(POSITION_MIN, POSITION_MAX);
                if let Some(rot) = rotate {
                    clip.rotate = rot.clamp(ROTATE_MIN_DEG_I32, ROTATE_MAX_DEG_I32);
                }
                if let Some(a) = anamorphic_desqueeze {
                    clip.anamorphic_desqueeze = a;
                }
                proj.dirty = true;
                true
            } else {
                false
            };
            drop(proj);
            reply.send(json!({"success": found})).ok();
            if found {
                on_project_changed();
            }
        }

        McpCommand::SetClipOpacity {
            clip_id,
            opacity,
            reply,
        } => {
            let mut proj = project.borrow_mut();
            let found = if let Some(clip) = proj.clip_mut(&clip_id) {
                clip.opacity = opacity.clamp(0.0, 1.0);
                proj.dirty = true;
                true
            } else {
                false
            };
            drop(proj);
            reply.send(json!({"success": found})).ok();
            if found {
                on_project_changed();
            }
        }

        McpCommand::SetClipVoiceIsolation {
            clip_id,
            voice_isolation,
            reply,
        } => {
            let mut proj = project.borrow_mut();
            let found = if let Some(clip) = proj.clip_mut(&clip_id) {
                clip.voice_isolation = voice_isolation.clamp(0.0, 1.0) as f32;
                proj.dirty = true;
                true
            } else {
                false
            };
            drop(proj);
            reply.send(json!({"success": found})).ok();
            if found {
                on_project_changed();
            }
        }

        McpCommand::SetClipVoiceEnhance {
            clip_id,
            enabled,
            strength,
            reply,
        } => {
            let mut proj = project.borrow_mut();
            let result = if let Some(clip) = proj.clip_mut(&clip_id) {
                clip.voice_enhance = enabled;
                if let Some(s) = strength {
                    clip.voice_enhance_strength = (s as f32).clamp(0.0, 1.0);
                }
                let snapshot = (clip.voice_enhance, clip.voice_enhance_strength);
                proj.dirty = true;
                Some(snapshot)
            } else {
                None
            };
            drop(proj);
            match result {
                Some((en, st)) => {
                    reply
                        .send(json!({
                            "success": true,
                            "enabled": en,
                            "strength": st as f64,
                        }))
                        .ok();
                    on_project_changed();
                }
                None => {
                    reply.send(json!({"success": false})).ok();
                }
            }
        }

        McpCommand::SetClipSubtitleVisible {
            clip_id,
            visible,
            reply,
        } => {
            let mut proj = project.borrow_mut();
            let found = if let Some(clip) = proj.clip_mut(&clip_id) {
                clip.subtitle_visible = visible;
                proj.dirty = true;
                true
            } else {
                false
            };
            drop(proj);
            reply
                .send(json!({"success": found, "visible": visible}))
                .ok();
            if found {
                on_project_changed();
            }
        }

        McpCommand::SetVoiceIsolationSource {
            clip_id,
            source,
            reply,
        } => {
            let new_source = crate::model::clip::VoiceIsolationSource::from_str(&source);
            let mut proj = project.borrow_mut();
            let found = if let Some(clip) = proj.clip_mut(&clip_id) {
                clip.voice_isolation_source = new_source;
                proj.dirty = true;
                true
            } else {
                false
            };
            drop(proj);
            reply
                .send(json!({"success": found, "source": new_source.as_str()}))
                .ok();
            if found {
                on_project_changed();
            }
        }

        McpCommand::SetVoiceIsolationSilenceParams {
            clip_id,
            threshold_db,
            min_ms,
            reply,
        } => {
            let mut proj = project.borrow_mut();
            let updated = if let Some(clip) = proj.clip_mut(&clip_id) {
                if let Some(t) = threshold_db {
                    clip.voice_isolation_silence_threshold_db = (t as f32).clamp(-60.0, -10.0);
                }
                if let Some(m) = min_ms {
                    clip.voice_isolation_silence_min_ms = m.clamp(50, 2000);
                }
                // Param change invalidates the cached analysis.
                clip.voice_isolation_speech_intervals.clear();
                Some((
                    clip.voice_isolation_silence_threshold_db,
                    clip.voice_isolation_silence_min_ms,
                ))
            } else {
                None
            };
            if updated.is_some() {
                proj.dirty = true;
            }
            drop(proj);
            match updated {
                Some((t, m)) => {
                    reply
                        .send(json!({
                            "success": true,
                            "threshold_db": t,
                            "min_ms": m
                        }))
                        .ok();
                    on_project_changed();
                }
                None => {
                    reply.send(json!({"success": false})).ok();
                }
            }
        }

        McpCommand::SuggestVoiceIsolationThreshold { clip_id, reply } => {
            let info = {
                let proj = project.borrow();
                proj.clip_ref(&clip_id)
                    .map(|c| (c.source_path.clone(), c.source_in, c.source_out))
            };
            match info {
                None => {
                    reply
                        .send(json!({"success": false, "error": "clip not found"}))
                        .ok();
                }
                Some((source_path, source_in, source_out)) => {
                    match crate::media::export::suggest_silence_threshold_db(
                        &source_path,
                        source_in,
                        source_out,
                    ) {
                        Ok(db) => {
                            reply
                                .send(json!({"success": true, "threshold_db": db}))
                                .ok();
                        }
                        Err(e) => {
                            reply
                                .send(json!({"success": false, "error": e.to_string()}))
                                .ok();
                        }
                    }
                }
            }
        }

        McpCommand::AnalyzeVoiceIsolationSilence { clip_id, reply } => {
            let info = {
                let proj = project.borrow();
                proj.clip_ref(&clip_id).map(|c| {
                    (
                        c.source_path.clone(),
                        c.source_in,
                        c.source_out,
                        c.voice_isolation_silence_threshold_db,
                        c.voice_isolation_silence_min_ms,
                    )
                })
            };
            match info {
                None => {
                    reply
                        .send(json!({"success": false, "error": "clip not found"}))
                        .ok();
                }
                Some((source_path, source_in, source_out, threshold_db, min_ms)) => {
                    let min_duration = (min_ms as f64) / 1000.0;
                    match crate::media::export::detect_silence(
                        &source_path,
                        source_in,
                        source_out,
                        threshold_db as f64,
                        min_duration,
                    ) {
                        Ok(silences) => {
                            let clip_duration_ns = source_out.saturating_sub(source_in);
                            let new_intervals = crate::media::export::invert_silences_to_speech(
                                &silences,
                                clip_duration_ns,
                            );
                            let count = new_intervals.len();
                            let intervals_json: Vec<_> =
                                new_intervals.iter().map(|(s, e)| json!([s, e])).collect();
                            {
                                let mut proj = project.borrow_mut();
                                if let Some(clip) = proj.clip_mut(&clip_id) {
                                    clip.voice_isolation_speech_intervals = new_intervals;
                                    proj.dirty = true;
                                }
                            }
                            reply
                                .send(json!({
                                    "success": true,
                                    "count": count,
                                    "intervals_ns": intervals_json
                                }))
                                .ok();
                            on_project_changed();
                        }
                        Err(e) => {
                            reply
                                .send(json!({"success": false, "error": e.to_string()}))
                                .ok();
                        }
                    }
                }
            }
        }

        McpCommand::SetClipEq {
            clip_id,
            low_freq,
            low_gain,
            low_q,
            mid_freq,
            mid_gain,
            mid_q,
            high_freq,
            high_gain,
            high_q,
            reply,
        } => {
            let mut proj = project.borrow_mut();
            let mut result_bands = crate::model::clip::default_eq_bands();
            let found = if let Some(clip) = proj.clip_mut(&clip_id) {
                if let Some(v) = low_freq {
                    clip.eq_bands[0].freq = v.clamp(20.0, 20000.0);
                }
                if let Some(v) = low_gain {
                    clip.eq_bands[0].gain = v.clamp(-24.0, 12.0);
                }
                if let Some(v) = low_q {
                    clip.eq_bands[0].q = v.clamp(0.1, 10.0);
                }
                if let Some(v) = mid_freq {
                    clip.eq_bands[1].freq = v.clamp(20.0, 20000.0);
                }
                if let Some(v) = mid_gain {
                    clip.eq_bands[1].gain = v.clamp(-24.0, 12.0);
                }
                if let Some(v) = mid_q {
                    clip.eq_bands[1].q = v.clamp(0.1, 10.0);
                }
                if let Some(v) = high_freq {
                    clip.eq_bands[2].freq = v.clamp(20.0, 20000.0);
                }
                if let Some(v) = high_gain {
                    clip.eq_bands[2].gain = v.clamp(-24.0, 12.0);
                }
                if let Some(v) = high_q {
                    clip.eq_bands[2].q = v.clamp(0.1, 10.0);
                }
                result_bands = clip.eq_bands;
                proj.dirty = true;
                true
            } else {
                false
            };
            drop(proj);
            if found {
                prog_player
                    .borrow_mut()
                    .update_eq_for_clip(&clip_id, result_bands);
            }
            reply.send(json!({
                "success": found,
                "eq_bands": {
                    "low": { "freq": result_bands[0].freq, "gain": result_bands[0].gain, "q": result_bands[0].q },
                    "mid": { "freq": result_bands[1].freq, "gain": result_bands[1].gain, "q": result_bands[1].q },
                    "high": { "freq": result_bands[2].freq, "gain": result_bands[2].gain, "q": result_bands[2].q }
                }
            })).ok();
            if found {
                on_project_changed();
            }
        }

        McpCommand::AnalyzeProjectLoudness { reply } => {
            // Snapshot the project so the render/analyze can run on the
            // main thread without holding a borrow across the ffmpeg
            // subprocess (MCP dispatch already runs on the GTK loop,
            // which is acceptable for a blocking tool call — matches
            // export_mp4 and normalize_clip_audio patterns).
            let project_snapshot = project.borrow().clone();
            let current_gain = project_snapshot.master_gain_db;
            let result = crate::media::export::analyze_project_loudness(&project_snapshot);
            let (pref_preset, pref_target) = {
                let p = preferences_state.borrow();
                (p.loudness_target_preset.clone(), p.loudness_target_lufs)
            };
            match result {
                Ok(report) => {
                    let target_lufs = crate::ui_state::loudness_target_preset_to_lufs(&pref_preset)
                        .unwrap_or(pref_target);
                    let delta = target_lufs - report.integrated_lufs;
                    reply
                        .send(json!({
                            "success": true,
                            "integrated_lufs": report.integrated_lufs,
                            "short_term_max_lufs": report.short_term_max_lufs,
                            "momentary_max_lufs": report.momentary_max_lufs,
                            "loudness_range_lu": report.loudness_range_lu,
                            "true_peak_dbtp": report.true_peak_dbtp,
                            "threshold_lufs": report.threshold_lufs,
                            "current_master_gain_db": current_gain,
                            "target_preset": pref_preset,
                            "target_lufs": target_lufs,
                            "delta_db": delta,
                        }))
                        .ok();
                }
                Err(e) => {
                    reply
                        .send(json!({"success": false, "error": e.to_string()}))
                        .ok();
                }
            }
        }

        McpCommand::SetProjectMasterGainDb {
            master_gain_db,
            reply,
        } => {
            let clamped = master_gain_db.clamp(-24.0, 24.0);
            let old_db = project.borrow().master_gain_db;
            if (clamped - old_db).abs() > 1e-9 {
                let cmd: Box<dyn crate::undo::EditCommand> =
                    Box::new(crate::undo::SetProjectMasterGainCommand {
                        old_db,
                        new_db: clamped,
                    });
                {
                    let mut proj = project.borrow_mut();
                    timeline_state.borrow_mut().history.execute(cmd, &mut proj);
                }
                prog_player.borrow_mut().set_master_gain_db(clamped);
                // Note: the Loudness popover's current-gain label is
                // refreshed on next analyze / open. We can't reach the
                // popover view from the MCP dispatch scope without
                // lifting the cell further up, which Phase 1 doesn't
                // warrant.
                on_project_changed();
            }
            reply
                .send(json!({"success": true, "master_gain_db": clamped}))
                .ok();
        }

        McpCommand::NormalizeClipAudio {
            clip_id,
            mode,
            target_level,
            reply,
        } => {
            // Extract clip info from the model.
            let clip_info = {
                let proj = project.borrow();
                proj.clip_ref(&clip_id).map(|clip| {
                    (
                        clip.source_path.clone(),
                        clip.source_in,
                        clip.source_out,
                        clip.volume,
                        clip.measured_loudness_lufs,
                    )
                })
            };
            if let Some((source_path, source_in, source_out, old_volume, _old_measured)) = clip_info
            {
                // Run analysis synchronously (blocks GTK main loop for a few seconds,
                // acceptable for MCP tool calls — same pattern as export_mp4).
                let result = match mode.as_str() {
                    "peak" => {
                        crate::media::export::analyze_peak_db(&source_path, source_in, source_out)
                            .map(|peak| {
                                let gain =
                                    crate::media::export::compute_peak_gain(peak, target_level);
                                (peak, gain)
                            })
                    }
                    _ => crate::media::export::analyze_loudness_lufs(
                        &source_path,
                        source_in,
                        source_out,
                    )
                    .map(|lufs| {
                        let gain = crate::media::export::compute_lufs_gain(lufs, target_level);
                        (lufs, gain)
                    }),
                };
                match result {
                    Ok((measured, gain)) => {
                        let new_volume = (old_volume as f64 * gain).clamp(0.0, 4.0) as f32;
                        let measured_lufs = if mode == "lufs" { Some(measured) } else { None };
                        {
                            let mut proj = project.borrow_mut();
                            if let Some(clip) = proj.clip_mut(&clip_id) {
                                clip.volume = new_volume;
                                if let Some(lufs) = measured_lufs {
                                    clip.measured_loudness_lufs = Some(lufs);
                                }
                            }
                            proj.dirty = true;
                        }
                        reply
                            .send(serde_json::json!({
                                "success": true,
                                "mode": mode,
                                "measured": measured,
                                "target": target_level,
                                "gain_linear": gain,
                                "old_volume": old_volume,
                                "new_volume": new_volume,
                            }))
                            .ok();
                        on_project_changed();
                    }
                    Err(e) => {
                        reply
                            .send(serde_json::json!({
                                "success": false,
                                "error": e.to_string(),
                            }))
                            .ok();
                    }
                }
            } else {
                reply
                    .send(serde_json::json!({"success": false, "error": "Clip not found"}))
                    .ok();
            }
        }

        McpCommand::MatchClipAudio {
            source_clip_id,
            source_start_ns,
            source_end_ns,
            source_channel_mode,
            reference_clip_id,
            reference_start_ns,
            reference_end_ns,
            reference_channel_mode,
            reply,
        } => {
            let clip_info = {
                let proj = project.borrow();
                let source =
                    crate::ui::window::collect_audio_match_clip_info(&proj, &source_clip_id)
                        .ok_or_else(|| "Source clip not found.".to_string());
                let reference =
                    crate::ui::window::collect_audio_match_clip_info(&proj, &reference_clip_id)
                        .ok_or_else(|| "Reference clip not found.".to_string());
                match (source, reference) {
                    (Ok(source), Ok(reference)) => Ok((source, reference)),
                    (Err(e), _) | (_, Err(e)) => Err(e),
                }
            };
            match clip_info.and_then(|(source, reference)| {
                let source_region = if source_start_ns.is_some() || source_end_ns.is_some() {
                    Some(crate::media::audio_match::AnalysisRegionNs {
                        start_ns: source_start_ns.unwrap_or(0),
                        end_ns: source_end_ns.unwrap_or(source.duration_ns),
                    })
                } else {
                    None
                };
                let reference_region = if reference_start_ns.is_some() || reference_end_ns.is_some()
                {
                    Some(crate::media::audio_match::AnalysisRegionNs {
                        start_ns: reference_start_ns.unwrap_or(0),
                        end_ns: reference_end_ns.unwrap_or(reference.duration_ns),
                    })
                } else {
                    None
                };
                crate::ui::window::run_audio_match_for_clips(
                    &source_clip_id,
                    &source,
                    source_region,
                    source_channel_mode,
                    &reference_clip_id,
                    &reference,
                    reference_region,
                    reference_channel_mode,
                )
            }) {
                Ok(prepared) => {
                    {
                        let mut proj = project.borrow_mut();
                        let cmd = crate::undo::MatchClipAudioCommand {
                            clip_id: prepared.clip_id.clone(),
                            old_volume: prepared.old_volume,
                            new_volume: prepared.new_volume,
                            old_measured_loudness: prepared.old_measured_loudness,
                            new_measured_loudness: prepared.new_measured_loudness,
                            old_eq_bands: prepared.old_eq_bands,
                            new_eq_bands: prepared.new_eq_bands,
                            old_match_eq_bands: prepared.old_match_eq_bands.clone(),
                            new_match_eq_bands: prepared.new_match_eq_bands.clone(),
                        };
                        let mut ts = timeline_state.borrow_mut();
                        ts.history.execute(Box::new(cmd), &mut proj);
                    }
                    {
                        let mut pp = prog_player.borrow_mut();
                        pp.update_match_eq_for_clip(
                            &prepared.clip_id,
                            prepared.new_match_eq_bands.clone(),
                        );
                    }
                    reply
                        .send(serde_json::json!({
                            "success": true,
                            "source_clip_id": source_clip_id,
                            "reference_clip_id": reference_clip_id,
                            "source_range_ns": {
                                "start": prepared.source_region.start_ns,
                                "end": prepared.source_region.end_ns,
                            },
                            "source_channel_mode": prepared.source_channel_mode.as_str(),
                            "reference_range_ns": {
                                "start": prepared.reference_region.start_ns,
                                "end": prepared.reference_region.end_ns,
                            },
                            "reference_channel_mode": prepared.reference_channel_mode.as_str(),
                            "source_loudness_lufs": prepared.source_loudness_lufs,
                            "reference_loudness_lufs": prepared.reference_loudness_lufs,
                            "gain_linear": prepared.volume_gain,
                            "old_volume": prepared.old_volume,
                            "new_volume": prepared.new_volume,
                            "eq_bands": prepared.new_eq_bands.iter().map(|band| serde_json::json!({
                                "freq": band.freq,
                                "gain": band.gain,
                                "q": band.q,
                            })).collect::<Vec<_>>(),
                            "match_eq_bands": prepared.new_match_eq_bands.iter().map(|band| serde_json::json!({
                                "freq": band.freq,
                                "gain": band.gain,
                                "q": band.q,
                            })).collect::<Vec<_>>(),
                            "source_profile_db": {
                                "low": prepared.source_profile.low_db,
                                "mid": prepared.source_profile.mid_db,
                                "high": prepared.source_profile.high_db,
                            },
                            "reference_profile_db": {
                                "low": prepared.reference_profile.low_db,
                                "mid": prepared.reference_profile.mid_db,
                                "high": prepared.reference_profile.high_db,
                            }
                        }))
                        .ok();
                    on_project_changed();
                }
                Err(error) => {
                    reply
                        .send(serde_json::json!({
                            "success": false,
                            "error": error,
                        }))
                        .ok();
                }
            }
        }

        McpCommand::ClearMatchEq { clip_id, reply } => {
            let old_match_eq_bands = {
                let proj = project.borrow();
                proj.clip_ref(&clip_id).map(|c| c.match_eq_bands.clone())
            };
            match old_match_eq_bands {
                Some(old_bands) => {
                    {
                        let mut proj = project.borrow_mut();
                        let cmd = crate::undo::ClearMatchEqCommand {
                            clip_id: clip_id.clone(),
                            old_match_eq_bands: old_bands,
                        };
                        let mut ts = timeline_state.borrow_mut();
                        ts.history.execute(Box::new(cmd), &mut proj);
                    }
                    {
                        let mut pp = prog_player.borrow_mut();
                        pp.update_match_eq_for_clip(&clip_id, Vec::new());
                    }
                    on_project_changed();
                    reply
                        .send(serde_json::json!({
                            "success": true,
                            "clip_id": clip_id,
                        }))
                        .ok();
                }
                None => {
                    reply
                        .send(serde_json::json!({
                            "success": false,
                            "error": "Clip not found",
                        }))
                        .ok();
                }
            }
        }

        McpCommand::DetectSceneCuts {
            clip_id,
            track_id,
            threshold,
            reply,
        } => {
            let clip_info = {
                let proj = project.borrow();
                proj.tracks.iter().find(|t| t.id == track_id).and_then(|t| {
                    t.clips
                        .iter()
                        .find(|c| c.id == clip_id)
                        .map(|c| (c.source_path.clone(), c.source_in, c.source_out))
                })
            };
            if let Some((source_path, source_in, source_out)) = clip_info {
                let cuts = crate::media::export::detect_scene_cuts(
                    &source_path,
                    source_in,
                    source_out,
                    threshold,
                )
                .unwrap_or_default();
                let n = cuts.len();
                if !cuts.is_empty() {
                    crate::ui::window::apply_scene_cut_results(
                        &clip_id,
                        &track_id,
                        &cuts,
                        project,
                        timeline_state,
                        on_project_changed,
                        Some(window),
                    );
                }
                reply
                    .send(serde_json::json!({
                        "success": true,
                        "cuts_detected": n,
                    }))
                    .ok();
            } else {
                reply
                    .send(serde_json::json!({"success": false, "error": "Clip or track not found"}))
                    .ok();
            }
        }

        McpCommand::GenerateMusic {
            prompt,
            duration_secs,
            track_index,
            timeline_start_ns,
            reference_audio_path,
            reply,
        } => {
            let music_cache = music_gen_cache.borrow();
            if !music_cache.is_available() {
                reply
                    .send(serde_json::json!({
                        "success": false,
                        "error": "MusicGen ONNX models not found. Download musicgen-small models to ~/.local/share/ultimateslice/models/musicgen-small/"
                    }))
                    .ok();
            } else {
                drop(music_cache);
                // Find or default audio track.
                let track_id = {
                    let proj = project.borrow();
                    let audio_tracks: Vec<_> =
                        proj.tracks.iter().filter(|t| t.is_audio()).collect();
                    let track = if let Some(idx) = track_index {
                        proj.tracks.get(idx).map(|t| t.id.clone())
                    } else {
                        audio_tracks.first().map(|t| t.id.clone())
                    };
                    track
                };
                if let Some(track_id) = track_id {
                    // If a reference audio file was supplied, analyze it
                    // synchronously here (the MCP request is already
                    // serialized) and append the derived hint to the
                    // prompt.  Failures degrade gracefully — the original
                    // prompt is used and a warning is logged so the tool
                    // never fails just because the reference was unreadable.
                    let final_prompt = if let Some(ref ref_path) = reference_audio_path {
                        match crate::media::audio_features::analyze_audio_file(
                            ref_path,
                            0,
                            u64::MAX,
                        ) {
                            Ok(features) => {
                                let hint = crate::media::audio_features::features_to_prompt_hint(
                                    &features,
                                );
                                if hint.is_empty() {
                                    prompt.clone()
                                } else {
                                    let augmented = format!("{}, {}", prompt.trim(), hint);
                                    log::info!(
                                        "generate_music: reference analysis OK for {}: \
                                             augmented prompt = {:?}",
                                        ref_path,
                                        augmented
                                    );
                                    augmented
                                }
                            }
                            Err(e) => {
                                log::warn!(
                                    "generate_music: reference analysis failed for {}: {}; \
                                         falling back to original prompt",
                                    ref_path,
                                    e
                                );
                                prompt.clone()
                            }
                        }
                    } else {
                        prompt.clone()
                    };

                    let playhead_ns = timeline_state.borrow().playhead_ns;
                    let start_ns = timeline_start_ns.unwrap_or(playhead_ns);
                    let job_id = uuid::Uuid::new_v4().to_string();
                    let job = crate::media::music_gen::MusicGenJob {
                        job_id: job_id.clone(),
                        prompt: final_prompt,
                        duration_secs,
                        output_path: std::path::PathBuf::new(), // will be set by cache
                        track_id,
                        timeline_start_ns: start_ns,
                        reference_audio_path: reference_audio_path
                            .as_ref()
                            .map(std::path::PathBuf::from),
                    };
                    music_gen_cache.borrow_mut().request(job);
                    reply
                        .send(serde_json::json!({
                            "success": true,
                            "job_id": job_id,
                            "message": "Music generation started. Poll list_clips to see the clip when ready."
                        }))
                        .ok();
                } else {
                    reply
                        .send(
                            serde_json::json!({"success": false, "error": "No audio track found"}),
                        )
                        .ok();
                }
            }
        }

        McpCommand::RecordVoiceover {
            duration_ns,
            track_index,
            reply,
        } => {
            if duration_ns == 0 {
                reply
                    .send(serde_json::json!({"success": false, "error": "duration_ns must be > 0"}))
                    .ok();
            } else {
                let playhead_ns = timeline_state.borrow().playhead_ns;
                // Find or create target audio track.
                let track_id = {
                    let mut proj = project.borrow_mut();
                    if let Some(idx) = track_index {
                        proj.tracks.get(idx).map(|t| t.id.clone())
                    } else {
                        proj.tracks
                            .iter()
                            .find(|t| t.is_audio())
                            .map(|t| t.id.clone())
                            .or_else(|| {
                                let new_track = crate::model::track::Track::new_audio("Audio 1");
                                let id = new_track.id.clone();
                                proj.tracks.push(new_track);
                                Some(id)
                            })
                    }
                };
                if track_id.is_none() {
                    reply
                        .send(serde_json::json!({"success": false, "error": "Invalid track_index"}))
                        .ok();
                } else {
                    let track_id = track_id.unwrap();
                    // Record synchronously (blocks MCP thread for duration_ns).
                    let mut rec = crate::media::voiceover::VoiceoverRecorder::new();
                    match rec.start_recording(playhead_ns, None, true) {
                        Ok(file_path) => {
                            let dur_ms = duration_ns / 1_000_000;
                            std::thread::sleep(std::time::Duration::from_millis(dur_ms));
                            match rec.stop_recording() {
                                Ok((path, actual_dur_ns, start_ns)) => {
                                    let clip = crate::model::clip::Clip::new(
                                        &path,
                                        actual_dur_ns,
                                        start_ns,
                                        crate::model::clip::ClipKind::Audio,
                                    );
                                    let clip_id = clip.id.clone();
                                    {
                                        let mut proj = project.borrow_mut();
                                        if let Some(track) =
                                            proj.tracks.iter_mut().find(|t| t.id == track_id)
                                        {
                                            track.add_clip(clip);
                                        }
                                        proj.dirty = true;
                                    }
                                    on_project_changed();
                                    reply
                                        .send(serde_json::json!({
                                            "success": true,
                                            "clip_id": clip_id,
                                            "file_path": path,
                                            "duration_ns": actual_dur_ns,
                                            "timeline_start_ns": start_ns,
                                        }))
                                        .ok();
                                }
                                Err(e) => {
                                    reply.send(serde_json::json!({"success": false, "error": e.to_string()})).ok();
                                }
                            }
                        }
                        Err(e) => {
                            reply
                                .send(serde_json::json!({"success": false, "error": e.to_string()}))
                                .ok();
                        }
                    }
                } // else (track_id found)
            }
        }

        McpCommand::SetClipBlendMode {
            clip_id,
            blend_mode,
            reply,
        } => {
            let parsed = match blend_mode.as_str() {
                "normal" => Some(crate::model::clip::BlendMode::Normal),
                "multiply" => Some(crate::model::clip::BlendMode::Multiply),
                "screen" => Some(crate::model::clip::BlendMode::Screen),
                "overlay" => Some(crate::model::clip::BlendMode::Overlay),
                "add" => Some(crate::model::clip::BlendMode::Add),
                "difference" => Some(crate::model::clip::BlendMode::Difference),
                "soft_light" => Some(crate::model::clip::BlendMode::SoftLight),
                _ => None,
            };
            let Some(parsed) = parsed else {
                reply
                    .send(json!({"success": false, "error": "blend_mode must be one of: normal, multiply, screen, overlay, add, difference, soft_light"}))
                    .ok();
                return;
            };
            let mut proj = project.borrow_mut();
            let found = if let Some(clip) = proj.clip_mut(&clip_id) {
                clip.blend_mode = parsed;
                true
            } else {
                false
            };
            if found {
                proj.dirty = true;
            }
            drop(proj);
            reply
                .send(json!({"success": found, "clip_id": clip_id, "blend_mode": blend_mode}))
                .ok();
            if found {
                on_project_changed();
            }
        }

        McpCommand::SetClipKeyframe {
            clip_id,
            property,
            timeline_pos_ns,
            value,
            interpolation,
            bezier_controls,
            reply,
        } => {
            let Some(property) = Phase1KeyframeProperty::parse(&property) else {
                reply
                    .send(json!({"success": false, "error": "property must be one of: position_x, position_y, scale, opacity, brightness, contrast, saturation, temperature, tint, volume, pan, rotate, crop_left, crop_right, crop_top, crop_bottom"}))
                    .ok();
                return;
            };
            let interp = interpolation
                .as_deref()
                .map(|s| match s {
                    "ease_in" | "easeIn" => crate::model::clip::KeyframeInterpolation::EaseIn,
                    "ease_out" | "easeOut" => crate::model::clip::KeyframeInterpolation::EaseOut,
                    "ease_in_out" | "ease" | "easeInOut" => {
                        crate::model::clip::KeyframeInterpolation::EaseInOut
                    }
                    _ => crate::model::clip::KeyframeInterpolation::Linear,
                })
                .unwrap_or(crate::model::clip::KeyframeInterpolation::Linear);
            let timeline_pos_ns =
                timeline_pos_ns.unwrap_or_else(|| prog_player.borrow().timeline_pos_ns);
            let mut found = false;
            let mut keyframe_time_ns = None;
            {
                let mut proj = project.borrow_mut();
                if let Some(clip) = proj.clip_mut(&clip_id) {
                    keyframe_time_ns =
                        Some(clip.upsert_phase1_keyframe_at_timeline_ns_with_interp(
                            property,
                            timeline_pos_ns,
                            value,
                            interp,
                        ));
                    if let (Some(local_ns), Some((x1, y1, x2, y2))) =
                        (keyframe_time_ns, bezier_controls)
                    {
                        let keyframes = clip.keyframes_for_phase1_property_mut(property);
                        if let Some(kf) = keyframes.iter_mut().find(|kf| kf.time_ns == local_ns) {
                            kf.bezier_controls = Some(crate::model::clip::BezierControls {
                                x1: x1.clamp(0.0, 1.0),
                                y1: y1.clamp(0.0, 1.0),
                                x2: x2.clamp(0.0, 1.0),
                                y2: y2.clamp(0.0, 1.0),
                            });
                        }
                    }
                    proj.dirty = true;
                    found = true;
                }
            }
            reply
                .send(json!({
                    "success": found,
                    "clip_id": clip_id,
                    "property": property.as_str(),
                    "timeline_pos_ns": timeline_pos_ns,
                    "clip_local_time_ns": keyframe_time_ns,
                    "bezier_controls": bezier_controls.map(|(x1, y1, x2, y2)| json!({
                        "x1": x1.clamp(0.0, 1.0),
                        "y1": y1.clamp(0.0, 1.0),
                        "x2": x2.clamp(0.0, 1.0),
                        "y2": y2.clamp(0.0, 1.0),
                    }))
                }))
                .ok();
            if found {
                on_project_changed();
            }
        }

        McpCommand::RemoveClipKeyframe {
            clip_id,
            property,
            timeline_pos_ns,
            reply,
        } => {
            let Some(property) = Phase1KeyframeProperty::parse(&property) else {
                reply
                    .send(json!({"success": false, "error": "property must be one of: position_x, position_y, scale, opacity, brightness, contrast, saturation, temperature, tint, volume, pan, rotate, crop_left, crop_right, crop_top, crop_bottom"}))
                    .ok();
                return;
            };
            let timeline_pos_ns =
                timeline_pos_ns.unwrap_or_else(|| prog_player.borrow().timeline_pos_ns);
            let mut found = false;
            let mut removed = false;
            {
                let mut proj = project.borrow_mut();
                if let Some(clip) = proj.clip_mut(&clip_id) {
                    removed = clip.remove_phase1_keyframe_at_timeline_ns(property, timeline_pos_ns);
                    if removed {
                        proj.dirty = true;
                    }
                    found = true;
                }
            }
            reply
                .send(json!({
                    "success": found && removed,
                    "clip_id": clip_id,
                    "property": property.as_str(),
                    "timeline_pos_ns": timeline_pos_ns,
                    "removed": removed
                }))
                .ok();
            if found && removed {
                on_project_changed();
            }
        }

        McpCommand::SetTitle { title, reply } => {
            project.borrow_mut().title = title.clone();
            project.borrow_mut().dirty = true;
            reply.send(json!({"success": true})).ok();
            on_project_changed();
        }

        McpCommand::SaveFcpxml { path, reply } => {
            // Sync bin data before save.
            crate::model::media_library::sync_bins_to_project(
                &library.borrow(),
                &mut project.borrow_mut(),
            );
            let result = {
                let proj = project.borrow();
                crate::fcpxml::writer::write_fcpxml_for_path(&proj, std::path::Path::new(&path))
                    .and_then(|xml| std::fs::write(&path, xml).map_err(|e| anyhow::anyhow!(e)))
            };
            match result {
                Ok(_) => {
                    {
                        let mut proj = project.borrow_mut();
                        proj.file_path = Some(path.clone());
                        proj.dirty = false;
                    }
                    on_project_changed();
                    reply.send(json!({"success": true, "path": path})).ok()
                }
                Err(e) => reply
                    .send(json!({"success": false, "error": e.to_string()}))
                    .ok(),
            };
        }

        McpCommand::SaveEdl { path, reply } => {
            let result = {
                let proj = project.borrow();
                let edl_content = crate::edl::writer::write_edl(&proj);
                std::fs::write(&path, edl_content).map_err(|e| anyhow::anyhow!(e))
            };
            match result {
                Ok(_) => {
                    let _ = reply.send(json!({"success": true, "path": path}));
                }
                Err(e) => {
                    let _ = reply.send(json!({"success": false, "error": e.to_string()}));
                }
            }
        }

        McpCommand::SaveOtio {
            path,
            path_mode,
            reply,
        } => {
            let Some(path_mode) = crate::otio::writer::OtioMediaPathMode::from_str(&path_mode)
            else {
                let _ = reply.send(json!({
                    "success": false,
                    "error": "path_mode must be one of: absolute, relative"
                }));
                return;
            };
            let result = {
                let proj = project.borrow();
                crate::otio::writer::write_otio_to_path(
                    &proj,
                    std::path::Path::new(&path),
                    path_mode,
                )
                .and_then(|json| std::fs::write(&path, json).map_err(|e| anyhow::anyhow!(e)))
            };
            match result {
                Ok(_) => {
                    let _ = reply.send(json!({
                        "success": true,
                        "path": path,
                        "path_mode": path_mode.as_str()
                    }));
                }
                Err(e) => {
                    let _ = reply.send(json!({"success": false, "error": e.to_string()}));
                }
            }
        }

        McpCommand::SaveProjectWithMedia { path, reply } => {
            crate::model::media_library::sync_bins_to_project(
                &library.borrow(),
                &mut project.borrow_mut(),
            );
            let result = {
                let proj = project.borrow();
                crate::fcpxml::writer::export_project_with_media(&proj, std::path::Path::new(&path))
            };
            match result {
                Ok(library_path) => {
                    {
                        let mut proj = project.borrow_mut();
                        proj.file_path = Some(path.clone());
                        proj.dirty = false;
                    }
                    on_project_changed();
                    reply
                        .send(json!({
                            "success": true,
                            "path": path,
                            "library_path": library_path.to_string_lossy()
                        }))
                        .ok()
                }
                Err(e) => reply
                    .send(json!({"success": false, "error": e.to_string()}))
                    .ok(),
            };
        }

        McpCommand::CollectProjectFiles {
            directory_path,
            mode,
            use_collected_locations_on_next_save,
            reply,
        } => {
            let proj_snapshot = project.borrow().clone();
            let library_snapshot = library.borrow().items.clone();
            let result = crate::fcpxml::writer::collect_files_with_manifest(
                &proj_snapshot,
                &library_snapshot,
                std::path::Path::new(&directory_path),
                mode,
                |_| {},
            );
            match result {
                Ok(manifest) => {
                    let summary = manifest.result.clone();
                    let apply_summary = if use_collected_locations_on_next_save {
                        crate::ui::window::apply_collected_files_manifest_to_project_state(
                            project,
                            library,
                            source_marks,
                            on_source_selected,
                            on_project_changed_full,
                            &manifest,
                        )
                    } else {
                        crate::fcpxml::writer::ApplyCollectedFilesResult {
                            project_media_references_updated: 0,
                            project_lut_references_updated: 0,
                            library_items_updated: 0,
                        }
                    };
                    reply
                        .send(json!({
                            "success": true,
                            "directory_path": summary.destination_dir.to_string_lossy(),
                            "mode": mode.as_str(),
                            "use_collected_locations_on_next_save": use_collected_locations_on_next_save,
                            "project_paths_updated": apply_summary.updated_any(),
                            "project_media_references_updated": apply_summary.project_media_references_updated,
                            "project_lut_references_updated": apply_summary.project_lut_references_updated,
                            "library_items_updated": apply_summary.library_items_updated,
                            "media_files": summary.media_files_copied,
                            "lut_files": summary.lut_files_copied,
                            "total_files": summary.total_files_copied()
                        }))
                        .ok();
                }
                Err(e) => {
                    reply
                        .send(json!({"success": false, "error": e.to_string()}))
                        .ok();
                }
            }
        }

        McpCommand::OpenFcpxml { path, reply } => {
            let (tx, rx) = std::sync::mpsc::sync_channel::<Result<Project, String>>(1);
            let path_bg = path.clone();
            std::thread::spawn(move || {
                let result = std::fs::read_to_string(&path_bg)
                    .map_err(|e| e.to_string())
                    .and_then(|xml| {
                        crate::fcpxml::parser::parse_fcpxml_with_path(
                            &xml,
                            Some(std::path::Path::new(&path_bg)),
                        )
                        .map_err(|e| e.to_string())
                    });
                let _ = tx.send(result);
            });
            timeline_state.borrow_mut().loading = true;
            let project = project.clone();
            let timeline_state = timeline_state.clone();
            let main_stack = main_stack.clone();
            let on_project_changed = on_project_changed_full.clone();
            let suppress_resume_on_next_reload = suppress_resume_on_next_reload.clone();
            let clear_media_browser_on_next_reload = clear_media_browser_on_next_reload.clone();
            glib::timeout_add_local(std::time::Duration::from_millis(10), move || {
                match rx.try_recv() {
                    Ok(Ok(mut new_proj)) => {
                        new_proj.file_path = Some(path.clone());
                        let track_count = new_proj.tracks.len();
                        let clip_count: usize = new_proj.tracks.iter().map(|t| t.clips.len()).sum();
                        *project.borrow_mut() = new_proj;
                        timeline_state.borrow_mut().loading = false;
                        main_stack.set_visible_child_name("editor");
                        reply.send(json!({"success": true, "path": path, "tracks": track_count, "clips": clip_count})).ok();
                        suppress_resume_on_next_reload.set(true);
                        clear_media_browser_on_next_reload.set(true);
                        let on_project_changed = on_project_changed.clone();
                        glib::timeout_add_local_once(
                            std::time::Duration::from_millis(0),
                            move || {
                                on_project_changed();
                            },
                        );
                        glib::ControlFlow::Break
                    }
                    Ok(Err(e)) => {
                        timeline_state.borrow_mut().loading = false;
                        reply.send(json!({"success": false, "error": e})).ok();
                        glib::ControlFlow::Break
                    }
                    Err(std::sync::mpsc::TryRecvError::Empty) => glib::ControlFlow::Continue,
                    Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                        timeline_state.borrow_mut().loading = false;
                        reply.send(json!({"success": false, "error": "open_fcpxml worker disconnected"})).ok();
                        glib::ControlFlow::Break
                    }
                }
            });
        }

        McpCommand::OpenOtio { path, reply } => {
            let (tx, rx) = std::sync::mpsc::sync_channel::<Result<Project, String>>(1);
            let path_bg = std::path::PathBuf::from(&path);
            std::thread::spawn(move || {
                let result = crate::ui::project_loader::load_project_from_path(&path_bg);
                let _ = tx.send(result);
            });
            timeline_state.borrow_mut().loading = true;
            let project = project.clone();
            let timeline_state = timeline_state.clone();
            let main_stack = main_stack.clone();
            let on_project_changed = on_project_changed_full.clone();
            let suppress_resume_on_next_reload = suppress_resume_on_next_reload.clone();
            let clear_media_browser_on_next_reload = clear_media_browser_on_next_reload.clone();
            glib::timeout_add_local(std::time::Duration::from_millis(10), move || {
                match rx.try_recv() {
                    Ok(Ok(mut new_proj)) => {
                        new_proj.file_path = Some(path.clone());
                        let track_count = new_proj.tracks.len();
                        let clip_count: usize = new_proj.tracks.iter().map(|t| t.clips.len()).sum();
                        *project.borrow_mut() = new_proj;
                        timeline_state.borrow_mut().loading = false;
                        main_stack.set_visible_child_name("editor");
                        reply.send(json!({"success": true, "path": path, "tracks": track_count, "clips": clip_count})).ok();
                        suppress_resume_on_next_reload.set(true);
                        clear_media_browser_on_next_reload.set(true);
                        let on_project_changed = on_project_changed.clone();
                        glib::timeout_add_local_once(
                            std::time::Duration::from_millis(0),
                            move || {
                                on_project_changed();
                            },
                        );
                        glib::ControlFlow::Break
                    }
                    Ok(Err(e)) => {
                        timeline_state.borrow_mut().loading = false;
                        reply.send(json!({"success": false, "error": e})).ok();
                        glib::ControlFlow::Break
                    }
                    Err(std::sync::mpsc::TryRecvError::Empty) => glib::ControlFlow::Continue,
                    Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                        timeline_state.borrow_mut().loading = false;
                        reply
                            .send(
                                json!({"success": false, "error": "open_otio worker disconnected"}),
                            )
                            .ok();
                        glib::ControlFlow::Break
                    }
                }
            });
        }

        McpCommand::ExportMp4 {
            path,
            audio_channel_layout,
            reply,
        } => {
            let proj = project.borrow().clone();
            let bg_paths = bg_removal_cache.borrow().paths.clone();
            let interp_paths = frame_interp_cache.borrow().snapshot_paths_by_clip_id(&proj);
            let layout = crate::media::export::AudioChannelLayout::from_str(&audio_channel_layout);
            std::thread::spawn(move || {
                let (done_tx, done_rx) = std::sync::mpsc::sync_channel::<Result<(), String>>(1);
                let proj_worker = proj.clone();
                let path_worker = path.clone();
                std::thread::spawn(move || {
                    let (tx, _rx) = std::sync::mpsc::channel();
                    let options = crate::media::export::ExportOptions {
                        audio_channel_layout: layout,
                        hdr_passthrough: false,
                        ..crate::media::export::ExportOptions::default()
                    };
                    let result = crate::media::export::export_project(
                        &proj_worker,
                        &path_worker,
                        options,
                        None,
                        &bg_paths,
                        &interp_paths,
                        tx,
                    )
                    .map_err(|e| e.to_string())
                    .map(|_| ());
                    let _ = done_tx.send(result);
                });

                match done_rx.recv_timeout(std::time::Duration::from_secs(660)) {
                    Ok(Ok(())) => {
                        let _ = reply.send(json!({"success": true, "path": path}));
                    }
                    Ok(Err(e)) => {
                        let _ = reply.send(json!({"success": false, "error": e}));
                    }
                    Err(_) => {
                        let _ = reply.send(json!({
                            "success": false,
                            "error": "MP4 export timed out after 11 minutes (export thread still running)"
                        }));
                    }
                }
            });
        }

        McpCommand::ListExportPresets { reply } => {
            let state = crate::ui_state::load_export_presets_state();
            let presets: Vec<_> = state
                .presets
                .iter()
                .map(|preset| {
                    let options = preset.to_export_options();
                    json!({
                        "name": preset.name,
                        "video_codec": match options.video_codec {
                            crate::media::export::VideoCodec::H264 => "h264",
                            crate::media::export::VideoCodec::H265 => "h265",
                            crate::media::export::VideoCodec::Vp9 => "vp9",
                            crate::media::export::VideoCodec::ProRes => "prores",
                            crate::media::export::VideoCodec::Av1 => "av1",
                        },
                        "container": match options.container {
                            crate::media::export::Container::Mp4 => "mp4",
                            crate::media::export::Container::Mov => "mov",
                            crate::media::export::Container::WebM => "webm",
                            crate::media::export::Container::Mkv => "mkv",
                            crate::media::export::Container::Gif => "gif",
                        },
                        "output_width": options.output_width,
                        "output_height": options.output_height,
                        "crf": options.crf,
                        "audio_codec": match options.audio_codec {
                            crate::media::export::AudioCodec::Aac => "aac",
                            crate::media::export::AudioCodec::Opus => "opus",
                            crate::media::export::AudioCodec::Flac => "flac",
                            crate::media::export::AudioCodec::Pcm => "pcm",
                        },
                        "audio_bitrate_kbps": options.audio_bitrate_kbps,
                        "audio_channel_layout": options.audio_channel_layout.as_str(),
                    })
                })
                .collect();
            reply
                .send(json!({"presets": presets, "last_used_preset": state.last_used_preset}))
                .ok();
        }

        McpCommand::SaveExportPreset {
            name,
            video_codec,
            container,
            output_width,
            output_height,
            crf,
            audio_codec,
            audio_bitrate_kbps,
            audio_channel_layout,
            reply,
        } => {
            let video_codec = match video_codec.as_str() {
                "h264" => crate::media::export::VideoCodec::H264,
                "h265" => crate::media::export::VideoCodec::H265,
                "vp9" => crate::media::export::VideoCodec::Vp9,
                "prores" => crate::media::export::VideoCodec::ProRes,
                "av1" => crate::media::export::VideoCodec::Av1,
                _ => {
                    reply
                        .send(json!({"success": false, "error": "video_codec must be one of: h264, h265, vp9, prores, av1"}))
                        .ok();
                    return;
                }
            };
            let container = match container.as_str() {
                "mp4" => crate::media::export::Container::Mp4,
                "mov" => crate::media::export::Container::Mov,
                "webm" => crate::media::export::Container::WebM,
                "mkv" => crate::media::export::Container::Mkv,
                "gif" => crate::media::export::Container::Gif,
                _ => {
                    reply
                        .send(json!({"success": false, "error": "container must be one of: mp4, mov, webm, mkv, gif"}))
                        .ok();
                    return;
                }
            };
            let audio_codec = match audio_codec.as_str() {
                "aac" => crate::media::export::AudioCodec::Aac,
                "opus" => crate::media::export::AudioCodec::Opus,
                "flac" => crate::media::export::AudioCodec::Flac,
                "pcm" => crate::media::export::AudioCodec::Pcm,
                _ => {
                    reply
                        .send(json!({"success": false, "error": "audio_codec must be one of: aac, opus, flac, pcm"}))
                        .ok();
                    return;
                }
            };
            if crf > 51 {
                reply
                    .send(json!({"success": false, "error": "crf must be between 0 and 51"}))
                    .ok();
                return;
            }
            let layout = crate::media::export::AudioChannelLayout::from_str(&audio_channel_layout);
            let options = crate::media::export::ExportOptions {
                video_codec,
                container,
                output_width,
                output_height,
                crf,
                audio_codec,
                audio_bitrate_kbps,
                gif_fps: None,
                audio_channel_layout: layout,
                hdr_passthrough: false,
            };
            let mut state = crate::ui_state::load_export_presets_state();
            match state.upsert_preset(crate::ui_state::ExportPreset::from_export_options(
                name, &options,
            )) {
                Ok(()) => {
                    let saved_name = state.last_used_preset.clone();
                    crate::ui_state::save_export_presets_state(&state);
                    reply
                        .send(json!({"success": true, "name": saved_name}))
                        .ok();
                }
                Err(e) => {
                    reply.send(json!({"success": false, "error": e})).ok();
                }
            }
        }

        McpCommand::DeleteExportPreset { name, reply } => {
            let mut state = crate::ui_state::load_export_presets_state();
            let removed = state.delete_preset(&name);
            if removed {
                crate::ui_state::save_export_presets_state(&state);
            }
            reply.send(json!({"success": removed, "name": name})).ok();
        }

        McpCommand::ListWorkspaceLayouts { reply } => {
            let state = workspace_layouts_state.borrow().clone();
            let current = serde_json::to_value(&state.current).unwrap_or(json!(null));
            let layouts = serde_json::to_value(&state.layouts).unwrap_or(json!([]));
            reply
                .send(json!({
                    "current": current,
                    "layouts": layouts,
                    "active_layout": state.active_layout
                }))
                .ok();
        }

        McpCommand::SaveWorkspaceLayout { name, reply } => {
            let arrangement = capture_workspace_arrangement();
            let result = {
                let mut state = workspace_layouts_state.borrow_mut();
                state.set_current_arrangement(arrangement.clone());
                state.upsert_layout(crate::ui_state::WorkspaceLayout { name, arrangement })
            };
            match result {
                Ok(()) => {
                    let state = workspace_layouts_state.borrow();
                    crate::ui_state::save_workspace_layouts_state(&state);
                    drop(state);
                    sync_workspace_layout_controls();
                    reply
                        .send(json!({
                            "success": true,
                            "name": workspace_layouts_state.borrow().active_layout.clone()
                        }))
                        .ok();
                }
                Err(error) => {
                    reply.send(json!({"success": false, "error": error})).ok();
                }
            }
        }

        McpCommand::ApplyWorkspaceLayout { name, reply } => {
            let arrangement = {
                let state = workspace_layouts_state.borrow();
                state
                    .get_layout(&name)
                    .map(|layout| layout.arrangement.clone())
            };
            let Some(arrangement) = arrangement else {
                reply
                    .send(json!({"success": false, "error": format!("Workspace layout not found: {name}")}))
                    .ok();
                return;
            };
            *workspace_layout_pending_name.borrow_mut() = Some(name.clone());
            apply_workspace_arrangement(arrangement);
            reply.send(json!({"success": true, "name": name})).ok();
        }

        McpCommand::RenameWorkspaceLayout {
            old_name,
            new_name,
            reply,
        } => {
            let result = {
                let mut state = workspace_layouts_state.borrow_mut();
                state.rename_layout(&old_name, &new_name)
            };
            match result {
                Ok(saved_name) => {
                    let state = workspace_layouts_state.borrow();
                    crate::ui_state::save_workspace_layouts_state(&state);
                    drop(state);
                    sync_workspace_layout_controls();
                    reply
                        .send(json!({"success": true, "name": saved_name}))
                        .ok();
                }
                Err(error) => {
                    reply.send(json!({"success": false, "error": error})).ok();
                }
            }
        }

        McpCommand::DeleteWorkspaceLayout { name, reply } => {
            let deleted = {
                let mut state = workspace_layouts_state.borrow_mut();
                state.delete_layout(&name)
            };
            if deleted {
                let state = workspace_layouts_state.borrow();
                crate::ui_state::save_workspace_layouts_state(&state);
                drop(state);
                sync_workspace_layout_controls();
            }
            reply.send(json!({"success": deleted, "name": name})).ok();
        }

        McpCommand::ResetWorkspaceLayout { reply } => {
            *workspace_layout_pending_name.borrow_mut() = None;
            apply_workspace_arrangement(crate::ui_state::WorkspaceArrangement::default());
            reply.send(json!({"success": true})).ok();
        }

        McpCommand::ExportWithPreset {
            path,
            preset_name,
            reply,
        } => {
            let state = crate::ui_state::load_export_presets_state();
            let Some(preset) = state.get_preset(&preset_name).cloned() else {
                reply
                    .send(json!({"success": false, "error": format!("Export preset not found: {preset_name}")}))
                    .ok();
                return;
            };
            let options = preset.to_export_options();
            let proj = project.borrow().clone();
            let bg_paths = bg_removal_cache.borrow().paths.clone();
            let interp_paths = frame_interp_cache.borrow().snapshot_paths_by_clip_id(&proj);
            std::thread::spawn(move || {
                let (done_tx, done_rx) = std::sync::mpsc::sync_channel::<Result<(), String>>(1);
                let proj_worker = proj.clone();
                let path_worker = path.clone();
                std::thread::spawn(move || {
                    let (tx, _rx) = std::sync::mpsc::channel();
                    let result = crate::media::export::export_project(
                        &proj_worker,
                        &path_worker,
                        options,
                        None,
                        &bg_paths,
                        &interp_paths,
                        tx,
                    )
                    .map_err(|e| e.to_string())
                    .map(|_| ());
                    let _ = done_tx.send(result);
                });

                match done_rx.recv_timeout(std::time::Duration::from_secs(660)) {
                    Ok(Ok(())) => {
                        let _ = reply.send(
                            json!({"success": true, "path": path, "preset_name": preset_name}),
                        );
                    }
                    Ok(Err(e)) => {
                        let _ = reply.send(json!({"success": false, "error": e}));
                    }
                    Err(_) => {
                        let _ = reply.send(json!({
                            "success": false,
                            "error": "export_with_preset timed out after 11 minutes (export thread still running)"
                        }));
                    }
                }
            });
        }

        McpCommand::ListLibrary { search_text, reply } => {
            fn library_clip_kind_id(kind: &ClipKind) -> &'static str {
                match kind {
                    ClipKind::Video => "video",
                    ClipKind::Audio => "audio",
                    ClipKind::Image => "image",
                    ClipKind::Title => "title",
                    ClipKind::Adjustment => "adjustment",
                    ClipKind::Compound => "compound",
                    ClipKind::Multicam => "multicam",
                    ClipKind::Audition => "audition",
                    ClipKind::Drawing => "drawing",
                }
            }

            let lib = library.borrow();
            let query = search_text
                .as_deref()
                .filter(|text| !text.trim().is_empty());
            let mut filtered_items: Vec<_> = lib.items.iter().collect();
            if let Some(query) = query {
                filtered_items.retain(|item| {
                    crate::model::media_library::media_search_match(item, query).is_some()
                });
                filtered_items.sort_by(|a, b| {
                    let a_match = crate::model::media_library::media_search_match(a, query);
                    let b_match = crate::model::media_library::media_search_match(b, query);
                    b_match
                        .as_ref()
                        .map(|m| m.score)
                        .unwrap_or_default()
                        .cmp(&a_match.as_ref().map(|m| m.score).unwrap_or_default())
                        .then_with(|| {
                            crate::model::media_library::media_display_name(a)
                                .cmp(&crate::model::media_library::media_display_name(b))
                        })
                        .then_with(|| a.source_path.cmp(&b.source_path))
                });
            }
            let items: Vec<_> = filtered_items
                .into_iter()
                .map(|item| {
                    let transcript_segment_count = item
                        .transcript_windows
                        .iter()
                        .map(|window| window.segments.len())
                        .sum::<usize>();
                    let search_match = query.and_then(|query| {
                        crate::model::media_library::media_search_match(item, query)
                    });
                    json!({
                        "id":          item.id,
                        "library_key": item.library_key(),
                        "label":       item.label,
                        "source_path": item.source_path,
                        "duration_ns": item.duration_ns,
                        "duration_s":  item.duration_ns as f64 / 1_000_000_000.0,
                        "is_audio_only": item.is_audio_only,
                        "has_audio": item.has_audio,
                        "is_image": item.is_image,
                        "is_animated_svg": item.is_animated_svg,
                        "video_width": item.video_width,
                        "video_height": item.video_height,
                        "frame_rate_num": item.frame_rate_num,
                        "frame_rate_den": item.frame_rate_den,
                        "codec_summary": item.codec_summary,
                        "file_size_bytes": item.file_size_bytes,
                        "clip_kind": item.clip_kind.as_ref().map(library_clip_kind_id),
                        "title_text": item.title_text,
                        "is_missing": item.is_missing,
                        "bin_id": item.bin_id,
                        "rating": crate::ui::window::media_rating_id(item.rating),
                        "keyword_ranges": item.keyword_ranges.iter().map(|range| json!({
                            "id": range.id,
                            "label": range.label,
                            "start_ns": range.start_ns,
                            "end_ns": range.end_ns,
                            "start_s": range.start_ns as f64 / 1_000_000_000.0,
                            "end_s": range.end_ns as f64 / 1_000_000_000.0,
                        })).collect::<Vec<_>>(),
                        "auto_tags_indexed": item.auto_tags_indexed,
                        "auto_tags": item.auto_tags.iter().map(|tag| json!({
                            "category": tag.category,
                            "label": tag.label,
                            "confidence": tag.confidence,
                            "best_frame_time_ns": tag.best_frame_time_ns,
                            "best_frame_time_s": tag.best_frame_time_ns.map(|v| v as f64 / 1_000_000_000.0),
                        })).collect::<Vec<_>>(),
                        "transcript_window_count": item.transcript_windows.len(),
                        "transcript_segment_count": transcript_segment_count,
                        "search_match": search_match.as_ref().map(|m| json!({
                            "field": m.field,
                            "score": m.score,
                            "excerpt": m.excerpt,
                            "source_in_ns": m.source_in_ns,
                            "source_out_ns": m.source_out_ns,
                            "source_in_s": m.source_in_ns.map(|v| v as f64 / 1_000_000_000.0),
                            "source_out_s": m.source_out_ns.map(|v| v as f64 / 1_000_000_000.0),
                        })),
                    })
                })
                .collect();
            reply.send(json!(items)).ok();
        }

        McpCommand::ImportMedia { path, reply } => {
            let metadata = crate::media::probe_cache::probe_media_metadata(&path);
            let duration_ns = metadata.duration_ns.unwrap_or(10 * 1_000_000_000);
            let audio_only = metadata.is_audio_only;
            let has_audio = metadata.has_audio;
            let source_timecode_base_ns = metadata.source_timecode_base_ns.or_else(|| {
                let lib = library.borrow();
                let proj = project.borrow();
                crate::ui::window::lookup_source_timecode_base_ns(&lib.items, &proj, &path)
            });
            let mut item = MediaItem::new(path.clone(), duration_ns);
            item.is_audio_only = audio_only;
            item.has_audio = has_audio;
            item.source_timecode_base_ns = source_timecode_base_ns;
            item.is_image = metadata.is_image;
            item.is_animated_svg = metadata.is_animated_svg;
            item.video_width = metadata.video_width;
            item.video_height = metadata.video_height;
            item.frame_rate_num = metadata.frame_rate_num;
            item.frame_rate_den = metadata.frame_rate_den;
            item.codec_summary = metadata.codec_summary.clone();
            item.file_size_bytes = metadata.file_size_bytes;
            let label = item.label.clone();
            library.borrow_mut().items.push(item);
            {
                let proj = project.borrow();
                let mut lib = library.borrow_mut();
                let mut st = timeline_state.borrow_mut();
                crate::ui::window::refresh_media_availability_state(
                    &proj,
                    lib.items.as_mut_slice(),
                    &mut st,
                );
            }
            reply
                .send(json!({
                    "success": true,
                    "label": label,
                    "duration_ns": duration_ns,
                    "is_audio_only": audio_only,
                    "has_audio": has_audio,
                    "source_timecode_base_ns": source_timecode_base_ns,
                    "video_width": metadata.video_width,
                    "video_height": metadata.video_height,
                    "frame_rate_num": metadata.frame_rate_num,
                    "frame_rate_den": metadata.frame_rate_den,
                    "codec_summary": metadata.codec_summary,
                    "file_size_bytes": metadata.file_size_bytes,
                    "is_missing": !crate::model::media_library::source_path_exists(&path)
                }))
                .ok();
            sync_library_change();
        }

        McpCommand::RelinkMedia { root_path, reply } => {
            let root = std::path::PathBuf::from(&root_path);
            if !root.is_dir() {
                reply
                    .send(json!({"success": false, "error": "root_path must be an existing directory"}))
                    .ok();
                return;
            }
            let summary = {
                let mut proj = project.borrow_mut();
                let mut lib = library.borrow_mut();
                crate::ui::window::relink_missing_media_under_root(
                    &mut proj,
                    lib.items.as_mut_slice(),
                    &root,
                )
            };
            {
                let proj = project.borrow();
                let mut lib = library.borrow_mut();
                let mut st = timeline_state.borrow_mut();
                crate::ui::window::refresh_media_availability_state(
                    &proj,
                    lib.items.as_mut_slice(),
                    &mut st,
                );
            }
            reply
                .send(json!({
                    "success": true,
                    "root_path": root_path,
                    "scanned_files": summary.scanned_files,
                    "updated_clip_count": summary.updated_clip_count,
                    "updated_library_count": summary.updated_library_count,
                    "remapped": summary.remapped.iter().map(|(old_path, new_path)| json!({
                        "old_path": old_path,
                        "new_path": new_path
                    })).collect::<Vec<_>>(),
                    "unresolved": summary.unresolved,
                }))
                .ok();
            if summary.updated_clip_count > 0 || summary.updated_library_count > 0 {
                sync_library_change();
            }
        }

        McpCommand::CreateBin {
            name,
            parent_id,
            reply,
        } => {
            use crate::model::media_library::MediaBin;
            let mut lib = library.borrow_mut();
            // Enforce max depth of 2
            if let Some(ref pid) = parent_id {
                let parent_depth = lib
                    .bins
                    .iter()
                    .find(|b| &b.id == pid)
                    .map(|b| b.depth(&lib.bins))
                    .unwrap_or(0);
                if parent_depth >= 2 {
                    reply
                        .send(json!({"error": "Maximum bin nesting depth (2) reached"}))
                        .ok();
                    return;
                }
                if !lib.bins.iter().any(|b| &b.id == pid) {
                    reply.send(json!({"error": "Parent bin not found"})).ok();
                    return;
                }
            }
            let bin = MediaBin::new(&name, parent_id.clone());
            let id = bin.id.clone();
            lib.bins.push(bin);
            drop(lib);
            reply
                .send(json!({"success": true, "id": id, "name": name, "parent_id": parent_id}))
                .ok();
            sync_library_change();
        }

        McpCommand::DeleteBin { bin_id, reply } => {
            let mut lib = library.borrow_mut();
            let bin = lib.bins.iter().find(|b| b.id == bin_id);
            if bin.is_none() {
                reply.send(json!({"error": "Bin not found"})).ok();
                return;
            }
            let parent_id = bin.unwrap().parent_id.clone();
            // Move items to parent/root
            for item in lib.items.iter_mut() {
                if item.bin_id.as_deref() == Some(&bin_id) {
                    item.bin_id = parent_id.clone();
                }
            }
            // Reparent child bins
            let child_ids: Vec<String> = lib
                .bins
                .iter()
                .filter(|b| b.parent_id.as_deref() == Some(&bin_id))
                .map(|b| b.id.clone())
                .collect();
            for cid in child_ids {
                if let Some(cb) = lib.bins.iter_mut().find(|b| b.id == cid) {
                    cb.parent_id = parent_id.clone();
                }
            }
            lib.bins.retain(|b| b.id != bin_id);
            drop(lib);
            reply.send(json!({"success": true})).ok();
            sync_library_change();
        }

        McpCommand::RenameBin {
            bin_id,
            name,
            reply,
        } => {
            let mut lib = library.borrow_mut();
            if let Some(bin) = lib.bins.iter_mut().find(|b| b.id == bin_id) {
                bin.name = name.clone();
                drop(lib);
                reply
                    .send(json!({"success": true, "bin_id": bin_id, "name": name}))
                    .ok();
                sync_library_change();
            } else {
                reply.send(json!({"error": "Bin not found"})).ok();
            }
        }

        McpCommand::ListBins { reply } => {
            let lib = library.borrow();
            let bins: Vec<_> = lib
                .bins
                .iter()
                .map(|b| {
                    let item_count = lib
                        .items
                        .iter()
                        .filter(|i| i.bin_id.as_deref() == Some(&b.id))
                        .count();
                    json!({
                        "id": b.id,
                        "name": b.name,
                        "parent_id": b.parent_id,
                        "item_count": item_count,
                    })
                })
                .collect();
            reply.send(json!(bins)).ok();
        }

        McpCommand::MoveToBin {
            source_paths,
            bin_id,
            reply,
        } => {
            let mut lib = library.borrow_mut();
            // Validate bin exists if specified
            if let Some(ref bid) = bin_id {
                if !lib.bins.iter().any(|b| &b.id == bid) {
                    reply.send(json!({"error": "Target bin not found"})).ok();
                    return;
                }
            }
            let mut moved = 0usize;
            for item in lib.items.iter_mut() {
                if source_paths.contains(&item.source_path) {
                    item.bin_id = bin_id.clone();
                    moved += 1;
                }
            }
            drop(lib);
            reply
                .send(json!({"success": true, "moved_count": moved}))
                .ok();
            sync_library_change();
        }

        McpCommand::ListCollections { reply } => {
            let lib = library.borrow();
            let collections: Vec<_> = lib
                .collections
                .iter()
                .map(|collection| {
                    json!({
                        "id": collection.id,
                        "name": collection.name,
                        "criteria": {
                            "search_text": collection.criteria.search_text,
                            "kind": crate::ui::window::media_kind_filter_id(collection.criteria.kind),
                            "resolution": crate::ui::window::resolution_filter_id(collection.criteria.resolution),
                            "frame_rate": crate::ui::window::frame_rate_filter_id(collection.criteria.frame_rate),
                            "rating": crate::ui::window::media_rating_filter_id(collection.criteria.rating),
                        },
                        "item_count": lib.items_in_collection(&collection.id).len(),
                    })
                })
                .collect();
            reply.send(json!(collections)).ok();
        }

        McpCommand::CreateCollection {
            name,
            search_text,
            kind,
            resolution,
            frame_rate,
            rating,
            reply,
        } => {
            let criteria = match crate::ui::window::collection_criteria_from_mcp(
                search_text,
                kind,
                resolution,
                frame_rate,
                rating,
            ) {
                Ok(criteria) => criteria,
                Err(error) => {
                    reply.send(json!({"error": error})).ok();
                    return;
                }
            };
            let mut lib = library.borrow_mut();
            let collection = MediaCollection::new(name.clone(), criteria);
            let id = collection.id.clone();
            lib.collections.push(collection);
            drop(lib);
            reply
                .send(json!({"success": true, "id": id, "name": name}))
                .ok();
            sync_library_change();
        }

        McpCommand::UpdateCollection {
            collection_id,
            name,
            search_text,
            kind,
            resolution,
            frame_rate,
            rating,
            reply,
        } => {
            let mut lib = library.borrow_mut();
            let Some(collection) = lib
                .collections
                .iter_mut()
                .find(|collection| collection.id == collection_id)
            else {
                reply.send(json!({"error": "Collection not found"})).ok();
                return;
            };
            if let Some(name) = name {
                let trimmed = name.trim();
                if trimmed.is_empty() {
                    reply
                        .send(json!({"error": "Collection name cannot be empty"}))
                        .ok();
                    return;
                }
                collection.name = trimmed.to_string();
            }
            let mut criteria = collection.criteria.clone();
            if let Some(search_text) = search_text {
                criteria.search_text = search_text;
            }
            if let Some(kind) = kind {
                let Some(parsed) = crate::ui::window::parse_media_kind_filter(Some(kind.as_str()))
                else {
                    reply.send(json!({"error": "invalid kind filter"})).ok();
                    return;
                };
                criteria.kind = parsed;
            }
            if let Some(resolution) = resolution {
                let Some(parsed) =
                    crate::ui::window::parse_resolution_filter(Some(resolution.as_str()))
                else {
                    reply
                        .send(json!({"error": "invalid resolution filter"}))
                        .ok();
                    return;
                };
                criteria.resolution = parsed;
            }
            if let Some(frame_rate) = frame_rate {
                let Some(parsed) =
                    crate::ui::window::parse_frame_rate_filter(Some(frame_rate.as_str()))
                else {
                    reply
                        .send(json!({"error": "invalid frame_rate filter"}))
                        .ok();
                    return;
                };
                criteria.frame_rate = parsed;
            }
            if let Some(rating) = rating {
                let Some(parsed) =
                    crate::ui::window::parse_media_rating_filter(Some(rating.as_str()))
                else {
                    reply.send(json!({"error": "invalid rating filter"})).ok();
                    return;
                };
                criteria.rating = parsed;
            }
            collection.criteria = criteria;
            let name = collection.name.clone();
            drop(lib);
            reply
                .send(json!({"success": true, "collection_id": collection_id, "name": name}))
                .ok();
            sync_library_change();
        }

        McpCommand::DeleteCollection {
            collection_id,
            reply,
        } => {
            let mut lib = library.borrow_mut();
            let initial_len = lib.collections.len();
            lib.collections
                .retain(|collection| collection.id != collection_id);
            if lib.collections.len() == initial_len {
                reply.send(json!({"error": "Collection not found"})).ok();
                return;
            }
            drop(lib);
            reply.send(json!({"success": true})).ok();
            sync_library_change();
        }

        McpCommand::SetMediaRating {
            library_key,
            rating,
            reply,
        } => {
            let parsed_rating = match rating.as_str() {
                "none" => MediaRating::None,
                "favorite" => MediaRating::Favorite,
                "reject" => MediaRating::Reject,
                _ => {
                    reply
                        .send(json!({"success": false, "error": "rating must be one of: none, favorite, reject"}))
                        .ok();
                    return;
                }
            };
            let updated = {
                let mut lib = library.borrow_mut();
                lib.items
                    .iter_mut()
                    .find(|item| item.matches_library_key(&library_key))
                    .map(|item| {
                        item.rating = parsed_rating;
                    })
                    .is_some()
            };
            if !updated {
                reply
                    .send(json!({"success": false, "error": "Library item not found"}))
                    .ok();
                return;
            }
            reply
                .send(json!({
                    "success": true,
                    "library_key": library_key,
                    "rating": crate::ui::window::media_rating_id(parsed_rating),
                }))
                .ok();
            sync_library_change();
        }

        McpCommand::AddMediaKeywordRange {
            library_key,
            label,
            start_ns,
            end_ns,
            reply,
        } => {
            let trimmed_label = label.trim().to_string();
            if trimmed_label.is_empty() {
                reply
                    .send(json!({"success": false, "error": "label cannot be empty"}))
                    .ok();
                return;
            }
            if end_ns <= start_ns {
                reply
                    .send(
                        json!({"success": false, "error": "end_ns must be greater than start_ns"}),
                    )
                    .ok();
                return;
            }
            let added_range = {
                let mut lib = library.borrow_mut();
                lib.items
                    .iter_mut()
                    .find(|item| item.matches_library_key(&library_key))
                    .map(|item| {
                        let range = MediaKeywordRange::new(trimmed_label.clone(), start_ns, end_ns);
                        item.keyword_ranges.push(range.clone());
                        range
                    })
            };
            let Some(added_range) = added_range else {
                reply
                    .send(json!({"success": false, "error": "Library item not found"}))
                    .ok();
                return;
            };
            reply
                .send(json!({
                    "success": true,
                    "library_key": library_key,
                    "range": {
                        "id": added_range.id,
                        "label": added_range.label,
                        "start_ns": added_range.start_ns,
                        "end_ns": added_range.end_ns,
                    }
                }))
                .ok();
            sync_library_change();
        }

        McpCommand::UpdateMediaKeywordRange {
            library_key,
            range_id,
            label,
            start_ns,
            end_ns,
            reply,
        } => {
            let trimmed_label = label.trim().to_string();
            if trimmed_label.is_empty() {
                reply
                    .send(json!({"success": false, "error": "label cannot be empty"}))
                    .ok();
                return;
            }
            if end_ns <= start_ns {
                reply
                    .send(
                        json!({"success": false, "error": "end_ns must be greater than start_ns"}),
                    )
                    .ok();
                return;
            }
            let updated = {
                let mut lib = library.borrow_mut();
                lib.items
                    .iter_mut()
                    .find(|item| item.matches_library_key(&library_key))
                    .and_then(|item| {
                        item.keyword_ranges
                            .iter_mut()
                            .find(|range| range.id == range_id)
                    })
                    .map(|range| {
                        range.label = trimmed_label.clone();
                        range.start_ns = start_ns;
                        range.end_ns = end_ns;
                    })
                    .is_some()
            };
            if !updated {
                reply
                    .send(json!({"success": false, "error": "Keyword range not found"}))
                    .ok();
                return;
            }
            reply
                .send(json!({
                    "success": true,
                    "library_key": library_key,
                    "range_id": range_id,
                    "label": trimmed_label,
                    "start_ns": start_ns,
                    "end_ns": end_ns,
                }))
                .ok();
            sync_library_change();
        }

        McpCommand::DeleteMediaKeywordRange {
            library_key,
            range_id,
            reply,
        } => {
            let deleted = {
                let mut lib = library.borrow_mut();
                if let Some(item) = lib
                    .items
                    .iter_mut()
                    .find(|item| item.matches_library_key(&library_key))
                {
                    let before = item.keyword_ranges.len();
                    item.keyword_ranges.retain(|range| range.id != range_id);
                    item.keyword_ranges.len() != before
                } else {
                    false
                }
            };
            if !deleted {
                reply
                    .send(json!({"success": false, "error": "Keyword range not found"}))
                    .ok();
                return;
            }
            reply
                .send(json!({
                    "success": true,
                    "library_key": library_key,
                    "range_id": range_id,
                }))
                .ok();
            sync_library_change();
        }

        McpCommand::ReorderTrack {
            from_index,
            to_index,
            reply,
        } => {
            let track_count = {
                let proj = project.borrow();
                proj.tracks.len()
            };
            if from_index >= track_count || to_index >= track_count {
                reply
                    .send(json!({"error": "Index out of range", "track_count": track_count}))
                    .ok();
            } else if from_index == to_index {
                reply
                    .send(json!({"success": true, "message": "No change needed"}))
                    .ok();
            } else {
                let cmd = crate::undo::ReorderTrackCommand {
                    from_index,
                    to_index,
                };
                {
                    let st = timeline_state.borrow_mut();
                    let project_rc = st.project.clone();
                    drop(st);
                    let mut proj = project_rc.borrow_mut();
                    timeline_state
                        .borrow_mut()
                        .history
                        .execute(Box::new(cmd), &mut proj);
                }
                reply
                    .send(json!({"success": true, "from_index": from_index, "to_index": to_index}))
                    .ok();
                on_project_changed();
            }
        }
        McpCommand::SetTransition {
            track_index,
            clip_index,
            kind,
            duration_ns,
            alignment,
            reply,
        } => {
            let candidate = {
                let proj = project.borrow();
                let Some(track) = proj.tracks.get(track_index) else {
                    reply.send(json!({"error":"Track index out of range","track_count":proj.tracks.len()})).ok();
                    return;
                };
                let Some(transition_alignment) = TransitionAlignment::from_str(&alignment) else {
                    reply
                        .send(json!({
                            "error":"Unsupported transition alignment",
                            "supported_alignments":["end_on_cut", "center_on_cut", "start_on_cut"]
                        }))
                        .ok();
                    return;
                };
                let Some(clip) = track.clips.get(clip_index) else {
                    reply.send(json!({"error":"clip_index must reference a clip with a following clip","clip_count":track.clips.len()})).ok();
                    return;
                };
                let validated = match validate_track_transition_request(
                    track,
                    clip_index,
                    &kind,
                    duration_ns,
                    transition_alignment,
                ) {
                    Ok(validated) => validated,
                    Err(err) => {
                        reply
                            .send(json!({
                                "error": err.to_string(),
                                "supported_kinds": supported_transition_kinds(),
                                "supported_alignments":["end_on_cut", "center_on_cut", "start_on_cut"]
                            }))
                            .ok();
                        return;
                    }
                };
                Some((
                    track.id.clone(),
                    clip.id.clone(),
                    clip.outgoing_transition.clone(),
                    validated,
                ))
            };
            let Some((track_id, clip_id, old_transition, validated)) = candidate else {
                return;
            };
            let cmd = crate::undo::SetClipTransitionCommand {
                clip_id,
                track_id,
                old_transition,
                new_transition: validated.transition.clone(),
            };
            {
                let st = timeline_state.borrow_mut();
                let project_rc = st.project.clone();
                drop(st);
                let mut proj = project_rc.borrow_mut();
                timeline_state
                    .borrow_mut()
                    .history
                    .execute(Box::new(cmd), &mut proj);
            }
            reply
                .send(json!({
                    "success": true,
                    "track_index": track_index,
                    "clip_index": clip_index,
                    "kind": validated.transition.kind,
                    "duration_ns": validated.transition.duration_ns,
                    "alignment": validated.transition.alignment.as_str(),
                    "max_duration_ns": validated.max_duration_ns
                }))
                .ok();
            on_project_changed_full();
        }

        McpCommand::CreateProject { title, reply } => {
            *project.borrow_mut() = crate::model::project::Project::new(title.clone());
            {
                let mut st = timeline_state.borrow_mut();
                st.playhead_ns = 0;
                st.scroll_offset = 0.0;
                st.pixels_per_second = 100.0;
                st.selected_clip_id = None;
                st.selected_track_id = None;
                st.history = crate::undo::EditHistory::new();
            }
            main_stack.set_visible_child_name("editor");
            reply.send(json!({"success": true, "title": title})).ok();
            suppress_resume_on_next_reload.set(true);
            clear_media_browser_on_next_reload.set(true);
            on_project_changed_full();
        }

        McpCommand::InsertClip {
            source_path,
            source_in_ns,
            source_out_ns,
            track_index,
            timeline_pos_ns,
            reply,
        } => {
            let clip_duration = source_out_ns.saturating_sub(source_in_ns);
            if clip_duration == 0 {
                reply
                    .send(json!({"error": "source_in_ns must be less than source_out_ns"}))
                    .ok();
                return;
            }
            let playhead = timeline_pos_ns.unwrap_or_else(|| timeline_state.borrow().playhead_ns);
            let magnetic_mode = timeline_state.borrow().magnetic_mode;
            let source_monitor_auto_link_av =
                preferences_state.borrow().source_monitor_auto_link_av;
            let source_info = {
                let lib = library.borrow();
                let proj = project.borrow();
                crate::ui::window::lookup_source_placement_info(&lib.items, &proj, &source_path)
            };
            let result = {
                let mut proj = project.borrow_mut();
                let placement_plan = crate::ui::window::build_source_placement_plan_by_track_index(
                    &proj,
                    track_index,
                    source_info,
                    source_monitor_auto_link_av,
                );
                let mut track_changes: Vec<TrackClipsChange> = Vec::new();
                let mut created_clip_ids: Vec<String> = Vec::new();
                let magnetic_mode_for_placement =
                    magnetic_mode && !placement_plan.uses_linked_pair();
                for (target_track_idx, clip) in crate::ui::window::build_source_clips_for_plan(
                    &placement_plan,
                    &source_path,
                    source_in_ns,
                    source_out_ns,
                    playhead,
                    source_info.source_timecode_base_ns,
                    source_info.audio_channel_mode,
                    None,
                    source_info.is_animated_svg,
                ) {
                    created_clip_ids.push(clip.id.clone());
                    track_changes.push(crate::ui::window::insert_clip_at_playhead_on_track(
                        &mut proj.tracks[target_track_idx],
                        clip,
                        playhead,
                        magnetic_mode_for_placement,
                    ));
                }
                if track_changes.is_empty() {
                    Err("No matching track found")
                } else {
                    proj.dirty = true;
                    Ok((
                        track_changes,
                        created_clip_ids.first().cloned().unwrap_or_default(),
                        created_clip_ids.into_iter().skip(1).collect::<Vec<_>>(),
                        placement_plan.link_group_id.clone(),
                    ))
                }
            };
            match result {
                Ok((mut track_changes, clip_id, linked_clip_ids, link_group_id)) => {
                    let cmd: Box<dyn crate::undo::EditCommand> = if track_changes.len() == 1 {
                        let change = track_changes.pop().unwrap();
                        Box::new(crate::undo::SetTrackClipsCommand {
                            track_id: change.track_id,
                            old_clips: change.old_clips,
                            new_clips: change.new_clips,
                            label: "Insert at playhead (MCP)".to_string(),
                        })
                    } else {
                        Box::new(crate::undo::SetMultipleTracksClipsCommand {
                            changes: track_changes,
                            label: "Insert at playhead (MCP)".to_string(),
                        })
                    };
                    let st = timeline_state.borrow_mut();
                    let project_rc = st.project.clone();
                    drop(st);
                    let mut proj = project_rc.borrow_mut();
                    timeline_state.borrow_mut().history.undo_stack.push(cmd);
                    timeline_state.borrow_mut().history.redo_stack.clear();
                    proj.dirty = true;
                    drop(proj);
                    reply
                        .send(json!({
                            "success": true,
                            "clip_id": clip_id,
                            "linked_clip_ids": linked_clip_ids,
                            "link_group_id": link_group_id
                        }))
                        .ok();
                    on_project_changed_full();
                }
                Err(e) => {
                    reply.send(json!({"error": e})).ok();
                }
            }
        }

        McpCommand::OverwriteClip {
            source_path,
            source_in_ns,
            source_out_ns,
            track_index,
            timeline_pos_ns,
            reply,
        } => {
            let clip_duration = source_out_ns.saturating_sub(source_in_ns);
            if clip_duration == 0 {
                reply
                    .send(json!({"error": "source_in_ns must be less than source_out_ns"}))
                    .ok();
                return;
            }
            let playhead = timeline_pos_ns.unwrap_or_else(|| timeline_state.borrow().playhead_ns);
            let magnetic_mode = timeline_state.borrow().magnetic_mode;
            let source_monitor_auto_link_av =
                preferences_state.borrow().source_monitor_auto_link_av;
            let range_start = playhead;
            let range_end = playhead + clip_duration;
            let source_info = {
                let lib = library.borrow();
                let proj = project.borrow();
                crate::ui::window::lookup_source_placement_info(&lib.items, &proj, &source_path)
            };
            let result = {
                let mut proj = project.borrow_mut();
                let placement_plan = crate::ui::window::build_source_placement_plan_by_track_index(
                    &proj,
                    track_index,
                    source_info,
                    source_monitor_auto_link_av,
                );
                let mut track_changes: Vec<TrackClipsChange> = Vec::new();
                let mut created_clip_ids: Vec<String> = Vec::new();
                let magnetic_mode_for_placement =
                    magnetic_mode && !placement_plan.uses_linked_pair();
                for (target_track_idx, clip) in crate::ui::window::build_source_clips_for_plan(
                    &placement_plan,
                    &source_path,
                    source_in_ns,
                    source_out_ns,
                    playhead,
                    source_info.source_timecode_base_ns,
                    source_info.audio_channel_mode,
                    None,
                    source_info.is_animated_svg,
                ) {
                    created_clip_ids.push(clip.id.clone());
                    track_changes.push(crate::ui::window::overwrite_clip_range_on_track(
                        &mut proj.tracks[target_track_idx],
                        clip,
                        range_start,
                        range_end,
                        magnetic_mode_for_placement,
                    ));
                }
                if track_changes.is_empty() {
                    Err("No matching track found")
                } else {
                    proj.dirty = true;
                    Ok((
                        track_changes,
                        created_clip_ids.first().cloned().unwrap_or_default(),
                        created_clip_ids.into_iter().skip(1).collect::<Vec<_>>(),
                        placement_plan.link_group_id.clone(),
                    ))
                }
            };
            match result {
                Ok((mut track_changes, clip_id, linked_clip_ids, link_group_id)) => {
                    let cmd: Box<dyn crate::undo::EditCommand> = if track_changes.len() == 1 {
                        let change = track_changes.pop().unwrap();
                        Box::new(crate::undo::SetTrackClipsCommand {
                            track_id: change.track_id,
                            old_clips: change.old_clips,
                            new_clips: change.new_clips,
                            label: "Overwrite at playhead (MCP)".to_string(),
                        })
                    } else {
                        Box::new(crate::undo::SetMultipleTracksClipsCommand {
                            changes: track_changes,
                            label: "Overwrite at playhead (MCP)".to_string(),
                        })
                    };
                    let st = timeline_state.borrow_mut();
                    let project_rc = st.project.clone();
                    drop(st);
                    let mut proj = project_rc.borrow_mut();
                    timeline_state.borrow_mut().history.undo_stack.push(cmd);
                    timeline_state.borrow_mut().history.redo_stack.clear();
                    proj.dirty = true;
                    drop(proj);
                    reply
                        .send(json!({
                            "success": true,
                            "clip_id": clip_id,
                            "linked_clip_ids": linked_clip_ids,
                            "link_group_id": link_group_id
                        }))
                        .ok();
                    on_project_changed_full();
                }
                Err(e) => {
                    reply.send(json!({"error": e})).ok();
                }
            }
        }

        McpCommand::SeekPlayhead {
            timeline_pos_ns,
            reply,
        } => {
            timeline_state.borrow_mut().playhead_ns = timeline_pos_ns;
            let needs_async = prog_player.borrow_mut().seek(timeline_pos_ns);
            if needs_async {
                // 3+ tracks: the pipeline is in Playing.  Let the GTK main
                // loop run so gtk4paintablesink can complete its preroll, then
                // restore Paused and reply.
                let pp = prog_player.clone();
                glib::timeout_add_local_once(std::time::Duration::from_millis(500), move || {
                    pp.borrow().complete_playing_pulse();
                    reply
                        .send(json!({"ok": true, "timeline_pos_ns": timeline_pos_ns}))
                        .ok();
                });
            } else {
                reply
                    .send(json!({"ok": true, "timeline_pos_ns": timeline_pos_ns}))
                    .ok();
            }
        }

        McpCommand::ExportDisplayedFrame { path, reply } => {
            if path.is_empty() {
                reply
                    .send(json!({"ok": false, "error": "path is required"}))
                    .ok();
            } else {
                match prog_player.borrow_mut().export_displayed_frame_ppm(&path) {
                    Ok(()) => reply
                        .send(json!({"ok": true, "path": path, "format": "ppm"}))
                        .ok(),
                    Err(e) => reply
                        .send(json!({"ok": false, "error": e.to_string()}))
                        .ok(),
                };
            }
        }

        McpCommand::ExportTimelineSnapshot {
            path,
            width,
            height,
            reply,
        } => {
            if path.is_empty() {
                reply
                    .send(json!({"ok": false, "error": "path is required"}))
                    .ok();
            } else {
                let st = timeline_state.borrow();
                match crate::ui::timeline::widget::export_timeline_snapshot_png(
                    &st,
                    width as i32,
                    height as i32,
                    &path,
                ) {
                    Ok(()) => reply
                        .send(json!({"ok": true, "path": path, "width": width, "height": height}))
                        .ok(),
                    Err(e) => reply.send(json!({"ok": false, "error": e})).ok(),
                };
            }
        }

        McpCommand::Play { reply } => {
            if let Some(cb) = timeline_state.borrow().on_extraction_pause.clone() {
                cb(true);
            }
            prog_player.borrow_mut().play();
            reply.send(json!({"ok": true})).ok();
        }

        McpCommand::Pause { reply } => {
            if let Some(cb) = timeline_state.borrow().on_extraction_pause.clone() {
                cb(false);
            }
            prog_player.borrow_mut().pause();
            reply.send(json!({"ok": true})).ok();
        }

        McpCommand::Stop { reply } => {
            if let Some(cb) = timeline_state.borrow().on_extraction_pause.clone() {
                cb(false);
            }
            prog_player.borrow_mut().stop();
            reply.send(json!({"ok": true})).ok();
        }

        McpCommand::TakeScreenshot { reply } => {
            // Generate a timestamped filename in the current working directory.
            let timestamp = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs();
            let filename = format!("ultimateslice-screenshot-{timestamp}.png");
            let path = std::env::current_dir()
                .unwrap_or_else(|_| std::path::PathBuf::from("."))
                .join(&filename);

            // Snapshot the window widget using GTK snapshot + GSK CairoRenderer.
            let w = window.width().max(1);
            let h = window.height().max(1);
            let paintable = gtk::WidgetPaintable::new(Some(window));
            let snapshot = gtk::Snapshot::new();
            paintable.snapshot(&snapshot, w as f64, h as f64);

            match snapshot.to_node() {
                Some(node) => {
                    let renderer = gtk::gsk::CairoRenderer::new();
                    match renderer.realize(None::<&gdk4::Surface>) {
                        Ok(()) => {
                            let bounds = gtk::graphene::Rect::new(0.0, 0.0, w as f32, h as f32);
                            let texture = renderer.render_texture(&node, Some(&bounds));
                            renderer.unrealize();
                            match texture.save_to_png(&path) {
                                Ok(()) => reply
                                    .send(json!({"ok": true, "path": path.to_string_lossy()}))
                                    .ok(),
                                Err(e) => reply
                                    .send(json!({"ok": false, "error": e.to_string()}))
                                    .ok(),
                            };
                        }
                        Err(e) => {
                            reply
                                .send(json!({"ok": false, "error": format!("Renderer realize failed: {e}")}))
                                .ok();
                        }
                    }
                }
                None => {
                    reply
                        .send(json!({"ok": false, "error": "Window produced no render node"}))
                        .ok();
                }
            }
        }

        McpCommand::SelectLibraryItem { path, reply } => {
            let item = library
                .borrow()
                .items
                .iter()
                .find(|i| i.source_path == path)
                .cloned();
            match item {
                Some(media_item) => {
                    if media_item.has_backing_file() {
                        on_source_selected(media_item.source_path.clone(), media_item.duration_ns);
                        reply
                            .send(json!({
                                "ok": true,
                                "label": media_item.label,
                                "duration_ns": media_item.duration_ns,
                            }))
                            .ok();
                    } else {
                        reply
                            .send(json!({
                                "ok": false,
                                "error": "Library item has no source media to preview",
                            }))
                            .ok();
                    }
                }
                None => {
                    reply
                        .send(json!({
                            "ok": false,
                            "error": format!("No library item with path: {path}"),
                        }))
                        .ok();
                }
            }
        }

        McpCommand::SourcePlay { reply } => {
            let _ = player.borrow().play();
            reply.send(json!({"ok": true})).ok();
        }

        McpCommand::SourcePause { reply } => {
            let _ = player.borrow().pause();
            reply.send(json!({"ok": true})).ok();
        }

        McpCommand::MatchFrame { clip_id, reply } => {
            let effective_id = clip_id.or_else(|| timeline_state.borrow().selected_clip_id.clone());
            let playhead_ns = timeline_state.borrow().playhead_ns;
            match effective_id {
                None => {
                    reply
                        .send(json!({"ok": false, "error": "No clip selected and no clip_id provided"}))
                        .ok();
                }
                Some(cid) => {
                    let (clip_exists, clip_info) = {
                        let proj = project.borrow();
                        (proj.clip_ref(&cid).is_some(), proj.clip_instance(&cid))
                    };
                    match clip_info {
                        None if clip_exists => {
                            reply
                                .send(json!({"ok": false, "error": "Clip has no source media (title/adjustment)"}))
                                .ok();
                        }
                        None => {
                            reply
                                .send(
                                    json!({"ok": false, "error": format!("Clip not found: {cid}")}),
                                )
                                .ok();
                        }
                        Some(instance) => {
                            let duration_ns = library
                                .borrow()
                                .items
                                .iter()
                                .find(|item| item.source_path == instance.source_path)
                                .map(|item| item.duration_ns)
                                .unwrap_or(instance.source_out);
                            on_source_selected(instance.source_path.clone(), duration_ns);
                            let source_pos = (instance.source_in
                                + playhead_ns.saturating_sub(instance.root_timeline_start))
                            .min(instance.source_out)
                            .max(instance.source_in);
                            let _ = player.borrow().seek(source_pos);
                            reply
                                .send(json!({
                                    "ok": true,
                                    "source_path": instance.source_path,
                                    "source_pos_ns": source_pos,
                                    "source_in_ns": instance.source_in,
                                    "source_out_ns": instance.source_out,
                                    "duration_ns": duration_ns,
                                    "clip_id": instance.clip_id,
                                    "track_id": instance.track_id,
                                    "compound_path_ids": instance.compound_path_ids,
                                    "compound_path_labels": instance.compound_path_labels,
                                }))
                                .ok();
                        }
                    }
                }
            }
        }

        McpCommand::ReverseMatchFrame { path, reply } => {
            let item = library
                .borrow()
                .items
                .iter()
                .find(|item| item.source_path == path)
                .cloned();
            match item {
                Some(media_item) if media_item.has_backing_file() => {
                    let (frame_rate, matches) = {
                        let proj = project.borrow();
                        (proj.frame_rate.clone(), proj.source_clip_instances(&path))
                    };
                    let matches_json: Vec<serde_json::Value> = matches
                        .into_iter()
                        .map(|instance| {
                            json!({
                                "clip_id": instance.clip_id,
                                "clip_label": instance.clip_label,
                                "track_id": instance.track_id,
                                "track_label": instance.track_label,
                                "timeline_start_ns": instance.timeline_start,
                                "timeline_end_ns": instance.timeline_end,
                                "root_timeline_start_ns": instance.root_timeline_start,
                                "root_timeline_end_ns": instance.root_timeline_end,
                                "root_timeline_timecode": crate::ui::timecode::format_ns_as_timecode(instance.root_timeline_start, &frame_rate),
                                "timeline_timecode": crate::ui::timecode::format_ns_as_timecode(instance.timeline_start, &frame_rate),
                                "source_in_ns": instance.source_in,
                                "source_out_ns": instance.source_out,
                                "compound_path_ids": instance.compound_path_ids,
                                "compound_path_labels": instance.compound_path_labels,
                            })
                        })
                        .collect();
                    reply
                        .send(json!({
                            "ok": true,
                            "path": path,
                            "label": media_item.label,
                            "count": matches_json.len(),
                            "matches": matches_json,
                        }))
                        .ok();
                }
                Some(_) => {
                    reply
                        .send(json!({
                            "ok": false,
                            "error": "Library item has no source media to match",
                        }))
                        .ok();
                }
                None => {
                    reply
                        .send(json!({
                            "ok": false,
                            "error": format!("No library item with path: {path}"),
                        }))
                        .ok();
                }
            }
        }

        McpCommand::ListBackups { reply } => {
            let backups = crate::project_versions::list_backup_files();
            let list: Vec<serde_json::Value> = backups
                .iter()
                .map(|entry| {
                    json!({
                        "path": entry.path.to_string_lossy(),
                        "name": entry.name,
                        "size_bytes": entry.size_bytes,
                    })
                })
                .collect();
            reply
                .send(json!({ "ok": true, "backups": list, "count": list.len() }))
                .ok();
        }

        McpCommand::ListProjectSnapshots { reply } => {
            let snapshots = {
                let proj = project.borrow();
                crate::project_versions::list_project_snapshots_for_project(&proj)
            };
            let list: Vec<serde_json::Value> = snapshots
                .iter()
                .map(|entry| {
                    json!({
                        "id": entry.metadata.id,
                        "name": entry.metadata.snapshot_name,
                        "project_title": entry.metadata.project_title,
                        "project_file_path": entry.metadata.project_file_path,
                        "created_at_unix_secs": entry.metadata.created_at_unix_secs,
                        "created_at": crate::project_versions::format_snapshot_timestamp(entry.metadata.created_at_unix_secs),
                        "path": entry.snapshot_path.to_string_lossy(),
                        "size_bytes": entry.size_bytes,
                    })
                })
                .collect();
            reply
                .send(json!({ "ok": true, "snapshots": list, "count": list.len() }))
                .ok();
        }

        McpCommand::CreateProjectSnapshot { name, reply } => {
            crate::model::media_library::sync_bins_to_project(
                &library.borrow(),
                &mut project.borrow_mut(),
            );
            let result = {
                let proj = project.borrow();
                crate::project_versions::write_snapshot_project_xml(&proj).and_then(|xml| {
                    crate::project_versions::create_project_snapshot(&proj, &xml, &name)
                })
            };
            match result {
                Ok(entry) => {
                    reply
                        .send(json!({
                            "ok": true,
                            "snapshot": {
                                "id": entry.metadata.id,
                                "name": entry.metadata.snapshot_name,
                                "project_title": entry.metadata.project_title,
                                "project_file_path": entry.metadata.project_file_path,
                                "created_at_unix_secs": entry.metadata.created_at_unix_secs,
                                "created_at": crate::project_versions::format_snapshot_timestamp(entry.metadata.created_at_unix_secs),
                                "path": entry.snapshot_path.to_string_lossy(),
                                "size_bytes": entry.size_bytes,
                            }
                        }))
                        .ok();
                }
                Err(e) => {
                    reply.send(json!({"ok": false, "error": e})).ok();
                }
            }
        }

        McpCommand::RestoreProjectSnapshot { snapshot_id, reply } => {
            let preserved_file_path = project.borrow().file_path.clone();
            let snapshot_id_for_worker = snapshot_id.clone();
            let (tx, rx) = std::sync::mpsc::sync_channel::<
                Result<(crate::project_versions::ProjectSnapshotEntry, Project), String>,
            >(1);
            std::thread::spawn(move || {
                let result =
                    crate::project_versions::load_project_snapshot(&snapshot_id_for_worker);
                let _ = tx.send(result);
            });
            timeline_state.borrow_mut().loading = true;
            let project = project.clone();
            let timeline_state = timeline_state.clone();
            let main_stack = main_stack.clone();
            let on_project_changed = on_project_changed_full.clone();
            let suppress_resume_on_next_reload = suppress_resume_on_next_reload.clone();
            let clear_media_browser_on_next_reload = clear_media_browser_on_next_reload.clone();
            glib::timeout_add_local(std::time::Duration::from_millis(10), move || {
                match rx.try_recv() {
                    Ok(Ok((entry, mut new_proj))) => {
                        let snapshot_name = entry.metadata.snapshot_name.clone();
                        new_proj.file_path = preserved_file_path.clone();
                        new_proj.dirty = true;
                        *project.borrow_mut() = new_proj;
                        timeline_state.borrow_mut().loading = false;
                        main_stack.set_visible_child_name("editor");
                        suppress_resume_on_next_reload.set(true);
                        clear_media_browser_on_next_reload.set(true);
                        let on_project_changed = on_project_changed.clone();
                        glib::timeout_add_local_once(
                            std::time::Duration::from_millis(0),
                            move || {
                                on_project_changed();
                            },
                        );
                        reply
                            .send(json!({
                                "ok": true,
                                "snapshot_id": entry.metadata.id,
                                "snapshot_name": snapshot_name,
                                "project_file_path": entry.metadata.project_file_path,
                                "dirty": true,
                            }))
                            .ok();
                        glib::ControlFlow::Break
                    }
                    Ok(Err(e)) => {
                        timeline_state.borrow_mut().loading = false;
                        reply.send(json!({"ok": false, "error": e})).ok();
                        glib::ControlFlow::Break
                    }
                    Err(std::sync::mpsc::TryRecvError::Empty) => glib::ControlFlow::Continue,
                    Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                        timeline_state.borrow_mut().loading = false;
                        reply.send(json!({"ok": false, "error": "restore_project_snapshot worker disconnected"})).ok();
                        glib::ControlFlow::Break
                    }
                }
            });
        }

        McpCommand::DeleteProjectSnapshot { snapshot_id, reply } => {
            match crate::project_versions::delete_project_snapshot(&snapshot_id) {
                Ok(()) => {
                    reply
                        .send(json!({"ok": true, "snapshot_id": snapshot_id}))
                        .ok();
                }
                Err(e) => {
                    reply.send(json!({"ok": false, "error": e})).ok();
                }
            }
        }

        McpCommand::SetClipStabilization {
            clip_id,
            enabled,
            smoothing,
            reply,
        } => {
            let mut found = false;
            {
                let mut proj = project.borrow_mut();
                if let Some(clip) = proj.clip_mut(&clip_id) {
                    clip.vidstab_enabled = enabled;
                    clip.vidstab_smoothing = (smoothing as f32).clamp(0.0, 1.0);
                    proj.dirty = true;
                    found = true;
                }
            }
            if found {
                on_project_changed();
                reply
                    .send(json!({
                        "ok": true,
                        "clip_id": clip_id,
                        "vidstab_enabled": enabled,
                        "vidstab_smoothing": smoothing,
                    }))
                    .ok();
            } else {
                reply
                    .send(json!({"ok": false, "error": format!("Clip not found: {clip_id}")}))
                    .ok();
            }
        }

        McpCommand::SetClipAutoCropTrack {
            clip_id,
            center_x,
            center_y,
            width,
            height,
            padding,
            reply,
        } => {
            // Ensure a motion tracker with the requested region exists on
            // the clip. If no tracker matches the region, create one so
            // the binding has somewhere to resolve its samples from.
            let tracker_id = {
                let mut proj = project.borrow_mut();
                let Some(clip) = proj.clip_mut(&clip_id) else {
                    reply
                        .send(json!({
                            "ok": false,
                            "error": format!("Clip not found: {clip_id}")
                        }))
                        .ok();
                    return;
                };
                if let Err(message) = crate::ui::window::clip_supports_tracking_analysis(clip) {
                    reply.send(json!({"ok": false, "error": message})).ok();
                    return;
                }
                let new_region = crate::model::clip::TrackingRegion {
                    center_x,
                    center_y,
                    width,
                    height,
                    rotation_deg: 0.0,
                };
                // Prefer the first existing tracker; update its region to
                // match so the caller's region is authoritative.
                let tracker_id = if let Some(tracker) = clip.motion_trackers.first_mut() {
                    tracker.analysis_region = new_region;
                    tracker.samples.clear();
                    tracker.id.clone()
                } else {
                    let mut tracker =
                        crate::model::clip::MotionTracker::new("Auto-crop tracker".to_string());
                    tracker.analysis_region = new_region;
                    let id = tracker.id.clone();
                    clip.motion_trackers.push(tracker);
                    id
                };
                proj.dirty = true;
                tracker_id
            };

            let (outcome, command) = crate::ui::window::run_auto_crop_track_for_clip(
                project,
                tracking_cache,
                tracking_job_owner_by_key,
                tracking_job_key_by_clip,
                &clip_id,
                &tracker_id,
                padding.unwrap_or(AUTO_CROP_DEFAULT_PADDING),
            );
            if let Some(cmd) = command {
                let mut st = timeline_state.borrow_mut();
                let mut proj = project.borrow_mut();
                st.history.execute(cmd, &mut proj);
            }
            on_project_changed();
            match outcome {
                AutoCropOutcome::Ok { message } => {
                    let proj = project.borrow();
                    let binding = proj
                        .clip_ref(&clip_id)
                        .and_then(|c| c.tracking_binding.clone());
                    reply
                        .send(json!({
                            "ok": true,
                            "clip_id": clip_id,
                            "tracker_id": tracker_id,
                            "status": "ready",
                            "message": message,
                            "scale_multiplier": binding.as_ref().map(|b| b.scale_multiplier),
                            "offset_x": binding.as_ref().map(|b| b.offset_x),
                            "offset_y": binding.as_ref().map(|b| b.offset_y),
                        }))
                        .ok();
                }
                AutoCropOutcome::Queued { message } => {
                    let proj = project.borrow();
                    let binding = proj
                        .clip_ref(&clip_id)
                        .and_then(|c| c.tracking_binding.clone());
                    reply
                        .send(json!({
                            "ok": true,
                            "clip_id": clip_id,
                            "tracker_id": tracker_id,
                            "status": "queued",
                            "message": message,
                            "scale_multiplier": binding.as_ref().map(|b| b.scale_multiplier),
                            "offset_x": binding.as_ref().map(|b| b.offset_x),
                            "offset_y": binding.as_ref().map(|b| b.offset_y),
                        }))
                        .ok();
                }
                AutoCropOutcome::Err { message } => {
                    reply.send(json!({"ok": false, "error": message})).ok();
                }
            }
        }

        McpCommand::ListFrei0rPlugins { reply } => {
            let registry = crate::media::frei0r_registry::Frei0rRegistry::get_or_discover();
            let plugins: Vec<Value> = registry
                .plugins
                .iter()
                .map(|p| {
                    let params: Vec<Value> = p
                        .params
                        .iter()
                        .map(|pr| {
                            let mut obj = json!({
                                "name": pr.name,
                                "display_name": pr.display_name,
                                "type": format!("{:?}", pr.param_type),
                                "default": pr.default_value,
                                "min": pr.min,
                                "max": pr.max,
                            });
                            if let Some(ref ev) = pr.enum_values {
                                obj["enum_values"] = json!(ev);
                            }
                            if let Some(ref ds) = pr.default_string {
                                obj["default_string"] = json!(ds);
                            }
                            obj
                        })
                        .collect();
                    json!({
                        "name": p.frei0r_name,
                        "display_name": p.display_name,
                        "gst_element_name": p.gst_element_name,
                        "description": p.description,
                        "category": p.category,
                        "params": params,
                    })
                })
                .collect();
            reply
                .send(json!({"plugins": plugins, "count": plugins.len()}))
                .ok();
        }

        McpCommand::ListClipFrei0rEffects { clip_id, reply } => {
            let proj = project.borrow();
            let mut found = false;
            let mut effects_json = Vec::new();
            if let Some(clip) = proj.clip_ref(&clip_id) {
                found = true;
                for e in &clip.frei0r_effects {
                    effects_json.push(json!({
                        "id": e.id,
                        "plugin_name": e.plugin_name,
                        "enabled": e.enabled,
                        "params": e.params,
                        "string_params": e.string_params,
                    }));
                }
            }
            if found {
                reply.send(json!({"effects": effects_json})).ok();
            } else {
                reply.send(json!({"error": "Clip not found"})).ok();
            }
        }

        McpCommand::AddClipFrei0rEffect {
            clip_id,
            plugin_name,
            params,
            string_params,
            reply,
        } => {
            let effect_id = uuid::Uuid::new_v4().to_string();
            let mut default_params = std::collections::HashMap::new();
            let mut default_string_params = std::collections::HashMap::new();
            // Populate defaults from registry.
            let registry = crate::media::frei0r_registry::Frei0rRegistry::get_or_discover();
            if let Some(info) = registry.find_by_name(&plugin_name) {
                for p in &info.params {
                    if p.param_type == crate::media::frei0r_registry::Frei0rParamType::String {
                        if let Some(ref s) = p.default_string {
                            default_string_params.insert(p.name.clone(), s.clone());
                        }
                    } else {
                        default_params.insert(p.name.clone(), p.default_value);
                    }
                }
            }
            // Override with user-supplied params.
            if let Some(user_params) = params {
                for (k, v) in user_params {
                    default_params.insert(k, v);
                }
            }
            if let Some(user_string_params) = string_params {
                for (k, v) in user_string_params {
                    default_string_params.insert(k, v);
                }
            }
            let effect = crate::model::clip::Frei0rEffect {
                id: effect_id.clone(),
                plugin_name: plugin_name.clone(),
                enabled: true,
                params: default_params,
                string_params: default_string_params,
            };
            let mut proj = project.borrow_mut();
            let found = if let Some(clip) = proj.clip_mut(&clip_id) {
                clip.frei0r_effects.push(effect);
                proj.dirty = true;
                true
            } else {
                false
            };
            drop(proj);
            if found {
                on_project_changed_full();
                reply
                    .send(json!({"success": true, "effect_id": effect_id}))
                    .ok();
            } else {
                reply.send(json!({"error": "Clip not found"})).ok();
            }
        }

        McpCommand::RemoveClipFrei0rEffect {
            clip_id,
            effect_id,
            reply,
        } => {
            let mut proj = project.borrow_mut();
            let found = if let Some(clip) = proj.clip_mut(&clip_id) {
                if let Some(pos) = clip.frei0r_effects.iter().position(|e| e.id == effect_id) {
                    clip.frei0r_effects.remove(pos);
                    proj.dirty = true;
                    true
                } else {
                    false
                }
            } else {
                false
            };
            drop(proj);
            if found {
                on_project_changed_full();
            }
            reply.send(json!({"success": found})).ok();
        }

        McpCommand::SetClipFrei0rEffectParams {
            clip_id,
            effect_id,
            params,
            string_params,
            reply,
        } => {
            let mut proj = project.borrow_mut();
            let found = if let Some(clip) = proj.clip_mut(&clip_id) {
                if let Some(effect) = clip.frei0r_effects.iter_mut().find(|e| e.id == effect_id) {
                    for (k, v) in params {
                        effect.params.insert(k, v);
                    }
                    if let Some(sp) = string_params {
                        for (k, v) in sp {
                            effect.string_params.insert(k, v);
                        }
                    }
                    proj.dirty = true;
                    true
                } else {
                    false
                }
            } else {
                false
            };
            drop(proj);
            if found {
                on_project_changed_full();
            }
            reply.send(json!({"success": found})).ok();
        }

        McpCommand::ReorderClipFrei0rEffects {
            clip_id,
            effect_ids,
            reply,
        } => {
            let mut proj = project.borrow_mut();
            let found = if let Some(clip) = proj.clip_mut(&clip_id) {
                // Build new order from effect_ids.
                let mut reordered = Vec::with_capacity(effect_ids.len());
                for eid in &effect_ids {
                    if let Some(pos) = clip.frei0r_effects.iter().position(|e| &e.id == eid) {
                        reordered.push(clip.frei0r_effects[pos].clone());
                    }
                }
                // Append any effects not mentioned (safety net).
                for e in &clip.frei0r_effects {
                    if !effect_ids.contains(&e.id) {
                        reordered.push(e.clone());
                    }
                }
                clip.frei0r_effects = reordered;
                proj.dirty = true;
                true
            } else {
                false
            };
            drop(proj);
            if found {
                on_project_changed_full();
            }
            reply.send(json!({"success": found})).ok();
        }

        McpCommand::AddTitleClip {
            template_id,
            track_index,
            timeline_start_ns,
            duration_ns,
            title_text,
            reply,
        } => {
            let template = crate::ui::title_templates::find_template(&template_id);
            if template.is_none() {
                reply
                    .send(json!({"error": format!("Unknown template: {template_id}")}))
                    .ok();
            } else {
                let template = template.unwrap();
                let playhead = timeline_state.borrow().playhead_ns;
                let start = timeline_start_ns.unwrap_or(playhead);
                let mut clip = crate::ui::title_templates::create_title_clip(template, start);
                if let Some(dur) = duration_ns {
                    clip.source_out = dur;
                }
                if let Some(text) = title_text {
                    clip.title_text = text.clone();
                    clip.label = text;
                }
                let clip_id = clip.id.clone();
                let mut proj = project.borrow_mut();
                let ti = track_index
                    .unwrap_or_else(|| proj.tracks.iter().position(|t| t.is_video()).unwrap_or(0));
                if ti < proj.tracks.len() {
                    let magnetic_mode = timeline_state.borrow().magnetic_mode;
                    let change = crate::ui::window::insert_clip_at_playhead_on_track(
                        &mut proj.tracks[ti],
                        clip,
                        start,
                        magnetic_mode,
                    );
                    let cmd = crate::undo::SetTrackClipsCommand {
                        track_id: change.track_id,
                        old_clips: change.old_clips,
                        new_clips: change.new_clips,
                        label: "Add title clip (MCP)".to_string(),
                    };
                    drop(proj);
                    {
                        let mut st = timeline_state.borrow_mut();
                        let mut proj = project.borrow_mut();
                        st.history.execute(Box::new(cmd), &mut proj);
                    }
                    on_project_changed_full();
                    reply.send(json!({"clip_id": clip_id})).ok();
                } else {
                    drop(proj);
                    reply
                        .send(json!({"error": "track_index out of range"}))
                        .ok();
                }
            }
        }

        McpCommand::AddAdjustmentLayer {
            track_index,
            timeline_start_ns,
            duration_ns,
            reply,
        } => {
            let clip = crate::model::clip::Clip::new_adjustment(timeline_start_ns, duration_ns);
            let clip_id = clip.id.clone();
            let proj_ref = project.borrow();
            if track_index < proj_ref.tracks.len() {
                let track_id = proj_ref.tracks[track_index].id.clone();
                drop(proj_ref);
                let cmd = crate::undo::AddAdjustmentLayerCommand { clip, track_id };
                {
                    let mut st = timeline_state.borrow_mut();
                    let mut proj = project.borrow_mut();
                    st.history.execute(Box::new(cmd), &mut proj);
                }
                on_project_changed_full();
                reply.send(json!({"clip_id": clip_id})).ok();
            } else {
                drop(proj_ref);
                reply
                    .send(json!({"error": "track_index out of range"}))
                    .ok();
            }
        }

        McpCommand::SetClipTitleStyle {
            clip_id,
            title_text,
            title_font,
            title_color,
            title_x,
            title_y,
            title_outline_width,
            title_outline_color,
            title_shadow,
            title_shadow_color,
            title_shadow_offset_x,
            title_shadow_offset_y,
            title_bg_box,
            title_bg_box_color,
            title_bg_box_padding,
            title_clip_bg_color,
            title_secondary_text,
            reply,
        } => {
            let mut proj = project.borrow_mut();
            let found = if let Some(clip) = proj
                .tracks
                .iter_mut()
                .flat_map(|t| t.clips.iter_mut())
                .find(|c| c.id == clip_id)
            {
                if let Some(v) = title_text {
                    clip.title_text = v;
                }
                if let Some(v) = title_font {
                    clip.title_font = v;
                }
                if let Some(v) = title_color {
                    clip.title_color = v;
                }
                if let Some(v) = title_x {
                    clip.title_x = v;
                }
                if let Some(v) = title_y {
                    clip.title_y = v;
                }
                if let Some(v) = title_outline_width {
                    clip.title_outline_width = v;
                }
                if let Some(v) = title_outline_color {
                    clip.title_outline_color = v;
                }
                if let Some(v) = title_shadow {
                    clip.title_shadow = v;
                }
                if let Some(v) = title_shadow_color {
                    clip.title_shadow_color = v;
                }
                if let Some(v) = title_shadow_offset_x {
                    clip.title_shadow_offset_x = v;
                }
                if let Some(v) = title_shadow_offset_y {
                    clip.title_shadow_offset_y = v;
                }
                if let Some(v) = title_bg_box {
                    clip.title_bg_box = v;
                }
                if let Some(v) = title_bg_box_color {
                    clip.title_bg_box_color = v;
                }
                if let Some(v) = title_bg_box_padding {
                    clip.title_bg_box_padding = v;
                }
                if let Some(v) = title_clip_bg_color {
                    clip.title_clip_bg_color = v;
                }
                if let Some(v) = title_secondary_text {
                    clip.title_secondary_text = v;
                }
                proj.dirty = true;
                true
            } else {
                false
            };
            // Lightweight live update: read the final clip values while we
            // still hold borrow_mut, then drop the borrow and push to player.
            let title_vals = if found {
                proj.tracks
                    .iter()
                    .flat_map(|t| t.clips.iter())
                    .find(|c| c.id == clip_id)
                    .map(|clip| {
                        (
                            clip.title_text.clone(),
                            clip.title_font.clone(),
                            clip.title_color,
                            clip.title_x,
                            clip.title_y,
                            clip.title_outline_width,
                            clip.title_outline_color,
                            clip.title_shadow,
                            clip.title_bg_box,
                        )
                    })
            } else {
                None
            };
            drop(proj);
            if let Some((text, font, color, x, y, ow, oc, shadow, bg)) = title_vals {
                let pp = prog_player.borrow();
                pp.update_current_title(&text, &font, color, x, y);
                pp.update_current_title_style(ow, oc, shadow, bg);
                pp.flush_compositor_for_title_update();
            }
            reply.send(json!({"success": found})).ok();
        }
        McpCommand::AddToExportQueue {
            output_path,
            preset_name,
            reply,
        } => {
            if output_path.is_empty() {
                reply
                    .send(json!({"success": false, "error": "output_path is required"}))
                    .ok();
                return;
            }
            let preset = if let Some(name) = preset_name {
                let state = crate::ui_state::load_export_presets_state();
                if let Some(p) = state
                    .presets
                    .iter()
                    .find(|p| p.name.eq_ignore_ascii_case(&name))
                {
                    p.clone()
                } else {
                    reply.send(json!({"success": false, "error": format!("preset '{name}' not found")})).ok();
                    return;
                }
            } else {
                let state = crate::ui_state::load_export_presets_state();
                let last = state.last_used_preset.clone();
                state
                    .presets
                    .into_iter()
                    .find(|p| Some(&p.name) == last.as_ref())
                    .or_else(|| {
                        let defaults = crate::ui_state::load_export_presets_state().presets;
                        defaults.into_iter().next()
                    })
                    .unwrap_or_else(|| crate::ui_state::ExportPreset {
                        name: "default".to_string(),
                        video_codec: crate::ui_state::ExportVideoCodec::H264,
                        container: crate::ui_state::ExportContainer::Mp4,
                        output_width: 1920,
                        output_height: 1080,
                        crf: 23,
                        audio_codec: crate::ui_state::ExportAudioCodec::Aac,
                        audio_bitrate_kbps: 192,
                        gif_fps: None,
                        audio_channel_layout: crate::ui_state::ExportAudioChannelLayout::Stereo,
                        hdr_passthrough: false,
                    })
            };
            let job = crate::ui_state::ExportQueueJob::new(&output_path, preset);
            let job_id = job.id.clone();
            let job_label = job.label.clone();
            let mut queue = crate::ui_state::load_export_queue_state();
            queue.jobs.push(job);
            crate::ui_state::save_export_queue_state(&queue);
            reply
                .send(json!({"success": true, "id": job_id, "label": job_label}))
                .ok();
        }
        McpCommand::ListExportQueue { reply } => {
            let queue = crate::ui_state::load_export_queue_state();
            let jobs: Vec<serde_json::Value> = queue
                .jobs
                .iter()
                .map(|j| {
                    json!({
                        "id": j.id,
                        "label": j.label,
                        "output_path": j.output_path,
                        "status": format!("{:?}", j.status).to_lowercase(),
                        "error": j.error
                    })
                })
                .collect();
            let count = jobs.len();
            reply.send(json!({"jobs": jobs, "count": count})).ok();
        }
        McpCommand::ClearExportQueue {
            status_filter,
            reply,
        } => {
            let mut queue = crate::ui_state::load_export_queue_state();
            let filter = status_filter.as_deref().unwrap_or("all");
            let before = queue.jobs.len();
            queue.jobs.retain(|j| match filter {
                "done" => j.status != crate::ui_state::ExportQueueJobStatus::Done,
                "error" => j.status != crate::ui_state::ExportQueueJobStatus::Error,
                _ => false, // "all" removes everything, so retain nothing
            });
            crate::ui_state::save_export_queue_state(&queue);
            let removed = before - queue.jobs.len();
            reply
                .send(json!({"success": true, "removed": removed}))
                .ok();
        }
        McpCommand::RunExportQueue { reply } => {
            let queue = crate::ui_state::load_export_queue_state();
            let pending: Vec<crate::ui_state::ExportQueueJob> = queue
                .jobs
                .iter()
                .filter(|j| j.status == crate::ui_state::ExportQueueJobStatus::Pending)
                .cloned()
                .collect();
            if pending.is_empty() {
                reply
                    .send(json!({"success": true, "message": "No pending jobs.", "results": []}))
                    .ok();
                return;
            }
            let proj_snapshot = project.borrow().clone();
            let bg_paths = bg_removal_cache.borrow().paths.clone();
            let interp_paths = frame_interp_cache
                .borrow()
                .snapshot_paths_by_clip_id(&proj_snapshot);
            std::thread::spawn(move || {
                let mut results = vec![];
                for job in &pending {
                    {
                        let mut q = crate::ui_state::load_export_queue_state();
                        if let Some(j) = q.jobs.iter_mut().find(|j| j.id == job.id) {
                            j.status = crate::ui_state::ExportQueueJobStatus::Running;
                        }
                        crate::ui_state::save_export_queue_state(&q);
                    }
                    let opts = job.options.to_export_options();
                    let (ptx, _prx) =
                        std::sync::mpsc::channel::<crate::media::export::ExportProgress>();
                    let export_result = crate::media::export::export_project(
                        &proj_snapshot,
                        &job.output_path,
                        opts,
                        None,
                        &bg_paths,
                        &interp_paths,
                        ptx,
                    );
                    let (new_status, err_msg) = match export_result {
                        Ok(()) => (crate::ui_state::ExportQueueJobStatus::Done, None),
                        Err(e) => (
                            crate::ui_state::ExportQueueJobStatus::Error,
                            Some(e.to_string()),
                        ),
                    };
                    {
                        let mut q = crate::ui_state::load_export_queue_state();
                        if let Some(j) = q.jobs.iter_mut().find(|j| j.id == job.id) {
                            j.status = new_status.clone();
                            j.error = err_msg.clone();
                        }
                        crate::ui_state::save_export_queue_state(&q);
                    }
                    results.push(json!({
                        "id": job.id,
                        "label": job.label,
                        "output_path": job.output_path,
                        "status": format!("{:?}", new_status).to_lowercase(),
                        "error": err_msg
                    }));
                }
                reply
                    .send(json!({"success": true, "results": results}))
                    .ok();
            });
        }
        McpCommand::CreateCompoundClip { clip_ids, reply } => {
            if clip_ids.len() < 2 {
                reply
                    .send(json!({"error": "At least 2 clip IDs required"}))
                    .ok();
                return;
            }
            // Select the specified clips in the timeline state, then create compound
            {
                let mut st = timeline_state.borrow_mut();
                st.set_selected_clip_ids(clip_ids.iter().cloned().collect());
                let changed = st.create_compound_from_selection();
                let proj_cb = st.on_project_changed.clone();
                drop(st);
                if changed {
                    if let Some(cb) = proj_cb {
                        cb();
                    }
                    // Find the compound clip ID (most recently added compound clip)
                    let proj = project.borrow();
                    let compound_id = proj
                        .tracks
                        .iter()
                        .flat_map(|t| t.clips.iter())
                        .find(|c| c.is_compound())
                        .map(|c| c.id.clone())
                        .unwrap_or_default();
                    reply
                        .send(json!({"success": true, "compound_clip_id": compound_id}))
                        .ok();
                } else {
                    reply
                        .send(json!({"error": "Failed to create compound clip"}))
                        .ok();
                }
            }
        }
        McpCommand::BreakApartCompoundClip { clip_id, reply } => {
            let mut st = timeline_state.borrow_mut();
            st.set_selected_clip_ids([clip_id.clone()].into_iter().collect());
            let changed = st.break_apart_compound();
            let proj_cb = st.on_project_changed.clone();
            drop(st);
            if changed {
                if let Some(cb) = proj_cb {
                    cb();
                }
                reply.send(json!({"success": true})).ok();
            } else {
                reply
                    .send(json!({"error": "Failed to break apart compound clip (not a compound clip or not found)"}))
                    .ok();
            }
        }
        McpCommand::CreateMulticamClip { clip_ids, reply } => {
            if clip_ids.len() < 2 {
                reply
                    .send(json!({"error": "At least 2 clip IDs required"}))
                    .ok();
                return;
            }
            // Collect clip info for audio sync
            let clip_infos: Vec<(String, String, u64, u64, u64, String)> = {
                let proj = project.borrow();
                clip_ids
                    .iter()
                    .filter_map(|id| {
                        proj.tracks
                            .iter()
                            .flat_map(|t| t.clips.iter().map(move |c| (t.id.clone(), c)))
                            .find(|(_, c)| &c.id == id)
                            .map(|(track_id, c)| {
                                (
                                    c.id.clone(),
                                    c.source_path.clone(),
                                    c.source_in,
                                    c.source_out,
                                    c.timeline_start,
                                    track_id,
                                )
                            })
                    })
                    .collect()
            };
            if clip_infos.len() < 2 {
                reply
                    .send(json!({"error": "Could not find 2+ clips with the provided IDs"}))
                    .ok();
                return;
            }
            // Run audio sync synchronously (MCP is blocking)
            let sync_clips: Vec<(String, String, u64, u64)> = clip_infos
                .iter()
                .map(|(id, path, src_in, src_out, _, _)| {
                    (id.clone(), path.clone(), *src_in, *src_out)
                })
                .collect();
            let sync_results = crate::media::audio_sync::sync_clips_by_audio(&sync_clips);
            let anchor_id = clip_infos[0].0.clone();
            let anchor_start = clip_infos[0].4;
            // Lookup helper: offset_ns for each clip (anchor = 0).
            let offset_for = |id: &str| -> i64 {
                if id == anchor_id {
                    0
                } else {
                    sync_results
                        .iter()
                        .find(|r| r.clip_id == id)
                        .map(|r| r.offset_ns)
                        .unwrap_or(0)
                }
            };
            // Compute each angle's effective landmark position in its source.
            // See the matching logic in the GUI multicam result handler above for
            // the full derivation. Short version: gcc_phat returns
            // `T_anchor − T_clip`, so the clip's landmark is at `−offset_ns`
            // relative to the anchor, and we SUBTRACT offset_ns here (not add).
            let signed_event_for = |id: &str, src_in: u64| -> i64 {
                let landmark = if id == anchor_id { 0 } else { -offset_for(id) };
                src_in as i64 + landmark
            };
            let desired: Vec<i64> = clip_infos
                .iter()
                .map(|(id, _, src_in, _, _, _)| signed_event_for(id, *src_in))
                .collect();
            let min_desired = desired.iter().copied().min().unwrap_or(0);
            // Build multicam angles from sync results
            let mut angles: Vec<crate::model::clip::MulticamAngle> = Vec::new();
            for (i, (id, path, src_in, src_out, _tl_start, _track_id)) in
                clip_infos.iter().enumerate()
            {
                let offset_ns = offset_for(id);
                let label = format!("Angle {}", i + 1);
                // Final synced source_in: effective landmark − min, always ≥ 0.
                let synced_in = (signed_event_for(id, *src_in) - min_desired) as u64;
                let synced_out = *src_out;
                angles.push(crate::model::clip::MulticamAngle {
                    id: uuid::Uuid::new_v4().to_string(),
                    label,
                    source_path: path.clone(),
                    source_in: synced_in,
                    source_out: synced_out,
                    sync_offset_ns: offset_ns,
                    source_timecode_base_ns: None,
                    media_duration_ns: None,
                    volume: if i == 0 { 1.0 } else { 0.0 },
                    muted: i != 0,
                    ..Default::default()
                });
            }
            if angles.len() >= 2 {
                let multicam = crate::model::clip::Clip::new_multicam(anchor_start, angles);
                let multicam_id = multicam.id.clone();
                let selected_ids: std::collections::HashSet<String> =
                    clip_infos.iter().map(|(id, ..)| id.clone()).collect();
                let mut proj = project.borrow_mut();
                let mut changes = Vec::new();
                let mut placement_track_id: Option<String> = None;
                for track in &proj.tracks {
                    if track.clips.iter().any(|c| selected_ids.contains(&c.id)) {
                        let old_clips = track.clips.clone();
                        let mut new_clips: Vec<crate::model::clip::Clip> = track
                            .clips
                            .iter()
                            .filter(|c| !selected_ids.contains(&c.id))
                            .cloned()
                            .collect();
                        if placement_track_id.is_none() {
                            new_clips.push(multicam.clone());
                            new_clips.sort_by_key(|c| c.timeline_start);
                            placement_track_id = Some(track.id.clone());
                        }
                        changes.push(crate::undo::TrackClipsChange {
                            track_id: track.id.clone(),
                            old_clips,
                            new_clips,
                        });
                    }
                }
                let cmd = Box::new(crate::undo::SetMultipleTracksClipsCommand {
                    changes,
                    label: "Create Multicam Clip".to_string(),
                });
                {
                    let mut st = timeline_state.borrow_mut();
                    st.history.execute(cmd, &mut proj);
                }
                drop(proj);
                on_project_changed();
                reply
                    .send(json!({"success": true, "multicam_clip_id": multicam_id}))
                    .ok();
            } else {
                reply
                    .send(json!({"error": "Failed to create multicam clip (audio sync produced fewer than 2 angles)"}))
                    .ok();
            }
        }
        McpCommand::AddAngleSwitch {
            clip_id,
            position_ns,
            angle_index,
            reply,
        } => {
            let mut proj = project.borrow_mut();
            let clip = proj
                .tracks
                .iter_mut()
                .flat_map(|t| t.clips.iter_mut())
                .find(|c| c.id == clip_id);
            if let Some(clip) = clip {
                if clip.kind != crate::model::clip::ClipKind::Multicam {
                    reply
                        .send(json!({"error": "Clip is not a multicam clip"}))
                        .ok();
                } else {
                    let num_angles = clip.multicam_angles.as_ref().map(|a| a.len()).unwrap_or(0);
                    if angle_index >= num_angles {
                        reply
                            .send(json!({"error": format!("angle_index {} out of range (clip has {} angles)", angle_index, num_angles)}))
                            .ok();
                    } else {
                        clip.insert_angle_switch(position_ns, angle_index);
                        drop(proj);
                        on_project_changed();
                        reply.send(json!({"success": true})).ok();
                    }
                }
            } else {
                reply
                    .send(json!({"error": format!("Clip not found: {clip_id}")}))
                    .ok();
            }
        }
        McpCommand::ListMulticamAngles { clip_id, reply } => {
            let proj = project.borrow();
            let clip = proj
                .tracks
                .iter()
                .flat_map(|t| t.clips.iter())
                .find(|c| c.id == clip_id);
            if let Some(clip) = clip {
                if clip.kind != crate::model::clip::ClipKind::Multicam {
                    reply
                        .send(json!({"error": "Clip is not a multicam clip"}))
                        .ok();
                } else {
                    let angles: Vec<serde_json::Value> = clip
                        .multicam_angles
                        .as_ref()
                        .map(|a| {
                            a.iter()
                                .enumerate()
                                .map(|(i, angle)| {
                                    json!({
                                        "index": i,
                                        "id": angle.id,
                                        "label": angle.label,
                                        "source_path": angle.source_path,
                                        "source_in": angle.source_in,
                                        "source_out": angle.source_out,
                                        "sync_offset_ns": angle.sync_offset_ns,
                                        "volume": angle.volume,
                                        "muted": angle.muted,
                                        "brightness": angle.brightness,
                                        "contrast": angle.contrast,
                                        "saturation": angle.saturation,
                                        "temperature": angle.temperature,
                                        "tint": angle.tint,
                                        "lut_paths": angle.lut_paths,
                                    })
                                })
                                .collect()
                        })
                        .unwrap_or_default();
                    let switches: Vec<serde_json::Value> = clip
                        .multicam_switches
                        .as_ref()
                        .map(|s| {
                            s.iter()
                                .map(|sw| {
                                    json!({
                                        "position_ns": sw.position_ns,
                                        "angle_index": sw.angle_index,
                                    })
                                })
                                .collect()
                        })
                        .unwrap_or_default();
                    reply
                        .send(json!({
                            "clip_id": clip_id,
                            "angles": angles,
                            "switches": switches,
                        }))
                        .ok();
                }
            } else {
                reply
                    .send(json!({"error": format!("Clip not found: {clip_id}")}))
                    .ok();
            }
        }
        McpCommand::SetMulticamAngleAudio {
            clip_id,
            angle_index,
            volume,
            muted,
            reply,
        } => {
            let mut proj = project.borrow_mut();
            if let Some(clip) = proj.clip_mut(&clip_id) {
                if !clip.is_multicam() {
                    reply
                        .send(json!({"error": "Clip is not a multicam clip"}))
                        .ok();
                    return;
                }
                if let Some(ref mut angles) = clip.multicam_angles {
                    if angle_index >= angles.len() {
                        reply.send(json!({"error": format!("Angle index {} out of range (0..{})", angle_index, angles.len())})).ok();
                        return;
                    }
                    if let Some(v) = volume {
                        angles[angle_index].volume = v.clamp(0.0, 2.0);
                    }
                    if let Some(m) = muted {
                        angles[angle_index].muted = m;
                    }
                    proj.dirty = true;
                    drop(proj);
                    on_project_changed();
                    reply.send(json!({"success": true})).ok();
                } else {
                    reply
                        .send(json!({"error": "Multicam clip has no angles"}))
                        .ok();
                }
            } else {
                reply
                    .send(json!({"error": format!("Clip not found: {clip_id}")}))
                    .ok();
            }
        }
        McpCommand::SetMulticamAngleColor {
            clip_id,
            angle_index,
            brightness,
            contrast,
            saturation,
            temperature,
            tint,
            lut_paths,
            reply,
        } => {
            let mut proj = project.borrow_mut();
            if let Some(clip) = proj.clip_mut(&clip_id) {
                if !clip.is_multicam() {
                    reply
                        .send(json!({"error": "Clip is not a multicam clip"}))
                        .ok();
                    return;
                }
                if let Some(ref mut angles) = clip.multicam_angles {
                    if angle_index >= angles.len() {
                        reply.send(json!({"error": format!("Angle index {} out of range (0..{})", angle_index, angles.len())})).ok();
                        return;
                    }
                    let a = &mut angles[angle_index];
                    if let Some(v) = brightness {
                        a.brightness = v.clamp(-1.0, 1.0);
                    }
                    if let Some(v) = contrast {
                        a.contrast = v.clamp(0.0, 2.0);
                    }
                    if let Some(v) = saturation {
                        a.saturation = v.clamp(0.0, 2.0);
                    }
                    if let Some(v) = temperature {
                        a.temperature = v.clamp(2000.0, 10000.0);
                    }
                    if let Some(v) = tint {
                        a.tint = v.clamp(-1.0, 1.0);
                    }
                    if let Some(luts) = lut_paths {
                        a.lut_paths = luts;
                    }
                    let result = json!({
                        "success": true,
                        "brightness": a.brightness,
                        "contrast": a.contrast,
                        "saturation": a.saturation,
                        "temperature": a.temperature,
                        "tint": a.tint,
                        "lut_paths": a.lut_paths,
                    });
                    proj.dirty = true;
                    drop(proj);
                    on_project_changed();
                    reply.send(result).ok();
                } else {
                    reply
                        .send(json!({"error": "Multicam clip has no angles"}))
                        .ok();
                }
            } else {
                reply
                    .send(json!({"error": format!("Clip not found: {clip_id}")}))
                    .ok();
            }
        }
        // ── Audition / clip-versions commands ─────────────────────────────
        McpCommand::CreateAuditionClip {
            clip_ids,
            active_index,
            reply,
        } => {
            if clip_ids.len() < 2 {
                reply
                    .send(json!({"error": "At least 2 clip IDs required"}))
                    .ok();
                return;
            }
            // Collect (clip, track_id) pairs.
            let hits: Vec<(crate::model::clip::Clip, String)> = {
                let proj = project.borrow();
                clip_ids
                    .iter()
                    .filter_map(|id| {
                        proj.tracks
                            .iter()
                            .flat_map(|t| t.clips.iter().map(move |c| (t.id.clone(), c)))
                            .find(|(_, c)| &c.id == id)
                            .map(|(tid, c)| (c.clone(), tid))
                    })
                    .collect()
            };
            if hits.len() < 2 {
                reply
                    .send(json!({"error": "Could not find 2+ clips with the provided IDs"}))
                    .ok();
                return;
            }
            // All clips must be on the same track.
            let first_track = hits[0].1.clone();
            if hits.iter().any(|(_, t)| t != &first_track) {
                reply
                    .send(json!({"error": "All audition takes must come from the same track"}))
                    .ok();
                return;
            }
            // Build takes from the original clips, in caller-provided order.
            let active_index = active_index.min(hits.len() - 1);
            let anchor_start = hits[active_index].0.timeline_start;
            let takes: Vec<crate::model::clip::AuditionTake> = hits
                .iter()
                .map(|(c, _)| crate::model::clip::AuditionTake {
                    id: uuid::Uuid::new_v4().to_string(),
                    label: c.label.clone(),
                    source_path: c.source_path.clone(),
                    source_in: c.source_in,
                    source_out: c.source_out,
                    source_timecode_base_ns: c.source_timecode_base_ns,
                    media_duration_ns: c.media_duration_ns,
                })
                .collect();
            let audition =
                crate::model::clip::Clip::new_audition(anchor_start, takes, active_index);
            let audition_id = audition.id.clone();
            let selected_ids: std::collections::HashSet<String> =
                hits.iter().map(|(c, _)| c.id.clone()).collect();
            let mut proj = project.borrow_mut();
            let mut changes = Vec::new();
            for track in &proj.tracks {
                if track.id != first_track {
                    continue;
                }
                let old_clips = track.clips.clone();
                let mut new_clips: Vec<crate::model::clip::Clip> = track
                    .clips
                    .iter()
                    .filter(|c| !selected_ids.contains(&c.id))
                    .cloned()
                    .collect();
                new_clips.push(audition.clone());
                new_clips.sort_by_key(|c| c.timeline_start);
                changes.push(crate::undo::TrackClipsChange {
                    track_id: track.id.clone(),
                    old_clips,
                    new_clips,
                });
            }
            let cmd = Box::new(crate::undo::SetMultipleTracksClipsCommand {
                changes,
                label: "Create Audition".to_string(),
            });
            {
                let mut st = timeline_state.borrow_mut();
                st.history.execute(cmd, &mut proj);
            }
            drop(proj);
            on_project_changed();
            reply
                .send(json!({"success": true, "audition_clip_id": audition_id}))
                .ok();
        }
        McpCommand::AddAuditionTake {
            audition_clip_id,
            source_path,
            source_in_ns,
            source_out_ns,
            label,
            reply,
        } => {
            let exists_and_is_audition = {
                let proj = project.borrow();
                proj.clip_ref(&audition_clip_id)
                    .map(|c| c.is_audition())
                    .unwrap_or(false)
            };
            if !exists_and_is_audition {
                reply
                    .send(json!({"error": "Clip is not an audition clip"}))
                    .ok();
                return;
            }
            let take = crate::model::clip::AuditionTake {
                id: uuid::Uuid::new_v4().to_string(),
                label: label.unwrap_or_else(|| {
                    std::path::Path::new(&source_path)
                        .file_stem()
                        .and_then(|s| s.to_str())
                        .unwrap_or("Take")
                        .to_string()
                }),
                source_path,
                source_in: source_in_ns,
                source_out: source_out_ns,
                source_timecode_base_ns: None,
                media_duration_ns: None,
            };
            let take_id = take.id.clone();
            let cmd = Box::new(crate::undo::AddAuditionTakeCommand {
                clip_id: audition_clip_id,
                take,
            });
            {
                let mut proj = project.borrow_mut();
                let mut st = timeline_state.borrow_mut();
                st.history.execute(cmd, &mut proj);
            }
            on_project_changed();
            reply
                .send(json!({"success": true, "take_id": take_id}))
                .ok();
        }
        McpCommand::RemoveAuditionTake {
            audition_clip_id,
            take_index,
            reply,
        } => {
            let (is_audition, active_index, num_takes) = {
                let proj = project.borrow();
                proj.clip_ref(&audition_clip_id)
                    .map(|c| {
                        (
                            c.is_audition(),
                            c.audition_active_take_index,
                            c.audition_takes.as_ref().map(|t| t.len()).unwrap_or(0),
                        )
                    })
                    .unwrap_or((false, 0, 0))
            };
            if !is_audition {
                reply
                    .send(json!({"error": "Clip is not an audition clip"}))
                    .ok();
                return;
            }
            if take_index >= num_takes {
                reply
                    .send(json!({"error": format!("take_index {take_index} out of range (clip has {num_takes} takes)")}))
                    .ok();
                return;
            }
            if take_index == active_index {
                reply
                    .send(json!({"error": "Cannot remove the active take. Switch to a different take first."}))
                    .ok();
                return;
            }
            let cmd = Box::new(crate::undo::RemoveAuditionTakeCommand {
                clip_id: audition_clip_id,
                take_index,
                removed: std::cell::RefCell::new(None),
            });
            {
                let mut proj = project.borrow_mut();
                let mut st = timeline_state.borrow_mut();
                st.history.execute(cmd, &mut proj);
            }
            on_project_changed();
            reply.send(json!({"success": true})).ok();
        }
        McpCommand::SetActiveAuditionTake {
            audition_clip_id,
            take_index,
            reply,
        } => {
            let (snapshot, num_takes) = {
                let proj = project.borrow();
                let clip = proj.clip_ref(&audition_clip_id);
                let snap = clip.cloned();
                let n = snap
                    .as_ref()
                    .and_then(|c| c.audition_takes.as_ref().map(|t| t.len()))
                    .unwrap_or(0);
                (snap, n)
            };
            if !snapshot.as_ref().map(|c| c.is_audition()).unwrap_or(false) {
                reply
                    .send(json!({"error": "Clip is not an audition clip"}))
                    .ok();
                return;
            }
            if take_index >= num_takes {
                reply
                    .send(json!({"error": format!("take_index {take_index} out of range (clip has {num_takes} takes)")}))
                    .ok();
                return;
            }
            let cmd = Box::new(crate::undo::SetActiveAuditionTakeCommand {
                clip_id: audition_clip_id,
                new_index: take_index,
                before_snapshot: snapshot,
            });
            {
                let mut proj = project.borrow_mut();
                let mut st = timeline_state.borrow_mut();
                st.history.execute(cmd, &mut proj);
            }
            on_project_changed();
            reply.send(json!({"success": true})).ok();
        }
        McpCommand::ListAuditionTakes {
            audition_clip_id,
            reply,
        } => {
            let proj = project.borrow();
            let Some(clip) = proj.clip_ref(&audition_clip_id) else {
                reply
                    .send(json!({"error": format!("Clip not found: {audition_clip_id}")}))
                    .ok();
                return;
            };
            if !clip.is_audition() {
                reply
                    .send(json!({"error": "Clip is not an audition clip"}))
                    .ok();
                return;
            }
            let takes: Vec<serde_json::Value> = clip
                .audition_takes
                .as_ref()
                .map(|takes| {
                    takes
                        .iter()
                        .enumerate()
                        .map(|(i, t)| {
                            json!({
                                "index": i,
                                "id": t.id,
                                "label": t.label,
                                "source_path": t.source_path,
                                "source_in_ns": t.source_in,
                                "source_out_ns": t.source_out,
                                "source_timecode_base_ns": t.source_timecode_base_ns,
                                "media_duration_ns": t.media_duration_ns,
                            })
                        })
                        .collect()
                })
                .unwrap_or_default();
            reply
                .send(json!({
                    "clip_id": audition_clip_id,
                    "active_take_index": clip.audition_active_take_index,
                    "takes": takes,
                }))
                .ok();
        }
        McpCommand::FinalizeAudition {
            audition_clip_id,
            reply,
        } => {
            let snapshot = project.borrow().clip_ref(&audition_clip_id).cloned();
            if !snapshot.as_ref().map(|c| c.is_audition()).unwrap_or(false) {
                reply
                    .send(json!({"error": "Clip is not an audition clip"}))
                    .ok();
                return;
            }
            let cmd = Box::new(crate::undo::FinalizeAuditionCommand {
                clip_id: audition_clip_id,
                before_snapshot: snapshot,
            });
            {
                let mut proj = project.borrow_mut();
                let mut st = timeline_state.borrow_mut();
                st.history.execute(cmd, &mut proj);
            }
            on_project_changed();
            reply.send(json!({"success": true})).ok();
        }
        // ── Subtitle / STT commands ────────────────────────────────────────
        McpCommand::GenerateSubtitles {
            clip_id,
            language,
            reply,
        } => {
            let proj = project.borrow();
            let clip_info = proj
                .clip_ref(&clip_id)
                .map(|c| (c.source_path.clone(), c.source_in, c.source_out));
            drop(proj);
            if let Some((source_path, source_in, source_out)) = clip_info {
                stt_cache
                    .borrow_mut()
                    .request(&source_path, source_in, source_out, &language);
                reply
                    .send(json!({"success": true, "status": "queued"}))
                    .ok();
            } else {
                reply
                    .send(json!({"error": format!("Clip not found: {clip_id}")}))
                    .ok();
            }
        }
        McpCommand::GetClipSubtitles { clip_id, reply } => {
            let proj = project.borrow();
            if let Some(clip) = proj.clip_ref(&clip_id) {
                reply
                    .send(json!({
                        "clip_id": clip_id,
                        "language": &clip.subtitles_language,
                        "segments": clip.subtitle_segments.iter().map(|s| json!({
                            "id": s.id,
                            "start_ns": s.start_ns,
                            "end_ns": s.end_ns,
                            "text": s.text,
                            "words": s.words.iter().map(|w| json!({
                                "start_ns": w.start_ns,
                                "end_ns": w.end_ns,
                                "text": w.text,
                            })).collect::<Vec<_>>(),
                        })).collect::<Vec<_>>(),
                    }))
                    .ok();
            } else {
                reply
                    .send(json!({"error": format!("Clip not found: {clip_id}")}))
                    .ok();
            }
        }
        McpCommand::EditSubtitleText {
            clip_id,
            segment_id,
            text,
            reply,
        } => {
            let mut proj = project.borrow_mut();
            if let Some(clip) = proj.clip_mut(&clip_id) {
                if let Some(seg) = clip
                    .subtitle_segments
                    .iter_mut()
                    .find(|s| s.id == segment_id)
                {
                    seg.text = text;
                    proj.dirty = true;
                    drop(proj);
                    on_project_changed();
                    reply.send(json!({"success": true})).ok();
                } else {
                    reply
                        .send(json!({"error": format!("Segment not found: {segment_id}")}))
                        .ok();
                }
            } else {
                reply
                    .send(json!({"error": format!("Clip not found: {clip_id}")}))
                    .ok();
            }
        }
        McpCommand::EditSubtitleTiming {
            clip_id,
            segment_id,
            start_ns,
            end_ns,
            reply,
        } => {
            let mut proj = project.borrow_mut();
            if let Some(clip) = proj.clip_mut(&clip_id) {
                if let Some(seg) = clip
                    .subtitle_segments
                    .iter_mut()
                    .find(|s| s.id == segment_id)
                {
                    seg.start_ns = start_ns;
                    seg.end_ns = end_ns;
                    proj.dirty = true;
                    drop(proj);
                    on_project_changed();
                    reply.send(json!({"success": true})).ok();
                } else {
                    reply
                        .send(json!({"error": format!("Segment not found: {segment_id}")}))
                        .ok();
                }
            } else {
                reply
                    .send(json!({"error": format!("Clip not found: {clip_id}")}))
                    .ok();
            }
        }
        McpCommand::ClearSubtitles { clip_id, reply } => {
            let mut proj = project.borrow_mut();
            if let Some(clip) = proj.clip_mut(&clip_id) {
                clip.subtitle_segments.clear();
                proj.dirty = true;
                drop(proj);
                on_project_changed();
                reply.send(json!({"success": true})).ok();
            } else {
                reply
                    .send(json!({"error": format!("Clip not found: {clip_id}")}))
                    .ok();
            }
        }
        McpCommand::DeleteTranscriptRange {
            clip_id,
            start_word_index,
            end_word_index,
            reply,
        } => {
            // Resolve word indices to clip-local time bounds inside a scoped
            // borrow of the project, then call the same TimelineState helper
            // the UI uses so the edit is one undo entry and walks compound
            // tracks correctly.
            let resolved: Result<(u64, u64), String> = {
                let proj = project.borrow();
                if let Some(clip) = proj.clip_ref(&clip_id) {
                    if end_word_index <= start_word_index {
                        Err(format!(
                            "end_word_index ({end_word_index}) must be > start_word_index ({start_word_index})"
                        ))
                    } else {
                        // Flatten clip words in segment-then-word order.
                        let mut flat: Vec<(u64, u64)> = Vec::new();
                        for seg in &clip.subtitle_segments {
                            for w in &seg.words {
                                flat.push((w.start_ns, w.end_ns));
                            }
                        }
                        let last_idx = end_word_index.saturating_sub(1) as usize;
                        if (start_word_index as usize) >= flat.len() || last_idx >= flat.len() {
                            Err(format!(
                                "Word index out of range: clip has {} word(s)",
                                flat.len()
                            ))
                        } else {
                            let start_ns = flat[start_word_index as usize].0;
                            let end_ns = flat[last_idx].1;
                            Ok((start_ns, end_ns))
                        }
                    }
                } else {
                    Err(format!("Clip not found: {clip_id}"))
                }
            };

            match resolved {
                Ok((word_start_ns, word_end_ns)) => {
                    let changed = timeline_state.borrow_mut().delete_transcript_word_range(
                        &clip_id,
                        word_start_ns,
                        word_end_ns,
                    );
                    if changed {
                        on_project_changed();
                        reply
                            .send(json!({
                                "success": true,
                                "deleted_word_count": end_word_index - start_word_index,
                            }))
                            .ok();
                    } else {
                        reply
                            .send(json!({"error": "No change applied (zero-length range or clip not found)"}))
                            .ok();
                    }
                }
                Err(msg) => {
                    reply.send(json!({"error": msg})).ok();
                }
            }
        }
        McpCommand::SetSubtitleStyle {
            clip_id,
            font,
            color,
            outline_color,
            outline_width,
            bg_box,
            bg_box_color,
            highlight_mode,
            highlight_color,
            bold,
            italic,
            underline,
            shadow,
            highlight_bold,
            highlight_color_flag,
            highlight_underline,
            highlight_stroke,
            highlight_italic,
            highlight_background,
            highlight_shadow,
            bg_highlight_color,
            highlight_stroke_color,
            reply,
        } => {
            let mut proj = project.borrow_mut();
            if let Some(clip) = proj.clip_mut(&clip_id) {
                if let Some(f) = font {
                    clip.subtitle_font = f;
                }
                if let Some(c) = color {
                    clip.subtitle_color = c;
                }
                if let Some(c) = outline_color {
                    clip.subtitle_outline_color = c;
                }
                if let Some(w) = outline_width {
                    clip.subtitle_outline_width = w;
                }
                if let Some(b) = bg_box {
                    clip.subtitle_bg_box = b;
                }
                if let Some(c) = bg_box_color {
                    clip.subtitle_bg_box_color = c;
                }
                // Legacy highlight_mode support: convert to flags
                if let Some(mode) = highlight_mode {
                    clip.subtitle_highlight_mode = match mode.as_str() {
                        "bold" => crate::model::clip::SubtitleHighlightMode::Bold,
                        "color" => crate::model::clip::SubtitleHighlightMode::Color,
                        "underline" => crate::model::clip::SubtitleHighlightMode::Underline,
                        "stroke" => crate::model::clip::SubtitleHighlightMode::Stroke,
                        _ => crate::model::clip::SubtitleHighlightMode::None,
                    };
                    // Also set flags from legacy mode for consistency
                    clip.subtitle_highlight_flags =
                        crate::model::clip::SubtitleHighlightFlags::from_legacy(
                            clip.subtitle_highlight_mode,
                        );
                }
                if let Some(c) = highlight_color {
                    clip.subtitle_highlight_color = c;
                }
                // New base style fields
                if let Some(v) = bold {
                    clip.subtitle_bold = v;
                }
                if let Some(v) = italic {
                    clip.subtitle_italic = v;
                }
                if let Some(v) = underline {
                    clip.subtitle_underline = v;
                }
                if let Some(v) = shadow {
                    clip.subtitle_shadow = v;
                }
                // New highlight flag fields
                if let Some(v) = highlight_bold {
                    clip.subtitle_highlight_flags.bold = v;
                }
                if let Some(v) = highlight_color_flag {
                    clip.subtitle_highlight_flags.color = v;
                }
                if let Some(v) = highlight_underline {
                    clip.subtitle_highlight_flags.underline = v;
                }
                if let Some(v) = highlight_stroke {
                    clip.subtitle_highlight_flags.stroke = v;
                }
                if let Some(v) = highlight_italic {
                    clip.subtitle_highlight_flags.italic = v;
                }
                if let Some(v) = highlight_background {
                    clip.subtitle_highlight_flags.background = v;
                }
                if let Some(v) = highlight_shadow {
                    clip.subtitle_highlight_flags.shadow = v;
                }
                if let Some(c) = bg_highlight_color {
                    clip.subtitle_bg_highlight_color = c;
                }
                if let Some(c) = highlight_stroke_color {
                    clip.subtitle_highlight_stroke_color = c;
                }
                proj.dirty = true;
                drop(proj);
                on_project_changed();
                reply.send(json!({"success": true})).ok();
            } else {
                reply
                    .send(json!({"error": format!("Clip not found: {clip_id}")}))
                    .ok();
            }
        }
        McpCommand::ExportSrt { path, reply } => {
            let proj = project.borrow();
            match crate::media::export::export_srt(&proj, &path) {
                Ok(()) => reply.send(json!({"success": true, "path": path})).ok(),
                Err(e) => reply
                    .send(json!({"error": format!("SRT export failed: {e}")}))
                    .ok(),
            };
        }
        // ── Script-to-Timeline MCP tools ────────────────────────────────
        McpCommand::LoadScript { path, reply } => {
            match crate::media::script::parse_script(&path) {
                Ok(script) => {
                    let scenes: Vec<serde_json::Value> = script
                        .scenes
                        .iter()
                        .map(|s| {
                            json!({
                                "id": s.id,
                                "heading": s.heading,
                                "scene_number": s.scene_number,
                                "element_count": s.elements.len(),
                            })
                        })
                        .collect();
                    // Store script path on project for persistence.
                    project.borrow_mut().parsed_script_path = Some(path.clone());
                    // Store parsed script in a thread-local for subsequent alignment.
                    MCP_LOADED_SCRIPT.with(|cell| {
                        *cell.borrow_mut() = Some(script);
                    });
                    reply
                        .send(json!({
                            "success": true,
                            "scene_count": scenes.len(),
                            "scenes": scenes,
                        }))
                        .ok();
                }
                Err(e) => {
                    reply
                        .send(json!({"error": format!("Failed to parse script: {e}")}))
                        .ok();
                }
            }
        }
        McpCommand::GetScriptScenes { reply } => {
            let result = MCP_LOADED_SCRIPT.with(|cell| {
                let script = cell.borrow();
                match script.as_ref() {
                    Some(s) => {
                        let scenes: Vec<serde_json::Value> = s
                            .scenes
                            .iter()
                            .map(|sc| {
                                json!({
                                    "id": sc.id,
                                    "heading": sc.heading,
                                    "scene_number": sc.scene_number,
                                    "element_count": sc.elements.len(),
                                })
                            })
                            .collect();
                        json!({"scenes": scenes})
                    }
                    None => json!({"error": "No script loaded. Call load_script first."}),
                }
            });
            reply.send(result).ok();
        }
        McpCommand::RunScriptAlignment {
            clip_paths,
            confidence_threshold,
            reply,
        } => {
            let result = MCP_LOADED_SCRIPT.with(|cell| {
                let script = cell.borrow();
                match script.as_ref() {
                    None => json!({"error": "No script loaded. Call load_script first."}),
                    Some(scr) => {
                        // Collect transcripts from clips that already have subtitles.
                        let proj = project.borrow();
                        let mut transcripts: Vec<(String, Vec<crate::model::clip::SubtitleSegment>)> =
                            Vec::new();
                        for path in &clip_paths {
                            // Find clip by source path.
                            let segs: Vec<crate::model::clip::SubtitleSegment> = proj
                                .tracks
                                .iter()
                                .flat_map(|t| t.clips.iter())
                                .filter(|c| c.source_path == *path)
                                .flat_map(|c| c.subtitle_segments.clone())
                                .collect();
                            if !segs.is_empty() {
                                transcripts.push((path.clone(), segs));
                            }
                        }
                        if transcripts.is_empty() {
                            return json!({"error": "No clips with subtitles found. Run generate_subtitles first."});
                        }
                        let result = crate::media::script_align::align_transcripts_to_script(
                            scr,
                            &transcripts,
                            confidence_threshold,
                        );
                        // Store for apply_script_assembly.
                        let mappings_json: Vec<serde_json::Value> = result
                            .mappings
                            .iter()
                            .map(|m| {
                                json!({
                                    "clip_source_path": m.clip_source_path,
                                    "scene_id": m.scene_id,
                                    "confidence": m.confidence,
                                    "source_in_ns": m.source_in_ns,
                                    "source_out_ns": m.source_out_ns,
                                    "transcript_excerpt": m.transcript_excerpt,
                                })
                            })
                            .collect();
                        let response = json!({
                            "mappings": mappings_json,
                            "unmatched_clips": result.unmatched_clips,
                            "mapped_count": result.mappings.len(),
                            "unmatched_count": result.unmatched_clips.len(),
                        });
                        MCP_ALIGNMENT_RESULT.with(|cell| {
                            *cell.borrow_mut() = Some(result);
                        });
                        response
                    }
                }
            });
            reply.send(result).ok();
        }
        McpCommand::ApplyScriptAssembly {
            include_titles,
            reply,
        } => {
            let result = MCP_LOADED_SCRIPT.with(|script_cell| {
                MCP_ALIGNMENT_RESULT.with(|align_cell| {
                    let script = script_cell.borrow();
                    let alignment = align_cell.borrow();
                    match (script.as_ref(), alignment.as_ref()) {
                        (None, _) => json!({"error": "No script loaded."}),
                        (_, None) => json!({"error": "No alignment results. Call run_script_alignment first."}),
                        (Some(scr), Some(al)) => {
                            let plan = crate::media::script_assembly::build_assembly_plan(
                                scr,
                                al,
                                0,
                                3_000_000_000,
                                include_titles,
                            );
                            let old_tracks = {
                                let mut proj = project.borrow_mut();
                                let mut lib = library.borrow_mut();
                                crate::media::script_assembly::apply_assembly_plan(
                                    &mut proj, &mut lib, &plan,
                                )
                            };
                            // Register undo.
                            let new_tracks = project.borrow().tracks.clone();
                            let cmd = crate::undo::ScriptAssemblyCommand {
                                old_tracks,
                                new_tracks,
                                label: "Script to Timeline (MCP)".to_string(),
                            };
                            crate::undo::EditCommand::undo(&cmd, &mut project.borrow_mut());
                            {
                                let mut st = timeline_state.borrow_mut();
                                let mut proj = project.borrow_mut();
                                st.history.execute(Box::new(cmd), &mut proj);
                            }
                            on_project_changed();
                            json!({
                                "success": true,
                                "video_clips": plan.video_clips.len(),
                                "title_clips": plan.title_clips.len(),
                                "unmatched": plan.unmatched_paths.len(),
                            })
                        }
                    }
                })
            });
            reply.send(result).ok();
        }
        McpCommand::ReorderByScript { track_id, reply } => {
            let proj = project.borrow();
            let script_path = proj.parsed_script_path.clone();
            drop(proj);

            let scene_order: std::collections::HashMap<String, usize> =
                if let Some(ref sp) = script_path {
                    crate::media::script::parse_script(sp)
                        .map(|s| {
                            s.scenes
                                .iter()
                                .enumerate()
                                .map(|(i, sc)| (sc.id.clone(), i))
                                .collect()
                        })
                        .unwrap_or_default()
                } else {
                    std::collections::HashMap::new()
                };

            let mut proj = project.borrow_mut();
            let track = proj.tracks.iter_mut().find(|t| t.id == track_id);
            match track {
                None => {
                    reply
                        .send(json!({"error": format!("Track not found: {track_id}")}))
                        .ok();
                }
                Some(track) => {
                    let old_clips = track.clips.clone();
                    track.clips.sort_by(|a, b| {
                        let ao = a
                            .scene_id
                            .as_ref()
                            .and_then(|id| scene_order.get(id))
                            .copied()
                            .unwrap_or(usize::MAX);
                        let bo = b
                            .scene_id
                            .as_ref()
                            .and_then(|id| scene_order.get(id))
                            .copied()
                            .unwrap_or(usize::MAX);
                        ao.cmp(&bo)
                    });
                    let mut cursor: u64 = 0;
                    for clip in &mut track.clips {
                        clip.timeline_start = cursor;
                        cursor += clip.duration() as u64;
                    }
                    let new_clips = track.clips.clone();
                    let tid = track_id.clone();
                    drop(proj);

                    let cmd = crate::undo::SetTrackClipsCommand {
                        track_id: tid,
                        old_clips,
                        new_clips,
                        label: "Re-order by Script (MCP)".into(),
                    };
                    crate::undo::EditCommand::undo(&cmd, &mut project.borrow_mut());
                    {
                        let mut st = timeline_state.borrow_mut();
                        let mut p = project.borrow_mut();
                        st.history.execute(Box::new(cmd), &mut p);
                    }
                    on_project_changed();
                    reply.send(json!({"success": true})).ok();
                }
            }
        }
        // ── Marker tools ──
        McpCommand::ListMarkers { reply } => {
            let proj = project.borrow();
            let markers: Vec<Value> = proj
                .markers
                .iter()
                .map(|m| {
                    json!({
                        "id": m.id,
                        "label": m.label,
                        "position_ns": m.position_ns,
                        "color": format!("{:08X}", m.color),
                        "notes": m.notes,
                    })
                })
                .collect();
            reply.send(json!({ "markers": markers })).ok();
        }
        McpCommand::AddMarker {
            position_ns,
            label,
            color,
            notes,
            reply,
        } => {
            let mut marker = crate::model::project::Marker::new(position_ns, label);
            if let Some(c) = color {
                marker.color = c;
            }
            if let Some(n) = notes {
                marker.notes = n;
            }
            let id = marker.id.clone();
            {
                let mut st = timeline_state.borrow_mut();
                let mut p = project.borrow_mut();
                st.history
                    .execute(Box::new(crate::undo::AddMarkerCommand { marker }), &mut p);
            }
            on_project_changed();
            reply.send(json!({ "marker_id": id })).ok();
        }
        McpCommand::RemoveMarker { marker_id, reply } => {
            let marker = {
                let p = project.borrow();
                p.markers.iter().find(|m| m.id == marker_id).cloned()
            };
            match marker {
                Some(marker) => {
                    {
                        let mut st = timeline_state.borrow_mut();
                        let mut p = project.borrow_mut();
                        st.history.execute(
                            Box::new(crate::undo::RemoveMarkerCommand { marker }),
                            &mut p,
                        );
                    }
                    on_project_changed();
                    reply.send(json!({ "success": true })).ok();
                }
                None => {
                    reply
                        .send(json!({"error": format!("Marker not found: {marker_id}")}))
                        .ok();
                }
            }
        }
        McpCommand::EditMarker {
            marker_id,
            label,
            color,
            notes,
            position_ns,
            reply,
        } => {
            let old = {
                let p = project.borrow();
                p.markers.iter().find(|m| m.id == marker_id).cloned()
            };
            match old {
                Some(old) => {
                    let mut new_marker = old.clone();
                    if let Some(l) = label {
                        new_marker.label = l;
                    }
                    if let Some(c) = color {
                        new_marker.color = c;
                    }
                    if let Some(n) = notes {
                        new_marker.notes = n;
                    }
                    if let Some(pos) = position_ns {
                        new_marker.position_ns = pos;
                    }
                    let cmd = crate::undo::EditMarkerCommand {
                        marker_id: marker_id.clone(),
                        old_state: old,
                        new_state: new_marker.clone(),
                    };
                    {
                        let mut st = timeline_state.borrow_mut();
                        let mut p = project.borrow_mut();
                        st.history.execute(Box::new(cmd), &mut p);
                    }
                    on_project_changed();
                    reply
                        .send(json!({
                            "marker": {
                                "id": new_marker.id,
                                "label": new_marker.label,
                                "position_ns": new_marker.position_ns,
                                "color": format!("{:08X}", new_marker.color),
                                "notes": new_marker.notes,
                            }
                        }))
                        .ok();
                }
                None => {
                    reply
                        .send(json!({"error": format!("Marker not found: {marker_id}")}))
                        .ok();
                }
            }
        }
    }
}

/// Parse a role string from MCP (case-insensitive) to AudioRole.
fn parse_audio_role(s: &str) -> Option<crate::model::track::AudioRole> {
    match s.to_lowercase().as_str() {
        "dialogue" | "d" => Some(crate::model::track::AudioRole::Dialogue),
        "effects" | "e" => Some(crate::model::track::AudioRole::Effects),
        "music" | "m" => Some(crate::model::track::AudioRole::Music),
        _ => None,
    }
}
