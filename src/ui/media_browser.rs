use gtk4::prelude::*;
use gtk4::{self as gtk, Box as GBox, Button, Label, ListBox, ListBoxRow, Orientation, ScrolledWindow};
use gdk4;
use gio;
use glib;
use std::cell::RefCell;
use std::rc::Rc;
use crate::model::media_library::MediaItem;

/// Builds the media browser panel.
///
/// * `library`            – shared list of imported media items
/// * `on_source_selected` – called when the user selects a library item (path, duration_ns)
/// * `on_append`          – called when the user clicks "Append to Timeline"
pub fn build_media_browser(
    library: Rc<RefCell<Vec<MediaItem>>>,
    on_source_selected: Rc<dyn Fn(String, u64)>,
    on_append: Rc<dyn Fn()>,
) -> GBox {
    let vbox = GBox::new(Orientation::Vertical, 4);
    vbox.set_width_request(190);

    let header = Label::new(Some("Media Library"));
    header.add_css_class("browser-header");
    header.set_margin_top(8);
    header.set_margin_bottom(4);
    vbox.append(&header);

    let import_btn = Button::with_label("+ Import Media…");
    import_btn.set_margin_start(8);
    import_btn.set_margin_end(8);
    vbox.append(&import_btn);

    let scroll = ScrolledWindow::new();
    scroll.set_vexpand(true);
    scroll.set_policy(gtk::PolicyType::Never, gtk::PolicyType::Automatic);

    let list = ListBox::new();
    list.set_selection_mode(gtk::SelectionMode::Single);
    list.add_css_class("media-list");
    scroll.set_child(Some(&list));
    vbox.append(&scroll);

    // "Append to Timeline" button at the bottom of the browser
    let append_btn = Button::with_label("⬇ Append to Timeline");
    append_btn.set_margin_start(8);
    append_btn.set_margin_end(8);
    append_btn.set_margin_bottom(8);
    append_btn.set_sensitive(false); // enabled once a clip is selected
    vbox.append(&append_btn);

    // Populate list from existing library items (e.g. after project load)
    {
        let lib = library.borrow();
        for item in lib.iter() {
            list.append(&make_list_row(&item.label, &item.source_path, item.duration_ns));
        }
    }

    // Selection → fire on_source_selected
    {
        let library = library.clone();
        let on_source_selected = on_source_selected.clone();
        let append_btn = append_btn.clone();
        list.connect_row_selected(move |_, row| {
            if let Some(row) = row {
                let idx = row.index() as usize;
                let lib = library.borrow();
                if let Some(item) = lib.get(idx) {
                    let path = item.source_path.clone();
                    let dur = item.duration_ns;
                    drop(lib);
                    append_btn.set_sensitive(true);
                    on_source_selected(path, dur);
                }
            } else {
                append_btn.set_sensitive(false);
            }
        });
    }

    // Append button → fire on_append
    {
        let on_append = on_append.clone();
        append_btn.connect_clicked(move |_| {
            on_append();
        });
    }

    // Import button → file chooser, adds to library only
    {
        let library = library.clone();
        let list = list.clone();
        let on_source_selected = on_source_selected.clone();
        let append_btn_weak = append_btn.downgrade();

        import_btn.connect_clicked(move |btn| {
            let dialog = gtk::FileDialog::new();
            dialog.set_title("Import Media");

            let filter = gtk::FileFilter::new();
            filter.add_mime_type("video/*");
            filter.add_mime_type("audio/*");
            filter.add_mime_type("image/*");
            filter.set_name(Some("Media Files"));

            let filters = gio::ListStore::new::<gtk::FileFilter>();
            filters.append(&filter);
            dialog.set_filters(Some(&filters));

            let library = library.clone();
            let list = list.clone();
            let on_source_selected = on_source_selected.clone();
            let append_btn_weak = append_btn_weak.clone();

            let window = btn.root().and_then(|r| r.downcast::<gtk::Window>().ok());

            dialog.open(window.as_ref(), gio::Cancellable::NONE, move |result| {
                if let Ok(file) = result {
                    if let Some(path) = file.path() {
                        let path_str = path.to_string_lossy().to_string();
                        let uri = format!("file://{path_str}");
                        let duration_ns = probe_duration(&uri).unwrap_or(10 * 1_000_000_000);

                        let item = MediaItem::new(path_str.clone(), duration_ns);
                        let label = item.label.clone();

                        // Add to library
                        library.borrow_mut().push(item);
                        let row = make_list_row(&label, &path_str, duration_ns);
                        list.append(&row);

                        // Auto-select the newly imported item
                        list.select_row(Some(&row));
                        if let Some(btn) = append_btn_weak.upgrade() {
                            btn.set_sensitive(true);
                        }
                        // Load into source viewer immediately
                        on_source_selected(path_str, duration_ns);
                    }
                }
            });
        });
    }

    vbox
}

fn make_list_row(label: &str, path: &str, duration_ns: u64) -> ListBoxRow {
    let row = ListBoxRow::new();
    let vbox = GBox::new(Orientation::Vertical, 2);
    vbox.set_margin_start(8);
    vbox.set_margin_end(8);
    vbox.set_margin_top(4);
    vbox.set_margin_bottom(4);

    let name_label = Label::new(Some(label));
    name_label.set_halign(gtk::Align::Start);
    name_label.add_css_class("clip-name");

    let path_label = Label::new(Some(
        std::path::Path::new(path)
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or(path),
    ));
    path_label.set_halign(gtk::Align::Start);
    path_label.add_css_class("clip-path");
    path_label.set_opacity(0.6);

    vbox.append(&name_label);
    vbox.append(&path_label);
    row.set_child(Some(&vbox));

    // Drag source: payload = "{source_path}|{duration_ns}"
    let drag_src = gtk::DragSource::new();
    drag_src.set_actions(gdk4::DragAction::COPY);
    let payload = format!("{path}|{duration_ns}");
    drag_src.connect_prepare(move |_src, _x, _y| {
        let val = glib::Value::from(&payload);
        Some(gdk4::ContentProvider::for_value(&val))
    });
    row.add_controller(drag_src);

    row
}

/// Quickly probe the duration of a media file using GStreamer discoverer.
pub fn probe_duration(uri: &str) -> Option<u64> {
    use gstreamer_pbutils::Discoverer;
    gstreamer::init().ok()?;
    let discoverer = Discoverer::new(gstreamer::ClockTime::from_seconds(5)).ok()?;
    let info = discoverer.discover_uri(uri).ok()?;
    info.duration().map(|d| d.nseconds())
}
