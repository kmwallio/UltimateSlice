use crate::fcpxml;
use crate::media::export::{
    export_project, AudioCodec, Container, ExportOptions, ExportProgress, VideoCodec,
};
use crate::model::media_library::MediaItem;
use crate::model::project::{FrameRate, Project};
use crate::recent;
use crate::ui::timeline::{ActiveTool, TimelineState};
use crate::ui_state::{self, ExportPreset, ExportPresetsState};
use gio;
use glib;
use gtk4::prelude::*;
use gtk4::{self as gtk, Button, HeaderBar, Label, Separator, ToggleButton};
use std::cell::RefCell;
use std::rc::Rc;

fn save_project_to_path(
    project: &Rc<RefCell<Project>>,
    path: &std::path::Path,
) -> Result<(), String> {
    let xml = {
        let proj = project.borrow();
        fcpxml::writer::write_fcpxml_for_path(&proj, path)
            .map_err(|e| format!("FCPXML write error: {e}"))?
    };
    std::fs::write(path, xml).map_err(|e| format!("Save error: {e}"))?;
    if let Some(p) = path.to_str() {
        recent::push(p);
    }
    {
        let mut proj = project.borrow_mut();
        proj.file_path = Some(path.to_string_lossy().to_string());
        proj.dirty = false;
    }
    Ok(())
}

enum ExportProjectWithMediaUiEvent {
    Progress(fcpxml::writer::ExportProjectWithMediaProgress),
    Done { library_dir: std::path::PathBuf },
    Error(String),
}

fn video_codec_from_selected(selected: u32) -> VideoCodec {
    match selected {
        0 => VideoCodec::H264,
        1 => VideoCodec::H265,
        2 => VideoCodec::Vp9,
        3 => VideoCodec::ProRes,
        4 => VideoCodec::Av1,
        _ => VideoCodec::H264,
    }
}

fn selected_from_video_codec(codec: &VideoCodec) -> u32 {
    match codec {
        VideoCodec::H264 => 0,
        VideoCodec::H265 => 1,
        VideoCodec::Vp9 => 2,
        VideoCodec::ProRes => 3,
        VideoCodec::Av1 => 4,
    }
}

fn container_from_selected(selected: u32) -> Container {
    match selected {
        0 => Container::Mp4,
        1 => Container::Mov,
        2 => Container::WebM,
        3 => Container::Mkv,
        _ => Container::Mp4,
    }
}

fn selected_from_container(container: &Container) -> u32 {
    match container {
        Container::Mp4 => 0,
        Container::Mov => 1,
        Container::WebM => 2,
        Container::Mkv => 3,
    }
}

fn output_resolution_from_selected(selected: u32) -> (u32, u32) {
    match selected {
        0 => (0, 0),
        1 => (3840, 2160),
        2 => (1920, 1080),
        3 => (1280, 720),
        4 => (854, 480),
        _ => (0, 0),
    }
}

fn selected_from_output_resolution(width: u32, height: u32) -> u32 {
    match (width, height) {
        (0, 0) => 0,
        (3840, 2160) => 1,
        (1920, 1080) => 2,
        (1280, 720) => 3,
        (854, 480) => 4,
        _ => 0,
    }
}

fn audio_codec_from_selected(selected: u32) -> AudioCodec {
    match selected {
        0 => AudioCodec::Aac,
        1 => AudioCodec::Opus,
        2 => AudioCodec::Flac,
        3 => AudioCodec::Pcm,
        _ => AudioCodec::Aac,
    }
}

fn selected_from_audio_codec(codec: &AudioCodec) -> u32 {
    match codec {
        AudioCodec::Aac => 0,
        AudioCodec::Opus => 1,
        AudioCodec::Flac => 2,
        AudioCodec::Pcm => 3,
    }
}

fn collect_export_options(
    vc_combo: &gtk::DropDown,
    ct_combo: &gtk::DropDown,
    or_combo: &gtk::DropDown,
    crf_slider: &gtk::Scale,
    ac_combo: &gtk::DropDown,
    ab_entry: &gtk::Entry,
) -> ExportOptions {
    let (output_width, output_height) = output_resolution_from_selected(or_combo.selected());
    ExportOptions {
        video_codec: video_codec_from_selected(vc_combo.selected()),
        container: container_from_selected(ct_combo.selected()),
        output_width,
        output_height,
        crf: crf_slider.value() as u32,
        audio_codec: audio_codec_from_selected(ac_combo.selected()),
        audio_bitrate_kbps: ab_entry.text().parse::<u32>().unwrap_or(192),
    }
}

fn apply_export_options(
    options: &ExportOptions,
    vc_combo: &gtk::DropDown,
    ct_combo: &gtk::DropDown,
    or_combo: &gtk::DropDown,
    crf_slider: &gtk::Scale,
    ac_combo: &gtk::DropDown,
    ab_entry: &gtk::Entry,
) {
    vc_combo.set_selected(selected_from_video_codec(&options.video_codec));
    ct_combo.set_selected(selected_from_container(&options.container));
    or_combo.set_selected(selected_from_output_resolution(
        options.output_width,
        options.output_height,
    ));
    crf_slider.set_value(options.crf as f64);
    ac_combo.set_selected(selected_from_audio_codec(&options.audio_codec));
    ab_entry.set_text(&options.audio_bitrate_kbps.to_string());
}

fn refresh_preset_dropdown(
    dropdown: &gtk::DropDown,
    state: &ExportPresetsState,
    selected_name: Option<&str>,
) {
    let model = gtk::StringList::new(&[]);
    model.append("(Custom)");
    for preset in &state.presets {
        model.append(&preset.name);
    }
    dropdown.set_model(Some(&model));
    let mut selected = 0_u32;
    if let Some(name) = selected_name {
        if let Some((idx, _)) = state
            .presets
            .iter()
            .enumerate()
            .find(|(_, p)| p.name.eq_ignore_ascii_case(name))
        {
            selected = (idx + 1) as u32;
        }
    }
    dropdown.set_selected(selected);
}

#[allow(deprecated)]
pub fn confirm_unsaved_then(
    window: Option<gtk::Window>,
    project: Rc<RefCell<Project>>,
    on_project_changed: Rc<dyn Fn()>,
    on_continue: Rc<dyn Fn()>,
) {
    if !project.borrow().dirty {
        on_continue();
        return;
    }

    let dialog = gtk::Dialog::builder()
        .title("Unsaved changes")
        .modal(true)
        .default_width(420)
        .build();
    dialog.set_transient_for(window.as_ref());
    dialog.add_button("Cancel", gtk::ResponseType::Cancel);
    dialog.add_button("Discard", gtk::ResponseType::Reject);
    dialog.add_button("Save…", gtk::ResponseType::Accept);
    let label = gtk::Label::new(Some(
        "You have unsaved changes. Save the current project before continuing?",
    ));
    label.set_wrap(true);
    label.set_margin_start(16);
    label.set_margin_end(16);
    label.set_margin_top(16);
    label.set_margin_bottom(16);
    dialog.content_area().append(&label);

    let project_c = project.clone();
    let on_project_changed_c = on_project_changed.clone();
    let on_continue_c = on_continue.clone();
    dialog.connect_response(move |d, resp| match resp {
        gtk::ResponseType::Reject => {
            d.close();
            on_continue_c();
        }
        gtk::ResponseType::Accept => {
            d.close();
            let existing_path = project_c.borrow().file_path.clone();
            if let Some(path) = existing_path {
                match save_project_to_path(&project_c, std::path::Path::new(&path)) {
                    Ok(()) => {
                        on_project_changed_c();
                        on_continue_c();
                    }
                    Err(e) => eprintln!("{e}"),
                }
            } else {
                let file_dialog = gtk::FileDialog::new();
                file_dialog.set_title("Save Project XML");
                file_dialog.set_initial_name(Some("project.uspxml"));
                let filter = gtk::FileFilter::new();
                filter.add_pattern("*.uspxml");
                filter.add_pattern("*.fcpxml");
                filter.set_name(Some("Project XML Files"));
                let filters = gio::ListStore::new::<gtk::FileFilter>();
                filters.append(&filter);
                file_dialog.set_filters(Some(&filters));
                let project_s = project_c.clone();
                let on_project_changed_s = on_project_changed_c.clone();
                let on_continue_s = on_continue_c.clone();
                file_dialog.save(window.as_ref(), gio::Cancellable::NONE, move |result| {
                    if let Ok(file) = result {
                        if let Some(path) = file.path() {
                            match save_project_to_path(&project_s, &path) {
                                Ok(()) => {
                                    on_project_changed_s();
                                    on_continue_s();
                                }
                                Err(e) => eprintln!("{e}"),
                            }
                        }
                    }
                });
            }
        }
        _ => d.close(),
    });
    dialog.present();
}

/// Build the main `HeaderBar` toolbar.
#[allow(deprecated)]
pub fn build_toolbar(
    project: Rc<RefCell<Project>>,
    _library: Rc<RefCell<Vec<MediaItem>>>,
    timeline_state: Rc<RefCell<TimelineState>>,
    bg_removal_cache: Rc<RefCell<crate::media::bg_removal_cache::BgRemovalCache>>,
    on_project_changed: impl Fn() + 'static + Clone,
    on_project_reloaded: impl Fn() + 'static + Clone,
    on_export_frame: impl Fn() + 'static + Clone,
) -> HeaderBar {
    let header = HeaderBar::new();

    let title = Label::new(Some("UltimateSlice"));
    title.add_css_class("title");
    header.set_title_widget(Some(&title));

    // New project
    let btn_new = Button::with_label("New");
    btn_new.set_tooltip_text(Some("New project (Ctrl+N)"));
    {
        let project = project.clone();
        let timeline_state = timeline_state.clone();
        let on_project_changed = on_project_changed.clone();
        let on_project_reloaded = on_project_reloaded.clone();
        btn_new.connect_clicked(move |btn| {
            let window = btn.root().and_then(|r| r.downcast::<gtk::Window>().ok());
            let on_project_changed_cb: Rc<dyn Fn()> = Rc::new(on_project_changed.clone());
            let action: Rc<dyn Fn()> = Rc::new({
                let project = project.clone();
                let timeline_state = timeline_state.clone();
                let on_project_changed = on_project_changed.clone();
                let on_project_reloaded = on_project_reloaded.clone();
                move || {
                    *project.borrow_mut() = Project::new("Untitled");
                    {
                        let mut st = timeline_state.borrow_mut();
                        st.playhead_ns = 0;
                        st.scroll_offset = 0.0;
                        st.pixels_per_second = 100.0;
                        st.selected_clip_id = None;
                        st.selected_track_id = None;
                    }
                    on_project_reloaded();
                    on_project_changed();
                }
            });
            confirm_unsaved_then(window, project.clone(), on_project_changed_cb, action);
        });
    }
    header.pack_start(&btn_new);

    // Open project XML
    let btn_open = Button::with_label("Open…");
    btn_open.set_tooltip_text(Some("Open project XML (Ctrl+O)"));
    {
        let project = project.clone();
        let timeline_state = timeline_state.clone();
        let on_project_changed = on_project_changed.clone();
        let on_project_reloaded = on_project_reloaded.clone();
        btn_open.connect_clicked(move |btn| {
            let window = btn.root().and_then(|r| r.downcast::<gtk::Window>().ok());
            let on_project_changed_cb: Rc<dyn Fn()> = Rc::new(on_project_changed.clone());
            let action: Rc<dyn Fn()> = Rc::new({
                let project = project.clone();
                let on_project_changed = on_project_changed.clone();
                let on_project_reloaded = on_project_reloaded.clone();
                let timeline_state_cb = timeline_state.clone();
                let window = window.clone();
                move || {
                    let dialog = gtk::FileDialog::new();
                    dialog.set_title("Open Project XML");

                    let filter = gtk::FileFilter::new();
                    filter.add_pattern("*.uspxml");
                    filter.add_pattern("*.fcpxml");
                    filter.add_pattern("*.xml");
                    filter.set_name(Some("Project XML Files"));
                    let filters = gio::ListStore::new::<gtk::FileFilter>();
                    filters.append(&filter);
                    dialog.set_filters(Some(&filters));

                    let project = project.clone();
                    let on_project_changed = on_project_changed.clone();
                    let on_project_reloaded = on_project_reloaded.clone();
                    let timeline_state_cb = timeline_state_cb.clone();
                    let window = window.clone();
                    dialog.open(window.as_ref(), gio::Cancellable::NONE, move |result| {
                        if let Ok(file) = result {
                            if let Some(path) = file.path() {
                                let path_str = path.to_string_lossy().to_string();
                                // Parse FCPXML on a background thread to avoid blocking the UI.
                                let (tx, rx) =
                                    std::sync::mpsc::sync_channel::<Result<Project, String>>(1);
                                let path_bg = path_str.clone();
                                std::thread::spawn(move || {
                                    let result = std::fs::read_to_string(&path_bg)
                                        .map_err(|e| format!("Failed to read file: {e}"))
                                        .and_then(|xml| {
                                            fcpxml::parser::parse_fcpxml_with_path(
                                                &xml,
                                                Some(std::path::Path::new(&path_bg)),
                                            )
                                            .map_err(|e| format!("FCPXML parse error: {e}"))
                                        });
                                    let _ = tx.send(result);
                                });
                                let project = project.clone();
                                let on_project_changed = on_project_changed.clone();
                                let on_project_reloaded = on_project_reloaded.clone();
                                let timeline_state_cb = timeline_state_cb.clone();
                                // Suppress timeline interaction while loading.
                                timeline_state_cb.borrow_mut().loading = true;
                                glib::timeout_add_local(
                                    std::time::Duration::from_millis(50),
                                    move || match rx.try_recv() {
                                        Ok(Ok(mut new_proj)) => {
                                            new_proj.file_path = Some(path_str.clone());
                                            recent::push(&path_str);
                                            *project.borrow_mut() = new_proj;
                                            {
                                                let mut st = timeline_state_cb.borrow_mut();
                                                st.playhead_ns = 0;
                                                st.scroll_offset = 0.0;
                                                st.pixels_per_second = 100.0;
                                                st.selected_clip_id = None;
                                                st.selected_track_id = None;
                                                st.loading = false;
                                            }
                                            on_project_reloaded();
                                            on_project_changed();
                                            glib::ControlFlow::Break
                                        }
                                        Ok(Err(e)) => {
                                            eprintln!("{e}");
                                            timeline_state_cb.borrow_mut().loading = false;
                                            glib::ControlFlow::Break
                                        }
                                        Err(std::sync::mpsc::TryRecvError::Empty) => {
                                            glib::ControlFlow::Continue
                                        }
                                        Err(_) => {
                                            timeline_state_cb.borrow_mut().loading = false;
                                            glib::ControlFlow::Break
                                        }
                                    },
                                );
                            }
                        }
                    });
                }
            });
            confirm_unsaved_then(window, project.clone(), on_project_changed_cb, action);
        });
    }
    header.pack_start(&btn_open);

    // Open Recent — popover with the last 10 projects
    let btn_recent = gtk::MenuButton::new();
    btn_recent.set_label("Recent");
    btn_recent.set_tooltip_text(Some("Open a recently used project"));
    {
        let project = project.clone();
        let timeline_state = timeline_state.clone();
        let on_project_changed = on_project_changed.clone();
        let on_project_reloaded = on_project_reloaded.clone();

        // Build the popover upfront so MenuButton can show it immediately.
        // Repopulate the inner box each time the popover opens (connect_show)
        // so the list reflects any projects opened during this session.
        let pop = gtk::Popover::new();
        let vbox = gtk::Box::new(gtk::Orientation::Vertical, 2);
        vbox.set_margin_start(4);
        vbox.set_margin_end(4);
        vbox.set_margin_top(4);
        vbox.set_margin_bottom(4);
        pop.set_child(Some(&vbox));
        btn_recent.set_popover(Some(&pop));

        let vbox_ref = vbox.clone();
        pop.connect_show(move |pop| {
            // Clear previous children
            while let Some(child) = vbox_ref.first_child() {
                vbox_ref.remove(&child);
            }

            let entries = recent::load();
            if entries.is_empty() {
                let empty = gtk::Label::new(Some("No recent projects"));
                empty.add_css_class("dim-label");
                empty.set_margin_start(8);
                empty.set_margin_end(8);
                empty.set_margin_top(4);
                empty.set_margin_bottom(4);
                vbox_ref.append(&empty);
            } else {
                for path_str in &entries {
                    let display = std::path::Path::new(path_str)
                        .file_name()
                        .and_then(|n| n.to_str())
                        .unwrap_or(path_str)
                        .to_string();
                    let row = gtk::Button::with_label(&display);
                    row.set_tooltip_text(Some(path_str));
                    row.add_css_class("flat");
                    row.set_halign(gtk::Align::Fill);
                    row.set_hexpand(true);

                    let path_owned = path_str.clone();
                    let project = project.clone();
                    let timeline_state = timeline_state.clone();
                    let on_project_changed = on_project_changed.clone();
                    let on_project_reloaded = on_project_reloaded.clone();
                    let pop_weak = pop.downgrade();
                    let row_for_window = row.clone();
                    row.connect_clicked(move |_| {
                        let on_project_changed_cb: Rc<dyn Fn()> =
                            Rc::new(on_project_changed.clone());
                        let action: Rc<dyn Fn()> = Rc::new({
                            let project = project.clone();
                            let timeline_state = timeline_state.clone();
                            let on_project_changed = on_project_changed.clone();
                            let on_project_reloaded = on_project_reloaded.clone();
                            let path_owned = path_owned.clone();
                            let pop_weak = pop_weak.clone();
                            move || {
                                if let Some(pop) = pop_weak.upgrade() {
                                    pop.popdown();
                                }
                                // Parse FCPXML on a background thread to avoid blocking the UI.
                                let (tx, rx) =
                                    std::sync::mpsc::sync_channel::<Result<Project, String>>(1);
                                let path_bg = path_owned.clone();
                                std::thread::spawn(move || {
                                    let result = std::fs::read_to_string(&path_bg)
                                        .map_err(|e| format!("Failed to open recent project: {e}"))
                                        .and_then(|xml| {
                                            fcpxml::parser::parse_fcpxml_with_path(
                                                &xml,
                                                Some(std::path::Path::new(&path_bg)),
                                            )
                                            .map_err(|e| format!("FCPXML parse error: {e}"))
                                        });
                                    let _ = tx.send(result);
                                });
                                let project = project.clone();
                                let timeline_state = timeline_state.clone();
                                let on_project_changed = on_project_changed.clone();
                                let on_project_reloaded = on_project_reloaded.clone();
                                let path_owned = path_owned.clone();
                                // Suppress timeline interaction while loading.
                                timeline_state.borrow_mut().loading = true;
                                glib::timeout_add_local(
                                    std::time::Duration::from_millis(50),
                                    move || match rx.try_recv() {
                                        Ok(Ok(mut new_proj)) => {
                                            new_proj.file_path = Some(path_owned.clone());
                                            recent::push(&path_owned);
                                            *project.borrow_mut() = new_proj;
                                            {
                                                let mut st = timeline_state.borrow_mut();
                                                st.playhead_ns = 0;
                                                st.scroll_offset = 0.0;
                                                st.pixels_per_second = 100.0;
                                                st.selected_clip_id = None;
                                                st.selected_track_id = None;
                                                st.loading = false;
                                            }
                                            on_project_reloaded();
                                            on_project_changed();
                                            glib::ControlFlow::Break
                                        }
                                        Ok(Err(e)) => {
                                            eprintln!("{e}");
                                            timeline_state.borrow_mut().loading = false;
                                            glib::ControlFlow::Break
                                        }
                                        Err(std::sync::mpsc::TryRecvError::Empty) => {
                                            glib::ControlFlow::Continue
                                        }
                                        Err(_) => {
                                            timeline_state.borrow_mut().loading = false;
                                            glib::ControlFlow::Break
                                        }
                                    },
                                );
                            }
                        });
                        let window = row_for_window
                            .root()
                            .and_then(|r| r.downcast::<gtk::Window>().ok());
                        confirm_unsaved_then(
                            window,
                            project.clone(),
                            on_project_changed_cb,
                            action,
                        );
                    });
                    vbox_ref.append(&row);
                }
            }
        });
    }
    header.pack_start(&btn_recent);
    let btn_save = Button::with_label("Save…");
    btn_save.set_tooltip_text(Some("Save project XML (Ctrl+S)"));
    {
        let project = project.clone();
        let on_project_changed = on_project_changed.clone();
        btn_save.connect_clicked(move |btn| {
            let dialog = gtk::FileDialog::new();
            dialog.set_title("Save Project XML");
            dialog.set_initial_name(Some("project.uspxml"));

            let filter = gtk::FileFilter::new();
            filter.add_pattern("*.uspxml");
            filter.add_pattern("*.fcpxml");
            filter.set_name(Some("Project XML Files"));
            let filters = gio::ListStore::new::<gtk::FileFilter>();
            filters.append(&filter);
            dialog.set_filters(Some(&filters));

            let project = project.clone();
            let on_project_changed = on_project_changed.clone();
            let window = btn.root().and_then(|r| r.downcast::<gtk::Window>().ok());

            dialog.save(window.as_ref(), gio::Cancellable::NONE, move |result| {
                if let Ok(file) = result {
                    if let Some(path) = file.path() {
                        match save_project_to_path(&project, &path) {
                            Ok(()) => {
                                println!("Saved to {}", path.display());
                                on_project_changed();
                            }
                            Err(e) => {
                                eprintln!("{e}");
                            }
                        }
                    }
                }
            });
        });
    }
    header.pack_start(&btn_save);

    // ── Project Settings ─────────────────────────────────────────────────
    let btn_settings = Button::with_label("⚙ Settings");
    btn_settings.set_tooltip_text(Some("Project canvas size and frame rate"));
    {
        let project = project.clone();
        let on_project_changed = on_project_changed.clone();
        btn_settings.connect_clicked(move |btn| {
            let window = btn.root().and_then(|r| r.downcast::<gtk::Window>().ok());
            let proj = project.borrow();

            let dialog = gtk::Dialog::builder()
                .title("Project Settings")
                .default_width(400)
                .build();
            dialog.set_transient_for(window.as_ref());
            dialog.set_modal(true);

            let grid = gtk::Grid::new();
            grid.set_margin_start(16);
            grid.set_margin_end(16);
            grid.set_margin_top(16);
            grid.set_margin_bottom(16);
            grid.set_row_spacing(10);
            grid.set_column_spacing(12);

            // ── Aspect ratio / resolution presets ──
            // Each aspect ratio group: (label, [(width, height, display_label)])
            let ar_presets: Vec<(&str, Vec<(u32, u32, &str)>)> = vec![
                ("16:9 (Widescreen)", vec![
                    (3840, 2160, "3840 × 2160  (4K UHD)"),
                    (2560, 1440, "2560 × 1440  (1440p QHD)"),
                    (1920, 1080, "1920 × 1080  (1080p HD)"),
                    (1280, 720,  "1280 × 720   (720p HD)"),
                ]),
                ("4:3 (Standard)", vec![
                    (1440, 1080, "1440 × 1080  (HD 4:3)"),
                    (1024, 768,  "1024 × 768   (XGA)"),
                    (720, 480,   "720 × 480    (SD NTSC)"),
                ]),
                ("9:16 (Vertical)", vec![
                    (1080, 1920, "1080 × 1920  (Full HD Vertical)"),
                    (720, 1280,  "720 × 1280   (HD Vertical)"),
                ]),
                ("1:1 (Square)", vec![
                    (2160, 2160, "2160 × 2160  (4K Square)"),
                    (1080, 1080, "1080 × 1080  (HD Square)"),
                ]),
            ];

            // Detect current aspect ratio and resolution index
            let (init_ar_idx, init_res_idx) = {
                let mut found = (4u32, 0u32); // default: Custom
                'outer: for (ai, (_label, resolutions)) in ar_presets.iter().enumerate() {
                    for (ri, &(w, h, _)) in resolutions.iter().enumerate() {
                        if w == proj.width && h == proj.height {
                            found = (ai as u32, ri as u32);
                            break 'outer;
                        }
                    }
                }
                found
            };

            // Row 0: Aspect Ratio dropdown
            let ar_label = gtk::Label::new(Some("Aspect Ratio:"));
            ar_label.set_halign(gtk::Align::End);
            let ar_combo = gtk::DropDown::from_strings(&[
                "16:9 (Widescreen)",
                "4:3 (Standard)",
                "9:16 (Vertical)",
                "1:1 (Square)",
                "Custom",
            ]);
            grid.attach(&ar_label, 0, 0, 1, 1);
            grid.attach(&ar_combo, 1, 0, 2, 1);

            // Row 1: Resolution dropdown (hidden when Custom)
            let res_label = gtk::Label::new(Some("Resolution:"));
            res_label.set_halign(gtk::Align::End);
            let initial_res_strings: Vec<&str> = if (init_ar_idx as usize) < ar_presets.len() {
                ar_presets[init_ar_idx as usize].1.iter().map(|r| r.2).collect()
            } else {
                vec!["1920 × 1080  (1080p HD)"]
            };
            let res_string_list = gtk::StringList::new(&initial_res_strings.iter().map(|s| *s).collect::<Vec<&str>>());
            let res_combo = gtk::DropDown::builder()
                .model(&res_string_list)
                .build();
            grid.attach(&res_label, 0, 1, 1, 1);
            grid.attach(&res_combo, 1, 1, 2, 1);

            // Row 2: Custom W×H spin buttons (visible only when Custom)
            let w_label = gtk::Label::new(Some("Width:"));
            w_label.set_halign(gtk::Align::End);
            let w_spin = gtk::SpinButton::with_range(128.0, 7680.0, 2.0);
            w_spin.set_value(proj.width as f64);
            let h_label = gtk::Label::new(Some("Height:"));
            h_label.set_halign(gtk::Align::End);
            let h_spin = gtk::SpinButton::with_range(128.0, 4320.0, 2.0);
            h_spin.set_value(proj.height as f64);
            let custom_box = gtk::Box::new(gtk::Orientation::Horizontal, 8);
            custom_box.append(&w_label);
            custom_box.append(&w_spin);
            custom_box.append(&h_label);
            custom_box.append(&h_spin);
            grid.attach(&custom_box, 0, 2, 3, 1);

            // Show/hide based on initial state
            let is_custom = init_ar_idx == 4;
            res_label.set_visible(!is_custom);
            res_combo.set_visible(!is_custom);
            custom_box.set_visible(is_custom);

            // Set initial selections BEFORE connecting signals
            ar_combo.set_selected(init_ar_idx);
            if !is_custom {
                res_combo.set_selected(init_res_idx);
            }

            // Wire aspect ratio change → repopulate resolution list
            {
                let res_combo = res_combo.clone();
                let res_label = res_label.clone();
                let custom_box = custom_box.clone();
                let ar_presets_labels: Vec<Vec<String>> = ar_presets.iter()
                    .map(|(_, resolutions)| resolutions.iter().map(|r| r.2.to_string()).collect())
                    .collect();
                ar_combo.connect_selected_notify(move |combo| {
                    let idx = combo.selected() as usize;
                    if idx < ar_presets_labels.len() {
                        // Preset aspect ratio: show resolution dropdown, hide custom
                        let labels: Vec<&str> = ar_presets_labels[idx].iter().map(|s| s.as_str()).collect();
                        let new_model = gtk::StringList::new(&labels);
                        res_combo.set_model(Some(&new_model));
                        res_combo.set_selected(0);
                        res_label.set_visible(true);
                        res_combo.set_visible(true);
                        custom_box.set_visible(false);
                    } else {
                        // Custom: hide resolution dropdown, show spin buttons
                        res_label.set_visible(false);
                        res_combo.set_visible(false);
                        custom_box.set_visible(true);
                    }
                });
            }

            // Row 3: Frame rate preset (unchanged)
            let fps_label = gtk::Label::new(Some("Frame Rate:"));
            fps_label.set_halign(gtk::Align::End);
            let fps_combo = gtk::DropDown::from_strings(&[
                "23.976 fps",
                "24 fps",
                "25 fps",
                "29.97 fps",
                "30 fps",
                "60 fps",
            ]);
            let fps_idx = match (proj.frame_rate.numerator, proj.frame_rate.denominator) {
                (24000, 1001) | (2997, 125) => 0,
                (24, 1) => 1,
                (25, 1) => 2,
                (30000, 1001) => 3,
                (30, 1) => 4,
                (60, 1) => 5,
                _ => 1,
            };
            fps_combo.set_selected(fps_idx);
            grid.attach(&fps_label, 0, 3, 1, 1);
            grid.attach(&fps_combo, 1, 3, 2, 1);

            dialog.content_area().append(&grid);
            dialog.add_button("Cancel", gtk::ResponseType::Cancel);
            dialog.add_button("Apply", gtk::ResponseType::Accept);

            // Clone presets data for the response handler
            let ar_res_data: Vec<Vec<(u32, u32)>> = ar_presets.iter()
                .map(|(_, resolutions)| resolutions.iter().map(|r| (r.0, r.1)).collect())
                .collect();

            drop(proj);
            let project = project.clone();
            let on_project_changed = on_project_changed.clone();
            dialog.connect_response(move |d, resp| {
                if resp == gtk::ResponseType::Accept {
                    let ar_idx = ar_combo.selected() as usize;
                    let (w, h) = if ar_idx < ar_res_data.len() {
                        // Preset aspect ratio + resolution
                        let res_idx = res_combo.selected() as usize;
                        if res_idx < ar_res_data[ar_idx].len() {
                            ar_res_data[ar_idx][res_idx]
                        } else {
                            ar_res_data[ar_idx][0]
                        }
                    } else {
                        // Custom
                        (w_spin.value() as u32, h_spin.value() as u32)
                    };
                    let fr = match fps_combo.selected() {
                        0 => FrameRate {
                            numerator: 24000,
                            denominator: 1001,
                        },
                        1 => FrameRate {
                            numerator: 24,
                            denominator: 1,
                        },
                        2 => FrameRate {
                            numerator: 25,
                            denominator: 1,
                        },
                        3 => FrameRate {
                            numerator: 30000,
                            denominator: 1001,
                        },
                        4 => FrameRate {
                            numerator: 30,
                            denominator: 1,
                        },
                        5 => FrameRate {
                            numerator: 60,
                            denominator: 1,
                        },
                        _ => FrameRate {
                            numerator: 24,
                            denominator: 1,
                        },
                    };
                    let mut proj = project.borrow_mut();
                    proj.width = w;
                    proj.height = h;
                    proj.frame_rate = fr;
                    proj.dirty = true;
                    drop(proj);
                    on_project_changed();
                }
                d.close();
            });
            dialog.present();
        });
    }
    header.pack_start(&btn_settings);

    // ── Advanced Export ──────────────────────────────────────────────────
    let btn_export = Button::with_label("Export");
    btn_export.set_tooltip_text(Some("Export with codec and resolution options"));
    btn_export.add_css_class("suggested-action");
    {
        let project = project.clone();
        let bg_removal_cache = bg_removal_cache.clone();
        btn_export.connect_clicked(move |btn| {
            let window = btn.root().and_then(|r| r.downcast::<gtk::Window>().ok());
            let proj_w = project.borrow().width;
            let proj_h = project.borrow().height;

            // ── Export options dialog ──
            let opt_dialog = gtk::Dialog::builder()
                .title("Export Settings")
                .default_width(400)
                .build();
            opt_dialog.set_transient_for(window.as_ref());
            opt_dialog.set_modal(true);

            let grid = gtk::Grid::new();
            grid.set_margin_start(16);
            grid.set_margin_end(16);
            grid.set_margin_top(16);
            grid.set_margin_bottom(16);
            grid.set_row_spacing(10);
            grid.set_column_spacing(12);

            let presets_state = Rc::new(RefCell::new(ui_state::load_export_presets_state()));

            // Preset controls
            let preset_label = gtk::Label::new(Some("Preset:"));
            preset_label.set_halign(gtk::Align::End);
            let preset_box = gtk::Box::new(gtk::Orientation::Horizontal, 6);
            let preset_dropdown = gtk::DropDown::from_strings(&["(Custom)"]);
            preset_dropdown.set_hexpand(true);
            let btn_save_preset = gtk::Button::with_label("Save As…");
            let btn_update_preset = gtk::Button::with_label("Update");
            let btn_delete_preset = gtk::Button::with_label("Delete");
            btn_update_preset.set_sensitive(false);
            btn_delete_preset.set_sensitive(false);
            preset_box.append(&preset_dropdown);
            preset_box.append(&btn_save_preset);
            preset_box.append(&btn_update_preset);
            preset_box.append(&btn_delete_preset);
            grid.attach(&preset_label, 0, 0, 1, 1);
            grid.attach(&preset_box, 1, 0, 1, 1);

            // Video codec
            let vc_label = gtk::Label::new(Some("Video Codec:"));
            vc_label.set_halign(gtk::Align::End);
            let vc_combo = gtk::DropDown::from_strings(&[
                "H.264 (libx264)",
                "H.265 / HEVC (libx265)",
                "VP9 (libvpx-vp9)",
                "ProRes (prores_ks)",
                "AV1 (libaom-av1)",
            ]);
            vc_combo.set_selected(0);
            grid.attach(&vc_label, 0, 1, 1, 1);
            grid.attach(&vc_combo, 1, 1, 1, 1);

            // Container
            let ct_label = gtk::Label::new(Some("Container:"));
            ct_label.set_halign(gtk::Align::End);
            let ct_combo = gtk::DropDown::from_strings(&[
                "MP4 (.mp4)",
                "QuickTime (.mov)",
                "WebM (.webm)",
                "Matroska (.mkv)",
            ]);
            ct_combo.set_selected(0);
            grid.attach(&ct_label, 0, 2, 1, 1);
            grid.attach(&ct_combo, 1, 2, 1, 1);

            // Output resolution
            let or_label = gtk::Label::new(Some("Resolution:"));
            or_label.set_halign(gtk::Align::End);
            let or_combo = gtk::DropDown::from_strings(&[
                &format!("Same as project  ({}×{})", proj_w, proj_h),
                "3840 × 2160  (4K)",
                "1920 × 1080  (1080p)",
                "1280 × 720   (720p)",
                "854 × 480    (480p)",
            ]);
            or_combo.set_selected(0);
            grid.attach(&or_label, 0, 3, 1, 1);
            grid.attach(&or_combo, 1, 3, 1, 1);

            // CRF
            let crf_label = gtk::Label::new(Some("Quality (CRF):"));
            crf_label.set_halign(gtk::Align::End);
            let crf_box = gtk::Box::new(gtk::Orientation::Horizontal, 6);
            let crf_slider = gtk::Scale::with_range(gtk::Orientation::Horizontal, 0.0, 51.0, 1.0);
            crf_slider.set_value(23.0);
            crf_slider.set_hexpand(true);
            crf_slider.set_draw_value(true);
            crf_slider.set_tooltip_text(Some("Lower = better quality / larger file (0–51)"));
            let crf_hint = gtk::Label::new(Some("(lower = better)"));
            crf_hint.add_css_class("dim-label");
            crf_box.append(&crf_slider);
            crf_box.append(&crf_hint);
            grid.attach(&crf_label, 0, 4, 1, 1);
            grid.attach(&crf_box, 1, 4, 1, 1);

            // Audio codec
            let ac_label = gtk::Label::new(Some("Audio Codec:"));
            ac_label.set_halign(gtk::Align::End);
            let ac_combo = gtk::DropDown::from_strings(&[
                "AAC",
                "Opus",
                "FLAC (lossless)",
                "PCM (uncompressed)",
            ]);
            ac_combo.set_selected(0);
            grid.attach(&ac_label, 0, 5, 1, 1);
            grid.attach(&ac_combo, 1, 5, 1, 1);

            // Audio bitrate
            let ab_label = gtk::Label::new(Some("Audio Bitrate:"));
            ab_label.set_halign(gtk::Align::End);
            let ab_entry = gtk::Entry::new();
            ab_entry.set_text("192");
            ab_entry.set_tooltip_text(Some("Audio bitrate in kbps (ignored for FLAC/PCM)"));
            grid.attach(&ab_label, 0, 6, 1, 1);
            grid.attach(&ab_entry, 1, 6, 1, 1);

            {
                let state = presets_state.borrow();
                refresh_preset_dropdown(
                    &preset_dropdown,
                    &state,
                    state.last_used_preset.as_deref(),
                );
                if let Some(name) = state.last_used_preset.as_deref() {
                    if let Some(preset) = state.get_preset(name) {
                        apply_export_options(
                            &preset.to_export_options(),
                            &vc_combo,
                            &ct_combo,
                            &or_combo,
                            &crf_slider,
                            &ac_combo,
                            &ab_entry,
                        );
                    }
                }
            }
            let preset_selected = preset_dropdown.selected() > 0;
            btn_update_preset.set_sensitive(preset_selected);
            btn_delete_preset.set_sensitive(preset_selected);

            {
                let presets_state = presets_state.clone();
                let vc_combo = vc_combo.clone();
                let ct_combo = ct_combo.clone();
                let or_combo = or_combo.clone();
                let crf_slider = crf_slider.clone();
                let ac_combo = ac_combo.clone();
                let ab_entry = ab_entry.clone();
                let btn_update_preset = btn_update_preset.clone();
                let btn_delete_preset = btn_delete_preset.clone();
                preset_dropdown.connect_selected_notify(move |dropdown| {
                    let selected = dropdown.selected();
                    btn_update_preset.set_sensitive(selected > 0);
                    btn_delete_preset.set_sensitive(selected > 0);
                    let mut state = presets_state.borrow_mut();
                    if selected == 0 {
                        state.last_used_preset = None;
                        ui_state::save_export_presets_state(&state);
                        return;
                    }
                    let Some(preset) = state.presets.get((selected - 1) as usize).cloned() else {
                        return;
                    };
                    state.last_used_preset = Some(preset.name.clone());
                    ui_state::save_export_presets_state(&state);
                    drop(state);
                    apply_export_options(
                        &preset.to_export_options(),
                        &vc_combo,
                        &ct_combo,
                        &or_combo,
                        &crf_slider,
                        &ac_combo,
                        &ab_entry,
                    );
                });
            }

            {
                let presets_state = presets_state.clone();
                let preset_dropdown = preset_dropdown.clone();
                let vc_combo = vc_combo.clone();
                let ct_combo = ct_combo.clone();
                let or_combo = or_combo.clone();
                let crf_slider = crf_slider.clone();
                let ac_combo = ac_combo.clone();
                let ab_entry = ab_entry.clone();
                btn_save_preset.connect_clicked(move |_| {
                    let dialog = gtk::Dialog::builder()
                        .title("Save Export Preset")
                        .default_width(360)
                        .modal(true)
                        .build();
                    let entry = gtk::Entry::new();
                    entry.set_placeholder_text(Some("Preset name"));
                    dialog.content_area().append(&entry);
                    dialog.add_button("Cancel", gtk::ResponseType::Cancel);
                    dialog.add_button("Save", gtk::ResponseType::Accept);
                    dialog.connect_response({
                        let presets_state = presets_state.clone();
                        let preset_dropdown = preset_dropdown.clone();
                        let vc_combo = vc_combo.clone();
                        let ct_combo = ct_combo.clone();
                        let or_combo = or_combo.clone();
                        let crf_slider = crf_slider.clone();
                        let ac_combo = ac_combo.clone();
                        let ab_entry = ab_entry.clone();
                        move |d, resp| {
                            if resp == gtk::ResponseType::Accept {
                                let name = entry.text().to_string();
                                let options = collect_export_options(
                                    &vc_combo,
                                    &ct_combo,
                                    &or_combo,
                                    &crf_slider,
                                    &ac_combo,
                                    &ab_entry,
                                );
                                let ok = {
                                    let mut state = presets_state.borrow_mut();
                                    let ok = state
                                        .upsert_preset(ExportPreset::from_export_options(
                                            name, &options,
                                        ))
                                        .is_ok();
                                    if ok {
                                        ui_state::save_export_presets_state(&state);
                                    }
                                    ok
                                };
                                if ok {
                                    let state = presets_state.borrow();
                                    refresh_preset_dropdown(
                                        &preset_dropdown,
                                        &state,
                                        state.last_used_preset.as_deref(),
                                    );
                                }
                            }
                            d.close();
                        }
                    });
                    dialog.present();
                });
            }

            {
                let presets_state = presets_state.clone();
                let preset_dropdown = preset_dropdown.clone();
                let vc_combo = vc_combo.clone();
                let ct_combo = ct_combo.clone();
                let or_combo = or_combo.clone();
                let crf_slider = crf_slider.clone();
                let ac_combo = ac_combo.clone();
                let ab_entry = ab_entry.clone();
                btn_update_preset.connect_clicked(move |_| {
                    let selected = preset_dropdown.selected();
                    if selected == 0 {
                        return;
                    }
                    let ok = {
                        let mut state = presets_state.borrow_mut();
                        let Some(existing_name) = state
                            .presets
                            .get((selected - 1) as usize)
                            .map(|preset| preset.name.clone())
                        else {
                            return;
                        };
                        let options = collect_export_options(
                            &vc_combo,
                            &ct_combo,
                            &or_combo,
                            &crf_slider,
                            &ac_combo,
                            &ab_entry,
                        );
                        let ok = state
                            .upsert_preset(ExportPreset::from_export_options(existing_name, &options))
                            .is_ok();
                        if ok {
                            ui_state::save_export_presets_state(&state);
                        }
                        ok
                    };
                    if ok {
                        let state = presets_state.borrow();
                        refresh_preset_dropdown(
                            &preset_dropdown,
                            &state,
                            state.last_used_preset.as_deref(),
                        );
                    }
                });
            }

            {
                let presets_state = presets_state.clone();
                let preset_dropdown = preset_dropdown.clone();
                btn_delete_preset.connect_clicked(move |_| {
                    let selected = preset_dropdown.selected();
                    if selected == 0 {
                        return;
                    }
                    let ok = {
                        let mut state = presets_state.borrow_mut();
                        let Some(existing_name) = state
                            .presets
                            .get((selected - 1) as usize)
                            .map(|preset| preset.name.clone())
                        else {
                            return;
                        };
                        let ok = state.delete_preset(&existing_name);
                        if ok {
                            ui_state::save_export_presets_state(&state);
                        }
                        ok
                    };
                    if ok {
                        let state = presets_state.borrow();
                        refresh_preset_dropdown(&preset_dropdown, &state, None);
                    }
                });
            }

            opt_dialog.content_area().append(&grid);
            opt_dialog.add_button("Cancel", gtk::ResponseType::Cancel);
            opt_dialog.add_button("Choose Output File…", gtk::ResponseType::Accept);

            let project = project.clone();
            let bg_removal_cache = bg_removal_cache.clone();
            opt_dialog.connect_response(move |d, resp| {
                if resp != gtk::ResponseType::Accept {
                    d.close();
                    return;
                }

                let options = collect_export_options(
                    &vc_combo,
                    &ct_combo,
                    &or_combo,
                    &crf_slider,
                    &ac_combo,
                    &ab_entry,
                );
                let mut state = presets_state.borrow_mut();
                state.last_used_preset = if preset_dropdown.selected() > 0 {
                    state
                        .presets
                        .get((preset_dropdown.selected() - 1) as usize)
                        .map(|preset| preset.name.clone())
                } else {
                    None
                };
                ui_state::save_export_presets_state(&state);
                let ext = options.container.extension();
                d.close();

                // Now open file-chooser for the output path
                let file_dialog = gtk::FileDialog::new();
                file_dialog.set_title("Export — Choose Output File");
                file_dialog.set_initial_name(Some(&format!("export.{ext}")));

                let window: Option<gtk::Window> = None; // no parent at this point
                let project = project.clone();
                let bg_removal_cache = bg_removal_cache.clone();
                file_dialog.save(window.as_ref(), gio::Cancellable::NONE, move |result| {
                    if let Ok(file) = result {
                        if let Some(path) = file.path() {
                            let output = path.to_string_lossy().to_string();
                            let output_clone = output.clone();
                            let proj = project.borrow().clone();
                            let opts = options.clone();
                            let bg_paths = bg_removal_cache.borrow().paths.clone();
                            let (tx, rx) = std::sync::mpsc::channel::<ExportProgress>();

                            std::thread::spawn(move || {
                                if let Err(e) = export_project(
                                    &proj,
                                    &output_clone,
                                    opts,
                                    None,
                                    &bg_paths,
                                    tx.clone(),
                                ) {
                                    let _ = tx.send(ExportProgress::Error(e.to_string()));
                                }
                            });

                            // Progress dialog
                            let progress_dialog = gtk::Window::builder()
                                .title("Exporting…")
                                .default_width(380)
                                .build();
                            let vbox = gtk::Box::new(gtk::Orientation::Vertical, 12);
                            vbox.set_margin_start(20);
                            vbox.set_margin_end(20);
                            vbox.set_margin_top(20);
                            vbox.set_margin_bottom(20);

                            let status_label = gtk::Label::new(Some("Preparing export…"));
                            status_label.set_halign(gtk::Align::Start);

                            let progress_bar = gtk::ProgressBar::new();
                            progress_bar.set_show_text(true);
                            progress_bar.set_text(Some("0%"));

                            let close_btn = gtk::Button::with_label("Cancel");
                            close_btn.set_halign(gtk::Align::End);

                            vbox.append(&status_label);
                            vbox.append(&progress_bar);
                            vbox.append(&close_btn);
                            progress_dialog.set_child(Some(&vbox));
                            progress_dialog.present();

                            {
                                let pd = progress_dialog.clone();
                                close_btn.connect_clicked(move |_| {
                                    pd.close();
                                });
                            }

                            glib::timeout_add_local(
                                std::time::Duration::from_millis(200),
                                move || {
                                    while let Ok(msg) = rx.try_recv() {
                                        match msg {
                                            ExportProgress::Progress(p) => {
                                                let p = p.clamp(0.0, 0.99);
                                                progress_bar.set_fraction(p);
                                                progress_bar
                                                    .set_text(Some(&format!("{:.0}%", p * 100.0)));
                                                status_label
                                                    .set_text(&format!("Exporting to {output}…"));
                                            }
                                            ExportProgress::Done => {
                                                progress_bar.set_fraction(1.0);
                                                progress_bar.set_text(Some("Done!"));
                                                status_label.set_text("Export complete.");
                                                close_btn.set_label("Close");
                                                return glib::ControlFlow::Break;
                                            }
                                            ExportProgress::Error(e) => {
                                                status_label.set_text(&format!("Error: {e}"));
                                                close_btn.set_label("Close");
                                                eprintln!("Export error: {e}");
                                                return glib::ControlFlow::Break;
                                            }
                                        }
                                    }
                                    glib::ControlFlow::Continue
                                },
                            );
                        }
                    }
                });
            });
            opt_dialog.present();
        });
    }
    let btn_export_more = gtk::Button::with_label("▼");
    btn_export_more.set_tooltip_text(Some("More export options"));
    btn_export_more.add_css_class("suggested-action");
    btn_export_more.add_css_class("export-split-toggle");
    let export_pop = gtk::Popover::new();
    let export_pop_box = gtk::Box::new(gtk::Orientation::Vertical, 2);
    export_pop_box.set_margin_start(4);
    export_pop_box.set_margin_end(4);
    export_pop_box.set_margin_top(4);
    export_pop_box.set_margin_bottom(4);
    let btn_export_project_with_media = gtk::Button::with_label("Export Project with Media…");
    btn_export_project_with_media.add_css_class("flat");
    {
        let project = project.clone();
        let on_project_changed = on_project_changed.clone();
        let export_pop_weak = export_pop.downgrade();
        btn_export_project_with_media.connect_clicked(move |btn| {
            if let Some(pop) = export_pop_weak.upgrade() {
                pop.popdown();
            }

            let dialog = gtk::FileDialog::new();
            dialog.set_title("Export Project with Media");
            dialog.set_initial_name(Some("project.uspxml"));

            let filter = gtk::FileFilter::new();
            filter.add_pattern("*.uspxml");
            filter.add_pattern("*.fcpxml");
            filter.set_name(Some("Project XML Files"));
            let filters = gio::ListStore::new::<gtk::FileFilter>();
            filters.append(&filter);
            dialog.set_filters(Some(&filters));

            let project = project.clone();
            let on_project_changed = on_project_changed.clone();
            let window = btn.root().and_then(|r| r.downcast::<gtk::Window>().ok());
            dialog.save(window.as_ref(), gio::Cancellable::NONE, move |result| {
                if let Ok(file) = result {
                    if let Some(path) = file.path() {
                        let output_path = path.clone();
                        let output_string = output_path.to_string_lossy().to_string();
                        let project_snapshot = project.borrow().clone();
                        let (tx, rx) = std::sync::mpsc::channel::<ExportProjectWithMediaUiEvent>();
                        let path_for_worker = output_path.clone();

                        std::thread::spawn(move || {
                            let result = fcpxml::writer::export_project_with_media_with_progress(
                                &project_snapshot,
                                &path_for_worker,
                                |progress| {
                                    let _ = tx.send(ExportProjectWithMediaUiEvent::Progress(progress));
                                },
                            );
                            match result {
                                Ok(library_dir) => {
                                    let _ = tx.send(ExportProjectWithMediaUiEvent::Done { library_dir });
                                }
                                Err(e) => {
                                    let _ = tx.send(ExportProjectWithMediaUiEvent::Error(e.to_string()));
                                }
                            }
                        });

                        let progress_dialog = gtk::Window::builder()
                            .title("Exporting Project with Media…")
                            .default_width(420)
                            .build();
                        let vbox = gtk::Box::new(gtk::Orientation::Vertical, 12);
                        vbox.set_margin_start(20);
                        vbox.set_margin_end(20);
                        vbox.set_margin_top(20);
                        vbox.set_margin_bottom(20);

                        let status_label = gtk::Label::new(Some("Preparing media package…"));
                        status_label.set_halign(gtk::Align::Start);
                        status_label.set_wrap(true);

                        let progress_bar = gtk::ProgressBar::new();
                        progress_bar.set_show_text(true);
                        progress_bar.set_fraction(0.0);
                        progress_bar.set_text(Some("0%"));

                        let close_btn = gtk::Button::with_label("Close");
                        close_btn.set_halign(gtk::Align::End);

                        vbox.append(&status_label);
                        vbox.append(&progress_bar);
                        vbox.append(&close_btn);
                        progress_dialog.set_child(Some(&vbox));
                        progress_dialog.present();

                        {
                            let pd = progress_dialog.clone();
                            close_btn.connect_clicked(move |_| {
                                pd.close();
                            });
                        }

                        let project = project.clone();
                        let on_project_changed = on_project_changed.clone();
                        glib::timeout_add_local(std::time::Duration::from_millis(120), move || {
                            while let Ok(event) = rx.try_recv() {
                                match event {
                                    ExportProjectWithMediaUiEvent::Progress(
                                        fcpxml::writer::ExportProjectWithMediaProgress::Copying {
                                            copied_files,
                                            total_files,
                                            current_file,
                                        },
                                    ) => {
                                        let fraction = if total_files == 0 {
                                            0.9
                                        } else {
                                            ((copied_files as f64) / (total_files as f64)) * 0.9
                                        };
                                        progress_bar.set_fraction(fraction.clamp(0.0, 0.9));
                                        progress_bar
                                            .set_text(Some(&format!("{:.0}%", fraction * 100.0)));
                                        status_label.set_text(&format!(
                                            "Copying {current_file} ({copied_files}/{total_files})…"
                                        ));
                                    }
                                    ExportProjectWithMediaUiEvent::Progress(
                                        fcpxml::writer::ExportProjectWithMediaProgress::WritingProjectXml,
                                    ) => {
                                        progress_bar.set_fraction(0.97);
                                        progress_bar.set_text(Some("97%"));
                                        status_label.set_text("Writing project XML…");
                                    }
                                    ExportProjectWithMediaUiEvent::Done { library_dir } => {
                                        if let Some(p) = output_path.to_str() {
                                            recent::push(p);
                                        }
                                        {
                                            let mut proj = project.borrow_mut();
                                            proj.file_path = Some(output_path.to_string_lossy().to_string());
                                            proj.dirty = false;
                                        }
                                        on_project_changed();
                                        progress_bar.set_fraction(1.0);
                                        progress_bar.set_text(Some("Done!"));
                                        status_label.set_text(&format!(
                                            "Exported package to {} with media in {}",
                                            output_string,
                                            library_dir.display()
                                        ));
                                        return glib::ControlFlow::Break;
                                    }
                                    ExportProjectWithMediaUiEvent::Error(e) => {
                                        status_label.set_text(&format!("Error: {e}"));
                                        eprintln!("Export-with-media error: {e}");
                                        return glib::ControlFlow::Break;
                                    }
                                }
                            }
                            glib::ControlFlow::Continue
                        });
                    }
                }
            });
        });
    }
    let btn_export_frame = gtk::Button::with_label("Export Frame…");
    btn_export_frame.add_css_class("flat");
    {
        let on_export_frame = on_export_frame.clone();
        let export_pop_weak = export_pop.downgrade();
        btn_export_frame.connect_clicked(move |_| {
            if let Some(pop) = export_pop_weak.upgrade() {
                pop.popdown();
            }
            on_export_frame();
        });
    }
    let btn_restore_backup = gtk::Button::with_label("Restore from Backup…");
    btn_restore_backup.add_css_class("flat");
    {
        let project = project.clone();
        let timeline_state = timeline_state.clone();
        let on_project_changed = on_project_changed.clone();
        let on_project_reloaded = on_project_reloaded.clone();
        let export_pop_weak = export_pop.downgrade();
        btn_restore_backup.connect_clicked(move |btn| {
            if let Some(pop) = export_pop_weak.upgrade() {
                pop.popdown();
            }
            let window = btn.root().and_then(|r| r.downcast::<gtk::Window>().ok());
            let dialog = gtk::FileDialog::new();
            dialog.set_title("Restore from Backup");
            if let Some(dir) = crate::ui::window::backup_dir() {
                let _ = std::fs::create_dir_all(&dir);
                dialog.set_initial_folder(Some(&gio::File::for_path(&dir)));
            }
            let filter = gtk::FileFilter::new();
            filter.add_pattern("*.uspxml");
            filter.add_pattern("*.fcpxml");
            filter.set_name(Some("Project Backups"));
            let filters = gio::ListStore::new::<gtk::FileFilter>();
            filters.append(&filter);
            dialog.set_filters(Some(&filters));
            let project = project.clone();
            let timeline_state = timeline_state.clone();
            let on_project_changed = on_project_changed.clone();
            let on_project_reloaded = on_project_reloaded.clone();
            dialog.open(window.as_ref(), gio::Cancellable::NONE, move |result| {
                if let Ok(file) = result {
                    if let Some(path) = file.path() {
                        let path_str = path.to_string_lossy().to_string();
                        let (tx, rx) =
                            std::sync::mpsc::sync_channel::<Result<Project, String>>(1);
                        let path_bg = path_str.clone();
                        std::thread::spawn(move || {
                            let result = std::fs::read_to_string(&path_bg)
                                .map_err(|e| format!("Failed to read backup: {e}"))
                                .and_then(|xml| {
                                    fcpxml::parser::parse_fcpxml_with_path(
                                        &xml,
                                        Some(std::path::Path::new(&path_bg)),
                                    )
                                    .map_err(|e| format!("Backup parse error: {e}"))
                                });
                            let _ = tx.send(result);
                        });
                        let project = project.clone();
                        let on_project_changed = on_project_changed.clone();
                        let on_project_reloaded = on_project_reloaded.clone();
                        let timeline_state = timeline_state.clone();
                        timeline_state.borrow_mut().loading = true;
                        glib::timeout_add_local(
                            std::time::Duration::from_millis(50),
                            move || match rx.try_recv() {
                                Ok(Ok(mut new_proj)) => {
                                    new_proj.dirty = false;
                                    *project.borrow_mut() = new_proj;
                                    timeline_state.borrow_mut().loading = false;
                                    on_project_reloaded();
                                    on_project_changed();
                                    glib::ControlFlow::Break
                                }
                                Ok(Err(e)) => {
                                    log::error!("Failed to restore backup: {e}");
                                    timeline_state.borrow_mut().loading = false;
                                    glib::ControlFlow::Break
                                }
                                Err(std::sync::mpsc::TryRecvError::Empty) => {
                                    glib::ControlFlow::Continue
                                }
                                Err(_) => {
                                    timeline_state.borrow_mut().loading = false;
                                    glib::ControlFlow::Break
                                }
                            },
                        );
                    }
                }
            });
        });
    }
    export_pop_box.append(&btn_export_project_with_media);
    export_pop_box.append(&btn_export_frame);
    export_pop_box.append(&btn_restore_backup);
    export_pop.set_child(Some(&export_pop_box));
    export_pop.set_parent(&btn_export_more);
    {
        let export_pop = export_pop.clone();
        btn_export_more.connect_clicked(move |_| {
            if export_pop.is_visible() {
                export_pop.popdown();
            } else {
                export_pop.popup();
            }
        });
    }

    let export_group = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    export_group.add_css_class("linked");
    export_group.add_css_class("export-split");
    export_group.append(&btn_export);
    export_group.append(&btn_export_more);
    header.pack_end(&export_group);

    let sep_history = Separator::new(gtk::Orientation::Vertical);
    sep_history.add_css_class("toolbar-separator");
    header.pack_start(&sep_history);

    // ── Undo / Redo ─────────────────────────────────────────────────────
    let btn_undo = Button::with_label("↩ Undo");
    btn_undo.set_tooltip_text(Some("Undo (Ctrl+Z)"));
    {
        let timeline_state = timeline_state.clone();
        let on_project_changed = on_project_changed.clone();
        btn_undo.connect_clicked(move |_| {
            timeline_state.borrow_mut().undo();
            on_project_changed();
        });
    }
    header.pack_start(&btn_undo);

    let btn_redo = Button::with_label("↪ Redo");
    btn_redo.set_tooltip_text(Some("Redo (Ctrl+Shift+Z)"));
    {
        let timeline_state = timeline_state.clone();
        let on_project_changed = on_project_changed.clone();
        btn_redo.connect_clicked(move |_| {
            timeline_state.borrow_mut().redo();
            on_project_changed();
        });
    }
    header.pack_start(&btn_redo);

    let sep_tools = Separator::new(gtk::Orientation::Vertical);
    sep_tools.add_css_class("toolbar-separator");
    header.pack_start(&sep_tools);

    // ── Tool selector: Select / Razor ───────────────────────────────────
    let btn_select = ToggleButton::with_label("↖ Select");
    btn_select.set_tooltip_text(Some("Selection tool (Escape)"));
    btn_select.set_active(true);

    let btn_razor = ToggleButton::with_label("✂ Razor");
    btn_razor.set_tooltip_text(Some("Razor/blade tool (B)"));
    btn_razor.set_group(Some(&btn_select));

    let btn_ripple = ToggleButton::with_label("⇤ Ripple");
    btn_ripple.set_tooltip_text(Some("Ripple edit tool (R)"));
    btn_ripple.set_group(Some(&btn_select));

    let btn_roll = ToggleButton::with_label("⇋ Roll");
    btn_roll.set_tooltip_text(Some("Roll edit tool (E)"));
    btn_roll.set_group(Some(&btn_select));

    let btn_slip = ToggleButton::with_label("↔ Slip");
    btn_slip.set_tooltip_text(Some("Slip edit tool (Y)"));
    btn_slip.set_group(Some(&btn_select));

    let btn_slide = ToggleButton::with_label("⇔ Slide");
    btn_slide.set_tooltip_text(Some("Slide edit tool (U)"));
    btn_slide.set_group(Some(&btn_select));

    {
        let timeline_state = timeline_state.clone();
        btn_select.connect_toggled(move |btn| {
            if btn.is_active() {
                timeline_state.borrow_mut().active_tool = ActiveTool::Select;
            }
        });
    }
    {
        let timeline_state = timeline_state.clone();
        btn_razor.connect_toggled(move |btn| {
            if btn.is_active() {
                timeline_state.borrow_mut().active_tool = ActiveTool::Razor;
            }
        });
    }
    {
        let timeline_state = timeline_state.clone();
        btn_ripple.connect_toggled(move |btn| {
            if btn.is_active() {
                timeline_state.borrow_mut().active_tool = ActiveTool::Ripple;
            }
        });
    }
    {
        let timeline_state = timeline_state.clone();
        btn_roll.connect_toggled(move |btn| {
            if btn.is_active() {
                timeline_state.borrow_mut().active_tool = ActiveTool::Roll;
            }
        });
    }
    {
        let timeline_state = timeline_state.clone();
        btn_slip.connect_toggled(move |btn| {
            if btn.is_active() {
                timeline_state.borrow_mut().active_tool = ActiveTool::Slip;
            }
        });
    }
    {
        let timeline_state = timeline_state.clone();
        btn_slide.connect_toggled(move |btn| {
            if btn.is_active() {
                timeline_state.borrow_mut().active_tool = ActiveTool::Slide;
            }
        });
    }
    let btn_magnetic = ToggleButton::with_label("Magnetic");
    btn_magnetic.set_tooltip_text(Some("Gap-free timeline mode (edited track)"));
    btn_magnetic.set_active(timeline_state.borrow().magnetic_mode);
    {
        let timeline_state = timeline_state.clone();
        let on_project_changed = on_project_changed.clone();
        btn_magnetic.connect_toggled(move |btn| {
            timeline_state.borrow_mut().magnetic_mode = btn.is_active();
            on_project_changed();
        });
    }

    header.pack_start(&btn_select);
    header.pack_start(&btn_razor);
    header.pack_start(&btn_ripple);
    header.pack_start(&btn_roll);
    header.pack_start(&btn_slip);
    header.pack_start(&btn_slide);
    header.pack_start(&btn_magnetic);

    // Wire on_tool_changed so keyboard shortcuts sync toolbar buttons
    {
        let btn_select = btn_select.clone();
        let btn_razor = btn_razor.clone();
        let btn_ripple = btn_ripple.clone();
        let btn_roll = btn_roll.clone();
        let btn_slip = btn_slip.clone();
        let btn_slide = btn_slide.clone();
        timeline_state.borrow_mut().on_tool_changed =
            Some(Rc::new(move |tool: ActiveTool| match tool {
                ActiveTool::Select => btn_select.set_active(true),
                ActiveTool::Razor => btn_razor.set_active(true),
                ActiveTool::Ripple => btn_ripple.set_active(true),
                ActiveTool::Roll => btn_roll.set_active(true),
                ActiveTool::Slip => btn_slip.set_active(true),
                ActiveTool::Slide => btn_slide.set_active(true),
            }));
    }

    header
}
