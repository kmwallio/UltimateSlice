use crate::fcpxml;
use crate::media::export::{
    export_project, AudioCodec, Container, ExportOptions, ExportProgress, VideoCodec,
};
use crate::model::project::{FrameRate, Project};
use crate::recent;
use crate::ui::timeline::{ActiveTool, TimelineState};
use gio;
use glib;
use gtk4::prelude::*;
use gtk4::{self as gtk, Button, HeaderBar, Label, Separator, ToggleButton};
use std::cell::RefCell;
use std::rc::Rc;

fn save_project_to_path(project: &Rc<RefCell<Project>>, path: &std::path::Path) -> Result<(), String> {
    let xml = {
        let proj = project.borrow();
        fcpxml::writer::write_fcpxml(&proj).map_err(|e| format!("FCPXML write error: {e}"))?
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
    dialog.connect_response(move |d, resp| {
        match resp {
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
                    file_dialog.set_title("Save FCPXML Project");
                    file_dialog.set_initial_name(Some("project.fcpxml"));
                    let filter = gtk::FileFilter::new();
                    filter.add_pattern("*.fcpxml");
                    filter.set_name(Some("FCPXML Files"));
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
        }
    });
    dialog.present();
}

/// Build the main `HeaderBar` toolbar.
pub fn build_toolbar(
    project: Rc<RefCell<Project>>,
    timeline_state: Rc<RefCell<TimelineState>>,
    on_project_changed: impl Fn() + 'static + Clone,
    on_project_reloaded: impl Fn() + 'static + Clone,
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

    // Open / Import FCPXML
    let btn_open = Button::with_label("Open…");
    btn_open.set_tooltip_text(Some("Open FCPXML project (Ctrl+O)"));
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
                    dialog.set_title("Open FCPXML Project");

                    let filter = gtk::FileFilter::new();
                    filter.add_pattern("*.fcpxml");
                    filter.add_pattern("*.xml");
                    filter.set_name(Some("FCPXML Files"));
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
                        let on_project_changed_cb: Rc<dyn Fn()> = Rc::new(on_project_changed.clone());
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
                        confirm_unsaved_then(window, project.clone(), on_project_changed_cb, action);
                    });
                    vbox_ref.append(&row);
                }
            }
        });
    }
    header.pack_start(&btn_recent);
    let btn_save = Button::with_label("Save…");
    btn_save.set_tooltip_text(Some("Save as FCPXML (Ctrl+S)"));
    {
        let project = project.clone();
        let on_project_changed = on_project_changed.clone();
        btn_save.connect_clicked(move |btn| {
            let dialog = gtk::FileDialog::new();
            dialog.set_title("Save FCPXML Project");
            dialog.set_initial_name(Some("project.fcpxml"));

            let filter = gtk::FileFilter::new();
            filter.add_pattern("*.fcpxml");
            filter.set_name(Some("FCPXML Files"));
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
                .default_width(360)
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

            // Resolution preset
            let res_label = gtk::Label::new(Some("Resolution:"));
            res_label.set_halign(gtk::Align::End);
            let res_combo = gtk::DropDown::from_strings(&[
                "1920 × 1080  (1080p HD)",
                "3840 × 2160  (4K UHD)",
                "1280 × 720   (720p HD)",
                "720 × 480    (SD NTSC)",
                "1080 × 1920  (9:16 Vertical)",
                "1080 × 1080  (1:1 Square)",
            ]);
            let res_idx = match (proj.width, proj.height) {
                (1920, 1080) => 0,
                (3840, 2160) => 1,
                (1280, 720) => 2,
                (720, 480) => 3,
                (1080, 1920) => 4,
                (1080, 1080) => 5,
                _ => 0,
            };
            res_combo.set_selected(res_idx);
            grid.attach(&res_label, 0, 0, 1, 1);
            grid.attach(&res_combo, 1, 0, 1, 1);

            // Frame rate preset
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
            grid.attach(&fps_label, 0, 1, 1, 1);
            grid.attach(&fps_combo, 1, 1, 1, 1);

            dialog.content_area().append(&grid);
            dialog.add_button("Cancel", gtk::ResponseType::Cancel);
            dialog.add_button("Apply", gtk::ResponseType::Accept);

            drop(proj);
            let project = project.clone();
            let on_project_changed = on_project_changed.clone();
            dialog.connect_response(move |d, resp| {
                if resp == gtk::ResponseType::Accept {
                    let (w, h) = match res_combo.selected() {
                        0 => (1920, 1080),
                        1 => (3840, 2160),
                        2 => (1280, 720),
                        3 => (720, 480),
                        4 => (1080, 1920),
                        5 => (1080, 1080),
                        _ => (1920, 1080),
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
    let btn_export = Button::with_label("Export…");
    btn_export.set_tooltip_text(Some("Export with codec and resolution options"));
    btn_export.add_css_class("suggested-action");
    {
        let project = project.clone();
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
            grid.attach(&vc_label, 0, 0, 1, 1);
            grid.attach(&vc_combo, 1, 0, 1, 1);

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
            grid.attach(&ct_label, 0, 1, 1, 1);
            grid.attach(&ct_combo, 1, 1, 1, 1);

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
            grid.attach(&or_label, 0, 2, 1, 1);
            grid.attach(&or_combo, 1, 2, 1, 1);

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
            grid.attach(&crf_label, 0, 3, 1, 1);
            grid.attach(&crf_box, 1, 3, 1, 1);

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
            grid.attach(&ac_label, 0, 4, 1, 1);
            grid.attach(&ac_combo, 1, 4, 1, 1);

            // Audio bitrate
            let ab_label = gtk::Label::new(Some("Audio Bitrate:"));
            ab_label.set_halign(gtk::Align::End);
            let ab_entry = gtk::Entry::new();
            ab_entry.set_text("192");
            ab_entry.set_tooltip_text(Some("Audio bitrate in kbps (ignored for FLAC/PCM)"));
            grid.attach(&ab_label, 0, 5, 1, 1);
            grid.attach(&ab_entry, 1, 5, 1, 1);

            opt_dialog.content_area().append(&grid);
            opt_dialog.add_button("Cancel", gtk::ResponseType::Cancel);
            opt_dialog.add_button("Choose Output File…", gtk::ResponseType::Accept);

            let project = project.clone();
            opt_dialog.connect_response(move |d, resp| {
                if resp != gtk::ResponseType::Accept {
                    d.close();
                    return;
                }

                let video_codec = match vc_combo.selected() {
                    0 => VideoCodec::H264,
                    1 => VideoCodec::H265,
                    2 => VideoCodec::Vp9,
                    3 => VideoCodec::ProRes,
                    4 => VideoCodec::Av1,
                    _ => VideoCodec::H264,
                };
                let container = match ct_combo.selected() {
                    0 => Container::Mp4,
                    1 => Container::Mov,
                    2 => Container::WebM,
                    3 => Container::Mkv,
                    _ => Container::Mp4,
                };
                let (out_w, out_h) = match or_combo.selected() {
                    0 => (0u32, 0u32),
                    1 => (3840, 2160),
                    2 => (1920, 1080),
                    3 => (1280, 720),
                    4 => (854, 480),
                    _ => (0, 0),
                };
                let crf = crf_slider.value() as u32;
                let audio_codec = match ac_combo.selected() {
                    0 => AudioCodec::Aac,
                    1 => AudioCodec::Opus,
                    2 => AudioCodec::Flac,
                    3 => AudioCodec::Pcm,
                    _ => AudioCodec::Aac,
                };
                let audio_bitrate_kbps = ab_entry.text().parse::<u32>().unwrap_or(192);
                let ext = container.extension();

                let options = ExportOptions {
                    video_codec,
                    container,
                    output_width: out_w,
                    output_height: out_h,
                    crf,
                    audio_codec,
                    audio_bitrate_kbps,
                };
                d.close();

                // Now open file-chooser for the output path
                let file_dialog = gtk::FileDialog::new();
                file_dialog.set_title("Export — Choose Output File");
                file_dialog.set_initial_name(Some(&format!("export.{ext}")));

                let window: Option<gtk::Window> = None; // no parent at this point
                let project = project.clone();
                file_dialog.save(window.as_ref(), gio::Cancellable::NONE, move |result| {
                    if let Ok(file) = result {
                        if let Some(path) = file.path() {
                            let output = path.to_string_lossy().to_string();
                            let output_clone = output.clone();
                            let proj = project.borrow().clone();
                            let opts = options.clone();

                            let (tx, rx) = std::sync::mpsc::channel::<ExportProgress>();

                            std::thread::spawn(move || {
                                if let Err(e) =
                                    export_project(&proj, &output_clone, opts, tx.clone())
                                {
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
    header.pack_end(&btn_export);

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

    header
}
