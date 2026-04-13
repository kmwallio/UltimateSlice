use crate::ui::window::build_window;
use gio;
use gtk4::prelude::*;
use gtk4::Application;
use std::cell::RefCell;
use std::rc::Rc;

const APP_ID: &str = "io.github.kmwallio.ultimateslice";

pub fn run(mcp_enabled: bool, startup_project_path: Option<String>) {
    // When running as an MCP server we've already terminated any prior instance
    // via the PID file. Use NON_UNIQUE so GTK doesn't block on D-Bus registration
    // while the old instance's session is still being cleaned up.
    let mut flags = gio::ApplicationFlags::HANDLES_OPEN;
    if mcp_enabled {
        flags |= gio::ApplicationFlags::NON_UNIQUE;
    }

    let app = Application::builder()
        .application_id(APP_ID)
        .flags(flags)
        .build();

    app.connect_startup(|_| {
        load_css();
        // Sweep cached drawing artifacts older than the threshold
        // (30 days). Content-hashed cache files are stable across
        // sessions, so anything that hasn't been hit in a month is
        // orphaned — saves disk over time without functional impact.
        let (removed, freed) = crate::media::drawing_render::sweep_drawing_cache();
        if removed > 0 {
            log::info!(
                "drawing cache sweep: removed {removed} stale artifact(s), freed {:.1} MB",
                freed as f64 / (1024.0 * 1024.0)
            );
        }
    });

    let startup_project_path = Rc::new(RefCell::new(startup_project_path));
    let startup_project_path_open = startup_project_path.clone();
    app.connect_activate(move |app| {
        let path = startup_project_path.borrow_mut().take();
        build_window(app, mcp_enabled, path);
    });
    app.connect_open(move |app, files, _hint| {
        let path = files
            .first()
            .and_then(|f| f.path())
            .map(|p| p.to_string_lossy().into_owned())
            .or_else(|| startup_project_path_open.borrow_mut().take());
        build_window(app, mcp_enabled, path);
    });

    // Strip --mcp and --mcp-attach from argv before GLib sees it (unknown flags cause errors).
    let args: Vec<String> = std::env::args()
        .filter(|a| a != "--mcp" && a != "--mcp-attach")
        .collect();
    app.run_with_args(&args);
}

fn load_css() {
    // Prefer the dark variant of the system theme (Adwaita-dark on GNOME)
    if let Some(settings) = gtk4::Settings::default() {
        settings.set_property("gtk-application-prefer-dark-theme", true);
    }

    let css = gtk4::CssProvider::new();
    let css_data = include_str!("style.css");
    css.load_from_string(css_data);
    gtk4::style_context_add_provider_for_display(
        &gdk4::Display::default().expect("no display"),
        &css,
        gtk4::STYLE_PROVIDER_PRIORITY_APPLICATION,
    );
}
