use crate::media::probe_cache::MediaProbeCache;
use crate::media::proxy_cache::ProxyCache;
use crate::media::thumb_cache::ThumbnailCache;
use crate::model::clip::ClipKind;
use crate::model::media_library::{
    media_auto_tag_state_key, media_auto_tag_summary, media_display_name, media_frame_rate_value,
    media_keyword_summary, media_matches_filters, media_rating_text, media_search_match,
    media_transcript_state_key, non_file_clip_kind_text, normalized_media_text, FrameRateFilter,
    MediaCollection, MediaFilterCriteria, MediaItem, MediaKindFilter, MediaLibrary, MediaRating,
    MediaRatingFilter, MediaSearchField, ResolutionFilter,
};
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
const COLLECTION_NONE_ID: &str = "__none__";

/// Distinguishes bin cells from media cells in the FlowBox.
#[derive(Debug, Clone)]
enum FlowBoxEntry {
    Bin {
        id: String,
        name: String,
    },
    Media {
        item_id: String,
        display_key: String,
    },
}

fn kind_filter_from_active_id(active_id: Option<glib::GString>) -> MediaKindFilter {
    match active_id.as_deref() {
        Some("video") => MediaKindFilter::Video,
        Some("audio") => MediaKindFilter::Audio,
        Some("image") => MediaKindFilter::Image,
        Some("offline") => MediaKindFilter::Offline,
        _ => MediaKindFilter::All,
    }
}

fn kind_filter_active_id(filter: MediaKindFilter) -> &'static str {
    match filter {
        MediaKindFilter::All => "all",
        MediaKindFilter::Video => "video",
        MediaKindFilter::Audio => "audio",
        MediaKindFilter::Image => "image",
        MediaKindFilter::Offline => "offline",
    }
}

fn resolution_filter_from_active_id(active_id: Option<glib::GString>) -> ResolutionFilter {
    match active_id.as_deref() {
        Some("sd") => ResolutionFilter::SdOrSmaller,
        Some("hd") => ResolutionFilter::Hd,
        Some("fhd") => ResolutionFilter::FullHd,
        Some("uhd") => ResolutionFilter::UltraHd,
        _ => ResolutionFilter::All,
    }
}

fn resolution_filter_active_id(filter: ResolutionFilter) -> &'static str {
    match filter {
        ResolutionFilter::All => "all",
        ResolutionFilter::SdOrSmaller => "sd",
        ResolutionFilter::Hd => "hd",
        ResolutionFilter::FullHd => "fhd",
        ResolutionFilter::UltraHd => "uhd",
    }
}

fn frame_rate_filter_from_active_id(active_id: Option<glib::GString>) -> FrameRateFilter {
    match active_id.as_deref() {
        Some("fps24") => FrameRateFilter::Fps24OrLess,
        Some("fps25_30") => FrameRateFilter::Fps25To30,
        Some("fps31_59") => FrameRateFilter::Fps31To59,
        Some("fps60") => FrameRateFilter::Fps60Plus,
        _ => FrameRateFilter::All,
    }
}

fn frame_rate_filter_active_id(filter: FrameRateFilter) -> &'static str {
    match filter {
        FrameRateFilter::All => "all",
        FrameRateFilter::Fps24OrLess => "fps24",
        FrameRateFilter::Fps25To30 => "fps25_30",
        FrameRateFilter::Fps31To59 => "fps31_59",
        FrameRateFilter::Fps60Plus => "fps60",
    }
}

fn rating_filter_from_active_id(active_id: Option<glib::GString>) -> MediaRatingFilter {
    match active_id.as_deref() {
        Some("favorite") => MediaRatingFilter::Favorite,
        Some("reject") => MediaRatingFilter::Reject,
        Some("unrated") => MediaRatingFilter::Unrated,
        _ => MediaRatingFilter::All,
    }
}

fn rating_filter_active_id(filter: MediaRatingFilter) -> &'static str {
    match filter {
        MediaRatingFilter::All => "all",
        MediaRatingFilter::Favorite => "favorite",
        MediaRatingFilter::Reject => "reject",
        MediaRatingFilter::Unrated => "unrated",
    }
}

fn collection_display_label(collection: &MediaCollection) -> String {
    collection.name.clone()
}

fn selected_collection_id(combo: &gtk::ComboBoxText) -> Option<String> {
    combo.active_id().and_then(|id| {
        let id = id.to_string();
        (id != COLLECTION_NONE_ID).then_some(id)
    })
}

fn refresh_collection_picker(
    combo: &gtk::ComboBoxText,
    rename_btn: &Button,
    delete_btn: &Button,
    lib: &MediaLibrary,
) {
    let active_id = selected_collection_id(combo);
    combo.remove_all();
    combo.append(Some(COLLECTION_NONE_ID), "Collections");
    for collection in &lib.collections {
        combo.append(
            Some(collection.id.as_str()),
            collection_display_label(collection).as_str(),
        );
    }
    let active_id = active_id
        .filter(|id| {
            lib.collections
                .iter()
                .any(|collection| &collection.id == id)
        })
        .unwrap_or_else(|| COLLECTION_NONE_ID.to_string());
    combo.set_active_id(Some(&active_id));
    let has_selected_collection = active_id != COLLECTION_NONE_ID;
    rename_btn.set_sensitive(has_selected_collection);
    delete_btn.set_sensitive(has_selected_collection);
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
    on_reverse_match_frame: Rc<dyn Fn(String)>,
    on_relink_media: Rc<dyn Fn()>,
    on_create_multicam_from_browser: Rc<dyn Fn(Vec<String>)>,
    on_library_changed: Rc<dyn Fn()>,
    // Given a set of source paths about to be removed from the
    // library, return how many timeline clips reference them. Used
    // to surface a "N timeline clips will go offline" warning in
    // the Remove-from-Library confirmation dialog. Implemented in
    // window.rs by walking Project::tracks recursively (including
    // compound_tracks).
    on_check_library_usage: Rc<dyn Fn(&[String]) -> usize>,
    // Kick off Convert LTC Audio to Timecode for a library item.
    // Runs the decode on a background thread (same pipeline as the
    // timeline-clip action) and populates source_timecode_base_ns
    // on the matching MediaItem + any timeline clips using the
    // same source when it finishes.
    on_convert_library_ltc: Rc<dyn Fn(String)>,
    // Create a subclip from the source monitor's current In/Out
    // marks against the given parent library item id. Invoked from
    // the right-click menu when the user has exactly one top-level
    // item selected that matches the source-monitor path AND the
    // marks are valid. The callback is responsible for reading
    // SourceMarks, constructing a MediaItem via
    // MediaItem::new_subclip, pushing it onto the library, and
    // firing on_library_changed.
    on_create_subclip_from_marks: Rc<dyn Fn(String)>,
    // Place the given library source paths on the timeline at
    // positions aligned by their decoded source timecode. Invoked
    // from a dedicated button shown only when 2+ library items are
    // selected and all of them have decoded TC. Implemented in
    // window.rs by computing anchor = earliest TC, then
    // new_timeline_start = (item.tc - anchor.tc) for each.
    on_place_aligned_by_timecode: Rc<dyn Fn(Vec<String>)>,
    proxy_cache: Rc<RefCell<ProxyCache>>,
    preferences_state: Rc<RefCell<PreferencesState>>,
) -> (GBox, Rc<dyn Fn()>, Rc<dyn Fn()>) {
    let vbox = GBox::new(Orientation::Vertical, 4);
    vbox.set_width_request(240);
    let has_library_content = {
        let lib = library.borrow();
        !lib.items.is_empty() || !lib.bins.is_empty()
    };

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
    header_import_btn.set_visible(has_library_content);
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

    let filters: Rc<RefCell<MediaFilterCriteria>> =
        Rc::new(RefCell::new(MediaFilterCriteria::default()));
    let applying_collection: Rc<Cell<bool>> = Rc::new(Cell::new(false));

    let filter_box = GBox::new(Orientation::Vertical, 4);
    filter_box.set_margin_start(8);
    filter_box.set_margin_end(8);
    filter_box.set_margin_bottom(2);
    filter_box.add_css_class("media-filter-box");
    filter_box.set_visible(has_library_content);

    let filter_search = gtk::SearchEntry::new();
    filter_search.set_placeholder_text(Some("Filter name, path, codec, or keyword"));
    filter_search.add_css_class("media-filter-search");
    filter_box.append(&filter_search);

    let collection_row = GBox::new(Orientation::Horizontal, 4);

    let collection_filter = gtk::ComboBoxText::new();
    collection_filter.append(Some(COLLECTION_NONE_ID), "Collections");
    collection_filter.set_active_id(Some(COLLECTION_NONE_ID));
    collection_filter.set_hexpand(true);
    collection_row.append(&collection_filter);

    let save_collection_btn = Button::from_icon_name("document-save-symbolic");
    save_collection_btn.add_css_class("browser-header-import");
    save_collection_btn.set_tooltip_text(Some("Save the current filters as a smart collection"));
    collection_row.append(&save_collection_btn);

    let rename_collection_btn = Button::from_icon_name("document-edit-symbolic");
    rename_collection_btn.add_css_class("browser-header-import");
    rename_collection_btn.set_tooltip_text(Some("Rename the selected smart collection"));
    rename_collection_btn.set_sensitive(false);
    collection_row.append(&rename_collection_btn);

    let delete_collection_btn = Button::from_icon_name("user-trash-symbolic");
    delete_collection_btn.add_css_class("browser-header-import");
    delete_collection_btn.set_tooltip_text(Some("Delete the selected smart collection"));
    delete_collection_btn.set_sensitive(false);
    collection_row.append(&delete_collection_btn);

    filter_box.append(&collection_row);

    let filter_row = GBox::new(Orientation::Horizontal, 4);

    let kind_filter = gtk::ComboBoxText::new();
    kind_filter.append(Some("all"), "All Types");
    kind_filter.append(Some("video"), "Video");
    kind_filter.append(Some("audio"), "Audio");
    kind_filter.append(Some("image"), "Images");
    kind_filter.append(Some("offline"), "Offline");
    kind_filter.set_active_id(Some("all"));
    kind_filter.set_hexpand(true);
    filter_row.append(&kind_filter);

    let resolution_filter = gtk::ComboBoxText::new();
    resolution_filter.append(Some("all"), "All Sizes");
    resolution_filter.append(Some("sd"), "SD or smaller");
    resolution_filter.append(Some("hd"), "HD");
    resolution_filter.append(Some("fhd"), "Full HD");
    resolution_filter.append(Some("uhd"), "4K+");
    resolution_filter.set_active_id(Some("all"));
    resolution_filter.set_hexpand(true);
    filter_row.append(&resolution_filter);

    let frame_rate_filter = gtk::ComboBoxText::new();
    frame_rate_filter.append(Some("all"), "All FPS");
    frame_rate_filter.append(Some("fps24"), "24 fps or less");
    frame_rate_filter.append(Some("fps25_30"), "25-30 fps");
    frame_rate_filter.append(Some("fps31_59"), "31-59 fps");
    frame_rate_filter.append(Some("fps60"), "60+ fps");
    frame_rate_filter.set_active_id(Some("all"));
    frame_rate_filter.set_hexpand(true);
    filter_row.append(&frame_rate_filter);

    let rating_filter = gtk::ComboBoxText::new();
    rating_filter.append(Some("all"), "All Ratings");
    rating_filter.append(Some("favorite"), "Favorite");
    rating_filter.append(Some("reject"), "Reject");
    rating_filter.append(Some("unrated"), "Unrated");
    rating_filter.set_active_id(Some("all"));
    rating_filter.set_hexpand(true);
    filter_row.append(&rating_filter);

    filter_box.append(&filter_row);
    vbox.append(&filter_box);

    let content_stack = gtk::Stack::new();
    content_stack.set_vexpand(true);
    content_stack.set_hexpand(true);

    let empty_state_box = GBox::new(Orientation::Vertical, 10);
    empty_state_box.set_vexpand(true);
    empty_state_box.set_hexpand(true);
    empty_state_box.set_valign(gtk::Align::Center);
    empty_state_box.set_halign(gtk::Align::Center);
    empty_state_box.set_margin_start(24);
    empty_state_box.set_margin_end(24);
    empty_state_box.set_margin_top(24);
    empty_state_box.set_margin_bottom(24);

    let import_btn = Button::with_label("Import Media…");
    import_btn.add_css_class("suggested-action");
    import_btn.set_halign(gtk::Align::Center);
    import_btn.set_width_request(180);
    empty_state_box.append(&import_btn);

    let empty_hint = Label::new(Some(
        "Import video, audio, or image files here, or drag files directly into the Media Library to start editing.",
    ));
    empty_hint.set_halign(gtk::Align::Center);
    empty_hint.set_justify(gtk::Justification::Center);
    empty_hint.set_xalign(0.5);
    empty_hint.set_wrap(true);
    empty_hint.set_max_width_chars(36);
    empty_hint.add_css_class("panel-empty-state");
    empty_state_box.append(&empty_hint);
    content_stack.add_named(&empty_state_box, Some("empty"));

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
    let refresh_collection_controls: Rc<dyn Fn()> = {
        let library = library.clone();
        let collection_filter = collection_filter.clone();
        let rename_collection_btn = rename_collection_btn.clone();
        let delete_collection_btn = delete_collection_btn.clone();
        Rc::new(move || {
            let lib = library.borrow();
            refresh_collection_picker(
                &collection_filter,
                &rename_collection_btn,
                &delete_collection_btn,
                &lib,
            );
        })
    };
    let refresh_filtered_view: Rc<dyn Fn()> = {
        let library = library.clone();
        let flow_box = flow_box.clone();
        let thumb_cache = thumb_cache.clone();
        let flow_box_paths = flow_box_paths.clone();
        let current_bin_id = current_bin_id.clone();
        let show_all_media = show_all_media.clone();
        let filters = filters.clone();
        let on_library_changed = on_library_changed.clone();
        Rc::new(move || {
            let lib = library.borrow();
            let current_filters = filters.borrow().clone();
            rebuild_flowbox_binned(
                &flow_box,
                &lib,
                &thumb_cache,
                &flow_box_paths,
                &current_bin_id.borrow(),
                *show_all_media.borrow(),
                &current_filters,
                &library,
                &on_library_changed,
            );
        })
    };
    let refresh_browser_view: Rc<dyn Fn()> = {
        let library = library.clone();
        let flow_box = flow_box.clone();
        let thumb_cache = thumb_cache.clone();
        let flow_box_paths = flow_box_paths.clone();
        let current_bin_id = current_bin_id.clone();
        let show_all_media = show_all_media.clone();
        let breadcrumb_bar = breadcrumb_bar.clone();
        let all_media_btn = all_media_btn.clone();
        let filters = filters.clone();
        let on_library_changed = on_library_changed.clone();
        Rc::new(move || {
            refresh_bin_view(
                &library,
                &flow_box,
                &thumb_cache,
                &flow_box_paths,
                &current_bin_id,
                &show_all_media,
                &breadcrumb_bar,
                &all_media_btn,
                &filters,
                &on_library_changed,
            );
        })
    };

    {
        let filters = filters.clone();
        let refresh_filtered_view = refresh_filtered_view.clone();
        let collection_filter = collection_filter.clone();
        let applying_collection = applying_collection.clone();
        filter_search.connect_search_changed(move |entry| {
            filters.borrow_mut().search_text = entry.text().trim().to_string();
            if !applying_collection.get() {
                collection_filter.set_active_id(Some(COLLECTION_NONE_ID));
            }
            refresh_filtered_view();
        });
    }
    {
        let filters = filters.clone();
        let refresh_filtered_view = refresh_filtered_view.clone();
        let collection_filter = collection_filter.clone();
        let applying_collection = applying_collection.clone();
        kind_filter.connect_changed(move |combo| {
            filters.borrow_mut().kind = kind_filter_from_active_id(combo.active_id());
            if !applying_collection.get() {
                collection_filter.set_active_id(Some(COLLECTION_NONE_ID));
            }
            refresh_filtered_view();
        });
    }
    {
        let filters = filters.clone();
        let refresh_filtered_view = refresh_filtered_view.clone();
        let collection_filter = collection_filter.clone();
        let applying_collection = applying_collection.clone();
        resolution_filter.connect_changed(move |combo| {
            filters.borrow_mut().resolution = resolution_filter_from_active_id(combo.active_id());
            if !applying_collection.get() {
                collection_filter.set_active_id(Some(COLLECTION_NONE_ID));
            }
            refresh_filtered_view();
        });
    }
    {
        let filters = filters.clone();
        let refresh_filtered_view = refresh_filtered_view.clone();
        let collection_filter = collection_filter.clone();
        let applying_collection = applying_collection.clone();
        frame_rate_filter.connect_changed(move |combo| {
            filters.borrow_mut().frame_rate = frame_rate_filter_from_active_id(combo.active_id());
            if !applying_collection.get() {
                collection_filter.set_active_id(Some(COLLECTION_NONE_ID));
            }
            refresh_filtered_view();
        });
    }
    {
        let filters = filters.clone();
        let refresh_filtered_view = refresh_filtered_view.clone();
        let collection_filter = collection_filter.clone();
        let applying_collection = applying_collection.clone();
        rating_filter.connect_changed(move |combo| {
            filters.borrow_mut().rating = rating_filter_from_active_id(combo.active_id());
            if !applying_collection.get() {
                collection_filter.set_active_id(Some(COLLECTION_NONE_ID));
            }
            refresh_filtered_view();
        });
    }
    {
        let library = library.clone();
        let filters = filters.clone();
        let applying_collection = applying_collection.clone();
        let filter_search = filter_search.clone();
        let kind_filter = kind_filter.clone();
        let resolution_filter = resolution_filter.clone();
        let frame_rate_filter = frame_rate_filter.clone();
        let rating_filter = rating_filter.clone();
        let current_bin_id = current_bin_id.clone();
        let show_all_media = show_all_media.clone();
        let refresh_browser_view = refresh_browser_view.clone();
        let rename_collection_btn = rename_collection_btn.clone();
        let delete_collection_btn = delete_collection_btn.clone();
        collection_filter.connect_changed(move |combo| {
            let Some(collection_id) = selected_collection_id(combo) else {
                rename_collection_btn.set_sensitive(false);
                delete_collection_btn.set_sensitive(false);
                return;
            };
            let Some(collection) = library.borrow().find_collection(&collection_id).cloned() else {
                combo.set_active_id(Some(COLLECTION_NONE_ID));
                rename_collection_btn.set_sensitive(false);
                delete_collection_btn.set_sensitive(false);
                return;
            };
            rename_collection_btn.set_sensitive(true);
            delete_collection_btn.set_sensitive(true);
            applying_collection.set(true);
            *filters.borrow_mut() = collection.criteria.clone();
            filter_search.set_text(&collection.criteria.search_text);
            kind_filter.set_active_id(Some(kind_filter_active_id(collection.criteria.kind)));
            resolution_filter.set_active_id(Some(resolution_filter_active_id(
                collection.criteria.resolution,
            )));
            frame_rate_filter.set_active_id(Some(frame_rate_filter_active_id(
                collection.criteria.frame_rate,
            )));
            rating_filter.set_active_id(Some(rating_filter_active_id(collection.criteria.rating)));
            applying_collection.set(false);
            *current_bin_id.borrow_mut() = None;
            *show_all_media.borrow_mut() = true;
            refresh_browser_view();
        });
    }
    {
        let library = library.clone();
        let filters = filters.clone();
        let collection_filter = collection_filter.clone();
        let refresh_browser_view = refresh_browser_view.clone();
        let refresh_collection_controls = refresh_collection_controls.clone();
        let on_library_changed = on_library_changed.clone();
        save_collection_btn.connect_clicked(move |_| {
            show_new_collection_dialog(
                &collection_filter,
                &library,
                filters.borrow().clone(),
                &refresh_browser_view,
                &refresh_collection_controls,
                &on_library_changed,
            );
        });
    }
    {
        let library = library.clone();
        let collection_filter = collection_filter.clone();
        let refresh_browser_view = refresh_browser_view.clone();
        let refresh_collection_controls = refresh_collection_controls.clone();
        let on_library_changed = on_library_changed.clone();
        rename_collection_btn.connect_clicked(move |_| {
            if let Some(collection_id) = selected_collection_id(&collection_filter) {
                show_rename_collection_dialog(
                    &collection_filter,
                    &collection_id,
                    &library,
                    &refresh_browser_view,
                    &refresh_collection_controls,
                    &on_library_changed,
                );
            }
        });
    }
    {
        let library = library.clone();
        let collection_filter = collection_filter.clone();
        let refresh_browser_view = refresh_browser_view.clone();
        let refresh_collection_controls = refresh_collection_controls.clone();
        let on_library_changed = on_library_changed.clone();
        delete_collection_btn.connect_clicked(move |_| {
            if let Some(collection_id) = selected_collection_id(&collection_filter) {
                delete_collection(
                    &collection_filter,
                    &collection_id,
                    &library,
                    &refresh_browser_view,
                    &refresh_collection_controls,
                    &on_library_changed,
                );
            }
        });
    }
    refresh_collection_controls();

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
        let filters_click = filters.clone();
        let collection_filter_click = collection_filter.clone();
        let on_library_changed_click = on_library_changed.clone();
        let dbl_click = gtk::GestureClick::new();
        dbl_click.set_button(1);
        dbl_click.set_propagation_phase(gtk::PropagationPhase::Capture);
        dbl_click.connect_pressed(move |gesture, n_press, x, y| {
            if n_press != 2 {
                return;
            }
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
                    collection_filter_click.set_active_id(Some(COLLECTION_NONE_ID));
                    let lib = library_click.borrow();
                    rebuild_flowbox_binned(
                        &flow_box_click,
                        &lib,
                        &thumb_cache_click,
                        &flow_box_paths_click,
                        &current_bin_id_click.borrow(),
                        false,
                        &filters_click.borrow(),
                        &library_click,
                        &on_library_changed_click,
                    );
                    rebuild_breadcrumb(
                        &breadcrumb_bar_click,
                        &lib,
                        &current_bin_id_click.borrow(),
                        &current_bin_id_click,
                        &show_all_media_click,
                        &flow_box_click,
                        &library_click,
                        &thumb_cache_click,
                        &flow_box_paths_click,
                        &all_media_btn_click,
                        &filters_click,
                        &on_library_changed_click,
                    );
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
            if n_press != 1 {
                return;
            }
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
    content_stack.add_named(&scroll, Some("library"));
    content_stack.set_visible_child_name(if has_library_content {
        "library"
    } else {
        "empty"
    });
    vbox.append(&content_stack);

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

    // "Place Aligned by Timecode" — parallel to Create Multicam
    // Clip. Visible when 2+ library items are selected and every
    // one has decoded source timecode. Drops them on a fresh
    // timeline lane at positions aligned by their decoded TC
    // (earliest-TC member becomes the anchor at timeline_start=0;
    // others shift by TC delta). Lets users skip the
    // select-both-on-timeline → right-click → Sync dance when all
    // the TC is already in the library.
    let place_aligned_btn = Button::with_label("Place Aligned by Timecode");
    place_aligned_btn.add_css_class("suggested-action");
    place_aligned_btn.set_margin_start(8);
    place_aligned_btn.set_margin_end(8);
    place_aligned_btn.set_margin_bottom(4);
    place_aligned_btn.set_visible(false);
    vbox.append(&place_aligned_btn);

    // Populate from existing library items (e.g. after project load).
    rebuild_flowbox_binned(
        &flow_box,
        &library.borrow(),
        &thumb_cache,
        &flow_box_paths,
        &current_bin_id.borrow(),
        *show_all_media.borrow(),
        &filters.borrow(),
        &library,
        &on_library_changed,
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
        let filters_ctx = filters.clone();
        let on_library_changed_ctx = on_library_changed.clone();
        let on_reverse_match_frame_ctx = on_reverse_match_frame.clone();
        let on_check_library_usage_ctx = on_check_library_usage.clone();
        let on_convert_library_ltc_ctx = on_convert_library_ltc.clone();
        let on_create_subclip_from_marks_ctx = on_create_subclip_from_marks.clone();
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

            // If the user right-clicked a media item that isn't
            // part of the current selection, select it first.
            // Matches standard desktop UX (Finder / FCP / etc.) —
            // without this step, right-click actions like
            // "Create Subclip from In/Out…" / "Convert LTC" /
            // "Remove from Library" silently skip because they
            // key off `flow_box.selected_children()` and the
            // clicked-but-unselected item never reaches the
            // media-actions branch. Preserves multi-select:
            // right-click inside an existing multi-selection
            // doesn't collapse it.
            if let (Some(ref fbc), Some(FlowBoxEntry::Media { .. })) =
                (clicked_child.as_ref(), clicked_entry.as_ref())
            {
                let already_selected = flow_box_ctx
                    .selected_children()
                    .iter()
                    .any(|c| c.index() == fbc.index());
                if !already_selected {
                    flow_box_ctx.unselect_all();
                    flow_box_ctx.select_child(*fbc);
                }
            }

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
                        let filters = filters_ctx.clone();
                        let popover = popover.clone();
                        let on_library_changed = on_library_changed_ctx.clone();
                        open_btn.connect_clicked(move |_| {
                            popover.popdown();
                            *current_bin_id.borrow_mut() = Some(bin_id.clone());
                            *show_all_media.borrow_mut() = false;
                            let lib = library.borrow();
                            rebuild_flowbox_binned(
                                &flow_box,
                                &lib,
                                &thumb_cache,
                                &flow_box_paths,
                                &current_bin_id.borrow(),
                                false,
                                &filters.borrow(),
                                &library,
                                &on_library_changed,
                            );
                            rebuild_breadcrumb(
                                &breadcrumb_bar,
                                &lib,
                                &current_bin_id.borrow(),
                                &current_bin_id,
                                &show_all_media,
                                &flow_box,
                                &library,
                                &thumb_cache,
                                &flow_box_paths,
                                &all_media_btn,
                                &filters,
                                &on_library_changed,
                            );
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
                        let filters = filters_ctx.clone();
                        let popover = popover.clone();
                        let on_library_changed = on_library_changed_ctx.clone();
                        rename_btn.connect_clicked(move |btn| {
                            popover.popdown();
                            show_rename_dialog(
                                btn,
                                &bin_id,
                                &library,
                                &flow_box,
                                &thumb_cache,
                                &flow_box_paths,
                                &current_bin_id,
                                &show_all_media,
                                &breadcrumb_bar,
                                &all_media_btn,
                                &filters,
                                &on_library_changed,
                            );
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
                        let filters = filters_ctx.clone();
                        let popover = popover.clone();
                        let on_library_changed = on_library_changed_ctx.clone();
                        delete_btn.connect_clicked(move |_| {
                            popover.popdown();
                            delete_bin(
                                &bin_id,
                                &library,
                                &flow_box,
                                &thumb_cache,
                                &flow_box_paths,
                                &current_bin_id,
                                &show_all_media,
                                &breadcrumb_bar,
                                &all_media_btn,
                                &filters,
                                &on_library_changed,
                            );
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
                                let filters = filters_ctx.clone();
                                let popover = popover.clone();
                                let on_library_changed = on_library_changed_ctx.clone();
                                sub_btn.connect_clicked(move |btn| {
                                    popover.popdown();
                                    show_new_bin_dialog(
                                        btn,
                                        Some(parent_id.clone()),
                                        &library,
                                        &flow_box,
                                        &thumb_cache,
                                        &flow_box_paths,
                                        &current_bin_id,
                                        &show_all_media,
                                        &breadcrumb_bar,
                                        &all_media_btn,
                                        &filters,
                                        &on_library_changed,
                                    );
                                });
                            }
                        }
                    }
                }
                Some(FlowBoxEntry::Media { .. }) => {
                    // Right-clicked on a media item — "Move to Bin" submenu
                    let selected = flow_box_ctx.selected_children();
                    let entries = flow_box_paths_ctx.borrow();
                    let selected_ids: Vec<String> = selected
                        .iter()
                        .filter_map(|c| {
                            let idx = c.index() as usize;
                            match entries.get(idx) {
                                Some(FlowBoxEntry::Media { item_id, .. }) => Some(item_id.clone()),
                                _ => None,
                            }
                        })
                        .collect();
                    drop(entries);

                    if !selected_ids.is_empty() {
                        let reverse_match_source_path = if selected_ids.len() == 1 {
                            let lib = library_ctx.borrow();
                            lib.items
                                .iter()
                                .find(|item| item.id == selected_ids[0] && item.has_backing_file())
                                .map(|item| item.source_path.clone())
                        } else {
                            None
                        };
                        if let Some(source_path) = reverse_match_source_path {
                            let reverse_match_btn =
                                add_menu_item(&menu_box, "Reverse Match Frame…");
                            let popover = popover.clone();
                            let on_reverse_match_frame = on_reverse_match_frame_ctx.clone();
                            reverse_match_btn.connect_clicked(move |_| {
                                popover.popdown();
                                on_reverse_match_frame(source_path.clone());
                            });
                        }

                        let favorite_btn = add_menu_item(&menu_box, "Mark Favorite");
                        {
                            let item_ids = selected_ids.clone();
                            let library = library_ctx.clone();
                            let flow_box = flow_box_ctx.clone();
                            let thumb_cache = thumb_cache_ctx.clone();
                            let flow_box_paths = flow_box_paths_ctx.clone();
                            let current_bin_id = current_bin_id_ctx.clone();
                            let show_all_media = show_all_media_ctx.clone();
                            let breadcrumb_bar = breadcrumb_bar_ctx.clone();
                            let all_media_btn = all_media_btn_ctx.clone();
                            let filters = filters_ctx.clone();
                            let popover = popover.clone();
                            let on_library_changed = on_library_changed_ctx.clone();
                            favorite_btn.connect_clicked(move |_| {
                                popover.popdown();
                                set_items_rating(
                                    &item_ids,
                                    MediaRating::Favorite,
                                    &library,
                                    &flow_box,
                                    &thumb_cache,
                                    &flow_box_paths,
                                    &current_bin_id,
                                    &show_all_media,
                                    &breadcrumb_bar,
                                    &all_media_btn,
                                    &filters,
                                    &on_library_changed,
                                );
                            });
                        }

                        let reject_btn = add_menu_item(&menu_box, "Mark Reject");
                        {
                            let item_ids = selected_ids.clone();
                            let library = library_ctx.clone();
                            let flow_box = flow_box_ctx.clone();
                            let thumb_cache = thumb_cache_ctx.clone();
                            let flow_box_paths = flow_box_paths_ctx.clone();
                            let current_bin_id = current_bin_id_ctx.clone();
                            let show_all_media = show_all_media_ctx.clone();
                            let breadcrumb_bar = breadcrumb_bar_ctx.clone();
                            let all_media_btn = all_media_btn_ctx.clone();
                            let filters = filters_ctx.clone();
                            let popover = popover.clone();
                            let on_library_changed = on_library_changed_ctx.clone();
                            reject_btn.connect_clicked(move |_| {
                                popover.popdown();
                                set_items_rating(
                                    &item_ids,
                                    MediaRating::Reject,
                                    &library,
                                    &flow_box,
                                    &thumb_cache,
                                    &flow_box_paths,
                                    &current_bin_id,
                                    &show_all_media,
                                    &breadcrumb_bar,
                                    &all_media_btn,
                                    &filters,
                                    &on_library_changed,
                                );
                            });
                        }

                        let clear_rating_btn = add_menu_item(&menu_box, "Clear Rating");
                        {
                            let item_ids = selected_ids.clone();
                            let library = library_ctx.clone();
                            let flow_box = flow_box_ctx.clone();
                            let thumb_cache = thumb_cache_ctx.clone();
                            let flow_box_paths = flow_box_paths_ctx.clone();
                            let current_bin_id = current_bin_id_ctx.clone();
                            let show_all_media = show_all_media_ctx.clone();
                            let breadcrumb_bar = breadcrumb_bar_ctx.clone();
                            let all_media_btn = all_media_btn_ctx.clone();
                            let filters = filters_ctx.clone();
                            let popover = popover.clone();
                            let on_library_changed = on_library_changed_ctx.clone();
                            clear_rating_btn.connect_clicked(move |_| {
                                popover.popdown();
                                set_items_rating(
                                    &item_ids,
                                    MediaRating::None,
                                    &library,
                                    &flow_box,
                                    &thumb_cache,
                                    &flow_box_paths,
                                    &current_bin_id,
                                    &show_all_media,
                                    &breadcrumb_bar,
                                    &all_media_btn,
                                    &filters,
                                    &on_library_changed,
                                );
                            });
                        }

                        // Create Subclip from Source Monitor Marks —
                        // single-select only, the item must not
                        // already be a subclip (we don't nest
                        // subclips; a range-of-a-range is redundant
                        // and makes the parent_id graph deeper than
                        // needed), and the source monitor must be
                        // showing THIS item with valid In < Out.
                        // When the button is visible the user knows
                        // the preconditions were already met; the
                        // window.rs-side callback re-checks everything
                        // defensively.
                        if selected_ids.len() == 1 {
                            let subclip_source_id: Option<String> = {
                                let lib = library_ctx.borrow();
                                lib.items
                                    .iter()
                                    .find(|i| {
                                        i.id == selected_ids[0]
                                            && !i.is_subclip()
                                            && i.has_backing_file()
                                    })
                                    .map(|i| i.id.clone())
                            };
                            if let Some(parent_id) = subclip_source_id {
                                let subclip_btn = add_menu_item(
                                    &menu_box,
                                    "Create Subclip from In/Out…",
                                );
                                subclip_btn.set_tooltip_text(Some(
                                    "Create a virtual subclip from the source monitor's \
                                     Mark In / Mark Out. The subclip shares the parent's \
                                     source file but carries its own In/Out window, label, \
                                     and timecode — drop it onto the timeline and it lands \
                                     with the window already applied.",
                                ));
                                let popover = popover.clone();
                                let on_create_subclip =
                                    on_create_subclip_from_marks_ctx.clone();
                                subclip_btn.connect_clicked(move |_| {
                                    popover.popdown();
                                    on_create_subclip(parent_id.clone());
                                });
                            }
                        }

                        // Convert LTC Audio to Timecode — single-select
                        // only, requires a backing file with an audio
                        // stream. Populates source_timecode_base_ns on
                        // the matching MediaItem (and any already-on-
                        // timeline clips using that source). Lets the
                        // user pre-decode before dragging clips in.
                        let convert_ltc_path: Option<String> = if selected_ids.len() == 1 {
                            let lib = library_ctx.borrow();
                            lib.items
                                .iter()
                                .find(|i| {
                                    i.id == selected_ids[0]
                                        && i.has_audio
                                        && i.has_backing_file()
                                })
                                .map(|i| i.source_path.clone())
                        } else {
                            None
                        };
                        if let Some(source_path) = convert_ltc_path {
                            let convert_btn =
                                add_menu_item(&menu_box, "Convert LTC Audio to Timecode…");
                            convert_btn.set_tooltip_text(Some(
                                "Decode LTC from this item's audio and store it as \
                                 source timecode metadata. Works before the item is on the timeline.",
                            ));
                            let popover = popover.clone();
                            let on_convert_library_ltc =
                                on_convert_library_ltc_ctx.clone();
                            convert_btn.connect_clicked(move |_| {
                                popover.popdown();
                                on_convert_library_ltc(source_path.clone());
                            });
                        }

                        // Remove from Library — also surfaces a
                        // confirmation dialog when timeline clips
                        // reference the item(s), so users don't
                        // accidentally orphan clips on the timeline.
                        let remove_btn = add_menu_item(&menu_box, "Remove from Library");
                        {
                            let item_ids = selected_ids.clone();
                            let library = library_ctx.clone();
                            let flow_box = flow_box_ctx.clone();
                            let thumb_cache = thumb_cache_ctx.clone();
                            let flow_box_paths = flow_box_paths_ctx.clone();
                            let current_bin_id = current_bin_id_ctx.clone();
                            let show_all_media = show_all_media_ctx.clone();
                            let breadcrumb_bar = breadcrumb_bar_ctx.clone();
                            let all_media_btn = all_media_btn_ctx.clone();
                            let filters = filters_ctx.clone();
                            let popover = popover.clone();
                            let on_library_changed = on_library_changed_ctx.clone();
                            let on_check_library_usage =
                                on_check_library_usage_ctx.clone();
                            remove_btn.connect_clicked(move |btn| {
                                popover.popdown();
                                let parent_window = btn
                                    .root()
                                    .and_then(|r| r.downcast::<gtk::Window>().ok());
                                remove_items_from_library_with_confirm(
                                    &item_ids,
                                    &library,
                                    &flow_box,
                                    &thumb_cache,
                                    &flow_box_paths,
                                    &current_bin_id,
                                    &show_all_media,
                                    &breadcrumb_bar,
                                    &all_media_btn,
                                    &filters,
                                    &on_library_changed,
                                    &on_check_library_usage,
                                    parent_window.as_ref(),
                                );
                            });
                        }

                        let lib = library_ctx.borrow();
                        if !lib.bins.is_empty() {
                            // "Move to Root" option
                            let root_btn = add_menu_item(&menu_box, "Move to Root");
                            {
                                let item_ids = selected_ids.clone();
                                let library = library_ctx.clone();
                                let flow_box = flow_box_ctx.clone();
                                let thumb_cache = thumb_cache_ctx.clone();
                                let flow_box_paths = flow_box_paths_ctx.clone();
                                let current_bin_id = current_bin_id_ctx.clone();
                                let show_all_media = show_all_media_ctx.clone();
                                let breadcrumb_bar = breadcrumb_bar_ctx.clone();
                                let all_media_btn = all_media_btn_ctx.clone();
                                let filters = filters_ctx.clone();
                                let popover = popover.clone();
                                let on_library_changed = on_library_changed_ctx.clone();
                                root_btn.connect_clicked(move |_| {
                                    popover.popdown();
                                    move_items_to_bin(
                                        &item_ids,
                                        None,
                                        &library,
                                        &flow_box,
                                        &thumb_cache,
                                        &flow_box_paths,
                                        &current_bin_id,
                                        &show_all_media,
                                        &breadcrumb_bar,
                                        &all_media_btn,
                                        &filters,
                                        &on_library_changed,
                                    );
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
                                let item_ids = selected_ids.clone();
                                let library = library_ctx.clone();
                                let flow_box = flow_box_ctx.clone();
                                let thumb_cache = thumb_cache_ctx.clone();
                                let flow_box_paths = flow_box_paths_ctx.clone();
                                let current_bin_id = current_bin_id_ctx.clone();
                                let show_all_media = show_all_media_ctx.clone();
                                let breadcrumb_bar = breadcrumb_bar_ctx.clone();
                                let all_media_btn = all_media_btn_ctx.clone();
                                let filters = filters_ctx.clone();
                                let popover = popover.clone();
                                let on_library_changed = on_library_changed_ctx.clone();
                                move_btn.connect_clicked(move |_| {
                                    popover.popdown();
                                    move_items_to_bin(
                                        &item_ids,
                                        Some(bin_id.clone()),
                                        &library,
                                        &flow_box,
                                        &thumb_cache,
                                        &flow_box_paths,
                                        &current_bin_id,
                                        &show_all_media,
                                        &breadcrumb_bar,
                                        &all_media_btn,
                                        &filters,
                                        &on_library_changed,
                                    );
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
                        let filters = filters_ctx.clone();
                        let popover = popover.clone();
                        let on_library_changed = on_library_changed_ctx.clone();
                        new_bin_btn.connect_clicked(move |btn| {
                            popover.popdown();
                            show_new_bin_dialog(
                                btn,
                                current_bin_id.borrow().clone(),
                                &library,
                                &flow_box,
                                &thumb_cache,
                                &flow_box_paths,
                                &current_bin_id,
                                &show_all_media,
                                &breadcrumb_bar,
                                &all_media_btn,
                                &filters,
                                &on_library_changed,
                            );
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
                        let filters = filters_ctx.clone();
                        let popover = popover.clone();
                        let on_library_changed = on_library_changed_ctx.clone();
                        new_bin_btn.connect_clicked(move |btn| {
                            popover.popdown();
                            show_new_bin_dialog(
                                btn,
                                current_bin_id.borrow().clone(),
                                &library,
                                &flow_box,
                                &thumb_cache,
                                &flow_box_paths,
                                &current_bin_id,
                                &show_all_media,
                                &breadcrumb_bar,
                                &all_media_btn,
                                &filters,
                                &on_library_changed,
                            );
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
        let place_aligned_btn = place_aligned_btn.clone();
        let flow_box_paths_for_sel = flow_box_paths.clone();
        flow_box.connect_selected_children_changed(move |fb| {
            let selected = fb.selected_children();
            // Count only media selections (not bins) for multicam
            let entries = flow_box_paths_for_sel.borrow();
            let media_ids: Vec<String> = selected
                .iter()
                .filter_map(|c| {
                    let idx = c.index() as usize;
                    match entries.get(idx) {
                        Some(FlowBoxEntry::Media { item_id, .. }) => Some(item_id.clone()),
                        _ => None,
                    }
                })
                .collect();
            let media_count = media_ids.len();
            multicam_btn.set_visible(media_count >= 2);
            // Place-Aligned-by-Timecode: visible only when every
            // selected item has decoded TC AND at least 2 are
            // selected. Anything missing TC keeps the button
            // hidden — we don't show a disabled state here because
            // the convert-LTC path is right-click-per-item, so a
            // visible-disabled state on this sidebar button would
            // be confusing about what to fix.
            let all_have_tc = media_count >= 2 && {
                let lib = library.borrow();
                media_ids.iter().all(|id| {
                    lib.items
                        .iter()
                        .find(|i| &i.id == id)
                        .map(|i| i.source_timecode_base_ns.is_some())
                        .unwrap_or(false)
                })
            };
            place_aligned_btn.set_visible(all_have_tc);
            if let Some(child) = selected.first() {
                let idx = child.index() as usize;
                match entries.get(idx) {
                    Some(FlowBoxEntry::Media { item_id, .. }) => {
                        let lib = library.borrow();
                        if let Some(item) = lib.items.iter().find(|i| &i.id == item_id) {
                            let path = item.source_path.clone();
                            let dur = item.duration_ns;
                            let is_missing = item.is_missing;
                            let has_backing_file = item.has_backing_file();
                            drop(lib);
                            drop(entries);
                            header_relink_btn.set_visible(has_backing_file && is_missing);
                            if has_backing_file {
                                on_source_selected(path, dur);
                            }
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

    // "Place Aligned by Timecode" click handler.
    {
        let library = library.clone();
        let flow_box = flow_box.clone();
        let on_place_aligned = on_place_aligned_by_timecode.clone();
        let flow_box_paths_for_pa = flow_box_paths.clone();
        place_aligned_btn.connect_clicked(move |_| {
            let selected = flow_box.selected_children();
            let entries = flow_box_paths_for_pa.borrow();
            let lib = library.borrow();
            let paths: Vec<String> = selected
                .iter()
                .filter_map(|child| {
                    let idx = child.index() as usize;
                    match entries.get(idx) {
                        Some(FlowBoxEntry::Media { item_id, .. }) => lib
                            .items
                            .iter()
                            .find(|i| &i.id == item_id)
                            .filter(|i| {
                                i.has_backing_file() && i.source_timecode_base_ns.is_some()
                            })
                            .map(|i| i.source_path.clone()),
                        _ => None,
                    }
                })
                .collect();
            drop(lib);
            drop(entries);
            if paths.len() >= 2 {
                on_place_aligned(paths);
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
                        Some(FlowBoxEntry::Media { item_id, .. }) => lib
                            .items
                            .iter()
                            .find(|i| &i.id == item_id)
                            .filter(|item| item.has_backing_file())
                            .map(|i| i.source_path.clone()),
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
        let content_stack = content_stack.clone();
        let header_import_btn = header_import_btn.clone();
        let thumb_redraw_scheduled = thumb_redraw_scheduled.clone();
        let current_bin_id = current_bin_id.clone();
        let show_all_media = show_all_media.clone();
        let breadcrumb_bar = breadcrumb_bar.clone();
        let all_media_btn = all_media_btn.clone();
        let filter_box = filter_box.clone();
        let filters = filters.clone();
        let on_library_changed = on_library_changed.clone();
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
                            item.is_animated_svg = result.is_animated_svg;
                            item.video_width = result.video_width;
                            item.video_height = result.video_height;
                            item.frame_rate_num = result.frame_rate_num;
                            item.frame_rate_den = result.frame_rate_den;
                            item.codec_summary = result.codec_summary.clone();
                            item.file_size_bytes = result.file_size_bytes;
                            item.hdr_colorimetry = result.hdr_colorimetry.clone();
                            if item.source_timecode_base_ns.is_none() {
                                item.source_timecode_base_ns = result.source_timecode_base_ns;
                            }
                        }
                    }
                }
                if !flowbox_matches_library_binned(
                    &flow_box_paths.borrow(),
                    &lib,
                    &current_bin_id.borrow(),
                    *show_all_media.borrow(),
                    &filters.borrow(),
                ) {
                    rebuild_flowbox_binned(
                        &flow_box,
                        &lib,
                        &thumb_cache,
                        &flow_box_paths,
                        &current_bin_id.borrow(),
                        *show_all_media.borrow(),
                        &filters.borrow(),
                        &library,
                        &on_library_changed,
                    );
                    rebuild_breadcrumb(
                        &breadcrumb_bar,
                        &lib,
                        &current_bin_id.borrow(),
                        &current_bin_id,
                        &show_all_media,
                        &flow_box,
                        &library,
                        &thumb_cache,
                        &flow_box_paths,
                        &all_media_btn,
                        &filters,
                        &on_library_changed,
                    );
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
                        let (has_backing_file, audio_only) = lib
                            .items
                            .iter()
                            .find(|i| i.source_path == *path)
                            .map(|i| (i.has_backing_file(), i.is_audio_only))
                            .unwrap_or((false, false));
                        if has_backing_file && !audio_only {
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
                            let (has_backing_file, audio_only, is_image) = lib
                                .items
                                .iter()
                                .find(|i| i.source_path == *path)
                                .map(|i| (i.has_backing_file(), i.is_audio_only, i.is_image))
                                .unwrap_or((false, false, false));
                            if has_backing_file && !audio_only && !is_image {
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
                    if let Some(FlowBoxEntry::Media { ref item_id, .. }) = entries.get(idx) {
                        if let Some(item) = lib.items.iter().find(|i| &i.id == item_id) {
                            if !item.has_backing_file() {
                                idx += 1;
                                child_widget = w.next_sibling();
                                continue;
                            }
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
            content_stack.set_visible_child_name(if has_content { "library" } else { "empty" });
            header_import_btn.set_visible(has_content);
            all_media_btn.set_visible(!lib.bins.is_empty());
            filter_box.set_visible(has_content);
            if !flowbox_matches_library_binned(
                &flow_box_paths.borrow(),
                &lib,
                &current_bin_id.borrow(),
                *show_all_media.borrow(),
                &filters.borrow(),
            ) {
                rebuild_flowbox_binned(
                    &flow_box,
                    &lib,
                    &thumb_cache,
                    &flow_box_paths,
                    &current_bin_id.borrow(),
                    *show_all_media.borrow(),
                    &filters.borrow(),
                    &library,
                    &on_library_changed,
                );
                rebuild_breadcrumb(
                    &breadcrumb_bar,
                    &lib,
                    &current_bin_id.borrow(),
                    &current_bin_id,
                    &show_all_media,
                    &flow_box,
                    &library,
                    &thumb_cache,
                    &flow_box_paths,
                    &all_media_btn,
                    &filters,
                    &on_library_changed,
                );
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
                    if item.has_backing_file() && !item.is_missing {
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
                        if !item.has_backing_file() || item.is_missing {
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
        let on_library_changed = on_library_changed.clone();

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
            let on_library_changed = on_library_changed.clone();

            let window = btn.root().and_then(|r| r.downcast::<gtk::Window>().ok());

            dialog.open_multiple(window.as_ref(), gio::Cancellable::NONE, move |result| {
                if let Ok(files) = result {
                    let mut imported_any = false;
                    for i in 0..files.n_items() {
                        let Some(obj) = files.item(i) else { continue };
                        let Ok(file) = obj.downcast::<gio::File>() else {
                            continue;
                        };
                        let Some(path) = file.path() else { continue };
                        if import_path_into_library(
                            path.to_string_lossy().to_string(),
                            &library,
                            &flow_box,
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
                    if imported_any {
                        on_library_changed();
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
        let on_library_changed = on_library_changed.clone();
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
            if imported_any {
                on_library_changed();
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
        let filters_all = filters.clone();
        let collection_filter_all = collection_filter.clone();
        let on_library_changed_all = on_library_changed.clone();
        all_media_btn.connect_clicked(move |_| {
            let is_all = *show_all_media_all.borrow();
            *show_all_media_all.borrow_mut() = !is_all;
            collection_filter_all.set_active_id(Some(COLLECTION_NONE_ID));
            if !is_all {
                // Entering All Media mode — clear bin navigation
                *current_bin_id_all.borrow_mut() = None;
            }
            let lib = library_all.borrow();
            rebuild_flowbox_binned(
                &flow_box_all,
                &lib,
                &thumb_cache_all,
                &flow_box_paths_all,
                &current_bin_id_all.borrow(),
                *show_all_media_all.borrow(),
                &filters_all.borrow(),
                &library_all,
                &on_library_changed_all,
            );
            rebuild_breadcrumb(
                &breadcrumb_bar_all,
                &lib,
                &current_bin_id_all.borrow(),
                &current_bin_id_all,
                &show_all_media_all,
                &flow_box_all,
                &library_all,
                &thumb_cache_all,
                &flow_box_paths_all,
                &all_media_btn_all,
                &filters_all,
                &on_library_changed_all,
            );
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
        let filters = filters.clone();
        let refresh_collection_controls = refresh_collection_controls.clone();
        let on_library_changed = on_library_changed.clone();
        Rc::new(move || {
            let lib = library.borrow();
            rebuild_flowbox_binned(
                &flow_box,
                &lib,
                &thumb_cache,
                &flow_box_paths,
                &current_bin_id.borrow(),
                *show_all_media.borrow(),
                &filters.borrow(),
                &library,
                &on_library_changed,
            );
            header_relink_btn.set_visible(false);
            refresh_collection_controls();
        })
    };

    (vbox, clear_selection, force_rebuild)
}

/// Build a single thumbnail grid cell.
fn make_grid_item(
    item: &MediaItem,
    thumb_cache: &Rc<RefCell<ThumbnailCache>>,
    search_text: &str,
) -> FlowBoxChild {
    let path = item.source_path.clone();
    let duration_ns = item.duration_ns;
    let is_missing = item.is_missing;
    let is_audio_only = item.is_audio_only;
    let has_backing_file = item.has_backing_file();
    let clip_kind = item.clip_kind.clone();
    let display_name = media_display_name(item);

    // Kick off thumbnail loading — only after probe (duration_ns > 0) and only
    // for files that actually have video.  Audio-only files have no video frame
    // to extract; trying causes noisy ffmpeg "Output file does not contain any
    // stream" errors.
    if has_backing_file && duration_ns > 0 && !is_audio_only {
        thumb_cache.borrow_mut().request(&path, 0);
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

    // Hover-scrub state: None = show the static first-frame thumbnail
    // (the cache.request(..., 0) call above already queued it). Some(t) =
    // the cursor is inside the thumbnail and the draw_func should paint
    // the frame at the corresponding source time instead. The value
    // lives in a Cell so both the motion controller and the draw_func
    // can read it cheaply every redraw.
    let scrub_time_ns: Rc<Cell<Option<u64>>> = Rc::new(Cell::new(None));
    {
        let path_owned = path.clone();
        let thumb_cache = thumb_cache.clone();
        let scrub_time_ns = scrub_time_ns.clone();
        thumb_area.set_draw_func(move |_, cr, w, h| {
            if !has_backing_file {
                draw_non_file_placeholder(cr, w, h, clip_kind.as_ref());
                return;
            }
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
                    stem_x + 14.0 * s,
                    stem_top + 4.0 * s,
                    stem_x + 14.0 * s,
                    stem_top + 14.0 * s,
                    stem_x + 4.0 * s,
                    stem_top + 18.0 * s,
                );
                cr.stroke().ok();

                return;
            }
            let cache = thumb_cache.borrow();
            // Hover-scrub: when the cursor is over this thumbnail, prefer
            // the scrubbed frame. Fall back to the static t=0 frame if
            // the hover frame hasn't been extracted yet — avoids flashing
            // a placeholder during ffmpeg latency.
            if let Some(t) = scrub_time_ns.get() {
                if let Some(surf) = cache.get(&path_owned, t) {
                    let sx = w as f64 / THUMB_W as f64;
                    let sy = h as f64 / THUMB_H as f64;
                    cr.scale(sx, sy);
                    let _ = cr.set_source_surface(surf, 0.0, 0.0);
                    cr.paint().ok();
                    return;
                }
            }
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

    // Mouse hover-scrub: mapping cursor x to a source time, quantized to
    // ~100 ms buckets so repeated motion events at nearby pixels share
    // cache entries. Only wire the controller when there is a meaningful
    // duration to scrub — still images and audio-only items have nothing
    // to preview.
    if has_backing_file && duration_ns > 0 && !is_audio_only {
        let motion = gtk::EventControllerMotion::new();
        {
            let scrub_time_ns = scrub_time_ns.clone();
            let thumb_area_inner = thumb_area.clone();
            let thumb_cache_inner = thumb_cache.clone();
            let path_motion = path.clone();
            motion.connect_motion(move |_ctrl, x, _y| {
                let width = thumb_area_inner.width().max(1) as f64;
                let frac = (x / width).clamp(0.0, 1.0);
                let raw_t = (frac * duration_ns as f64).round() as u64;
                let bucket =
                    crate::media::thumb_cache::quantize_hover_scrub_time_ns(raw_t);
                if scrub_time_ns.get() != Some(bucket) {
                    scrub_time_ns.set(Some(bucket));
                    thumb_cache_inner
                        .borrow_mut()
                        .request(&path_motion, bucket);
                    thumb_area_inner.queue_draw();
                }
            });
        }
        {
            let scrub_time_ns = scrub_time_ns.clone();
            let thumb_area_inner = thumb_area.clone();
            motion.connect_leave(move |_ctrl| {
                if scrub_time_ns.get().is_some() {
                    scrub_time_ns.set(None);
                    thumb_area_inner.queue_draw();
                }
            });
        }
        thumb_area.add_controller(motion);
    }

    cell.append(&thumb_area);

    let name_label = Label::new(Some(&display_name));
    name_label.set_halign(gtk::Align::Center);
    name_label.set_max_width_chars(22);
    name_label.set_ellipsize(gtk4::pango::EllipsizeMode::End);
    name_label.add_css_class("clip-name");
    cell.append(&name_label);

    if let Some(primary_text) = media_primary_text(item) {
        let primary_label = Label::new(Some(&primary_text));
        primary_label.set_halign(gtk::Align::Center);
        primary_label.set_max_width_chars(26);
        primary_label.set_ellipsize(gtk4::pango::EllipsizeMode::End);
        primary_label.add_css_class("media-meta-primary");
        cell.append(&primary_label);
    }

    if let Some(secondary_text) = media_secondary_text(item) {
        let secondary_label = Label::new(Some(&secondary_text));
        secondary_label.set_halign(gtk::Align::Center);
        secondary_label.set_max_width_chars(26);
        secondary_label.set_ellipsize(gtk4::pango::EllipsizeMode::End);
        secondary_label.add_css_class("media-meta-secondary");
        cell.append(&secondary_label);
    }

    if let Some(rating_text) = media_rating_text(item.rating) {
        let rating_label = Label::new(Some(rating_text));
        rating_label.set_halign(gtk::Align::Center);
        rating_label.add_css_class("media-rating-badge");
        match item.rating {
            MediaRating::Favorite => rating_label.add_css_class("media-rating-favorite"),
            MediaRating::Reject => rating_label.add_css_class("media-rating-reject"),
            MediaRating::None => {}
        }
        cell.append(&rating_label);
    }

    if let Some(keyword_text) = media_keyword_card_text(item) {
        let keyword_label = Label::new(Some(&keyword_text));
        keyword_label.set_halign(gtk::Align::Center);
        keyword_label.set_max_width_chars(26);
        keyword_label.set_ellipsize(gtk4::pango::EllipsizeMode::End);
        keyword_label.add_css_class("media-keyword-summary");
        cell.append(&keyword_label);
    }

    if let Some(auto_tag_text) = media_auto_tag_card_text(item) {
        let auto_tag_label = Label::new(Some(&auto_tag_text));
        auto_tag_label.set_halign(gtk::Align::Center);
        auto_tag_label.set_max_width_chars(26);
        auto_tag_label.set_ellipsize(gtk4::pango::EllipsizeMode::End);
        auto_tag_label.add_css_class("media-keyword-summary");
        cell.append(&auto_tag_label);
    }

    if let Some(search_hint) = media_search_hint(item, search_text) {
        let search_label = Label::new(Some(&search_hint));
        search_label.set_halign(gtk::Align::Center);
        search_label.set_max_width_chars(26);
        search_label.set_ellipsize(gtk4::pango::EllipsizeMode::End);
        search_label.add_css_class("media-meta-secondary");
        cell.append(&search_label);
    }

    let offline_label = Label::new(Some("OFFLINE"));
    offline_label.set_halign(gtk::Align::Center);
    offline_label.add_css_class("media-offline-badge");
    offline_label.set_visible(is_missing);
    cell.append(&offline_label);

    // Subclip badge — visually differentiates range-of-source items
    // from top-level library entries. Shown unconditionally on
    // subclips (never OFF when parent_id is set).
    if item.is_subclip() {
        let subclip_label = Label::new(Some("SUBCLIP"));
        subclip_label.set_halign(gtk::Align::Center);
        // Reuse the keyword-summary badge CSS — it's a subtle tag-
        // style pill that reads well next to the rating / offline
        // badges above. Plenty of room to promote to a dedicated
        // class later if the design wants more contrast.
        subclip_label.add_css_class("media-keyword-summary");
        cell.append(&subclip_label);
    }

    let child = FlowBoxChild::new();
    child.set_child(Some(&cell));
    let tooltip = media_tooltip_text(item, Some(search_text));
    child.set_tooltip_text(Some(&tooltip));
    if is_missing {
        child.add_css_class("media-missing-item");
    }
    if has_backing_file {
        // Drag source payload: `"{source_path}|{duration_ns}|{item_id}"`.
        // The trailing `|{item_id}` was added when subclips shipped —
        // the drop target can look up the MediaItem by id to read
        // its subclip source_in/source_out so dropped subclips land
        // with the correct window already applied. Legacy two-part
        // payloads (pre-subclip saves / external drops) still parse
        // because the receiver splits on `|` and treats a missing
        // third component as "no specific item, use source monitor
        // marks or full range".
        let drag_src = gtk::DragSource::new();
        drag_src.set_actions(gdk4::DragAction::COPY);
        drag_src.set_exclusive(false);
        let payload = format!("{}|{duration_ns}|{}", item.source_path, item.id);
        let val = glib::Value::from(&payload);
        drag_src.set_content(Some(&gdk4::ContentProvider::for_value(&val)));
        child.add_controller(drag_src);
    }

    child
}

fn draw_non_file_placeholder(cr: &gtk::cairo::Context, w: i32, h: i32, kind: Option<&ClipKind>) {
    let (bg_r, bg_g, bg_b, badge) = match kind {
        Some(ClipKind::Title) => (0.28, 0.20, 0.08, "TITLE"),
        Some(ClipKind::Adjustment) => (0.17, 0.18, 0.28, "ADJ"),
        Some(ClipKind::Compound) => (0.10, 0.22, 0.20, "CMP"),
        Some(ClipKind::Multicam) => (0.24, 0.12, 0.26, "MC"),
        Some(ClipKind::Audition) => (0.28, 0.22, 0.06, "AUD"),
        Some(ClipKind::Video) => (0.18, 0.20, 0.28, "CLIP"),
        Some(ClipKind::Audio) => (0.14, 0.14, 0.22, "AUDIO"),
        Some(ClipKind::Image) => (0.18, 0.24, 0.18, "IMG"),
        Some(ClipKind::Drawing) => (0.22, 0.10, 0.22, "DRAW"),
        None => (0.15, 0.15, 0.20, "ITEM"),
    };

    cr.set_source_rgb(bg_r, bg_g, bg_b);
    cr.rectangle(0.0, 0.0, w as f64, h as f64);
    cr.fill().ok();

    cr.set_source_rgba(1.0, 1.0, 1.0, 0.14);
    cr.rectangle(6.0, 6.0, (w - 12).max(0) as f64, (h - 12).max(0) as f64);
    cr.stroke().ok();

    cr.set_source_rgb(0.94, 0.94, 0.96);
    cr.select_font_face(
        "Sans",
        gtk::cairo::FontSlant::Normal,
        gtk::cairo::FontWeight::Bold,
    );
    cr.set_font_size((w.min(h) as f64 / 5.8).clamp(14.0, 24.0));
    if let Ok(extents) = cr.text_extents(badge) {
        let x = (w as f64 - extents.width()) / 2.0 - extents.x_bearing();
        let y = (h as f64 - extents.height()) / 2.0 - extents.y_bearing();
        cr.move_to(x, y);
        cr.show_text(badge).ok();
    }
}

fn flowbox_matches_library_binned(
    current_entries: &[FlowBoxEntry],
    lib: &MediaLibrary,
    current_bin_id: &Option<String>,
    show_all: bool,
    filters: &MediaFilterCriteria,
) -> bool {
    let expected = build_expected_entries(lib, current_bin_id, show_all, filters);
    if current_entries.len() != expected.len() {
        return false;
    }
    current_entries
        .iter()
        .zip(expected.iter())
        .all(|(a, b)| match (a, b) {
            (
                FlowBoxEntry::Bin {
                    id: id_a,
                    name: name_a,
                },
                FlowBoxEntry::Bin {
                    id: id_b,
                    name: name_b,
                },
            ) => id_a == id_b && name_a == name_b,
            (
                FlowBoxEntry::Media {
                    item_id: id_a,
                    display_key: d_a,
                },
                FlowBoxEntry::Media {
                    item_id: id_b,
                    display_key: d_b,
                },
            ) => id_a == id_b && d_a == d_b,
            _ => false,
        })
}

fn build_expected_entries(
    lib: &MediaLibrary,
    current_bin_id: &Option<String>,
    show_all: bool,
    filters: &MediaFilterCriteria,
) -> Vec<FlowBoxEntry> {
    let mut entries = Vec::new();
    if show_all {
        // All Media mode: show all items flat, no bins
        let mut items: Vec<&MediaItem> = lib
            .items
            .iter()
            .filter(|item| media_matches_filters(item, filters))
            .collect();
        sort_media_items(items.as_mut_slice(), filters.search_text.as_str());
        for item in items {
            entries.push(FlowBoxEntry::Media {
                item_id: item.id.clone(),
                display_key: media_display_key(item),
            });
        }
    } else {
        // Show child bins first (alphabetically), then items in current bin
        let mut child_bins = lib.child_bins(current_bin_id.as_deref());
        child_bins.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
        for bin in &child_bins {
            entries.push(FlowBoxEntry::Bin {
                id: bin.id.clone(),
                name: bin.name.clone(),
            });
        }
        let mut items: Vec<&MediaItem> = lib
            .items_in_bin(current_bin_id.as_deref())
            .into_iter()
            .filter(|item| media_matches_filters(item, filters))
            .collect();
        sort_media_items(items.as_mut_slice(), filters.search_text.as_str());
        for item in items {
            entries.push(FlowBoxEntry::Media {
                item_id: item.id.clone(),
                display_key: media_display_key(item),
            });
        }
    }
    entries
}

fn media_display_key(item: &MediaItem) -> String {
    format!(
        "{}|{}|{}|{}|{}|{}|{}|{}|{}|{}",
        item.id,
        item.source_path,
        item.is_missing,
        media_display_name(item),
        media_primary_text(item).unwrap_or_default(),
        media_secondary_text(item).unwrap_or_default(),
        media_rating_text(item.rating).unwrap_or_default(),
        media_keyword_state_key(item),
        media_auto_tag_state_key(item),
        media_transcript_state_key(item),
    )
}

fn media_primary_text(item: &MediaItem) -> Option<String> {
    if let Some(kind) = item.clip_kind.as_ref() {
        let mut parts = vec![non_file_clip_kind_text(kind).to_string()];
        if matches!(kind, ClipKind::Title) {
            if let Some(label) = normalized_media_text(Some(item.label.as_str())) {
                if normalized_media_text(item.title_text.as_deref()).as_deref()
                    != Some(label.as_str())
                {
                    parts.push(label);
                }
            }
        }
        return Some(parts.join(" • "));
    }

    if !item.is_missing
        && item.duration_ns == 0
        && item.codec_summary.is_none()
        && item.video_width.is_none()
        && item.video_height.is_none()
    {
        return Some("Analyzing metadata…".to_string());
    }

    let mut parts = Vec::new();
    if let Some(resolution) = media_resolution_text(item) {
        parts.push(resolution);
    }
    if !item.is_image {
        if let Some(frame_rate) = media_frame_rate_text(item) {
            parts.push(frame_rate);
        }
    }
    if parts.is_empty() {
        if item.is_audio_only {
            parts.push("Audio only".to_string());
        } else if item.is_animated_svg {
            parts.push("Animated SVG".to_string());
        } else if item.is_image {
            parts.push("Still image".to_string());
        }
    }
    if parts.is_empty() {
        None
    } else {
        Some(parts.join(" • "))
    }
}

fn media_secondary_text(item: &MediaItem) -> Option<String> {
    if item.clip_kind.is_some() {
        return (item.duration_ns > 0).then(|| format_duration_short(item.duration_ns));
    }

    let mut parts = Vec::new();
    if let Some(codec) = item.codec_summary.as_ref() {
        parts.push(codec.clone());
    }
    if item.duration_ns > 0 {
        parts.push(format_duration_short(item.duration_ns));
    }
    if let Some(file_size_bytes) = item.file_size_bytes {
        parts.push(format_file_size(file_size_bytes));
    }
    if parts.is_empty() {
        None
    } else {
        Some(parts.join(" • "))
    }
}

fn media_keyword_card_text(item: &MediaItem) -> Option<String> {
    media_keyword_summary(item, 2).map(|summary| format!("Keywords: {summary}"))
}

fn media_auto_tag_card_text(item: &MediaItem) -> Option<String> {
    media_auto_tag_summary(item, 3).map(|summary| format!("Tags: {summary}"))
}

fn media_keyword_state_key(item: &MediaItem) -> String {
    item.keyword_ranges
        .iter()
        .map(|range| {
            format!(
                "{}:{}:{}:{}",
                range.id, range.label, range.start_ns, range.end_ns
            )
        })
        .collect::<Vec<_>>()
        .join("|")
}

fn media_auto_tag_detail_lines(item: &MediaItem) -> Vec<String> {
    item.auto_tags
        .iter()
        .map(|tag| {
            let confidence = (tag.confidence * 100.0).round();
            match tag.best_frame_time_ns {
                Some(time_ns) => format!(
                    "Tag: {} — {} ({confidence:.0}%) @ {}",
                    tag.category.label(),
                    tag.label,
                    format_duration_short(time_ns)
                ),
                None => format!(
                    "Tag: {} — {} ({confidence:.0}%)",
                    tag.category.label(),
                    tag.label
                ),
            }
        })
        .collect()
}

fn sort_media_items(items: &mut [&MediaItem], search_text: &str) {
    if search_text.trim().is_empty() {
        return;
    }
    items.sort_by(|a, b| {
        let a_match = media_search_match(a, search_text);
        let b_match = media_search_match(b, search_text);
        b_match
            .as_ref()
            .map(|m| m.score)
            .unwrap_or_default()
            .cmp(&a_match.as_ref().map(|m| m.score).unwrap_or_default())
            .then_with(|| media_display_name(a).cmp(&media_display_name(b)))
            .then_with(|| a.source_path.cmp(&b.source_path))
    });
}

fn media_keyword_detail_lines(item: &MediaItem) -> Vec<String> {
    item.keyword_ranges
        .iter()
        .map(|range| {
            let label = normalized_media_text(Some(range.label.as_str()))
                .unwrap_or_else(|| "Untitled".to_string());
            let start = format_duration_short(range.start_ns);
            let end_ns = range.end_ns.max(range.start_ns);
            if end_ns == range.start_ns {
                format!("Keyword: {label} @ {start}")
            } else {
                let end = format_duration_short(end_ns);
                format!("Keyword: {label} ({start} - {end})")
            }
        })
        .collect()
}

fn media_search_hint(item: &MediaItem, search_text: &str) -> Option<String> {
    let search_match = media_search_match(item, search_text)?;
    match search_match.field {
        MediaSearchField::Transcript => Some(format!(
            "Spoken: {}",
            search_match.excerpt.unwrap_or_default()
        )),
        MediaSearchField::AutoTag => Some(format!(
            "Tags: {}",
            search_match.excerpt.unwrap_or_default()
        )),
        MediaSearchField::Visual => Some(format!(
            "Visual: {}",
            search_match.excerpt.unwrap_or_default()
        )),
        _ => None,
    }
}

fn media_tooltip_text(item: &MediaItem, search_text: Option<&str>) -> String {
    if let Some(kind) = item.clip_kind.as_ref() {
        let mut lines = vec![non_file_clip_kind_text(kind).to_string()];
        if let Some(title_text) = normalized_media_text(item.title_text.as_deref()) {
            lines.push(format!("Text: {title_text}"));
        }
        if let Some(label) = normalized_media_text(Some(item.label.as_str())) {
            let label_key = if matches!(kind, ClipKind::Title) {
                "Template"
            } else {
                "Label"
            };
            if normalized_media_text(item.title_text.as_deref()).as_deref() != Some(label.as_str())
            {
                lines.push(format!("{label_key}: {label}"));
            }
        }
        if item.duration_ns > 0 {
            lines.push(format!(
                "Duration: {}",
                format_duration_short(item.duration_ns)
            ));
        }
        if let Some(rating_text) = media_rating_text(item.rating) {
            lines.push(format!("Rating: {rating_text}"));
        }
        lines.extend(media_keyword_detail_lines(item));
        lines.extend(media_auto_tag_detail_lines(item));
        if let Some(search_text) = search_text.filter(|text| !text.trim().is_empty()) {
            if let Some(search_match) = media_search_match(item, search_text) {
                lines.push(format!(
                    "Search hit: {}",
                    media_search_field_label(search_match.field)
                ));
                if let Some(excerpt) = search_match.excerpt {
                    lines.push(format!("Match: {excerpt}"));
                }
            }
        }
        return lines.join("\n");
    }

    let mut lines = vec![item.source_path.clone()];
    if item.is_missing {
        lines.push("Status: OFFLINE".to_string());
    }
    if let Some(codec) = item.codec_summary.as_ref() {
        lines.push(format!("Codec: {codec}"));
    }
    if let Some(resolution) = media_resolution_text(item) {
        lines.push(format!("Resolution: {resolution}"));
    }
    if let Some(frame_rate) = media_frame_rate_text(item) {
        lines.push(format!("Frame rate: {frame_rate}"));
    }
    if item.duration_ns > 0 {
        lines.push(format!(
            "Duration: {}",
            format_duration_short(item.duration_ns)
        ));
    }
    if let Some(file_size_bytes) = item.file_size_bytes {
        lines.push(format!("Size: {}", format_file_size(file_size_bytes)));
    }
    if let Some(rating_text) = media_rating_text(item.rating) {
        lines.push(format!("Rating: {rating_text}"));
    }
    lines.extend(media_keyword_detail_lines(item));
    lines.extend(media_auto_tag_detail_lines(item));
    let transcript_segments = item
        .transcript_windows
        .iter()
        .map(|window| window.segments.len())
        .sum::<usize>();
    if transcript_segments > 0 {
        lines.push(format!("Transcript segments: {transcript_segments}"));
    }
    if let Some(search_text) = search_text.filter(|text| !text.trim().is_empty()) {
        if let Some(search_match) = media_search_match(item, search_text) {
            lines.push(format!(
                "Search hit: {}",
                media_search_field_label(search_match.field)
            ));
            if let Some(excerpt) = search_match.excerpt {
                lines.push(format!("Match: {excerpt}"));
            }
        }
    }
    lines.join("\n")
}

fn media_search_field_label(field: MediaSearchField) -> &'static str {
    match field {
        MediaSearchField::DisplayName => "name",
        MediaSearchField::Label => "label",
        MediaSearchField::TitleText => "title text",
        MediaSearchField::SourcePath => "file path",
        MediaSearchField::Codec => "codec",
        MediaSearchField::Keyword => "keyword",
        MediaSearchField::AutoTag => "auto tag",
        MediaSearchField::Transcript => "spoken content",
        MediaSearchField::Visual => "visual content",
    }
}

fn media_resolution_text(item: &MediaItem) -> Option<String> {
    item.video_width
        .zip(item.video_height)
        .map(|(width, height)| format!("{width}x{height}"))
}

fn media_frame_rate_text(item: &MediaItem) -> Option<String> {
    let fps = media_frame_rate_value(item)?;
    let mut text = format!("{fps:.2}");
    while text.ends_with('0') {
        text.pop();
    }
    if text.ends_with('.') {
        text.pop();
    }
    Some(format!("{text} fps"))
}

fn format_duration_short(ns: u64) -> String {
    let total_seconds = ns / 1_000_000_000;
    let hours = total_seconds / 3600;
    let minutes = (total_seconds % 3600) / 60;
    let seconds = total_seconds % 60;
    if hours > 0 {
        format!("{hours}:{minutes:02}:{seconds:02}")
    } else {
        format!("{minutes}:{seconds:02}")
    }
}

fn format_file_size(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut value = bytes as f64;
    let mut unit_idx = 0usize;
    while value >= 1024.0 && unit_idx < UNITS.len() - 1 {
        value /= 1024.0;
        unit_idx += 1;
    }
    if unit_idx == 0 {
        format!("{bytes} {}", UNITS[unit_idx])
    } else {
        format!("{value:.1} {}", UNITS[unit_idx])
    }
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
    let display_key = media_display_key(&item);
    let child = make_grid_item(&item, thumb_cache, "");
    let item_id = item.id.clone();
    library.borrow_mut().items.push(item);
    flow_box.insert(&child, -1);
    flow_box_paths.borrow_mut().push(FlowBoxEntry::Media {
        item_id,
        display_key,
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
    filters: &MediaFilterCriteria,
    library_rc: &Rc<RefCell<MediaLibrary>>,
    on_library_changed: &Rc<dyn Fn()>,
) {
    // Temporarily detach from ScrolledWindow parent to avoid GTK adjustment
    // warnings during bulk child removal/insertion.
    let scroll_parent = fb
        .parent()
        .and_then(|p| p.downcast::<ScrolledWindow>().ok());
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
    let entries = build_expected_entries(lib, current_bin_id, show_all, filters);
    let mut paths = flow_box_paths.borrow_mut();
    paths.clear();
    for entry in &entries {
        match entry {
            FlowBoxEntry::Bin { id, name } => {
                let child = make_bin_item(name, id, library_rc, on_library_changed);
                fb.insert(&child, -1);
            }
            FlowBoxEntry::Media { item_id, .. } => {
                let item = lib.items.iter().find(|i| &i.id == item_id);
                if let Some(item) = item {
                    let child = make_grid_item(item, thumb_cache, filters.search_text.as_str());
                    fb.insert(&child, -1);
                }
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
fn make_bin_item(
    name: &str,
    id: &str,
    library: &Rc<RefCell<MediaLibrary>>,
    on_library_changed: &Rc<dyn Fn()>,
) -> FlowBoxChild {
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
        cr.arc(
            fx + r,
            fy + r,
            r,
            std::f64::consts::PI,
            1.5 * std::f64::consts::PI,
        );
        cr.arc(
            fx + fw - r,
            fy + r,
            r,
            1.5 * std::f64::consts::PI,
            2.0 * std::f64::consts::PI,
        );
        cr.arc(fx + fw - r, fy + fh - r, r, 0.0, 0.5 * std::f64::consts::PI);
        cr.arc(
            fx + r,
            fy + fh - r,
            r,
            0.5 * std::f64::consts::PI,
            std::f64::consts::PI,
        );
        cr.close_path();
        cr.fill().ok();

        // Folder tab
        cr.set_source_rgb(0.50, 0.60, 0.75);
        let tw = 16.0 * s;
        let th = 6.0 * s;
        cr.new_path();
        cr.arc(
            fx + r,
            fy - th + r,
            r,
            std::f64::consts::PI,
            1.5 * std::f64::consts::PI,
        );
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
    let on_library_changed = on_library_changed.clone();
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
            drop(lib);
            on_library_changed();
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
    filters: &Rc<RefCell<MediaFilterCriteria>>,
    on_library_changed: &Rc<dyn Fn()>,
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
        let filters = filters.clone();
        let on_library_changed = on_library_changed.clone();
        btn.connect_clicked(move |_| {
            *current_bin_id_rc.borrow_mut() = None;
            let lib = library_rc.borrow();
            rebuild_flowbox_binned(
                &flow_box,
                &lib,
                &thumb_cache,
                &flow_box_paths,
                &None,
                false,
                &filters.borrow(),
                &library_rc,
                &on_library_changed,
            );
            rebuild_breadcrumb(
                &bar_for_closure,
                &lib,
                &None,
                &current_bin_id_rc,
                &show_all_media_rc,
                &flow_box,
                &library_rc,
                &thumb_cache,
                &flow_box_paths,
                &all_media_btn,
                &filters,
                &on_library_changed,
            );
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
            let filters = filters.clone();
            let on_library_changed = on_library_changed.clone();
            btn.connect_clicked(move |_| {
                *current_bin_id_rc.borrow_mut() = Some(target_id.clone());
                let lib = library_rc.borrow();
                let cid = current_bin_id_rc.borrow().clone();
                rebuild_flowbox_binned(
                    &flow_box,
                    &lib,
                    &thumb_cache,
                    &flow_box_paths,
                    &cid,
                    false,
                    &filters.borrow(),
                    &library_rc,
                    &on_library_changed,
                );
                rebuild_breadcrumb(
                    &bar_for_closure,
                    &lib,
                    &cid,
                    &current_bin_id_rc,
                    &show_all_media_rc,
                    &flow_box,
                    &library_rc,
                    &thumb_cache,
                    &flow_box_paths,
                    &all_media_btn,
                    &filters,
                    &on_library_changed,
                );
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
    filters: &Rc<RefCell<MediaFilterCriteria>>,
    on_library_changed: &Rc<dyn Fn()>,
) {
    let lib = library.borrow();
    let cid = current_bin_id.borrow().clone();
    let sa = *show_all_media.borrow();
    rebuild_flowbox_binned(
        flow_box,
        &lib,
        thumb_cache,
        flow_box_paths,
        &cid,
        sa,
        &filters.borrow(),
        library,
        on_library_changed,
    );
    rebuild_breadcrumb(
        breadcrumb_bar,
        &lib,
        &cid,
        current_bin_id,
        show_all_media,
        flow_box,
        library,
        thumb_cache,
        flow_box_paths,
        all_media_btn,
        filters,
        on_library_changed,
    );
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
    filters: &Rc<RefCell<MediaFilterCriteria>>,
    on_library_changed: &Rc<dyn Fn()>,
) {
    let window = flow_box
        .root()
        .and_then(|r| r.downcast::<gtk::Window>().ok());
    let dialog = gtk::Dialog::with_buttons(
        Some("New Bin"),
        window.as_ref(),
        gtk::DialogFlags::MODAL | gtk::DialogFlags::DESTROY_WITH_PARENT,
        &[
            ("Create", gtk::ResponseType::Accept),
            ("Cancel", gtk::ResponseType::Cancel),
        ],
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
    let filters = filters.clone();
    let on_library_changed = on_library_changed.clone();
    dialog.connect_response(move |dlg, response| {
        if response == gtk::ResponseType::Accept {
            let name = entry.text().to_string();
            if !name.is_empty() {
                use crate::model::media_library::MediaBin;
                let bin = MediaBin::new(name, parent_id.clone());
                library.borrow_mut().bins.push(bin);
                refresh_bin_view(
                    &library,
                    &flow_box,
                    &thumb_cache,
                    &flow_box_paths,
                    &current_bin_id,
                    &show_all_media,
                    &breadcrumb_bar,
                    &all_media_btn,
                    &filters,
                    &on_library_changed,
                );
                on_library_changed();
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
    filters: &Rc<RefCell<MediaFilterCriteria>>,
    on_library_changed: &Rc<dyn Fn()>,
) {
    let current_name = library
        .borrow()
        .bins
        .iter()
        .find(|b| b.id == bin_id)
        .map(|b| b.name.clone())
        .unwrap_or_default();
    let window = flow_box
        .root()
        .and_then(|r| r.downcast::<gtk::Window>().ok());
    let dialog = gtk::Dialog::with_buttons(
        Some("Rename Bin"),
        window.as_ref(),
        gtk::DialogFlags::MODAL | gtk::DialogFlags::DESTROY_WITH_PARENT,
        &[
            ("Rename", gtk::ResponseType::Accept),
            ("Cancel", gtk::ResponseType::Cancel),
        ],
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
    let filters = filters.clone();
    let on_library_changed = on_library_changed.clone();
    dialog.connect_response(move |dlg, response| {
        if response == gtk::ResponseType::Accept {
            let name = entry.text().to_string();
            if !name.is_empty() {
                if let Some(bin) = library
                    .borrow_mut()
                    .bins
                    .iter_mut()
                    .find(|b| b.id == bin_id)
                {
                    bin.name = name;
                }
                refresh_bin_view(
                    &library,
                    &flow_box,
                    &thumb_cache,
                    &flow_box_paths,
                    &current_bin_id,
                    &show_all_media,
                    &breadcrumb_bar,
                    &all_media_btn,
                    &filters,
                    &on_library_changed,
                );
                on_library_changed();
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
    filters: &Rc<RefCell<MediaFilterCriteria>>,
    on_library_changed: &Rc<dyn Fn()>,
) {
    let mut lib = library.borrow_mut();
    let parent_id = lib
        .bins
        .iter()
        .find(|b| b.id == bin_id)
        .and_then(|b| b.parent_id.clone());

    // Move items in this bin to the parent bin (or root)
    for item in lib.items.iter_mut() {
        if item.bin_id.as_deref() == Some(bin_id) {
            item.bin_id = parent_id.clone();
        }
    }

    // Reparent child bins to the parent
    let child_ids: Vec<String> = lib
        .bins
        .iter()
        .filter(|b| b.parent_id.as_deref() == Some(bin_id))
        .map(|b| b.id.clone())
        .collect();
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
    refresh_bin_view(
        library,
        flow_box,
        thumb_cache,
        flow_box_paths,
        current_bin_id,
        show_all_media,
        breadcrumb_bar,
        all_media_btn,
        filters,
        on_library_changed,
    );
    on_library_changed();
}

/// Move media items to a bin (or root if bin_id is None).
/// Core removal: delete the given library items, purge any smart-
/// collection pins that referenced them, and refresh the browser.
/// Does NOT touch the timeline — any clips whose `source_path`
/// matched one of the removed items will surface as OFFLINE (via
/// the existing `is_missing` detection pass that runs on project
/// reload). Cache entries (proxy / thumbnail / waveform / etc.)
/// stay on disk and get cleaned up by the existing eviction /
/// project-close paths.
fn remove_items_from_library(
    item_ids: &[String],
    library: &Rc<RefCell<MediaLibrary>>,
    flow_box: &FlowBox,
    thumb_cache: &Rc<RefCell<ThumbnailCache>>,
    flow_box_paths: &Rc<RefCell<Vec<FlowBoxEntry>>>,
    current_bin_id: &Rc<RefCell<Option<String>>>,
    show_all_media: &Rc<RefCell<bool>>,
    breadcrumb_bar: &GBox,
    all_media_btn: &Button,
    filters: &Rc<RefCell<MediaFilterCriteria>>,
    on_library_changed: &Rc<dyn Fn()>,
) {
    {
        let mut lib = library.borrow_mut();
        // Drop matching items from the master list.
        lib.items.retain(|item| !item_ids.contains(&item.id));
    }
    refresh_bin_view(
        library,
        flow_box,
        thumb_cache,
        flow_box_paths,
        current_bin_id,
        show_all_media,
        breadcrumb_bar,
        all_media_btn,
        filters,
        on_library_changed,
    );
    on_library_changed();
}

/// User-facing entry point: check whether any selected item is used
/// on the timeline; if so, ask for confirmation with the usage count
/// before proceeding. If no timeline use, remove immediately.
#[allow(clippy::too_many_arguments)]
fn remove_items_from_library_with_confirm(
    item_ids: &[String],
    library: &Rc<RefCell<MediaLibrary>>,
    flow_box: &FlowBox,
    thumb_cache: &Rc<RefCell<ThumbnailCache>>,
    flow_box_paths: &Rc<RefCell<Vec<FlowBoxEntry>>>,
    current_bin_id: &Rc<RefCell<Option<String>>>,
    show_all_media: &Rc<RefCell<bool>>,
    breadcrumb_bar: &GBox,
    all_media_btn: &Button,
    filters: &Rc<RefCell<MediaFilterCriteria>>,
    on_library_changed: &Rc<dyn Fn()>,
    on_check_library_usage: &Rc<dyn Fn(&[String]) -> usize>,
    parent: Option<&gtk::Window>,
) {
    if item_ids.is_empty() {
        return;
    }

    // Collect the source paths for timeline-usage checking. Items
    // without a backing file (e.g. generated media) can't be
    // referenced by timeline clips' source_path, so they get
    // skipped in the count.
    let source_paths: Vec<String> = {
        let lib = library.borrow();
        lib.items
            .iter()
            .filter(|i| item_ids.contains(&i.id))
            .map(|i| i.source_path.clone())
            .filter(|p| !p.is_empty())
            .collect()
    };
    let usage_count = if source_paths.is_empty() {
        0
    } else {
        on_check_library_usage(&source_paths)
    };

    if usage_count == 0 {
        // No timeline clips reference these items — straight removal.
        remove_items_from_library(
            item_ids,
            library,
            flow_box,
            thumb_cache,
            flow_box_paths,
            current_bin_id,
            show_all_media,
            breadcrumb_bar,
            all_media_btn,
            filters,
            on_library_changed,
        );
        return;
    }

    // Used on the timeline — surface a confirmation dialog.
    let item_label = if item_ids.len() == 1 {
        "1 item".to_string()
    } else {
        format!("{} items", item_ids.len())
    };
    let clip_label = if usage_count == 1 {
        "1 timeline clip".to_string()
    } else {
        format!("{usage_count} timeline clips")
    };
    let message = format!(
        "Remove {item_label} from the Media Library?\n\n\
         {clip_label} on the timeline will be marked OFFLINE \
         but will not be deleted. You can relink them later if you \
         re-import the source file."
    );

    let dialog = gtk::AlertDialog::builder()
        .message("Remove from Library")
        .detail(&message)
        .buttons(["Cancel", "Remove"])
        .cancel_button(0)
        .default_button(0)
        .modal(true)
        .build();

    let item_ids = item_ids.to_vec();
    let library = library.clone();
    let flow_box = flow_box.clone();
    let thumb_cache = thumb_cache.clone();
    let flow_box_paths = flow_box_paths.clone();
    let current_bin_id = current_bin_id.clone();
    let show_all_media = show_all_media.clone();
    let breadcrumb_bar = breadcrumb_bar.clone();
    let all_media_btn = all_media_btn.clone();
    let filters = filters.clone();
    let on_library_changed = on_library_changed.clone();
    dialog.choose(parent, gio::Cancellable::NONE, move |result| {
        if let Ok(1) = result {
            remove_items_from_library(
                &item_ids,
                &library,
                &flow_box,
                &thumb_cache,
                &flow_box_paths,
                &current_bin_id,
                &show_all_media,
                &breadcrumb_bar,
                &all_media_btn,
                &filters,
                &on_library_changed,
            );
        }
    });
}

fn move_items_to_bin(
    item_ids: &[String],
    bin_id: Option<String>,
    library: &Rc<RefCell<MediaLibrary>>,
    flow_box: &FlowBox,
    thumb_cache: &Rc<RefCell<ThumbnailCache>>,
    flow_box_paths: &Rc<RefCell<Vec<FlowBoxEntry>>>,
    current_bin_id: &Rc<RefCell<Option<String>>>,
    show_all_media: &Rc<RefCell<bool>>,
    breadcrumb_bar: &GBox,
    all_media_btn: &Button,
    filters: &Rc<RefCell<MediaFilterCriteria>>,
    on_library_changed: &Rc<dyn Fn()>,
) {
    {
        let mut lib = library.borrow_mut();
        for item in lib.items.iter_mut() {
            if item_ids.contains(&item.id) {
                item.bin_id = bin_id.clone();
            }
        }
    }
    refresh_bin_view(
        library,
        flow_box,
        thumb_cache,
        flow_box_paths,
        current_bin_id,
        show_all_media,
        breadcrumb_bar,
        all_media_btn,
        filters,
        on_library_changed,
    );
    on_library_changed();
}

fn set_items_rating(
    item_ids: &[String],
    rating: MediaRating,
    library: &Rc<RefCell<MediaLibrary>>,
    flow_box: &FlowBox,
    thumb_cache: &Rc<RefCell<ThumbnailCache>>,
    flow_box_paths: &Rc<RefCell<Vec<FlowBoxEntry>>>,
    current_bin_id: &Rc<RefCell<Option<String>>>,
    show_all_media: &Rc<RefCell<bool>>,
    breadcrumb_bar: &GBox,
    all_media_btn: &Button,
    filters: &Rc<RefCell<MediaFilterCriteria>>,
    on_library_changed: &Rc<dyn Fn()>,
) {
    {
        let mut lib = library.borrow_mut();
        for item in lib.items.iter_mut() {
            if item_ids.contains(&item.id) {
                item.rating = rating;
            }
        }
    }
    refresh_bin_view(
        library,
        flow_box,
        thumb_cache,
        flow_box_paths,
        current_bin_id,
        show_all_media,
        breadcrumb_bar,
        all_media_btn,
        filters,
        on_library_changed,
    );
    on_library_changed();
}

#[allow(deprecated)]
fn show_new_collection_dialog(
    collection_filter: &gtk::ComboBoxText,
    library: &Rc<RefCell<MediaLibrary>>,
    criteria: MediaFilterCriteria,
    refresh_browser_view: &Rc<dyn Fn()>,
    refresh_collection_controls: &Rc<dyn Fn()>,
    on_library_changed: &Rc<dyn Fn()>,
) {
    let window = collection_filter
        .root()
        .and_then(|root| root.downcast::<gtk::Window>().ok());
    let dialog = gtk::Dialog::with_buttons(
        Some("Save Smart Collection"),
        window.as_ref(),
        gtk::DialogFlags::MODAL | gtk::DialogFlags::DESTROY_WITH_PARENT,
        &[
            ("Save", gtk::ResponseType::Accept),
            ("Cancel", gtk::ResponseType::Cancel),
        ],
    );
    dialog.set_default_response(gtk::ResponseType::Accept);
    let entry = gtk::Entry::new();
    entry.set_placeholder_text(Some("Collection name"));
    entry.set_activates_default(true);
    entry.set_margin_top(8);
    entry.set_margin_bottom(8);
    entry.set_margin_start(8);
    entry.set_margin_end(8);
    dialog.content_area().append(&entry);

    let library = library.clone();
    let collection_filter = collection_filter.clone();
    let refresh_browser_view = refresh_browser_view.clone();
    let refresh_collection_controls = refresh_collection_controls.clone();
    let on_library_changed = on_library_changed.clone();
    dialog.connect_response(move |dlg, response| {
        if response == gtk::ResponseType::Accept {
            let name = entry.text().trim().to_string();
            if !name.is_empty() {
                let mut lib = library.borrow_mut();
                let collection = MediaCollection::new(name, criteria.clone());
                let collection_id = collection.id.clone();
                lib.collections.push(collection);
                drop(lib);
                refresh_collection_controls();
                collection_filter.set_active_id(Some(&collection_id));
                refresh_browser_view();
                on_library_changed();
            }
        }
        dlg.close();
    });
    dialog.present();
}

#[allow(deprecated)]
fn show_rename_collection_dialog(
    widget: &impl gtk4::prelude::IsA<gtk::Widget>,
    collection_id: &str,
    library: &Rc<RefCell<MediaLibrary>>,
    refresh_browser_view: &Rc<dyn Fn()>,
    refresh_collection_controls: &Rc<dyn Fn()>,
    on_library_changed: &Rc<dyn Fn()>,
) {
    let current_name = library
        .borrow()
        .collections
        .iter()
        .find(|collection| collection.id == collection_id)
        .map(|collection| collection.name.clone())
        .unwrap_or_default();
    let window = widget
        .root()
        .and_then(|root| root.downcast::<gtk::Window>().ok());
    let dialog = gtk::Dialog::with_buttons(
        Some("Rename Smart Collection"),
        window.as_ref(),
        gtk::DialogFlags::MODAL | gtk::DialogFlags::DESTROY_WITH_PARENT,
        &[
            ("Rename", gtk::ResponseType::Accept),
            ("Cancel", gtk::ResponseType::Cancel),
        ],
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

    let collection_id = collection_id.to_string();
    let library = library.clone();
    let refresh_browser_view = refresh_browser_view.clone();
    let refresh_collection_controls = refresh_collection_controls.clone();
    let on_library_changed = on_library_changed.clone();
    dialog.connect_response(move |dlg, response| {
        if response == gtk::ResponseType::Accept {
            let name = entry.text().trim().to_string();
            if !name.is_empty() {
                if let Some(collection) = library
                    .borrow_mut()
                    .collections
                    .iter_mut()
                    .find(|collection| collection.id == collection_id)
                {
                    collection.name = name;
                }
                refresh_collection_controls();
                refresh_browser_view();
                on_library_changed();
            }
        }
        dlg.close();
    });
    dialog.present();
}

fn delete_collection(
    collection_filter: &gtk::ComboBoxText,
    collection_id: &str,
    library: &Rc<RefCell<MediaLibrary>>,
    refresh_browser_view: &Rc<dyn Fn()>,
    refresh_collection_controls: &Rc<dyn Fn()>,
    on_library_changed: &Rc<dyn Fn()>,
) {
    {
        let mut lib = library.borrow_mut();
        lib.collections
            .retain(|collection| collection.id != collection_id);
    }
    collection_filter.set_active_id(Some(COLLECTION_NONE_ID));
    refresh_collection_controls();
    refresh_browser_view();
    on_library_changed();
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::clip::SubtitleSegment;
    use crate::model::media_library::{
        MediaAutoTag, MediaAutoTagCategory, MediaBin, MediaKeywordRange,
    };

    fn make_video_item(path: &str) -> MediaItem {
        let mut item = MediaItem::new(path, 83_000_000_000);
        item.is_missing = false;
        item.has_audio = true;
        item.video_width = Some(3840);
        item.video_height = Some(2160);
        item.frame_rate_num = Some(24000);
        item.frame_rate_den = Some(1001);
        item.codec_summary = Some("H.264 / AAC".to_string());
        item.file_size_bytes = Some(5 * 1024 * 1024);
        item
    }

    fn make_title_item(title_text: &str) -> MediaItem {
        let mut item = MediaItem::new("", 4_000_000_000);
        item.id = "title-item".to_string();
        item.is_missing = false;
        item.label = "Lower Third".to_string();
        item.clip_kind = Some(ClipKind::Title);
        item.title_text = Some(title_text.to_string());
        item
    }

    fn add_transcript(item: &mut MediaItem, text: &str) {
        item.upsert_transcript_window(
            0,
            item.duration_ns,
            vec![SubtitleSegment {
                id: "seg-1".to_string(),
                start_ns: 0,
                end_ns: 1_000_000_000,
                text: text.to_string(),
                words: Vec::new(),
            }],
        );
    }

    fn add_auto_tag(
        item: &mut MediaItem,
        category: MediaAutoTagCategory,
        label: &str,
        confidence: f32,
    ) {
        let mut auto_tags = item.auto_tags.clone();
        auto_tags.push(
            MediaAutoTag::new(category, label, confidence, Some(1_000_000_000)).expect("auto tag"),
        );
        item.upsert_auto_tags(auto_tags);
    }

    #[test]
    fn media_secondary_text_formats_codec_duration_and_size() {
        let item = make_video_item("/tmp/clip.mov");
        assert_eq!(
            media_primary_text(&item).as_deref(),
            Some("3840x2160 • 23.98 fps")
        );
        assert_eq!(
            media_secondary_text(&item).as_deref(),
            Some("H.264 / AAC • 1:23 • 5.0 MB")
        );
    }

    #[test]
    fn media_display_key_tracks_rating_and_keywords() {
        let mut item = make_video_item("/tmp/clip.mov");
        let base_key = media_display_key(&item);

        item.rating = MediaRating::Favorite;
        assert_ne!(media_display_key(&item), base_key);

        item.rating = MediaRating::None;
        item.keyword_ranges
            .push(MediaKeywordRange::new("B-roll", 250_000_000, 750_000_000));
        assert_ne!(media_display_key(&item), base_key);

        let keyword_key = media_display_key(&item);
        add_transcript(&mut item, "Find the sticker");
        assert_ne!(media_display_key(&item), keyword_key);

        let transcript_key = media_display_key(&item);
        add_auto_tag(&mut item, MediaAutoTagCategory::Setting, "outdoor", 0.81);
        assert_ne!(media_display_key(&item), transcript_key);
    }

    #[test]
    fn media_tooltip_text_includes_rating_and_keywords() {
        let mut item = make_video_item("/tmp/clip.mov");
        item.rating = MediaRating::Favorite;
        item.keyword_ranges.push(MediaKeywordRange::new(
            "Close Up",
            250_000_000,
            1_250_000_000,
        ));

        let tooltip = media_tooltip_text(&item, None);
        assert!(tooltip.contains("Rating: Favorite"));
        assert!(tooltip.contains("Keyword: Close Up"));

        add_auto_tag(&mut item, MediaAutoTagCategory::Subject, "person", 0.74);
        let tooltip = media_tooltip_text(&item, None);
        assert!(tooltip.contains("Tag: Subject — person"));
    }

    #[test]
    fn media_matches_filters_by_search_kind_and_resolution() {
        let item = make_video_item("/tmp/dialog_take.mov");
        let filters = MediaFilterCriteria {
            search_text: "dialog".to_string(),
            kind: MediaKindFilter::Video,
            resolution: ResolutionFilter::UltraHd,
            ..Default::default()
        };
        assert!(media_matches_filters(&item, &filters));

        let filters = MediaFilterCriteria {
            search_text: "aac".to_string(),
            kind: MediaKindFilter::Audio,
            resolution: ResolutionFilter::All,
            ..Default::default()
        };
        assert!(!media_matches_filters(&item, &filters));
    }

    #[test]
    fn title_items_show_title_text_and_search_by_it() {
        let item = make_title_item("Jane Doe");
        assert_eq!(media_display_name(&item), "Jane Doe");
        assert_eq!(
            media_primary_text(&item).as_deref(),
            Some("Title clip • Lower Third")
        );
        assert_eq!(media_secondary_text(&item).as_deref(), Some("0:04"));
        assert!(media_tooltip_text(&item, None).contains("Text: Jane Doe"));

        let filters = MediaFilterCriteria {
            search_text: "jane".to_string(),
            kind: MediaKindFilter::All,
            resolution: ResolutionFilter::All,
            ..Default::default()
        };
        assert!(media_matches_filters(&item, &filters));
    }

    #[test]
    fn transcript_search_hint_and_tooltip_show_spoken_match() {
        let mut item = make_video_item("/tmp/dialog.mov");
        add_transcript(&mut item, "Find the sticker on the table");

        let hint = media_search_hint(&item, "sticker").expect("expected spoken search hint");
        assert!(hint.contains("Spoken:"));
        assert!(hint.contains("[sticker]"));

        let tooltip = media_tooltip_text(&item, Some("sticker"));
        assert!(tooltip.contains("Search hit: spoken content"));
        assert!(tooltip.contains("Match: Find the [sticker]"));
    }

    #[test]
    fn auto_tag_search_hint_and_tooltip_show_tag_match() {
        let mut item = make_video_item("/tmp/outdoor.mov");
        add_auto_tag(&mut item, MediaAutoTagCategory::Setting, "outdoor", 0.79);
        add_auto_tag(&mut item, MediaAutoTagCategory::ShotType, "wide", 0.83);

        let hint = media_search_hint(&item, "outdoor wide").expect("expected tag search hint");
        assert!(hint.contains("Tags:"));

        let tooltip = media_tooltip_text(&item, Some("outdoor wide"));
        assert!(tooltip.contains("Search hit: auto tag"));
        assert!(tooltip.contains("Match:"));
    }

    #[test]
    fn build_expected_entries_sorts_search_results_by_relevance() {
        let mut lib = MediaLibrary::new();

        let mut exact = make_video_item("/tmp/sticker.mov");
        exact.label = "Sticker Interview".to_string();
        add_transcript(&mut exact, "Find the sticker on the table");
        let exact_id = exact.id.clone();
        lib.items.push(exact);

        let mut transcript_only = make_video_item("/tmp/alt.mov");
        transcript_only.label = "Alternate take".to_string();
        add_transcript(&mut transcript_only, "A sticker is hidden in frame");
        let transcript_id = transcript_only.id.clone();
        lib.items.push(transcript_only);

        let filters = MediaFilterCriteria {
            search_text: "sticker".to_string(),
            ..Default::default()
        };
        let entries = build_expected_entries(&lib, &None, true, &filters);
        assert!(matches!(
            entries.first(),
            Some(FlowBoxEntry::Media { item_id, .. }) if item_id == &exact_id
        ));
        assert!(matches!(
            entries.get(1),
            Some(FlowBoxEntry::Media { item_id, .. }) if item_id == &transcript_id
        ));
    }

    #[test]
    fn build_expected_entries_keeps_bins_while_filtering_items() {
        let bin = MediaBin::new("Dialogue", None);
        let mut lib = MediaLibrary::new();
        lib.bins.push(bin.clone());

        let mut root_item = make_video_item("/tmp/broll.mov");
        root_item.label = "Broll".to_string();
        lib.items.push(root_item);

        let mut bin_item = make_video_item("/tmp/dialog.mov");
        bin_item.label = "Dialog".to_string();
        bin_item.bin_id = Some(bin.id.clone());
        let bin_item_id = bin_item.id.clone();
        lib.items.push(bin_item);

        let filters = MediaFilterCriteria {
            search_text: "dialog".to_string(),
            kind: MediaKindFilter::All,
            resolution: ResolutionFilter::All,
            ..Default::default()
        };

        let root_entries = build_expected_entries(&lib, &None, false, &filters);
        assert!(matches!(
            root_entries.first(),
            Some(FlowBoxEntry::Bin { .. })
        ));
        assert_eq!(root_entries.len(), 1);

        let bin_entries = build_expected_entries(&lib, &Some(bin.id), false, &filters);
        assert_eq!(bin_entries.len(), 1);
        assert!(matches!(
            bin_entries.first(),
            Some(FlowBoxEntry::Media { item_id, .. }) if item_id == &bin_item_id
        ));
    }

    #[test]
    fn media_matches_filters_by_frame_rate_bucket() {
        let item = make_video_item("/tmp/hfr.mov");
        let filters = MediaFilterCriteria {
            frame_rate: FrameRateFilter::Fps24OrLess,
            ..Default::default()
        };
        assert!(media_matches_filters(&item, &filters));

        let filters = MediaFilterCriteria {
            frame_rate: FrameRateFilter::Fps60Plus,
            ..Default::default()
        };
        assert!(!media_matches_filters(&item, &filters));
    }
}
