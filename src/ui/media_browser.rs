use crate::media::probe_cache::MediaProbeCache;
use crate::media::proxy_cache::{ProxyCache, ProxyScale};
use crate::media::thumb_cache::ThumbnailCache;
use crate::model::media_library::{MediaItem, MediaLibrary};
use crate::ui_state::PreferencesState;
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

/// Distinguishes bin cells from media cells in the FlowBox.
#[derive(Debug, Clone)]
enum FlowBoxEntry {
    Bin { id: String, name: String },
    Media { path: String, is_missing: bool, is_audio_only: bool },
}

/// Builds the media browser panel.
///
/// * `library`            – shared list of imported media items
/// * `on_source_selected` – called when the user selects a library item (path, duration_ns)
/// * `proxy_cache`        – shared proxy cache; used to pre-generate proxies when proxy mode is on
/// * `preferences_state`  – shared preferences; read for proxy mode/scale
pub fn build_media_browser(
    library: Rc<RefCell<MediaLibrary>>,
    on_source_selected: Rc<dyn Fn(String, u64)>,
    on_relink_media: Rc<dyn Fn()>,
    on_create_multicam_from_browser: Rc<dyn Fn(Vec<String>)>,
    proxy_cache: Rc<RefCell<ProxyCache>>,
    preferences_state: Rc<RefCell<PreferencesState>>,
) -> (GBox, Rc<dyn Fn()>, Rc<dyn Fn()>) {
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

    let all_media_btn = Button::with_label("All");
    all_media_btn.add_css_class("bin-breadcrumb-btn");
    all_media_btn.set_tooltip_text(Some("Show all media regardless of bin"));
    all_media_btn.set_visible(false); // only visible when bins exist
    header_row.append(&all_media_btn);

    let header_import_btn = Button::from_icon_name("list-add-symbolic");
    header_import_btn.add_css_class("browser-header-import");
    header_import_btn.set_tooltip_text(Some("Import Media…"));
    header_import_btn.set_visible(!library.borrow().items.is_empty());
    header_row.append(&header_import_btn);

    let header_relink_btn = Button::with_label("Relink…");
    header_relink_btn.add_css_class("browser-header-import");
    header_relink_btn.set_tooltip_text(Some("Relink selected offline media"));
    header_relink_btn.set_visible(false);
    {
        let on_relink_media = on_relink_media.clone();
        header_relink_btn.connect_clicked(move |_| on_relink_media());
    }
    header_row.append(&header_relink_btn);

    vbox.append(&header_row);

    // Breadcrumb navigation bar — shows path to current bin.
    let breadcrumb_bar = GBox::new(Orientation::Horizontal, 2);
    breadcrumb_bar.set_margin_start(8);
    breadcrumb_bar.set_margin_end(8);
    breadcrumb_bar.set_margin_bottom(2);
    breadcrumb_bar.add_css_class("bin-breadcrumb-bar");
    breadcrumb_bar.set_visible(false);
    vbox.append(&breadcrumb_bar);

    // Big import button — shown only when the library is empty.
    let import_btn = Button::with_label("+ Import Media…");
    import_btn.set_margin_start(8);
    import_btn.set_margin_end(8);
    import_btn.set_visible(library.borrow().items.is_empty());
    vbox.append(&import_btn);

    let empty_hint = Label::new(Some("Import media or drag files here to start editing."));
    empty_hint.set_halign(gtk::Align::Start);
    empty_hint.set_xalign(0.0);
    empty_hint.set_wrap(true);
    empty_hint.set_margin_start(8);
    empty_hint.set_margin_end(8);
    empty_hint.add_css_class("panel-empty-state");
    empty_hint.set_visible(library.borrow().items.is_empty());
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
    flow_box.set_selection_mode(gtk::SelectionMode::Multiple);
    flow_box.set_homogeneous(true);
    flow_box.set_max_children_per_line(u32::MAX); // auto-wrap based on available width
    flow_box.set_min_children_per_line(1);
    flow_box.set_column_spacing(4);
    flow_box.set_row_spacing(4);
    flow_box.set_margin_start(4);
    flow_box.set_margin_end(4);
    flow_box.set_margin_top(4);
    flow_box.add_css_class("media-grid");

    // Bin navigation state.
    let current_bin_id: Rc<RefCell<Option<String>>> = Rc::new(RefCell::new(None));
    let show_all_media: Rc<RefCell<bool>> = Rc::new(RefCell::new(false));

    let flow_box_paths: Rc<RefCell<Vec<FlowBoxEntry>>> = Rc::new(RefCell::new(Vec::new()));

    // Double-click handler: navigate into bins (Capture phase, claims on double-click only).
    {
        let flow_box_click = flow_box.clone();
        let flow_box_paths_click = flow_box_paths.clone();
        let library_click = library.clone();
        let current_bin_id_click = current_bin_id.clone();
        let show_all_media_click = show_all_media.clone();
        let thumb_cache_click = thumb_cache.clone();
        let breadcrumb_bar_click = breadcrumb_bar.clone();
        let all_media_btn_click = all_media_btn.clone();
        let dbl_click = gtk::GestureClick::new();
        dbl_click.set_button(1);
        dbl_click.set_propagation_phase(gtk::PropagationPhase::Capture);
        dbl_click.connect_pressed(move |gesture, n_press, x, y| {
            if n_press != 2 { return; }
            let clicked_child: Option<FlowBoxChild> = {
                let mut found = None;
                let mut child = flow_box_click.first_child();
                while let Some(c) = child {
                    if let Ok(fbc) = c.clone().downcast::<FlowBoxChild>() {
                        let alloc = fbc.allocation();
                        if x >= alloc.x() as f64
                            && x < (alloc.x() + alloc.width()) as f64
                            && y >= alloc.y() as f64
                            && y < (alloc.y() + alloc.height()) as f64
                        {
                            found = Some(fbc);
                            break;
                        }
                    }
                    child = c.next_sibling();
                }
                found
            };
            if let Some(ref child) = clicked_child {
                let idx = child.index() as usize;
                let entries = flow_box_paths_click.borrow();
                if let Some(FlowBoxEntry::Bin { ref id, .. }) = entries.get(idx) {
                    let bin_id = id.clone();
                    drop(entries);
                    *current_bin_id_click.borrow_mut() = Some(bin_id);
                    *show_all_media_click.borrow_mut() = false;
                    let lib = library_click.borrow();
                    rebuild_flowbox_binned(&flow_box_click, &lib, &thumb_cache_click, &flow_box_paths_click, &current_bin_id_click.borrow(), false, &library_click);
                    rebuild_breadcrumb(&breadcrumb_bar_click, &lib, &current_bin_id_click.borrow(), &current_bin_id_click, &show_all_media_click, &flow_box_click, &library_click, &thumb_cache_click, &flow_box_paths_click, &all_media_btn_click);
                    gesture.set_state(gtk::EventSequenceState::Claimed);
                }
            }
        });
        flow_box.add_controller(dbl_click);
    }
    // Selection handler: single-click selects on release so DragSource can start on press.
    // Ctrl+click toggles. Uses Bubble phase so child DragSource gets first shot at press.
    {
        let flow_box_click = flow_box.clone();
        let click = gtk::GestureClick::new();
        click.set_button(1);
        click.connect_released(move |_gesture, n_press, x, y| {
            if n_press != 1 { return; }
            let mods = _gesture.current_event_state();
            let ctrl = mods.contains(gtk::gdk::ModifierType::CONTROL_MASK)
                || mods.contains(gtk::gdk::ModifierType::META_MASK);

            let clicked_child: Option<FlowBoxChild> = {
                let mut found = None;
                let mut child = flow_box_click.first_child();
                while let Some(c) = child {
                    if let Ok(fbc) = c.clone().downcast::<FlowBoxChild>() {
                        let alloc = fbc.allocation();
                        if x >= alloc.x() as f64
                            && x < (alloc.x() + alloc.width()) as f64
                            && y >= alloc.y() as f64
                            && y < (alloc.y() + alloc.height()) as f64
                        {
                            found = Some(fbc);
                            break;
                        }
                    }
                    child = c.next_sibling();
                }
                found
            };

            if ctrl {
                if let Some(ref child) = clicked_child {
                    if child.is_selected() {
                        flow_box_click.unselect_child(child);
                    } else {
                        flow_box_click.select_child(child);
                    }
                }
            } else {
                flow_box_click.unselect_all();
                if let Some(ref child) = clicked_child {
                    flow_box_click.select_child(child);
                }
            }
        });
        flow_box.add_controller(click);
    }

    scroll.set_child(Some(&flow_box));
    vbox.append(&scroll);

    // "Create Multicam Clip" button — visible when 2+ media items are selected.
    let multicam_btn = Button::with_label("Create Multicam Clip");
    multicam_btn.add_css_class("suggested-action");
    multicam_btn.set_margin_start(8);
    multicam_btn.set_margin_end(8);
    multicam_btn.set_margin_bottom(4);
    multicam_btn.set_visible(false);
    multicam_btn.set_tooltip_text(Some(
        "Sync selected media by audio and create a multicam clip on the timeline",
    ));
    vbox.append(&multicam_btn);

    // Populate from existing library items (e.g. after project load).
    rebuild_flowbox_binned(
        &flow_box,
        &library.borrow(),
        &thumb_cache,
        &flow_box_paths,
        &current_bin_id.borrow(),
        *show_all_media.borrow(),
        &library,
    );

    // Right-click context menu for bin operations.
    {
        let flow_box_ctx = flow_box.clone();
        let library_ctx = library.clone();
        let flow_box_paths_ctx = flow_box_paths.clone();
        let current_bin_id_ctx = current_bin_id.clone();
        let show_all_media_ctx = show_all_media.clone();
        let thumb_cache_ctx = thumb_cache.clone();
        let breadcrumb_bar_ctx = breadcrumb_bar.clone();
        let all_media_btn_ctx = all_media_btn.clone();
        let rclick = gtk::GestureClick::new();
        rclick.set_button(3); // right button
        rclick.set_propagation_phase(gtk::PropagationPhase::Capture);
        rclick.connect_pressed(move |gesture, _n_press, x, y| {
            gesture.set_state(gtk::EventSequenceState::Claimed);

            // Find which child was right-clicked
            let clicked_child: Option<FlowBoxChild> = {
                let mut found = None;
                let mut child = flow_box_ctx.first_child();
                while let Some(c) = child {
                    if let Ok(fbc) = c.clone().downcast::<FlowBoxChild>() {
                        let alloc = fbc.allocation();
                        if x >= alloc.x() as f64
                            && x < (alloc.x() + alloc.width()) as f64
                            && y >= alloc.y() as f64
                            && y < (alloc.y() + alloc.height()) as f64
                        {
                            found = Some(fbc);
                            break;
                        }
                    }
                    child = c.next_sibling();
                }
                found
            };

            let clicked_entry = clicked_child.as_ref().and_then(|c| {
                let idx = c.index() as usize;
                flow_box_paths_ctx.borrow().get(idx).cloned()
            });

            // Build popover menu
            let popover = gtk::Popover::new();
            popover.set_has_arrow(false);
            popover.set_position(gtk::PositionType::Bottom);
            let menu_box = GBox::new(Orientation::Vertical, 0);
            menu_box.set_margin_top(4);
            menu_box.set_margin_bottom(4);
            menu_box.set_margin_start(4);
            menu_box.set_margin_end(4);

            let add_menu_item = |menu_box: &GBox, label: &str| -> Button {
                let btn = Button::with_label(label);
                btn.add_css_class("flat");
                btn.set_halign(gtk::Align::Fill);
                menu_box.append(&btn);
                btn
            };

            match &clicked_entry {
                Some(FlowBoxEntry::Bin { id, name: _ }) => {
                    // Right-clicked on a bin
                    let open_btn = add_menu_item(&menu_box, "Open");
                    {
                        let bin_id = id.clone();
                        let current_bin_id = current_bin_id_ctx.clone();
                        let show_all_media = show_all_media_ctx.clone();
                        let flow_box = flow_box_ctx.clone();
                        let library = library_ctx.clone();
                        let thumb_cache = thumb_cache_ctx.clone();
                        let flow_box_paths = flow_box_paths_ctx.clone();
                        let breadcrumb_bar = breadcrumb_bar_ctx.clone();
                        let all_media_btn = all_media_btn_ctx.clone();
                        let popover = popover.clone();
                        open_btn.connect_clicked(move |_| {
                            popover.popdown();
                            *current_bin_id.borrow_mut() = Some(bin_id.clone());
                            *show_all_media.borrow_mut() = false;
                            let lib = library.borrow();
                            rebuild_flowbox_binned(&flow_box, &lib, &thumb_cache, &flow_box_paths, &current_bin_id.borrow(), false, &library);
                            rebuild_breadcrumb(&breadcrumb_bar, &lib, &current_bin_id.borrow(), &current_bin_id, &show_all_media, &flow_box, &library, &thumb_cache, &flow_box_paths, &all_media_btn);
                        });
                    }

                    let rename_btn = add_menu_item(&menu_box, "Rename…");
                    {
                        let bin_id = id.clone();
                        let library = library_ctx.clone();
                        let flow_box = flow_box_ctx.clone();
                        let thumb_cache = thumb_cache_ctx.clone();
                        let flow_box_paths = flow_box_paths_ctx.clone();
                        let current_bin_id = current_bin_id_ctx.clone();
                        let show_all_media = show_all_media_ctx.clone();
                        let breadcrumb_bar = breadcrumb_bar_ctx.clone();
                        let all_media_btn = all_media_btn_ctx.clone();
                        let popover = popover.clone();
                        rename_btn.connect_clicked(move |btn| {
                            popover.popdown();
                            show_rename_dialog(btn, &bin_id, &library, &flow_box, &thumb_cache, &flow_box_paths, &current_bin_id, &show_all_media, &breadcrumb_bar, &all_media_btn);
                        });
                    }

                    let delete_btn = add_menu_item(&menu_box, "Delete");
                    {
                        let bin_id = id.clone();
                        let library = library_ctx.clone();
                        let flow_box = flow_box_ctx.clone();
                        let thumb_cache = thumb_cache_ctx.clone();
                        let flow_box_paths = flow_box_paths_ctx.clone();
                        let current_bin_id = current_bin_id_ctx.clone();
                        let show_all_media = show_all_media_ctx.clone();
                        let breadcrumb_bar = breadcrumb_bar_ctx.clone();
                        let all_media_btn = all_media_btn_ctx.clone();
                        let popover = popover.clone();
                        delete_btn.connect_clicked(move |_| {
                            popover.popdown();
                            delete_bin(&bin_id, &library, &flow_box, &thumb_cache, &flow_box_paths, &current_bin_id, &show_all_media, &breadcrumb_bar, &all_media_btn);
                        });
                    }

                    // "New Sub-bin" only if depth < 1
                    {
                        let lib = library_ctx.borrow();
                        let bin = lib.bins.iter().find(|b| b.id == *id);
                        if let Some(bin) = bin {
                            if bin.depth(&lib.bins) < 1 {
                                drop(lib);
                                let sub_btn = add_menu_item(&menu_box, "New Sub-bin…");
                                let parent_id = id.clone();
                                let library = library_ctx.clone();
                                let flow_box = flow_box_ctx.clone();
                                let thumb_cache = thumb_cache_ctx.clone();
                                let flow_box_paths = flow_box_paths_ctx.clone();
                                let current_bin_id = current_bin_id_ctx.clone();
                                let show_all_media = show_all_media_ctx.clone();
                                let breadcrumb_bar = breadcrumb_bar_ctx.clone();
                                let all_media_btn = all_media_btn_ctx.clone();
                                let popover = popover.clone();
                                sub_btn.connect_clicked(move |btn| {
                                    popover.popdown();
                                    show_new_bin_dialog(btn, Some(parent_id.clone()), &library, &flow_box, &thumb_cache, &flow_box_paths, &current_bin_id, &show_all_media, &breadcrumb_bar, &all_media_btn);
                                });
                            }
                        }
                    }
                }
                Some(FlowBoxEntry::Media { .. }) => {
                    // Right-clicked on a media item — "Move to Bin" submenu
                    let selected = flow_box_ctx.selected_children();
                    let entries = flow_box_paths_ctx.borrow();
                    let selected_paths: Vec<String> = selected.iter().filter_map(|c| {
                        let idx = c.index() as usize;
                        match entries.get(idx) {
                            Some(FlowBoxEntry::Media { path, .. }) => Some(path.clone()),
                            _ => None,
                        }
                    }).collect();
                    drop(entries);

                    if !selected_paths.is_empty() {
                        let lib = library_ctx.borrow();
                        if !lib.bins.is_empty() {
                            // "Move to Root" option
                            let root_btn = add_menu_item(&menu_box, "Move to Root");
                            {
                                let paths = selected_paths.clone();
                                let library = library_ctx.clone();
                                let flow_box = flow_box_ctx.clone();
                                let thumb_cache = thumb_cache_ctx.clone();
                                let flow_box_paths = flow_box_paths_ctx.clone();
                                let current_bin_id = current_bin_id_ctx.clone();
                                let show_all_media = show_all_media_ctx.clone();
                                let breadcrumb_bar = breadcrumb_bar_ctx.clone();
                                let all_media_btn = all_media_btn_ctx.clone();
                                let popover = popover.clone();
                                root_btn.connect_clicked(move |_| {
                                    popover.popdown();
                                    move_items_to_bin(&paths, None, &library, &flow_box, &thumb_cache, &flow_box_paths, &current_bin_id, &show_all_media, &breadcrumb_bar, &all_media_btn);
                                });
                            }

                            // One button per bin
                            for bin in lib.bins.iter() {
                                let label = if bin.parent_id.is_some() {
                                    format!("  └ {}", bin.name)
                                } else {
                                    format!("Move to \"{}\"", bin.name)
                                };
                                let move_btn = add_menu_item(&menu_box, &label);
                                let bin_id = bin.id.clone();
                                let paths = selected_paths.clone();
                                let library = library_ctx.clone();
                                let flow_box = flow_box_ctx.clone();
                                let thumb_cache = thumb_cache_ctx.clone();
                                let flow_box_paths = flow_box_paths_ctx.clone();
                                let current_bin_id = current_bin_id_ctx.clone();
                                let show_all_media = show_all_media_ctx.clone();
                                let breadcrumb_bar = breadcrumb_bar_ctx.clone();
                                let all_media_btn = all_media_btn_ctx.clone();
                                let popover = popover.clone();
                                move_btn.connect_clicked(move |_| {
                                    popover.popdown();
                                    move_items_to_bin(&paths, Some(bin_id.clone()), &library, &flow_box, &thumb_cache, &flow_box_paths, &current_bin_id, &show_all_media, &breadcrumb_bar, &all_media_btn);
                                });
                            }
                        }
                        drop(lib);
                    }

                    // "New Bin" always available
                    let new_bin_btn = add_menu_item(&menu_box, "New Bin…");
                    {
                        let library = library_ctx.clone();
                        let flow_box = flow_box_ctx.clone();
                        let thumb_cache = thumb_cache_ctx.clone();
                        let flow_box_paths = flow_box_paths_ctx.clone();
                        let current_bin_id = current_bin_id_ctx.clone();
                        let show_all_media = show_all_media_ctx.clone();
                        let breadcrumb_bar = breadcrumb_bar_ctx.clone();
                        let all_media_btn = all_media_btn_ctx.clone();
                        let popover = popover.clone();
                        new_bin_btn.connect_clicked(move |btn| {
                            popover.popdown();
                            show_new_bin_dialog(btn, current_bin_id.borrow().clone(), &library, &flow_box, &thumb_cache, &flow_box_paths, &current_bin_id, &show_all_media, &breadcrumb_bar, &all_media_btn);
                        });
                    }
                }
                None => {
                    // Right-clicked on empty area
                    let new_bin_btn = add_menu_item(&menu_box, "New Bin…");
                    {
                        let library = library_ctx.clone();
                        let flow_box = flow_box_ctx.clone();
                        let thumb_cache = thumb_cache_ctx.clone();
                        let flow_box_paths = flow_box_paths_ctx.clone();
                        let current_bin_id = current_bin_id_ctx.clone();
                        let show_all_media = show_all_media_ctx.clone();
                        let breadcrumb_bar = breadcrumb_bar_ctx.clone();
                        let all_media_btn = all_media_btn_ctx.clone();
                        let popover = popover.clone();
                        new_bin_btn.connect_clicked(move |btn| {
                            popover.popdown();
                            show_new_bin_dialog(btn, current_bin_id.borrow().clone(), &library, &flow_box, &thumb_cache, &flow_box_paths, &current_bin_id, &show_all_media, &breadcrumb_bar, &all_media_btn);
                        });
                    }
                }
            }

            popover.set_child(Some(&menu_box));
            popover.set_parent(&flow_box_ctx);
            // Position near click
            popover.set_pointing_to(Some(&gdk4::Rectangle::new(x as i32, y as i32, 1, 1)));
            // Unparent on close so the popover doesn't interfere with FlowBox child removal.
            {
                let popover_ref = popover.clone();
                popover.connect_closed(move |_| {
                    popover_ref.unparent();
                });
            }
            popover.popup();
        });
        flow_box.add_controller(rclick);
    }

    // Selection → fire on_source_selected + toggle relink/multicam buttons.
    {
        let library = library.clone();
        let on_source_selected = on_source_selected.clone();
        let header_relink_btn = header_relink_btn.clone();
        let multicam_btn = multicam_btn.clone();
        let flow_box_paths_for_sel = flow_box_paths.clone();
        flow_box.connect_selected_children_changed(move |fb| {
            let selected = fb.selected_children();
            // Count only media selections (not bins) for multicam
            let entries = flow_box_paths_for_sel.borrow();
            let media_count = selected.iter().filter(|c| {
                let idx = c.index() as usize;
                matches!(entries.get(idx), Some(FlowBoxEntry::Media { .. }))
            }).count();
            multicam_btn.set_visible(media_count >= 2);
            if let Some(child) = selected.first() {
                let idx = child.index() as usize;
                match entries.get(idx) {
                    Some(FlowBoxEntry::Media { path, .. }) => {
                        let lib = library.borrow();
                        if let Some(item) = lib.items.iter().find(|i| &i.source_path == path) {
                            let path = item.source_path.clone();
                            let dur = item.duration_ns;
                            let is_missing = item.is_missing;
                            drop(lib);
                            drop(entries);
                            header_relink_btn.set_visible(is_missing);
                            on_source_selected(path, dur);
                            return;
                        }
                    }
                    Some(FlowBoxEntry::Bin { .. }) => {
                        // Bin selected — don't load in source monitor
                        header_relink_btn.set_visible(false);
                    }
                    None => {
                        header_relink_btn.set_visible(false);
                    }
                }
            } else {
                header_relink_btn.set_visible(false);
            }
        });
    }

    // "Create Multicam Clip" button click handler.
    {
        let library = library.clone();
        let flow_box = flow_box.clone();
        let on_create_multicam = on_create_multicam_from_browser.clone();
        let flow_box_paths_for_mc = flow_box_paths.clone();
        multicam_btn.connect_clicked(move |_| {
            let selected = flow_box.selected_children();
            let entries = flow_box_paths_for_mc.borrow();
            let lib = library.borrow();
            let paths: Vec<String> = selected
                .iter()
                .filter_map(|child| {
                    let idx = child.index() as usize;
                    match entries.get(idx) {
                        Some(FlowBoxEntry::Media { path, .. }) => {
                            lib.items.iter().find(|i| &i.source_path == path).map(|i| i.source_path.clone())
                        }
                        _ => None,
                    }
                })
                .collect();
            drop(lib);
            drop(entries);
            if paths.len() >= 2 {
                on_create_multicam(paths);
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
        let proxy_cache = proxy_cache.clone();
        let preferences_state = preferences_state.clone();
        let flow_box_paths = flow_box_paths.clone();
        let empty_hint = empty_hint.clone();
        let import_btn = import_btn.clone();
        let header_import_btn = header_import_btn.clone();
        let thumb_redraw_scheduled = thumb_redraw_scheduled.clone();
        let current_bin_id = current_bin_id.clone();
        let show_all_media = show_all_media.clone();
        let breadcrumb_bar = breadcrumb_bar.clone();
        let all_media_btn = all_media_btn.clone();
        glib::timeout_add_local(std::time::Duration::from_millis(100), move || {
            // Drain completed probe results → update library items (lightweight).
            let resolved = probe_cache.borrow_mut().poll();
            if !resolved.is_empty() {
                let cache = probe_cache.borrow();
                let mut lib = library.borrow_mut();
                for path in &resolved {
                    if let Some(result) = cache.get(path) {
                        if let Some(item) = lib.items.iter_mut().find(|i| i.source_path == *path) {
                            item.duration_ns = result.duration_ns;
                            item.is_audio_only = result.is_audio_only;
                            item.has_audio = result.has_audio;
                            item.is_image = result.is_image;
                            if item.source_timecode_base_ns.is_none() {
                                item.source_timecode_base_ns = result.source_timecode_base_ns;
                            }
                        }
                    }
                }
                if !flowbox_matches_library_binned(&flow_box_paths.borrow(), &lib, &current_bin_id.borrow(), *show_all_media.borrow()) {
                    rebuild_flowbox_binned(&flow_box, &lib, &thumb_cache, &flow_box_paths, &current_bin_id.borrow(), *show_all_media.borrow(), &library);
                    rebuild_breadcrumb(&breadcrumb_bar, &lib, &current_bin_id.borrow(), &current_bin_id, &show_all_media, &flow_box, &library, &thumb_cache, &flow_box_paths, &all_media_btn);
                }
                drop(lib);
                // Now that probes are done, start thumbnail extraction for newly-probed
                // files (one at a time as probes complete, avoiding the burst of threads
                // that occurred when all thumbnails started simultaneously on import).
                // Skip audio-only files — they have no video frame to extract.
                {
                    let lib = library.borrow();
                    let mut tc = thumb_cache.borrow_mut();
                    for path in &resolved {
                        let audio_only = lib.items
                            .iter()
                            .find(|i| i.source_path == *path)
                            .map(|i| i.is_audio_only)
                            .unwrap_or(false);
                        if !audio_only {
                            tc.request(path, 0);
                        }
                    }
                }
                // Pre-generate proxies for newly-probed video items when proxy mode is
                // enabled.  This ensures proxies are ready before the user selects the
                // clip in the browser; without this, the first preview always uses the
                // full-res original because proxy generation only starts on selection.
                {
                    let prefs = preferences_state.borrow();
                    let proxy_mode = prefs.proxy_mode.clone();
                    if proxy_mode.is_enabled() {
                        let scale = crate::ui::window::proxy_scale_for_mode(&proxy_mode);
                        let lib = library.borrow();
                        let mut pc = proxy_cache.borrow_mut();
                        for path in &resolved {
                            let audio_only = lib.items
                                .iter()
                                .find(|i| i.source_path == *path)
                                .map(|i| i.is_audio_only)
                                .unwrap_or(false);
                            let is_image = lib.items
                                .iter()
                                .find(|i| i.source_path == *path)
                                .map(|i| i.is_image)
                                .unwrap_or(false);
                            if !audio_only && !is_image {
                                pc.request(path, scale.clone(), None);
                            }
                        }
                    }
                }
                // Update drag-source payloads on existing children (avoids full rebuild).
                let entries = flow_box_paths.borrow();
                let lib = library.borrow();
                let mut child_widget = flow_box.first_child();
                let mut idx = 0usize;
                while let Some(w) = child_widget {
                    if let Some(FlowBoxEntry::Media { ref path, .. }) = entries.get(idx) {
                        if let Some(item) = lib.items.iter().find(|i| &i.source_path == path) {
                            let payload = format!("{}|{}", item.source_path, item.duration_ns);
                            let val = glib::Value::from(&payload);
                            for ctrl in w.observe_controllers().into_iter().flatten() {
                                if let Ok(ds) = ctrl.downcast::<gtk::DragSource>() {
                                    ds.set_content(Some(&gdk4::ContentProvider::for_value(&val)));
                                    break;
                                }
                            }
                        }
                    }
                    idx += 1;
                    child_widget = w.next_sibling();
                }
                drop(entries);
                drop(lib);
                drop(cache);
            }

            let lib = library.borrow();
            let has_content = !lib.items.is_empty() || !lib.bins.is_empty();
            empty_hint.set_visible(!has_content);
            import_btn.set_visible(!has_content);
            header_import_btn.set_visible(has_content);
            all_media_btn.set_visible(!lib.bins.is_empty());
            if !flowbox_matches_library_binned(&flow_box_paths.borrow(), &lib, &current_bin_id.borrow(), *show_all_media.borrow()) {
                rebuild_flowbox_binned(&flow_box, &lib, &thumb_cache, &flow_box_paths, &current_bin_id.borrow(), *show_all_media.borrow(), &library);
                rebuild_breadcrumb(&breadcrumb_bar, &lib, &current_bin_id.borrow(), &current_bin_id, &show_all_media, &flow_box, &library, &thumb_cache, &flow_box_paths, &all_media_btn);
            }
            // Start probes for all non-missing library items.
            // probe_cache.request() is a no-op for paths already pending or cached.
            // This covers two cases:
            //   - items from project load with duration_ns == 0 (FCPXML didn't store it)
            //   - items from project load with duration_ns != 0 but is_audio_only/is_image
            //     still at their default (false) — the probe corrects those fields so proxy
            //     pre-generation can make an accurate decision.
            {
                let mut pc = probe_cache.borrow_mut();
                for item in lib.items.iter() {
                    if !item.is_missing {
                        pc.request(&item.source_path);
                    }
                }
            }
            // Pre-generate proxies for all already-probed video items when proxy mode is
            // enabled.  This handles items that were probed in a previous run or that had
            // duration set in the FCPXML (so the probe-resolved loop above never fired for
            // them in this session).  proxy_cache.request() is a no-op for items already
            // pending, completed, or failed.
            {
                let prefs = preferences_state.borrow();
                let proxy_mode = prefs.proxy_mode.clone();
                if proxy_mode.is_enabled() {
                    let scale = crate::ui::window::proxy_scale_for_mode(&proxy_mode);
                    let pc = probe_cache.borrow();
                    let mut pxc = proxy_cache.borrow_mut();
                    for item in lib.items.iter() {
                        if item.is_missing {
                            continue;
                        }
                        if let Some(result) = pc.get(&item.source_path) {
                            if !result.is_audio_only && !result.is_image {
                                pxc.request(&item.source_path, scale, None);
                            }
                        }
                    }
                }
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
        let current_bin_id = current_bin_id.clone();

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
            let current_bin_id = current_bin_id.clone();

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
                            &current_bin_id.borrow(),
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
        let current_bin_id = current_bin_id.clone();
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
                    &current_bin_id.borrow(),
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

    // Wire "All Media" toggle button.
    {
        let library_all = library.clone();
        let flow_box_all = flow_box.clone();
        let thumb_cache_all = thumb_cache.clone();
        let flow_box_paths_all = flow_box_paths.clone();
        let current_bin_id_all = current_bin_id.clone();
        let show_all_media_all = show_all_media.clone();
        let breadcrumb_bar_all = breadcrumb_bar.clone();
        let all_media_btn_all = all_media_btn.clone();
        all_media_btn.connect_clicked(move |_| {
            let is_all = *show_all_media_all.borrow();
            *show_all_media_all.borrow_mut() = !is_all;
            if !is_all {
                // Entering All Media mode — clear bin navigation
                *current_bin_id_all.borrow_mut() = None;
            }
            let lib = library_all.borrow();
            rebuild_flowbox_binned(&flow_box_all, &lib, &thumb_cache_all, &flow_box_paths_all, &current_bin_id_all.borrow(), *show_all_media_all.borrow(), &library_all);
            rebuild_breadcrumb(&breadcrumb_bar_all, &lib, &current_bin_id_all.borrow(), &current_bin_id_all, &show_all_media_all, &flow_box_all, &library_all, &thumb_cache_all, &flow_box_paths_all, &all_media_btn_all);
        });
    }

    let clear_selection: Rc<dyn Fn()> = {
        let flow_box = flow_box.clone();
        Rc::new(move || {
            flow_box.unselect_all();
        })
    };

    let force_rebuild: Rc<dyn Fn()> = {
        let library = library.clone();
        let flow_box = flow_box.clone();
        let thumb_cache = thumb_cache.clone();
        let flow_box_paths = flow_box_paths.clone();
        let header_relink_btn = header_relink_btn.clone();
        let current_bin_id = current_bin_id.clone();
        let show_all_media = show_all_media.clone();
        Rc::new(move || {
            let lib = library.borrow();
            rebuild_flowbox_binned(&flow_box, &lib, &thumb_cache, &flow_box_paths, &current_bin_id.borrow(), *show_all_media.borrow(), &library);
            header_relink_btn.set_visible(false);
        })
    };

    (vbox, clear_selection, force_rebuild)
}

/// Build a single thumbnail grid cell.
fn make_grid_item(
    label: &str,
    path: &str,
    duration_ns: u64,
    is_missing: bool,
    is_audio_only: bool,
    thumb_cache: &Rc<RefCell<ThumbnailCache>>,
) -> FlowBoxChild {
    // Kick off thumbnail loading — only after probe (duration_ns > 0) and only
    // for files that actually have video.  Audio-only files have no video frame
    // to extract; trying causes noisy ffmpeg "Output file does not contain any
    // stream" errors.
    if duration_ns > 0 && !is_audio_only {
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
            if is_audio_only {
                // Dark purple-tinted background for audio-only items.
                cr.set_source_rgb(0.10, 0.08, 0.16);
                cr.rectangle(0.0, 0.0, w as f64, h as f64);
                cr.fill().ok();

                // Draw a music note (eighth note) using Cairo paths — reliable
                // on all systems regardless of font glyph availability.
                let cx = w as f64 / 2.0;
                let cy = h as f64 / 2.0;
                let s = (h.min(w) as f64 / 80.0).clamp(0.5, 1.4);

                cr.set_source_rgb(0.60, 0.45, 0.88);

                // Notehead — filled ellipse, slightly tilted counter-clockwise.
                cr.save().ok();
                cr.translate(cx - 4.0 * s, cy + 10.0 * s);
                cr.rotate(-0.38);
                cr.scale(9.0 * s, 6.0 * s);
                cr.arc(0.0, 0.0, 1.0, 0.0, std::f64::consts::TAU);
                cr.restore().ok();
                cr.fill().ok();

                // Stem — from right edge of notehead, straight up.
                let stem_x = cx + 5.0 * s;
                let stem_bottom = cy + 10.5 * s;
                let stem_top = cy - 16.0 * s;
                cr.set_line_width(2.5 * s);
                cr.move_to(stem_x, stem_bottom);
                cr.line_to(stem_x, stem_top);
                cr.stroke().ok();

                // Flag — a bezier curve sweeping right from the top of the stem.
                cr.move_to(stem_x, stem_top);
                cr.curve_to(
                    stem_x + 14.0 * s, stem_top + 4.0 * s,
                    stem_x + 14.0 * s, stem_top + 14.0 * s,
                    stem_x + 4.0 * s,  stem_top + 18.0 * s,
                );
                cr.stroke().ok();

                return;
            }
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

    let offline_label = Label::new(Some("OFFLINE"));
    offline_label.set_halign(gtk::Align::Center);
    offline_label.add_css_class("media-offline-badge");
    offline_label.set_visible(is_missing);
    cell.append(&offline_label);

    let child = FlowBoxChild::new();
    child.set_child(Some(&cell));
    child.set_tooltip_text(Some(path));
    if is_missing {
        child.add_css_class("media-missing-item");
    }
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

fn flowbox_matches_library_binned(
    current_entries: &[FlowBoxEntry],
    lib: &MediaLibrary,
    current_bin_id: &Option<String>,
    show_all: bool,
) -> bool {
    let expected = build_expected_entries(lib, current_bin_id, show_all);
    if current_entries.len() != expected.len() {
        return false;
    }
    current_entries.iter().zip(expected.iter()).all(|(a, b)| match (a, b) {
        (FlowBoxEntry::Bin { id: id_a, name: name_a }, FlowBoxEntry::Bin { id: id_b, name: name_b }) => {
            id_a == id_b && name_a == name_b
        }
        (FlowBoxEntry::Media { path: p_a, is_missing: m_a, is_audio_only: ao_a },
         FlowBoxEntry::Media { path: p_b, is_missing: m_b, is_audio_only: ao_b }) => {
            p_a == p_b && m_a == m_b && ao_a == ao_b
        }
        _ => false,
    })
}

fn build_expected_entries(
    lib: &MediaLibrary,
    current_bin_id: &Option<String>,
    show_all: bool,
) -> Vec<FlowBoxEntry> {
    let mut entries = Vec::new();
    if show_all {
        // All Media mode: show all items flat, no bins
        for item in lib.items.iter() {
            entries.push(FlowBoxEntry::Media {
                path: item.source_path.clone(),
                is_missing: item.is_missing,
                is_audio_only: item.is_audio_only,
            });
        }
    } else {
        // Show child bins first (alphabetically), then items in current bin
        let mut child_bins = lib.child_bins(current_bin_id.as_deref());
        child_bins.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
        for bin in &child_bins {
            entries.push(FlowBoxEntry::Bin { id: bin.id.clone(), name: bin.name.clone() });
        }
        let items = lib.items_in_bin(current_bin_id.as_deref());
        for item in items {
            entries.push(FlowBoxEntry::Media {
                path: item.source_path.clone(),
                is_missing: item.is_missing,
                is_audio_only: item.is_audio_only,
            });
        }
    }
    entries
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
    library: &Rc<RefCell<MediaLibrary>>,
    flow_box: &FlowBox,
    thumb_cache: &Rc<RefCell<ThumbnailCache>>,
    probe_cache: &Rc<RefCell<MediaProbeCache>>,
    flow_box_paths: &Rc<RefCell<Vec<FlowBoxEntry>>>,
    current_bin_id: &Option<String>,
) -> Option<(String, u64, FlowBoxChild)> {
    if path_str.is_empty() {
        return None;
    }
    // Start background probe (non-blocking). Duration/audio-only updated by 250ms timer.
    probe_cache.borrow_mut().request(&path_str);
    let duration_ns = 0; // placeholder until probe completes
    let mut item = MediaItem::new(path_str.clone(), duration_ns);
    item.bin_id = current_bin_id.clone();
    let label = item.label.clone();
    let is_missing = item.is_missing;
    library.borrow_mut().items.push(item);
    let child = make_grid_item(&label, &path_str, duration_ns, is_missing, false, thumb_cache);
    flow_box.insert(&child, -1);
    flow_box_paths.borrow_mut().push(FlowBoxEntry::Media {
        path: path_str.clone(),
        is_missing,
        is_audio_only: false,
    });
    Some((path_str, duration_ns, child))
}

pub fn parse_external_drop_paths(payload: &str) -> Vec<String> {
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

fn rebuild_flowbox_binned(
    fb: &FlowBox,
    lib: &MediaLibrary,
    thumb_cache: &Rc<RefCell<ThumbnailCache>>,
    flow_box_paths: &Rc<RefCell<Vec<FlowBoxEntry>>>,
    current_bin_id: &Option<String>,
    show_all: bool,
    library_rc: &Rc<RefCell<MediaLibrary>>,
) {
    // Temporarily detach from ScrolledWindow parent to avoid GTK adjustment
    // warnings during bulk child removal/insertion.
    let scroll_parent = fb.parent().and_then(|p| p.downcast::<ScrolledWindow>().ok());
    if let Some(ref sw) = scroll_parent {
        sw.set_child(gtk::Widget::NONE);
    }

    // Remove FlowBoxChild items only (skip non-FlowBoxChild widgets like popovers).
    let mut children_to_remove = Vec::new();
    let mut c = fb.first_child();
    while let Some(widget) = c {
        let next = widget.next_sibling();
        if widget.downcast_ref::<FlowBoxChild>().is_some() {
            children_to_remove.push(widget);
        }
        c = next;
    }
    for child in children_to_remove {
        fb.remove(&child);
    }
    let entries = build_expected_entries(lib, current_bin_id, show_all);
    let mut paths = flow_box_paths.borrow_mut();
    paths.clear();
    for entry in &entries {
        match entry {
            FlowBoxEntry::Bin { id, name } => {
                let child = make_bin_item(name, id, library_rc);
                fb.insert(&child, -1);
            }
            FlowBoxEntry::Media { path, is_missing, is_audio_only } => {
                let item = lib.items.iter().find(|i| &i.source_path == path);
                let duration_ns = item.map(|i| i.duration_ns).unwrap_or(0);
                let label = item.map(|i| i.label.as_str()).unwrap_or("media");
                let child = make_grid_item(label, path, duration_ns, *is_missing, *is_audio_only, thumb_cache);
                fb.insert(&child, -1);
            }
        }
    }
    *paths = entries;

    // Re-attach to ScrolledWindow.
    if let Some(ref sw) = scroll_parent {
        sw.set_child(Some(fb));
    }
}

/// Build a folder icon cell for a bin in the FlowBox.
fn make_bin_item(name: &str, id: &str, library: &Rc<RefCell<MediaLibrary>>) -> FlowBoxChild {
    let cell = GBox::new(Orientation::Vertical, 2);
    cell.set_margin_start(2);
    cell.set_margin_end(2);
    cell.set_margin_top(2);
    cell.set_margin_bottom(2);

    let thumb_area = DrawingArea::new();
    thumb_area.set_content_width(THUMB_W);
    thumb_area.set_content_height(THUMB_H);
    thumb_area.set_draw_func(move |_, cr, w, h| {
        // Dark blue-gray background
        cr.set_source_rgb(0.12, 0.14, 0.20);
        cr.rectangle(0.0, 0.0, w as f64, h as f64);
        cr.fill().ok();

        // Draw folder icon
        let cx = w as f64 / 2.0;
        let cy = h as f64 / 2.0;
        let s = (h.min(w) as f64 / 100.0).clamp(0.6, 1.2);

        // Folder body
        cr.set_source_rgb(0.55, 0.65, 0.80);
        let fw = 40.0 * s;
        let fh = 28.0 * s;
        let fx = cx - fw / 2.0;
        let fy = cy - fh / 2.0 + 4.0 * s;
        let r = 3.0 * s;
        cr.new_path();
        cr.arc(fx + r, fy + r, r, std::f64::consts::PI, 1.5 * std::f64::consts::PI);
        cr.arc(fx + fw - r, fy + r, r, 1.5 * std::f64::consts::PI, 2.0 * std::f64::consts::PI);
        cr.arc(fx + fw - r, fy + fh - r, r, 0.0, 0.5 * std::f64::consts::PI);
        cr.arc(fx + r, fy + fh - r, r, 0.5 * std::f64::consts::PI, std::f64::consts::PI);
        cr.close_path();
        cr.fill().ok();

        // Folder tab
        cr.set_source_rgb(0.50, 0.60, 0.75);
        let tw = 16.0 * s;
        let th = 6.0 * s;
        cr.new_path();
        cr.arc(fx + r, fy - th + r, r, std::f64::consts::PI, 1.5 * std::f64::consts::PI);
        cr.line_to(fx + tw, fy - th);
        cr.line_to(fx + tw + 4.0 * s, fy);
        cr.line_to(fx, fy);
        cr.close_path();
        cr.fill().ok();
    });
    cell.append(&thumb_area);

    let name_label = Label::new(Some(name));
    name_label.set_halign(gtk::Align::Center);
    name_label.set_max_width_chars(22);
    name_label.set_ellipsize(gtk4::pango::EllipsizeMode::End);
    name_label.add_css_class("clip-name");
    name_label.add_css_class("bin-folder-name");
    cell.append(&name_label);

    let child = FlowBoxChild::new();
    child.set_child(Some(&cell));
    child.add_css_class("bin-folder-cell");
    child.set_tooltip_text(Some(name));

    // DropTarget: accept media items dragged onto this bin cell.
    let drop_target = gtk::DropTarget::new(glib::Type::STRING, gdk4::DragAction::COPY);
    let bin_id = id.to_string();
    let library = library.clone();
    drop_target.connect_drop(move |_target, value, _x, _y| {
        let payload = match value.get::<String>() {
            Ok(s) => s,
            Err(_) => return false,
        };
        // Only accept internal "{path}|{duration}" payloads, not external file:// URIs.
        if !payload.contains('|') || payload.contains("file://") {
            return false;
        }
        let source_path = match payload.split('|').next() {
            Some(p) if !p.is_empty() => p.to_string(),
            _ => return false,
        };
        let mut lib = library.borrow_mut();
        if let Some(item) = lib.items.iter_mut().find(|i| i.source_path == source_path) {
            item.bin_id = Some(bin_id.clone());
            true
        } else {
            false
        }
    });
    child.add_controller(drop_target);

    child
}

fn rebuild_breadcrumb(
    bar: &GBox,
    lib: &MediaLibrary,
    current_bin_id: &Option<String>,
    current_bin_id_rc: &Rc<RefCell<Option<String>>>,
    show_all_media_rc: &Rc<RefCell<bool>>,
    flow_box: &FlowBox,
    library_rc: &Rc<RefCell<MediaLibrary>>,
    thumb_cache: &Rc<RefCell<ThumbnailCache>>,
    flow_box_paths: &Rc<RefCell<Vec<FlowBoxEntry>>>,
    all_media_btn: &Button,
) {
    // Remove all existing breadcrumb children.
    // Collect first to avoid infinite loop if remove() fails on a non-child.
    {
        let mut children = Vec::new();
        let mut c = bar.first_child();
        while let Some(widget) = c {
            let next = widget.next_sibling();
            children.push(widget);
            c = next;
        }
        for child in children {
            bar.remove(&child);
        }
    }

    let show_all = *show_all_media_rc.borrow();

    if show_all {
        let lbl = Label::new(Some("All Media"));
        lbl.add_css_class("bin-breadcrumb-active");
        bar.append(&lbl);
        bar.set_visible(true);
        all_media_btn.set_visible(!lib.bins.is_empty());
        return;
    }

    let Some(ref bin_id) = current_bin_id else {
        // At root — hide breadcrumb bar
        bar.set_visible(false);
        all_media_btn.set_visible(!lib.bins.is_empty());
        return;
    };

    bar.set_visible(true);
    all_media_btn.set_visible(!lib.bins.is_empty());

    // "Root" button
    {
        let btn = Button::with_label("Media");
        btn.add_css_class("bin-breadcrumb-btn");
        let current_bin_id_rc = current_bin_id_rc.clone();
        let show_all_media_rc = show_all_media_rc.clone();
        let flow_box = flow_box.clone();
        let library_rc = library_rc.clone();
        let thumb_cache = thumb_cache.clone();
        let flow_box_paths = flow_box_paths.clone();
        let bar_for_closure = bar.clone();
        let all_media_btn = all_media_btn.clone();
        btn.connect_clicked(move |_| {
            *current_bin_id_rc.borrow_mut() = None;
            let lib = library_rc.borrow();
            rebuild_flowbox_binned(&flow_box, &lib, &thumb_cache, &flow_box_paths, &None, false, &library_rc);
            rebuild_breadcrumb(&bar_for_closure, &lib, &None, &current_bin_id_rc, &show_all_media_rc, &flow_box, &library_rc, &thumb_cache, &flow_box_paths, &all_media_btn);
        });
        bar.append(&btn);
    }

    let ancestors = lib.bin_ancestors(bin_id);
    for (i, bin) in ancestors.iter().enumerate() {
        let sep = Label::new(Some(" \u{203a} "));
        sep.add_css_class("bin-breadcrumb-sep");
        bar.append(&sep);

        let is_last = i == ancestors.len() - 1;
        if is_last {
            let lbl = Label::new(Some(&bin.name));
            lbl.add_css_class("bin-breadcrumb-active");
            bar.append(&lbl);
        } else {
            let btn = Button::with_label(&bin.name);
            btn.add_css_class("bin-breadcrumb-btn");
            let target_id = bin.id.clone();
            let current_bin_id_rc = current_bin_id_rc.clone();
            let show_all_media_rc = show_all_media_rc.clone();
            let flow_box = flow_box.clone();
            let library_rc = library_rc.clone();
            let thumb_cache = thumb_cache.clone();
            let flow_box_paths = flow_box_paths.clone();
            let bar_for_closure = bar.clone();
            let all_media_btn = all_media_btn.clone();
            btn.connect_clicked(move |_| {
                *current_bin_id_rc.borrow_mut() = Some(target_id.clone());
                let lib = library_rc.borrow();
                let cid = current_bin_id_rc.borrow().clone();
                rebuild_flowbox_binned(&flow_box, &lib, &thumb_cache, &flow_box_paths, &cid, false, &library_rc);
                rebuild_breadcrumb(&bar_for_closure, &lib, &cid, &current_bin_id_rc, &show_all_media_rc, &flow_box, &library_rc, &thumb_cache, &flow_box_paths, &all_media_btn);
            });
            bar.append(&btn);
        }
    }
}

/// Helper: refresh the flowbox and breadcrumb after a bin operation.
fn refresh_bin_view(
    library: &Rc<RefCell<MediaLibrary>>,
    flow_box: &FlowBox,
    thumb_cache: &Rc<RefCell<ThumbnailCache>>,
    flow_box_paths: &Rc<RefCell<Vec<FlowBoxEntry>>>,
    current_bin_id: &Rc<RefCell<Option<String>>>,
    show_all_media: &Rc<RefCell<bool>>,
    breadcrumb_bar: &GBox,
    all_media_btn: &Button,
) {
    let lib = library.borrow();
    let cid = current_bin_id.borrow().clone();
    let sa = *show_all_media.borrow();
    rebuild_flowbox_binned(flow_box, &lib, thumb_cache, flow_box_paths, &cid, sa, library);
    rebuild_breadcrumb(breadcrumb_bar, &lib, &cid, current_bin_id, show_all_media, flow_box, library, thumb_cache, flow_box_paths, all_media_btn);
}

/// Show a dialog to create a new bin.
#[allow(deprecated)]
fn show_new_bin_dialog(
    widget: &impl gtk4::prelude::IsA<gtk::Widget>,
    parent_id: Option<String>,
    library: &Rc<RefCell<MediaLibrary>>,
    flow_box: &FlowBox,
    thumb_cache: &Rc<RefCell<ThumbnailCache>>,
    flow_box_paths: &Rc<RefCell<Vec<FlowBoxEntry>>>,
    current_bin_id: &Rc<RefCell<Option<String>>>,
    show_all_media: &Rc<RefCell<bool>>,
    breadcrumb_bar: &GBox,
    all_media_btn: &Button,
) {
    let window = flow_box.root().and_then(|r| r.downcast::<gtk::Window>().ok());
    let dialog = gtk::Dialog::with_buttons(
        Some("New Bin"),
        window.as_ref(),
        gtk::DialogFlags::MODAL | gtk::DialogFlags::DESTROY_WITH_PARENT,
        &[("Create", gtk::ResponseType::Accept), ("Cancel", gtk::ResponseType::Cancel)],
    );
    dialog.set_default_response(gtk::ResponseType::Accept);
    let entry = gtk::Entry::new();
    entry.set_placeholder_text(Some("Bin name"));
    entry.set_activates_default(true);
    entry.set_margin_top(8);
    entry.set_margin_bottom(8);
    entry.set_margin_start(8);
    entry.set_margin_end(8);
    dialog.content_area().append(&entry);

    let library = library.clone();
    let flow_box = flow_box.clone();
    let thumb_cache = thumb_cache.clone();
    let flow_box_paths = flow_box_paths.clone();
    let current_bin_id = current_bin_id.clone();
    let show_all_media = show_all_media.clone();
    let breadcrumb_bar = breadcrumb_bar.clone();
    let all_media_btn = all_media_btn.clone();
    dialog.connect_response(move |dlg, response| {
        if response == gtk::ResponseType::Accept {
            let name = entry.text().to_string();
            if !name.is_empty() {
                use crate::model::media_library::MediaBin;
                let bin = MediaBin::new(name, parent_id.clone());
                library.borrow_mut().bins.push(bin);
                refresh_bin_view(&library, &flow_box, &thumb_cache, &flow_box_paths, &current_bin_id, &show_all_media, &breadcrumb_bar, &all_media_btn);
            }
        }
        dlg.close();
    });
    dialog.present();
}

/// Show a dialog to rename a bin.
#[allow(deprecated)]
fn show_rename_dialog(
    widget: &impl gtk4::prelude::IsA<gtk::Widget>,
    bin_id: &str,
    library: &Rc<RefCell<MediaLibrary>>,
    flow_box: &FlowBox,
    thumb_cache: &Rc<RefCell<ThumbnailCache>>,
    flow_box_paths: &Rc<RefCell<Vec<FlowBoxEntry>>>,
    current_bin_id: &Rc<RefCell<Option<String>>>,
    show_all_media: &Rc<RefCell<bool>>,
    breadcrumb_bar: &GBox,
    all_media_btn: &Button,
) {
    let current_name = library.borrow().bins.iter().find(|b| b.id == bin_id).map(|b| b.name.clone()).unwrap_or_default();
    let window = flow_box.root().and_then(|r| r.downcast::<gtk::Window>().ok());
    let dialog = gtk::Dialog::with_buttons(
        Some("Rename Bin"),
        window.as_ref(),
        gtk::DialogFlags::MODAL | gtk::DialogFlags::DESTROY_WITH_PARENT,
        &[("Rename", gtk::ResponseType::Accept), ("Cancel", gtk::ResponseType::Cancel)],
    );
    dialog.set_default_response(gtk::ResponseType::Accept);
    let entry = gtk::Entry::new();
    entry.set_text(&current_name);
    entry.set_activates_default(true);
    entry.set_margin_top(8);
    entry.set_margin_bottom(8);
    entry.set_margin_start(8);
    entry.set_margin_end(8);
    dialog.content_area().append(&entry);

    let bin_id = bin_id.to_string();
    let library = library.clone();
    let flow_box = flow_box.clone();
    let thumb_cache = thumb_cache.clone();
    let flow_box_paths = flow_box_paths.clone();
    let current_bin_id = current_bin_id.clone();
    let show_all_media = show_all_media.clone();
    let breadcrumb_bar = breadcrumb_bar.clone();
    let all_media_btn = all_media_btn.clone();
    dialog.connect_response(move |dlg, response| {
        if response == gtk::ResponseType::Accept {
            let name = entry.text().to_string();
            if !name.is_empty() {
                if let Some(bin) = library.borrow_mut().bins.iter_mut().find(|b| b.id == bin_id) {
                    bin.name = name;
                }
                refresh_bin_view(&library, &flow_box, &thumb_cache, &flow_box_paths, &current_bin_id, &show_all_media, &breadcrumb_bar, &all_media_btn);
            }
        }
        dlg.close();
    });
    dialog.present();
}

/// Delete a bin, moving its items to the parent (or root).
fn delete_bin(
    bin_id: &str,
    library: &Rc<RefCell<MediaLibrary>>,
    flow_box: &FlowBox,
    thumb_cache: &Rc<RefCell<ThumbnailCache>>,
    flow_box_paths: &Rc<RefCell<Vec<FlowBoxEntry>>>,
    current_bin_id: &Rc<RefCell<Option<String>>>,
    show_all_media: &Rc<RefCell<bool>>,
    breadcrumb_bar: &GBox,
    all_media_btn: &Button,
) {
    let mut lib = library.borrow_mut();
    let parent_id = lib.bins.iter().find(|b| b.id == bin_id).and_then(|b| b.parent_id.clone());

    // Move items in this bin to the parent bin (or root)
    for item in lib.items.iter_mut() {
        if item.bin_id.as_deref() == Some(bin_id) {
            item.bin_id = parent_id.clone();
        }
    }

    // Reparent child bins to the parent
    let child_ids: Vec<String> = lib.bins.iter().filter(|b| b.parent_id.as_deref() == Some(bin_id)).map(|b| b.id.clone()).collect();
    for cid in child_ids {
        if let Some(child_bin) = lib.bins.iter_mut().find(|b| b.id == cid) {
            child_bin.parent_id = parent_id.clone();
        }
    }

    // Remove the bin itself
    lib.bins.retain(|b| b.id != bin_id);

    // If we were viewing the deleted bin, go to its parent
    if current_bin_id.borrow().as_deref() == Some(bin_id) {
        *current_bin_id.borrow_mut() = parent_id;
    }

    drop(lib);
    refresh_bin_view(library, flow_box, thumb_cache, flow_box_paths, current_bin_id, show_all_media, breadcrumb_bar, all_media_btn);
}

/// Move media items to a bin (or root if bin_id is None).
fn move_items_to_bin(
    paths: &[String],
    bin_id: Option<String>,
    library: &Rc<RefCell<MediaLibrary>>,
    flow_box: &FlowBox,
    thumb_cache: &Rc<RefCell<ThumbnailCache>>,
    flow_box_paths: &Rc<RefCell<Vec<FlowBoxEntry>>>,
    current_bin_id: &Rc<RefCell<Option<String>>>,
    show_all_media: &Rc<RefCell<bool>>,
    breadcrumb_bar: &GBox,
    all_media_btn: &Button,
) {
    {
        let mut lib = library.borrow_mut();
        for item in lib.items.iter_mut() {
            if paths.contains(&item.source_path) {
                item.bin_id = bin_id.clone();
            }
        }
    }
    refresh_bin_view(library, flow_box, thumb_cache, flow_box_paths, current_bin_id, show_all_media, breadcrumb_bar, all_media_btn);
}
