use gtk4::prelude::*;
use gtk4::{Align, Box as GBox, Button, DrawingArea, Expander, Label, Orientation, ScrolledWindow};
use std::cell::RefCell;
use std::rc::Rc;

use crate::media::thumb_cache::ThumbnailCache;
use crate::project_versions::RecoverableAutosave;

/// Thumbnail dimensions for recent-project cards in the list. Same 16:9
/// shape as the editor's clip thumbnails, scaled down so two columns
/// (thumb + text) fit comfortably without resizing the welcome layout.
const THUMB_LIST_W: i32 = 120;
const THUMB_LIST_H: i32 = 68;

/// Larger thumbnail for the featured hero card — gets a bigger surface
/// area in the layout so it can take the visual lead.
const THUMB_HERO_W: i32 = 200;
const THUMB_HERO_H: i32 = 112;

#[derive(Clone)]
struct WelcomeRecentProject {
    path: String,
    display_name: String,
    directory_label: String,
    date_label: Option<String>,
    /// Path of the first asset's media source, extracted via the
    /// lightweight `welcome_project_peek` helper. `None` when the
    /// project has no recognizable asset (just-saved blank project,
    /// asset-less template, unreadable file, etc.) — the card then
    /// falls back to a dim placeholder instead of showing nothing.
    first_source: Option<String>,
}

/// Build the welcome panel shown on fresh launch (no startup project).
/// Contains branding, New/Open buttons, optional crash-recovery section,
/// a featured recent project, release highlights, and a recent projects list.
pub fn build_welcome_panel(
    on_new_project: Rc<dyn Fn()>,
    on_open_project: Rc<dyn Fn()>,
    on_open_recent: Rc<dyn Fn(String)>,
    recoverable: Vec<RecoverableAutosave>,
    on_recover: Rc<dyn Fn(RecoverableAutosave)>,
    on_discard_autosave: Rc<dyn Fn(RecoverableAutosave)>,
) -> GBox {
    let outer = GBox::new(Orientation::Vertical, 0);
    outer.set_vexpand(true);
    outer.set_hexpand(true);

    let scroll = ScrolledWindow::new();
    scroll.set_hexpand(true);
    scroll.set_vexpand(true);
    scroll.set_policy(gtk4::PolicyType::Never, gtk4::PolicyType::Automatic);
    scroll.set_propagate_natural_height(true);
    outer.append(&scroll);

    let inner = GBox::new(Orientation::Vertical, 16);
    inner.set_valign(Align::Center);
    inner.set_margin_start(40);
    inner.set_margin_end(40);
    inner.set_margin_top(40);
    inner.set_margin_bottom(40);
    scroll.set_child(Some(&inner));

    let recent_projects = load_recent_projects();

    // Shared thumbnail cache for every card on this welcome panel. Lives
    // for as long as the panel is alive; created fresh on each welcome
    // build (the editor has its own separate cache for the timeline).
    // A glib timer below polls the cache and queues redraws on the
    // DrawingArea widgets when frames finish extracting in the
    // background.
    let thumb_cache: Rc<RefCell<ThumbnailCache>> =
        Rc::new(RefCell::new(ThumbnailCache::new()));
    let thumb_areas: Rc<RefCell<Vec<DrawingArea>>> = Rc::new(RefCell::new(Vec::new()));

    // Title
    let title = Label::new(Some("UltimateSlice"));
    title.add_css_class("welcome-title");
    title.set_halign(Align::Center);
    inner.append(&title);

    // Subtitle
    let subtitle = Label::new(Some("Non-linear Video Editor"));
    subtitle.add_css_class("welcome-subtitle");
    subtitle.set_halign(Align::Center);
    inner.append(&subtitle);

    // Spacer
    let spacer = GBox::new(Orientation::Vertical, 0);
    spacer.set_height_request(16);
    inner.append(&spacer);

    // Action buttons
    let btn_row = GBox::new(Orientation::Horizontal, 12);
    btn_row.set_halign(Align::Center);

    let btn_new = Button::with_label("New Project");
    btn_new.add_css_class("suggested-action");
    btn_new.set_width_request(160);
    {
        let cb = on_new_project.clone();
        btn_new.connect_clicked(move |_| cb());
    }
    btn_row.append(&btn_new);

    let btn_open = Button::with_label("Open Project\u{2026}");
    btn_open.set_width_request(160);
    {
        let cb = on_open_project.clone();
        btn_open.connect_clicked(move |_| cb());
    }
    btn_row.append(&btn_open);

    inner.append(&btn_row);

    if let Some(featured) = recent_projects.first().cloned() {
        let hero = GBox::new(Orientation::Vertical, 8);
        hero.add_css_class("welcome-card");
        hero.add_css_class("welcome-hero-card");
        hero.set_hexpand(true);

        let hero_header = Label::new(Some("Jump back in"));
        hero_header.add_css_class("welcome-section-header");
        hero_header.set_halign(Align::Start);
        hero.append(&hero_header);

        // Two-column body: thumbnail on the left, text + actions on
        // the right.
        let hero_body = GBox::new(Orientation::Horizontal, 16);
        let hero_thumb = build_thumb_area(
            featured.first_source.clone(),
            thumb_cache.clone(),
            THUMB_HERO_W,
            THUMB_HERO_H,
        );
        thumb_areas.borrow_mut().push(hero_thumb.clone());
        hero_body.append(&hero_thumb);

        let hero_text = GBox::new(Orientation::Vertical, 6);
        hero_text.set_hexpand(true);

        let hero_name = Label::new(Some(&featured.display_name));
        hero_name.add_css_class("welcome-hero-project");
        hero_name.set_halign(Align::Start);
        hero_name.set_wrap(true);
        hero_text.append(&hero_name);

        let hero_meta = Label::new(Some(&format_recent_project_meta(&featured)));
        hero_meta.add_css_class("welcome-card-summary");
        hero_meta.set_halign(Align::Start);
        hero_meta.set_wrap(true);
        hero_text.append(&hero_meta);

        let hero_path = Label::new(Some(&featured.directory_label));
        hero_path.add_css_class("welcome-card-detail");
        hero_path.set_halign(Align::Start);
        hero_path.set_ellipsize(gtk4::pango::EllipsizeMode::Start);
        hero_path.set_max_width_chars(56);
        hero_path.set_tooltip_text(Some(&featured.path));
        hero_text.append(&hero_path);

        let hero_actions = GBox::new(Orientation::Horizontal, 8);
        let btn_open_recent = Button::with_label("Open Most Recent");
        btn_open_recent.add_css_class("suggested-action");
        btn_open_recent.set_tooltip_text(Some(&featured.path));
        {
            let cb = on_open_recent.clone();
            let path = featured.path.clone();
            btn_open_recent.connect_clicked(move |_| cb(path.clone()));
        }
        hero_actions.append(&btn_open_recent);
        hero_text.append(&hero_actions);

        hero_body.append(&hero_text);
        hero.append(&hero_body);

        inner.append(&hero);
    }

    // ── Recovery section ─────────────────────────────────────────────
    if !recoverable.is_empty() {
        let recovery_frame = GBox::new(Orientation::Vertical, 6);
        recovery_frame.add_css_class("welcome-recovery-section");

        let recovery_header = Label::new(Some("⚠ Recover Unsaved Work"));
        recovery_header.add_css_class("welcome-recovery-header");
        recovery_header.set_halign(Align::Start);
        recovery_frame.append(&recovery_header);

        for entry in recoverable {
            let row = GBox::new(Orientation::Horizontal, 8);
            row.set_hexpand(true);

            // Project title and path info
            let info_box = GBox::new(Orientation::Vertical, 2);
            info_box.set_hexpand(true);

            let title_label = Label::new(Some(&entry.metadata.project_title));
            title_label.set_halign(Align::Start);
            title_label.set_ellipsize(gtk4::pango::EllipsizeMode::Middle);
            info_box.append(&title_label);

            let detail = if let Some(ref fp) = entry.metadata.project_file_path {
                let fname = std::path::Path::new(fp)
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or(fp);
                format!(
                    "{} — {}",
                    format_autosave_age(entry.metadata.saved_at_unix_secs),
                    fname
                )
            } else {
                format!(
                    "{} — unsaved project",
                    format_autosave_age(entry.metadata.saved_at_unix_secs)
                )
            };
            let detail_label = Label::new(Some(&detail));
            detail_label.add_css_class("dim-label");
            detail_label.set_halign(Align::Start);
            detail_label.set_ellipsize(gtk4::pango::EllipsizeMode::End);
            info_box.append(&detail_label);

            row.append(&info_box);

            // Recover button
            let btn_recover = Button::with_label("Recover");
            btn_recover.add_css_class("suggested-action");
            {
                let cb = on_recover.clone();
                let e = entry.clone();
                btn_recover.connect_clicked(move |_| cb(e.clone()));
            }
            row.append(&btn_recover);

            // Discard button
            let btn_discard = Button::with_label("Discard");
            btn_discard.add_css_class("destructive-action");
            {
                let cb = on_discard_autosave.clone();
                let e = entry.clone();
                let row_weak = row.downgrade();
                btn_discard.connect_clicked(move |_| {
                    cb(e.clone());
                    // Remove this row from the UI
                    if let Some(r) = row_weak.upgrade() {
                        if let Some(parent) = r.parent() {
                            if let Some(bx) = parent.downcast_ref::<GBox>() {
                                bx.remove(&r);
                            }
                        }
                    }
                });
            }
            row.append(&btn_discard);

            recovery_frame.append(&row);
        }

        inner.append(&recovery_frame);
    }

    // Recent projects section
    if recent_projects.len() > 1 {
        let recent_header = Label::new(Some("More Recent Projects"));
        recent_header.add_css_class("welcome-section-header");
        recent_header.set_halign(Align::Start);
        inner.append(&recent_header);

        let recent_list = GBox::new(Orientation::Vertical, 2);
        recent_list.add_css_class("welcome-card");
        recent_list.set_hexpand(true);

        for recent in recent_projects.iter().skip(1) {
            recent_list.append(&build_recent_project_row(
                recent,
                on_open_recent.clone(),
                thumb_cache.clone(),
                &thumb_areas,
            ));
        }

        inner.append(&recent_list);
    }

    let whats_new_card = GBox::new(Orientation::Vertical, 8);
    whats_new_card.add_css_class("welcome-card");
    whats_new_card.set_hexpand(true);

    let whats_new_header = Label::new(Some(&format!(
        "What's New in UltimateSlice {}",
        env!("CARGO_PKG_VERSION")
    )));
    whats_new_header.add_css_class("welcome-section-header");
    whats_new_header.set_halign(Align::Start);
    whats_new_card.append(&whats_new_header);

    let whats_new_summary = Label::new(Some(
        "A quick look at the most visible recent workflow and discoverability upgrades.",
    ));
    whats_new_summary.add_css_class("welcome-card-summary");
    whats_new_summary.set_halign(Align::Start);
    whats_new_summary.set_wrap(true);
    whats_new_card.append(&whats_new_summary);

    let whats_new_expander = Expander::new(Some("Show release highlights"));
    whats_new_expander.set_expanded(false);
    let whats_new_body = GBox::new(Orientation::Vertical, 6);
    for line in [
        "Toasts now handle short-lived status updates instead of flashing the window title.",
        "The footer Jobs menu tracks exports, proxies, AI work, and other active background tasks.",
        "Source Monitor routing and its compact footer are easier to use on smaller displays.",
        "Timeline, Media Library, and Inspector empty states now guide first-run actions more clearly.",
    ] {
        let label = Label::new(Some(line));
        label.set_halign(Align::Start);
        label.set_xalign(0.0);
        label.set_wrap(true);
        label.add_css_class("welcome-card-detail");
        whats_new_body.append(&label);
    }
    whats_new_expander.set_child(Some(&whats_new_body));
    whats_new_card.append(&whats_new_expander);
    inner.append(&whats_new_card);

    // Tip
    let spacer3 = GBox::new(Orientation::Vertical, 0);
    spacer3.set_height_request(16);
    inner.append(&spacer3);

    let tip = Label::new(Some(
        "Tip: Drag media files onto the timeline to start editing.",
    ));
    tip.add_css_class("welcome-tip");
    inner.append(&tip);

    // Poll the shared thumbnail cache and queue redraws on each
    // DrawingArea when its surface arrives. Bounded run (60 ticks ×
    // 250 ms = 15 s) so the timer can't outlive a meaningful "all
    // ten thumbs extracted" window. Welcome screens with offline
    // media will simply paint placeholders forever, which is fine —
    // re-entering the welcome screen restarts the timer.
    if !thumb_areas.borrow().is_empty() {
        let thumb_cache_poll = thumb_cache.clone();
        let thumb_areas_poll = thumb_areas.clone();
        let mut ticks = 0u32;
        glib::timeout_add_local(std::time::Duration::from_millis(250), move || {
            ticks += 1;
            let has_new = thumb_cache_poll.borrow_mut().poll();
            if has_new {
                for area in thumb_areas_poll.borrow().iter() {
                    area.queue_draw();
                }
            }
            if ticks >= 60 {
                glib::ControlFlow::Break
            } else {
                glib::ControlFlow::Continue
            }
        });
    }
    outer
}

fn load_recent_projects() -> Vec<WelcomeRecentProject> {
    crate::recent::load()
        .into_iter()
        .map(|path| {
            let display_name = std::path::Path::new(&path)
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or(&path)
                .to_string();
            let directory_label = std::path::Path::new(&path)
                .parent()
                .and_then(|p| p.to_str())
                .unwrap_or(&path)
                .to_string();
            let first_source = crate::ui::welcome_project_peek::peek_first_asset_source(
                std::path::Path::new(&path),
            );
            WelcomeRecentProject {
                date_label: modification_date_label(&path),
                path,
                display_name,
                directory_label,
                first_source,
            }
        })
        .collect()
}

/// Build a DrawingArea that paints the recent project's first-frame
/// thumbnail (or a dim rounded-rect placeholder while the thumb is
/// still being extracted, or forever if the source is missing).
///
/// The cache is shared across every card on the welcome screen so the
/// same source path being referenced by two recents only extracts once.
/// A glib timer set up in `build_welcome_panel` polls the cache and
/// queues redraws on each area when its surface becomes available.
fn build_thumb_area(
    first_source: Option<String>,
    thumb_cache: Rc<RefCell<ThumbnailCache>>,
    width: i32,
    height: i32,
) -> DrawingArea {
    let area = DrawingArea::new();
    area.set_content_width(width);
    area.set_content_height(height);
    area.add_css_class("welcome-thumb");

    if let Some(ref src) = first_source {
        // Kick off the extraction immediately so it's ready by the
        // time the user's eye gets to this card. request() is a no-op
        // if the surface is already cached.
        thumb_cache.borrow_mut().request(src, 0);
    }

    area.set_draw_func(move |_, cr, w, h| {
        let (wf, hf) = (w as f64, h as f64);
        // Rounded placeholder background — always drawn so the card
        // has a visible shape even before (or instead of) the thumb.
        rounded_rect_local(cr, 0.0, 0.0, wf, hf, 6.0);
        cr.save().ok();
        cr.clip();
        cr.set_source_rgba(0.18, 0.21, 0.28, 0.92);
        let _ = cr.paint();

        if let Some(ref src) = first_source {
            let cache = thumb_cache.borrow();
            if let Some(surface) = cache.get(src, 0) {
                let sw = surface.width() as f64;
                let sh = surface.height() as f64;
                if sw > 0.0 && sh > 0.0 {
                    // Cover-style fit: scale to fill the card, center,
                    // crop the overflow with the clip we set above.
                    let scale = (wf / sw).max(hf / sh);
                    let dx = (wf - sw * scale) / 2.0;
                    let dy = (hf - sh * scale) / 2.0;
                    cr.save().ok();
                    cr.translate(dx, dy);
                    cr.scale(scale, scale);
                    let _ = cr.set_source_surface(surface, 0.0, 0.0);
                    let _ = cr.paint();
                    cr.restore().ok();
                }
            }
        }
        cr.restore().ok();
    });

    area
}

/// Local copy of the timeline widget's `rounded_rect` so we don't drag
/// the timeline module's whole surface into the welcome screen just for
/// one Cairo helper.
fn rounded_rect_local(cr: &gtk4::cairo::Context, x: f64, y: f64, w: f64, h: f64, r: f64) {
    use std::f64::consts::PI;
    cr.new_sub_path();
    let _ = cr.arc(x + w - r, y + r, r, -PI / 2.0, 0.0);
    let _ = cr.arc(x + w - r, y + h - r, r, 0.0, PI / 2.0);
    let _ = cr.arc(x + r, y + h - r, r, PI / 2.0, PI);
    let _ = cr.arc(x + r, y + r, r, PI, 3.0 * PI / 2.0);
    cr.close_path();
}

fn build_recent_project_row(
    recent: &WelcomeRecentProject,
    on_open_recent: Rc<dyn Fn(String)>,
    thumb_cache: Rc<RefCell<ThumbnailCache>>,
    thumb_areas: &Rc<RefCell<Vec<DrawingArea>>>,
) -> Button {
    let row = Button::new();
    row.add_css_class("flat");
    row.add_css_class("welcome-recent-item");

    let row_box = GBox::new(Orientation::Horizontal, 12);
    row_box.set_hexpand(true);

    let thumb_area = build_thumb_area(
        recent.first_source.clone(),
        thumb_cache,
        THUMB_LIST_W,
        THUMB_LIST_H,
    );
    thumb_areas.borrow_mut().push(thumb_area.clone());
    row_box.append(&thumb_area);

    let info_box = GBox::new(Orientation::Vertical, 2);
    info_box.set_hexpand(true);
    info_box.set_valign(Align::Center);

    let name_label = Label::new(Some(&recent.display_name));
    name_label.set_halign(Align::Start);
    name_label.set_hexpand(true);
    name_label.set_ellipsize(gtk4::pango::EllipsizeMode::Middle);
    info_box.append(&name_label);

    let directory_label = Label::new(Some(&recent.directory_label));
    directory_label.add_css_class("welcome-card-detail");
    directory_label.set_halign(Align::Start);
    directory_label.set_ellipsize(gtk4::pango::EllipsizeMode::Start);
    directory_label.set_max_width_chars(48);
    info_box.append(&directory_label);

    row_box.append(&info_box);

    if let Some(date_label) = &recent.date_label {
        let date = Label::new(Some(date_label));
        date.add_css_class("dim-label");
        date.set_halign(Align::End);
        row_box.append(&date);
    }

    row.set_child(Some(&row_box));
    row.set_tooltip_text(Some(&recent.path));

    let path_owned = recent.path.clone();
    row.connect_clicked(move |_| {
        on_open_recent(path_owned.clone());
    });
    row
}

fn format_recent_project_meta(recent: &WelcomeRecentProject) -> String {
    match &recent.date_label {
        Some(date) => format!("Most recent project - last modified {date}"),
        None => "Most recent project".to_string(),
    }
}

fn modification_date_label(path_str: &str) -> Option<String> {
    std::fs::metadata(path_str)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| {
            let secs = t.duration_since(std::time::UNIX_EPOCH).ok()?.as_secs();
            let days = secs / 86400;
            let (y, m, d) = crate::project_versions::days_to_ymd(days);
            Some(format!("{y:04}-{m:02}-{d:02}"))
        })
}

/// Format a unix timestamp as a human-readable age string (e.g. "2 hours ago").
fn format_autosave_age(saved_at: u64) -> String {
    let now = crate::project_versions::now_unix_secs();
    let diff = now.saturating_sub(saved_at);
    if diff < 60 {
        "just now".to_string()
    } else if diff < 3600 {
        let mins = diff / 60;
        format!("{mins} min ago")
    } else if diff < 86400 {
        let hours = diff / 3600;
        if hours == 1 {
            "1 hour ago".to_string()
        } else {
            format!("{hours} hours ago")
        }
    } else {
        let days = diff / 86400;
        if days == 1 {
            "1 day ago".to_string()
        } else {
            format!("{days} days ago")
        }
    }
}
