//! Post-export "share / locate" helpers and popover.
//!
//! After an export finishes, the progress dialog reveals a **Share / Locate**
//! menu button whose popover offers the common next steps for a freshly
//! rendered file: reveal it in the system file manager, open it with the
//! default application, or copy its path to the clipboard. The launch
//! actions use GTK4's portal-friendly `FileLauncher` so they work inside the
//! Flatpak sandbox.

use gio;
use gtk4::prelude::*;
use gtk4::{self as gtk};
use std::path::Path;

/// Reveal `path` in the system file manager. `open_containing_folder` asks
/// the desktop portal to highlight the file inside its folder when
/// supported, falling back to just opening the folder otherwise.
pub fn reveal_in_file_manager(parent: Option<&gtk::Window>, path: &Path) {
    let file = gio::File::for_path(path);
    let launcher = gtk::FileLauncher::new(Some(&file));
    launcher.open_containing_folder(parent, gio::Cancellable::NONE, move |res| {
        if let Err(e) = res {
            log::warn!("Reveal in file manager failed: {e}");
        }
    });
}

/// Open `path` with the user's default application for that file type.
pub fn open_file(parent: Option<&gtk::Window>, path: &Path) {
    let file = gio::File::for_path(path);
    let launcher = gtk::FileLauncher::new(Some(&file));
    launcher.launch(parent, gio::Cancellable::NONE, move |res| {
        if let Err(e) = res {
            log::warn!("Open exported file failed: {e}");
        }
    });
}

/// Copy `text` to the clipboard associated with `widget`'s display.
pub fn copy_to_clipboard(widget: &impl IsA<gtk::Widget>, text: &str) {
    widget.clipboard().set_text(text);
}

/// Build a "Share / Locate" [`gtk::MenuButton`] whose popover offers
/// Reveal-in-file-manager / Open / Copy-path for a finished export at
/// `path`. `parent` is used as the transient parent for the launch dialogs.
pub fn build_share_button(parent: &gtk::Window, path: String) -> gtk::MenuButton {
    let menu_btn = gtk::MenuButton::new();
    menu_btn.set_label("Share / Locate");
    menu_btn.set_tooltip_text(Some(
        "Reveal, open, or copy the path of the exported file",
    ));

    let popover = gtk::Popover::new();
    let vbox = gtk::Box::new(gtk::Orientation::Vertical, 2);
    vbox.set_margin_top(4);
    vbox.set_margin_bottom(4);
    vbox.set_margin_start(4);
    vbox.set_margin_end(4);

    let make_item = |label: &str| {
        let b = gtk::Button::with_label(label);
        b.add_css_class("flat");
        b.set_halign(gtk::Align::Fill);
        b
    };

    let reveal = make_item("Reveal in file manager");
    {
        let parent = parent.clone();
        let p = path.clone();
        let popover = popover.clone();
        reveal.connect_clicked(move |_| {
            popover.popdown();
            reveal_in_file_manager(Some(&parent), Path::new(&p));
        });
    }
    vbox.append(&reveal);

    let open = make_item("Open");
    {
        let parent = parent.clone();
        let p = path.clone();
        let popover = popover.clone();
        open.connect_clicked(move |_| {
            popover.popdown();
            open_file(Some(&parent), Path::new(&p));
        });
    }
    vbox.append(&open);

    let copy = make_item("Copy path");
    {
        let p = path.clone();
        let popover = popover.clone();
        copy.connect_clicked(move |btn| {
            popover.popdown();
            copy_to_clipboard(btn, &p);
        });
    }
    vbox.append(&copy);

    popover.set_child(Some(&vbox));
    menu_btn.set_popover(Some(&popover));
    menu_btn
}
