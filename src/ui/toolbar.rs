use crate::fcpxml;
use crate::media::export::{
    export_project, AudioChannelLayout, AudioCodec, Container, ExportOptions, ExportProgress,
    VideoCodec,
};
use crate::model::media_library::MediaLibrary;
use crate::model::project::{FrameRate, Project};
use crate::recent;
use crate::ui::timeline::{ActiveTool, TimelineState};
use crate::ui_state::{self, ExportPreset, ExportPresetsState, ExportQueueJob};
use gio;
use glib;
use gtk4::prelude::*;
use gtk4::{self as gtk, Button, HeaderBar, Label, Separator, ToggleButton};
use std::cell::{Cell, RefCell};
use std::rc::Rc;

// -----------------------------------------------------------------------
// Project-settings dropdown tables
// -----------------------------------------------------------------------
//
// Both the project-settings dialog (frame rate + canvas resolution) and
// the export dialog historically duplicated the same constant lists in
// three or four places — once for the dropdown labels, once for the
// init-from-current-state lookup, once inside the change handler, and
// once inside the dialog response handler. The structs below collapse
// each of those to a single source of truth so adding a new framerate
// or resolution preset is a one-line edit.

/// One option in the Project Settings frame-rate dropdown.
struct FramerateOption {
    label: &'static str,
    num: u32,
    den: u32,
}

/// Project-settings frame-rate options. The display label is what appears
/// in the dropdown; `(num, den)` is what gets written into `Project::frame_rate`.
const FRAMERATE_OPTIONS: &[FramerateOption] = &[
    FramerateOption {
        label: "23.976 fps",
        num: 24000,
        den: 1001,
    },
    FramerateOption {
        label: "24 fps",
        num: 24,
        den: 1,
    },
    FramerateOption {
        label: "25 fps",
        num: 25,
        den: 1,
    },
    FramerateOption {
        label: "29.97 fps",
        num: 30000,
        den: 1001,
    },
    FramerateOption {
        label: "30 fps",
        num: 30,
        den: 1,
    },
    FramerateOption {
        label: "60 fps",
        num: 60,
        den: 1,
    },
];

/// Default frame-rate index used when the current project's frame rate
/// doesn't match any preset (24 fps).
const DEFAULT_FRAMERATE_INDEX: usize = 1;

fn is_primary_toolbar_tool(tool: ActiveTool) -> bool {
    matches!(
        tool,
        ActiveTool::Select
            | ActiveTool::Razor
            | ActiveTool::Ripple
            | ActiveTool::Roll
            | ActiveTool::Slip
            | ActiveTool::Slide
    )
}

fn toolbar_tool_label(tool: ActiveTool) -> &'static str {
    match tool {
        ActiveTool::Select => "↖ Select",
        ActiveTool::Razor => "✂ Razor",
        ActiveTool::Ripple => "⇤ Ripple",
        ActiveTool::Roll => "⇋ Roll",
        ActiveTool::Slip => "↔ Slip",
        ActiveTool::Slide => "⇔ Slide",
        ActiveTool::Draw => "✎ Draw",
    }
}

fn toolbar_tool_tooltip(tool: ActiveTool) -> &'static str {
    match tool {
        ActiveTool::Select => "Selection tool (Escape)",
        ActiveTool::Razor => "Razor/blade tool (B)",
        ActiveTool::Ripple => "Ripple edit tool (R)",
        ActiveTool::Roll => "Roll edit tool (E)",
        ActiveTool::Slip => "Slip edit tool (Y)",
        ActiveTool::Slide => "Slide edit tool (U)",
        ActiveTool::Draw => "Vector drawing tool (D)",
    }
}

/// One canvas-resolution preset within an aspect-ratio group.
struct ResolutionPreset {
    width: u32,
    height: u32,
    label: &'static str,
}

/// One aspect-ratio group in the Project Settings dialog.
struct AspectRatioGroup {
    label: &'static str,
    presets: &'static [ResolutionPreset],
}

/// Project-settings aspect-ratio groups, each containing the discrete
/// resolution presets that share that aspect.
const ASPECT_RATIO_PRESETS: &[AspectRatioGroup] = &[
    AspectRatioGroup {
        label: "16:9 (Widescreen)",
        presets: &[
            ResolutionPreset {
                width: 3840,
                height: 2160,
                label: "3840 × 2160  (4K UHD)",
            },
            ResolutionPreset {
                width: 2560,
                height: 1440,
                label: "2560 × 1440  (1440p QHD)",
            },
            ResolutionPreset {
                width: 1920,
                height: 1080,
                label: "1920 × 1080  (1080p HD)",
            },
            ResolutionPreset {
                width: 1280,
                height: 720,
                label: "1280 × 720   (720p HD)",
            },
        ],
    },
    AspectRatioGroup {
        label: "4:3 (Standard)",
        presets: &[
            ResolutionPreset {
                width: 1440,
                height: 1080,
                label: "1440 × 1080  (HD 4:3)",
            },
            ResolutionPreset {
                width: 1024,
                height: 768,
                label: "1024 × 768   (XGA)",
            },
            ResolutionPreset {
                width: 720,
                height: 480,
                label: "720 × 480    (SD NTSC)",
            },
        ],
    },
    AspectRatioGroup {
        label: "9:16 (Vertical)",
        presets: &[
            ResolutionPreset {
                width: 1080,
                height: 1920,
                label: "1080 × 1920  (Full HD Vertical)",
            },
            ResolutionPreset {
                width: 720,
                height: 1280,
                label: "720 × 1280   (HD Vertical)",
            },
        ],
    },
    AspectRatioGroup {
        label: "1:1 (Square)",
        presets: &[
            ResolutionPreset {
                width: 2160,
                height: 2160,
                label: "2160 × 2160  (4K Square)",
            },
            ResolutionPreset {
                width: 1080,
                height: 1080,
                label: "1080 × 1080  (HD Square)",
            },
        ],
    },
];

/// Label for the "Custom" entry appended after `ASPECT_RATIO_PRESETS` in the
/// dropdown. Selecting this hides the preset dropdown and reveals W×H spinners.
const CUSTOM_ASPECT_LABEL: &str = "Custom";

fn save_project_to_path(
    project: &Rc<RefCell<Project>>,
    library: &Rc<RefCell<MediaLibrary>>,
    path: &std::path::Path,
) -> Result<(), String> {
    // Sync bin data from library into project before writing.
    crate::model::media_library::sync_bins_to_project(&library.borrow(), &mut project.borrow_mut());
    let xml = {
        let proj = project.borrow();
        fcpxml::writer::write_fcpxml_for_path(&proj, path)
            .map_err(|e| format!("FCPXML write error: {e}"))?
    };
    std::fs::write(path, &xml).map_err(|e| format!("Save error: {e}"))?;
    if let Some(p) = path.to_str() {
        recent::push(p);
    }
    {
        let mut proj = project.borrow_mut();
        proj.file_path = Some(path.to_string_lossy().to_string());
        // Keep source_fcpxml in sync with what was written to disk so that
        // subsequent clean-save passthroughs return the correct content
        // (including up-to-date us:library-items, us:bins, etc.).
        if !fcpxml::writer::use_strict_fcpxml_for_path(path) {
            proj.source_fcpxml = Some(xml);
        }
        proj.dirty = false;
    }
    // Remove autosave now that the project is safely saved to disk.
    crate::project_versions::delete_autosave_for_project(&project.borrow());
    Ok(())
}

fn create_named_snapshot(
    project: &Rc<RefCell<Project>>,
    library: &Rc<RefCell<MediaLibrary>>,
    snapshot_name: &str,
) -> Result<crate::project_versions::ProjectSnapshotEntry, String> {
    crate::model::media_library::sync_bins_to_project(&library.borrow(), &mut project.borrow_mut());
    let proj = project.borrow();
    let xml = crate::project_versions::write_snapshot_project_xml(&proj)?;
    crate::project_versions::create_project_snapshot(&proj, &xml, snapshot_name)
}

fn apply_restored_project_state(
    project: &Rc<RefCell<Project>>,
    timeline_state: &Rc<RefCell<TimelineState>>,
    on_project_changed: &Rc<dyn Fn()>,
    on_project_reloaded: &Rc<dyn Fn()>,
    mut new_proj: Project,
    preserved_file_path: Option<String>,
) {
    new_proj.file_path = preserved_file_path;
    new_proj.dirty = true;
    *project.borrow_mut() = new_proj;
    timeline_state.borrow_mut().loading = false;
    on_project_reloaded();
    on_project_changed();
}

fn restore_project_version_async<F>(
    project: Rc<RefCell<Project>>,
    timeline_state: Rc<RefCell<TimelineState>>,
    on_project_changed: Rc<dyn Fn()>,
    on_project_reloaded: Rc<dyn Fn()>,
    preserved_file_path: Option<String>,
    loader: F,
) where
    F: FnOnce() -> Result<Project, String> + Send + 'static,
{
    let (tx, rx) = std::sync::mpsc::sync_channel::<Result<Project, String>>(1);
    std::thread::spawn(move || {
        let _ = tx.send(loader());
    });
    timeline_state.borrow_mut().loading = true;
    glib::timeout_add_local(std::time::Duration::from_millis(50), move || {
        match rx.try_recv() {
            Ok(Ok(new_proj)) => {
                apply_restored_project_state(
                    &project,
                    &timeline_state,
                    &on_project_changed,
                    &on_project_reloaded,
                    new_proj,
                    preserved_file_path.clone(),
                );
                glib::ControlFlow::Break
            }
            Ok(Err(e)) => {
                log::error!("{e}");
                timeline_state.borrow_mut().loading = false;
                glib::ControlFlow::Break
            }
            Err(std::sync::mpsc::TryRecvError::Empty) => glib::ControlFlow::Continue,
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                timeline_state.borrow_mut().loading = false;
                glib::ControlFlow::Break
            }
        }
    });
}

#[allow(deprecated)]
fn choose_snapshot_name(
    window: Option<gtk::Window>,
    on_selected: Rc<dyn Fn(String) -> Result<(), String>>,
) {
    let dialog = gtk::Dialog::builder()
        .title("Create Snapshot")
        .modal(true)
        .default_width(420)
        .build();
    dialog.set_transient_for(window.as_ref());
    dialog.add_button("Cancel", gtk::ResponseType::Cancel);
    dialog.add_button("Create Snapshot", gtk::ResponseType::Accept);
    dialog.set_default_response(gtk::ResponseType::Accept);

    let content = gtk::Box::new(gtk::Orientation::Vertical, 12);
    content.set_margin_start(16);
    content.set_margin_end(16);
    content.set_margin_top(16);
    content.set_margin_bottom(16);

    let description = gtk::Label::new(Some(
        "Save the current project as a named milestone without changing its main save path.",
    ));
    description.set_wrap(true);
    description.set_xalign(0.0);

    let name_entry = gtk::Entry::new();
    name_entry.set_placeholder_text(Some("Before color pass"));
    name_entry.set_activates_default(true);

    let error_label = gtk::Label::new(None);
    error_label.set_wrap(true);
    error_label.set_xalign(0.0);

    content.append(&description);
    content.append(&name_entry);
    content.append(&error_label);
    dialog.content_area().append(&content);

    {
        let error_label = error_label.clone();
        name_entry.connect_changed(move |_| {
            error_label.set_text("");
        });
    }

    dialog.connect_response(move |d, resp| match resp {
        gtk::ResponseType::Accept => {
            let snapshot_name = name_entry.text().trim().to_string();
            if snapshot_name.is_empty() {
                error_label.set_text("Snapshot name cannot be empty.");
                return;
            }
            match on_selected(snapshot_name) {
                Ok(()) => d.close(),
                Err(e) => error_label.set_text(&e),
            }
        }
        _ => d.close(),
    });
    dialog.present();
}

#[allow(deprecated)]
fn confirm_delete_snapshot(
    window: Option<gtk::Window>,
    snapshot_name: &str,
    on_confirm: Rc<dyn Fn()>,
) {
    let dialog = gtk::Dialog::builder()
        .title("Delete Snapshot")
        .modal(true)
        .default_width(420)
        .build();
    dialog.set_transient_for(window.as_ref());
    dialog.add_button("Cancel", gtk::ResponseType::Cancel);
    dialog.add_button("Delete", gtk::ResponseType::Accept);

    let label = gtk::Label::new(Some(&format!(
        "Delete snapshot \"{snapshot_name}\"? This cannot be undone."
    )));
    label.set_wrap(true);
    label.set_margin_start(16);
    label.set_margin_end(16);
    label.set_margin_top(16);
    label.set_margin_bottom(16);
    label.set_xalign(0.0);
    dialog.content_area().append(&label);

    dialog.connect_response(move |d, resp| match resp {
        gtk::ResponseType::Accept => {
            d.close();
            on_confirm();
        }
        _ => d.close(),
    });
    dialog.present();
}

#[allow(deprecated)]
fn show_project_snapshots_dialog(
    window: Option<gtk::Window>,
    project: Rc<RefCell<Project>>,
    library: Rc<RefCell<MediaLibrary>>,
    timeline_state: Rc<RefCell<TimelineState>>,
    on_project_changed: Rc<dyn Fn()>,
    on_project_reloaded: Rc<dyn Fn()>,
) {
    let dialog = gtk::Dialog::builder()
        .title("Project Snapshots")
        .modal(true)
        .default_width(520)
        .build();
    dialog.set_transient_for(window.as_ref());
    dialog.add_button("Close", gtk::ResponseType::Close);

    let content = gtk::Box::new(gtk::Orientation::Vertical, 12);
    content.set_margin_start(16);
    content.set_margin_end(16);
    content.set_margin_top(16);
    content.set_margin_bottom(16);

    let description = gtk::Label::new(Some(
        "Snapshots capture named milestone versions of the current project. Restoring one loads that version into the editor and keeps your main save target unchanged until you save again.",
    ));
    description.set_wrap(true);
    description.set_xalign(0.0);

    let snapshot_dropdown = gtk::DropDown::from_strings(&[]);
    snapshot_dropdown.set_hexpand(true);

    let details_label = gtk::Label::new(None);
    details_label.set_wrap(true);
    details_label.set_xalign(0.0);

    let empty_label = gtk::Label::new(None);
    empty_label.set_wrap(true);
    empty_label.set_xalign(0.0);
    empty_label.add_css_class("dim-label");

    let action_row = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    let btn_restore = gtk::Button::with_label("Restore Selected");
    let btn_delete = gtk::Button::with_label("Delete Selected");
    btn_restore.set_tooltip_text(Some(
        "Restore the selected snapshot into the current project",
    ));
    btn_delete.set_tooltip_text(Some("Delete the selected snapshot from disk"));
    action_row.append(&btn_restore);
    action_row.append(&btn_delete);

    content.append(&description);
    content.append(&snapshot_dropdown);
    content.append(&details_label);
    content.append(&empty_label);
    content.append(&action_row);
    dialog.content_area().append(&content);

    let entries_state: Rc<RefCell<Vec<crate::project_versions::ProjectSnapshotEntry>>> =
        Rc::new(RefCell::new(Vec::new()));

    let sync_selection_state: Rc<dyn Fn()> = {
        let snapshot_dropdown = snapshot_dropdown.clone();
        let btn_restore = btn_restore.clone();
        let btn_delete = btn_delete.clone();
        let details_label = details_label.clone();
        let empty_label = empty_label.clone();
        let entries_state = entries_state.clone();
        Rc::new(move || {
            let selected = snapshot_dropdown.selected() as usize;
            let entries = entries_state.borrow();
            if let Some(entry) = entries.get(selected) {
                btn_restore.set_sensitive(true);
                btn_delete.set_sensitive(true);
                let source = entry
                    .metadata
                    .project_file_path
                    .as_deref()
                    .unwrap_or("Unsaved project");
                details_label.set_text(&format!(
                    "Created {} • Source {}",
                    crate::project_versions::format_snapshot_timestamp(
                        entry.metadata.created_at_unix_secs
                    ),
                    source
                ));
                empty_label.set_visible(false);
            } else {
                btn_restore.set_sensitive(false);
                btn_delete.set_sensitive(false);
                details_label.set_text("No snapshot selected.");
                empty_label.set_visible(true);
            }
        })
    };

    {
        let sync_selection_state = sync_selection_state.clone();
        snapshot_dropdown.connect_selected_notify(move |_| {
            sync_selection_state();
        });
    }

    let refresh_entries: Rc<dyn Fn()> = {
        let project = project.clone();
        let snapshot_dropdown = snapshot_dropdown.clone();
        let details_label = details_label.clone();
        let empty_label = empty_label.clone();
        let entries_state = entries_state.clone();
        let sync_selection_state = sync_selection_state.clone();
        Rc::new(move || {
            let entries = {
                let proj = project.borrow();
                crate::project_versions::list_project_snapshots_for_project(&proj)
            };
            let has_entries = !entries.is_empty();
            let model = gtk::StringList::new(&[]);
            for entry in &entries {
                model.append(&format!(
                    "{} — {}",
                    entry.metadata.snapshot_name,
                    crate::project_versions::format_snapshot_timestamp(
                        entry.metadata.created_at_unix_secs
                    )
                ));
            }
            *entries_state.borrow_mut() = entries;
            snapshot_dropdown.set_model(Some(&model));
            snapshot_dropdown.set_sensitive(has_entries);
            snapshot_dropdown.set_selected(0);
            if has_entries {
                empty_label.set_text("");
            } else {
                empty_label.set_text("No named snapshots exist for the current project yet.");
                details_label
                    .set_text("Create a snapshot from the Export menu to capture a milestone.");
            }
            sync_selection_state();
        })
    };

    {
        let window = window.clone();
        let project = project.clone();
        let library = library.clone();
        let timeline_state = timeline_state.clone();
        let on_project_changed = on_project_changed.clone();
        let on_project_reloaded = on_project_reloaded.clone();
        let entries_state = entries_state.clone();
        let snapshot_dropdown = snapshot_dropdown.clone();
        let dialog_weak = dialog.downgrade();
        btn_restore.connect_clicked(move |_| {
            let Some(entry) = entries_state
                .borrow()
                .get(snapshot_dropdown.selected() as usize)
                .cloned()
            else {
                return;
            };
            let on_project_changed_guard = on_project_changed.clone();
            let action: Rc<dyn Fn()> = Rc::new({
                let project = project.clone();
                let timeline_state = timeline_state.clone();
                let on_project_changed = on_project_changed.clone();
                let on_project_reloaded = on_project_reloaded.clone();
                let dialog_weak = dialog_weak.clone();
                let snapshot_id = entry.metadata.id.clone();
                move || {
                    if let Some(dialog) = dialog_weak.upgrade() {
                        dialog.close();
                    }
                    let preserved_file_path = project.borrow().file_path.clone();
                    let snapshot_id_for_load = snapshot_id.clone();
                    restore_project_version_async(
                        project.clone(),
                        timeline_state.clone(),
                        on_project_changed.clone(),
                        on_project_reloaded.clone(),
                        preserved_file_path,
                        move || {
                            crate::project_versions::load_project_snapshot(&snapshot_id_for_load)
                                .map(|(_, project)| project)
                        },
                    );
                }
            });
            confirm_unsaved_then(
                window.clone(),
                project.clone(),
                library.clone(),
                on_project_changed_guard,
                action,
            );
        });
    }

    {
        let window = window.clone();
        let refresh_entries = refresh_entries.clone();
        let entries_state = entries_state.clone();
        let snapshot_dropdown = snapshot_dropdown.clone();
        btn_delete.connect_clicked(move |_| {
            let Some(entry) = entries_state
                .borrow()
                .get(snapshot_dropdown.selected() as usize)
                .cloned()
            else {
                return;
            };
            let on_confirm: Rc<dyn Fn()> = Rc::new({
                let refresh_entries = refresh_entries.clone();
                let snapshot_id = entry.metadata.id.clone();
                move || {
                    if let Err(e) = crate::project_versions::delete_project_snapshot(&snapshot_id) {
                        log::error!("{e}");
                    }
                    refresh_entries();
                }
            });
            confirm_delete_snapshot(window.clone(), &entry.metadata.snapshot_name, on_confirm);
        });
    }

    dialog.connect_response(|d, _| {
        d.close();
    });
    refresh_entries();
    dialog.present();
}

enum ExportProjectWithMediaUiEvent {
    Progress(fcpxml::writer::ExportProjectWithMediaProgress),
    Done { library_dir: std::path::PathBuf },
    Error(String),
}

enum CollectFilesUiEvent {
    Progress(fcpxml::writer::CollectFilesProgress),
    Done(fcpxml::writer::CollectFilesManifest),
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
        4 => Container::Gif,
        _ => Container::Mp4,
    }
}

fn selected_from_container(container: &Container) -> u32 {
    match container {
        Container::Mp4 => 0,
        Container::Mov => 1,
        Container::WebM => 2,
        Container::Mkv => 3,
        Container::Gif => 4,
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

fn audio_channel_layout_from_selected(selected: u32) -> AudioChannelLayout {
    match selected {
        1 => AudioChannelLayout::Surround51,
        2 => AudioChannelLayout::Surround71,
        _ => AudioChannelLayout::Stereo,
    }
}

fn selected_from_audio_channel_layout(layout: &AudioChannelLayout) -> u32 {
    match layout {
        AudioChannelLayout::Stereo => 0,
        AudioChannelLayout::Surround51 => 1,
        AudioChannelLayout::Surround71 => 2,
    }
}

fn collect_export_options(
    vc_combo: &gtk::DropDown,
    ct_combo: &gtk::DropDown,
    or_combo: &gtk::DropDown,
    crf_slider: &gtk::Scale,
    ac_combo: &gtk::DropDown,
    ab_entry: &gtk::Entry,
    gif_fps_spin: &gtk::SpinButton,
    cl_combo: &gtk::DropDown,
) -> ExportOptions {
    let (output_width, output_height) = output_resolution_from_selected(or_combo.selected());
    let container = container_from_selected(ct_combo.selected());
    let gif_fps = if container == Container::Gif {
        Some(gif_fps_spin.value() as u32)
    } else {
        None
    };
    ExportOptions {
        video_codec: video_codec_from_selected(vc_combo.selected()),
        container,
        output_width,
        output_height,
        crf: crf_slider.value() as u32,
        audio_codec: audio_codec_from_selected(ac_combo.selected()),
        audio_bitrate_kbps: ab_entry.text().parse::<u32>().unwrap_or(192),
        gif_fps,
        audio_channel_layout: audio_channel_layout_from_selected(cl_combo.selected()),
        hdr_passthrough: false,
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
    gif_fps_spin: &gtk::SpinButton,
    cl_combo: &gtk::DropDown,
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
    if let Some(fps) = options.gif_fps {
        gif_fps_spin.set_value(fps as f64);
    }
    cl_combo.set_selected(selected_from_audio_channel_layout(
        &options.audio_channel_layout,
    ));
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
    library: Rc<RefCell<MediaLibrary>>,
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
    let library_c = library.clone();
    let on_project_changed_c = on_project_changed.clone();
    let on_continue_c = on_continue.clone();
    dialog.connect_response(move |d, resp| match resp {
        gtk::ResponseType::Reject => {
            // User chose to discard — remove autosave for this project.
            crate::project_versions::delete_autosave_for_project(&project_c.borrow());
            d.close();
            on_continue_c();
        }
        gtk::ResponseType::Accept => {
            d.close();
            let existing_path = project_c.borrow().file_path.clone();
            if let Some(path) = existing_path {
                match save_project_to_path(&project_c, &library_c, std::path::Path::new(&path)) {
                    Ok(()) => {
                        on_project_changed_c();
                        on_continue_c();
                    }
                    Err(e) => log::error!("{e}"),
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
                let library_s = library_c.clone();
                let on_project_changed_s = on_project_changed_c.clone();
                let on_continue_s = on_continue_c.clone();
                file_dialog.save(window.as_ref(), gio::Cancellable::NONE, move |result| {
                    if let Ok(file) = result {
                        if let Some(path) = file.path() {
                            match save_project_to_path(&project_s, &library_s, &path) {
                                Ok(()) => {
                                    on_project_changed_s();
                                    on_continue_s();
                                }
                                Err(e) => log::error!("{e}"),
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

#[allow(deprecated)]
fn choose_collect_files_target(
    window: Option<gtk::Window>,
    project: Rc<RefCell<Project>>,
    on_selected: Rc<dyn Fn(std::path::PathBuf, fcpxml::writer::CollectFilesMode, bool)>,
) {
    let dialog = gtk::Dialog::builder()
        .title("Collect Files")
        .modal(true)
        .default_width(460)
        .build();
    dialog.set_transient_for(window.as_ref());
    dialog.add_button("Cancel", gtk::ResponseType::Cancel);
    dialog.add_button("Choose Folder…", gtk::ResponseType::Accept);

    let content = gtk::Box::new(gtk::Orientation::Vertical, 12);
    content.set_margin_start(16);
    content.set_margin_end(16);
    content.set_margin_top(16);
    content.set_margin_bottom(16);

    let label = gtk::Label::new(Some(
        "Copy project media into a destination folder for archival or transfer. Choose whether to collect only timeline-used media or the entire project library. Existing clip LUT files used on the timeline are included automatically.",
    ));
    label.set_wrap(true);
    label.set_xalign(0.0);

    let timeline_only =
        gtk::CheckButton::with_label(fcpxml::writer::CollectFilesMode::TimelineUsedOnly.ui_label());
    timeline_only.set_active(true);
    let entire_library =
        gtk::CheckButton::with_label(fcpxml::writer::CollectFilesMode::EntireLibrary.ui_label());
    entire_library.set_group(Some(&timeline_only));

    let mode_hint = gtk::Label::new(Some(
        "Timeline-used only makes a smaller handoff copy. Entire library also includes imported clips that are not currently on the timeline.",
    ));
    mode_hint.set_wrap(true);
    mode_hint.set_xalign(0.0);
    mode_hint.add_css_class("dim-label");

    let use_collected_locations_on_next_save =
        gtk::CheckButton::with_label("Use collected locations on next save");
    let use_locations_hint = gtk::Label::new(Some(
        "After copying finishes, update the current project to point at the collected files so the next project save/export writes those paths.",
    ));
    use_locations_hint.set_wrap(true);
    use_locations_hint.set_xalign(0.0);
    use_locations_hint.add_css_class("dim-label");

    content.append(&label);
    content.append(&timeline_only);
    content.append(&entire_library);
    content.append(&mode_hint);
    content.append(&use_collected_locations_on_next_save);
    content.append(&use_locations_hint);
    dialog.content_area().append(&content);

    dialog.connect_response(move |d, resp| match resp {
        gtk::ResponseType::Accept => {
            let mode = if entire_library.is_active() {
                fcpxml::writer::CollectFilesMode::EntireLibrary
            } else {
                fcpxml::writer::CollectFilesMode::TimelineUsedOnly
            };
            d.close();

            let file_dialog = gtk::FileDialog::new();
            file_dialog.set_title("Choose Destination Folder");
            if let Some(file_path) = project.borrow().file_path.clone() {
                if let Some(parent) = std::path::Path::new(&file_path).parent() {
                    file_dialog.set_initial_folder(Some(&gio::File::for_path(parent)));
                }
            }

            let window = window.clone();
            let on_selected = on_selected.clone();
            let use_collected_locations_on_next_save =
                use_collected_locations_on_next_save.is_active();
            file_dialog.select_folder(window.as_ref(), gio::Cancellable::NONE, move |result| {
                if let Ok(file) = result {
                    if let Some(path) = file.path() {
                        on_selected(path, mode, use_collected_locations_on_next_save);
                    }
                }
            });
        }
        _ => d.close(),
    });
    dialog.present();
}

/// Build the main `HeaderBar` toolbar.
#[allow(deprecated)]
pub fn build_toolbar(
    project: Rc<RefCell<Project>>,
    library: Rc<RefCell<MediaLibrary>>,
    timeline_state: Rc<RefCell<TimelineState>>,
    bg_removal_cache: Rc<RefCell<crate::media::bg_removal_cache::BgRemovalCache>>,
    frame_interp_cache: Rc<RefCell<crate::media::frame_interp_cache::FrameInterpCache>>,
    render_replace_cache: Rc<RefCell<crate::media::render_replace_cache::RenderReplaceCache>>,
    on_project_changed: impl Fn() + 'static + Clone,
    on_project_reloaded: impl Fn() + 'static + Clone,
    on_show_editor: impl Fn() + 'static + Clone,
    on_use_collected_locations: impl Fn(fcpxml::writer::CollectFilesManifest) + 'static + Clone,
    on_export_frame: impl Fn() + 'static + Clone,
    on_show_project_health: impl Fn(Option<gtk::Window>) + 'static + Clone,
    on_record_voiceover: impl Fn() + 'static + Clone,
    on_replay_onboarding: impl Fn() + 'static + Clone,
) -> (HeaderBar, Button, Button) {
    let header = HeaderBar::new();

    let title = Label::new(Some("UltimateSlice"));
    title.add_css_class("title");
    header.set_title_widget(Some(&title));

    // New project
    let btn_new = Button::with_label("New");
    btn_new.set_tooltip_text(Some("New project (Ctrl+N)"));
    {
        let project = project.clone();
        let library = library.clone();
        let timeline_state = timeline_state.clone();
        let on_project_changed = on_project_changed.clone();
        let on_project_reloaded = on_project_reloaded.clone();
        let on_show_editor = on_show_editor.clone();
        btn_new.connect_clicked(move |btn| {
            let window = btn.root().and_then(|r| r.downcast::<gtk::Window>().ok());
            let on_project_changed_cb: Rc<dyn Fn()> = Rc::new(on_project_changed.clone());
            let action: Rc<dyn Fn()> = Rc::new({
                let project = project.clone();
                let timeline_state = timeline_state.clone();
                let on_project_changed = on_project_changed.clone();
                let on_project_reloaded = on_project_reloaded.clone();
                let on_show_editor = on_show_editor.clone();
                move || {
                    // Clean up autosave for the project being replaced.
                    crate::project_versions::delete_autosave_for_project(&project.borrow());
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
                    on_show_editor();
                }
            });
            confirm_unsaved_then(
                window,
                project.clone(),
                library.clone(),
                on_project_changed_cb,
                action,
            );
        });
    }
    header.pack_start(&btn_new);

    // Open project XML
    let btn_open = Button::with_label("Open…");
    btn_open.set_tooltip_text(Some("Open project file (Ctrl+O)"));
    {
        let project = project.clone();
        let library = library.clone();
        let timeline_state = timeline_state.clone();
        let on_project_changed = on_project_changed.clone();
        let on_project_reloaded = on_project_reloaded.clone();
        let on_show_editor = on_show_editor.clone();
        btn_open.connect_clicked(move |btn| {
            let window = btn.root().and_then(|r| r.downcast::<gtk::Window>().ok());
            let on_project_changed_cb: Rc<dyn Fn()> = Rc::new(on_project_changed.clone());
            let action: Rc<dyn Fn()> = Rc::new({
                let project = project.clone();
                let on_project_changed = on_project_changed.clone();
                let on_project_reloaded = on_project_reloaded.clone();
                let on_show_editor = on_show_editor.clone();
                let timeline_state_cb = timeline_state.clone();
                let window = window.clone();
                move || {
                    let dialog = gtk::FileDialog::new();
                    dialog.set_title("Open Project");

                    let filter = gtk::FileFilter::new();
                    filter.add_pattern("*.uspxml");
                    filter.add_pattern("*.fcpxml");
                    filter.add_pattern("*.xml");
                    filter.add_pattern("*.otio");
                    filter.set_name(Some("Project Files"));
                    let filters = gio::ListStore::new::<gtk::FileFilter>();
                    filters.append(&filter);
                    dialog.set_filters(Some(&filters));

                    let project = project.clone();
                    let on_project_changed = on_project_changed.clone();
                    let on_project_reloaded = on_project_reloaded.clone();
                    let on_show_editor = on_show_editor.clone();
                    let timeline_state_cb = timeline_state_cb.clone();
                    let window = window.clone();
                    dialog.open(window.as_ref(), gio::Cancellable::NONE, move |result| {
                        if let Ok(file) = result {
                            if let Some(path) = file.path() {
                                let path_str = path.to_string_lossy().to_string();
                                // Parse FCPXML on a background thread to avoid blocking the UI.
                                let (tx, rx) =
                                    std::sync::mpsc::sync_channel::<Result<Project, String>>(1);
                                let path_bg = path.clone();
                                std::thread::spawn(move || {
                                    let result =
                                        crate::ui::project_loader::load_project_from_path(&path_bg);
                                    let _ = tx.send(result);
                                });
                                let project = project.clone();
                                let on_project_changed = on_project_changed.clone();
                                let on_project_reloaded = on_project_reloaded.clone();
                                let on_show_editor = on_show_editor.clone();
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
                                            on_show_editor();
                                            glib::ControlFlow::Break
                                        }
                                        Ok(Err(e)) => {
                                            log::error!("{e}");
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
            confirm_unsaved_then(
                window,
                project.clone(),
                library.clone(),
                on_project_changed_cb,
                action,
            );
        });
    }
    header.pack_start(&btn_open);

    // Open Recent — popover with the last 10 projects
    let btn_recent = gtk::MenuButton::new();
    btn_recent.set_label("Recent");
    btn_recent.set_tooltip_text(Some("Open a recently used project"));
    {
        let project = project.clone();
        let library = library.clone();
        let timeline_state = timeline_state.clone();
        let on_project_changed = on_project_changed.clone();
        let on_project_reloaded = on_project_reloaded.clone();
        let on_show_editor = on_show_editor.clone();

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
                    let library = library.clone();
                    let timeline_state = timeline_state.clone();
                    let on_project_changed = on_project_changed.clone();
                    let on_project_reloaded = on_project_reloaded.clone();
                    let on_show_editor = on_show_editor.clone();
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
                            let on_show_editor = on_show_editor.clone();
                            let path_owned = path_owned.clone();
                            let pop_weak = pop_weak.clone();
                            move || {
                                if let Some(pop) = pop_weak.upgrade() {
                                    pop.popdown();
                                }
                                // Parse FCPXML on a background thread to avoid blocking the UI.
                                let (tx, rx) =
                                    std::sync::mpsc::sync_channel::<Result<Project, String>>(1);
                                let path_bg = std::path::PathBuf::from(&path_owned);
                                std::thread::spawn(move || {
                                    let result =
                                        crate::ui::project_loader::load_project_from_path(&path_bg)
                                            .map_err(|e| {
                                                format!("Failed to open recent project: {e}")
                                            });
                                    let _ = tx.send(result);
                                });
                                let project = project.clone();
                                let timeline_state = timeline_state.clone();
                                let on_project_changed = on_project_changed.clone();
                                let on_project_reloaded = on_project_reloaded.clone();
                                let on_show_editor = on_show_editor.clone();
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
                                            on_show_editor();
                                            glib::ControlFlow::Break
                                        }
                                        Ok(Err(e)) => {
                                            log::error!("{e}");
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
                            library.clone(),
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

    let btn_help = gtk::MenuButton::new();
    btn_help.set_label("Help");
    btn_help.set_tooltip_text(Some("Keyboard shortcuts and onboarding help"));
    {
        let pop = gtk::Popover::new();
        let vbox = gtk::Box::new(gtk::Orientation::Vertical, 2);
        vbox.set_margin_start(4);
        vbox.set_margin_end(4);
        vbox.set_margin_top(4);
        vbox.set_margin_bottom(4);

        let btn_shortcuts = gtk::Button::with_label("Keyboard Shortcuts");
        btn_shortcuts.add_css_class("flat");
        btn_shortcuts.set_halign(gtk::Align::Fill);
        btn_shortcuts.set_hexpand(true);
        {
            let pop_weak = pop.downgrade();
            btn_shortcuts.connect_clicked(move |btn| {
                if let Some(pop) = pop_weak.upgrade() {
                    pop.popdown();
                }
                if let Some(window) = btn.root().and_then(|r| r.downcast::<gtk::Window>().ok()) {
                    crate::ui::timeline::widget::show_shortcuts_dialog(&window);
                }
            });
        }
        vbox.append(&btn_shortcuts);

        let btn_replay_tour = gtk::Button::with_label("Replay Tour");
        btn_replay_tour.add_css_class("flat");
        btn_replay_tour.set_halign(gtk::Align::Fill);
        btn_replay_tour.set_hexpand(true);
        {
            let pop_weak = pop.downgrade();
            let on_replay_onboarding = on_replay_onboarding.clone();
            btn_replay_tour.connect_clicked(move |_| {
                if let Some(pop) = pop_weak.upgrade() {
                    pop.popdown();
                }
                on_replay_onboarding();
            });
        }
        vbox.append(&btn_replay_tour);

        pop.set_child(Some(&vbox));
        btn_help.set_popover(Some(&pop));
    }
    header.pack_start(&btn_help);
    let btn_save = Button::with_label("Save…");
    btn_save.set_tooltip_text(Some("Save project XML (Ctrl+S)"));
    {
        let project = project.clone();
        let library = library.clone();
        let on_project_changed = on_project_changed.clone();
        btn_save.connect_clicked(move |btn| {
            // If the project already has a save path, save directly without a dialog.
            let existing_path = project.borrow().file_path.clone();
            if let Some(ref path_str) = existing_path {
                let path = std::path::Path::new(path_str);
                match save_project_to_path(&project, &library, path) {
                    Ok(()) => {
                        println!("Saved to {}", path.display());
                        on_project_changed();
                    }
                    Err(e) => log::error!("{e}"),
                }
                return;
            }

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
            let library = library.clone();
            let on_project_changed = on_project_changed.clone();
            let window = btn.root().and_then(|r| r.downcast::<gtk::Window>().ok());

            dialog.save(window.as_ref(), gio::Cancellable::NONE, move |result| {
                if let Ok(file) = result {
                    if let Some(path) = file.path() {
                        match save_project_to_path(&project, &library, &path) {
                            Ok(()) => {
                                println!("Saved to {}", path.display());
                                on_project_changed();
                            }
                            Err(e) => {
                                log::error!("{e}");
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
            // Source of truth: ASPECT_RATIO_PRESETS at the top of this file.

            // Detect current aspect ratio and resolution index. Falls back to
            // the "Custom" sentinel index (= ASPECT_RATIO_PRESETS.len()) when
            // the project's W×H doesn't match any preset.
            let custom_ar_idx = ASPECT_RATIO_PRESETS.len() as u32;
            let (init_ar_idx, init_res_idx) = {
                let mut found = (custom_ar_idx, 0u32);
                'outer: for (ai, group) in ASPECT_RATIO_PRESETS.iter().enumerate() {
                    for (ri, preset) in group.presets.iter().enumerate() {
                        if preset.width == proj.width && preset.height == proj.height {
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
            let mut ar_dropdown_labels: Vec<&str> =
                ASPECT_RATIO_PRESETS.iter().map(|g| g.label).collect();
            ar_dropdown_labels.push(CUSTOM_ASPECT_LABEL);
            let ar_combo = gtk::DropDown::from_strings(&ar_dropdown_labels);
            grid.attach(&ar_label, 0, 0, 1, 1);
            grid.attach(&ar_combo, 1, 0, 2, 1);

            // Row 1: Resolution dropdown (hidden when Custom)
            let res_label = gtk::Label::new(Some("Resolution:"));
            res_label.set_halign(gtk::Align::End);
            let initial_res_strings: Vec<&str> =
                if let Some(group) = ASPECT_RATIO_PRESETS.get(init_ar_idx as usize) {
                    group.presets.iter().map(|p| p.label).collect()
                } else {
                    vec!["1920 × 1080  (1080p HD)"]
                };
            let res_string_list = gtk::StringList::new(
                &initial_res_strings
                    .iter()
                    .map(|s| *s)
                    .collect::<Vec<&str>>(),
            );
            let res_combo = gtk::DropDown::builder().model(&res_string_list).build();
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
            let is_custom = init_ar_idx == custom_ar_idx;
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
                ar_combo.connect_selected_notify(move |combo| {
                    let idx = combo.selected() as usize;
                    if let Some(group) = ASPECT_RATIO_PRESETS.get(idx) {
                        // Preset aspect ratio: show resolution dropdown, hide custom
                        let labels: Vec<&str> = group.presets.iter().map(|p| p.label).collect();
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

            // Row 3: Frame rate preset
            let fps_label = gtk::Label::new(Some("Frame Rate:"));
            fps_label.set_halign(gtk::Align::End);
            let fps_dropdown_labels: Vec<&str> =
                FRAMERATE_OPTIONS.iter().map(|o| o.label).collect();
            let fps_combo = gtk::DropDown::from_strings(&fps_dropdown_labels);
            let fps_idx = FRAMERATE_OPTIONS
                .iter()
                .position(|o| {
                    o.num == proj.frame_rate.numerator && o.den == proj.frame_rate.denominator
                })
                .or_else(|| {
                    // Backwards compat: 2997/125 is the simplified form of
                    // 24000/1001 (= 23.976). Old project files may store either.
                    if proj.frame_rate.numerator == 2997 && proj.frame_rate.denominator == 125 {
                        Some(0)
                    } else {
                        None
                    }
                })
                .unwrap_or(DEFAULT_FRAMERATE_INDEX) as u32;
            fps_combo.set_selected(fps_idx);
            grid.attach(&fps_label, 0, 3, 1, 1);
            grid.attach(&fps_combo, 1, 3, 2, 1);

            dialog.content_area().append(&grid);
            dialog.add_button("Cancel", gtk::ResponseType::Cancel);
            dialog.add_button("Apply", gtk::ResponseType::Accept);

            drop(proj);
            let project = project.clone();
            let on_project_changed = on_project_changed.clone();
            dialog.connect_response(move |d, resp| {
                if resp == gtk::ResponseType::Accept {
                    let ar_idx = ar_combo.selected() as usize;
                    let (w, h) = if let Some(group) = ASPECT_RATIO_PRESETS.get(ar_idx) {
                        // Preset aspect ratio + resolution
                        let res_idx = res_combo.selected() as usize;
                        let preset = group
                            .presets
                            .get(res_idx)
                            .or_else(|| group.presets.first())
                            .expect("aspect ratio group should have at least one preset");
                        (preset.width, preset.height)
                    } else {
                        // Custom
                        (w_spin.value() as u32, h_spin.value() as u32)
                    };
                    let opt = FRAMERATE_OPTIONS
                        .get(fps_combo.selected() as usize)
                        .unwrap_or(&FRAMERATE_OPTIONS[DEFAULT_FRAMERATE_INDEX]);
                    let fr = FrameRate {
                        numerator: opt.num,
                        denominator: opt.den,
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
        let frame_interp_cache = frame_interp_cache.clone();
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
            btn_save_preset
                .set_tooltip_text(Some("Save the current export settings as a named preset"));
            btn_update_preset
                .set_tooltip_text(Some("Update the selected preset with the current settings"));
            btn_delete_preset.set_tooltip_text(Some("Delete the selected export preset"));
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
                "Animated GIF (.gif)",
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

            // CRF (hidden for GIF)
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

            // GIF frame rate (shown only for GIF container)
            let gif_fps_label = gtk::Label::new(Some("GIF Frame Rate:"));
            gif_fps_label.set_halign(gtk::Align::End);
            let gif_fps_spin = gtk::SpinButton::with_range(1.0, 30.0, 1.0);
            gif_fps_spin.set_value(15.0);
            gif_fps_spin.set_tooltip_text(Some(
                "Frames per second for the animated GIF (lower = smaller file)",
            ));
            grid.attach(&gif_fps_label, 0, 5, 1, 1);
            grid.attach(&gif_fps_spin, 1, 5, 1, 1);
            gif_fps_label.set_visible(false);
            gif_fps_spin.set_visible(false);

            // Audio codec (hidden for GIF)
            let ac_label = gtk::Label::new(Some("Audio Codec:"));
            ac_label.set_halign(gtk::Align::End);
            let ac_combo = gtk::DropDown::from_strings(&[
                "AAC",
                "Opus",
                "FLAC (lossless)",
                "PCM (uncompressed)",
            ]);
            ac_combo.set_selected(0);
            grid.attach(&ac_label, 0, 6, 1, 1);
            grid.attach(&ac_combo, 1, 6, 1, 1);

            // Audio bitrate (hidden for GIF)
            let ab_label = gtk::Label::new(Some("Audio Bitrate:"));
            ab_label.set_halign(gtk::Align::End);
            let ab_entry = gtk::Entry::new();
            ab_entry.set_text("192");
            ab_entry.set_tooltip_text(Some("Audio bitrate in kbps (ignored for FLAC/PCM)"));
            grid.attach(&ab_label, 0, 7, 1, 1);
            grid.attach(&ab_entry, 1, 7, 1, 1);

            // Audio channel layout — Stereo (default) / 5.1 / 7.1 surround.
            // Hidden for GIF (no audio).
            let cl_label = gtk::Label::new(Some("Audio Channels:"));
            cl_label.set_halign(gtk::Align::End);
            let cl_combo = gtk::DropDown::from_strings(&["Stereo", "5.1 Surround", "7.1 Surround"]);
            cl_combo.set_selected(0);
            cl_combo.set_tooltip_text(Some(
                "Output audio channel layout. Surround uses role-based auto-routing \
                 (Dialogue → Center, Music → Front L/R, Effects → Front+Surround) \
                 with an automatic LFE bass tap. Per-track overrides live in the \
                 Inspector. Supported by AAC / Opus / FLAC / PCM. Not used for GIF.",
            ));
            grid.attach(&cl_label, 0, 8, 1, 1);
            grid.attach(&cl_combo, 1, 8, 1, 1);

            // Connect container selection to show/hide GIF-specific and audio rows
            {
                let gif_fps_label = gif_fps_label.clone();
                let gif_fps_spin = gif_fps_spin.clone();
                let crf_label = crf_label.clone();
                let crf_box = crf_box.clone();
                let ac_label = ac_label.clone();
                let ac_combo = ac_combo.clone();
                let ab_label = ab_label.clone();
                let ab_entry = ab_entry.clone();
                let cl_label = cl_label.clone();
                let cl_combo = cl_combo.clone();
                let vc_label = vc_label.clone();
                let vc_combo = vc_combo.clone();
                ct_combo.connect_selected_notify(move |ct| {
                    let is_gif = ct.selected() == 4;
                    gif_fps_label.set_visible(is_gif);
                    gif_fps_spin.set_visible(is_gif);
                    // Hide video codec + CRF rows for GIF (GIF handles its own encoding)
                    vc_label.set_visible(!is_gif);
                    vc_combo.set_visible(!is_gif);
                    crf_label.set_visible(!is_gif);
                    crf_box.set_visible(!is_gif);
                    ac_label.set_visible(!is_gif);
                    ac_combo.set_visible(!is_gif);
                    ab_label.set_visible(!is_gif);
                    ab_entry.set_visible(!is_gif);
                    cl_label.set_visible(!is_gif);
                    cl_combo.set_visible(!is_gif);
                });
            }

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
                            &gif_fps_spin,
                            &cl_combo,
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
                let gif_fps_spin = gif_fps_spin.clone();
                let cl_combo = cl_combo.clone();
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
                        &gif_fps_spin,
                        &cl_combo,
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
                let gif_fps_spin = gif_fps_spin.clone();
                let cl_combo = cl_combo.clone();
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
                        let gif_fps_spin = gif_fps_spin.clone();
                        let cl_combo = cl_combo.clone();
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
                                    &gif_fps_spin,
                                    &cl_combo,
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
                let gif_fps_spin = gif_fps_spin.clone();
                let cl_combo = cl_combo.clone();
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
                            &gif_fps_spin,
                            &cl_combo,
                        );
                        let ok = state
                            .upsert_preset(ExportPreset::from_export_options(
                                existing_name,
                                &options,
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
            opt_dialog.add_button("Add to Queue", gtk::ResponseType::Other(1));
            opt_dialog.add_button("Choose Output File…", gtk::ResponseType::Accept);

            let project = project.clone();
            let bg_removal_cache = bg_removal_cache.clone();
            let frame_interp_cache = frame_interp_cache.clone();
            let render_replace_cache = render_replace_cache.clone();
            opt_dialog.connect_response(move |d, resp| {
                if resp != gtk::ResponseType::Accept && resp != gtk::ResponseType::Other(1) {
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
                    &gif_fps_spin,
                    &cl_combo,
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
                drop(state);

                // "Add to Queue" — prompt for output path then add to the queue without exporting
                if resp == gtk::ResponseType::Other(1) {
                    d.close();
                    let file_dialog = gtk::FileDialog::new();
                    file_dialog.set_title("Add to Export Queue — Choose Output File");
                    file_dialog.set_initial_name(Some(&format!("export.{ext}")));
                    let window: Option<gtk::Window> = None;
                    let options_q = options.clone();
                    file_dialog.save(window.as_ref(), gio::Cancellable::NONE, move |result| {
                        if let Ok(file) = result {
                            if let Some(path) = file.path() {
                                let output = path.to_string_lossy().to_string();
                                let preset =
                                    ExportPreset::from_export_options("(queued)", &options_q);
                                let job = ExportQueueJob::new(&output, preset);
                                let mut queue = ui_state::load_export_queue_state();
                                queue.jobs.push(job);
                                ui_state::save_export_queue_state(&queue);
                                // Brief confirmation toast via a small notification window
                                let note = gtk::Window::builder()
                                    .title("Added to Queue")
                                    .default_width(320)
                                    .build();
                                let lbl = gtk::Label::new(Some(&format!(
                                    "Added to export queue:\n{}",
                                    std::path::Path::new(&output)
                                        .file_name()
                                        .and_then(|n| n.to_str())
                                        .unwrap_or(&output)
                                )));
                                lbl.set_margin_start(16);
                                lbl.set_margin_end(16);
                                lbl.set_margin_top(16);
                                lbl.set_margin_bottom(16);
                                note.set_child(Some(&lbl));
                                note.present();
                                let note_weak = note.downgrade();
                                glib::timeout_add_local_once(
                                    std::time::Duration::from_secs(2),
                                    move || {
                                        if let Some(w) = note_weak.upgrade() {
                                            w.close();
                                        }
                                    },
                                );
                            }
                        }
                    });
                    return;
                }

                d.close();

                // Now open file-chooser for the output path
                let file_dialog = gtk::FileDialog::new();
                file_dialog.set_title("Export — Choose Output File");
                file_dialog.set_initial_name(Some(&format!("export.{ext}")));

                let window: Option<gtk::Window> = None; // no parent at this point
                let project = project.clone();
                let bg_removal_cache = bg_removal_cache.clone();
                let frame_interp_cache = frame_interp_cache.clone();
                let render_replace_cache = render_replace_cache.clone();
                file_dialog.save(window.as_ref(), gio::Cancellable::NONE, move |result| {
                    if let Ok(file) = result {
                        if let Some(path) = file.path() {
                            let output = path.to_string_lossy().to_string();
                            let output_clone = output.clone();
                            let proj = project.borrow().clone();
                            let opts = options.clone();
                            let bg_paths = bg_removal_cache.borrow().paths.clone();
                            let interp_paths =
                                frame_interp_cache.borrow().snapshot_paths_by_clip_id(&proj);
                            // Render-and-Replace sidecar snapshot. Any clip
                            // with render_replace_enabled and a ready bake
                            // will have its source swapped for the baked
                            // ProRes sidecar during export, skipping the
                            // baked-scope filter chain. Empty map → all
                            // clips render live as before.
                            let rr_paths = render_replace_cache.borrow().paths.clone();
                            let (tx, rx) = std::sync::mpsc::channel::<ExportProgress>();

                            std::thread::spawn(move || {
                                if let Err(e) = export_project(
                                    &proj,
                                    &output_clone,
                                    opts,
                                    None,
                                    &bg_paths,
                                    &interp_paths,
                                    &rr_paths,
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
                                                log::error!("Export error: {e}");
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
                                        log::error!("Export-with-media error: {e}");
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
    let btn_collect_files = gtk::Button::with_label("Collect Files…");
    btn_collect_files.add_css_class("flat");
    {
        let project = project.clone();
        let library = library.clone();
        let export_pop_weak = export_pop.downgrade();
        btn_collect_files.connect_clicked(move |btn| {
            if let Some(pop) = export_pop_weak.upgrade() {
                pop.popdown();
            }

            let window = btn.root().and_then(|r| r.downcast::<gtk::Window>().ok());
            let on_selected: Rc<dyn Fn(std::path::PathBuf, fcpxml::writer::CollectFilesMode, bool)> =
                Rc::new({
                    let project = project.clone();
                    let library = library.clone();
                    let on_use_collected_locations = on_use_collected_locations.clone();
                    move |destination_dir, mode, use_collected_locations_on_next_save| {
                        let destination_string = destination_dir.to_string_lossy().to_string();
                        let project_snapshot = project.borrow().clone();
                        let library_snapshot = library.borrow().items.clone();
                        let (tx, rx) = std::sync::mpsc::channel::<CollectFilesUiEvent>();
                        let destination_for_worker = destination_dir.clone();

                        std::thread::spawn(move || {
                            let result = fcpxml::writer::collect_files_with_manifest(
                                &project_snapshot,
                                &library_snapshot,
                                &destination_for_worker,
                                mode,
                                |progress| {
                                    let _ = tx.send(CollectFilesUiEvent::Progress(progress));
                                },
                            );
                            match result {
                                Ok(summary) => {
                                    let _ = tx.send(CollectFilesUiEvent::Done(summary));
                                }
                                Err(e) => {
                                    let _ = tx.send(CollectFilesUiEvent::Error(e.to_string()));
                                }
                            }
                        });

                        let progress_dialog = gtk::Window::builder()
                            .title("Collecting Files…")
                            .default_width(420)
                            .build();
                        let vbox = gtk::Box::new(gtk::Orientation::Vertical, 12);
                        vbox.set_margin_start(20);
                        vbox.set_margin_end(20);
                        vbox.set_margin_top(20);
                        vbox.set_margin_bottom(20);

                        let status_label = gtk::Label::new(Some(&format!(
                            "Preparing {} collection…",
                            mode.ui_label()
                        )));
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

                        let on_use_collected_locations = on_use_collected_locations.clone();
                        glib::timeout_add_local(std::time::Duration::from_millis(120), move || {
                            while let Ok(event) = rx.try_recv() {
                                match event {
                                    CollectFilesUiEvent::Progress(
                                        fcpxml::writer::CollectFilesProgress::Copying {
                                            copied_files,
                                            total_files,
                                            current_file,
                                        },
                                    ) => {
                                        let fraction = if total_files == 0 {
                                            0.0
                                        } else {
                                            (copied_files as f64) / (total_files as f64)
                                        };
                                        progress_bar.set_fraction(fraction.clamp(0.0, 1.0));
                                        progress_bar
                                            .set_text(Some(&format!("{:.0}%", fraction * 100.0)));
                                        status_label.set_text(&format!(
                                        "Copying {current_file} ({copied_files}/{total_files})…"
                                    ));
                                    }
                                    CollectFilesUiEvent::Done(manifest) => {
                                        let summary = manifest.result.clone();
                                        if use_collected_locations_on_next_save {
                                            on_use_collected_locations(manifest);
                                        }
                                        progress_bar.set_fraction(1.0);
                                        progress_bar.set_text(Some("Done!"));
                                        let status = if use_collected_locations_on_next_save {
                                            format!(
                                                "Collected {} media file(s) and {} LUT file(s) into {} and updated the project to use those files on the next save",
                                                summary.media_files_copied,
                                                summary.lut_files_copied,
                                                destination_string
                                            )
                                        } else {
                                            format!(
                                                "Collected {} media file(s) and {} LUT file(s) into {}",
                                                summary.media_files_copied,
                                                summary.lut_files_copied,
                                                destination_string
                                            )
                                        };
                                        status_label.set_text(&status);
                                        return glib::ControlFlow::Break;
                                    }
                                    CollectFilesUiEvent::Error(e) => {
                                        status_label.set_text(&format!("Error: {e}"));
                                        log::error!("Collect-files error: {e}");
                                        return glib::ControlFlow::Break;
                                    }
                                }
                            }
                            glib::ControlFlow::Continue
                        });
                    }
                });
            choose_collect_files_target(window, project.clone(), on_selected);
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
    let btn_create_snapshot = gtk::Button::with_label("Create Snapshot…");
    btn_create_snapshot.add_css_class("flat");
    {
        let project = project.clone();
        let library = library.clone();
        let export_pop_weak = export_pop.downgrade();
        btn_create_snapshot.connect_clicked(move |btn| {
            if let Some(pop) = export_pop_weak.upgrade() {
                pop.popdown();
            }
            let window = btn.root().and_then(|r| r.downcast::<gtk::Window>().ok());
            let on_selected: Rc<dyn Fn(String) -> Result<(), String>> = Rc::new({
                let project = project.clone();
                let library = library.clone();
                move |snapshot_name| {
                    create_named_snapshot(&project, &library, &snapshot_name).map(|_| ())
                }
            });
            choose_snapshot_name(window, on_selected);
        });
    }
    let btn_manage_snapshots = gtk::Button::with_label("Manage Snapshots…");
    btn_manage_snapshots.add_css_class("flat");
    {
        let project = project.clone();
        let library = library.clone();
        let timeline_state = timeline_state.clone();
        let on_project_changed: Rc<dyn Fn()> = Rc::new(on_project_changed.clone());
        let on_project_reloaded: Rc<dyn Fn()> = Rc::new(on_project_reloaded.clone());
        let export_pop_weak = export_pop.downgrade();
        btn_manage_snapshots.connect_clicked(move |btn| {
            if let Some(pop) = export_pop_weak.upgrade() {
                pop.popdown();
            }
            let window = btn.root().and_then(|r| r.downcast::<gtk::Window>().ok());
            show_project_snapshots_dialog(
                window,
                project.clone(),
                library.clone(),
                timeline_state.clone(),
                on_project_changed.clone(),
                on_project_reloaded.clone(),
            );
        });
    }
    let btn_restore_backup = gtk::Button::with_label("Restore from Backup…");
    btn_restore_backup.add_css_class("flat");
    {
        let project = project.clone();
        let library = library.clone();
        let timeline_state = timeline_state.clone();
        let on_project_changed = on_project_changed.clone();
        let on_project_reloaded = on_project_reloaded.clone();
        let export_pop_weak = export_pop.downgrade();
        btn_restore_backup.connect_clicked(move |btn| {
            if let Some(pop) = export_pop_weak.upgrade() {
                pop.popdown();
            }
            let window = btn.root().and_then(|r| r.downcast::<gtk::Window>().ok());
            let on_project_changed_guard: Rc<dyn Fn()> = Rc::new(on_project_changed.clone());
            let action: Rc<dyn Fn()> = Rc::new({
                let project = project.clone();
                let timeline_state = timeline_state.clone();
                let on_project_changed: Rc<dyn Fn()> = Rc::new(on_project_changed.clone());
                let on_project_reloaded: Rc<dyn Fn()> = Rc::new(on_project_reloaded.clone());
                let window = window.clone();
                move || {
                    let dialog = gtk::FileDialog::new();
                    dialog.set_title("Restore from Backup");
                    if let Some(dir) = crate::project_versions::backup_dir() {
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
                                let preserved_file_path = project.borrow().file_path.clone();
                                restore_project_version_async(
                                    project.clone(),
                                    timeline_state.clone(),
                                    on_project_changed.clone(),
                                    on_project_reloaded.clone(),
                                    preserved_file_path,
                                    move || crate::project_versions::load_fcpxml_project(&path),
                                );
                            }
                        }
                    });
                }
            });
            confirm_unsaved_then(
                window,
                project.clone(),
                library.clone(),
                on_project_changed_guard,
                action,
            );
        });
    }
    let btn_export_edl = gtk::Button::with_label("Export EDL…");
    btn_export_edl.add_css_class("flat");
    {
        let project = project.clone();
        let export_pop_weak = export_pop.downgrade();
        btn_export_edl.connect_clicked(move |btn| {
            if let Some(pop) = export_pop_weak.upgrade() {
                pop.popdown();
            }

            let dialog = gtk::FileDialog::new();
            dialog.set_title("Export EDL");
            dialog.set_initial_name(Some("timeline.edl"));

            let filter = gtk::FileFilter::new();
            filter.add_pattern("*.edl");
            filter.set_name(Some("CMX 3600 EDL Files"));
            let filters = gio::ListStore::new::<gtk::FileFilter>();
            filters.append(&filter);
            dialog.set_filters(Some(&filters));

            let project = project.clone();
            let window = btn.root().and_then(|r| r.downcast::<gtk::Window>().ok());
            dialog.save(window.as_ref(), gio::Cancellable::NONE, move |result| {
                if let Ok(file) = result {
                    if let Some(path) = file.path() {
                        let edl_content = crate::edl::writer::write_edl(&project.borrow());
                        match std::fs::write(&path, edl_content) {
                            Ok(_) => {
                                log::info!("EDL exported to {}", path.display());
                            }
                            Err(e) => {
                                log::error!("Failed to export EDL: {e}");
                            }
                        }
                    }
                }
            });
        });
    }

    // -- Export OTIO button --
    let btn_export_otio = gtk::Button::with_label("Export OTIO…");
    btn_export_otio.add_css_class("flat");
    {
        let project = project.clone();
        let export_pop_weak = export_pop.downgrade();
        btn_export_otio.connect_clicked(move |btn| {
            if let Some(pop) = export_pop_weak.upgrade() {
                pop.popdown();
            }

            let project = project.clone();
            let window = btn.root().and_then(|r| r.downcast::<gtk::Window>().ok());
            let mode_dialog = gtk::Dialog::builder()
                .title("Export OTIO")
                .modal(true)
                .default_width(460)
                .build();
            mode_dialog.set_transient_for(window.as_ref());
            mode_dialog.add_button("Cancel", gtk::ResponseType::Cancel);
            mode_dialog.add_button("Continue…", gtk::ResponseType::Accept);

            let content = gtk::Box::new(gtk::Orientation::Vertical, 12);
            content.set_margin_start(16);
            content.set_margin_end(16);
            content.set_margin_top(16);
            content.set_margin_bottom(16);

            let description = gtk::Label::new(Some(
                "Choose how UltimateSlice should write media references inside the exported OTIO file.",
            ));
            description.set_wrap(true);
            description.set_xalign(0.0);

            let absolute_paths = gtk::CheckButton::with_label(
                crate::otio::writer::OtioMediaPathMode::Absolute.label(),
            );
            absolute_paths.set_active(true);
            let relative_paths = gtk::CheckButton::with_label(
                crate::otio::writer::OtioMediaPathMode::Relative.label(),
            );
            relative_paths.set_group(Some(&absolute_paths));

            let hint = gtk::Label::new(Some(
                "Absolute paths keep the current behavior. Relative paths are written relative to the exported .otio file location and are resolved from that folder when the OTIO file is opened again.",
            ));
            hint.set_wrap(true);
            hint.set_xalign(0.0);
            hint.add_css_class("dim-label");

            content.append(&description);
            content.append(&absolute_paths);
            content.append(&relative_paths);
            content.append(&hint);
            mode_dialog.content_area().append(&content);

            mode_dialog.connect_response(move |d, resp| {
                if resp != gtk::ResponseType::Accept {
                    d.close();
                    return;
                }
                let path_mode = if relative_paths.is_active() {
                    crate::otio::writer::OtioMediaPathMode::Relative
                } else {
                    crate::otio::writer::OtioMediaPathMode::Absolute
                };
                d.close();

                let dialog = gtk::FileDialog::new();
                dialog.set_title("Export OTIO");
                dialog.set_initial_name(Some("timeline.otio"));

                let filter = gtk::FileFilter::new();
                filter.add_pattern("*.otio");
                filter.set_name(Some("OpenTimelineIO Files"));
                let filters = gio::ListStore::new::<gtk::FileFilter>();
                filters.append(&filter);
                dialog.set_filters(Some(&filters));

                let project = project.clone();
                let window = window.clone();
                dialog.save(window.as_ref(), gio::Cancellable::NONE, move |result| {
                    if let Ok(file) = result {
                        if let Some(path) = file.path() {
                            match crate::otio::writer::write_otio_to_path(
                                &project.borrow(),
                                &path,
                                path_mode,
                            ) {
                                Ok(json) => match std::fs::write(&path, json) {
                                    Ok(_) => {
                                        log::info!(
                                            "OTIO exported to {} with {} media references",
                                            path.display(),
                                            path_mode.as_str()
                                        );
                                    }
                                    Err(e) => {
                                        log::error!("Failed to write OTIO file: {e}");
                                    }
                                },
                                Err(e) => {
                                    log::error!("Failed to generate OTIO: {e}");
                                }
                            }
                        }
                    }
                });
            });
            mode_dialog.present();
        });
    }

    export_pop_box.append(&btn_export_project_with_media);
    export_pop_box.append(&btn_collect_files);
    export_pop_box.append(&btn_export_frame);
    export_pop_box.append(&btn_create_snapshot);
    export_pop_box.append(&btn_manage_snapshots);
    export_pop_box.append(&btn_export_edl);
    export_pop_box.append(&btn_export_otio);
    export_pop_box.append(&btn_restore_backup);

    // Export Queue dialog entry
    let btn_export_queue = gtk::Button::with_label("Export Queue…");
    btn_export_queue.add_css_class("flat");
    btn_export_queue.set_tooltip_text(Some("View and run the batch export queue"));
    {
        let export_pop_weak = export_pop.downgrade();
        let project = project.clone();
        let bg_removal_cache = bg_removal_cache.clone();
        let frame_interp_cache = frame_interp_cache.clone();
        btn_export_queue.connect_clicked(move |btn| {
            if let Some(pop) = export_pop_weak.upgrade() {
                pop.popdown();
            }
            let window = btn.root().and_then(|r| r.downcast::<gtk::Window>().ok());
            let dialog = crate::ui::export_queue::build_export_queue_dialog(
                project.clone(),
                bg_removal_cache.clone(),
                frame_interp_cache.clone(),
                window.as_ref(),
            );
            dialog.present();
        });
    }
    export_pop_box.append(&btn_export_queue);

    let btn_project_health = gtk::Button::with_label("Project Health…");
    btn_project_health.add_css_class("flat");
    btn_project_health.set_tooltip_text(Some("Inspect offline media and managed cache usage"));
    {
        let export_pop_weak = export_pop.downgrade();
        let on_show_project_health = on_show_project_health.clone();
        btn_project_health.connect_clicked(move |btn| {
            if let Some(pop) = export_pop_weak.upgrade() {
                pop.popdown();
            }
            let window = btn.root().and_then(|r| r.downcast::<gtk::Window>().ok());
            on_show_project_health(window);
        });
    }
    export_pop_box.append(&btn_project_health);

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

    // ── Record Voiceover ────────────────────────────────────────────────────
    let btn_record = Button::with_label("Record");
    btn_record.set_tooltip_text(Some(
        "Record voiceover from microphone at playhead position",
    ));
    btn_record.add_css_class("small-btn");
    {
        let on_record_voiceover = on_record_voiceover.clone();
        btn_record.connect_clicked(move |_| on_record_voiceover());
    }
    header.pack_end(&btn_record);

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
            if timeline_state.borrow_mut().undo() {
                on_project_changed();
            }
        });
    }
    header.pack_start(&btn_undo);

    let btn_redo = Button::with_label("↪ Redo");
    btn_redo.set_tooltip_text(Some("Redo (Ctrl+Shift+Z)"));
    {
        let timeline_state = timeline_state.clone();
        let on_project_changed = on_project_changed.clone();
        btn_redo.connect_clicked(move |_| {
            if timeline_state.borrow_mut().redo() {
                on_project_changed();
            }
        });
    }
    header.pack_start(&btn_redo);

    let sep_tools = Separator::new(gtk::Orientation::Vertical);
    sep_tools.add_css_class("toolbar-separator");
    header.pack_start(&sep_tools);

    // ── Tool selector ───────────────────────────────────────────────────
    let current_tool = timeline_state.borrow().active_tool;
    let initial_primary_tool = if is_primary_toolbar_tool(current_tool) {
        current_tool
    } else {
        ActiveTool::Select
    };
    let last_primary_tool = Rc::new(Cell::new(initial_primary_tool));
    let btn_tool_menu = ToggleButton::new();
    btn_tool_menu.set_label("Tool Menu");
    btn_tool_menu.set_tooltip_text(Some("Choose Select, Razor, Ripple, Roll, Slip, or Slide"));
    btn_tool_menu.set_active(is_primary_toolbar_tool(current_tool));
    let tool_menu_row = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    let tool_menu_label = gtk::Label::new(Some(toolbar_tool_label(initial_primary_tool)));
    tool_menu_label.set_width_chars(9);
    tool_menu_label.set_xalign(0.0);
    let tool_menu_arrow = gtk::Image::from_icon_name("pan-down-symbolic");
    tool_menu_row.append(&tool_menu_label);
    tool_menu_row.append(&tool_menu_arrow);
    btn_tool_menu.set_child(Some(&tool_menu_row));

    let tool_pop = gtk::Popover::new();
    tool_pop.set_autohide(true);
    tool_pop.set_cascade_popdown(true);
    let tool_pop_box = gtk::Box::new(gtk::Orientation::Vertical, 2);
    tool_pop_box.set_margin_start(4);
    tool_pop_box.set_margin_end(4);
    tool_pop_box.set_margin_top(4);
    tool_pop_box.set_margin_bottom(4);
    let build_tool_button = |tool: ActiveTool| {
        let btn = Button::with_label(toolbar_tool_label(tool));
        btn.add_css_class("flat");
        btn.set_tooltip_text(Some(toolbar_tool_tooltip(tool)));
        btn.set_halign(gtk::Align::Fill);
        btn.set_hexpand(true);
        btn
    };
    let btn_select = build_tool_button(ActiveTool::Select);
    let btn_razor = build_tool_button(ActiveTool::Razor);
    let btn_ripple = build_tool_button(ActiveTool::Ripple);
    let btn_roll = build_tool_button(ActiveTool::Roll);
    let btn_slip = build_tool_button(ActiveTool::Slip);
    let btn_slide = build_tool_button(ActiveTool::Slide);
    tool_pop_box.append(&btn_select);
    tool_pop_box.append(&btn_razor);
    tool_pop_box.append(&btn_ripple);
    tool_pop_box.append(&btn_roll);
    tool_pop_box.append(&btn_slip);
    tool_pop_box.append(&btn_slide);
    tool_pop.set_child(Some(&tool_pop_box));
    tool_pop.set_parent(&btn_tool_menu);
    {
        let tool_pop = tool_pop.clone();
        let timeline_state = timeline_state.clone();
        btn_tool_menu.connect_clicked(move |btn| {
            if tool_pop.is_visible() {
                tool_pop.popdown();
            } else {
                tool_pop.popup();
            }
            let should_be_active = is_primary_toolbar_tool(timeline_state.borrow().active_tool);
            if btn.is_active() != should_be_active {
                btn.set_active(should_be_active);
            }
        });
    }

    let btn_draw = ToggleButton::with_label("✎ Draw");
    btn_draw.set_tooltip_text(Some(
        "Vector drawing tool (D)\n\
         While active, on the program monitor:\n\
         • 1 / 2 / 3 / 4 — pick Stroke / Rectangle / Ellipse / Arrow\n\
         • drag to draw on the playhead's drawing clip (creates one if needed)\n\
         • Delete / Backspace — remove the most recent item",
    ));
    btn_draw.set_active(current_tool == ActiveTool::Draw);
    let btn_draw_tools = Button::new();
    let draw_tools_icon = gtk::Image::from_icon_name("applications-graphics-symbolic");
    btn_draw_tools.set_child(Some(&draw_tools_icon));
    btn_draw_tools.set_tooltip_text(Some("Draw tool options"));
    btn_draw_tools.add_css_class("toolbar-split-toggle");
    btn_draw_tools.set_valign(gtk::Align::Center);
    let draw_group = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    draw_group.add_css_class("linked");
    draw_group.set_valign(gtk::Align::Center);
    draw_group.append(&btn_draw);
    draw_group.append(&btn_draw_tools);

    // Set the active tool and fire any installed `on_tool_changed`
    // listener (the TransformOverlay's draw-mode subscriber lives
    // there). Idempotent: calling with the already-active tool is a
    // no-op for listeners via GTK's set_active short-circuit.
    fn apply_tool(timeline_state: &Rc<RefCell<TimelineState>>, tool: ActiveTool) {
        let cb = {
            let mut st = timeline_state.borrow_mut();
            if st.active_tool == tool {
                return;
            }
            st.active_tool = tool;
            st.on_tool_changed.clone()
        };
        if let Some(cb) = cb {
            cb(tool);
        }
    }
    {
        let timeline_state = timeline_state.clone();
        let tool_pop = tool_pop.downgrade();
        btn_select.connect_clicked(move |_| {
            if let Some(tool_pop) = tool_pop.upgrade() {
                tool_pop.popdown();
            }
            apply_tool(&timeline_state, ActiveTool::Select);
        });
    }
    {
        let timeline_state = timeline_state.clone();
        let tool_pop = tool_pop.downgrade();
        btn_razor.connect_clicked(move |_| {
            if let Some(tool_pop) = tool_pop.upgrade() {
                tool_pop.popdown();
            }
            apply_tool(&timeline_state, ActiveTool::Razor);
        });
    }
    {
        let timeline_state = timeline_state.clone();
        let tool_pop = tool_pop.downgrade();
        btn_ripple.connect_clicked(move |_| {
            if let Some(tool_pop) = tool_pop.upgrade() {
                tool_pop.popdown();
            }
            apply_tool(&timeline_state, ActiveTool::Ripple);
        });
    }
    {
        let timeline_state = timeline_state.clone();
        let tool_pop = tool_pop.downgrade();
        btn_roll.connect_clicked(move |_| {
            if let Some(tool_pop) = tool_pop.upgrade() {
                tool_pop.popdown();
            }
            apply_tool(&timeline_state, ActiveTool::Roll);
        });
    }
    {
        let timeline_state = timeline_state.clone();
        let tool_pop = tool_pop.downgrade();
        btn_slip.connect_clicked(move |_| {
            if let Some(tool_pop) = tool_pop.upgrade() {
                tool_pop.popdown();
            }
            apply_tool(&timeline_state, ActiveTool::Slip);
        });
    }
    {
        let timeline_state = timeline_state.clone();
        let tool_pop = tool_pop.downgrade();
        btn_slide.connect_clicked(move |_| {
            if let Some(tool_pop) = tool_pop.upgrade() {
                tool_pop.popdown();
            }
            apply_tool(&timeline_state, ActiveTool::Slide);
        });
    }
    {
        let timeline_state = timeline_state.clone();
        btn_draw.connect_toggled(move |btn| {
            if btn.is_active() {
                apply_tool(&timeline_state, ActiveTool::Draw);
            } else if timeline_state.borrow().active_tool == ActiveTool::Draw {
                btn.set_active(true);
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

    header.pack_start(&btn_tool_menu);
    header.pack_start(&draw_group);
    header.pack_start(&btn_magnetic);

    let sync_tool_menu_ui: Rc<dyn Fn(ActiveTool)> = Rc::new({
        let btn_tool_menu = btn_tool_menu.clone();
        let tool_menu_label = tool_menu_label.clone();
        let btn_select = btn_select.clone();
        let btn_razor = btn_razor.clone();
        let btn_ripple = btn_ripple.clone();
        let btn_roll = btn_roll.clone();
        let btn_slip = btn_slip.clone();
        let btn_slide = btn_slide.clone();
        let last_primary_tool = last_primary_tool.clone();
        move |tool: ActiveTool| {
            if is_primary_toolbar_tool(tool) {
                last_primary_tool.set(tool);
            }
            let shown_tool = last_primary_tool.get();
            tool_menu_label.set_text(toolbar_tool_label(shown_tool));
            let active_primary = is_primary_toolbar_tool(tool).then_some(tool);
            btn_tool_menu.set_active(active_primary.is_some());
            btn_select.set_sensitive(active_primary != Some(ActiveTool::Select));
            btn_razor.set_sensitive(active_primary != Some(ActiveTool::Razor));
            btn_ripple.set_sensitive(active_primary != Some(ActiveTool::Ripple));
            btn_roll.set_sensitive(active_primary != Some(ActiveTool::Roll));
            btn_slip.set_sensitive(active_primary != Some(ActiveTool::Slip));
            btn_slide.set_sensitive(active_primary != Some(ActiveTool::Slide));
        }
    });
    sync_tool_menu_ui(current_tool);

    // Wire on_tool_changed so keyboard shortcuts sync toolbar controls
    {
        let btn_draw = btn_draw.clone();
        let sync_tool_menu_ui = sync_tool_menu_ui.clone();
        timeline_state.borrow_mut().on_tool_changed = Some(Rc::new(move |tool: ActiveTool| {
            sync_tool_menu_ui(tool);
            btn_draw.set_active(tool == ActiveTool::Draw);
        }));
    }

    (header, btn_record, btn_draw_tools)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn primary_toolbar_tools_exclude_draw() {
        assert!(is_primary_toolbar_tool(ActiveTool::Select));
        assert!(is_primary_toolbar_tool(ActiveTool::Slide));
        assert!(!is_primary_toolbar_tool(ActiveTool::Draw));
    }

    #[test]
    fn toolbar_tool_metadata_matches_expected_labels() {
        assert_eq!(toolbar_tool_label(ActiveTool::Razor), "✂ Razor");
        assert_eq!(toolbar_tool_tooltip(ActiveTool::Slip), "Slip edit tool (Y)");
    }
}
