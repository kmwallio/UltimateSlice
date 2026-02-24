use gtk4::prelude::*;
use gtk4::{self as gtk, ApplicationWindow, Box as GBox, Orientation, Paned, ScrolledWindow};
use std::cell::RefCell;
use std::rc::Rc;
use crate::model::project::Project;
use crate::media::player::Player;
use crate::ui::{media_browser, preview, toolbar};
use crate::ui::timeline::{TimelineState, build_timeline};

/// Build and show the main application window.
pub fn build_window(app: &gtk::Application) {
    let window = ApplicationWindow::builder()
        .application(app)
        .title("UltimateSlice")
        .default_width(1440)
        .default_height(900)
        .build();

    // Shared project state
    let project = Rc::new(RefCell::new(Project::new("Untitled")));

    // Create GStreamer player
    let (player, paintable) = Player::new().expect("Failed to create GStreamer player");
    let player = Rc::new(RefCell::new(player));

    // Timeline state
    let timeline_state = Rc::new(RefCell::new(TimelineState::new(project.clone())));

    // Connect player seek callback from timeline
    {
        let player = player.clone();
        timeline_state.borrow_mut().on_seek = Some(Box::new(move |ns| {
            let _ = player.borrow().seek(ns);
        }));
    }

    // Callback: redraw timeline and update title after any project change
    let _timeline_state_cb = timeline_state.clone();
    let window_weak = window.downgrade();
    let project_cb = project.clone();
    let on_project_changed: Rc<dyn Fn()> = Rc::new(move || {
        if let Some(win) = window_weak.upgrade() {
            let title = format!("UltimateSlice — {}", project_cb.borrow().title);
            win.set_title(Some(&title));
        }
        // Timeline redraws itself via queue_draw — timeline widget holds a ref
    });

    // Build toolbar
    let header = toolbar::build_toolbar(
        project.clone(),
        {
            let on_project_changed = on_project_changed.clone();
            move || on_project_changed()
        },
    );
    window.set_titlebar(Some(&header));

    // Root vertical split: top = browser+preview, bottom = timeline
    let root_paned = Paned::new(Orientation::Vertical);
    root_paned.set_vexpand(true);
    root_paned.set_hexpand(true);
    root_paned.set_position(520);

    // Top area: horizontal split — media browser | preview
    let top_paned = Paned::new(Orientation::Horizontal);
    top_paned.set_hexpand(true);
    top_paned.set_vexpand(true);
    top_paned.set_position(200);

    // Media browser
    let on_project_changed_clone = on_project_changed.clone();
    let browser = media_browser::build_media_browser(project.clone(), move || {
        on_project_changed_clone();
    });
    top_paned.set_start_child(Some(&browser));

    // Preview
    let preview_widget = preview::build_preview(player.clone(), paintable);
    top_paned.set_end_child(Some(&preview_widget));

    root_paned.set_start_child(Some(&top_paned));

    // Timeline (bottom)
    let timeline_scroll = ScrolledWindow::new();
    timeline_scroll.set_policy(gtk::PolicyType::Automatic, gtk::PolicyType::Automatic);
    timeline_scroll.set_vexpand(true);
    timeline_scroll.set_hexpand(true);

    let timeline_widget = build_timeline(timeline_state.clone());
    timeline_scroll.set_child(Some(&timeline_widget));
    root_paned.set_end_child(Some(&timeline_scroll));

    window.set_child(Some(&root_paned));

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
