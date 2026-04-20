use crate::media::project_health::{
    ProjectHealthPathCategory, ProjectHealthPathKind, ProjectHealthPathSummary,
    ProjectHealthSnapshot,
};
use gtk4::prelude::*;
use gtk4::{self as gtk};
use std::rc::Rc;

pub fn build_project_health_dialog(
    snapshot_provider: Rc<dyn Fn() -> ProjectHealthSnapshot>,
    on_cleanup: Rc<dyn Fn(ProjectHealthPathKind) -> Result<String, String>>,
    on_relink_media: Rc<dyn Fn()>,
    transient_for: Option<&gtk::Window>,
) -> gtk::Window {
    let win = gtk::Window::builder()
        .title("Project Health")
        .default_width(760)
        .default_height(560)
        .build();
    if let Some(parent) = transient_for {
        win.set_transient_for(Some(parent));
        win.set_modal(true);
    }

    let vbox = gtk::Box::new(gtk::Orientation::Vertical, 12);
    vbox.set_margin_start(16);
    vbox.set_margin_end(16);
    vbox.set_margin_top(16);
    vbox.set_margin_bottom(16);
    win.set_child(Some(&vbox));

    let summary_label = gtk::Label::new(None);
    summary_label.set_wrap(true);
    summary_label.set_xalign(0.0);
    summary_label.add_css_class("title-4");
    vbox.append(&summary_label);

    let overview_hint = gtk::Label::new(Some(
        "Offline media, managed/generated cache disk usage, proxy sidecars stored in UltimateSlice.cache directories, and installed model directories in one place. Thumbnail previews are in-memory only and are not included here.",
    ));
    overview_hint.set_wrap(true);
    overview_hint.set_xalign(0.0);
    overview_hint.add_css_class("dim-label");
    vbox.append(&overview_hint);

    let offline_frame = gtk::Frame::new(Some("Offline media"));
    let offline_box = gtk::Box::new(gtk::Orientation::Vertical, 6);
    offline_box.set_margin_start(12);
    offline_box.set_margin_end(12);
    offline_box.set_margin_top(12);
    offline_box.set_margin_bottom(12);
    let offline_label = gtk::Label::new(None);
    offline_label.set_wrap(true);
    offline_label.set_selectable(true);
    offline_label.set_xalign(0.0);
    offline_box.append(&offline_label);
    offline_frame.set_child(Some(&offline_box));
    vbox.append(&offline_frame);

    let caches_label = gtk::Label::new(Some("Cache and model locations"));
    caches_label.set_xalign(0.0);
    caches_label.add_css_class("heading");
    vbox.append(&caches_label);

    let scroll = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .vscrollbar_policy(gtk::PolicyType::Automatic)
        .vexpand(true)
        .build();
    let cache_list = gtk::ListBox::new();
    cache_list.set_selection_mode(gtk::SelectionMode::None);
    cache_list.add_css_class("rich-list");
    scroll.set_child(Some(&cache_list));
    vbox.append(&scroll);

    let status_label = gtk::Label::new(None);
    status_label.set_xalign(0.0);
    status_label.add_css_class("dim-label");
    vbox.append(&status_label);

    let button_bar = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    let btn_refresh = gtk::Button::with_label("Refresh");
    let btn_relink = gtk::Button::with_label("Relink Offline Media…");
    let btn_close = gtk::Button::with_label("Close");
    btn_relink.add_css_class("suggested-action");
    button_bar.append(&btn_refresh);
    button_bar.append(&btn_relink);
    button_bar.append(&btn_close);
    vbox.append(&button_bar);

    let refresh_view = {
        let summary_label = summary_label.clone();
        let offline_label = offline_label.clone();
        let cache_list = cache_list.clone();
        let status_label = status_label.clone();
        let btn_relink = btn_relink.clone();
        let snapshot_provider = snapshot_provider.clone();
        let on_cleanup = on_cleanup.clone();
        move || {
            let snapshot = snapshot_provider();
            render_snapshot(
                &summary_label,
                &offline_label,
                &cache_list,
                &status_label,
                &btn_relink,
                snapshot_provider.clone(),
                on_cleanup.clone(),
                snapshot,
            );
        }
    };
    refresh_view();

    {
        let refresh_view = refresh_view.clone();
        btn_refresh.connect_clicked(move |_| refresh_view());
    }
    {
        let status_label = status_label.clone();
        btn_relink.connect_clicked(move |_| {
            status_label
                .set_text("Use the relink dialog to resolve offline files, then click Refresh.");
            on_relink_media();
        });
    }
    {
        let win = win.clone();
        btn_close.connect_clicked(move |_| win.close());
    }

    win
}

fn render_snapshot(
    summary_label: &gtk::Label,
    offline_label: &gtk::Label,
    cache_list: &gtk::ListBox,
    status_label: &gtk::Label,
    relink_button: &gtk::Button,
    snapshot_provider: Rc<dyn Fn() -> ProjectHealthSnapshot>,
    on_cleanup: Rc<dyn Fn(ProjectHealthPathKind) -> Result<String, String>>,
    snapshot: ProjectHealthSnapshot,
) {
    summary_label.set_text(&format!(
        "{} offline source(s) • {} generated cache • {} installed models",
        snapshot.offline_project_source_count,
        format_file_size(snapshot.generated_cache_bytes),
        format_file_size(snapshot.installed_model_bytes),
    ));
    relink_button.set_sensitive(!snapshot.offline_paths.is_empty());

    if snapshot.offline_paths.is_empty() {
        offline_label.set_text("No offline media detected.");
    } else {
        let mut lines = vec![format!(
            "{} project source(s) offline • {} library item(s) currently flagged offline",
            snapshot.offline_project_source_count, snapshot.offline_library_item_count
        )];
        for path in snapshot.offline_paths.iter().take(6) {
            lines.push(format!("• {path}"));
        }
        if snapshot.offline_paths.len() > 6 {
            lines.push(format!(
                "• …and {} more",
                snapshot.offline_paths.len().saturating_sub(6)
            ));
        }
        offline_label.set_text(&lines.join("\n"));
    }

    while let Some(child) = cache_list.first_child() {
        cache_list.remove(&child);
    }
    for summary in snapshot.paths {
        cache_list.append(&build_path_row(
            &summary,
            status_label,
            cache_list,
            summary_label,
            offline_label,
            relink_button,
            snapshot_provider.clone(),
            on_cleanup.clone(),
        ));
    }
}

fn build_path_row(
    summary: &ProjectHealthPathSummary,
    status_label: &gtk::Label,
    cache_list: &gtk::ListBox,
    summary_label: &gtk::Label,
    offline_label: &gtk::Label,
    relink_button: &gtk::Button,
    snapshot_provider: Rc<dyn Fn() -> ProjectHealthSnapshot>,
    on_cleanup: Rc<dyn Fn(ProjectHealthPathKind) -> Result<String, String>>,
) -> gtk::ListBoxRow {
    let row = gtk::ListBoxRow::new();
    let row_box = gtk::Box::new(gtk::Orientation::Horizontal, 12);
    row_box.set_margin_start(12);
    row_box.set_margin_end(12);
    row_box.set_margin_top(8);
    row_box.set_margin_bottom(8);

    let info_box = gtk::Box::new(gtk::Orientation::Vertical, 4);
    info_box.set_hexpand(true);

    let title = gtk::Label::new(Some(summary.label));
    title.set_xalign(0.0);
    title.add_css_class("heading");

    let detail = gtk::Label::new(Some(&format_path_detail(summary)));
    detail.set_xalign(0.0);
    detail.set_wrap(true);
    detail.add_css_class("dim-label");

    let path_label = gtk::Label::new(Some(&summary.path));
    path_label.set_xalign(0.0);
    path_label.set_wrap(true);
    path_label.set_selectable(true);
    path_label.add_css_class("dim-label");

    info_box.append(&title);
    info_box.append(&detail);
    info_box.append(&path_label);
    row_box.append(&info_box);

    if summary.cleanup_supported {
        let btn = gtk::Button::with_label("Clear cache");
        let kind = summary.kind;
        let status_label = status_label.clone();
        let cache_list = cache_list.clone();
        let summary_label = summary_label.clone();
        let offline_label = offline_label.clone();
        let relink_button = relink_button.clone();
        btn.connect_clicked(move |_| {
            let message = match on_cleanup(kind) {
                Ok(message) => message,
                Err(err) => format!("Error: {err}"),
            };
            status_label.set_text(&message);
            let snapshot = snapshot_provider();
            render_snapshot(
                &summary_label,
                &offline_label,
                &cache_list,
                &status_label,
                &relink_button,
                snapshot_provider.clone(),
                on_cleanup.clone(),
                snapshot,
            );
        });
        row_box.append(&btn);
    }

    row.set_child(Some(&row_box));
    row
}

fn format_path_detail(summary: &ProjectHealthPathSummary) -> String {
    let category = match summary.category {
        ProjectHealthPathCategory::GeneratedCache => "Generated cache",
        ProjectHealthPathCategory::InstalledModel => "Installed model",
    };
    if !summary.exists {
        return format!("{category} • not present");
    }
    format!(
        "{category} • {} file(s) • {}",
        summary.file_count,
        format_file_size(summary.size_bytes)
    )
}

fn format_file_size(bytes: u64) -> String {
    if bytes == 0 {
        return "0 B".to_string();
    }
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
