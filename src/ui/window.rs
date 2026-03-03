use crate::media::player::Player;
use crate::media::program_player::{ProgramClip, ProgramPlayer};
use crate::model::clip::{Clip, ClipKind};
use crate::model::media_library::MediaItem;
use crate::model::project::Project;
use crate::model::track::TrackKind;
use crate::ui::timeline::{build_timeline_panel, TimelineState};
use crate::ui::{inspector, media_browser, preferences, preview, program_monitor, toolbar};
use glib;
use gtk4::prelude::*;
use gtk4::{self as gtk, ApplicationWindow, Orientation, Paned, ScrolledWindow};
use std::cell::{Cell, RefCell};
use std::collections::HashSet;
use std::rc::Rc;

fn auto_preview_divisor(
    project_width: u32,
    project_height: u32,
    canvas_width: i32,
    canvas_height: i32,
    current_divisor: u32,
) -> u32 {
    let cw = canvas_width.max(1) as f64;
    let ch = canvas_height.max(1) as f64;
    let pw = project_width.max(2) as f64;
    let ph = project_height.max(2) as f64;
    let ratio = (pw / cw).max(ph / ch);
    let cur = match current_divisor {
        1 | 2 | 4 => current_divisor,
        _ => 1,
    };
    match cur {
        1 => {
            if ratio >= 1.9 {
                2
            } else {
                1
            }
        }
        2 => {
            if ratio >= 3.6 {
                4
            } else if ratio <= 1.35 {
                1
            } else {
                2
            }
        }
        4 => {
            if ratio <= 2.4 {
                2
            } else {
                4
            }
        }
        _ => 1,
    }
}

fn proxy_scale_for_mode(
    mode: &crate::ui_state::ProxyMode,
) -> crate::media::proxy_cache::ProxyScale {
    match mode {
        crate::ui_state::ProxyMode::QuarterRes => crate::media::proxy_cache::ProxyScale::Quarter,
        _ => crate::media::proxy_cache::ProxyScale::Half,
    }
}

fn collect_unique_clip_sources(project: &Project) -> Vec<(String, Option<String>)> {
    let mut seen: HashSet<(String, Option<String>)> = HashSet::new();
    project
        .tracks
        .iter()
        .filter(|t| t.kind == TrackKind::Video)
        .flat_map(|t| t.clips.iter())
        .filter_map(|c| {
            let key = (c.source_path.clone(), c.lut_path.clone());
            if seen.insert(key.clone()) {
                Some(key)
            } else {
                None
            }
        })
        .collect()
}

fn active_video_track_count(project: &Project, timeline_pos_ns: u64) -> usize {
    project
        .tracks
        .iter()
        .filter(|t| t.kind == TrackKind::Video)
        .filter(|t| {
            t.clips
                .iter()
                .any(|c| timeline_pos_ns >= c.timeline_start && timeline_pos_ns < c.timeline_end())
        })
        .count()
}

/// Build and show the main application window.
pub fn build_window(app: &gtk::Application, mcp_enabled: bool) {
    let window = ApplicationWindow::builder()
        .application(app)
        .title("UltimateSlice")
        .default_width(1440)
        .default_height(900)
        .build();

    let project = Rc::new(RefCell::new(Project::new("Untitled")));

    // Shared media library (items visible in the browser, not yet on timeline)
    let library: Rc<RefCell<Vec<MediaItem>>> = Rc::new(RefCell::new(Vec::new()));
    let preferences_state = Rc::new(RefCell::new(crate::ui_state::load_preferences_state()));

    // MCP command channel — created unconditionally so the socket transport can
    // be toggled at runtime via Preferences without restarting.
    let (mcp_sender, mcp_receiver) = std::sync::mpsc::channel::<crate::mcp::McpCommand>();
    let mcp_sender = Rc::new(mcp_sender);
    let mcp_receiver = Rc::new(RefCell::new(Some(mcp_receiver))); // taken once in the MCP block
    let mcp_socket_stop: Rc<RefCell<Option<std::sync::Arc<std::sync::atomic::AtomicBool>>>> =
        Rc::new(RefCell::new(None));

    let initial_hw_accel = preferences_state.borrow().hardware_acceleration_enabled;
    let initial_playback_priority = preferences_state.borrow().playback_priority.clone();
    let initial_proxy_mode = preferences_state.borrow().proxy_mode.clone();
    let initial_preview_quality = preferences_state.borrow().preview_quality.clone();
    let initial_show_waveform_on_video = preferences_state.borrow().show_waveform_on_video;
    let initial_show_timeline_preview = preferences_state.borrow().show_timeline_preview;
    let (player, paintable) =
        Player::new(initial_hw_accel).expect("Failed to create GStreamer player");
    let player = Rc::new(RefCell::new(player));

    let (mut prog_player_raw, prog_paintable, prog_paintable2) =
        ProgramPlayer::new().expect("Failed to create program player");
    {
        let p = project.borrow();
        prog_player_raw.set_project_dimensions(p.width, p.height);
        prog_player_raw.set_frame_rate(p.frame_rate.numerator, p.frame_rate.denominator);
    }
    prog_player_raw.set_playback_priority(initial_playback_priority);
    prog_player_raw.set_proxy_enabled(initial_proxy_mode.is_enabled());
    prog_player_raw.set_preview_quality(initial_preview_quality.divisor());
    let prog_player = Rc::new(RefCell::new(prog_player_raw));

    let proxy_cache = Rc::new(RefCell::new(crate::media::proxy_cache::ProxyCache::new()));
    let effective_proxy_enabled = Rc::new(Cell::new(initial_proxy_mode.is_enabled()));
    let effective_proxy_scale_divisor = Rc::new(Cell::new(match initial_proxy_mode {
        crate::ui_state::ProxyMode::QuarterRes => 4,
        _ => 2,
    }));

    let timeline_state = Rc::new(RefCell::new(TimelineState::new(project.clone())));
    timeline_state.borrow_mut().show_waveform_on_video = initial_show_waveform_on_video;
    timeline_state.borrow_mut().show_timeline_preview = initial_show_timeline_preview;
    let pending_program_seek_ticket = Rc::new(Cell::new(0u64));
    let pending_reload_ticket = Rc::new(Cell::new(0u64));
    let suppress_resume_on_next_reload = Rc::new(Cell::new(false));

    // ── Build toolbar ─────────────────────────────────────────────────────
    let window_weak = window.downgrade();

    // Two-phase setup: create a stable Rc handle now, fill in the real
    // implementation after the timeline panel is built (so we can capture
    // a weak reference to it for explicit queue_draw).
    let on_project_changed_impl: Rc<RefCell<Option<Box<dyn Fn()>>>> = Rc::new(RefCell::new(None));
    let on_project_changed: Rc<dyn Fn()> = {
        let cb = on_project_changed_impl.clone();
        Rc::new(move || {
            if let Some(f) = cb.borrow().as_ref() {
                f();
            }
        })
    };
    let open_preferences_impl: Rc<RefCell<Option<Rc<dyn Fn()>>>> = Rc::new(RefCell::new(None));
    let open_preferences: Rc<dyn Fn()> = {
        let cb = open_preferences_impl.clone();
        Rc::new(move || {
            if let Some(f) = cb.borrow().as_ref() {
                f();
            }
        })
    };
    *open_preferences_impl.borrow_mut() = Some({
        let window_weak = window_weak.clone();
        let preferences_state = preferences_state.clone();
        let player = player.clone();
        let prog_player = prog_player.clone();
        let proxy_cache = proxy_cache.clone();
        let project = project.clone();
        let timeline_state = timeline_state.clone();
        let mcp_sender = mcp_sender.clone();
        let mcp_socket_stop = mcp_socket_stop.clone();
        Rc::new(move || {
            if let Some(win) = window_weak.upgrade() {
                let current = preferences_state.borrow().clone();
                let old_proxy_mode = current.proxy_mode.clone();
                let preferences_state = preferences_state.clone();
                let player = player.clone();
                let prog_player = prog_player.clone();
                let proxy_cache = proxy_cache.clone();
                let project = project.clone();
                let timeline_state = timeline_state.clone();
                let mcp_sender = mcp_sender.clone();
                let mcp_socket_stop = mcp_socket_stop.clone();
                let on_save: Rc<dyn Fn(crate::ui_state::PreferencesState)> =
                    Rc::new(move |new_state| {
                        *preferences_state.borrow_mut() = new_state.clone();
                        crate::ui_state::save_preferences_state(&new_state);
                        if let Err(e) = player
                            .borrow()
                            .set_hardware_acceleration(new_state.hardware_acceleration_enabled)
                        {
                            eprintln!("Failed to apply hardware acceleration setting: {e}");
                        }
                        prog_player
                            .borrow_mut()
                            .set_playback_priority(new_state.playback_priority.clone());
                        prog_player
                            .borrow_mut()
                            .set_proxy_enabled(new_state.proxy_mode.is_enabled());
                        prog_player
                            .borrow_mut()
                            .set_preview_quality(new_state.preview_quality.divisor());
                        if new_state.proxy_mode.is_enabled() {
                            // If the proxy scale changed, invalidate old entries so clips are
                            // re-transcoded at the new resolution.
                            if new_state.proxy_mode != old_proxy_mode {
                                proxy_cache.borrow_mut().invalidate_all();
                            }
                            let scale = match new_state.proxy_mode {
                                crate::ui_state::ProxyMode::QuarterRes => {
                                    crate::media::proxy_cache::ProxyScale::Quarter
                                }
                                _ => crate::media::proxy_cache::ProxyScale::Half,
                            };
                            let clips: Vec<(String, Option<String>)> = {
                                let proj = project.borrow();
                                proj.tracks
                                    .iter()
                                    .flat_map(|t| t.clips.iter())
                                    .map(|c| (c.source_path.clone(), c.lut_path.clone()))
                                    .collect()
                            };
                            {
                                let mut cache = proxy_cache.borrow_mut();
                                for (path, lut) in &clips {
                                    cache.request(path, scale, lut.as_deref());
                                }
                            }
                            let paths = proxy_cache.borrow().proxies.clone();
                            prog_player.borrow_mut().update_proxy_paths(paths);
                        }
                        timeline_state.borrow_mut().show_waveform_on_video =
                            new_state.show_waveform_on_video;
                        timeline_state.borrow_mut().show_timeline_preview =
                            new_state.show_timeline_preview;
                        // Start/stop MCP socket server based on preference change.
                        if new_state.mcp_socket_enabled && mcp_socket_stop.borrow().is_none() {
                            let stop = crate::mcp::start_mcp_socket_server((*mcp_sender).clone());
                            *mcp_socket_stop.borrow_mut() = Some(stop);
                        } else if !new_state.mcp_socket_enabled {
                            if let Some(stop) = mcp_socket_stop.borrow_mut().take() {
                                stop.store(true, std::sync::atomic::Ordering::Relaxed);
                            }
                        }
                    });
                preferences::show_preferences_dialog(win.upcast_ref(), current, on_save);
            }
        })
    });

    // ── Build inspector (after on_project_changed is defined so we can pass it) ──
    // timeline_panel_cell is shared between the inspector's on_audio_changed callback
    // and the program monitor poll timer. Declare it early (filled in after timeline build).
    let timeline_panel_cell: Rc<RefCell<Option<gtk4::Widget>>> = Rc::new(RefCell::new(None));
    // transform_overlay_cell holds the TransformOverlay after the program monitor is built.
    let transform_overlay_cell: Rc<
        RefCell<Option<Rc<crate::ui::transform_overlay::TransformOverlay>>>,
    > = Rc::new(RefCell::new(None));
    let (inspector_box, inspector_view) = inspector::build_inspector(
        project.clone(),
        // on_clip_changed: name changes → full project-changed cycle
        {
            let cb = on_project_changed.clone();
            move || cb()
        },
        // on_color_changed: slider → direct filter update, no pipeline reload
        {
            let prog_player = prog_player.clone();
            let window_weak = window_weak.clone();
            let project = project.clone();
            move |b, c, s, d, sh, shd, mid, hil| {
                prog_player
                    .borrow_mut()
                    .update_current_effects(b as f64, c as f64, s as f64, d as f64, sh as f64, shd as f64, mid as f64, hil as f64);
                // Update window title dirty marker without a full reload
                if let Some(win) = window_weak.upgrade() {
                    let proj = project.borrow();
                    let title = format!("UltimateSlice — {} •", proj.title);
                    win.set_title(Some(&title));
                }
            }
        },
        // on_audio_changed: volume/pan slider → direct update, no pipeline reload
        {
            let prog_player = prog_player.clone();
            let window_weak = window_weak.clone();
            let project = project.clone();
            let cell = timeline_panel_cell.clone();
            // clip_id comes directly from the inspector (authoritative selected clip),
            // avoiding any race with timeline_state.selected_clip_id.
            move |clip_id: &str, vol: f32, pan: f32| {
                // The inspector already persisted vol/pan to the project model.
                // Just mark dirty and update live GStreamer audio for the exact clip.
                {
                    let mut proj = project.borrow_mut();
                    proj.dirty = true;
                }
                prog_player.borrow_mut().update_audio_for_clip(clip_id, vol as f64, pan as f64);
                if let Some(win) = window_weak.upgrade() {
                    let proj = project.borrow();
                    let title = format!("UltimateSlice — {} •", proj.title);
                    win.set_title(Some(&title));
                }
                // Redraw timeline so the waveform height/color reflects the new volume.
                if let Some(ref w) = *cell.borrow() {
                    w.queue_draw();
                }
            }
        },
        // on_transform_changed: crop/rotate/flip/scale/position → direct update, no pipeline reload
        {
            let player = player.clone();
            let prog_player = prog_player.clone();
            let window_weak = window_weak.clone();
            let project = project.clone();
            let timeline_state = timeline_state.clone();
            let transform_overlay_cell = transform_overlay_cell.clone();
            move |cl, cr, ct, cb, rot, fh, fv, sc, px, py| {
                player.borrow().set_transform(cl, cr, ct, cb, rot, fh, fv);
                let selected = timeline_state.borrow().selected_clip_id.clone();
                let mut pp = prog_player.borrow_mut();
                if let Some(ref clip_id) = selected {
                    pp.update_transform_for_clip(clip_id, cl, cr, ct, cb, rot, fh, fv, sc, px, py);
                } else {
                    pp.update_current_transform(cl, cr, ct, cb, rot, fh, fv, sc, px, py);
                }
                // Keep the transform overlay in sync so drag handles reflect slider changes.
                if let Some(ref to) = *transform_overlay_cell.borrow() {
                    to.set_transform(sc, px, py);
                    to.set_crop(cl, cr, ct, cb);
                }
                if let Some(win) = window_weak.upgrade() {
                    let proj = project.borrow();
                    let title = format!("UltimateSlice — {} •", proj.title);
                    win.set_title(Some(&title));
                }
            }
        },
        // on_title_changed: text/position → direct update on textoverlay element
        {
            let prog_player = prog_player.clone();
            let project = project.clone();
            let window_weak = window_weak.clone();
            move |text: String, x: f64, y: f64| {
                // Get font/color from the currently selected clip
                let (font, color) = {
                    let proj = project.borrow();
                    let pp = prog_player.borrow();
                    if let Some(idx) = pp.current_clip_idx() {
                        if let Some(clip) = proj.tracks.iter().flat_map(|t| t.clips.iter()).nth(idx)
                        {
                            (clip.title_font.clone(), clip.title_color)
                        } else {
                            ("Sans Bold 36".to_string(), 0xFFFFFFFF)
                        }
                    } else {
                        ("Sans Bold 36".to_string(), 0xFFFFFFFF)
                    }
                };
                prog_player
                    .borrow_mut()
                    .update_current_title(&text, &font, color, x, y);
                if let Some(win) = window_weak.upgrade() {
                    let proj = project.borrow();
                    let title = format!("UltimateSlice — {} •", proj.title);
                    win.set_title(Some(&title));
                }
            }
        },
        // on_speed_changed: speed slider → reload current clip at new rate
        {
            let on_project_changed = on_project_changed.clone();
            move |_speed: f64| {
                // Reload clips so the timeline width and player both reflect the new speed.
                on_project_changed();
            }
        },
        // on_lut_changed: LUT file assigned/cleared → full project-changed cycle + proxy re-request
        {
            let on_project_changed = on_project_changed.clone();
            let proxy_cache = proxy_cache.clone();
            let preferences_state = preferences_state.clone();
            let project = project.clone();
            let prog_player = prog_player.clone();
            move |_lut_path: Option<String>| {
                on_project_changed();
                // Re-generate proxies so the newly assigned/cleared LUT is baked in.
                let prefs = preferences_state.borrow();
                if prefs.proxy_mode.is_enabled() {
                    let scale = match prefs.proxy_mode {
                        crate::ui_state::ProxyMode::QuarterRes => {
                            crate::media::proxy_cache::ProxyScale::Quarter
                        }
                        _ => crate::media::proxy_cache::ProxyScale::Half,
                    };
                    let clips: Vec<(String, Option<String>)> = {
                        let proj = project.borrow();
                        proj.tracks
                            .iter()
                            .flat_map(|t| t.clips.iter())
                            .map(|c| (c.source_path.clone(), c.lut_path.clone()))
                            .collect()
                    };
                    {
                        let mut cache = proxy_cache.borrow_mut();
                        cache.invalidate_all();
                        for (path, lut) in &clips {
                            cache.request(path, scale, lut.as_deref());
                        }
                    }
                    let paths = proxy_cache.borrow().proxies.clone();
                    prog_player.borrow_mut().update_proxy_paths(paths);
                }
            }
        },
        // on_opacity_changed: clip opacity slider → update top layer alpha immediately
        {
            let prog_player = prog_player.clone();
            let window_weak = window_weak.clone();
            let project = project.clone();
            let timeline_state = timeline_state.clone();
            move |opacity: f64| {
                let selected = timeline_state.borrow().selected_clip_id.clone();
                let mut pp = prog_player.borrow_mut();
                if let Some(ref clip_id) = selected {
                    pp.update_opacity_for_clip(clip_id, opacity);
                } else {
                    pp.update_current_opacity(opacity);
                }
                if let Some(win) = window_weak.upgrade() {
                    let proj = project.borrow();
                    let title = format!("UltimateSlice — {} •", proj.title);
                    win.set_title(Some(&title));
                }
            }
        },
        // on_reverse_changed: reverse checkbox → reload timeline and project
        {
            let on_project_changed = on_project_changed.clone();
            move |_reversed: bool| {
                // Reload clips so the timeline badge reflects the new reverse state.
                on_project_changed();
            }
        },
    );

    // Wire timeline's on_project_changed + on_seek + on_play_pause
    {
        let cb = on_project_changed.clone();
        timeline_state.borrow_mut().on_project_changed = Some(Rc::new(move || cb()));
    }
    // Wire on_clip_selected: lightweight inspector sync without pipeline rebuild.
    {
        let inspector_view = inspector_view.clone();
        let project = project.clone();
        timeline_state.borrow_mut().on_clip_selected = Some(Rc::new(move |clip_id: Option<String>| {
            let proj = project.borrow();
            inspector_view.update(&proj, clip_id.as_deref());
        }));
    }
    {
        let prog_player = prog_player.clone();
        let pending_program_seek_ticket = pending_program_seek_ticket.clone();
        timeline_state.borrow_mut().on_seek = Some(Rc::new(move |ns| {
            let ticket = pending_program_seek_ticket.get().wrapping_add(1);
            pending_program_seek_ticket.set(ticket);
            let prog_player_seek = prog_player.clone();
            let pending_program_seek_ticket_check = pending_program_seek_ticket.clone();
            glib::timeout_add_local_once(std::time::Duration::from_millis(0), move || {
                if pending_program_seek_ticket_check.get() != ticket {
                    return;
                }
                let seek_started = std::time::Instant::now();
                let needs_async = prog_player_seek.borrow_mut().seek(ns);
                log::debug!(
                    "window:on_seek timeline_pos={} needs_async={} elapsed_ms={}",
                    ns,
                    needs_async,
                    seek_started.elapsed().as_millis()
                );
                if needs_async {
                    // The pipeline is in Playing; let the GTK main loop run so
                    // gtk4paintablesink can complete its preroll, then restore Paused.
                    let pp = prog_player_seek.clone();
                    glib::timeout_add_local_once(std::time::Duration::from_millis(250), move || {
                        pp.borrow().complete_playing_pulse();
                    });
                }
            });
        }));
    }
    {
        let prog_player = prog_player.clone();
        let timeline_state2 = timeline_state.clone();
        timeline_state.borrow_mut().on_play_pause = Some(Rc::new(move || {
            let is_playing = prog_player.borrow().is_playing();
            // Pause extraction when starting playback, resume when stopping.
            if let Some(cb) = timeline_state2.borrow().on_extraction_pause.clone() {
                cb(!is_playing); // !is_playing because toggle hasn't happened yet
            }
            prog_player.borrow_mut().toggle_play_pause();
        }));
    }
    let header = toolbar::build_toolbar(
        project.clone(),
        timeline_state.clone(),
        {
            let cb = on_project_changed.clone();
            move || cb()
        },
        {
            let suppress_resume_on_next_reload = suppress_resume_on_next_reload.clone();
            move || suppress_resume_on_next_reload.set(true)
        },
    );
    window.set_titlebar(Some(&header));

    // ── Root layout: horizontal paned (content | inspector) ──────────────
    let root_hpaned = Paned::new(Orientation::Horizontal);
    root_hpaned.set_hexpand(true);
    root_hpaned.set_vexpand(true);
    root_hpaned.set_position(1120);

    let root_vpaned = Paned::new(Orientation::Vertical);
    root_vpaned.set_vexpand(true);
    root_vpaned.set_hexpand(true);
    root_vpaned.set_position(520);

    let top_paned = Paned::new(Orientation::Horizontal);
    top_paned.set_hexpand(true);
    top_paned.set_vexpand(true);
    top_paned.set_position(320);

    // ── Build preview first so we have source_marks ───────────────────────
    // on_append stub: real impl filled in below after source_marks is available.
    let on_append_impl: Rc<RefCell<Option<Rc<dyn Fn()>>>> = Rc::new(RefCell::new(None));
    let on_append: Rc<dyn Fn()> = {
        let cb = on_append_impl.clone();
        Rc::new(move || {
            if let Some(f) = cb.borrow().as_ref() {
                f();
            }
        })
    };
    let on_insert_impl: Rc<RefCell<Option<Rc<dyn Fn()>>>> = Rc::new(RefCell::new(None));
    let on_insert: Rc<dyn Fn()> = {
        let cb = on_insert_impl.clone();
        Rc::new(move || {
            if let Some(f) = cb.borrow().as_ref() {
                f();
            }
        })
    };
    let on_overwrite_impl: Rc<RefCell<Option<Rc<dyn Fn()>>>> = Rc::new(RefCell::new(None));
    let on_overwrite: Rc<dyn Fn()> = {
        let cb = on_overwrite_impl.clone();
        Rc::new(move || {
            if let Some(f) = cb.borrow().as_ref() {
                f();
            }
        })
    };
    let on_close_preview_impl: Rc<RefCell<Option<Rc<dyn Fn()>>>> = Rc::new(RefCell::new(None));
    let on_close_preview: Rc<dyn Fn()> = {
        let cb = on_close_preview_impl.clone();
        Rc::new(move || {
            if let Some(f) = cb.borrow().as_ref() {
                f();
            }
        })
    };
    let (preview_widget, source_marks, clip_name_label) = preview::build_preview(
        player.clone(),
        paintable,
        on_append.clone(),
        on_insert.clone(),
        on_overwrite.clone(),
        on_close_preview.clone(),
    );

    // Wire on_drop_clip — placed here so it can read source_marks to honour
    // the in/out selection set in the source monitor.
    {
        let project = project.clone();
        let on_project_changed = on_project_changed.clone();
        let source_marks = source_marks.clone();
        let timeline_state_for_drop = timeline_state.clone();
        timeline_state.borrow_mut().on_drop_clip = Some(Rc::new(
            move |source_path, duration_ns, track_idx, timeline_start_ns| {
                let magnetic_mode = timeline_state_for_drop.borrow().magnetic_mode;
                let mut proj = project.borrow_mut();
                if let Some(track) = proj.tracks.get_mut(track_idx) {
                    use crate::model::clip::ClipKind;
                    use crate::model::track::TrackKind;
                    let kind = match track.kind {
                        TrackKind::Video => ClipKind::Video,
                        TrackKind::Audio => ClipKind::Audio,
                    };
                    // If the source monitor has in/out marks for this clip, use them;
                    // otherwise fall back to the full source range.
                    let (src_in, src_out) = {
                        let marks = source_marks.borrow();
                        if marks.path == source_path && marks.in_ns < marks.out_ns {
                            (marks.in_ns, marks.out_ns)
                        } else {
                            (0, duration_ns)
                        }
                    };
                    let mut clip = Clip::new(source_path, src_out, timeline_start_ns, kind);
                    clip.source_in = src_in;
                    clip.source_out = src_out;
                    track.add_clip(clip);
                    if magnetic_mode {
                        track.compact_gap_free();
                    }
                    proj.dirty = true;
                    drop(proj);
                    on_project_changed();
                }
            },
        ));
    }

    // ── Build program monitor ──────────────────────────────────────────────
    let prog_monitor_host = gtk::Box::new(Orientation::Vertical, 0);
    prog_monitor_host.set_hexpand(true);
    prog_monitor_host.set_vexpand(true);
    let monitor_state = Rc::new(RefCell::new(crate::ui_state::load_program_monitor_state()));
    let popout_window_cell: Rc<RefCell<Option<ApplicationWindow>>> = Rc::new(RefCell::new(None));
    let monitor_popped = Rc::new(Cell::new(false));
    let on_toggle_popout_impl: Rc<RefCell<Option<Rc<dyn Fn()>>>> = Rc::new(RefCell::new(None));
    let on_toggle_popout: Rc<dyn Fn()> = {
        let cb = on_toggle_popout_impl.clone();
        Rc::new(move || {
            if let Some(f) = cb.borrow().as_ref() {
                f();
            }
        })
    };

    let (
        prog_monitor_widget,
        pos_label,
        speed_label,
        picture_a,
        picture_b,
        vu_meter,
        vu_peak_cell,
        prog_canvas_frame,
    ) = {
        // Build the interactive transform overlay and wire its drag callback.
        let transform_overlay = Rc::new(crate::ui::transform_overlay::TransformOverlay::new(
            {
                let inspector_view = inspector_view.clone();
                let prog_player = prog_player.clone();
                let project = project.clone();
                let timeline_state = timeline_state.clone();
                let window_weak = window_weak.clone();
                move |sc, px, py| {
                    // 1. Update selected clip in model
                    let selected = timeline_state.borrow().selected_clip_id.clone();
                    if let Some(ref clip_id) = selected {
                        let mut proj = project.borrow_mut();
                        for track in &mut proj.tracks {
                            if let Some(clip) = track.clips.iter_mut().find(|c| &c.id == clip_id) {
                                clip.scale = sc;
                                clip.position_x = px;
                                clip.position_y = py;
                                proj.dirty = true;
                                break;
                            }
                        }
                    }
                    // 2. Sync inspector sliders without re-triggering the transform callback
                    {
                        *inspector_view.updating.borrow_mut() = true;
                        inspector_view.scale_slider.set_value(sc);
                        inspector_view.position_x_slider.set_value(px);
                        inspector_view.position_y_slider.set_value(py);
                        *inspector_view.updating.borrow_mut() = false;
                    }
                    // 3. Push to GStreamer without blocking reseek (live mode handles preview)
                    let cl = inspector_view.crop_left_slider.value() as i32;
                    let crv = inspector_view.crop_right_slider.value() as i32;
                    let ct = inspector_view.crop_top_slider.value() as i32;
                    let cb = inspector_view.crop_bottom_slider.value() as i32;
                    let rot = inspector_view
                        .rotate_combo
                        .active_id()
                        .and_then(|id| id.parse::<i32>().ok())
                        .unwrap_or(0);
                    let fh = inspector_view.flip_h_btn.is_active();
                    let fv = inspector_view.flip_v_btn.is_active();
                    let mut pp = prog_player.borrow_mut();
                    pp.enter_transform_live_mode();
                    pp.set_transform_properties_only(
                        selected.as_deref(),
                        cl,
                        crv,
                        ct,
                        cb,
                        rot,
                        fh,
                        fv,
                        sc,
                        px,
                        py,
                    );
                    // 4. Update window dirty marker
                    if let Some(win) = window_weak.upgrade() {
                        let proj = project.borrow();
                        win.set_title(Some(&format!("UltimateSlice — {} •", proj.title)));
                    }
                }
            },
            {
                let inspector_view = inspector_view.clone();
                let player = player.clone();
                let prog_player = prog_player.clone();
                let project = project.clone();
                let timeline_state = timeline_state.clone();
                let window_weak = window_weak.clone();
                move |cl, cr, ct, cb| {
                    let selected = timeline_state.borrow().selected_clip_id.clone();
                    if let Some(ref clip_id) = selected {
                        let mut proj = project.borrow_mut();
                        for track in &mut proj.tracks {
                            if let Some(clip) = track.clips.iter_mut().find(|c| &c.id == clip_id) {
                                clip.crop_left = cl;
                                clip.crop_right = cr;
                                clip.crop_top = ct;
                                clip.crop_bottom = cb;
                                proj.dirty = true;
                                break;
                            }
                        }
                    }
                    {
                        *inspector_view.updating.borrow_mut() = true;
                        inspector_view.crop_left_slider.set_value(cl as f64);
                        inspector_view.crop_right_slider.set_value(cr as f64);
                        inspector_view.crop_top_slider.set_value(ct as f64);
                        inspector_view.crop_bottom_slider.set_value(cb as f64);
                        *inspector_view.updating.borrow_mut() = false;
                    }
                    let rot = inspector_view
                        .rotate_combo
                        .active_id()
                        .and_then(|id| id.parse::<i32>().ok())
                        .unwrap_or(0);
                    let fh = inspector_view.flip_h_btn.is_active();
                    let fv = inspector_view.flip_v_btn.is_active();
                    let sc = inspector_view.scale_slider.value();
                    let px = inspector_view.position_x_slider.value();
                    let py = inspector_view.position_y_slider.value();
                    player.borrow().set_transform(cl, cr, ct, cb, rot, fh, fv);
                    let mut pp = prog_player.borrow_mut();
                    pp.enter_transform_live_mode();
                    pp.set_transform_properties_only(
                        selected.as_deref(),
                        cl,
                        cr,
                        ct,
                        cb,
                        rot,
                        fh,
                        fv,
                        sc,
                        px,
                        py,
                    );
                    if let Some(win) = window_weak.upgrade() {
                        let proj = project.borrow();
                        win.set_title(Some(&format!("UltimateSlice — {} •", proj.title)));
                    }
                }
            },
            {
                // on_drag_begin: force paused editing so timeline doesn't
                // continue advancing while transform handles are dragged.
                let prog_player = prog_player.clone();
                move || {
                    prog_player.borrow_mut().pause();
                }
            },
            {
                // on_drag_end: exit live transform mode and do a final reseek
                // so the composited frame accurately reflects the last state.
                let prog_player = prog_player.clone();
                move || {
                    prog_player.borrow_mut().exit_transform_live_mode();
                }
            },
        ));
        // Initialise project dimensions (default 1920×1080 until first on_project_changed)
        {
            let p = project.borrow();
            transform_overlay.set_project_dimensions(p.width, p.height);
        }

        // Store the overlay handle for use in on_project_changed_impl
        let to = transform_overlay.clone();
        *transform_overlay_cell.borrow_mut() = Some(transform_overlay);

        program_monitor::build_program_monitor(
            prog_player.clone(),
            prog_paintable,
            prog_paintable2,
            {
                let p = project.borrow();
                p.width
            },
            {
                let p = project.borrow();
                p.height
            },
            // on_stop
            {
                let pp = prog_player.clone();
                let ts = timeline_state.clone();
                let cell = timeline_panel_cell.clone();
                move || {
                    if let Some(cb) = ts.borrow().on_extraction_pause.clone() {
                        cb(false);
                    }
                    pp.borrow_mut().stop();
                    ts.borrow_mut().playhead_ns = 0;
                    if let Some(ref w) = *cell.borrow() {
                        w.queue_draw();
                    }
                }
            },
            // on_play_pause
            {
                let pp = prog_player.clone();
                let ts = timeline_state.clone();
                move || {
                    let is_playing = pp.borrow().is_playing();
                    if let Some(cb) = ts.borrow().on_extraction_pause.clone() {
                        cb(!is_playing);
                    }
                    pp.borrow_mut().toggle_play_pause();
                }
            },
            {
                let cb = on_toggle_popout.clone();
                move || cb()
            },
            Some(to.drawing_area.clone()),
        )
    };

    // Give the transform overlay access to picture_a so it can query the actual
    // paintable intrinsic dimensions for pixel-perfect frame rect alignment.
    // Also give it the canvas AspectFrame so canvas_video_rect() can use
    // compute_bounds() to find the true canvas rect at any zoom level.
    if let Some(ref to) = *transform_overlay_cell.borrow() {
        to.set_picture(picture_a.clone());
        to.set_canvas_widget(prog_canvas_frame.clone().upcast::<gtk4::Widget>());
    }

    // ── Build colour scopes panel (hidden by default) ──────────────────────
    let (scopes_widget, scopes_state) = crate::ui::color_scopes::build_color_scopes();
    let scopes_revealer = gtk::Revealer::new();
    scopes_revealer.set_transition_type(gtk::RevealerTransitionType::SlideDown);
    scopes_revealer.set_child(Some(&scopes_widget));
    scopes_revealer.set_reveal_child(false);
    let docked_scopes_paned = Paned::new(Orientation::Vertical);
    docked_scopes_paned.set_hexpand(true);
    docked_scopes_paned.set_vexpand(true);
    docked_scopes_paned.set_resize_start_child(true);
    docked_scopes_paned.set_resize_end_child(true);
    docked_scopes_paned.set_shrink_end_child(true);
    docked_scopes_paned.set_start_child(Some(&prog_monitor_widget));
    docked_scopes_paned.set_end_child(Option::<&gtk::Widget>::None);
    {
        let state = monitor_state.borrow().clone();
        docked_scopes_paned.set_position(state.docked_split_pos.max(160));
    }
    {
        let monitor_state = monitor_state.clone();
        let monitor_popped = monitor_popped.clone();
        docked_scopes_paned.connect_position_notify(move |p| {
            if monitor_popped.get() {
                return;
            }
            let pos = p.position().max(160);
            let mut state = monitor_state.borrow_mut();
            if state.docked_split_pos != pos {
                state.docked_split_pos = pos;
                crate::ui_state::save_program_monitor_state(&state);
            }
        });
    }

    // 33 ms poll timer (~30 FPS): smoother playhead/timeline updates and
    // tighter clip-boundary handoff timing.
    {
        let pp = prog_player.clone();
        let ts = timeline_state.clone();
        let cell = timeline_panel_cell.clone();
        let last_pos_ns = Rc::new(Cell::new(u64::MAX));
        let last_pos_ns_c = last_pos_ns.clone();
        let last_draw_ns = Rc::new(Cell::new(u64::MAX));
        let last_draw_ns_c = last_draw_ns.clone();
        let vu = vu_meter.clone();
        let vu_pc = vu_peak_cell.clone();
        let scopes_rev = scopes_revealer.clone();
        let scopes_st = scopes_state.clone();
        let speed_lbl = speed_label.clone();
        let preferences_state = preferences_state.clone();
        let project = project.clone();
        let prog_canvas_frame = prog_canvas_frame.clone();
        let proxy_cache = proxy_cache.clone();
        let effective_proxy_enabled = effective_proxy_enabled.clone();
        let effective_proxy_scale_divisor = effective_proxy_scale_divisor.clone();
        let last_auto_check_us: Rc<Cell<i64>> = Rc::new(Cell::new(0));
        let last_auto_check_us_c = last_auto_check_us.clone();
        let last_auto_quality_switch_us: Rc<Cell<i64>> = Rc::new(Cell::new(0));
        let last_auto_quality_switch_us_c = last_auto_quality_switch_us.clone();
        let last_auto_proxy_switch_us: Rc<Cell<i64>> = Rc::new(Cell::new(0));
        let last_auto_proxy_switch_us_c = last_auto_proxy_switch_us.clone();
        let last_proxy_refresh_us: Rc<Cell<i64>> = Rc::new(Cell::new(0));
        let last_proxy_refresh_us_c = last_proxy_refresh_us.clone();
        glib::timeout_add_local(std::time::Duration::from_millis(33), move || {
            let (pos_ns, playing, opacity_a, opacity_b, peaks, scope_frame, jkl_rate) = {
                let mut player = pp.borrow_mut();
                let now_us = glib::monotonic_time();
                if now_us - last_auto_check_us_c.get() >= 250_000 {
                    last_auto_check_us_c.set(now_us);
                    let (preview_quality, proxy_mode) = {
                        let prefs = preferences_state.borrow();
                        (prefs.preview_quality.clone(), prefs.proxy_mode.clone())
                    };
                    let auto_preview_mode =
                        matches!(preview_quality, crate::ui_state::PreviewQuality::Auto);
                    let divisor = match preview_quality {
                        crate::ui_state::PreviewQuality::Auto => {
                            let (pw, ph) = {
                                let proj = project.borrow();
                                (proj.width, proj.height)
                            };
                            auto_preview_divisor(
                                pw,
                                ph,
                                prog_canvas_frame.width(),
                                prog_canvas_frame.height(),
                                player.preview_divisor(),
                            )
                        }
                        _ => preview_quality.divisor(),
                    };
                    let current_divisor = player.preview_divisor();
                    let can_switch_auto_quality = !player.is_playing()
                        || now_us - last_auto_quality_switch_us_c.get() >= 2_000_000;
                    if divisor == current_divisor
                        || !auto_preview_mode
                        || can_switch_auto_quality
                    {
                        if auto_preview_mode && divisor != current_divisor {
                            last_auto_quality_switch_us_c.set(now_us);
                        }
                        player.set_preview_quality(divisor);
                    }

                    // Auto-assist for heavy timelines: when manual proxy mode is Off,
                    // enable proxies for 3+ overlaps and disable with hysteresis so
                    // boundary transitions do not flap proxy state every few frames.
                    let overlap_tracks = {
                        let proj = project.borrow();
                        active_video_track_count(&proj, player.timeline_pos_ns)
                    };
                    let manual_proxy_mode = proxy_mode.is_enabled();
                    let current_proxy_enabled = effective_proxy_enabled.get();
                    let desired_proxy_enabled = if manual_proxy_mode {
                        true
                    } else if current_proxy_enabled {
                        overlap_tracks >= 2
                    } else {
                        overlap_tracks >= 3
                    };
                    let desired_scale = if manual_proxy_mode {
                        proxy_scale_for_mode(&proxy_mode)
                    } else if desired_proxy_enabled && divisor >= 4 {
                        crate::media::proxy_cache::ProxyScale::Quarter
                    } else {
                        crate::media::proxy_cache::ProxyScale::Half
                    };
                    let desired_scale_divisor = if matches!(
                        desired_scale,
                        crate::media::proxy_cache::ProxyScale::Quarter
                    ) {
                        4
                    } else {
                        2
                    };
                    let wants_proxy_change = current_proxy_enabled != desired_proxy_enabled;
                    let wants_scale_change =
                        desired_proxy_enabled
                            && effective_proxy_scale_divisor.get() != desired_scale_divisor;
                    let can_switch_auto_proxy =
                        now_us - last_auto_proxy_switch_us_c.get() >= 1_500_000;
                    if (wants_proxy_change || wants_scale_change)
                        && (manual_proxy_mode || can_switch_auto_proxy)
                    {
                        player.set_proxy_enabled(desired_proxy_enabled);
                        effective_proxy_enabled.set(desired_proxy_enabled);
                        effective_proxy_scale_divisor.set(desired_scale_divisor);
                        last_auto_proxy_switch_us_c.set(now_us);
                    }
                    let refresh_proxy_paths =
                        manual_proxy_mode
                            || (desired_proxy_enabled
                                && now_us - last_proxy_refresh_us_c.get() >= 1_000_000);
                    if desired_proxy_enabled && refresh_proxy_paths {
                        last_proxy_refresh_us_c.set(now_us);
                        let clip_sources = {
                            let proj = project.borrow();
                            collect_unique_clip_sources(&proj)
                        };
                        {
                            let mut cache = proxy_cache.borrow_mut();
                            for (path, lut) in &clip_sources {
                                cache.request(path, desired_scale, lut.as_deref());
                            }
                        }
                        let paths = proxy_cache.borrow().proxies.clone();
                        player.update_proxy_paths(paths);
                    }
                }
                player.poll();
                let (oa, ob) = player.transition_opacities();
                let sf = if scopes_rev.reveals_child() {
                    player.try_pull_scope_frame()
                } else {
                    None
                };
                let rate = player.jkl_rate();
                (
                    player.timeline_pos_ns,
                    player.is_playing(),
                    oa,
                    ob,
                    player.audio_peak_db,
                    sf,
                    rate,
                )
            };
            // Apply cross-dissolve opacities to the two program monitor pictures.
            picture_a.set_opacity(opacity_a);
            picture_b.set_opacity(opacity_b);
            // Force monitor repaint while paused so post-seek paintable updates
            // become visible even when timeline position is unchanged between ticks.
            if !playing {
                picture_a.queue_draw();
                picture_b.queue_draw();
            }
            // Update VU meter with current audio peak levels.
            vu_pc.set(peaks);
            vu.queue_draw();
            // Update colour scopes with the latest video frame.
            if let Some(frame) = scope_frame {
                crate::ui::color_scopes::update_scope_frame(&scopes_st, frame);
            }
            // Update J/K/L speed label.
            if jkl_rate == 0.0 || jkl_rate == 1.0 {
                speed_lbl.set_visible(false);
            } else {
                let abs = jkl_rate.abs() as u32;
                let arrow = if jkl_rate > 0.0 { "▶▶" } else { "◀◀" };
                speed_lbl.set_text(&format!("{arrow} {abs}×"));
                speed_lbl.set_visible(true);
            }
            if pos_ns != last_pos_ns_c.get() {
                pos_label.set_text(&program_monitor::format_timecode(pos_ns));
                ts.borrow_mut().playhead_ns = pos_ns;
                let should_draw = if !playing {
                    true
                } else {
                    let last = last_draw_ns_c.get();
                    last == u64::MAX || pos_ns.saturating_sub(last) >= 50_000_000
                };
                if should_draw {
                    if let Some(ref w) = *cell.borrow() {
                        w.queue_draw();
                    }
                    last_draw_ns_c.set(pos_ns);
                }
                last_pos_ns_c.set(pos_ns);
            }
            glib::ControlFlow::Continue
        });
    }

    // Scopes toggle for the docked monitor/scopes split.
    {
        let scopes_btn = gtk::ToggleButton::with_label("▾ Scopes");
        scopes_btn.add_css_class("flat");
        scopes_btn.set_halign(gtk::Align::Start);
        scopes_btn.set_margin_start(4);
        let rev = scopes_revealer.clone();
        let docked_paned = docked_scopes_paned.clone();
        let monitor_state = monitor_state.clone();
        let prog_player_scope = prog_player.clone();
        scopes_btn.connect_toggled(move |b| {
            let visible = b.is_active();
            prog_player_scope.borrow().set_scope_enabled(visible);
            if visible {
                if docked_paned.end_child().is_none() {
                    docked_paned.set_end_child(Some(&rev));
                    let pos = monitor_state.borrow().docked_split_pos.max(160);
                    docked_paned.set_position(pos);
                }
                rev.set_reveal_child(true);
            } else {
                let pos = docked_paned.position().max(160);
                {
                    let mut state = monitor_state.borrow_mut();
                    if state.docked_split_pos != pos {
                        state.docked_split_pos = pos;
                        crate::ui_state::save_program_monitor_state(&state);
                    }
                }
                rev.set_reveal_child(false);
                docked_paned.set_end_child(Option::<&gtk::Widget>::None);
            }
        });
        prog_monitor_host.append(&scopes_btn);
    }
    let program_empty_hint = gtk::Label::new(Some(
        "Import media, then append or insert a clip to preview your timeline here.",
    ));
    program_empty_hint.set_halign(gtk::Align::Start);
    program_empty_hint.set_xalign(0.0);
    program_empty_hint.set_wrap(true);
    program_empty_hint.set_margin_start(8);
    program_empty_hint.set_margin_end(8);
    program_empty_hint.set_margin_bottom(6);
    program_empty_hint.add_css_class("panel-empty-state");
    program_empty_hint.set_visible(true);
    prog_monitor_host.append(&program_empty_hint);
    prog_monitor_host.append(&docked_scopes_paned);
    top_paned.set_end_child(Some(&prog_monitor_host));

    // Program monitor pop-out/dock toggle
    *on_toggle_popout_impl.borrow_mut() = Some({
        let app = app.clone();
        let docked_paned = docked_scopes_paned.clone();
        let monitor = prog_monitor_widget.clone();
        let pop_cell = popout_window_cell.clone();
        let popped = monitor_popped.clone();
        let monitor_state = monitor_state.clone();
        let scopes_rev = scopes_revealer.clone();
        Rc::new(move || {
            if !popped.get() {
                let state = monitor_state.borrow().clone();
                let pop_win = ApplicationWindow::builder()
                    .application(&app)
                    .title("UltimateSlice — Program Monitor")
                    .default_width(state.width.max(320))
                    .default_height(state.height.max(180))
                    .build();

                docked_paned.set_start_child(Option::<&gtk::Widget>::None);
                pop_win.set_child(Some(&monitor));
                scopes_rev.set_vexpand(true);

                let docked_paned_c = docked_paned.clone();
                let monitor_c = monitor.clone();
                let pop_cell_c = pop_cell.clone();
                let popped_c = popped.clone();
                let monitor_state_c = monitor_state.clone();
                let scopes_rev_c = scopes_rev.clone();
                pop_win.connect_close_request(move |w| {
                    let mut state = monitor_state_c.borrow_mut();
                    state.width = w.width().max(320);
                    state.height = w.height().max(180);
                    state.popped = false;
                    crate::ui_state::save_program_monitor_state(&state);
                    w.set_child(Option::<&gtk::Widget>::None);
                    if monitor_c.parent().is_none() {
                        docked_paned_c.set_start_child(Some(&monitor_c));
                    }
                    scopes_rev_c.set_vexpand(false);
                    popped_c.set(false);
                    *pop_cell_c.borrow_mut() = None;
                    glib::Propagation::Proceed
                });

                pop_win.present();
                popped.set(true);
                {
                    let mut state = monitor_state.borrow_mut();
                    state.popped = true;
                    crate::ui_state::save_program_monitor_state(&state);
                }
                *pop_cell.borrow_mut() = Some(pop_win);
            } else {
                let win = pop_cell.borrow().as_ref().cloned();
                if let Some(w) = win {
                    w.close();
                }
            }
        })
    });

    // ── on_append: reads source_marks, creates clip, adds to timeline ─────
    *on_append_impl.borrow_mut() = Some({
        let project = project.clone();
        let source_marks = source_marks.clone();
        let on_project_changed = on_project_changed.clone();
        let timeline_state = timeline_state.clone();
        Rc::new(move || {
            let marks = source_marks.borrow();
            if marks.path.is_empty() {
                return;
            }
            let path = marks.path.clone();
            let in_ns = marks.in_ns;
            let out_ns = marks.out_ns;
            let is_audio = marks.is_audio_only;
            drop(marks);

            let ts = timeline_state.borrow();
            let magnetic_mode = ts.magnetic_mode;
            let active_tid = ts.selected_track_id.clone();
            drop(ts);

            let target_kind = if is_audio {
                TrackKind::Audio
            } else {
                TrackKind::Video
            };
            let clip_kind = if is_audio {
                ClipKind::Audio
            } else {
                ClipKind::Video
            };

            {
                let mut proj = project.borrow_mut();
                // Prefer the active track if its kind matches, else first matching track
                let track = if let Some(ref tid) = active_tid {
                    if proj
                        .tracks
                        .iter()
                        .any(|t| &t.id == tid && t.kind == target_kind)
                    {
                        proj.tracks.iter_mut().find(|t| &t.id == tid)
                    } else {
                        proj.tracks.iter_mut().find(|t| t.kind == target_kind)
                    }
                } else {
                    proj.tracks.iter_mut().find(|t| t.kind == target_kind)
                };
                if let Some(track) = track {
                    let timeline_start = track.duration();
                    let mut clip = Clip::new(path, out_ns, timeline_start, clip_kind);
                    clip.source_in = in_ns;
                    clip.source_out = out_ns;
                    track.add_clip(clip);
                    if magnetic_mode {
                        track.compact_gap_free();
                    }
                    proj.dirty = true;
                }
            }
            on_project_changed();
        })
    });

    // ── on_insert: reads source_marks, creates clip at playhead, shifts subsequent clips ──
    *on_insert_impl.borrow_mut() = Some({
        let project = project.clone();
        let source_marks = source_marks.clone();
        let on_project_changed = on_project_changed.clone();
        let timeline_state = timeline_state.clone();
        Rc::new(move || {
            let marks = source_marks.borrow();
            if marks.path.is_empty() {
                return;
            }
            let path = marks.path.clone();
            let in_ns = marks.in_ns;
            let out_ns = marks.out_ns;
            let is_audio = marks.is_audio_only;
            drop(marks);

            let ts = timeline_state.borrow();
            let magnetic_mode = ts.magnetic_mode;
            let playhead = ts.playhead_ns;
            let active_tid = ts.selected_track_id.clone();
            drop(ts);

            let target_kind = if is_audio {
                TrackKind::Audio
            } else {
                TrackKind::Video
            };
            let clip_kind = if is_audio {
                ClipKind::Audio
            } else {
                ClipKind::Video
            };
            let clip_duration = out_ns.saturating_sub(in_ns);
            if clip_duration == 0 {
                return;
            }

            {
                let mut proj = project.borrow_mut();
                let track = if let Some(ref tid) = active_tid {
                    if proj
                        .tracks
                        .iter()
                        .any(|t| &t.id == tid && t.kind == target_kind)
                    {
                        proj.tracks.iter_mut().find(|t| &t.id == tid)
                    } else {
                        proj.tracks.iter_mut().find(|t| t.kind == target_kind)
                    }
                } else {
                    proj.tracks.iter_mut().find(|t| t.kind == target_kind)
                };
                if let Some(track) = track {
                    let old_clips = track.clips.clone();
                    let track_id = track.id.clone();

                    // Shift clips that start at or after playhead
                    for c in track.clips.iter_mut() {
                        if c.timeline_start >= playhead {
                            c.timeline_start += clip_duration;
                        }
                    }

                    let mut new_clip = Clip::new(path, out_ns, playhead, clip_kind);
                    new_clip.source_in = in_ns;
                    new_clip.source_out = out_ns;
                    track.add_clip(new_clip);

                    if magnetic_mode {
                        track.compact_gap_free();
                    }

                    let new_clips = track.clips.clone();
                    drop(proj);

                    let cmd = crate::undo::SetTrackClipsCommand {
                        track_id,
                        old_clips,
                        new_clips,
                        label: "Insert at playhead".to_string(),
                    };
                    let st = timeline_state.borrow_mut();
                    let project_rc = st.project.clone();
                    drop(st);
                    let mut proj = project_rc.borrow_mut();
                    // Push directly — clips are already applied
                    timeline_state
                        .borrow_mut()
                        .history
                        .undo_stack
                        .push(Box::new(cmd));
                    timeline_state.borrow_mut().history.redo_stack.clear();
                    proj.dirty = true;
                }
            }
            on_project_changed();
        })
    });

    // ── on_overwrite: reads source_marks, replaces timeline range at playhead ──
    *on_overwrite_impl.borrow_mut() = Some({
        let project = project.clone();
        let source_marks = source_marks.clone();
        let on_project_changed = on_project_changed.clone();
        let timeline_state = timeline_state.clone();
        Rc::new(move || {
            let marks = source_marks.borrow();
            if marks.path.is_empty() {
                return;
            }
            let path = marks.path.clone();
            let in_ns = marks.in_ns;
            let out_ns = marks.out_ns;
            let is_audio = marks.is_audio_only;
            drop(marks);

            let ts = timeline_state.borrow();
            let magnetic_mode = ts.magnetic_mode;
            let playhead = ts.playhead_ns;
            let active_tid = ts.selected_track_id.clone();
            drop(ts);

            let target_kind = if is_audio {
                TrackKind::Audio
            } else {
                TrackKind::Video
            };
            let clip_kind = if is_audio {
                ClipKind::Audio
            } else {
                ClipKind::Video
            };
            let clip_duration = out_ns.saturating_sub(in_ns);
            if clip_duration == 0 {
                return;
            }

            let range_start = playhead;
            let range_end = playhead + clip_duration;

            {
                let mut proj = project.borrow_mut();
                let track = if let Some(ref tid) = active_tid {
                    if proj
                        .tracks
                        .iter()
                        .any(|t| &t.id == tid && t.kind == target_kind)
                    {
                        proj.tracks.iter_mut().find(|t| &t.id == tid)
                    } else {
                        proj.tracks.iter_mut().find(|t| t.kind == target_kind)
                    }
                } else {
                    proj.tracks.iter_mut().find(|t| t.kind == target_kind)
                };
                if let Some(track) = track {
                    let old_clips = track.clips.clone();
                    let track_id = track.id.clone();

                    // Resolve overlaps with the overwrite range
                    let mut kept: Vec<Clip> = Vec::new();
                    for c in track.clips.drain(..) {
                        let c_start = c.timeline_start;
                        let c_end = c.timeline_end();
                        if c_end <= range_start || c_start >= range_end {
                            // No overlap — keep as-is
                            kept.push(c);
                        } else if c_start >= range_start && c_end <= range_end {
                            // Fully contained — remove (skip)
                        } else if c_start < range_start && c_end > range_end {
                            // Clip spans entire range — split into two
                            let mut left = c.clone();
                            left.source_out = left.source_in + (range_start - c_start);
                            let mut right = c;
                            let trim_left = range_end - right.timeline_start;
                            right.source_in += trim_left;
                            right.timeline_start = range_end;
                            kept.push(left);
                            kept.push(right);
                        } else if c_start < range_start {
                            // Overlap at end — trim out-point
                            let mut trimmed = c;
                            trimmed.source_out =
                                trimmed.source_in + (range_start - trimmed.timeline_start);
                            kept.push(trimmed);
                        } else {
                            // Overlap at start — trim in-point, adjust timeline_start
                            let mut trimmed = c;
                            let trim_amount = range_end - trimmed.timeline_start;
                            trimmed.source_in += trim_amount;
                            trimmed.timeline_start = range_end;
                            kept.push(trimmed);
                        }
                    }
                    track.clips = kept;

                    let mut new_clip = Clip::new(path, out_ns, playhead, clip_kind);
                    new_clip.source_in = in_ns;
                    new_clip.source_out = out_ns;
                    track.add_clip(new_clip);

                    if magnetic_mode {
                        track.compact_gap_free();
                    }

                    let new_clips = track.clips.clone();
                    drop(proj);

                    let cmd = crate::undo::SetTrackClipsCommand {
                        track_id,
                        old_clips,
                        new_clips,
                        label: "Overwrite at playhead".to_string(),
                    };
                    let st = timeline_state.borrow_mut();
                    let project_rc = st.project.clone();
                    drop(st);
                    let mut proj = project_rc.borrow_mut();
                    timeline_state
                        .borrow_mut()
                        .history
                        .undo_stack
                        .push(Box::new(cmd));
                    timeline_state.borrow_mut().history.redo_stack.clear();
                    proj.dirty = true;
                }
            }
            on_project_changed();
        })
    });

    // ── on_source_selected: loads clip into player + resets source_marks ──
    let on_source_selected: Rc<dyn Fn(String, u64)> = {
        let player = player.clone();
        let source_marks = source_marks.clone();
        let preview_widget = preview_widget.clone();
        let clip_name_label = clip_name_label.clone();
        let library = library.clone();
        Rc::new(move |path: String, duration_ns: u64| {
            // Show the source preview now that a clip is selected
            preview_widget.set_visible(true);
            // Update the clip name label
            let name = std::path::Path::new(&path)
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or(&path)
                .to_string();
            clip_name_label.set_text(&name);
            let uri = format!("file://{path}");
            // Guard against duplicate selection-changed emissions for the same
            // item; avoid redundant playbin reconfiguration.
            let should_reload = {
                let m = source_marks.borrow();
                m.path != path
            };
            // Look up is_audio_only from library item (set by background probe).
            let audio_only = library
                .borrow()
                .iter()
                .find(|i| i.source_path == path)
                .map(|i| i.is_audio_only)
                .unwrap_or(false);
            if should_reload {
                let _ = player.borrow().load(&uri);
            }
            let mut m = source_marks.borrow_mut();
            m.path = path;
            m.duration_ns = duration_ns;
            m.in_ns = 0;
            m.out_ns = duration_ns;
            m.display_pos_ns = 0;
            m.is_audio_only = audio_only;
        })
    };

    // ── Media browser ─────────────────────────────────────────────────────
    let (browser, clear_media_selection) =
        media_browser::build_media_browser(library.clone(), on_source_selected.clone());
    // ── on_close_preview: deselect media + hide preview + reset source state ──
    *on_close_preview_impl.borrow_mut() = Some({
        let clear_media_selection = clear_media_selection.clone();
        let preview_widget = preview_widget.clone();
        let clip_name_label = clip_name_label.clone();
        let source_marks = source_marks.clone();
        let player = player.clone();
        Rc::new(move || {
            clear_media_selection();
            preview_widget.set_visible(false);
            clip_name_label.set_text("No source loaded");
            {
                let mut m = source_marks.borrow_mut();
                *m = crate::model::media_library::SourceMarks::default();
            }
            let _ = player.borrow().stop();
        })
    });
    // Left panel: vertical Paned — browser (top) + source preview (bottom, hidden until selection)
    // The Paned lets the user resize the split after a source is selected.
    preview_widget.set_visible(false);
    let left_vpaned = Paned::new(Orientation::Vertical);
    left_vpaned.set_vexpand(true);
    left_vpaned.set_position(320); // browser gets ~320 px by default
    left_vpaned.set_start_child(Some(&browser));
    left_vpaned.set_end_child(Some(&preview_widget));
    top_paned.set_start_child(Some(&left_vpaned));

    root_vpaned.set_start_child(Some(&top_paned));

    // ── Timeline ──────────────────────────────────────────────────────────
    let timeline_scroll = ScrolledWindow::new();
    timeline_scroll.set_policy(gtk::PolicyType::Automatic, gtk::PolicyType::Automatic);
    timeline_scroll.set_vexpand(true);
    timeline_scroll.set_hexpand(true);

    let (timeline_panel, timeline_area) =
        build_timeline_panel(timeline_state.clone(), on_project_changed.clone());
    timeline_scroll.set_child(Some(&timeline_panel));
    root_vpaned.set_end_child(Some(&timeline_scroll));

    // Fill in the timeline area cell so the poll timer + stop button can redraw it.
    *timeline_panel_cell.borrow_mut() = Some(timeline_area.clone().upcast::<gtk4::Widget>());

    // Now that timeline_panel exists, fill in the real on_project_changed implementation.
    // This runs after every edit: updates title, inspector, program player clip list,
    // and queues a redraw on the timeline.
    {
        let inspector_view = inspector_view.clone();
        let project = project.clone();
        let timeline_state = timeline_state.clone();
        let library = library.clone();
        let window_weak = window_weak.clone();
        let prog_player = prog_player.clone();
        let proxy_cache = proxy_cache.clone();
        let preferences_state = preferences_state.clone();
        let panel_weak = timeline_area.downgrade();
        let transform_overlay_cell = transform_overlay_cell.clone();
        let prog_canvas_frame = prog_canvas_frame.clone();
        let program_empty_hint = program_empty_hint.clone();
        let pending_reload_ticket = pending_reload_ticket.clone();
        let suppress_resume_on_next_reload = suppress_resume_on_next_reload.clone();

        *on_project_changed_impl.borrow_mut() = Some(Box::new(move || {
            // Update window title
            if let Some(win) = window_weak.upgrade() {
                let proj = project.borrow();
                let dirty_marker = if proj.dirty { " •" } else { "" };
                win.set_title(Some(&format!(
                    "UltimateSlice — {}{dirty_marker}",
                    proj.title
                )));
            }

            // Update inspector and collect program clips — drop proj borrow before GStreamer call
            let (clips, media_from_project, project_dims, project_frame_rate): (
                Vec<ProgramClip>,
                Vec<(String, u64)>,
                (u32, u32),
                (u32, u32),
            ) = {
                let proj = project.borrow();
                let selected = timeline_state.borrow().selected_clip_id.clone();
                inspector_view.update(&proj, selected.as_deref());

                // Sync transform overlay: show handles when a clip is selected
                if let Some(ref to) = *transform_overlay_cell.borrow() {
                    to.set_project_dimensions(proj.width, proj.height);
                    // Keep canvas frame aspect ratio in sync with project dimensions.
                    if proj.height > 0 {
                        prog_canvas_frame.set_ratio(proj.width as f32 / proj.height as f32);
                    }
                    if let Some(ref cid) = selected {
                        let clip_opt = proj
                            .tracks
                            .iter()
                            .flat_map(|t| t.clips.iter())
                            .find(|c| &c.id == cid);
                        if let Some(c) = clip_opt {
                            // Don't show transform handles for audio-only clips — they
                            // have no visual representation on the canvas.
                            let is_visual = c.kind != ClipKind::Audio;
                            if is_visual {
                                to.set_transform(c.scale, c.position_x, c.position_y);
                                to.set_crop(c.crop_left, c.crop_right, c.crop_top, c.crop_bottom);
                                to.set_clip_selected(true);
                            } else {
                                to.set_clip_selected(false);
                            }
                        } else {
                            to.set_clip_selected(false);
                        }
                    } else {
                        to.set_clip_selected(false);
                    }
                }

                let clips = proj
                    .tracks
                    .iter()
                    .enumerate()
                    .flat_map(|(t_idx, t)| {
                        let audio_only = t.kind == TrackKind::Audio;
                        t.clips.iter().map(move |c| ProgramClip {
                            id: c.id.clone(),
                            source_path: c.source_path.clone(),
                            source_in_ns: c.source_in,
                            source_out_ns: c.source_out,
                            timeline_start_ns: c.timeline_start,
                            brightness: c.brightness as f64,
                            contrast: c.contrast as f64,
                            saturation: c.saturation as f64,
                            denoise: c.denoise as f64,
                            sharpness: c.sharpness as f64,
                            volume: c.volume as f64,
                            pan: c.pan as f64,
                            crop_left: c.crop_left,
                            crop_right: c.crop_right,
                            crop_top: c.crop_top,
                            crop_bottom: c.crop_bottom,
                            rotate: c.rotate,
                            flip_h: c.flip_h,
                            flip_v: c.flip_v,
                            title_text: c.title_text.clone(),
                            title_font: c.title_font.clone(),
                            title_color: c.title_color,
                            title_x: c.title_x,
                            title_y: c.title_y,
                            speed: c.speed,
                            is_audio_only: audio_only,
                            track_index: t_idx,
                            transition_after: c.transition_after.clone(),
                            transition_after_ns: c.transition_after_ns,
                            lut_path: c.lut_path.clone(),
                            scale: c.scale,
                            opacity: c.opacity,
                            position_x: c.position_x,
                            position_y: c.position_y,
                            shadows: c.shadows as f64,
                            midtones: c.midtones as f64,
                            highlights: c.highlights as f64,
                            has_audio: true, // default; overridden by probe cache below
                        })
                    })
                    .collect();
                // Keep media browser in sync with timeline clip sources after project open/load.
                // Collect only unique source paths to avoid redundant work.
                let mut media_seen: HashSet<&str> = HashSet::new();
                let media: Vec<(String, u64)> = proj
                    .tracks
                    .iter()
                    .flat_map(|t| t.clips.iter())
                    .filter(|c| media_seen.insert(c.source_path.as_str()))
                    .map(|c| (c.source_path.clone(), c.source_out))
                    .collect();
                (clips, media, (proj.width, proj.height), (proj.frame_rate.numerator, proj.frame_rate.denominator))
            }; // proj borrow dropped here — safe to call GStreamer below
            program_empty_hint.set_visible(clips.is_empty());

            {
                let mut lib = library.borrow_mut();
                let seen: HashSet<&str> = lib.iter().map(|i| i.source_path.as_str()).collect();
                let new_items: Vec<_> = media_from_project
                    .into_iter()
                    .filter(|(path, _)| !seen.contains(path.as_str()))
                    .collect();
                for (path, dur) in new_items {
                    lib.push(MediaItem::new(path, dur));
                }
            }

            // Reload program player — preserve current position so the monitor
            // doesn't jump to 0 on every project change (e.g., clip name edit).
            let suppress_resume = suppress_resume_on_next_reload.replace(false);
            let (prev_pos, was_playing) = {
                let pp = prog_player.borrow();
                (
                    pp.timeline_pos_ns,
                    !suppress_resume
                        && matches!(pp.state(), crate::media::player::PlayerState::Playing),
                )
            };
            let (proj_w, proj_h) = project_dims;
            let (fr_num, fr_den) = project_frame_rate;
            let prog_player_reload = prog_player.clone();
            let preferences_state_reload = preferences_state.clone();
            let project_reload = project.clone();
            let proxy_cache_reload = proxy_cache.clone();
            let reload_ticket = pending_reload_ticket.get().wrapping_add(1);
            pending_reload_ticket.set(reload_ticket);
            let pending_reload_ticket_phase1 = pending_reload_ticket.clone();
            glib::timeout_add_local_once(std::time::Duration::from_millis(0), move || {
                if pending_reload_ticket_phase1.get() != reload_ticket {
                    return;
                }
                let phase1_started = std::time::Instant::now();
                // Resolve proxy paths BEFORE load_clips so the first
                // rebuild_pipeline_at() uses proxies instead of originals.
                {
                    let prefs = preferences_state_reload.borrow();
                    if prefs.proxy_mode.is_enabled() {
                        let scale = match prefs.proxy_mode {
                            crate::ui_state::ProxyMode::QuarterRes => {
                                crate::media::proxy_cache::ProxyScale::Quarter
                            }
                            _ => crate::media::proxy_cache::ProxyScale::Half,
                        };
                        let clip_sources: Vec<(String, Option<String>)> = {
                            let proj = project_reload.borrow();
                            let mut seen: HashSet<(String, Option<String>)> = HashSet::new();
                            proj.tracks
                                .iter()
                                .flat_map(|t| t.clips.iter())
                                .filter_map(|c| {
                                    let key = (c.source_path.clone(), c.lut_path.clone());
                                    if seen.insert(key.clone()) {
                                        Some(key)
                                    } else {
                                        None
                                    }
                                })
                                .collect()
                        };
                        {
                            let mut cache = proxy_cache_reload.borrow_mut();
                            for (path, lut) in &clip_sources {
                                cache.request(path, scale, lut.as_deref());
                            }
                        }
                        let paths = proxy_cache_reload.borrow().proxies.clone();
                        prog_player_reload.borrow_mut().update_proxy_paths(paths);
                    }
                }

                {
                    let mut pp = prog_player_reload.borrow_mut();
                    pp.set_project_dimensions(proj_w, proj_h);
                    pp.set_frame_rate(fr_num, fr_den);
                    pp.load_clips(clips);
                }
                log::debug!(
                    "window:on_project_changed phase1_load ticket={} elapsed_ms={}",
                    reload_ticket,
                    phase1_started.elapsed().as_millis()
                );

                let prog_player_reload_phase2 = prog_player_reload.clone();
                let pending_reload_ticket_phase2 = pending_reload_ticket_phase1.clone();
                glib::timeout_add_local_once(std::time::Duration::from_millis(16), move || {
                    if pending_reload_ticket_phase2.get() != reload_ticket {
                        return;
                    }
                    let phase2_started = std::time::Instant::now();
                    let mut pp = prog_player_reload_phase2.borrow_mut();
                    if !pp.clips.is_empty() {
                        if was_playing {
                            // Preserve playback behavior after clip reloads.
                            let _ = pp.seek(prev_pos);
                            pp.play();
                        } else {
                            // Rebuild the pipeline at the previous position so the
                            // program monitor shows the correct composited frame.
                            // Without this, load_clips() leaves no decoder slots
                            // loaded and the monitor can stay on the previous frame.
                            let pos = prev_pos.min(pp.timeline_dur_ns);
                            let needs_async = pp.seek(pos);
                            if needs_async {
                                drop(pp);
                                let pp2 = prog_player_reload_phase2.clone();
                                glib::timeout_add_local_once(
                                    std::time::Duration::from_millis(250),
                                    move || {
                                        pp2.borrow().complete_playing_pulse();
                                    },
                                );
                            }
                        }
                    }
                    log::debug!(
                        "window:on_project_changed phase2_seek ticket={} elapsed_ms={}",
                        reload_ticket,
                        phase2_started.elapsed().as_millis()
                    );
                });
            });

            // Force immediate timeline redraw (don't wait for 100ms timer)
            if let Some(p) = panel_weak.upgrade() {
                if let Some(area_widget) = p.first_child() {
                    if let Ok(area) = area_widget.downcast::<gtk::DrawingArea>() {
                        let track_count = project.borrow().tracks.len().max(1);
                        area.set_content_height((24.0 + 60.0 * track_count as f64) as i32);
                        area.queue_draw();
                    } else {
                        p.queue_draw();
                    }
                } else {
                    p.queue_draw();
                }
            }
        }));
    }

    root_hpaned.set_start_child(Some(&root_vpaned));

    // Right sidebar: inspector + transitions pane
    let right_sidebar = gtk::Box::new(Orientation::Vertical, 6);
    right_sidebar.set_margin_start(6);
    right_sidebar.set_margin_end(6);
    right_sidebar.set_margin_top(6);
    right_sidebar.set_margin_bottom(6);

    let inspector_scroll = ScrolledWindow::new();
    inspector_scroll.set_vexpand(true);
    inspector_scroll.set_policy(gtk::PolicyType::Never, gtk::PolicyType::Automatic);
    inspector_scroll.set_child(Some(&inspector_box));
    right_sidebar.append(&inspector_scroll);

    let transitions_header = gtk::Box::new(Orientation::Horizontal, 6);
    let transitions_title = gtk::Label::new(Some("Transitions"));
    transitions_title.set_halign(gtk::Align::Start);
    transitions_title.set_hexpand(true);
    let transitions_toggle = gtk::Button::with_label("Hide Transitions");
    transitions_toggle.add_css_class("small-btn");
    transitions_header.append(&transitions_title);
    transitions_header.append(&transitions_toggle);
    right_sidebar.append(&transitions_header);

    let transitions_revealer = gtk::Revealer::new();
    transitions_revealer.set_reveal_child(true);
    let transitions_list = gtk::ListBox::new();
    transitions_list.add_css_class("boxed-list");
    transitions_list.set_selection_mode(gtk::SelectionMode::None);

    // Helper: add a transition row with drag-source to the list.
    let add_transition_row = |list: &gtk::ListBox, display: &str, kind: &str| {
        let row = gtk::ListBoxRow::new();
        let bx = gtk::Box::new(Orientation::Horizontal, 6);
        bx.set_margin_start(8);
        bx.set_margin_end(8);
        bx.set_margin_top(6);
        bx.set_margin_bottom(6);
        let name_lbl = gtk::Label::new(Some(display));
        name_lbl.set_halign(gtk::Align::Start);
        name_lbl.set_hexpand(true);
        let hint_lbl = gtk::Label::new(Some("Drag to clip boundary"));
        hint_lbl.add_css_class("dim-label");
        bx.append(&name_lbl);
        bx.append(&hint_lbl);
        row.set_child(Some(&bx));
        let drag_src = gtk::DragSource::new();
        drag_src.set_actions(gdk4::DragAction::COPY);
        drag_src.set_exclusive(false);
        let payload = format!("transition:{kind}");
        let val = glib::Value::from(&payload);
        drag_src.set_content(Some(&gdk4::ContentProvider::for_value(&val)));
        row.add_controller(drag_src);
        list.append(&row);
    };

    add_transition_row(&transitions_list, "Cross-dissolve", "cross_dissolve");
    add_transition_row(&transitions_list, "Fade to black", "fade_to_black");
    add_transition_row(&transitions_list, "Wipe right →", "wipe_right");
    add_transition_row(&transitions_list, "← Wipe left", "wipe_left");

    transitions_revealer.set_child(Some(&transitions_list));
    right_sidebar.append(&transitions_revealer);

    {
        let revealer = transitions_revealer.clone();
        transitions_toggle.connect_clicked(move |btn| {
            let show = !revealer.reveals_child();
            revealer.set_reveal_child(show);
            btn.set_label(if show {
                "Hide Transitions"
            } else {
                "Show Transitions"
            });
        });
    }

    root_hpaned.set_end_child(Some(&right_sidebar));

    // ── Status bar (proxy progress) ───────────────────────────────────────
    let status_bar = gtk::Box::new(Orientation::Horizontal, 8);
    status_bar.set_margin_start(8);
    status_bar.set_margin_end(8);
    status_bar.set_margin_top(4);
    status_bar.set_margin_bottom(4);
    status_bar.add_css_class("status-bar");
    status_bar.set_visible(false);
    let status_label = gtk::Label::new(Some("Generating proxies…"));
    status_label.set_halign(gtk::Align::Start);
    status_label.add_css_class("status-bar-label");
    let status_progress = gtk::ProgressBar::new();
    status_progress.set_hexpand(true);
    status_progress.add_css_class("proxy-progress");
    status_bar.append(&status_label);
    status_bar.append(&status_progress);

    // Wrap main content + status bar in a vertical box
    let outer_vbox = gtk::Box::new(Orientation::Vertical, 0);
    outer_vbox.append(&root_hpaned);
    outer_vbox.append(&status_bar);
    window.set_child(Some(&outer_vbox));

    // Poll proxy cache every 500ms to drain completed transcodes and update status bar.
    {
        let proxy_cache = proxy_cache.clone();
        let prog_player = prog_player.clone();
        let effective_proxy_enabled = effective_proxy_enabled.clone();
        let status_bar = status_bar.clone();
        let status_label = status_label.clone();
        let status_progress = status_progress.clone();
        glib::timeout_add_local(std::time::Duration::from_millis(500), move || {
            let resolved = proxy_cache.borrow_mut().poll();
            // Always sync proxy paths when proxies are effectively enabled — disk-cached proxies
            // are added synchronously by request() and never appear in `resolved`.
            if effective_proxy_enabled.get() {
                if !resolved.is_empty() || !proxy_cache.borrow().proxies.is_empty() {
                    let paths = proxy_cache.borrow().proxies.clone();
                    prog_player.borrow_mut().update_proxy_paths(paths);
                }
            }
            let progress = proxy_cache.borrow().progress();
            if progress.in_flight {
                status_bar.set_visible(true);
                status_label.set_text(&format!(
                    "Generating proxies… {}/{}",
                    progress.completed, progress.total
                ));
                if progress.total > 0 {
                    status_progress.set_fraction(progress.completed as f64 / progress.total as f64);
                }
            } else {
                status_bar.set_visible(false);
            }
            glib::ControlFlow::Continue
        });
    }

    // ── MCP server (stdio + optional socket) ────────────────────────────
    {
        let mcp_receiver = mcp_receiver
            .borrow_mut()
            .take()
            .expect("MCP receiver already taken");

        // Stdio transport (--mcp flag)
        if mcp_enabled {
            let stdio_sender = (*mcp_sender).clone();
            std::thread::spawn(move || {
                crate::mcp::server::run_stdio_server(stdio_sender);
            });
            eprintln!("[MCP] Server listening on stdio (JSON-RPC 2.0 / MCP 2024-11-05)");
        }

        // Socket transport (Preferences toggle) — can start/stop at runtime.
        if preferences_state.borrow().mcp_socket_enabled {
            let stop = crate::mcp::start_mcp_socket_server((*mcp_sender).clone());
            *mcp_socket_stop.borrow_mut() = Some(stop);
        }

        let project = project.clone();
        let library = library.clone();
        let player = player.clone();
        let prog_player = prog_player.clone();
        let timeline_state = timeline_state.clone();
        let preferences_state = preferences_state.clone();
        let proxy_cache = proxy_cache.clone();
        let on_close_preview = on_close_preview.clone();
        let on_project_changed = on_project_changed.clone();
        let suppress_resume_on_next_reload = suppress_resume_on_next_reload.clone();
        let window_weak = window.downgrade();
        // Poll the mpsc channel every 10 ms on the GTK main thread.
        glib::timeout_add_local(std::time::Duration::from_millis(10), move || {
            while let Ok(cmd) = mcp_receiver.try_recv() {
                if let Some(win) = window_weak.upgrade() {
                    handle_mcp_command(
                        cmd,
                        &win,
                        &project,
                        &library,
                        &player,
                        &prog_player,
                        &timeline_state,
                        &preferences_state,
                        &proxy_cache,
                        &on_close_preview,
                        &on_project_changed,
                        &suppress_resume_on_next_reload,
                    );
                }
            }
            glib::ControlFlow::Continue
        });
    }

    // Auto-save: every 60 seconds, write to a temp file if the project is dirty.
    {
        let project = project.clone();
        let window_weak = window.downgrade();
        glib::timeout_add_local(std::time::Duration::from_secs(60), move || {
            let is_dirty = project.borrow().dirty;
            if is_dirty {
                let xml_result = {
                    let proj = project.borrow();
                    crate::fcpxml::writer::write_fcpxml(&proj)
                };
                if let Ok(xml) = xml_result {
                    let path = "/tmp/ultimateslice-autosave.fcpxml";
                    if std::fs::write(path, xml).is_ok() {
                        if let Some(win) = window_weak.upgrade() {
                            let proj = project.borrow();
                            let title = format!("UltimateSlice — {} (Auto-saved)", proj.title);
                            win.set_title(Some(&title));
                            // Restore normal title after 3 seconds
                            let win_w2 = win.downgrade();
                            let proj_title = proj.title.clone();
                            glib::timeout_add_local_once(
                                std::time::Duration::from_secs(3),
                                move || {
                                    if let Some(w) = win_w2.upgrade() {
                                        w.set_title(Some(&format!(
                                            "UltimateSlice — {} •",
                                            proj_title
                                        )));
                                    }
                                },
                            );
                        }
                    }
                }
            }
            glib::ControlFlow::Continue
        });
    }

    // ── Window-level J/K/L: shuttle scrubbing in the program monitor ─────────
    // L — play forward, each press cycles speed: 1×→2×→4×→8×
    // K — pause / reset shuttle speed
    // J — play backward, each press cycles speed: −1×→−2×→−4×→−8×
    {
        use std::cell::Cell;
        let prog_player = prog_player.clone();
        let jkl_rate_cell: Rc<Cell<f64>> = Rc::new(Cell::new(0.0));
        let key_ctrl = gtk4::EventControllerKey::new();
        key_ctrl.set_propagation_phase(gtk4::PropagationPhase::Capture);
        key_ctrl.connect_key_pressed(move |ctrl, key, _, _mods| {
            use gtk4::gdk::Key;
            if key != Key::j
                && key != Key::J
                && key != Key::k
                && key != Key::K
                && key != Key::l
                && key != Key::L
            {
                return glib::Propagation::Proceed;
            }
            // Don't intercept when a text entry has focus.
            if let Some(widget) = ctrl.widget() {
                if let Some(focused) = widget.root().and_then(|r| r.focus()) {
                    if focused.is::<gtk4::Entry>() || focused.is::<gtk4::TextView>() {
                        return glib::Propagation::Proceed;
                    }
                }
            }
            let current = jkl_rate_cell.get();
            let new_rate = if key == Key::k || key == Key::K {
                0.0
            } else if key == Key::l || key == Key::L {
                // Cycle: stopped/reverse → 1×, then double up to 8×.
                match current as i64 {
                    r if r <= 0 => 1.0,
                    1 => 2.0,
                    2 => 4.0,
                    _ => 8.0,
                }
            } else {
                // J: cycle: stopped/forward → −1×, then double up to −8×.
                match current as i64 {
                    r if r >= 0 => -1.0,
                    -1 => -2.0,
                    -2 => -4.0,
                    _ => -8.0,
                }
            };
            jkl_rate_cell.set(new_rate);
            prog_player.borrow_mut().set_jkl_rate(new_rate);
            glib::Propagation::Stop
        });
        window.add_controller(key_ctrl);
    }
    // ── Window-level M key: add marker at current playhead (works regardless of focus) ──
    {
        let project = project.clone();
        let prog_player = prog_player.clone();
        let on_project_changed = on_project_changed.clone();
        let key_ctrl = gtk4::EventControllerKey::new();
        key_ctrl.set_propagation_phase(gtk4::PropagationPhase::Capture);
        key_ctrl.connect_key_pressed(move |ctrl, key, _, _mods| {
            use gtk4::gdk::Key;
            if key != Key::m && key != Key::M {
                return glib::Propagation::Proceed;
            }
            // Don't intercept M when a text entry or similar has focus
            if let Some(widget) = ctrl.widget() {
                if let Some(focused) = widget.root().and_then(|r| r.focus()) {
                    if focused.is::<gtk4::Entry>() || focused.is::<gtk4::TextView>() {
                        return glib::Propagation::Proceed;
                    }
                }
            }
            let pos = prog_player.borrow().timeline_pos_ns;
            project.borrow_mut().add_marker(pos, "Marker");
            on_project_changed();
            glib::Propagation::Stop
        });
        window.add_controller(key_ctrl);
    }
    // ── Window-level , and . keys: Insert / Overwrite at playhead ─────────
    {
        let on_insert = on_insert.clone();
        let on_overwrite = on_overwrite.clone();
        let key_ctrl = gtk4::EventControllerKey::new();
        key_ctrl.set_propagation_phase(gtk4::PropagationPhase::Capture);
        key_ctrl.connect_key_pressed(move |ctrl, key, _, mods| {
            use gtk4::gdk::{Key, ModifierType};
            // Skip if Ctrl is held (Ctrl+, = Preferences)
            if mods.contains(ModifierType::CONTROL_MASK) {
                return glib::Propagation::Proceed;
            }
            if key != Key::comma && key != Key::period {
                return glib::Propagation::Proceed;
            }
            // Don't intercept when a text entry has focus
            if let Some(widget) = ctrl.widget() {
                if let Some(focused) = widget.root().and_then(|r| r.focus()) {
                    if focused.is::<gtk4::Entry>() || focused.is::<gtk4::TextView>() {
                        return glib::Propagation::Proceed;
                    }
                }
            }
            if key == Key::comma {
                on_insert();
            } else {
                on_overwrite();
            }
            glib::Propagation::Stop
        });
        window.add_controller(key_ctrl);
    }
    // ── Window-level Ctrl+, key: open Preferences ───────────────────────────
    {
        let open_preferences = open_preferences.clone();
        let key_ctrl = gtk4::EventControllerKey::new();
        key_ctrl.set_propagation_phase(gtk4::PropagationPhase::Capture);
        key_ctrl.connect_key_pressed(move |_, key, _, mods| {
            use gtk4::gdk::{Key, ModifierType};
            if mods.contains(ModifierType::CONTROL_MASK) && key == Key::comma {
                open_preferences();
                return glib::Propagation::Stop;
            }
            glib::Propagation::Proceed
        });
        window.add_controller(key_ctrl);
    }

    if monitor_state.borrow().popped {
        on_toggle_popout();
    }

    window.present();
}

// ── MCP command handler (runs on GTK main thread) ────────────────────────────

fn handle_mcp_command(
    cmd: crate::mcp::McpCommand,
    window: &gtk::ApplicationWindow,
    project: &Rc<RefCell<Project>>,
    library: &Rc<RefCell<Vec<MediaItem>>>,
    player: &Rc<RefCell<Player>>,
    prog_player: &Rc<RefCell<ProgramPlayer>>,
    timeline_state: &Rc<RefCell<TimelineState>>,
    preferences_state: &Rc<RefCell<crate::ui_state::PreferencesState>>,
    proxy_cache: &Rc<RefCell<crate::media::proxy_cache::ProxyCache>>,
    on_close_preview: &Rc<dyn Fn()>,
    on_project_changed: &Rc<dyn Fn()>,
    suppress_resume_on_next_reload: &Rc<Cell<bool>>,
) {
    use crate::mcp::McpCommand;
    use crate::model::clip::ClipKind;
    use serde_json::json;

    match cmd {
        McpCommand::GetProject { reply } => {
            let proj = project.borrow();
            let v = serde_json::to_value(&*proj).unwrap_or(json!(null));
            reply.send(v).ok();
        }

        McpCommand::ListTracks { reply } => {
            let proj = project.borrow();
            let tracks: Vec<_> = proj
                .tracks
                .iter()
                .enumerate()
                .map(|(i, t)| {
                    json!({
                        "index":      i,
                        "id":         t.id,
                        "label":      t.label,
                        "kind":       format!("{:?}", t.kind),
                        "clip_count": t.clips.len(),
                        "muted":      t.muted,
                        "locked":     t.locked,
                    })
                })
                .collect();
            reply.send(json!(tracks)).ok();
        }

        McpCommand::ListClips { reply } => {
            let proj = project.borrow();
            let clips: Vec<_> = proj
                .tracks
                .iter()
                .enumerate()
                .flat_map(|(ti, track)| {
                    track.clips.iter().map(move |c| {
                        json!({
                            "id":               c.id,
                            "label":            c.label,
                            "source_path":      c.source_path,
                            "track_index":      ti,
                            "track_id":         track.id,
                            "timeline_start_ns": c.timeline_start,
                            "source_in_ns":     c.source_in,
                            "source_out_ns":    c.source_out,
                            "duration_ns":      c.duration(),
                            "brightness":       c.brightness,
                            "contrast":         c.contrast,
                            "saturation":       c.saturation,
                            "denoise":          c.denoise,
                            "sharpness":        c.sharpness,
                            "shadows":          c.shadows,
                            "midtones":         c.midtones,
                            "highlights":       c.highlights,
                            "opacity":          c.opacity,
                        })
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

        McpCommand::SetMagneticMode { enabled, reply } => {
            timeline_state.borrow_mut().magnetic_mode = enabled;
            reply
                .send(json!({"success": true, "magnetic_mode": enabled}))
                .ok();
            on_project_changed();
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
                    "proxy_mode": prefs.proxy_mode.as_str(),
                    "show_timeline_preview": prefs.show_timeline_preview,
                    "gsk_renderer": prefs.gsk_renderer.as_str(),
                    "preview_quality": prefs.preview_quality.as_str()
                }))
                .ok();
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
                            "playback_priority": new_state.playback_priority.as_str()
                        }))
                        .ok();
                }
                Err(e) => {
                    reply
                        .send(json!({
                            "success": false,
                            "hardware_acceleration_enabled": enabled,
                            "playback_priority": new_state.playback_priority.as_str(),
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

        McpCommand::SetProxyMode { mode, reply } => {
            let parsed = crate::ui_state::ProxyMode::from_str(&mode);
            let enabled = parsed.is_enabled();
            let new_state = {
                let mut prefs = preferences_state.borrow_mut();
                prefs.proxy_mode = parsed;
                prefs.clone()
            };
            crate::ui_state::save_preferences_state(&new_state);
            prog_player.borrow_mut().set_proxy_enabled(enabled);
            if enabled {
                let scale = match new_state.proxy_mode {
                    crate::ui_state::ProxyMode::QuarterRes => {
                        crate::media::proxy_cache::ProxyScale::Quarter
                    }
                    _ => crate::media::proxy_cache::ProxyScale::Half,
                };
                let clip_sources: Vec<(String, Option<String>)> = {
                    let proj = project.borrow();
                    let mut seen: HashSet<(String, Option<String>)> = HashSet::new();
                    proj.tracks
                        .iter()
                        .flat_map(|t| t.clips.iter())
                        .filter_map(|c| {
                            let key = (c.source_path.clone(), c.lut_path.clone());
                            if seen.insert(key.clone()) {
                                Some(key)
                            } else {
                                None
                            }
                        })
                        .collect()
                };
                {
                    let mut cache = proxy_cache.borrow_mut();
                    cache.invalidate_all();
                    for (path, lut) in &clip_sources {
                        cache.request(path, scale, lut.as_deref());
                    }
                }
                let paths = proxy_cache.borrow().proxies.clone();
                prog_player.borrow_mut().update_proxy_paths(paths);
            }
            reply
                .send(json!({
                    "success": true,
                    "proxy_mode": new_state.proxy_mode.as_str()
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

        McpCommand::AddClip {
            source_path,
            track_index,
            timeline_start_ns,
            source_in_ns,
            source_out_ns,
            reply,
        } => {
            let magnetic_mode = timeline_state.borrow().magnetic_mode;
            let clip_id = {
                let mut proj = project.borrow_mut();
                if let Some(track) = proj.tracks.get_mut(track_index) {
                    let mut clip = Clip::new(
                        source_path,
                        source_out_ns,
                        timeline_start_ns,
                        ClipKind::Video,
                    );
                    clip.source_in = source_in_ns;
                    clip.source_out = source_out_ns;
                    let id = clip.id.clone();
                    track.add_clip(clip);
                    if magnetic_mode {
                        track.compact_gap_free();
                    }
                    proj.dirty = true;
                    Ok(id)
                } else {
                    Err(format!("Track index {track_index} does not exist"))
                }
            };
            match clip_id {
                Ok(id) => {
                    reply.send(json!({"success": true, "clip_id": id})).ok();
                    on_project_changed();
                }
                Err(e) => {
                    reply.send(json!({"success": false, "error": e})).ok();
                }
            }
        }

        McpCommand::RemoveClip { clip_id, reply } => {
            let magnetic_mode = timeline_state.borrow().magnetic_mode;
            let mut proj = project.borrow_mut();
            let mut found = false;
            for track in proj.tracks.iter_mut() {
                if let Some(pos) = track.clips.iter().position(|c| c.id == clip_id) {
                    track.clips.remove(pos);
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

        McpCommand::MoveClip {
            clip_id,
            new_start_ns,
            reply,
        } => {
            let magnetic_mode = timeline_state.borrow().magnetic_mode;
            let mut proj = project.borrow_mut();
            let mut found = false;
            for track in proj.tracks.iter_mut() {
                if let Some(idx) = track.clips.iter().position(|c| c.id == clip_id) {
                    track.clips[idx].timeline_start = new_start_ns;
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

        McpCommand::SlipClip {
            clip_id,
            delta_ns,
            reply,
        } => {
            let mut proj = project.borrow_mut();
            let mut found = false;
            for track in proj.tracks.iter_mut() {
                if let Some(clip) = track.clips.iter_mut().find(|c| c.id == clip_id) {
                    let new_in = (clip.source_in as i64 + delta_ns).max(0) as u64;
                    let new_out =
                        (clip.source_out as i64 + delta_ns).max(new_in as i64 + 1_000_000) as u64;
                    clip.source_in = new_in;
                    clip.source_out = new_out;
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
                on_project_changed();
            }
        }

        McpCommand::SetClipColor {
            clip_id,
            brightness,
            contrast,
            saturation,
            denoise,
            sharpness,
            shadows,
            midtones,
            highlights,
            reply,
        } => {
            let mut proj = project.borrow_mut();
            let mut found = false;
            'outer: for track in proj.tracks.iter_mut() {
                for clip in track.clips.iter_mut() {
                    if clip.id == clip_id {
                        clip.brightness = brightness as f32;
                        clip.contrast = contrast as f32;
                        clip.saturation = saturation as f32;
                        clip.denoise = denoise as f32;
                        clip.sharpness = sharpness as f32;
                        clip.shadows = shadows as f32;
                        clip.midtones = midtones as f32;
                        clip.highlights = highlights as f32;
                        proj.dirty = true;
                        found = true;
                        break 'outer;
                    }
                }
            }
            drop(proj);
            reply.send(json!({"success": found})).ok();
            if found {
                on_project_changed();
            }
        }

        McpCommand::SetClipLut {
            clip_id,
            lut_path,
            reply,
        } => {
            let mut proj = project.borrow_mut();
            let mut found = false;
            'outer: for track in proj.tracks.iter_mut() {
                for clip in track.clips.iter_mut() {
                    if clip.id == clip_id {
                        clip.lut_path = lut_path.clone();
                        proj.dirty = true;
                        found = true;
                        break 'outer;
                    }
                }
            }
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
            reply,
        } => {
            let mut proj = project.borrow_mut();
            let mut found = false;
            'outer: for track in proj.tracks.iter_mut() {
                for clip in track.clips.iter_mut() {
                    if clip.id == clip_id {
                        clip.scale = scale.clamp(0.1, 4.0);
                        clip.position_x = position_x.clamp(-1.0, 1.0);
                        clip.position_y = position_y.clamp(-1.0, 1.0);
                        proj.dirty = true;
                        found = true;
                        break 'outer;
                    }
                }
            }
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
            let mut found = false;
            'outer: for track in proj.tracks.iter_mut() {
                for clip in track.clips.iter_mut() {
                    if clip.id == clip_id {
                        clip.opacity = opacity.clamp(0.0, 1.0);
                        proj.dirty = true;
                        found = true;
                        break 'outer;
                    }
                }
            }
            drop(proj);
            reply.send(json!({"success": found})).ok();
            if found {
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
            let result = {
                let proj = project.borrow();
                crate::fcpxml::writer::write_fcpxml(&proj)
                    .and_then(|xml| std::fs::write(&path, xml).map_err(|e| anyhow::anyhow!(e)))
            };
            match result {
                Ok(_) => reply.send(json!({"success": true, "path": path})).ok(),
                Err(e) => reply
                    .send(json!({"success": false, "error": e.to_string()}))
                    .ok(),
            };
        }

        McpCommand::OpenFcpxml { path, reply } => {
            let (tx, rx) = std::sync::mpsc::sync_channel::<Result<Project, String>>(1);
            let path_bg = path.clone();
            std::thread::spawn(move || {
                let result = std::fs::read_to_string(&path_bg)
                    .map_err(|e| e.to_string())
                    .and_then(|xml| {
                        crate::fcpxml::parser::parse_fcpxml(&xml).map_err(|e| e.to_string())
                    });
                let _ = tx.send(result);
            });
            timeline_state.borrow_mut().loading = true;
            let project = project.clone();
            let timeline_state = timeline_state.clone();
            let on_project_changed = on_project_changed.clone();
            let suppress_resume_on_next_reload = suppress_resume_on_next_reload.clone();
            glib::timeout_add_local(std::time::Duration::from_millis(10), move || {
                match rx.try_recv() {
                    Ok(Ok(mut new_proj)) => {
                        new_proj.file_path = Some(path.clone());
                        let track_count = new_proj.tracks.len();
                        let clip_count: usize = new_proj.tracks.iter().map(|t| t.clips.len()).sum();
                        *project.borrow_mut() = new_proj;
                        timeline_state.borrow_mut().loading = false;
                        reply.send(json!({"success": true, "path": path, "tracks": track_count, "clips": clip_count})).ok();
                        suppress_resume_on_next_reload.set(true);
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

        McpCommand::ExportMp4 { path, reply } => {
            let proj = project.borrow().clone();
            std::thread::spawn(move || {
                let (done_tx, done_rx) = std::sync::mpsc::sync_channel::<Result<(), String>>(1);
                let proj_worker = proj.clone();
                let path_worker = path.clone();
                std::thread::spawn(move || {
                    let (tx, _rx) = std::sync::mpsc::channel();
                    let result = crate::media::export::export_project(
                        &proj_worker,
                        &path_worker,
                        crate::media::export::ExportOptions::default(),
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

        McpCommand::ListLibrary { reply } => {
            let lib = library.borrow();
            let items: Vec<_> = lib
                .iter()
                .map(|item| {
                    json!({
                        "label":       item.label,
                        "source_path": item.source_path,
                        "duration_ns": item.duration_ns,
                        "duration_s":  item.duration_ns as f64 / 1_000_000_000.0,
                    })
                })
                .collect();
            reply.send(json!(items)).ok();
        }

        McpCommand::ImportMedia { path, reply } => {
            let uri = format!("file://{path}");
            let duration_ns =
                crate::ui::media_browser::probe_duration(&uri).unwrap_or(10 * 1_000_000_000);
            let audio_only = crate::ui::media_browser::probe_is_audio_only(&uri);
            let mut item = MediaItem::new(path.clone(), duration_ns);
            item.is_audio_only = audio_only;
            let label = item.label.clone();
            library.borrow_mut().push(item);
            reply
                .send(json!({"success": true, "label": label, "duration_ns": duration_ns}))
                .ok();
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
            reply,
        } => {
            let candidate = {
                let proj = project.borrow();
                let Some(track) = proj.tracks.get(track_index) else {
                    reply.send(json!({"error":"Track index out of range","track_count":proj.tracks.len()})).ok();
                    return;
                };
                if clip_index + 1 >= track.clips.len() {
                    reply.send(json!({"error":"clip_index must reference a clip with a following clip","clip_count":track.clips.len()})).ok();
                    return;
                }
                let clip = &track.clips[clip_index];
                Some((
                    track.id.clone(),
                    clip.id.clone(),
                    clip.transition_after.clone(),
                    clip.transition_after_ns,
                    clip.duration(),
                ))
            };
            let Some((track_id, clip_id, old_kind, old_duration_ns, clip_dur_ns)) = candidate
            else {
                return;
            };
            let new_kind = kind.trim().to_string();
            let supported = ["cross_dissolve", "fade_to_black", "wipe_right", "wipe_left"];
            if !new_kind.is_empty() && !supported.contains(&new_kind.as_str()) {
                reply
                    .send(json!({"error":"Unsupported transition kind","supported":supported}))
                    .ok();
                return;
            }
            let new_duration_ns = if new_kind.is_empty() {
                0
            } else {
                duration_ns.min(clip_dur_ns.saturating_sub(1_000_000))
            };
            let cmd = crate::undo::SetClipTransitionCommand {
                clip_id,
                track_id,
                old_transition: old_kind,
                old_transition_ns: old_duration_ns,
                new_transition: new_kind.clone(),
                new_transition_ns: new_duration_ns,
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
                    "kind": new_kind,
                    "duration_ns": new_duration_ns
                }))
                .ok();
            on_project_changed();
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
            reply.send(json!({"success": true, "title": title})).ok();
            suppress_resume_on_next_reload.set(true);
            on_project_changed();
        }

        McpCommand::InsertClip {
            source_path,
            source_in_ns,
            source_out_ns,
            track_index,
            reply,
        } => {
            let clip_duration = source_out_ns.saturating_sub(source_in_ns);
            if clip_duration == 0 {
                reply
                    .send(json!({"error": "source_in_ns must be less than source_out_ns"}))
                    .ok();
                return;
            }
            let playhead = timeline_state.borrow().playhead_ns;
            let magnetic_mode = timeline_state.borrow().magnetic_mode;
            let result = {
                let mut proj = project.borrow_mut();
                let track = if let Some(idx) = track_index {
                    proj.tracks.get_mut(idx)
                } else {
                    proj.tracks
                        .iter_mut()
                        .find(|t| t.kind == crate::model::track::TrackKind::Video)
                };
                if let Some(track) = track {
                    let old_clips = track.clips.clone();
                    let track_id = track.id.clone();
                    for c in track.clips.iter_mut() {
                        if c.timeline_start >= playhead {
                            c.timeline_start += clip_duration;
                        }
                    }
                    let mut new_clip =
                        Clip::new(source_path, source_out_ns, playhead, ClipKind::Video);
                    new_clip.source_in = source_in_ns;
                    new_clip.source_out = source_out_ns;
                    let clip_id = new_clip.id.clone();
                    track.add_clip(new_clip);
                    if magnetic_mode {
                        track.compact_gap_free();
                    }
                    let new_clips = track.clips.clone();
                    proj.dirty = true;
                    Ok((track_id, clip_id, old_clips, new_clips))
                } else {
                    Err("No matching track found")
                }
            };
            match result {
                Ok((track_id, clip_id, old_clips, new_clips)) => {
                    let cmd = crate::undo::SetTrackClipsCommand {
                        track_id,
                        old_clips,
                        new_clips,
                        label: "Insert at playhead (MCP)".to_string(),
                    };
                    let st = timeline_state.borrow_mut();
                    let project_rc = st.project.clone();
                    drop(st);
                    let mut proj = project_rc.borrow_mut();
                    timeline_state
                        .borrow_mut()
                        .history
                        .undo_stack
                        .push(Box::new(cmd));
                    timeline_state.borrow_mut().history.redo_stack.clear();
                    proj.dirty = true;
                    drop(proj);
                    reply
                        .send(json!({"success": true, "clip_id": clip_id}))
                        .ok();
                    on_project_changed();
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
            reply,
        } => {
            let clip_duration = source_out_ns.saturating_sub(source_in_ns);
            if clip_duration == 0 {
                reply
                    .send(json!({"error": "source_in_ns must be less than source_out_ns"}))
                    .ok();
                return;
            }
            let playhead = timeline_state.borrow().playhead_ns;
            let magnetic_mode = timeline_state.borrow().magnetic_mode;
            let range_start = playhead;
            let range_end = playhead + clip_duration;
            let result = {
                let mut proj = project.borrow_mut();
                let track = if let Some(idx) = track_index {
                    proj.tracks.get_mut(idx)
                } else {
                    proj.tracks
                        .iter_mut()
                        .find(|t| t.kind == crate::model::track::TrackKind::Video)
                };
                if let Some(track) = track {
                    let old_clips = track.clips.clone();
                    let track_id = track.id.clone();
                    let mut kept: Vec<Clip> = Vec::new();
                    for c in track.clips.drain(..) {
                        let c_start = c.timeline_start;
                        let c_end = c.timeline_end();
                        if c_end <= range_start || c_start >= range_end {
                            kept.push(c);
                        } else if c_start >= range_start && c_end <= range_end {
                            // fully contained — remove
                        } else if c_start < range_start && c_end > range_end {
                            let mut left = c.clone();
                            left.source_out = left.source_in + (range_start - c_start);
                            let mut right = c;
                            let trim_left = range_end - right.timeline_start;
                            right.source_in += trim_left;
                            right.timeline_start = range_end;
                            kept.push(left);
                            kept.push(right);
                        } else if c_start < range_start {
                            let mut trimmed = c;
                            trimmed.source_out =
                                trimmed.source_in + (range_start - trimmed.timeline_start);
                            kept.push(trimmed);
                        } else {
                            let mut trimmed = c;
                            let trim_amount = range_end - trimmed.timeline_start;
                            trimmed.source_in += trim_amount;
                            trimmed.timeline_start = range_end;
                            kept.push(trimmed);
                        }
                    }
                    track.clips = kept;
                    let mut new_clip =
                        Clip::new(source_path, source_out_ns, playhead, ClipKind::Video);
                    new_clip.source_in = source_in_ns;
                    new_clip.source_out = source_out_ns;
                    let clip_id = new_clip.id.clone();
                    track.add_clip(new_clip);
                    if magnetic_mode {
                        track.compact_gap_free();
                    }
                    let new_clips = track.clips.clone();
                    proj.dirty = true;
                    Ok((track_id, clip_id, old_clips, new_clips))
                } else {
                    Err("No matching track found")
                }
            };
            match result {
                Ok((track_id, clip_id, old_clips, new_clips)) => {
                    let cmd = crate::undo::SetTrackClipsCommand {
                        track_id,
                        old_clips,
                        new_clips,
                        label: "Overwrite at playhead (MCP)".to_string(),
                    };
                    let st = timeline_state.borrow_mut();
                    let project_rc = st.project.clone();
                    drop(st);
                    let mut proj = project_rc.borrow_mut();
                    timeline_state
                        .borrow_mut()
                        .history
                        .undo_stack
                        .push(Box::new(cmd));
                    timeline_state.borrow_mut().history.redo_stack.clear();
                    proj.dirty = true;
                    drop(proj);
                    reply
                        .send(json!({"success": true, "clip_id": clip_id}))
                        .ok();
                    on_project_changed();
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
                // Phase 1: trigger re-seek + async playing pulse (brief borrow).
                let (
                    start_scope_seq,
                    start_compositor_seq,
                    was_enabled,
                    left_playing,
                    scope_seq_arc,
                    compositor_seq_arc,
                ) = {
                    let pp = prog_player.borrow();
                    let (start_scope_seq, was_enabled, left_playing) = pp.prepare_export();
                    let scope_seq_arc = pp.scope_frame_seq_arc();
                    let compositor_seq_arc = pp.compositor_frame_seq_arc();
                    let start_compositor_seq =
                        compositor_seq_arc.load(std::sync::atomic::Ordering::Relaxed);
                    (
                        start_scope_seq,
                        start_compositor_seq,
                        was_enabled,
                        left_playing,
                        scope_seq_arc,
                        compositor_seq_arc,
                    )
                };
                let prog_player = prog_player.clone();
                if left_playing {
                    // Phase 2a: The pipeline is Playing.  Give the main loop
                    // 250ms to service gtk4paintablesink, then complete the
                    // pulse (Playing→Paused triggers compositor preroll).
                    // Phase 2b: After the pulse completes, poll for the new
                    // preroll frame from the appsink.
                    let pulse_delay = std::time::Duration::from_millis(250);
                    glib::timeout_add_local_once(pulse_delay, move || {
                        {
                            let pp = prog_player.borrow();
                            pp.complete_playing_pulse();
                        }
                        // Now poll for the new scope frame.
                        let deadline =
                            std::time::Instant::now() + std::time::Duration::from_millis(2000);
                        glib::timeout_add_local(std::time::Duration::from_millis(16), move || {
                            let scope_now =
                                scope_seq_arc.load(std::sync::atomic::Ordering::Relaxed);
                            let comp_now =
                                compositor_seq_arc.load(std::sync::atomic::Ordering::Relaxed);
                            if scope_now > start_scope_seq
                                || comp_now > start_compositor_seq
                                || std::time::Instant::now() >= deadline
                            {
                                let pp = prog_player.borrow();
                                match pp.finish_export(
                                    &path,
                                    was_enabled,
                                    start_scope_seq,
                                    start_compositor_seq,
                                ) {
                                    Ok(()) => reply
                                        .send(json!({"ok": true, "path": path, "format": "ppm"}))
                                        .ok(),
                                    Err(e) => reply
                                        .send(json!({"ok": false, "error": e.to_string()}))
                                        .ok(),
                                };
                                return glib::ControlFlow::Break;
                            }
                            glib::ControlFlow::Continue
                        });
                    });
                } else {
                    // ≤2 tracks: the pulse already completed synchronously.
                    // The frame should already be available.
                    let pp = prog_player.borrow();
                    match pp.finish_export(
                        &path,
                        was_enabled,
                        start_scope_seq,
                        start_compositor_seq,
                    ) {
                        Ok(()) => reply
                            .send(json!({"ok": true, "path": path, "format": "ppm"}))
                            .ok(),
                        Err(e) => reply
                            .send(json!({"ok": false, "error": e.to_string()}))
                            .ok(),
                    };
                }
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
    }
}
