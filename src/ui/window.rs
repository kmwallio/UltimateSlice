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
use crate::ui::{media_browser, preview, toolbar, inspector, program_monitor};
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

    let (player, paintable) = Player::new().expect("Failed to create GStreamer player");
    let player = Rc::new(RefCell::new(player));

    let (prog_player_raw, prog_paintable) = ProgramPlayer::new()
        .expect("Failed to create program player");
    let prog_player = Rc::new(RefCell::new(prog_player_raw));

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
            let _ = player.borrow().seek(ns);
            prog_player.borrow_mut().seek(ns);
        }));
    }
    {
        let player = player.clone();
        let prog_player = prog_player.clone();
        timeline_state.borrow_mut().on_play_pause = Some(Rc::new(move || {
            let p = player.borrow();
            match p.state() {
                crate::media::player::PlayerState::Playing => { let _ = p.pause(); }
                _ => { let _ = p.play(); }
            }
            drop(p);
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
    let (preview_widget, source_marks, clip_name_label) = preview::build_preview(player.clone(), paintable, on_append.clone());

    // Wire on_drop_clip — placed here so it can read source_marks to honour
    // the in/out selection set in the source monitor.
    {
        let project = project.clone();
        let on_project_changed = on_project_changed.clone();
        let source_marks = source_marks.clone();
        timeline_state.borrow_mut().on_drop_clip = Some(Rc::new(move |source_path, duration_ns, track_idx, timeline_start_ns| {
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
                proj.dirty = true;
                drop(proj);
                on_project_changed();
            }
        }));
    }

    // ── Build program monitor ──────────────────────────────────────────────
    // timeline_panel doesn't exist yet; use a shared cell filled in after build.
    let timeline_panel_cell: Rc<RefCell<Option<gtk4::Widget>>> = Rc::new(RefCell::new(None));

    let (prog_monitor_widget, pos_label) = program_monitor::build_program_monitor(
        prog_player.clone(),
        prog_paintable,
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
            let player = player.clone();
            move || {
                let p = player.borrow();
                match p.state() {
                    crate::media::player::PlayerState::Playing => { let _ = p.pause(); }
                    _ => { let _ = p.play(); }
                }
                drop(p);
                pp.borrow_mut().toggle_play_pause();
            }
        },
    );

    // 100 ms poll timer: advance playback, update timecode + timeline playhead
    {
        let pp = prog_player.clone();
        let ts = timeline_state.clone();
        let cell = timeline_panel_cell.clone();
        let last_pos_ns = Rc::new(Cell::new(u64::MAX));
        let last_pos_ns_c = last_pos_ns.clone();
        glib::timeout_add_local(std::time::Duration::from_millis(100), move || {
            let pos_ns = {
                let mut player = pp.borrow_mut();
                player.poll();
                player.timeline_pos_ns
            };
            if pos_ns != last_pos_ns_c.get() {
                pos_label.set_text(&program_monitor::format_timecode(pos_ns));
                ts.borrow_mut().playhead_ns = pos_ns;
                if let Some(ref w) = *cell.borrow() { w.queue_draw(); }
                last_pos_ns_c.set(pos_ns);
            }
            glib::ControlFlow::Continue
        });
    }

    top_paned.set_end_child(Some(&prog_monitor_widget));

    // ── on_append: reads source_marks, creates clip, adds to timeline ─────
    *on_append_impl.borrow_mut() = Some({
        let project = project.clone();
        let source_marks = source_marks.clone();
        let on_project_changed = on_project_changed.clone();
        Rc::new(move || {
            let marks = source_marks.borrow();
            if marks.path.is_empty() { return; }
            let path = marks.path.clone();
            let in_ns = marks.in_ns;
            let out_ns = marks.out_ns;
            drop(marks);

            {
                let mut proj = project.borrow_mut();
                if let Some(track) = proj.tracks.iter_mut().find(|t| t.kind == TrackKind::Video) {
                    let timeline_start = track.duration();
                    let mut clip = Clip::new(path, out_ns, timeline_start, ClipKind::Video);
                    clip.source_in = in_ns;
                    clip.source_out = out_ns;
                    track.add_clip(clip);
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
            let _ = player.borrow().load(&uri);
            let mut m = source_marks.borrow_mut();
            m.path = path;
            m.duration_ns = duration_ns;
            m.in_ns = 0;
            m.out_ns = duration_ns;
            m.display_pos_ns = 0;
        })
    };

    // ── Media browser ─────────────────────────────────────────────────────
    let browser = media_browser::build_media_browser(
        library.clone(),
        on_source_selected.clone(),
    );
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

    // Inspector on the right
    let inspector_scroll = ScrolledWindow::new();
    inspector_scroll.set_policy(gtk::PolicyType::Never, gtk::PolicyType::Automatic);
    inspector_scroll.set_child(Some(&inspector_box));
    root_hpaned.set_end_child(Some(&inspector_scroll));

    window.set_child(Some(&root_hpaned));

    // ── MCP server (optional, enabled via --mcp flag) ─────────────────────
    if mcp_enabled {
        let mcp_receiver = crate::mcp::start_mcp_server();
        let project = project.clone();
        let library = library.clone();
        let on_project_changed = on_project_changed.clone();
        // Poll the mpsc channel every 10 ms on the GTK main thread.
        glib::timeout_add_local(std::time::Duration::from_millis(10), move || {
            while let Ok(cmd) = mcp_receiver.try_recv() {
                handle_mcp_command(cmd, &project, &library, &on_project_changed);
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

    window.present();
}

// ── MCP command handler (runs on GTK main thread) ────────────────────────────

fn handle_mcp_command(
    cmd: crate::mcp::McpCommand,
    project: &Rc<RefCell<Project>>,
    library: &Rc<RefCell<Vec<MediaItem>>>,
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

        McpCommand::AddClip { source_path, track_index, timeline_start_ns, source_in_ns, source_out_ns, reply } => {
            let clip_id = {
                let mut proj = project.borrow_mut();
                if let Some(track) = proj.tracks.get_mut(track_index) {
                    let mut clip = Clip::new(source_path, source_out_ns, timeline_start_ns, ClipKind::Video);
                    clip.source_in  = source_in_ns;
                    clip.source_out = source_out_ns;
                    let id = clip.id.clone();
                    track.add_clip(clip);
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
            let mut proj = project.borrow_mut();
            let mut found = false;
            for track in proj.tracks.iter_mut() {
                if let Some(pos) = track.clips.iter().position(|c| c.id == clip_id) {
                    track.clips.remove(pos);
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
            let mut proj = project.borrow_mut();
            let mut found = false;
            'outer: for track in proj.tracks.iter_mut() {
                for clip in track.clips.iter_mut() {
                    if clip.id == clip_id {
                        clip.timeline_start = new_start_ns;
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

        McpCommand::TrimClip { clip_id, source_in_ns, source_out_ns, reply } => {
            let mut proj = project.borrow_mut();
            let mut found = false;
            'outer: for track in proj.tracks.iter_mut() {
                for clip in track.clips.iter_mut() {
                    if clip.id == clip_id {
                        clip.source_in  = source_in_ns;
                        clip.source_out = source_out_ns;
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
            let item = MediaItem::new(path.clone(), duration_ns);
            let label = item.label.clone();
            library.borrow_mut().push(item);
            reply.send(json!({"success": true, "label": label, "duration_ns": duration_ns})).ok();
            on_project_changed();
        }
    }
}
