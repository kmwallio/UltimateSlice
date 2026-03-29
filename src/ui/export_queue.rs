use crate::media::bg_removal_cache::BgRemovalCache;
use crate::media::export::{export_project, ExportProgress};
use crate::model::project::Project;
use crate::ui_state::{self, ExportQueueJob, ExportQueueJobStatus, ExportQueueState};
use glib;
use gtk4::prelude::*;
use gtk4::{self as gtk};
use std::cell::RefCell;
use std::rc::Rc;
use std::sync::mpsc;

/// Build and return the Export Queue management dialog.
pub fn build_export_queue_dialog(
    project: Rc<RefCell<Project>>,
    bg_removal_cache: Rc<RefCell<BgRemovalCache>>,
    transient_for: Option<&gtk::Window>,
) -> gtk::Window {
    let win = gtk::Window::builder()
        .title("Export Queue")
        .default_width(580)
        .default_height(400)
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

    // Shared queue state (reloaded each time we rebuild the list)
    let queue_state: Rc<RefCell<ExportQueueState>> =
        Rc::new(RefCell::new(ui_state::load_export_queue_state()));

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

    let btn_run = gtk::Button::with_label("▶ Run Queue");
    btn_run.add_css_class("suggested-action");
    btn_run.set_tooltip_text(Some("Export all pending jobs in the queue"));

    bar.append(&status_label);
    bar.append(&btn_clear);
    bar.append(&btn_run);
    vbox.append(&bar);

    // ── Helpers ──────────────────────────────────────────────────────────

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

    let rebuild_list = {
        let list_box = list_box.clone();
        let queue_state = queue_state.clone();
        let status_label = status_label.clone();
        let btn_run = btn_run.clone();
        move || {
            // Clear existing rows
            while let Some(child) = list_box.first_child() {
                list_box.remove(&child);
            }

            let queue = queue_state.borrow();
            let pending = queue
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
            } else {
                for job in &queue.jobs {
                    let row_box = gtk::Box::new(gtk::Orientation::Horizontal, 8);
                    row_box.set_margin_start(12);
                    row_box.set_margin_end(8);
                    row_box.set_margin_top(6);
                    row_box.set_margin_bottom(6);

                    let info_box = gtk::Box::new(gtk::Orientation::Vertical, 2);
                    info_box.set_hexpand(true);

                    let name_label = gtk::Label::new(Some(&job.label));
                    name_label.set_halign(gtk::Align::Start);
                    name_label.set_ellipsize(gtk4::pango::EllipsizeMode::Middle);

                    let path_label = gtk::Label::new(Some(&job.output_path));
                    path_label.set_halign(gtk::Align::Start);
                    path_label.add_css_class("dim-label");
                    path_label.set_ellipsize(gtk4::pango::EllipsizeMode::Start);

                    if let Some(ref err) = job.error {
                        let err_label = gtk::Label::new(Some(err));
                        err_label.add_css_class("error");
                        err_label.set_halign(gtk::Align::Start);
                        err_label.set_ellipsize(gtk4::pango::EllipsizeMode::End);
                        info_box.append(&name_label);
                        info_box.append(&path_label);
                        info_box.append(&err_label);
                    } else {
                        info_box.append(&name_label);
                        info_box.append(&path_label);
                    }

                    let badge = gtk::Label::new(Some(status_badge_text(&job.status)));
                    badge.add_css_class(status_badge_css(&job.status));
                    badge.set_width_chars(10);

                    row_box.append(&info_box);
                    row_box.append(&badge);

                    // Remove button (only for pending/error jobs)
                    if matches!(
                        job.status,
                        ExportQueueJobStatus::Pending | ExportQueueJobStatus::Error
                    ) {
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
                            // Rebuild via idle
                            let queue_state2 = queue_state_rm.clone();
                            let list_box2 = list_box_rm.clone();
                            let status_label2 = status_label_rm.clone();
                            let btn_run2 = btn_run_rm.clone();
                            glib::idle_add_local_once(move || {
                                rebuild_list_fn(
                                    &list_box2,
                                    &queue_state2,
                                    &status_label2,
                                    &btn_run2,
                                );
                            });
                        });
                        row_box.append(&btn_remove);
                    }

                    let row = gtk::ListBoxRow::new();
                    row.set_child(Some(&row_box));
                    list_box.append(&row);
                }
            }

            status_label.set_text(&format!(
                "{} job(s) total, {} pending",
                queue.jobs.len(),
                pending
            ));
            btn_run.set_sensitive(pending > 0);
        }
    };

    // We need a free function for use inside closures that can't capture `rebuild_list`
    fn rebuild_list_fn(
        list_box: &gtk::ListBox,
        queue_state: &Rc<RefCell<ExportQueueState>>,
        status_label: &gtk::Label,
        btn_run: &gtk::Button,
    ) {
        while let Some(child) = list_box.first_child() {
            list_box.remove(&child);
        }
        let queue = queue_state.borrow();
        let pending = queue
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
        } else {
            for job in &queue.jobs {
                let row_box = gtk::Box::new(gtk::Orientation::Horizontal, 8);
                row_box.set_margin_start(12);
                row_box.set_margin_end(8);
                row_box.set_margin_top(6);
                row_box.set_margin_bottom(6);

                let info_box = gtk::Box::new(gtk::Orientation::Vertical, 2);
                info_box.set_hexpand(true);

                let name_label = gtk::Label::new(Some(&job.label));
                name_label.set_halign(gtk::Align::Start);

                let path_label = gtk::Label::new(Some(&job.output_path));
                path_label.set_halign(gtk::Align::Start);
                path_label.add_css_class("dim-label");

                info_box.append(&name_label);
                info_box.append(&path_label);
                if let Some(ref err) = job.error {
                    let err_label = gtk::Label::new(Some(err));
                    err_label.add_css_class("error");
                    err_label.set_halign(gtk::Align::Start);
                    info_box.append(&err_label);
                }

                let badge = gtk::Label::new(Some(match &job.status {
                    ExportQueueJobStatus::Pending => "Pending",
                    ExportQueueJobStatus::Running => "Running…",
                    ExportQueueJobStatus::Done => "Done ✓",
                    ExportQueueJobStatus::Error => "Error ✗",
                }));
                badge.add_css_class(match &job.status {
                    ExportQueueJobStatus::Pending => "dim-label",
                    ExportQueueJobStatus::Running => "accent",
                    ExportQueueJobStatus::Done => "success",
                    ExportQueueJobStatus::Error => "error",
                });

                row_box.append(&info_box);
                row_box.append(&badge);

                let row = gtk::ListBoxRow::new();
                row.set_child(Some(&row_box));
                list_box.append(&row);
            }
        }

        status_label.set_text(&format!(
            "{} job(s) total, {} pending",
            queue.jobs.len(),
            pending
        ));
        btn_run.set_sensitive(pending > 0);
    }

    rebuild_list();

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
            rebuild_list_fn(&list_box, &queue_state, &status_label, &btn_run);
        });
    }

    // ── Run Queue button ──────────────────────────────────────────────────
    {
        let queue_state = queue_state.clone();
        let list_box = list_box.clone();
        let status_label = status_label.clone();
        let btn_run_clone = btn_run.clone();
        let btn_clear = btn_clear.clone();
        let project = project.clone();
        let bg_removal_cache = bg_removal_cache.clone();
        btn_run.connect_clicked(move |btn| {
            btn.set_sensitive(false);
            btn_clear.set_sensitive(false);

            // Collect pending job IDs
            let pending_ids: Vec<String> = queue_state
                .borrow()
                .jobs
                .iter()
                .filter(|j| j.status == ExportQueueJobStatus::Pending)
                .map(|j| j.id.clone())
                .collect();

            if pending_ids.is_empty() {
                return;
            }

            // Channel for per-job progress messages
            #[derive(Debug)]
            enum QueueMsg {
                JobStarted(String),
                Progress(String, f64),
                JobDone(String),
                JobError(String, String),
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

            std::thread::spawn(move || {
                for job in &jobs_snapshot {
                    let _ = tx.send(QueueMsg::JobStarted(job.id.clone()));
                    let opts = job.options.to_export_options();
                    let (ptx, prx) = mpsc::channel::<ExportProgress>();
                    let output = job.output_path.clone();
                    let output_bg = output.clone();
                    let proj2 = proj_snapshot.clone();
                    let bg_paths2 = bg_paths.clone();
                    let job_id = job.id.clone();
                    let tx2 = tx.clone();
                    let handle = std::thread::spawn(move || {
                        if let Err(e) =
                            export_project(&proj2, &output_bg, opts, None, &bg_paths2, ptx.clone())
                        {
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
            glib::timeout_add_local(std::time::Duration::from_millis(150), move || {
                while let Ok(msg) = rx.try_recv() {
                    match msg {
                        QueueMsg::JobStarted(id) => {
                            let mut q = queue_state.borrow_mut();
                            if let Some(job) = q.jobs.iter_mut().find(|j| j.id == id) {
                                job.status = ExportQueueJobStatus::Running;
                            }
                            ui_state::save_export_queue_state(&q);
                            drop(q);
                            rebuild_list_fn(
                                &list_box,
                                &queue_state,
                                &status_label,
                                &btn_run_poll,
                            );
                        }
                        QueueMsg::Progress(id, p) => {
                            status_label.set_text(&format!("Exporting… {:.0}%", p * 100.0));
                        }
                        QueueMsg::JobDone(id) => {
                            let mut q = queue_state.borrow_mut();
                            if let Some(job) = q.jobs.iter_mut().find(|j| j.id == id) {
                                job.status = ExportQueueJobStatus::Done;
                            }
                            ui_state::save_export_queue_state(&q);
                            drop(q);
                            rebuild_list_fn(
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
                            ui_state::save_export_queue_state(&q);
                            drop(q);
                            rebuild_list_fn(
                                &list_box,
                                &queue_state,
                                &status_label,
                                &btn_run_poll,
                            );
                        }
                        QueueMsg::AllDone => {
                            btn_run_poll.set_sensitive(false);
                            btn_clear_poll.set_sensitive(true);
                            let pending = queue_state
                                .borrow()
                                .jobs
                                .iter()
                                .filter(|j| j.status == ExportQueueJobStatus::Pending)
                                .count();
                            btn_run_poll.set_sensitive(pending > 0);
                            rebuild_list_fn(
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
