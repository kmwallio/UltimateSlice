use crate::media::bg_removal_cache::BgRemovalCache;
use crate::media::export::{export_project, ExportProgress};
use crate::media::frame_interp_cache::FrameInterpCache;
use crate::model::project::Project;
use crate::ui_state::{self, ExportQueueJob, ExportQueueJobStatus, ExportQueueState};
use gdk4;
use glib;
use gtk4::prelude::*;
use gtk4::{self as gtk};
use std::cell::RefCell;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc};
use std::time::Duration;

/// Drop-target row used as the "send to end of queue" zone. Lives at the
/// bottom of the list whenever there are at least two pending jobs.
const END_ZONE_ROW_NAME: &str = "export-queue-end-drop-zone";

/// Build and return the Export Queue management dialog.
pub fn build_export_queue_dialog(
    project: Rc<RefCell<Project>>,
    bg_removal_cache: Rc<RefCell<BgRemovalCache>>,
    frame_interp_cache: Rc<RefCell<FrameInterpCache>>,
    render_replace_cache: Rc<RefCell<crate::media::render_replace_cache::RenderReplaceCache>>,
    transient_for: Option<&gtk::Window>,
) -> gtk::Window {
    let win = gtk::Window::builder()
        .title("Export Queue")
        .default_width(620)
        .default_height(420)
        .build();
    if let Some(parent) = transient_for {
        win.set_transient_for(Some(parent));
        win.set_modal(true);
    }

    let vbox = gtk::Box::new(gtk::Orientation::Vertical, 0);
    win.set_child(Some(&vbox));

    // Scrolled list of jobs
    let scroll = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .vscrollbar_policy(gtk::PolicyType::Automatic)
        .vexpand(true)
        .build();
    let list_box = gtk::ListBox::new();
    list_box.set_selection_mode(gtk::SelectionMode::None);
    list_box.add_css_class("rich-list");
    scroll.set_child(Some(&list_box));
    vbox.append(&scroll);

    // Shared queue state (reloaded each time the dialog opens).
    let queue_state: Rc<RefCell<ExportQueueState>> =
        Rc::new(RefCell::new(ui_state::load_export_queue_state()));

    // Load-time fixup: if the app crashed (or was force-quit) during an
    // earlier export run, the persisted queue will have that job stuck
    // at status=Running with no worker behind it. Flip those back to
    // Pending so the user can Run Queue again to recover. Only writes
    // to disk if anything actually changed.
    {
        let mut q = queue_state.borrow_mut();
        let repaired = q.repair_stuck_running();
        if repaired > 0 {
            log::info!(
                "Export queue: repaired {repaired} job(s) stuck at Running \
                 (app likely crashed during a previous queue run)."
            );
            ui_state::save_export_queue_state(&q);
        }
    }

    // Pause-after-current flag — set by the Pause button, read by the
    // worker thread between jobs. Pausing does NOT kill the current
    // export; it just gates the start of the next one.
    let pause_flag = Arc::new(AtomicBool::new(false));

    // Bottom action bar
    let bar = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    bar.set_margin_start(12);
    bar.set_margin_end(12);
    bar.set_margin_top(8);
    bar.set_margin_bottom(8);

    let status_label = gtk::Label::new(Some(""));
    status_label.set_halign(gtk::Align::Start);
    status_label.set_hexpand(true);
    status_label.add_css_class("dim-label");

    let btn_clear = gtk::Button::with_label("Clear Done/Error");
    btn_clear.set_tooltip_text(Some("Remove all completed and failed jobs from the queue"));

    let btn_pause = gtk::Button::with_label("⏸ Pause");
    btn_pause.set_tooltip_text(Some(
        "Pause the queue after the current job finishes. The currently \
         exporting job runs to completion — it is not interrupted.",
    ));
    btn_pause.set_visible(false);

    let btn_run = gtk::Button::with_label("▶ Run Queue");
    btn_run.add_css_class("suggested-action");
    btn_run.set_tooltip_text(Some("Export all pending jobs in the queue"));

    bar.append(&status_label);
    bar.append(&btn_clear);
    bar.append(&btn_pause);
    bar.append(&btn_run);
    vbox.append(&bar);

    // Initial rebuild + button-state sync.
    rebuild_list_impl(&list_box, &queue_state, &status_label, &btn_run);

    // ── Clear Done/Error button ───────────────────────────────────────────
    {
        let queue_state = queue_state.clone();
        let list_box = list_box.clone();
        let status_label = status_label.clone();
        let btn_run = btn_run.clone();
        btn_clear.connect_clicked(move |_| {
            let mut q = queue_state.borrow_mut();
            q.jobs.retain(|j| {
                !matches!(
                    j.status,
                    ExportQueueJobStatus::Done | ExportQueueJobStatus::Error
                )
            });
            ui_state::save_export_queue_state(&q);
            drop(q);
            rebuild_list_impl(&list_box, &queue_state, &status_label, &btn_run);
        });
    }

    // ── Pause / Resume button ─────────────────────────────────────────────
    {
        let pause_flag = pause_flag.clone();
        btn_pause.connect_clicked(move |btn| {
            let now_paused = !pause_flag.load(Ordering::Relaxed);
            pause_flag.store(now_paused, Ordering::Relaxed);
            if now_paused {
                btn.set_label("▶ Resume");
                btn.set_tooltip_text(Some(
                    "Resume the queue. The next pending job will start once \
                     the current export finishes (or immediately if idle).",
                ));
            } else {
                btn.set_label("⏸ Pause");
                btn.set_tooltip_text(Some(
                    "Pause the queue after the current job finishes. The \
                     currently exporting job runs to completion — it is not \
                     interrupted.",
                ));
            }
        });
    }

    // ── Run Queue button ──────────────────────────────────────────────────
    {
        let queue_state = queue_state.clone();
        let list_box = list_box.clone();
        let status_label = status_label.clone();
        let btn_run_clone = btn_run.clone();
        let btn_clear = btn_clear.clone();
        let btn_pause_run = btn_pause.clone();
        let project = project.clone();
        let bg_removal_cache = bg_removal_cache.clone();
        let pause_flag_outer = pause_flag.clone();
        btn_run.connect_clicked(move |btn| {
            btn.set_sensitive(false);
            btn_clear.set_sensitive(false);
            // Fresh run starts unpaused; remember to reset the pause label.
            pause_flag_outer.store(false, Ordering::Relaxed);
            btn_pause_run.set_label("⏸ Pause");
            btn_pause_run.set_visible(true);
            btn_pause_run.set_sensitive(true);

            // Collect pending job IDs.
            let pending_ids: Vec<String> = queue_state
                .borrow()
                .jobs
                .iter()
                .filter(|j| j.status == ExportQueueJobStatus::Pending)
                .map(|j| j.id.clone())
                .collect();

            if pending_ids.is_empty() {
                btn_pause_run.set_visible(false);
                return;
            }

            // Channel for per-job progress messages
            #[derive(Debug)]
            enum QueueMsg {
                JobStarted(String),
                Progress(String, f64),
                JobDone(String),
                JobError(String, String),
                Paused,
                Resumed,
                AllDone,
            }

            let (tx, rx) = mpsc::channel::<QueueMsg>();

            // Snapshot job list for the background thread
            let jobs_snapshot: Vec<ExportQueueJob> = queue_state
                .borrow()
                .jobs
                .iter()
                .filter(|j| pending_ids.contains(&j.id))
                .cloned()
                .collect();

            let proj_snapshot = project.borrow().clone();
            let bg_paths = bg_removal_cache.borrow().paths.clone();
            let interp_paths = frame_interp_cache
                .borrow()
                .snapshot_paths_by_clip_id(&proj_snapshot);
            // Snapshot the render-replace sidecar map once for the
            // whole queue. Each job runs on a fresh thread from this
            // snapshot, so queued jobs consume whatever bakes are
            // ready at queue-start time (mid-queue bake completions
            // aren't picked up — but the queue is already running
            // non-interactively). Empty when the cache has no bakes.
            let rr_paths = render_replace_cache.borrow().paths.clone();

            let pause_flag_worker = pause_flag_outer.clone();
            std::thread::spawn(move || {
                let mut last_pause_seen = false;
                for job in &jobs_snapshot {
                    // Pause gate: block before starting each new job while
                    // the flag is set. Send Paused/Resumed transitions so
                    // the UI status label can reflect what's happening.
                    while pause_flag_worker.load(Ordering::Relaxed) {
                        if !last_pause_seen {
                            let _ = tx.send(QueueMsg::Paused);
                            last_pause_seen = true;
                        }
                        std::thread::sleep(Duration::from_millis(200));
                    }
                    if last_pause_seen {
                        let _ = tx.send(QueueMsg::Resumed);
                        last_pause_seen = false;
                    }

                    let _ = tx.send(QueueMsg::JobStarted(job.id.clone()));
                    let opts = job.options.to_export_options();
                    let (ptx, prx) = mpsc::channel::<ExportProgress>();
                    let output = job.output_path.clone();
                    let output_bg = output.clone();
                    let proj2 = proj_snapshot.clone();
                    let bg_paths2 = bg_paths.clone();
                    let interp_paths2 = interp_paths.clone();
                    let rr_paths2 = rr_paths.clone();
                    let handle = std::thread::spawn(move || {
                        if let Err(e) = export_project(
                            &proj2,
                            &output_bg,
                            opts,
                            None,
                            &bg_paths2,
                            &interp_paths2,
                            &rr_paths2,
                            ptx.clone(),
                        ) {
                            let _ = ptx.send(ExportProgress::Error(e.to_string()));
                        }
                    });
                    let mut last_err: Option<String> = None;
                    loop {
                        match prx.recv() {
                            Ok(ExportProgress::Progress(p)) => {
                                let _ = tx.send(QueueMsg::Progress(job.id.clone(), p));
                            }
                            Ok(ExportProgress::Done) => {
                                let _ = tx.send(QueueMsg::JobDone(job.id.clone()));
                                break;
                            }
                            Ok(ExportProgress::Error(e)) => {
                                last_err = Some(e.clone());
                                let _ = tx.send(QueueMsg::JobError(job.id.clone(), e));
                                break;
                            }
                            Err(_) => {
                                // Channel closed — check if the thread errored
                                if last_err.is_none() {
                                    let _ = tx.send(QueueMsg::JobDone(job.id.clone()));
                                }
                                break;
                            }
                        }
                    }
                    let _ = handle.join();
                }
                let _ = tx.send(QueueMsg::AllDone);
            });

            // Poll the channel and update UI
            let queue_state = queue_state.clone();
            let list_box = list_box.clone();
            let status_label = status_label.clone();
            let btn_run_poll = btn_run_clone.clone();
            let btn_clear_poll = btn_clear.clone();
            let btn_pause_poll = btn_pause_run.clone();
            let pause_flag_poll = pause_flag_outer.clone();
            glib::timeout_add_local(std::time::Duration::from_millis(150), move || {
                while let Ok(msg) = rx.try_recv() {
                    match msg {
                        QueueMsg::JobStarted(id) => {
                            let mut q = queue_state.borrow_mut();
                            if let Some(job) = q.jobs.iter_mut().find(|j| j.id == id) {
                                job.status = ExportQueueJobStatus::Running;
                                job.error = None;
                            }
                            ui_state::set_export_queue_runtime_progress(&id, None);
                            ui_state::save_export_queue_state(&q);
                            drop(q);
                            rebuild_list_impl(
                                &list_box,
                                &queue_state,
                                &status_label,
                                &btn_run_poll,
                            );
                        }
                        QueueMsg::Progress(id, p) => {
                            ui_state::set_export_queue_runtime_progress(&id, Some(p));
                            status_label.set_text(&format!("Exporting… {:.0}%", p * 100.0));
                        }
                        QueueMsg::JobDone(id) => {
                            let mut q = queue_state.borrow_mut();
                            if let Some(job) = q.jobs.iter_mut().find(|j| j.id == id) {
                                job.status = ExportQueueJobStatus::Done;
                            }
                            ui_state::set_export_queue_runtime_progress(&id, None);
                            ui_state::save_export_queue_state(&q);
                            drop(q);
                            rebuild_list_impl(
                                &list_box,
                                &queue_state,
                                &status_label,
                                &btn_run_poll,
                            );
                        }
                        QueueMsg::JobError(id, err) => {
                            let mut q = queue_state.borrow_mut();
                            if let Some(job) = q.jobs.iter_mut().find(|j| j.id == id) {
                                job.status = ExportQueueJobStatus::Error;
                                job.error = Some(err);
                            }
                            ui_state::set_export_queue_runtime_progress(&id, None);
                            ui_state::save_export_queue_state(&q);
                            drop(q);
                            rebuild_list_impl(
                                &list_box,
                                &queue_state,
                                &status_label,
                                &btn_run_poll,
                            );
                        }
                        QueueMsg::Paused => {
                            status_label
                                .set_text("Queue paused — current job finished. Resume to continue.");
                        }
                        QueueMsg::Resumed => {
                            status_label.set_text("Resuming…");
                        }
                        QueueMsg::AllDone => {
                            btn_run_poll.set_sensitive(false);
                            btn_clear_poll.set_sensitive(true);
                            btn_pause_poll.set_visible(false);
                            pause_flag_poll.store(false, Ordering::Relaxed);
                            btn_pause_poll.set_label("⏸ Pause");
                            let pending = queue_state
                                .borrow()
                                .jobs
                                .iter()
                                .filter(|j| j.status == ExportQueueJobStatus::Pending)
                                .count();
                            btn_run_poll.set_sensitive(pending > 0);
                            rebuild_list_impl(
                                &list_box,
                                &queue_state,
                                &status_label,
                                &btn_run_poll,
                            );
                            return glib::ControlFlow::Break;
                        }
                    }
                }
                glib::ControlFlow::Continue
            });
        });
    }

    win
}

fn status_badge_text(status: &ExportQueueJobStatus) -> &'static str {
    match status {
        ExportQueueJobStatus::Pending => "Pending",
        ExportQueueJobStatus::Running => "Running…",
        ExportQueueJobStatus::Done => "Done ✓",
        ExportQueueJobStatus::Error => "Error ✗",
    }
}

fn status_badge_css(status: &ExportQueueJobStatus) -> &'static str {
    match status {
        ExportQueueJobStatus::Pending => "dim-label",
        ExportQueueJobStatus::Running => "accent",
        ExportQueueJobStatus::Done => "success",
        ExportQueueJobStatus::Error => "error",
    }
}

/// Single source of truth for redrawing the queue list. Called once at
/// dialog open and again after every state mutation (remove, retry,
/// reorder, queue progress). Closures from previous rebuilds are dropped
/// with the old widgets — no lingering references.
fn rebuild_list_impl(
    list_box: &gtk::ListBox,
    queue_state: &Rc<RefCell<ExportQueueState>>,
    status_label: &gtk::Label,
    btn_run: &gtk::Button,
) {
    while let Some(child) = list_box.first_child() {
        list_box.remove(&child);
    }

    let queue = queue_state.borrow();
    let pending_count = queue
        .jobs
        .iter()
        .filter(|j| j.status == ExportQueueJobStatus::Pending)
        .count();

    if queue.jobs.is_empty() {
        let empty = gtk::Label::new(Some("No export jobs in the queue."));
        empty.add_css_class("dim-label");
        empty.set_margin_start(12);
        empty.set_margin_top(16);
        empty.set_margin_bottom(16);
        list_box.append(&empty);
        status_label.set_text("0 job(s) total, 0 pending");
        btn_run.set_sensitive(false);
        return;
    }

    for job in &queue.jobs {
        let is_pending = job.status == ExportQueueJobStatus::Pending;
        let is_errored = job.status == ExportQueueJobStatus::Error;

        let row_box = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        row_box.set_margin_start(8);
        row_box.set_margin_end(8);
        row_box.set_margin_top(6);
        row_box.set_margin_bottom(6);

        // Drag handle on pending rows. Non-pending jobs get a blank
        // placeholder of the same width so column alignment is stable.
        let handle = gtk::Label::new(Some(if is_pending { "≡" } else { "" }));
        handle.set_width_chars(2);
        handle.add_css_class("dim-label");
        if is_pending {
            handle.set_tooltip_text(Some("Drag to reorder this job in the queue"));
        }
        row_box.append(&handle);

        let info_box = gtk::Box::new(gtk::Orientation::Vertical, 2);
        info_box.set_hexpand(true);

        let name_label = gtk::Label::new(Some(&job.label));
        name_label.set_halign(gtk::Align::Start);
        name_label.set_ellipsize(gtk4::pango::EllipsizeMode::Middle);

        let path_label = gtk::Label::new(Some(&job.output_path));
        path_label.set_halign(gtk::Align::Start);
        path_label.add_css_class("dim-label");
        path_label.set_ellipsize(gtk4::pango::EllipsizeMode::Start);

        info_box.append(&name_label);
        info_box.append(&path_label);
        if let Some(ref err) = job.error {
            let err_label = gtk::Label::new(Some(err));
            err_label.add_css_class("error");
            err_label.set_halign(gtk::Align::Start);
            err_label.set_ellipsize(gtk4::pango::EllipsizeMode::End);
            info_box.append(&err_label);
        }

        let badge = gtk::Label::new(Some(status_badge_text(&job.status)));
        badge.add_css_class(status_badge_css(&job.status));
        badge.set_width_chars(10);

        row_box.append(&info_box);
        row_box.append(&badge);

        // Retry button on errored rows. Flips the job back to Pending so
        // the user can re-run it via Run Queue without re-exporting from
        // the project window.
        if is_errored {
            let btn_retry = gtk::Button::with_label("↻");
            btn_retry.add_css_class("flat");
            btn_retry.set_tooltip_text(Some("Retry — set this job back to Pending"));
            let job_id = job.id.clone();
            let queue_state_rt = queue_state.clone();
            let list_box_rt = list_box.clone();
            let status_label_rt = status_label.clone();
            let btn_run_rt = btn_run.clone();
            btn_retry.connect_clicked(move |_| {
                let mut q = queue_state_rt.borrow_mut();
                if q.retry_errored(&job_id) {
                    ui_state::save_export_queue_state(&q);
                }
                drop(q);
                rebuild_list_impl(&list_box_rt, &queue_state_rt, &status_label_rt, &btn_run_rt);
            });
            row_box.append(&btn_retry);
        }

        // Remove button (only for pending/error jobs — running/done are
        // anchored).
        if is_pending || is_errored {
            let btn_remove = gtk::Button::with_label("✕");
            btn_remove.add_css_class("flat");
            btn_remove.set_tooltip_text(Some("Remove from queue"));
            let job_id = job.id.clone();
            let queue_state_rm = queue_state.clone();
            let list_box_rm = list_box.clone();
            let status_label_rm = status_label.clone();
            let btn_run_rm = btn_run.clone();
            btn_remove.connect_clicked(move |_| {
                let mut q = queue_state_rm.borrow_mut();
                q.jobs.retain(|j| j.id != job_id);
                ui_state::save_export_queue_state(&q);
                drop(q);
                rebuild_list_impl(&list_box_rm, &queue_state_rm, &status_label_rm, &btn_run_rm);
            });
            row_box.append(&btn_remove);
        }

        let row = gtk::ListBoxRow::new();
        row.set_child(Some(&row_box));

        // Drag source + drop target on pending rows. The payload is the
        // job id as a plain string; the drop handler swaps positions via
        // ExportQueueState::move_pending_before.
        if is_pending {
            let drag_src = gtk::DragSource::new();
            drag_src.set_actions(gdk4::DragAction::MOVE);
            let val = glib::Value::from(&job.id);
            drag_src.set_content(Some(&gdk4::ContentProvider::for_value(&val)));
            row.add_controller(drag_src);

            let drop_target = gtk::DropTarget::new(glib::Type::STRING, gdk4::DragAction::MOVE);
            let target_id = job.id.clone();
            let queue_state_dt = queue_state.clone();
            let list_box_dt = list_box.clone();
            let status_label_dt = status_label.clone();
            let btn_run_dt = btn_run.clone();
            drop_target.connect_drop(move |_t, value, _x, _y| {
                let src_id = match value.get::<String>() {
                    Ok(s) => s,
                    Err(_) => return false,
                };
                let mut q = queue_state_dt.borrow_mut();
                let moved = q.move_pending_before(&src_id, &target_id);
                if moved {
                    ui_state::save_export_queue_state(&q);
                }
                drop(q);
                if moved {
                    rebuild_list_impl(
                        &list_box_dt,
                        &queue_state_dt,
                        &status_label_dt,
                        &btn_run_dt,
                    );
                }
                moved
            });
            row.add_controller(drop_target);
        }

        list_box.append(&row);
    }

    // End-of-queue drop zone: only meaningful when there are at least two
    // pending jobs (otherwise there's nothing to send anywhere). Drop here
    // moves the dragged pending job to the very end of the Vec.
    if pending_count >= 2 {
        let end_row_box = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        end_row_box.set_margin_start(8);
        end_row_box.set_margin_end(8);
        end_row_box.set_margin_top(4);
        end_row_box.set_margin_bottom(4);
        let end_label =
            gtk::Label::new(Some("⤓  Drop here to move to end of queue"));
        end_label.add_css_class("dim-label");
        end_label.set_halign(gtk::Align::Center);
        end_label.set_hexpand(true);
        end_row_box.append(&end_label);

        let end_row = gtk::ListBoxRow::new();
        end_row.set_widget_name(END_ZONE_ROW_NAME);
        end_row.set_selectable(false);
        end_row.set_activatable(false);
        end_row.set_child(Some(&end_row_box));

        let drop_target = gtk::DropTarget::new(glib::Type::STRING, gdk4::DragAction::MOVE);
        let queue_state_end = queue_state.clone();
        let list_box_end = list_box.clone();
        let status_label_end = status_label.clone();
        let btn_run_end = btn_run.clone();
        drop_target.connect_drop(move |_t, value, _x, _y| {
            let src_id = match value.get::<String>() {
                Ok(s) => s,
                Err(_) => return false,
            };
            let mut q = queue_state_end.borrow_mut();
            let moved = q.move_pending_to_end(&src_id);
            if moved {
                ui_state::save_export_queue_state(&q);
            }
            drop(q);
            if moved {
                rebuild_list_impl(
                    &list_box_end,
                    &queue_state_end,
                    &status_label_end,
                    &btn_run_end,
                );
            }
            moved
        });
        end_row.add_controller(drop_target);
        list_box.append(&end_row);
    }

    status_label.set_text(&format!(
        "{} job(s) total, {} pending",
        queue.jobs.len(),
        pending_count
    ));
    btn_run.set_sensitive(pending_count > 0);
}
