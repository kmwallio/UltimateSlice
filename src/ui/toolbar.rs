use gtk4::prelude::*;
use gtk4::{self as gtk, Button, HeaderBar, Label, ToggleButton};
use gio;
use glib;
use std::cell::RefCell;
use std::rc::Rc;
use crate::model::project::Project;
use crate::fcpxml;
use crate::media::export::{export_project, ExportProgress};
use crate::ui::timeline::{TimelineState, ActiveTool};

/// Build the main `HeaderBar` toolbar.
pub fn build_toolbar(
    project: Rc<RefCell<Project>>,
    timeline_state: Rc<RefCell<TimelineState>>,
    on_project_changed: impl Fn() + 'static + Clone,
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
        btn_new.connect_clicked(move |_| {
            *project.borrow_mut() = Project::new("Untitled");
            {
                let mut st = timeline_state.borrow_mut();
                st.playhead_ns = 0;
                st.scroll_offset = 0.0;
                st.pixels_per_second = 100.0;
                st.selected_clip_id = None;
                st.selected_track_id = None;
            }
            on_project_changed();
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
        btn_open.connect_clicked(move |btn| {
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
            let timeline_state_cb = timeline_state.clone();
            let window = btn.root().and_then(|r| r.downcast::<gtk::Window>().ok());

            dialog.open(window.as_ref(), gio::Cancellable::NONE, move |result| {
                if let Ok(file) = result {
                    if let Some(path) = file.path() {
                        match std::fs::read_to_string(&path) {
                            Ok(xml) => match fcpxml::parser::parse_fcpxml(&xml) {
                                Ok(mut new_proj) => {
                                    new_proj.file_path = path.to_str().map(|s| s.to_string());
                                    *project.borrow_mut() = new_proj;
                                    {
                                        let mut st = timeline_state_cb.borrow_mut();
                                        st.playhead_ns = 0;
                                        st.scroll_offset = 0.0;
                                        st.pixels_per_second = 100.0;
                                        st.selected_clip_id = None;
                                        st.selected_track_id = None;
                                    }
                                    on_project_changed();
                                }
                                Err(e) => eprintln!("FCPXML parse error: {e}"),
                            },
                            Err(e) => eprintln!("Failed to read file: {e}"),
                        }
                    }
                }
            });
        });
    }
    header.pack_start(&btn_open);

    // Save FCPXML
    let btn_save = Button::with_label("Save…");
    btn_save.set_tooltip_text(Some("Save as FCPXML (Ctrl+S)"));
    {
        let project = project.clone();
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
            let window = btn.root().and_then(|r| r.downcast::<gtk::Window>().ok());

            dialog.save(window.as_ref(), gio::Cancellable::NONE, move |result| {
                if let Ok(file) = result {
                    if let Some(path) = file.path() {
                        let proj = project.borrow();
                        match fcpxml::writer::write_fcpxml(&proj) {
                            Ok(xml) => {
                                if let Err(e) = std::fs::write(&path, xml) {
                                    eprintln!("Save error: {e}");
                                } else {
                                    println!("Saved to {}", path.display());
                                }
                            }
                            Err(e) => eprintln!("FCPXML write error: {e}"),
                        }
                    }
                }
            });
        });
    }
    header.pack_start(&btn_save);

    // Export MP4
    let btn_export = Button::with_label("Export MP4…");
    btn_export.set_tooltip_text(Some("Export to MP4/H.264"));
    btn_export.add_css_class("suggested-action");
    {
        let project = project.clone();
        btn_export.connect_clicked(move |btn| {
            let dialog = gtk::FileDialog::new();
            dialog.set_title("Export MP4");
            dialog.set_initial_name(Some("export.mp4"));

            let project = project.clone();
            let window = btn.root().and_then(|r| r.downcast::<gtk::Window>().ok());

            dialog.save(window.as_ref(), gio::Cancellable::NONE, move |result| {
                if let Ok(file) = result {
                    if let Some(path) = file.path() {
                        let output = path.to_string_lossy().to_string();
                        let output_clone = output.clone();
                        let proj = project.borrow().clone();

                        let (tx, rx) = std::sync::mpsc::channel::<ExportProgress>();

                        // Run export on a background thread
                        std::thread::spawn(move || {
                            if let Err(e) = export_project(&proj, &output_clone, tx.clone()) {
                                let _ = tx.send(ExportProgress::Error(e.to_string()));
                            }
                        });

                        // Build a progress dialog
                        let progress_dialog = gtk::Window::builder()
                            .title("Exporting…")
                            .default_width(360)
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

                        let cancel_btn = gtk::Button::with_label("Cancel");
                        cancel_btn.set_halign(gtk::Align::End);

                        vbox.append(&status_label);
                        vbox.append(&progress_bar);
                        vbox.append(&cancel_btn);
                        progress_dialog.set_child(Some(&vbox));
                        progress_dialog.present();

                        // Cancel is not yet wired to stop the background thread —
                        // just close the dialog for now
                        {
                            let pd = progress_dialog.clone();
                            cancel_btn.connect_clicked(move |_| { pd.close(); });
                        }

                        // Poll progress on the GTK main loop
                        glib::timeout_add_local(
                            std::time::Duration::from_millis(200),
                            move || {
                                while let Ok(msg) = rx.try_recv() {
                                    match msg {
                                        ExportProgress::Progress(p) => {
                                            progress_bar.set_fraction(p as f64);
                                            progress_bar.set_text(Some(&format!("{:.0}%", p * 100.0)));
                                            status_label.set_text(&format!("Exporting to {output}…"));
                                        }
                                        ExportProgress::Done => {
                                            progress_bar.set_fraction(1.0);
                                            progress_bar.set_text(Some("Done!"));
                                            status_label.set_text("Export complete.");
                                            cancel_btn.set_label("Close");
                                            return glib::ControlFlow::Break;
                                        }
                                        ExportProgress::Error(e) => {
                                            status_label.set_text(&format!("Error: {e}"));
                                            cancel_btn.set_label("Close");
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
    }
    header.pack_end(&btn_export);

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

    // ── Tool selector: Select / Razor ───────────────────────────────────
    let btn_select = ToggleButton::with_label("↖ Select");
    btn_select.set_tooltip_text(Some("Selection tool (Escape)"));
    btn_select.set_active(true);

    let btn_razor = ToggleButton::with_label("✂ Razor");
    btn_razor.set_tooltip_text(Some("Razor/blade tool (B)"));
    btn_razor.set_group(Some(&btn_select));

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

    header.pack_start(&btn_select);
    header.pack_start(&btn_razor);

    header
}
