use crate::media::probe_cache::MediaProbeCache;
use crate::media::thumb_cache::ThumbnailCache;
use crate::model::media_library::MediaItem;
use gdk4;
use gio;
use glib;
use gtk4::prelude::*;
use gtk4::{
    self as gtk, Box as GBox, Button, DrawingArea, FlowBox, FlowBoxChild, Label, Orientation,
    ScrolledWindow,
};
use std::cell::{Cell, RefCell};
use std::rc::Rc;

const THUMB_W: i32 = 160;
const THUMB_H: i32 = 90;

/// Builds the media browser panel.
///
/// * `library`            – shared list of imported media items
/// * `on_source_selected` – called when the user selects a library item (path, duration_ns)
pub fn build_media_browser(
    library: Rc<RefCell<Vec<MediaItem>>>,
    on_source_selected: Rc<dyn Fn(String, u64)>,
) -> (GBox, Rc<dyn Fn()>) {
    let vbox = GBox::new(Orientation::Vertical, 4);
    vbox.set_width_request(240);

    // Header row: "Media Library" title + compact "+" button (shown when library is non-empty).
    let header_row = GBox::new(Orientation::Horizontal, 0);
    header_row.set_margin_top(8);
    header_row.set_margin_bottom(4);
    header_row.set_margin_start(8);
    header_row.set_margin_end(8);

    let header = Label::new(Some("Media Library"));
    header.add_css_class("browser-header");
    header.set_hexpand(true);
    header.set_halign(gtk::Align::Start);
    header_row.append(&header);

    let header_import_btn = Button::from_icon_name("list-add-symbolic");
    header_import_btn.add_css_class("browser-header-import");
    header_import_btn.set_tooltip_text(Some("Import Media…"));
    header_import_btn.set_visible(!library.borrow().is_empty());
    header_row.append(&header_import_btn);

    vbox.append(&header_row);

    // Big import button — shown only when the library is empty.
    let import_btn = Button::with_label("+ Import Media…");
    import_btn.set_margin_start(8);
    import_btn.set_margin_end(8);
    import_btn.set_visible(library.borrow().is_empty());
    vbox.append(&import_btn);

    let empty_hint = Label::new(Some("Import media or drag files here to start editing."));
    empty_hint.set_halign(gtk::Align::Start);
    empty_hint.set_xalign(0.0);
    empty_hint.set_wrap(true);
    empty_hint.set_margin_start(8);
    empty_hint.set_margin_end(8);
    empty_hint.add_css_class("panel-empty-state");
    empty_hint.set_visible(library.borrow().is_empty());
    vbox.append(&empty_hint);

    let scroll = ScrolledWindow::new();
    scroll.set_vexpand(true);
    scroll.set_policy(gtk::PolicyType::Automatic, gtk::PolicyType::Automatic);

    // Async thumbnail cache (per-browser instance, all on GTK main thread).
    let thumb_cache: Rc<RefCell<ThumbnailCache>> = Rc::new(RefCell::new(ThumbnailCache::new()));

    // Async media probe cache — duration/audio-only detection in background threads.
    let probe_cache: Rc<RefCell<MediaProbeCache>> = Rc::new(RefCell::new(MediaProbeCache::new()));

    // Grid of thumbnail cells.
    let flow_box = FlowBox::new();
    flow_box.set_selection_mode(gtk::SelectionMode::Single);
    flow_box.set_homogeneous(true);
    flow_box.set_max_children_per_line(u32::MAX); // auto-wrap based on available width
    flow_box.set_min_children_per_line(1);
    flow_box.set_column_spacing(4);
    flow_box.set_row_spacing(4);
    flow_box.set_margin_start(4);
    flow_box.set_margin_end(4);
    flow_box.set_margin_top(4);
    flow_box.add_css_class("media-grid");
    scroll.set_child(Some(&flow_box));
    vbox.append(&scroll);
    let flow_box_paths = Rc::new(RefCell::new(Vec::<String>::new()));

    // Populate from existing library items (e.g. after project load).
    {
        let lib = library.borrow();
        for item in lib.iter() {
            let child = make_grid_item(
                &item.label,
                &item.source_path,
                item.duration_ns,
                &thumb_cache,
            );
            flow_box.insert(&child, -1);
            flow_box_paths.borrow_mut().push(item.source_path.clone());
        }
    }

    // Selection → fire on_source_selected.
    {
        let library = library.clone();
        let on_source_selected = on_source_selected.clone();
        flow_box.connect_selected_children_changed(move |fb| {
            let selected = fb.selected_children();
            if let Some(child) = selected.first() {
                let idx = child.index() as usize;
                let lib = library.borrow();
                if let Some(item) = lib.get(idx) {
                    let path = item.source_path.clone();
                    let dur = item.duration_ns;
                    drop(lib);
                    on_source_selected(path, dur);
                }
            }
        });
    }

    // Debounced thumbnail redraw trigger (coalesces bursts during bulk import).
    let thumb_redraw_scheduled = Rc::new(Cell::new(false));

    // 100ms timer: keep grid in sync with library + poll thumbnail & probe caches.
    {
        let library = library.clone();
        let flow_box = flow_box.clone();
        let thumb_cache = thumb_cache.clone();
        let probe_cache = probe_cache.clone();
        let flow_box_paths = flow_box_paths.clone();
        let empty_hint = empty_hint.clone();
        let import_btn = import_btn.clone();
        let header_import_btn = header_import_btn.clone();
        let thumb_redraw_scheduled = thumb_redraw_scheduled.clone();
        glib::timeout_add_local(std::time::Duration::from_millis(100), move || {
            // Drain completed probe results → update library items (lightweight).
            let resolved = probe_cache.borrow_mut().poll();
            if !resolved.is_empty() {
                let cache = probe_cache.borrow();
                let mut lib = library.borrow_mut();
                for path in &resolved {
                    if let Some(result) = cache.get(path) {
                        if let Some(item) = lib.iter_mut().find(|i| i.source_path == *path) {
                            item.duration_ns = result.duration_ns;
                            item.is_audio_only = result.is_audio_only;
                            item.has_audio = result.has_audio;
                            if item.source_timecode_base_ns.is_none() {
                                item.source_timecode_base_ns = result.source_timecode_base_ns;
                            }
                        }
                    }
                }
                if !flowbox_matches_library(&flow_box_paths.borrow(), &lib) {
                    rebuild_flowbox(&flow_box, &lib, &thumb_cache, &flow_box_paths);
                }
                drop(lib);
                // Now that probes are done, start thumbnail extraction for newly-probed
                // files (one at a time as probes complete, avoiding the burst of threads
                // that occurred when all thumbnails started simultaneously on import).
                {
                    let mut tc = thumb_cache.borrow_mut();
                    for path in &resolved {
                        tc.request(path, 0);
                    }
                }
                // Update drag-source payloads on existing children (avoids full rebuild).
                let lib = library.borrow();
                let mut child = flow_box.first_child();
                let mut idx = 0usize;
                while let Some(w) = child {
                    if let Some(item) = lib.get(idx) {
                        let payload = format!("{}|{}", item.source_path, item.duration_ns);
                        let val = glib::Value::from(&payload);
                        // Each FlowBoxChild has exactly one DragSource controller.
                        for ctrl in w.observe_controllers().into_iter().flatten() {
                            if let Ok(ds) = ctrl.downcast::<gtk::DragSource>() {
                                ds.set_content(Some(&gdk4::ContentProvider::for_value(&val)));
                                break;
                            }
                        }
                    }
                    idx += 1;
                    child = w.next_sibling();
                }
                drop(lib);
                drop(cache);
            }

            let lib = library.borrow();
            empty_hint.set_visible(lib.is_empty());
            import_btn.set_visible(lib.is_empty());
            header_import_btn.set_visible(!lib.is_empty());
            if !flowbox_matches_library(&flow_box_paths.borrow(), &lib) {
                rebuild_flowbox(&flow_box, &lib, &thumb_cache, &flow_box_paths);
            }
            drop(lib);
            let thumbnails_ready = !thumb_cache.borrow_mut().poll_ready_keys().is_empty();
            if thumbnails_ready && !thumb_redraw_scheduled.replace(true) {
                let flow_box = flow_box.clone();
                let thumb_redraw_scheduled = thumb_redraw_scheduled.clone();
                glib::timeout_add_local_once(std::time::Duration::from_millis(40), move || {
                    queue_flowbox_thumbnail_draws(&flow_box);
                    thumb_redraw_scheduled.set(false);
                });
            }
            glib::ControlFlow::Continue
        });
    }

    // Shared import handler: opens the file-chooser dialog for both import buttons.
    let do_import: Rc<dyn Fn(&Button)> = {
        let library = library.clone();
        let flow_box = flow_box.clone();
        let thumb_cache = thumb_cache.clone();
        let probe_cache = probe_cache.clone();
        let flow_box_paths = flow_box_paths.clone();

        Rc::new(move |btn: &Button| {
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
            let flow_box = flow_box.clone();
            let thumb_cache = thumb_cache.clone();
            let probe_cache = probe_cache.clone();
            let flow_box_paths = flow_box_paths.clone();

            let window = btn.root().and_then(|r| r.downcast::<gtk::Window>().ok());

            dialog.open_multiple(window.as_ref(), gio::Cancellable::NONE, move |result| {
                if let Ok(files) = result {
                    for i in 0..files.n_items() {
                        let Some(obj) = files.item(i) else { continue };
                        let Ok(file) = obj.downcast::<gio::File>() else {
                            continue;
                        };
                        let Some(path) = file.path() else { continue };
                        let _ = import_path_into_library(
                            path.to_string_lossy().to_string(),
                            &library,
                            &flow_box,
                            &thumb_cache,
                            &probe_cache,
                            &flow_box_paths,
                        );
                    }
                }
            });
        })
    };

    {
        let do_import = do_import.clone();
        import_btn.connect_clicked(move |btn| do_import(btn));
    }
    {
        let do_import = do_import.clone();
        header_import_btn.connect_clicked(move |btn| do_import(btn));
    }

    // External drag-and-drop import into media pane (e.g. from file manager).
    {
        let library = library.clone();
        let flow_box = flow_box.clone();
        let thumb_cache = thumb_cache.clone();
        let probe_cache = probe_cache.clone();
        let flow_box_paths = flow_box_paths.clone();
        let drop_target = gtk::DropTarget::new(glib::Type::STRING, gdk4::DragAction::COPY);
        let flow_box_for_drop = flow_box.clone();
        drop_target.connect_drop(move |_target, value, _x, _y| {
            let payload = match value.get::<String>() {
                Ok(s) => s,
                Err(_) => return false,
            };
            let mut imported_any = false;
            for path in parse_external_drop_paths(&payload) {
                if import_path_into_library(
                    path,
                    &library,
                    &flow_box_for_drop,
                    &thumb_cache,
                    &probe_cache,
                    &flow_box_paths,
                )
                .is_some()
                {
                    imported_any = true;
                }
            }
            imported_any
        });
        flow_box.add_controller(drop_target);
    }

    let clear_selection: Rc<dyn Fn()> = {
        let flow_box = flow_box.clone();
        Rc::new(move || {
            flow_box.unselect_all();
        })
    };

    (vbox, clear_selection)
}

/// Build a single thumbnail grid cell.
fn make_grid_item(
    label: &str,
    path: &str,
    duration_ns: u64,
    thumb_cache: &Rc<RefCell<ThumbnailCache>>,
) -> FlowBoxChild {
    // Kick off thumbnail loading — but only after the media has been probed
    // (duration_ns > 0) so we don't flood GStreamer with many concurrent
    // pipelines when bulk-importing files.
    if duration_ns > 0 {
        thumb_cache.borrow_mut().request(path, 0);
    }

    let cell = GBox::new(Orientation::Vertical, 2);
    cell.set_margin_start(2);
    cell.set_margin_end(2);
    cell.set_margin_top(2);
    cell.set_margin_bottom(2);

    // Thumbnail DrawingArea.
    let thumb_area = DrawingArea::new();
    thumb_area.set_content_width(THUMB_W);
    thumb_area.set_content_height(THUMB_H);
    {
        let path_owned = path.to_string();
        let thumb_cache = thumb_cache.clone();
        thumb_area.set_draw_func(move |_, cr, w, h| {
            let cache = thumb_cache.borrow();
            if let Some(surf) = cache.get(&path_owned, 0) {
                let sx = w as f64 / THUMB_W as f64;
                let sy = h as f64 / THUMB_H as f64;
                cr.scale(sx, sy);
                let _ = cr.set_source_surface(surf, 0.0, 0.0);
                cr.paint().ok();
                return;
            }
            // Placeholder while loading.
            cr.set_source_rgb(0.15, 0.15, 0.20);
            cr.rectangle(0.0, 0.0, w as f64, h as f64);
            cr.fill().ok();
        });
    }
    cell.append(&thumb_area);

    // Filename label (stem only, truncated).
    let filename = std::path::Path::new(path)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or(label)
        .to_string();
    let name_label = Label::new(Some(&filename));
    name_label.set_halign(gtk::Align::Center);
    name_label.set_max_width_chars(22);
    name_label.set_ellipsize(gtk4::pango::EllipsizeMode::End);
    name_label.add_css_class("clip-name");
    cell.append(&name_label);

    let child = FlowBoxChild::new();
    child.set_child(Some(&cell));
    // Drag source: payload = "{source_path}|{duration_ns}"
    let drag_src = gtk::DragSource::new();
    drag_src.set_actions(gdk4::DragAction::COPY);
    drag_src.set_exclusive(false);
    let payload = format!("{path}|{duration_ns}");
    let val = glib::Value::from(&payload);
    drag_src.set_content(Some(&gdk4::ContentProvider::for_value(&val)));
    child.add_controller(drag_src);

    child
}

#[derive(Debug, Clone, Copy, Default)]
pub struct MediaProbeMetadata {
    pub duration_ns: Option<u64>,
    pub is_audio_only: bool,
    pub has_audio: bool,
}

/// Probe duration + stream characteristics in one Discoverer pass.
pub fn probe_media_metadata(uri: &str) -> MediaProbeMetadata {
    use gstreamer_pbutils::Discoverer;
    let Ok(()) = gstreamer::init() else {
        return MediaProbeMetadata::default();
    };
    let Ok(discoverer) = Discoverer::new(gstreamer::ClockTime::from_seconds(5)) else {
        return MediaProbeMetadata::default();
    };
    let Ok(info) = discoverer.discover_uri(uri) else {
        return MediaProbeMetadata::default();
    };
    MediaProbeMetadata {
        duration_ns: info.duration().map(|d| d.nseconds()),
        is_audio_only: info.video_streams().is_empty(),
        has_audio: !info.audio_streams().is_empty(),
    }
}

fn flowbox_matches_library(current_paths: &[String], lib: &[MediaItem]) -> bool {
    current_paths.len() == lib.len()
        && current_paths
            .iter()
            .zip(lib.iter())
            .all(|(a, b)| a == &b.source_path)
}

fn queue_flowbox_thumbnail_draws(fb: &FlowBox) {
    let mut child = fb.first_child();
    while let Some(w) = child {
        if let Some(cell) = w.first_child() {
            if let Some(thumb) = cell.first_child() {
                if let Ok(area) = thumb.clone().downcast::<DrawingArea>() {
                    area.queue_draw();
                } else {
                    thumb.queue_draw();
                }
            }
        }
        child = w.next_sibling();
    }
}

fn import_path_into_library(
    path_str: String,
    library: &Rc<RefCell<Vec<MediaItem>>>,
    flow_box: &FlowBox,
    thumb_cache: &Rc<RefCell<ThumbnailCache>>,
    probe_cache: &Rc<RefCell<MediaProbeCache>>,
    flow_box_paths: &Rc<RefCell<Vec<String>>>,
) -> Option<(String, u64, FlowBoxChild)> {
    if path_str.is_empty() {
        return None;
    }
    // Start background probe (non-blocking). Duration/audio-only updated by 250ms timer.
    probe_cache.borrow_mut().request(&path_str);
    let duration_ns = 0; // placeholder until probe completes
    let item = MediaItem::new(path_str.clone(), duration_ns);
    let label = item.label.clone();
    library.borrow_mut().push(item);
    let child = make_grid_item(&label, &path_str, duration_ns, thumb_cache);
    flow_box.insert(&child, -1);
    flow_box_paths.borrow_mut().push(path_str.clone());
    Some((path_str, duration_ns, child))
}

fn parse_external_drop_paths(payload: &str) -> Vec<String> {
    // Ignore internal app payloads ("{source_path}|{duration_ns}").
    if payload.contains('|') && !payload.contains("file://") {
        return Vec::new();
    }
    let mut out = Vec::new();
    for line in payload.lines() {
        let s = line.trim();
        if s.is_empty() || s.starts_with('#') {
            continue;
        }
        if s.starts_with("file://") {
            let f = gio::File::for_uri(s);
            if let Some(path) = f.path() {
                out.push(path.to_string_lossy().to_string());
            }
        } else if std::path::Path::new(s).is_absolute() {
            out.push(s.to_string());
        }
    }
    out
}

fn rebuild_flowbox(
    fb: &FlowBox,
    lib: &[MediaItem],
    thumb_cache: &Rc<RefCell<ThumbnailCache>>,
    flow_box_paths: &Rc<RefCell<Vec<String>>>,
) {
    while let Some(child) = fb.first_child() {
        fb.remove(&child);
    }
    let mut paths = flow_box_paths.borrow_mut();
    paths.clear();
    for item in lib.iter() {
        let child = make_grid_item(
            &item.label,
            &item.source_path,
            item.duration_ns,
            thumb_cache,
        );
        fb.insert(&child, -1);
        paths.push(item.source_path.clone());
    }
}
