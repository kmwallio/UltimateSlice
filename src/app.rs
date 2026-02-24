use gtk4::prelude::*;
use gtk4::Application;
use crate::ui::window::build_window;

const APP_ID: &str = "io.github.ultimateslice";

pub fn run() {
    let app = Application::builder()
        .application_id(APP_ID)
        .build();

    app.connect_startup(|_| {
        load_css();
    });

    app.connect_activate(|app| {
        build_window(app);
    });

    app.run();
}

fn load_css() {
    let css = gtk4::CssProvider::new();
    let css_data = include_str!("style.css");
    css.load_from_data(css_data);
    gtk4::style_context_add_provider_for_display(
        &gdk4::Display::default().expect("no display"),
        &css,
        gtk4::STYLE_PROVIDER_PRIORITY_APPLICATION,
    );
}
