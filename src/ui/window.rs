use gtk4::prelude::*;
use gtk4::{self as gtk, ApplicationWindow, Orientation, Paned, ScrolledWindow};
use std::cell::RefCell;
use std::rc::Rc;
use crate::model::project::Project;
use crate::model::clip::{Clip, ClipKind};
use crate::model::track::TrackKind;
use crate::model::media_library::MediaItem;
use crate::media::player::Player;
use crate::ui::{media_browser, preview, toolbar, inspector};
use crate::ui::timeline::{TimelineState, build_timeline};

/// Build and show the main application window.
pub fn build_window(app: &gtk::Application) {
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

    let timeline_state = Rc::new(RefCell::new(TimelineState::new(project.clone())));

    // ── Build inspector ───────────────────────────────────────────────────
    let (inspector_box, inspector_view) = inspector::build_inspector(
        project.clone(),
        || {},
    );

    // ── Build toolbar ─────────────────────────────────────────────────────
    let window_weak = window.downgrade();

    let on_project_changed: Rc<dyn Fn()> = {
        let inspector_view = inspector_view.clone();
        let project = project.clone();
        let timeline_state = timeline_state.clone();
        let window_weak = window_weak.clone();

        Rc::new(move || {
            if let Some(win) = window_weak.upgrade() {
                let proj = project.borrow();
                let dirty_marker = if proj.dirty { " •" } else { "" };
                win.set_title(Some(&format!("UltimateSlice — {}{dirty_marker}", proj.title)));
            }
            let proj = project.borrow();
            let selected = timeline_state.borrow().selected_clip_id.clone();
            inspector_view.update(&proj, selected.as_deref());
        })
    };

    // Wire timeline's on_project_changed + on_seek + on_play_pause
    {
        let cb = on_project_changed.clone();
        timeline_state.borrow_mut().on_project_changed = Some(Rc::new(move || cb()));
    }
    {
        let player = player.clone();
        timeline_state.borrow_mut().on_seek = Some(Rc::new(move |ns| {
            let _ = player.borrow().seek(ns);
        }));
    }
    {
        let player = player.clone();
        timeline_state.borrow_mut().on_play_pause = Some(Rc::new(move || {
            let p = player.borrow();
            match p.state() {
                crate::media::player::PlayerState::Playing => { let _ = p.pause(); }
                _ => { let _ = p.play(); }
            }
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
    let (preview_widget, source_marks) = preview::build_preview(player.clone(), paintable);
    top_paned.set_end_child(Some(&preview_widget));

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
            let uri = format!("file://{path}");
            let _ = player.borrow().load(&uri);
            let mut m = source_marks.borrow_mut();
            m.path = path;
            m.duration_ns = duration_ns;
            m.in_ns = 0;
            m.out_ns = duration_ns;
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

    let timeline_widget = build_timeline(timeline_state.clone());
    timeline_scroll.set_child(Some(&timeline_widget));
    root_vpaned.set_end_child(Some(&timeline_scroll));

    root_hpaned.set_start_child(Some(&root_vpaned));

    // Inspector on the right
    let inspector_scroll = ScrolledWindow::new();
    inspector_scroll.set_policy(gtk::PolicyType::Never, gtk::PolicyType::Automatic);
    inspector_scroll.set_child(Some(&inspector_box));
    root_hpaned.set_end_child(Some(&inspector_scroll));

    window.set_child(Some(&root_hpaned));

    // Update timeline playhead from player position every 100ms
    {
        let player = player.clone();
        let timeline_state = timeline_state.clone();
        let tl_widget = timeline_widget.clone();
        glib::timeout_add_local(std::time::Duration::from_millis(100), move || {
            let pos = player.borrow().position();
            timeline_state.borrow_mut().playhead_ns = pos;
            tl_widget.queue_draw();
            glib::ControlFlow::Continue
        });
    }

    window.present();
}
