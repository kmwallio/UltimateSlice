use gtk4::prelude::*;
use gtk4::{self as gtk, Box as GBox, Button, Label, ListBox, ListBoxRow, Orientation, ScrolledWindow};
use gio;
use std::cell::RefCell;
use std::rc::Rc;
use crate::model::project::Project;
use crate::model::clip::{Clip, ClipKind};
use crate::model::track::TrackKind;

/// Builds the media browser panel.
/// `on_clip_added` is called when the user imports a file.
pub fn build_media_browser(
    project: Rc<RefCell<Project>>,
    on_clip_added: impl Fn() + 'static,
) -> GBox {
    let vbox = GBox::new(Orientation::Vertical, 4);
    vbox.set_width_request(180);

    let header = Label::new(Some("Media Browser"));
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

    // Collect all current clips for initial population
    {
        let proj = project.borrow();
        for track in &proj.tracks {
            for clip in &track.clips {
                list.append(&make_list_row(&clip.label, &clip.source_path));
            }
        }
    }

    // Import button — open file chooser
    {
        let project = project.clone();
        let list = list.clone();
        let on_clip_added = Rc::new(on_clip_added);

        import_btn.connect_clicked(move |btn| {
            let dialog = gtk::FileDialog::new();
            dialog.set_title("Import Media");

            // Build a filter for common video/audio types
            let filter = gtk::FileFilter::new();
            filter.add_mime_type("video/*");
            filter.add_mime_type("audio/*");
            filter.add_mime_type("image/*");
            filter.set_name(Some("Media Files"));

            let filters = gio::ListStore::new::<gtk::FileFilter>();
            filters.append(&filter);
            dialog.set_filters(Some(&filters));

            let project = project.clone();
            let list = list.clone();
            let on_clip_added = on_clip_added.clone();

            // Get the root window from the button
            let window = btn.root().and_then(|r| r.downcast::<gtk::Window>().ok());

            dialog.open(window.as_ref(), gio::Cancellable::NONE, move |result| {
                if let Ok(file) = result {
                    if let Some(path) = file.path() {
                        let path_str = path.to_string_lossy().to_string();
                        let uri = format!("file://{path_str}");

                        // Probe duration via GStreamer
                        let duration_ns = probe_duration(&uri).unwrap_or(10 * 1_000_000_000);

                        let clip = Clip::new(
                            path_str.clone(),
                            duration_ns,
                            0,
                            ClipKind::Video,
                        );

                        let label = clip.label.clone();
                        list.append(&make_list_row(&label, &path_str));

                        // Add clip to first video track at the end
                        let mut proj = project.borrow_mut();
                        if let Some(track) = proj.tracks.iter_mut().find(|t| t.kind == TrackKind::Video) {
                            let timeline_start = track.duration();
                            let mut c = clip;
                            c.timeline_start = timeline_start;
                            track.add_clip(c);
                        }
                        proj.dirty = true;
                        on_clip_added();
                    }
                }
            });
        });
    }

    vbox
}

fn make_list_row(label: &str, path: &str) -> ListBoxRow {
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
    row
}

/// Quickly probe the duration of a media file using GStreamer discoverer.
fn probe_duration(uri: &str) -> Option<u64> {
    use gstreamer_pbutils::Discoverer;
    gstreamer::init().ok()?;
    let discoverer = Discoverer::new(gstreamer::ClockTime::from_seconds(5)).ok()?;
    let info = discoverer.discover_uri(uri).ok()?;
    info.duration().map(|d| d.nseconds())
}
