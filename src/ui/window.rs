use gtk4::prelude::*;
use gtk4::{self as gtk, ApplicationWindow, Orientation, Paned, ScrolledWindow};
use std::cell::RefCell;
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

    // ── Build inspector ───────────────────────────────────────────────────
    let (inspector_box, inspector_view) = inspector::build_inspector(
        project.clone(),
        || {},
    );

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
    top_paned.set_position(220);

    // ── Build preview first so we have source_marks ───────────────────────
    let (preview_widget, source_marks, clip_name_label) = preview::build_preview(player.clone(), paintable);

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
    let prog_monitor_widget = program_monitor::build_program_monitor(
        prog_player.clone(),
        prog_paintable,
    );

    // Source + Program monitors side-by-side
    let monitors_paned = Paned::new(Orientation::Horizontal);
    monitors_paned.set_hexpand(true);
    monitors_paned.set_vexpand(true);
    monitors_paned.set_position(640);
    monitors_paned.set_start_child(Some(&preview_widget));
    monitors_paned.set_end_child(Some(&prog_monitor_widget));

    top_paned.set_end_child(Some(&monitors_paned));

    // ── on_append: reads source_marks, creates clip, adds to timeline ─────
    let on_append: Rc<dyn Fn()> = {
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
    };

    // ── on_source_selected: loads clip into player + resets source_marks ──
    let on_source_selected: Rc<dyn Fn(String, u64)> = {
        let player = player.clone();
        let source_marks = source_marks.clone();
        Rc::new(move |path: String, duration_ns: u64| {
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
        on_append.clone(),
    );
    top_paned.set_start_child(Some(&browser));

    root_vpaned.set_start_child(Some(&top_paned));

    // ── Timeline ──────────────────────────────────────────────────────────
    let timeline_scroll = ScrolledWindow::new();
    timeline_scroll.set_policy(gtk::PolicyType::Automatic, gtk::PolicyType::Automatic);
    timeline_scroll.set_vexpand(true);
    timeline_scroll.set_hexpand(true);

    let timeline_panel = build_timeline_panel(timeline_state.clone(), on_project_changed.clone());
    timeline_scroll.set_child(Some(&timeline_panel));
    root_vpaned.set_end_child(Some(&timeline_scroll));

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
        let panel_weak = timeline_panel.downgrade();

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
                let clips = proj.tracks.iter().flat_map(|t| {
                    t.clips.iter().map(|c| ProgramClip {
                        source_path:       c.source_path.clone(),
                        source_in_ns:      c.source_in,
                        source_out_ns:     c.source_out,
                        timeline_start_ns: c.timeline_start,
                        brightness:        c.brightness as f64,
                        contrast:          c.contrast as f64,
                        saturation:        c.saturation as f64,
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

            // Reload program player (GStreamer state change; must not hold proj borrow)
            prog_player.borrow_mut().load_clips(clips);

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

    // Update timeline playhead from program timeline position every 100ms.
    // (Using source player position can snap playhead back to 0 when source monitor is idle.)
    {
        let prog_player = prog_player.clone();
        let timeline_state = timeline_state.clone();
        let panel_weak = timeline_panel.downgrade();
        glib::timeout_add_local(std::time::Duration::from_millis(100), move || {
            let pos = prog_player.borrow().timeline_pos_ns;
            timeline_state.borrow_mut().playhead_ns = pos;
            if let Some(p) = panel_weak.upgrade() { p.queue_draw(); }
            glib::ControlFlow::Continue
        });
    }

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

        McpCommand::SetClipColor { clip_id, brightness, contrast, saturation, reply } => {
            let mut proj = project.borrow_mut();
            let mut found = false;
            'outer: for track in proj.tracks.iter_mut() {
                for clip in track.clips.iter_mut() {
                    if clip.id == clip_id {
                        clip.brightness = brightness as f32;
                        clip.contrast   = contrast as f32;
                        clip.saturation = saturation as f32;
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

        McpCommand::ExportMp4 { path, reply } => {
            let proj = project.borrow().clone();
            std::thread::spawn(move || {
                let (done_tx, done_rx) = std::sync::mpsc::sync_channel::<Result<(), String>>(1);
                let proj_worker = proj.clone();
                let path_worker = path.clone();
                std::thread::spawn(move || {
                    let (tx, _rx) = std::sync::mpsc::channel();
                    let result = crate::media::export::export_project(&proj_worker, &path_worker, tx)
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
