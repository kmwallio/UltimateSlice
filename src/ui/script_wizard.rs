// SPDX-License-Identifier: GPL-3.0-or-later
//! 5-step Script-to-Timeline wizard dialog.
//!
//! Steps:
//! 1. Load screenplay (FDX or Fountain)
//! 2. Select media clips
//! 3. Background STT + alignment (with progress)
//! 4. Review mappings (reassign, confidence badges)
//! 5. Generate timeline

use std::cell::RefCell;
use std::rc::Rc;

use glib;
use gtk4::prelude::*;
use gtk4::{self as gtk};

use crate::media::script::{self, Script};
use crate::media::script_align::{self, AlignmentResult, SceneMapping};
use crate::media::script_assembly::{self, AssemblyPlan};
use crate::media::stt_cache::SttCache;
use crate::model::clip::SubtitleSegment;
use crate::model::media_library::MediaLibrary;
use crate::model::project::Project;
use crate::ui::timeline::TimelineState;
use crate::undo::{EditCommand, ScriptAssemblyCommand};

/// Launch the Script-to-Timeline wizard.
pub fn show_script_wizard(
    parent: Option<&gtk::Window>,
    project: Rc<RefCell<Project>>,
    library: Rc<RefCell<MediaLibrary>>,
    stt_cache: Rc<RefCell<SttCache>>,
    timeline_state: Rc<RefCell<TimelineState>>,
    on_project_changed: Rc<dyn Fn()>,
) {
    let dialog = gtk::Window::builder()
        .title("Script to Timeline")
        .default_width(700)
        .default_height(520)
        .modal(true)
        .build();
    if let Some(p) = parent {
        dialog.set_transient_for(Some(p));
    }

    let main_box = gtk::Box::new(gtk::Orientation::Vertical, 0);
    dialog.set_child(Some(&main_box));

    // Stack for wizard pages.
    let stack = gtk::Stack::new();
    stack.set_transition_type(gtk::StackTransitionType::SlideLeftRight);
    stack.set_vexpand(true);
    main_box.append(&stack);

    // ── Shared wizard state ─────────────────────────────────────────────
    let parsed_script: Rc<RefCell<Option<Script>>> = Rc::new(RefCell::new(None));
    let selected_paths: Rc<RefCell<Vec<String>>> = Rc::new(RefCell::new(Vec::new()));
    let alignment_result: Rc<RefCell<Option<AlignmentResult>>> = Rc::new(RefCell::new(None));
    let collected_transcripts: Rc<RefCell<Vec<(String, Vec<SubtitleSegment>)>>> =
        Rc::new(RefCell::new(Vec::new()));
    let include_titles: Rc<RefCell<bool>> = Rc::new(RefCell::new(true));
    // User overrides from review step: vec of (clip_source_path, scene_id).
    let user_overrides: Rc<RefCell<Vec<(String, String)>>> = Rc::new(RefCell::new(Vec::new()));

    // ── Step 1: Load Script ─────────────────────────────────────────────
    let step1 = build_step1(&parsed_script);
    stack.add_named(&step1, Some("step1"));

    // ── Step 2: Select Clips ────────────────────────────────────────────
    let step2 = build_step2(&selected_paths);
    stack.add_named(&step2, Some("step2"));

    // ── Step 3: STT + Alignment ─────────────────────────────────────────
    let step3_box = gtk::Box::new(gtk::Orientation::Vertical, 12);
    step3_box.set_margin_start(16);
    step3_box.set_margin_end(16);
    step3_box.set_margin_top(16);
    step3_box.set_margin_bottom(16);
    let step3_label = gtk::Label::new(Some("Transcribing and aligning clips..."));
    step3_label.set_halign(gtk::Align::Start);
    step3_box.append(&step3_label);
    let step3_progress = gtk::ProgressBar::new();
    step3_progress.set_show_text(true);
    step3_box.append(&step3_progress);
    let step3_detail = gtk::Label::new(Some(""));
    step3_detail.set_halign(gtk::Align::Start);
    step3_detail.add_css_class("dim-label");
    step3_box.append(&step3_detail);
    stack.add_named(&step3_box, Some("step3"));

    // ── Step 4: Review Mapping ──────────────────────────────────────────
    let step4_box = gtk::Box::new(gtk::Orientation::Vertical, 8);
    step4_box.set_margin_start(16);
    step4_box.set_margin_end(16);
    step4_box.set_margin_top(16);
    step4_box.set_margin_bottom(16);
    let step4_header = gtk::Label::new(Some("Review Scene Mappings"));
    step4_header.set_halign(gtk::Align::Start);
    step4_header.add_css_class("heading");
    step4_box.append(&step4_header);

    let titles_check = gtk::CheckButton::with_label("Add scene heading title cards");
    titles_check.set_active(true);
    {
        let include_titles = include_titles.clone();
        titles_check.connect_toggled(move |cb| {
            *include_titles.borrow_mut() = cb.is_active();
        });
    }
    step4_box.append(&titles_check);

    let step4_scroll = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .vscrollbar_policy(gtk::PolicyType::Automatic)
        .vexpand(true)
        .build();
    let step4_list = gtk::ListBox::new();
    step4_list.set_selection_mode(gtk::SelectionMode::None);
    step4_scroll.set_child(Some(&step4_list));
    step4_box.append(&step4_scroll);
    stack.add_named(&step4_box, Some("step4"));

    // ── Step 5: Generate ────────────────────────────────────────────────
    let step5_box = gtk::Box::new(gtk::Orientation::Vertical, 12);
    step5_box.set_margin_start(16);
    step5_box.set_margin_end(16);
    step5_box.set_margin_top(16);
    step5_box.set_margin_bottom(16);
    let step5_summary = gtk::Label::new(Some("Ready to generate timeline."));
    step5_summary.set_halign(gtk::Align::Start);
    step5_box.append(&step5_summary);
    stack.add_named(&step5_box, Some("step5"));

    // ── Button bar ──────────────────────────────────────────────────────
    let btn_bar = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    btn_bar.set_margin_start(16);
    btn_bar.set_margin_end(16);
    btn_bar.set_margin_top(8);
    btn_bar.set_margin_bottom(16);
    btn_bar.set_halign(gtk::Align::End);

    let btn_cancel = gtk::Button::with_label("Cancel");
    let btn_back = gtk::Button::with_label("Back");
    let btn_next = gtk::Button::with_label("Next");
    btn_next.add_css_class("suggested-action");

    btn_bar.append(&btn_cancel);
    btn_bar.append(&btn_back);
    btn_bar.append(&btn_next);
    main_box.append(&btn_bar);

    btn_back.set_visible(false);

    let current_step: Rc<RefCell<u32>> = Rc::new(RefCell::new(1));

    // ── Cancel ──────────────────────────────────────────────────────────
    {
        let dialog = dialog.clone();
        btn_cancel.connect_clicked(move |_| dialog.close());
    }

    // ── Back ────────────────────────────────────────────────────────────
    {
        let stack = stack.clone();
        let current_step = current_step.clone();
        let btn_back_inner = btn_back.clone();
        let btn_next_inner = btn_next.clone();
        btn_back.connect_clicked(move |_| {
            let mut step = current_step.borrow_mut();
            if *step > 1 {
                *step -= 1;
                stack.set_visible_child_name(&format!("step{}", *step));
                if *step == 1 {
                    btn_back_inner.set_visible(false);
                }
                btn_next_inner.set_label("Next");
                btn_next_inner.set_sensitive(true);
            }
        });
    }

    // ── Next / Generate ─────────────────────────────────────────────────
    {
        let stack = stack.clone();
        let current_step = current_step.clone();
        let btn_back = btn_back.clone();
        let btn_next_ref = btn_next.clone();
        let dialog = dialog.clone();
        let parsed_script = parsed_script.clone();
        let selected_paths = selected_paths.clone();
        let alignment_result = alignment_result.clone();
        let include_titles = include_titles.clone();
        let stt_cache = stt_cache.clone();
        let project = project.clone();
        let library = library.clone();
        let timeline_state = timeline_state.clone();
        let on_project_changed = on_project_changed.clone();
        let collected_transcripts = collected_transcripts.clone();
        let step3_progress = step3_progress.clone();
        let step3_detail = step3_detail.clone();
        let step3_label = step3_label.clone();
        let step4_list = step4_list.clone();
        let step5_summary = step5_summary.clone();

        btn_next.connect_clicked(move |_| {
            let step = *current_step.borrow();
            match step {
                1 => {
                    // Validate: script must be loaded.
                    if parsed_script.borrow().is_none() {
                        return;
                    }
                    *current_step.borrow_mut() = 2;
                    stack.set_visible_child_name("step2");
                    btn_back.set_visible(true);
                }
                2 => {
                    // Validate: at least one clip selected.
                    if selected_paths.borrow().is_empty() {
                        return;
                    }
                    *current_step.borrow_mut() = 3;
                    stack.set_visible_child_name("step3");
                    btn_next_ref.set_sensitive(false);

                    // Start STT + alignment.
                    start_stt_and_align(
                        stt_cache.clone(),
                        parsed_script.clone(),
                        selected_paths.clone(),
                        alignment_result.clone(),
                        collected_transcripts.clone(),
                        step3_progress.clone(),
                        step3_detail.clone(),
                        step3_label.clone(),
                        btn_next_ref.clone(),
                    );
                }
                3 => {
                    // Populate review list.
                    populate_review_list(
                        &step4_list,
                        &alignment_result.borrow(),
                        &parsed_script.borrow(),
                    );
                    *current_step.borrow_mut() = 4;
                    stack.set_visible_child_name("step4");
                    btn_next_ref.set_label("Generate");
                }
                4 => {
                    // Show summary, then generate.
                    let ar = alignment_result.borrow();
                    if let Some(ref result) = *ar {
                        let n_mapped = result.mappings.len();
                        let n_unmatched = result.unmatched_clips.len();
                        step5_summary.set_text(&format!(
                            "{n_mapped} clips mapped to scenes, {n_unmatched} unmatched."
                        ));
                    }
                    *current_step.borrow_mut() = 5;
                    stack.set_visible_child_name("step5");
                    btn_next_ref.set_label("Generate Timeline");
                }
                5 => {
                    // Execute assembly.
                    execute_assembly(
                        project.clone(),
                        library.clone(),
                        timeline_state.clone(),
                        parsed_script.clone(),
                        alignment_result.clone(),
                        collected_transcripts.clone(),
                        *include_titles.borrow(),
                        on_project_changed.clone(),
                    );
                    dialog.close();
                }
                _ => {}
            }
        });
    }

    stack.set_visible_child_name("step1");
    dialog.present();
}

// ── Step 1: Load Script ─────────────────────────────────────────────────

fn build_step1(parsed_script: &Rc<RefCell<Option<Script>>>) -> gtk::Box {
    let vbox = gtk::Box::new(gtk::Orientation::Vertical, 12);
    vbox.set_margin_start(16);
    vbox.set_margin_end(16);
    vbox.set_margin_top(16);
    vbox.set_margin_bottom(16);

    let header = gtk::Label::new(Some("Step 1: Load Screenplay"));
    header.set_halign(gtk::Align::Start);
    header.add_css_class("heading");
    vbox.append(&header);

    let desc = gtk::Label::new(Some(
        "Select a Final Draft (.fdx) or Fountain (.fountain) screenplay file.",
    ));
    desc.set_halign(gtk::Align::Start);
    desc.set_wrap(true);
    desc.add_css_class("dim-label");
    vbox.append(&desc);

    let file_box = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    let file_label = gtk::Label::new(Some("No file selected"));
    file_label.set_halign(gtk::Align::Start);
    file_label.set_hexpand(true);
    file_label.set_ellipsize(pango::EllipsizeMode::Middle);

    let btn_choose = gtk::Button::with_label("Choose File...");
    file_box.append(&file_label);
    file_box.append(&btn_choose);
    vbox.append(&file_box);

    let info_label = gtk::Label::new(Some(""));
    info_label.set_halign(gtk::Align::Start);
    info_label.set_wrap(true);
    vbox.append(&info_label);

    {
        let parsed_script = parsed_script.clone();
        let file_label = file_label.clone();
        let info_label = info_label.clone();
        btn_choose.connect_clicked(move |btn| {
            let dialog = gtk::FileDialog::new();
            dialog.set_title("Open Screenplay");

            let filter = gtk::FileFilter::new();
            filter.add_pattern("*.fdx");
            filter.add_pattern("*.fountain");
            filter.add_pattern("*.spmd");
            filter.set_name(Some("Screenplay Files (*.fdx, *.fountain)"));
            let filters = gio::ListStore::new::<gtk::FileFilter>();
            filters.append(&filter);
            dialog.set_filters(Some(&filters));

            let window = btn.root().and_then(|r| r.downcast::<gtk::Window>().ok());
            let parsed_script = parsed_script.clone();
            let file_label = file_label.clone();
            let info_label = info_label.clone();
            dialog.open(window.as_ref(), gio::Cancellable::NONE, move |result| {
                if let Ok(file) = result {
                    if let Some(path) = file.path() {
                        let path_str = path.to_string_lossy().to_string();
                        match script::parse_script(&path_str) {
                            Ok(s) => {
                                let scene_count = s.scenes.len();
                                let title_str = s.title.as_deref().unwrap_or("(untitled)");
                                file_label.set_text(&path_str);
                                info_label.set_text(&format!(
                                    "Title: {title_str} | {scene_count} scenes"
                                ));
                                *parsed_script.borrow_mut() = Some(s);
                            }
                            Err(e) => {
                                file_label.set_text("Error loading file");
                                info_label.set_text(&format!("Parse error: {e}"));
                                *parsed_script.borrow_mut() = None;
                            }
                        }
                    }
                }
            });
        });
    }

    vbox
}

// ── Step 2: Select Clips ────────────────────────────────────────────────

fn build_step2(selected_paths: &Rc<RefCell<Vec<String>>>) -> gtk::Box {
    let vbox = gtk::Box::new(gtk::Orientation::Vertical, 12);
    vbox.set_margin_start(16);
    vbox.set_margin_end(16);
    vbox.set_margin_top(16);
    vbox.set_margin_bottom(16);

    let header = gtk::Label::new(Some("Step 2: Select Media Clips"));
    header.set_halign(gtk::Align::Start);
    header.add_css_class("heading");
    vbox.append(&header);

    let desc = gtk::Label::new(Some(
        "Choose the media files that correspond to scenes in your screenplay.",
    ));
    desc.set_halign(gtk::Align::Start);
    desc.set_wrap(true);
    desc.add_css_class("dim-label");
    vbox.append(&desc);

    let scroll = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .vscrollbar_policy(gtk::PolicyType::Automatic)
        .vexpand(true)
        .build();
    let list_box = gtk::ListBox::new();
    list_box.set_selection_mode(gtk::SelectionMode::None);
    scroll.set_child(Some(&list_box));
    vbox.append(&scroll);

    let count_label = gtk::Label::new(Some("0 clips selected"));
    count_label.set_halign(gtk::Align::Start);
    count_label.add_css_class("dim-label");
    vbox.append(&count_label);

    let btn_add = gtk::Button::with_label("Add Files...");
    vbox.append(&btn_add);

    {
        let selected_paths = selected_paths.clone();
        let list_box = list_box.clone();
        let count_label = count_label.clone();
        btn_add.connect_clicked(move |btn| {
            let dialog = gtk::FileDialog::new();
            dialog.set_title("Select Media Clips");

            let filter = gtk::FileFilter::new();
            filter.add_mime_type("video/*");
            filter.add_mime_type("audio/*");
            filter.set_name(Some("Media Files"));
            let filters = gio::ListStore::new::<gtk::FileFilter>();
            filters.append(&filter);
            dialog.set_filters(Some(&filters));

            let window = btn.root().and_then(|r| r.downcast::<gtk::Window>().ok());
            let selected_paths = selected_paths.clone();
            let list_box = list_box.clone();
            let count_label = count_label.clone();
            dialog.open_multiple(window.as_ref(), gio::Cancellable::NONE, move |result| {
                if let Ok(files) = result {
                    let mut paths = selected_paths.borrow_mut();
                    for i in 0..files.n_items() {
                        if let Some(file) = files.item(i) {
                            if let Some(f) = file.downcast_ref::<gio::File>() {
                                if let Some(p) = f.path() {
                                    let ps = p.to_string_lossy().to_string();
                                    if !paths.contains(&ps) {
                                        paths.push(ps.clone());
                                        let row = gtk::Label::new(Some(&ps));
                                        row.set_halign(gtk::Align::Start);
                                        row.set_ellipsize(pango::EllipsizeMode::Middle);
                                        list_box.append(&row);
                                    }
                                }
                            }
                        }
                    }
                    count_label.set_text(&format!("{} clips selected", paths.len()));
                }
            });
        });
    }

    vbox
}

// ── Step 3: Background STT + Alignment ──────────────────────────────────

fn start_stt_and_align(
    stt_cache: Rc<RefCell<SttCache>>,
    parsed_script: Rc<RefCell<Option<Script>>>,
    selected_paths: Rc<RefCell<Vec<String>>>,
    alignment_result: Rc<RefCell<Option<AlignmentResult>>>,
    transcripts: Rc<RefCell<Vec<(String, Vec<SubtitleSegment>)>>>,
    progress_bar: gtk::ProgressBar,
    detail_label: gtk::Label,
    phase_label: gtk::Label,
    btn_next: gtk::Button,
) {
    // Queue STT requests for all selected clips.
    let paths = selected_paths.borrow().clone();
    {
        let mut cache = stt_cache.borrow_mut();
        for path in &paths {
            // Use full file duration (source_in=0, source_out=u64::MAX as sentinel).
            cache.request(path, 0, u64::MAX, "auto");
        }
    }

    transcripts.borrow_mut().clear();
    let total = paths.len();

    // Poll STT progress.
    let stt_cache_poll = stt_cache.clone();
    let transcripts_poll = transcripts.clone();
    let alignment_result_poll = alignment_result.clone();
    let parsed_script_poll = parsed_script.clone();
    let progress_bar_poll = progress_bar.clone();
    let detail_label_poll = detail_label.clone();
    let phase_label_poll = phase_label.clone();
    let btn_next_poll = btn_next.clone();

    glib::timeout_add_local(std::time::Duration::from_millis(200), move || {
        // Poll completed STT results.
        let results = {
            let mut cache = stt_cache_poll.borrow_mut();
            cache.poll()
        };
        for result in results {
            transcripts_poll
                .borrow_mut()
                .push((result.source_path.clone(), result.segments));
        }

        let completed = transcripts_poll.borrow().len();
        let fraction = if total > 0 {
            completed as f64 / total as f64
        } else {
            1.0
        };

        if completed < total {
            // Still transcribing.
            phase_label_poll.set_text("Transcribing clips...");
            progress_bar_poll.set_fraction(fraction * 0.8); // Reserve 20% for alignment.
            detail_label_poll.set_text(&format!("{completed}/{total} clips transcribed"));
            glib::ControlFlow::Continue
        } else {
            // All transcripts done — run alignment.
            phase_label_poll.set_text("Aligning transcripts to script...");
            progress_bar_poll.set_fraction(0.9);

            let script_opt = parsed_script_poll.borrow();
            if let Some(ref scr) = *script_opt {
                let transcripts_vec = transcripts_poll.borrow().clone();
                let result = script_align::align_transcripts_to_script(scr, &transcripts_vec, 0.15);
                let n_mapped = result.mappings.len();
                let n_unmatched = result.unmatched_clips.len();
                *alignment_result_poll.borrow_mut() = Some(result);

                progress_bar_poll.set_fraction(1.0);
                phase_label_poll.set_text("Alignment complete.");
                detail_label_poll.set_text(&format!(
                    "{n_mapped} clips matched, {n_unmatched} unmatched"
                ));
            }

            btn_next_poll.set_sensitive(true);
            glib::ControlFlow::Break
        }
    });
}

// ── Step 4: Populate review list ────────────────────────────────────────

fn populate_review_list(
    list_box: &gtk::ListBox,
    alignment: &Option<AlignmentResult>,
    script: &Option<Script>,
) {
    // Clear existing rows.
    while let Some(child) = list_box.first_child() {
        list_box.remove(&child);
    }

    let (alignment, script) = match (alignment, script) {
        (Some(a), Some(s)) => (a, s),
        _ => return,
    };

    // Build scene heading lookup.
    let scene_headings: std::collections::HashMap<&str, &str> = script
        .scenes
        .iter()
        .map(|s| (s.id.as_str(), s.heading.as_str()))
        .collect();

    for mapping in &alignment.mappings {
        let row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        row.set_margin_start(4);
        row.set_margin_end(4);
        row.set_margin_top(4);
        row.set_margin_bottom(4);

        // Scene heading.
        let scene_heading = scene_headings
            .get(mapping.scene_id.as_str())
            .unwrap_or(&"Unknown");
        let scene_label = gtk::Label::new(Some(scene_heading));
        scene_label.set_halign(gtk::Align::Start);
        scene_label.set_hexpand(true);
        scene_label.set_ellipsize(pango::EllipsizeMode::End);

        // Arrow.
        let arrow = gtk::Label::new(Some("\u{2192}")); // →

        // Clip name.
        let clip_name = std::path::Path::new(&mapping.clip_source_path)
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or(&mapping.clip_source_path);
        let clip_label = gtk::Label::new(Some(clip_name));
        clip_label.set_halign(gtk::Align::End);
        clip_label.set_ellipsize(pango::EllipsizeMode::Middle);

        // Confidence badge.
        let conf = mapping.confidence;
        let badge_text = format!("{:.0}%", conf * 100.0);
        let badge = gtk::Label::new(Some(&badge_text));
        badge.set_width_chars(5);
        if conf >= 0.8 {
            badge.add_css_class("success"); // Green
        } else if conf >= 0.5 {
            badge.add_css_class("warning"); // Yellow
        } else {
            badge.add_css_class("error"); // Red
        }

        row.append(&scene_label);
        row.append(&arrow);
        row.append(&clip_label);
        row.append(&badge);
        list_box.append(&row);
    }

    // Unmatched clips section.
    if !alignment.unmatched_clips.is_empty() {
        let sep = gtk::Separator::new(gtk::Orientation::Horizontal);
        list_box.append(&sep);

        let unmatched_header = gtk::Label::new(Some("Unmatched Clips"));
        unmatched_header.set_halign(gtk::Align::Start);
        unmatched_header.add_css_class("heading");
        list_box.append(&unmatched_header);

        for path in &alignment.unmatched_clips {
            let name = std::path::Path::new(path)
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or(path);
            let label = gtk::Label::new(Some(name));
            label.set_halign(gtk::Align::Start);
            label.add_css_class("dim-label");
            list_box.append(&label);
        }
    }
}

// ── Step 5: Execute assembly ────────────────────────────────────────────

fn execute_assembly(
    project: Rc<RefCell<Project>>,
    library: Rc<RefCell<MediaLibrary>>,
    timeline_state: Rc<RefCell<TimelineState>>,
    parsed_script: Rc<RefCell<Option<Script>>>,
    alignment_result: Rc<RefCell<Option<AlignmentResult>>>,
    collected_transcripts: Rc<RefCell<Vec<(String, Vec<SubtitleSegment>)>>>,
    include_titles: bool,
    on_project_changed: Rc<dyn Fn()>,
) {
    let script_opt = parsed_script.borrow();
    let alignment_opt = alignment_result.borrow();

    let (script, alignment) = match (script_opt.as_ref(), alignment_opt.as_ref()) {
        (Some(s), Some(a)) => (s, a),
        _ => return,
    };

    let title_duration_ns = 3_000_000_000; // 3 seconds per title card
    let plan = script_assembly::build_assembly_plan(
        script,
        alignment,
        0,
        title_duration_ns,
        include_titles,
    );

    let transcripts = collected_transcripts.borrow().clone();
    let old_tracks = {
        let mut proj = project.borrow_mut();
        let mut lib = library.borrow_mut();
        let old_tracks = script_assembly::apply_assembly_plan(&mut proj, &mut lib, &plan);
        for (source_path, segments) in transcripts.iter() {
            crate::model::media_library::upsert_media_transcript(
                &mut lib,
                source_path,
                0,
                u64::MAX,
                segments.clone(),
            );
        }
        old_tracks
    };

    // Store script path for FCPXML persistence.
    {
        let mut proj = project.borrow_mut();
        proj.parsed_script_path = Some(script.path.clone());
    }

    // Register undo command.
    // The assembly has already been applied, so we undo it first, then
    // re-apply through history.execute() so undo/redo work correctly.
    {
        let new_tracks = project.borrow().tracks.clone();
        let cmd = ScriptAssemblyCommand {
            old_tracks,
            new_tracks,
            label: "Script to Timeline".to_string(),
        };
        // Undo the apply so execute() re-applies it through the history.
        EditCommand::undo(&cmd, &mut project.borrow_mut());
        let mut ts = timeline_state.borrow_mut();
        let mut proj = project.borrow_mut();
        ts.history.execute(Box::new(cmd), &mut proj);
    }

    on_project_changed();
}
