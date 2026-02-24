use gtk4::prelude::*;
use gtk4::{self as gtk, ApplicationWindow, Orientation, Paned, ScrolledWindow};
use std::cell::RefCell;
use std::rc::Rc;
use crate::model::project::Project;
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

    let (player, paintable) = Player::new().expect("Failed to create GStreamer player");
    let player = Rc::new(RefCell::new(player));

    let timeline_state = Rc::new(RefCell::new(TimelineState::new(project.clone())));

    // Connect player seek callback from timeline
    {
        let player = player.clone();
        timeline_state.borrow_mut().on_seek = Some(Box::new(move |ns| {
            let _ = player.borrow().seek(ns);
        }));
    }

    // ── Build inspector ───────────────────────────────────────────────────
    let (inspector_box, inspector_view) = inspector::build_inspector(
        project.clone(),
        {
            // no-op: timeline redraws from its own update loop
            || {}
        },
    );

    // ── Build toolbar ─────────────────────────────────────────────────────
    let window_weak = window.downgrade();
    let _project_cb = project.clone();

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

    // Wire timeline's on_project_changed
    {
        let cb = on_project_changed.clone();
        timeline_state.borrow_mut().on_project_changed = Some(Box::new(move || cb()));
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

    // Left/center: vertical split (top=browser+preview, bottom=timeline)
    let root_vpaned = Paned::new(Orientation::Vertical);
    root_vpaned.set_vexpand(true);
    root_vpaned.set_hexpand(true);
    root_vpaned.set_position(520);

    // Top area: horizontal split — media browser | preview
    let top_paned = Paned::new(Orientation::Horizontal);
    top_paned.set_hexpand(true);
    top_paned.set_vexpand(true);
    top_paned.set_position(200);

    let browser = media_browser::build_media_browser(project.clone(), {
        let cb = on_project_changed.clone();
        move || cb()
    });
    top_paned.set_start_child(Some(&browser));

    let preview_widget = preview::build_preview(player.clone(), paintable);
    top_paned.set_end_child(Some(&preview_widget));

    root_vpaned.set_start_child(Some(&top_paned));

    // Timeline (bottom)
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

    // Update playhead from player position every 100ms
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
