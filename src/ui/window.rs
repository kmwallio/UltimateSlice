use gtk4::prelude::*;
use gtk4::{self as gtk, ApplicationWindow, Orientation, Paned, ScrolledWindow};
use glib;
use std::cell::{Cell, RefCell};
use std::collections::HashSet;
use std::rc::Rc;
use crate::model::project::Project;
use crate::model::clip::{Clip, ClipKind};
use crate::model::track::TrackKind;
use crate::model::media_library::MediaItem;
use crate::media::player::Player;
use crate::media::program_player::{ProgramPlayer, ProgramClip};
use crate::ui::{media_browser, preview, toolbar, inspector, program_monitor, preferences};
use crate::ui::timeline::{TimelineState, build_timeline_panel};

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

    let initial_hw_accel = preferences_state.borrow().hardware_acceleration_enabled;
    let initial_playback_priority = preferences_state.borrow().playback_priority.clone();
    let initial_proxy_mode = preferences_state.borrow().proxy_mode.clone();
    let (player, paintable) = Player::new(initial_hw_accel).expect("Failed to create GStreamer player");
    let player = Rc::new(RefCell::new(player));

    let (mut prog_player_raw, prog_paintable, prog_paintable2) = ProgramPlayer::new()
        .expect("Failed to create program player");
    prog_player_raw.set_playback_priority(initial_playback_priority);
    prog_player_raw.set_proxy_enabled(initial_proxy_mode.is_enabled());
    let prog_player = Rc::new(RefCell::new(prog_player_raw));

    let proxy_cache = Rc::new(RefCell::new(crate::media::proxy_cache::ProxyCache::new()));

    let timeline_state = Rc::new(RefCell::new(TimelineState::new(project.clone())));

    // ── Build toolbar ─────────────────────────────────────────────────────
    let window_weak = window.downgrade();

    // Two-phase setup: create a stable Rc handle now, fill in the real
    // implementation after the timeline panel is built (so we can capture
    // a weak reference to it for explicit queue_draw).
    let on_project_changed_impl: Rc<RefCell<Option<Box<dyn Fn()>>>> =
        Rc::new(RefCell::new(None));
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
            if let Some(f) = cb.borrow().as_ref() { f(); }
        })
    };
    *open_preferences_impl.borrow_mut() = Some({
        let window_weak = window_weak.clone();
        let preferences_state = preferences_state.clone();
        let player = player.clone();
        let prog_player = prog_player.clone();
        let proxy_cache = proxy_cache.clone();
        let project = project.clone();
        Rc::new(move || {
            if let Some(win) = window_weak.upgrade() {
                let current = preferences_state.borrow().clone();
                let old_proxy_mode = current.proxy_mode.clone();
                let preferences_state = preferences_state.clone();
                let player = player.clone();
                let prog_player = prog_player.clone();
                let proxy_cache = proxy_cache.clone();
                let project = project.clone();
                let on_save: Rc<dyn Fn(crate::ui_state::PreferencesState)> = Rc::new(move |new_state| {
                    *preferences_state.borrow_mut() = new_state.clone();
                    crate::ui_state::save_preferences_state(&new_state);
                    if let Err(e) = player.borrow().set_hardware_acceleration(new_state.hardware_acceleration_enabled) {
                        eprintln!("Failed to apply hardware acceleration setting: {e}");
                    }
                    prog_player.borrow_mut().set_playback_priority(new_state.playback_priority.clone());
                    prog_player.borrow_mut().set_proxy_enabled(new_state.proxy_mode.is_enabled());
                    if new_state.proxy_mode.is_enabled() {
                        // If the proxy scale changed, invalidate old entries so clips are
                        // re-transcoded at the new resolution.
                        if new_state.proxy_mode != old_proxy_mode {
                            proxy_cache.borrow_mut().invalidate_all();
                        }
                        let scale = match new_state.proxy_mode {
                            crate::ui_state::ProxyMode::QuarterRes => crate::media::proxy_cache::ProxyScale::Quarter,
                            _ => crate::media::proxy_cache::ProxyScale::Half,
                        };
                        let clips: Vec<(String, Option<String>)> = {
                            let proj = project.borrow();
                            proj.tracks.iter()
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
                });
                preferences::show_preferences_dialog(win.upcast_ref(), current, on_save);
            }
        })
    });

    // ── Build inspector (after on_project_changed is defined so we can pass it) ──
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
            move |b, c, s, d, sh| {
                prog_player.borrow_mut().update_current_effects(
                    b as f64, c as f64, s as f64, d as f64, sh as f64);
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
            move |vol, pan| {
                prog_player.borrow_mut().update_current_audio(vol as f64, pan as f64);
                if let Some(win) = window_weak.upgrade() {
                    let proj = project.borrow();
                    let title = format!("UltimateSlice — {} •", proj.title);
                    win.set_title(Some(&title));
                }
            }
        },
        // on_transform_changed: crop/rotate/flip → direct update, no pipeline reload
        {
            let player = player.clone();
            let prog_player = prog_player.clone();
            let window_weak = window_weak.clone();
            let project = project.clone();
            move |cl, cr, ct, cb, rot, fh, fv| {
                player.borrow().set_transform(cl, cr, ct, cb, rot, fh, fv);
                prog_player.borrow_mut().update_current_transform(cl, cr, ct, cb, rot, fh, fv);
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
                        if let Some(clip) = proj.tracks.iter()
                            .flat_map(|t| t.clips.iter())
                            .nth(idx)
                        {
                            (clip.title_font.clone(), clip.title_color)
                        } else {
                            ("Sans Bold 36".to_string(), 0xFFFFFFFF)
                        }
                    } else {
                        ("Sans Bold 36".to_string(), 0xFFFFFFFF)
                    }
                };
                prog_player.borrow_mut().update_current_title(&text, &font, color, x, y);
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
                        crate::ui_state::ProxyMode::QuarterRes => crate::media::proxy_cache::ProxyScale::Quarter,
                        _ => crate::media::proxy_cache::ProxyScale::Half,
                    };
                    let clips: Vec<(String, Option<String>)> = {
                        let proj = project.borrow();
                        proj.tracks.iter()
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
    );

    // Wire timeline's on_project_changed + on_seek + on_play_pause
    {
        let cb = on_project_changed.clone();
        timeline_state.borrow_mut().on_project_changed = Some(Rc::new(move || cb()));
    }
    {
        let player = player.clone();
        let prog_player = prog_player.clone();
        timeline_state.borrow_mut().on_seek = Some(Rc::new(move |ns| {
            prog_player.borrow_mut().seek(ns);
        }));
    }
    {
        let prog_player = prog_player.clone();
        timeline_state.borrow_mut().on_play_pause = Some(Rc::new(move || {
            prog_player.borrow_mut().toggle_play_pause();
        }));
    }
    let header = toolbar::build_toolbar(project.clone(), timeline_state.clone(), {
        let cb = on_project_changed.clone();
        move || cb()
    });
    window.set_titlebar(Some(&header));

    // ── Root layout: horizontal paned (content | inspector) ──────────────
    let root_hpaned = Paned::new(Orientation::Horizontal);
    root_hpaned.set_hexpand(true);
    root_hpaned.set_vexpand(true);
    root_hpaned.set_position(1200);

    let root_vpaned = Paned::new(Orientation::Vertical);
    root_vpaned.set_vexpand(true);
    root_vpaned.set_hexpand(true);
    root_vpaned.set_position(520);

    let top_paned = Paned::new(Orientation::Horizontal);
    top_paned.set_hexpand(true);
    top_paned.set_vexpand(true);
    top_paned.set_position(280);

    // ── Build preview first so we have source_marks ───────────────────────
    // on_append stub: real impl filled in below after source_marks is available.
    let on_append_impl: Rc<RefCell<Option<Rc<dyn Fn()>>>> = Rc::new(RefCell::new(None));
    let on_append: Rc<dyn Fn()> = {
        let cb = on_append_impl.clone();
        Rc::new(move || {
            if let Some(f) = cb.borrow().as_ref() { f(); }
        })
    };
    let on_close_preview_impl: Rc<RefCell<Option<Rc<dyn Fn()>>>> = Rc::new(RefCell::new(None));
    let on_close_preview: Rc<dyn Fn()> = {
        let cb = on_close_preview_impl.clone();
        Rc::new(move || {
            if let Some(f) = cb.borrow().as_ref() { f(); }
        })
    };
    let (preview_widget, source_marks, clip_name_label) = preview::build_preview(
        player.clone(),
        paintable,
        on_append.clone(),
        on_close_preview.clone(),
    );

    // Wire on_drop_clip — placed here so it can read source_marks to honour
    // the in/out selection set in the source monitor.
    {
        let project = project.clone();
        let on_project_changed = on_project_changed.clone();
        let source_marks = source_marks.clone();
        let timeline_state_for_drop = timeline_state.clone();
        timeline_state.borrow_mut().on_drop_clip = Some(Rc::new(move |source_path, duration_ns, track_idx, timeline_start_ns| {
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
        }));
    }

    // ── Build program monitor ──────────────────────────────────────────────
    // timeline_panel doesn't exist yet; use a shared cell filled in after build.
    let timeline_panel_cell: Rc<RefCell<Option<gtk4::Widget>>> = Rc::new(RefCell::new(None));
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
            if let Some(f) = cb.borrow().as_ref() { f(); }
        })
    };

    let (prog_monitor_widget, pos_label, picture_a, picture_b, vu_meter, vu_peak_cell) = program_monitor::build_program_monitor(
        prog_player.clone(),
        prog_paintable,
        prog_paintable2,
        // on_stop
        {
            let pp = prog_player.clone();
            let ts = timeline_state.clone();
            let cell = timeline_panel_cell.clone();
            move || {
                pp.borrow_mut().stop();
                ts.borrow_mut().playhead_ns = 0;
                if let Some(ref w) = *cell.borrow() { w.queue_draw(); }
            }
        },
        // on_play_pause
        {
            let pp = prog_player.clone();
            move || {
                pp.borrow_mut().toggle_play_pause();
            }
        },
        {
            let cb = on_toggle_popout.clone();
            move || cb()
        },
    );

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
        glib::timeout_add_local(std::time::Duration::from_millis(33), move || {
            let (pos_ns, playing, opacity_a, opacity_b, peaks) = {
                let mut player = pp.borrow_mut();
                player.poll();
                let (oa, ob) = player.transition_opacities();
                (player.timeline_pos_ns, player.is_playing(), oa, ob, player.audio_peak_db)
            };
            // Apply cross-dissolve opacities to the two program monitor pictures.
            picture_a.set_opacity(opacity_a);
            picture_b.set_opacity(opacity_b);
            // Update VU meter with current audio peak levels.
            vu_pc.set(peaks);
            vu.queue_draw();
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
                    if let Some(ref w) = *cell.borrow() { w.queue_draw(); }
                    last_draw_ns_c.set(pos_ns);
                }
                last_pos_ns_c.set(pos_ns);
            }
            glib::ControlFlow::Continue
        });
    }

    prog_monitor_host.append(&prog_monitor_widget);
    top_paned.set_end_child(Some(&prog_monitor_host));

    // Program monitor pop-out/dock toggle
    *on_toggle_popout_impl.borrow_mut() = Some({
        let app = app.clone();
        let host = prog_monitor_host.clone();
        let monitor = prog_monitor_widget.clone();
        let pop_cell = popout_window_cell.clone();
        let popped = monitor_popped.clone();
        let monitor_state = monitor_state.clone();
        Rc::new(move || {
            if !popped.get() {
                let state = monitor_state.borrow().clone();
                let pop_win = ApplicationWindow::builder()
                    .application(&app)
                    .title("UltimateSlice — Program Monitor")
                    .default_width(state.width.max(320))
                    .default_height(state.height.max(180))
                    .build();

                host.remove(&monitor);
                pop_win.set_child(Some(&monitor));

                let host_c = host.clone();
                let monitor_c = monitor.clone();
                let pop_cell_c = pop_cell.clone();
                let popped_c = popped.clone();
                let monitor_state_c = monitor_state.clone();
                pop_win.connect_close_request(move |w| {
                    let mut state = monitor_state_c.borrow_mut();
                    state.width = w.width().max(320);
                    state.height = w.height().max(180);
                    state.popped = false;
                    crate::ui_state::save_program_monitor_state(&state);
                    w.set_child(Option::<&gtk::Widget>::None);
                    if monitor_c.parent().is_none() {
                        host_c.append(&monitor_c);
                    }
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
            if marks.path.is_empty() { return; }
            let path = marks.path.clone();
            let in_ns = marks.in_ns;
            let out_ns = marks.out_ns;
            let is_audio = marks.is_audio_only;
            drop(marks);

            let ts = timeline_state.borrow();
            let magnetic_mode = ts.magnetic_mode;
            let active_tid = ts.selected_track_id.clone();
            drop(ts);

            let target_kind = if is_audio { TrackKind::Audio } else { TrackKind::Video };
            let clip_kind = if is_audio { ClipKind::Audio } else { ClipKind::Video };

            {
                let mut proj = project.borrow_mut();
                // Prefer the active track if its kind matches, else first matching track
                let track = if let Some(ref tid) = active_tid {
                    if proj.tracks.iter().any(|t| &t.id == tid && t.kind == target_kind) {
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
            // Look up is_audio_only from library item (set by background probe).
            let audio_only = library.borrow().iter()
                .find(|i| i.source_path == path)
                .map(|i| i.is_audio_only)
                .unwrap_or(false);
            let _ = player.borrow().load(&uri);
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
    let (browser, clear_media_selection) = media_browser::build_media_browser(
        library.clone(),
        on_source_selected.clone(),
    );
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

    let (timeline_panel, timeline_area) = build_timeline_panel(timeline_state.clone(), on_project_changed.clone());
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

        *on_project_changed_impl.borrow_mut() = Some(Box::new(move || {
            // Update window title
            if let Some(win) = window_weak.upgrade() {
                let proj = project.borrow();
                let dirty_marker = if proj.dirty { " •" } else { "" };
                win.set_title(Some(&format!("UltimateSlice — {}{dirty_marker}", proj.title)));
            }

            // Update inspector and collect program clips — drop proj borrow before GStreamer call
            let (clips, media_from_project): (Vec<ProgramClip>, Vec<(String, u64)>) = {
                let proj = project.borrow();
                let selected = timeline_state.borrow().selected_clip_id.clone();
                inspector_view.update(&proj, selected.as_deref());

                let clips = proj.tracks.iter().enumerate().flat_map(|(t_idx, t)| {
                    let audio_only = t.kind == TrackKind::Audio;
                    t.clips.iter().map(move |c| ProgramClip {
                        source_path:       c.source_path.clone(),
                        source_in_ns:      c.source_in,
                        source_out_ns:     c.source_out,
                        timeline_start_ns: c.timeline_start,
                        brightness:        c.brightness as f64,
                        contrast:          c.contrast as f64,
                        saturation:        c.saturation as f64,
                        denoise:           c.denoise as f64,
                        sharpness:         c.sharpness as f64,
                        volume:            c.volume as f64,
                        pan:               c.pan as f64,
                        crop_left:         c.crop_left,
                        crop_right:        c.crop_right,
                        crop_top:          c.crop_top,
                        crop_bottom:       c.crop_bottom,
                        rotate:            c.rotate,
                        flip_h:            c.flip_h,
                        flip_v:            c.flip_v,
                        title_text:        c.title_text.clone(),
                        title_font:        c.title_font.clone(),
                        title_color:       c.title_color,
                        title_x:           c.title_x,
                        title_y:           c.title_y,
                        speed:             c.speed,
                        is_audio_only:     audio_only,
                        track_index:       t_idx,
                        transition_after:  c.transition_after.clone(),
                        transition_after_ns:c.transition_after_ns,
                        lut_path:          c.lut_path.clone(),
                    })
                }).collect();
                // Keep media browser in sync with timeline clip sources after project open/load.
                let media = proj.tracks.iter().flat_map(|t| t.clips.iter())
                    .map(|c| (c.source_path.clone(), c.source_out))
                    .collect();
                (clips, media)
            }; // proj borrow dropped here — safe to call GStreamer below

            {
                let mut lib = library.borrow_mut();
                let mut seen: HashSet<String> = lib.iter().map(|i| i.source_path.clone()).collect();
                for (path, dur) in media_from_project {
                    if seen.insert(path.clone()) {
                        lib.push(MediaItem::new(path, dur));
                    }
                }
            }

            // Reload program player — preserve current position so the monitor
            // doesn't jump to 0 on every project change (e.g., clip name edit).
            let prev_pos = prog_player.borrow().timeline_pos_ns;
            let was_playing = matches!(
                prog_player.borrow().state(),
                crate::media::player::PlayerState::Playing
            );
            prog_player.borrow_mut().load_clips(clips);
            // Seek back to the previous position (loads the clip at that point
            // and applies the current color correction values).
            if !prog_player.borrow().clips.is_empty() {
                prog_player.borrow_mut().seek(prev_pos);
                if was_playing {
                    prog_player.borrow_mut().play();
                }
            }

            // Request proxy generation for all clips if proxy mode is enabled.
            // Each clip's lut_path is passed so LUT-assigned clips get their own baked proxy.
            {
                let prefs = preferences_state.borrow();
                if prefs.proxy_mode.is_enabled() {
                    let scale = match prefs.proxy_mode {
                        crate::ui_state::ProxyMode::QuarterRes => crate::media::proxy_cache::ProxyScale::Quarter,
                        _ => crate::media::proxy_cache::ProxyScale::Half,
                    };
                    let clip_sources: Vec<(String, Option<String>)> = {
                        let proj = project.borrow();
                        proj.tracks.iter()
                            .flat_map(|t| t.clips.iter())
                            .map(|c| (c.source_path.clone(), c.lut_path.clone()))
                            .collect()
                    };
                    {
                        let mut cache = proxy_cache.borrow_mut();
                        for (path, lut) in &clip_sources {
                            cache.request(path, scale, lut.as_deref());
                        }
                    }
                    // Disk-cached proxies are added to self.proxies synchronously by
                    // request() above. Push them to the player immediately so the seek
                    // that follows can use them rather than falling back to source files.
                    let paths = proxy_cache.borrow().proxies.clone();
                    prog_player.borrow_mut().update_proxy_paths(paths);
                }
            }

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

    let transition_row = gtk::ListBoxRow::new();
    let transition_box = gtk::Box::new(Orientation::Horizontal, 6);
    transition_box.set_margin_start(8);
    transition_box.set_margin_end(8);
    transition_box.set_margin_top(6);
    transition_box.set_margin_bottom(6);
    let transition_name = gtk::Label::new(Some("Cross-dissolve"));
    transition_name.set_halign(gtk::Align::Start);
    transition_name.set_hexpand(true);
    let transition_hint = gtk::Label::new(Some("Drag to clip boundary"));
    transition_hint.add_css_class("dim-label");
    transition_box.append(&transition_name);
    transition_box.append(&transition_hint);
    transition_row.set_child(Some(&transition_box));
    let drag_src = gtk::DragSource::new();
    drag_src.set_actions(gdk4::DragAction::COPY);
    drag_src.set_exclusive(false);
    let payload = String::from("transition:cross_dissolve");
    let val = glib::Value::from(&payload);
    drag_src.set_content(Some(&gdk4::ContentProvider::for_value(&val)));
    transition_row.add_controller(drag_src);
    transitions_list.append(&transition_row);

    transitions_revealer.set_child(Some(&transitions_list));
    right_sidebar.append(&transitions_revealer);

    {
        let revealer = transitions_revealer.clone();
        transitions_toggle.connect_clicked(move |btn| {
            let show = !revealer.reveals_child();
            revealer.set_reveal_child(show);
            btn.set_label(if show { "Hide Transitions" } else { "Show Transitions" });
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
        let preferences_state = preferences_state.clone();
        let status_bar = status_bar.clone();
        let status_label = status_label.clone();
        let status_progress = status_progress.clone();
        glib::timeout_add_local(std::time::Duration::from_millis(500), move || {
            let resolved = proxy_cache.borrow_mut().poll();
            // Always sync proxy paths when proxy mode is enabled — disk-cached proxies
            // are added synchronously by request() and never appear in `resolved`.
            {
                let prefs = preferences_state.borrow();
                if prefs.proxy_mode.is_enabled() {
                    if !resolved.is_empty() || !proxy_cache.borrow().proxies.is_empty() {
                        let paths = proxy_cache.borrow().proxies.clone();
                        prog_player.borrow_mut().update_proxy_paths(paths);
                    }
                }
            }
            let progress = proxy_cache.borrow().progress();
            if progress.in_flight {
                status_bar.set_visible(true);
                status_label.set_text(&format!("Generating proxies… {}/{}", progress.completed, progress.total));
                if progress.total > 0 {
                    status_progress.set_fraction(progress.completed as f64 / progress.total as f64);
                }
            } else {
                status_bar.set_visible(false);
            }
            glib::ControlFlow::Continue
        });
    }

    // ── MCP server (optional, enabled via --mcp flag) ─────────────────────
    if mcp_enabled {
        let mcp_receiver = crate::mcp::start_mcp_server();
        let project = project.clone();
        let library = library.clone();
        let player = player.clone();
        let prog_player = prog_player.clone();
        let timeline_state = timeline_state.clone();
        let preferences_state = preferences_state.clone();
        let proxy_cache = proxy_cache.clone();
        let on_close_preview = on_close_preview.clone();
        let on_project_changed = on_project_changed.clone();
        // Poll the mpsc channel every 10 ms on the GTK main thread.
        glib::timeout_add_local(std::time::Duration::from_millis(10), move || {
            while let Ok(cmd) = mcp_receiver.try_recv() {
                handle_mcp_command(
                    cmd,
                    &project,
                    &library,
                    &player,
                    &prog_player,
                    &timeline_state,
                    &preferences_state,
                    &proxy_cache,
                    &on_close_preview,
                    &on_project_changed,
                );
            }
            glib::ControlFlow::Continue
        });
        eprintln!("[MCP] Server listening on stdio (JSON-RPC 2.0 / MCP 2024-11-05)");
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
                            glib::timeout_add_local_once(std::time::Duration::from_secs(3), move || {
                                if let Some(w) = win_w2.upgrade() {
                                    w.set_title(Some(&format!("UltimateSlice — {} •", proj_title)));
                                }
                            });
                        }
                    }
                }
            }
            glib::ControlFlow::Continue
        });
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
    project: &Rc<RefCell<Project>>,
    library: &Rc<RefCell<Vec<MediaItem>>>,
    player: &Rc<RefCell<Player>>,
    prog_player: &Rc<RefCell<ProgramPlayer>>,
    timeline_state: &Rc<RefCell<TimelineState>>,
    preferences_state: &Rc<RefCell<crate::ui_state::PreferencesState>>,
    proxy_cache: &Rc<RefCell<crate::media::proxy_cache::ProxyCache>>,
    on_close_preview: &Rc<dyn Fn()>,
    on_project_changed: &Rc<dyn Fn()>,
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
            let tracks: Vec<_> = proj.tracks.iter().enumerate().map(|(i, t)| json!({
                "index":      i,
                "id":         t.id,
                "label":      t.label,
                "kind":       format!("{:?}", t.kind),
                "clip_count": t.clips.len(),
                "muted":      t.muted,
                "locked":     t.locked,
            })).collect();
            reply.send(json!(tracks)).ok();
        }

        McpCommand::ListClips { reply } => {
            let proj = project.borrow();
            let clips: Vec<_> = proj.tracks.iter().enumerate()
                .flat_map(|(ti, track)| track.clips.iter().map(move |c| json!({
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
                })))
                .collect();
            reply.send(json!(clips)).ok();
        }

        McpCommand::GetTimelineSettings { reply } => {
            let magnetic_mode = timeline_state.borrow().magnetic_mode;
            reply.send(json!({
                "magnetic_mode": magnetic_mode
            })).ok();
        }

        McpCommand::SetMagneticMode { enabled, reply } => {
            timeline_state.borrow_mut().magnetic_mode = enabled;
            reply.send(json!({"success": true, "magnetic_mode": enabled})).ok();
            on_project_changed();
        }

        McpCommand::CloseSourcePreview { reply } => {
            on_close_preview();
            reply.send(json!({"success": true})).ok();
        }

        McpCommand::GetPreferences { reply } => {
            let prefs = preferences_state.borrow().clone();
            reply.send(json!({
                "hardware_acceleration_enabled": prefs.hardware_acceleration_enabled,
                "playback_priority": prefs.playback_priority.as_str(),
                "proxy_mode": prefs.proxy_mode.as_str()
            })).ok();
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
                    reply.send(json!({
                        "success": true,
                        "hardware_acceleration_enabled": enabled,
                        "playback_priority": new_state.playback_priority.as_str()
                    })).ok();
                }
                Err(e) => {
                    reply.send(json!({
                        "success": false,
                        "hardware_acceleration_enabled": enabled,
                        "playback_priority": new_state.playback_priority.as_str(),
                        "error": e.to_string()
                    })).ok();
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
            reply.send(json!({
                "success": true,
                "playback_priority": new_state.playback_priority.as_str()
            })).ok();
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
                    crate::ui_state::ProxyMode::QuarterRes => crate::media::proxy_cache::ProxyScale::Quarter,
                    _ => crate::media::proxy_cache::ProxyScale::Half,
                };
                let clip_sources: Vec<(String, Option<String>)> = {
                    let proj = project.borrow();
                    proj.tracks.iter()
                        .flat_map(|t| t.clips.iter())
                        .map(|c| (c.source_path.clone(), c.lut_path.clone()))
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
            reply.send(json!({
                "success": true,
                "proxy_mode": new_state.proxy_mode.as_str()
            })).ok();
        }

        McpCommand::AddClip { source_path, track_index, timeline_start_ns, source_in_ns, source_out_ns, reply } => {
            let magnetic_mode = timeline_state.borrow().magnetic_mode;
            let clip_id = {
                let mut proj = project.borrow_mut();
                if let Some(track) = proj.tracks.get_mut(track_index) {
                    let mut clip = Clip::new(source_path, source_out_ns, timeline_start_ns, ClipKind::Video);
                    clip.source_in  = source_in_ns;
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
                Err(e) => { reply.send(json!({"success": false, "error": e})).ok(); }
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
            if found { on_project_changed(); }
        }

        McpCommand::MoveClip { clip_id, new_start_ns, reply } => {
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
            if found { on_project_changed(); }
        }

        McpCommand::TrimClip { clip_id, source_in_ns, source_out_ns, reply } => {
            let magnetic_mode = timeline_state.borrow().magnetic_mode;
            let mut proj = project.borrow_mut();
            let mut found = false;
            for track in proj.tracks.iter_mut() {
                if let Some(idx) = track.clips.iter().position(|c| c.id == clip_id) {
                    track.clips[idx].source_in  = source_in_ns;
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
            if found { on_project_changed(); }
        }

        McpCommand::SetClipColor { clip_id, brightness, contrast, saturation, denoise, sharpness, reply } => {
            let mut proj = project.borrow_mut();
            let mut found = false;
            'outer: for track in proj.tracks.iter_mut() {
                for clip in track.clips.iter_mut() {
                    if clip.id == clip_id {
                        clip.brightness = brightness as f32;
                        clip.contrast   = contrast as f32;
                        clip.saturation = saturation as f32;
                        clip.denoise    = denoise as f32;
                        clip.sharpness  = sharpness as f32;
                        proj.dirty = true;
                        found = true;
                        break 'outer;
                    }
                }
            }
            drop(proj);
            reply.send(json!({"success": found})).ok();
            if found { on_project_changed(); }
        }

        McpCommand::SetClipLut { clip_id, lut_path, reply } => {
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
            if found { on_project_changed(); }
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
                Ok(_)  => reply.send(json!({"success": true, "path": path})).ok(),
                Err(e) => reply.send(json!({"success": false, "error": e.to_string()})).ok(),
            };
        }

        McpCommand::OpenFcpxml { path, reply } => {
            match std::fs::read_to_string(&path)
                .map_err(|e| e.to_string())
                .and_then(|xml| crate::fcpxml::parser::parse_fcpxml(&xml).map_err(|e| e.to_string()))
            {
                Ok(mut new_proj) => {
                    new_proj.file_path = Some(path.clone());
                    let track_count = new_proj.tracks.len();
                    let clip_count: usize = new_proj.tracks.iter().map(|t| t.clips.len()).sum();
                    *project.borrow_mut() = new_proj;
                    on_project_changed();
                    reply.send(json!({"success": true, "path": path, "tracks": track_count, "clips": clip_count})).ok();
                }
                Err(e) => { reply.send(json!({"success": false, "error": e})).ok(); }
            };
        }

        McpCommand::ExportMp4 { path, reply } => {
            let proj = project.borrow().clone();
            std::thread::spawn(move || {
                let (done_tx, done_rx) = std::sync::mpsc::sync_channel::<Result<(), String>>(1);
                let proj_worker = proj.clone();
                let path_worker = path.clone();
                std::thread::spawn(move || {
                    let (tx, _rx) = std::sync::mpsc::channel();
                    let result = crate::media::export::export_project(&proj_worker, &path_worker, crate::media::export::ExportOptions::default(), tx)
                        .map_err(|e| e.to_string())
                        .map(|_| ());
                    let _ = done_tx.send(result);
                });

                match done_rx.recv_timeout(std::time::Duration::from_secs(660)) {
                    Ok(Ok(())) => { let _ = reply.send(json!({"success": true, "path": path})); }
                    Ok(Err(e)) => { let _ = reply.send(json!({"success": false, "error": e})); }
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
            let items: Vec<_> = lib.iter().map(|item| json!({
                "label":       item.label,
                "source_path": item.source_path,
                "duration_ns": item.duration_ns,
                "duration_s":  item.duration_ns as f64 / 1_000_000_000.0,
            })).collect();
            reply.send(json!(items)).ok();
        }

        McpCommand::ImportMedia { path, reply } => {
            let uri = format!("file://{path}");
            let duration_ns = crate::ui::media_browser::probe_duration(&uri)
                .unwrap_or(10 * 1_000_000_000);
            let audio_only = crate::ui::media_browser::probe_is_audio_only(&uri);
            let mut item = MediaItem::new(path.clone(), duration_ns);
            item.is_audio_only = audio_only;
            let label = item.label.clone();
            library.borrow_mut().push(item);
            reply.send(json!({"success": true, "label": label, "duration_ns": duration_ns})).ok();
            on_project_changed();
        }

        McpCommand::ReorderTrack { from_index, to_index, reply } => {
            let track_count = {
                let proj = project.borrow();
                proj.tracks.len()
            };
            if from_index >= track_count || to_index >= track_count {
                reply.send(json!({"error": "Index out of range", "track_count": track_count})).ok();
            } else if from_index == to_index {
                reply.send(json!({"success": true, "message": "No change needed"})).ok();
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
                    timeline_state.borrow_mut().history.execute(Box::new(cmd), &mut proj);
                }
                reply.send(json!({"success": true, "from_index": from_index, "to_index": to_index})).ok();
                on_project_changed();
            }
        }
        McpCommand::SetTransition { track_index, clip_index, kind, duration_ns, reply } => {
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
                Some((track.id.clone(), clip.id.clone(), clip.transition_after.clone(), clip.transition_after_ns, clip.duration()))
            };
            let Some((track_id, clip_id, old_kind, old_duration_ns, clip_dur_ns)) = candidate else { return; };
            let new_kind = kind.trim().to_string();
            if !new_kind.is_empty() && new_kind != "cross_dissolve" {
                reply.send(json!({"error":"Unsupported transition kind","supported":["cross_dissolve"]})).ok();
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
                timeline_state.borrow_mut().history.execute(Box::new(cmd), &mut proj);
            }
            reply.send(json!({
                "success": true,
                "track_index": track_index,
                "clip_index": clip_index,
                "kind": new_kind,
                "duration_ns": new_duration_ns
            })).ok();
            on_project_changed();
        }
    }
}
