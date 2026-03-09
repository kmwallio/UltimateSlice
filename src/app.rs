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
    let flags = if mcp_enabled {
        gio::ApplicationFlags::NON_UNIQUE
    } else {
        gio::ApplicationFlags::empty()
    };

    let app = Application::builder()
        .application_id(APP_ID)
        .flags(flags)
        .build();

    app.connect_startup(|_| {
        load_css();
    });

    let startup_project_path = Rc::new(RefCell::new(startup_project_path));
    app.connect_activate(move |app| {
        let path = startup_project_path.borrow_mut().take();
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
