use crate::ui_state::{
    clamp_prerender_crf, CrossfadeCurve, GskRenderer, PlaybackPriority, PreferencesState,
    PrerenderEncodingPreset, PreviewQuality, ProxyMode, MAX_PRERENDER_CRF, MIN_PRERENDER_CRF,
};
use gtk4::prelude::*;
use gtk4::{
    self as gtk, Box as GBox, CheckButton, Label, Orientation, ResponseType, Stack, StackSidebar,
};
use std::rc::Rc;

const THIRD_PARTY_COMPONENTS: &str = "\
Third-party crates and libraries:\n\
• gtk4-rs / gdk4 / gio / glib / pango — GTK/Pango runtime libraries LGPL-2.1-or-later\n\
• GStreamer + gstreamer-rs bindings — LGPL-2.1-or-later\n\
• quick-xml — MIT\n\
• serde / serde_json — MIT OR Apache-2.0\n\
• uuid — MIT OR Apache-2.0\n\
• anyhow / thiserror / log / env_logger — MIT OR Apache-2.0\n\
• rustfft — MIT OR Apache-2.0\n\
• whisper-rs — Unlicense\n\
• ort (ONNX Runtime) — MIT OR Apache-2.0\n\
• ndarray — MIT OR Apache-2.0\n\
• resvg / usvg / tiny-skia — MIT OR Apache-2.0\n\
• tempfile — MIT OR Apache-2.0\n\
• FFmpeg (export/runtime tooling) — LGPL-2.1-or-later (built with GPL options in Flatpak)\n\
• x264 (Flatpak build) — GPL-2.0-or-later\n\
\n\
See Cargo.toml/Cargo.lock and io.github.kmwallio.ultimateslice.yml for full dependency details.";

const LICENSE_NOTICE: &str = "\
UltimateSlice project license: GPL-3.0-or-later.\n\
\n\
This application uses third-party open-source crates and libraries.\n\
Please review each dependency license in Cargo.lock, the Flatpak manifest,\n\
and upstream project repositories for complete terms and notices.";

#[allow(deprecated)]
pub fn show_preferences_dialog(
    parent: &gtk::Window,
    current: PreferencesState,
    on_save: Rc<dyn Fn(PreferencesState)>,
    _bg_removal_cache: Rc<std::cell::RefCell<crate::media::bg_removal_cache::BgRemovalCache>>,
) {
    let dialog = gtk::Dialog::builder()
        .title("Preferences")
        .transient_for(parent)
        .modal(true)
        .default_width(640)
        .default_height(420)
        .build();
    dialog.add_button("Cancel", ResponseType::Cancel);
    dialog.add_button("Save", ResponseType::Accept);

    let body = GBox::new(Orientation::Horizontal, 0);
    body.set_margin_start(12);
    body.set_margin_end(12);
    body.set_margin_top(12);
    body.set_margin_bottom(12);

    let stack = Stack::new();
    stack.set_hexpand(true);
    stack.set_vexpand(true);
    stack.set_margin_start(12);
    stack.set_margin_end(8);
    stack.set_margin_top(4);
    stack.set_margin_bottom(4);

    let sidebar = StackSidebar::new();
    sidebar.set_stack(&stack);
    sidebar.set_margin_start(8);
    sidebar.set_margin_end(8);
    sidebar.set_margin_top(8);
    sidebar.set_margin_bottom(8);
    sidebar.set_vexpand(true);

    let general_box = GBox::new(Orientation::Vertical, 8);
    general_box.set_margin_start(8);
    general_box.set_margin_end(8);
    general_box.set_margin_top(8);
    let general_label = Label::new(Some("General preferences will appear here."));
    general_label.set_halign(gtk::Align::Start);
    general_box.append(&general_label);
    let about_btn = gtk::Button::with_label("About & Open-source credits");
    about_btn.set_halign(gtk::Align::Start);
    {
        let dialog_weak = dialog.downgrade();
        about_btn.connect_clicked(move |_| {
            let about = gtk::AboutDialog::builder()
                .program_name("UltimateSlice")
                .version(env!("CARGO_PKG_VERSION"))
                .comments(THIRD_PARTY_COMPONENTS)
                .license_type(gtk::License::Custom)
                .build();
            about.set_license(Some(LICENSE_NOTICE));
            about.set_authors(&["UltimateSlice contributors"]);
            about.set_website(Some("https://github.com/kmwallio/UltimateSlice"));
            if let Some(parent) = dialog_weak.upgrade() {
                about.set_transient_for(Some(&parent));
            }
            about.set_modal(true);
            about.present();
        });
    }
    // Backup settings
    let backup_label = Label::new(Some("Backup"));
    backup_label.set_halign(gtk::Align::Start);
    backup_label.add_css_class("title-4");
    general_box.append(&backup_label);

    let backup_enabled_check =
        gtk::CheckButton::with_label("Auto-backup (versioned copies every 60 s)");
    backup_enabled_check.set_active(current.backup_enabled);
    backup_enabled_check.set_halign(gtk::Align::Start);
    general_box.append(&backup_enabled_check);

    let max_versions_row = GBox::new(Orientation::Horizontal, 6);
    let max_versions_label = Label::new(Some("Max backup versions per project:"));
    max_versions_row.append(&max_versions_label);
    let backup_max_versions_spin = gtk::SpinButton::with_range(1.0, 100.0, 1.0);
    backup_max_versions_spin.set_value(current.backup_max_versions as f64);
    max_versions_row.append(&backup_max_versions_spin);
    general_box.append(&max_versions_row);

    general_box.append(&about_btn);
    stack.add_titled(&general_box, Some("general"), "General");

    let playback_box = GBox::new(Orientation::Vertical, 10);
    playback_box.set_margin_start(8);
    playback_box.set_margin_end(8);
    playback_box.set_margin_top(8);
    let playback_label = Label::new(Some("Playback / Performance"));
    playback_label.set_halign(gtk::Align::Start);
    playback_label.add_css_class("title-4");
    let hw_accel = CheckButton::with_label("Enable hardware acceleration");
    hw_accel.set_active(current.hardware_acceleration_enabled);
    hw_accel.set_halign(gtk::Align::Start);
    let playback_priority = gtk4::ComboBoxText::new();
    playback_priority.append(Some("smooth"), "Smooth (prioritize playback continuity)");
    playback_priority.append(Some("balanced"), "Balanced");
    playback_priority.append(
        Some("accurate"),
        "Accurate (prioritize seek/frame precision)",
    );
    playback_priority.set_active_id(Some(current.playback_priority.as_str()));
    playback_priority.set_halign(gtk::Align::Start);
    let source_playback_priority = gtk4::ComboBoxText::new();
    source_playback_priority.append(Some("smooth"), "Smooth (faster source seeks)");
    source_playback_priority.append(Some("balanced"), "Balanced");
    source_playback_priority.append(Some("accurate"), "Accurate (frame-precise source seeks)");
    source_playback_priority.set_active_id(Some(current.source_playback_priority.as_str()));
    source_playback_priority.set_halign(gtk::Align::Start);
    let hint = Label::new(Some(
        "Applies to source preview playback immediately (with non-GL fallback when needed).",
    ));
    hint.set_halign(gtk::Align::Start);
    hint.add_css_class("dim-label");
    let priority_hint = Label::new(Some("Program monitor playback priority controls smoothness vs frame precision during active playback."));
    priority_hint.set_halign(gtk::Align::Start);
    priority_hint.add_css_class("dim-label");
    playback_box.append(&playback_label);
    playback_box.append(&hw_accel);
    playback_box.append(&hint);
    playback_box.append(&Label::new(Some("Program monitor playback priority")));
    playback_box.append(&playback_priority);
    playback_box.append(&priority_hint);
    playback_box.append(&Label::new(Some("Source monitor playback priority")));
    playback_box.append(&source_playback_priority);

    let pq_label = Label::new(Some("Preview quality"));
    pq_label.set_halign(gtk::Align::Start);
    let preview_quality = gtk4::ComboBoxText::new();
    preview_quality.append(Some("auto"), "Auto (adapt to monitor size)");
    preview_quality.append(Some("full"), "Full (project resolution)");
    preview_quality.append(Some("half"), "Half (÷2 — lower memory)");
    preview_quality.append(Some("quarter"), "Quarter (÷4 — lowest memory)");
    preview_quality.set_active_id(Some(current.preview_quality.as_str()));
    preview_quality.set_halign(gtk::Align::Start);
    let pq_hint = Label::new(Some("Auto adapts quality to current Program Monitor size. Manual levels scale the compositor output for preview playback. Export always uses full resolution."));
    pq_hint.set_halign(gtk::Align::Start);
    pq_hint.add_css_class("dim-label");
    pq_hint.set_wrap(true);
    pq_hint.set_max_width_chars(60);
    playback_box.append(&pq_label);
    playback_box.append(&preview_quality);
    playback_box.append(&pq_hint);

    let renderer_label = Label::new(Some("GTK renderer"));
    renderer_label.set_halign(gtk::Align::Start);
    let gsk_renderer = gtk4::ComboBoxText::new();
    gsk_renderer.append(Some("auto"), "Auto (let GTK decide)");
    gsk_renderer.append(Some("cairo"), "Cairo (Software — no GPU memory)");
    gsk_renderer.append(Some("opengl"), "OpenGL (moderate GPU memory)");
    gsk_renderer.append(Some("vulkan"), "Vulkan (highest quality)");
    gsk_renderer.set_active_id(Some(current.gsk_renderer.as_str()));
    gsk_renderer.set_halign(gtk::Align::Start);
    let renderer_hint = Label::new(Some("Choose Cairo on devices with limited GPU memory to avoid Vulkan out-of-memory errors. Requires restart."));
    renderer_hint.set_halign(gtk::Align::Start);
    renderer_hint.add_css_class("dim-label");
    renderer_hint.set_wrap(true);
    renderer_hint.set_max_width_chars(60);
    playback_box.append(&renderer_label);
    playback_box.append(&gsk_renderer);
    playback_box.append(&renderer_hint);

    let experimental_check = CheckButton::with_label("Experimental preview optimizations");
    experimental_check.set_active(current.experimental_preview_optimizations);
    experimental_check.set_halign(gtk::Align::Start);
    let experimental_hint = Label::new(Some("Skip video decode for fully-occluded clips during multi-track preview playback. May reduce CPU/GPU usage at overlap boundaries."));
    experimental_hint.set_halign(gtk::Align::Start);
    experimental_hint.add_css_class("dim-label");
    experimental_hint.set_wrap(true);
    experimental_hint.set_max_width_chars(60);
    playback_box.append(&experimental_check);
    playback_box.append(&experimental_hint);

    let realtime_check = CheckButton::with_label("Real-time preview");
    realtime_check.set_active(current.realtime_preview);
    realtime_check.set_halign(gtk::Align::Start);
    let realtime_hint = Label::new(Some("Pre-builds upcoming decoder slots so clip transitions are near-instant. Uses more CPU and memory."));
    realtime_hint.set_halign(gtk::Align::Start);
    realtime_hint.add_css_class("dim-label");
    realtime_hint.set_wrap(true);
    realtime_hint.set_max_width_chars(60);
    playback_box.append(&realtime_check);
    playback_box.append(&realtime_hint);

    stack.add_titled(&playback_box, Some("playback"), "Playback");

    let proxies_box = GBox::new(Orientation::Vertical, 10);
    proxies_box.set_margin_start(8);
    proxies_box.set_margin_end(8);
    proxies_box.set_margin_top(8);
    let proxies_label = Label::new(Some("Proxies & Render Cache"));
    proxies_label.set_halign(gtk::Align::Start);
    proxies_label.add_css_class("title-4");
    let proxies_intro = Label::new(Some(
        "Configure proxy-generation quality plus where reusable proxy and prerender cache files live on disk.",
    ));
    proxies_intro.set_halign(gtk::Align::Start);
    proxies_intro.add_css_class("dim-label");
    proxies_intro.set_wrap(true);
    proxies_intro.set_max_width_chars(60);
    proxies_box.append(&proxies_label);
    proxies_box.append(&proxies_intro);

    let proxy_label = Label::new(Some("Proxy preview mode"));
    proxy_label.set_halign(gtk::Align::Start);
    let proxy_mode = gtk4::ComboBoxText::new();
    proxy_mode.append(Some("off"), "Off (use original media)");
    proxy_mode.append(Some("half_res"), "Half resolution");
    proxy_mode.append(Some("quarter_res"), "Quarter resolution");
    proxy_mode.set_active_id(Some(current.proxy_mode.as_str()));
    proxy_mode.set_halign(gtk::Align::Start);
    let proxy_hint = Label::new(Some(
        "Generate lightweight proxy files for smoother preview playback. Export always uses original media.",
    ));
    proxy_hint.set_halign(gtk::Align::Start);
    proxy_hint.add_css_class("dim-label");
    proxy_hint.set_wrap(true);
    proxy_hint.set_max_width_chars(60);
    proxies_box.append(&proxy_label);
    proxies_box.append(&proxy_mode);
    proxies_box.append(&proxy_hint);

    let persist_proxies_check = CheckButton::with_label("Persist proxies next to original media");
    persist_proxies_check.set_active(current.persist_proxies_next_to_original_media);
    persist_proxies_check.set_halign(gtk::Align::Start);
    let persist_proxies_hint = Label::new(Some(
        "When enabled, successful local proxy transcodes are mirrored into `UltimateSlice.cache/` beside the source file for reuse after reopen. If disabled, UltimateSlice prefers the managed local cache and only falls back beside media when needed.",
    ));
    persist_proxies_hint.set_halign(gtk::Align::Start);
    persist_proxies_hint.add_css_class("dim-label");
    persist_proxies_hint.set_wrap(true);
    persist_proxies_hint.set_max_width_chars(60);
    proxies_box.append(&persist_proxies_check);
    proxies_box.append(&persist_proxies_hint);

    let preview_luts_check = CheckButton::with_label("Preview LUTs (Proxy Off mode)");
    preview_luts_check.set_active(current.preview_luts);
    preview_luts_check.set_halign(gtk::Align::Start);
    let preview_luts_hint = Label::new(Some(
        "When Proxy mode is Off, render project-size LUT-baked preview media for LUT-assigned clips.",
    ));
    preview_luts_hint.set_halign(gtk::Align::Start);
    preview_luts_hint.add_css_class("dim-label");
    preview_luts_hint.set_wrap(true);
    preview_luts_hint.set_max_width_chars(60);
    proxies_box.append(&preview_luts_check);
    proxies_box.append(&preview_luts_hint);

    let background_prerender_check = CheckButton::with_label("Background prerender");
    background_prerender_check.set_active(current.background_prerender);
    background_prerender_check.set_halign(gtk::Align::Start);
    let background_prerender_hint = Label::new(Some(
        "Renders upcoming complex overlap sections (3+ video tracks) to disk in the background and uses them when available. Falls back to normal playback when unavailable.",
    ));
    background_prerender_hint.set_halign(gtk::Align::Start);
    background_prerender_hint.add_css_class("dim-label");
    background_prerender_hint.set_wrap(true);
    background_prerender_hint.set_max_width_chars(60);
    proxies_box.append(&background_prerender_check);
    proxies_box.append(&background_prerender_hint);

    let prerender_preset_label = Label::new(Some("Prerender encoding preset"));
    prerender_preset_label.set_halign(gtk::Align::Start);
    let prerender_preset = gtk4::ComboBoxText::new();
    prerender_preset.append(Some("ultrafast"), "Ultrafast (lowest CPU, largest files)");
    prerender_preset.append(Some("superfast"), "Superfast");
    prerender_preset.append(Some("veryfast"), "Veryfast (default)");
    prerender_preset.append(Some("faster"), "Faster");
    prerender_preset.append(Some("fast"), "Fast");
    prerender_preset.append(Some("medium"), "Medium (highest quality per byte)");
    prerender_preset.set_active_id(Some(current.prerender_preset.as_str()));
    prerender_preset.set_halign(gtk::Align::Start);
    let prerender_preset_hint = Label::new(Some(
        "Slower presets spend more CPU to improve compression efficiency for reusable prerender cache clips.",
    ));
    prerender_preset_hint.set_halign(gtk::Align::Start);
    prerender_preset_hint.add_css_class("dim-label");
    prerender_preset_hint.set_wrap(true);
    prerender_preset_hint.set_max_width_chars(60);
    proxies_box.append(&prerender_preset_label);
    proxies_box.append(&prerender_preset);
    proxies_box.append(&prerender_preset_hint);

    let prerender_crf_label = Label::new(Some("Prerender quality (CRF)"));
    prerender_crf_label.set_halign(gtk::Align::Start);
    let prerender_crf =
        gtk::SpinButton::with_range(MIN_PRERENDER_CRF as f64, MAX_PRERENDER_CRF as f64, 1.0);
    prerender_crf.set_digits(0);
    prerender_crf.set_numeric(true);
    prerender_crf.set_value(clamp_prerender_crf(current.prerender_crf) as f64);
    prerender_crf.set_halign(gtk::Align::Start);
    let prerender_crf_hint = Label::new(Some(
        "Lower CRF increases prerender fidelity and cache size. Default is 20; x264 supports 0-51.",
    ));
    prerender_crf_hint.set_halign(gtk::Align::Start);
    prerender_crf_hint.add_css_class("dim-label");
    prerender_crf_hint.set_wrap(true);
    prerender_crf_hint.set_max_width_chars(60);
    proxies_box.append(&prerender_crf_label);
    proxies_box.append(&prerender_crf);
    proxies_box.append(&prerender_crf_hint);

    let persist_prerenders_check =
        CheckButton::with_label("Persist prerenders next to project file");
    persist_prerenders_check.set_active(current.persist_prerenders_next_to_project_file);
    persist_prerenders_check.set_halign(gtk::Align::Start);
    let persist_prerenders_hint = Label::new(Some(
        "For saved projects, keep compatible prerender segments in a sibling `UltimateSlice.cache/prerender-vN/` directory beside the project file. If disabled, prerenders stay temporary and are not reused across restarts.",
    ));
    persist_prerenders_hint.set_halign(gtk::Align::Start);
    persist_prerenders_hint.add_css_class("dim-label");
    persist_prerenders_hint.set_wrap(true);
    persist_prerenders_hint.set_max_width_chars(60);
    proxies_box.append(&persist_prerenders_check);
    proxies_box.append(&persist_prerenders_hint);

    stack.add_titled(&proxies_box, Some("proxies"), "Proxies");

    // ── Timeline section ──────────────────────────────────────────────────
    let timeline_box = GBox::new(Orientation::Vertical, 10);
    timeline_box.set_margin_start(8);
    timeline_box.set_margin_end(8);
    timeline_box.set_margin_top(8);
    let timeline_label = Label::new(Some("Timeline"));
    timeline_label.set_halign(gtk::Align::Start);
    timeline_label.add_css_class("title-4");
    let waveform_video_check = CheckButton::with_label("Show audio waveforms on video clips");
    waveform_video_check.set_active(current.show_waveform_on_video);
    waveform_video_check.set_halign(gtk::Align::Start);
    let waveform_hint = Label::new(Some("Overlays color-coded audio waveforms on the lower portion of video clip tiles. Thumbnails remain visible above."));
    waveform_hint.set_halign(gtk::Align::Start);
    waveform_hint.add_css_class("dim-label");
    waveform_hint.set_wrap(true);
    waveform_hint.set_max_width_chars(60);
    timeline_box.append(&timeline_label);
    timeline_box.append(&waveform_video_check);
    timeline_box.append(&waveform_hint);
    let timeline_preview_check = CheckButton::with_label("Show timeline preview");
    timeline_preview_check.set_active(current.show_timeline_preview);
    timeline_preview_check.set_halign(gtk::Align::Start);
    let timeline_preview_hint = Label::new(Some("When enabled, video clips show a thumbnail strip. When disabled, only start/end thumbnails are shown."));
    timeline_preview_hint.set_halign(gtk::Align::Start);
    timeline_preview_hint.add_css_class("dim-label");
    timeline_preview_hint.set_wrap(true);
    timeline_preview_hint.set_max_width_chars(60);
    timeline_box.append(&timeline_preview_check);
    timeline_box.append(&timeline_preview_hint);
    let source_monitor_auto_link_av_check =
        CheckButton::with_label("Auto-link source monitor A/V placements");
    source_monitor_auto_link_av_check.set_active(current.source_monitor_auto_link_av);
    source_monitor_auto_link_av_check.set_halign(gtk::Align::Start);
    let source_monitor_auto_link_av_hint = Label::new(Some(
        "When enabled, Append/Insert/Overwrite places linked video+audio clips when matching tracks exist. The video clip is muted and audio comes from the dedicated audio clip.",
    ));
    source_monitor_auto_link_av_hint.set_halign(gtk::Align::Start);
    source_monitor_auto_link_av_hint.add_css_class("dim-label");
    source_monitor_auto_link_av_hint.set_wrap(true);
    source_monitor_auto_link_av_hint.set_max_width_chars(60);
    timeline_box.append(&source_monitor_auto_link_av_check);
    timeline_box.append(&source_monitor_auto_link_av_hint);
    let crossfade_enabled_check =
        CheckButton::with_label("Enable automatic audio crossfades at edit points");
    crossfade_enabled_check.set_active(current.crossfade_enabled);
    crossfade_enabled_check.set_halign(gtk::Align::Start);
    let crossfade_curve = gtk4::ComboBoxText::new();
    crossfade_curve.append(Some("equal_power"), "Equal power");
    crossfade_curve.append(Some("linear"), "Linear");
    crossfade_curve.set_active_id(Some(current.crossfade_curve.as_str()));
    crossfade_curve.set_halign(gtk::Align::Start);
    let crossfade_duration_ms = gtk4::SpinButton::with_range(10.0, 10_000.0, 10.0);
    crossfade_duration_ms.set_value((current.crossfade_duration_ns as f64) / 1_000_000.0);
    crossfade_duration_ms.set_digits(0);
    crossfade_duration_ms.set_halign(gtk::Align::Start);
    let crossfade_hint = Label::new(Some("Crossfades are used for adjacent audio edits. Equal power keeps perceived loudness more consistent. Duration is in milliseconds."));
    crossfade_hint.set_halign(gtk::Align::Start);
    crossfade_hint.add_css_class("dim-label");
    crossfade_hint.set_wrap(true);
    crossfade_hint.set_max_width_chars(60);
    timeline_box.append(&crossfade_enabled_check);
    timeline_box.append(&Label::new(Some("Crossfade curve")));
    timeline_box.append(&crossfade_curve);
    timeline_box.append(&Label::new(Some("Crossfade duration (ms)")));
    timeline_box.append(&crossfade_duration_ms);
    timeline_box.append(&crossfade_hint);
    stack.add_titled(&timeline_box, Some("timeline"), "Timeline");

    // ── Integration section ───────────────────────────────────────────────
    let integration_box = GBox::new(Orientation::Vertical, 10);
    integration_box.set_margin_start(8);
    integration_box.set_margin_end(8);
    integration_box.set_margin_top(8);
    let integration_label = Label::new(Some("Integration"));
    integration_label.set_halign(gtk::Align::Start);
    integration_label.add_css_class("title-4");
    let mcp_socket_check = CheckButton::with_label("Enable MCP socket server");
    mcp_socket_check.set_active(current.mcp_socket_enabled);
    mcp_socket_check.set_halign(gtk::Align::Start);
    let socket_path_str = crate::mcp::server::socket_path().display().to_string();
    let mcp_socket_hint = Label::new(Some(
        &format!("Allow AI agents to connect to this running instance via a Unix socket at {socket_path_str}"),
    ));
    mcp_socket_hint.set_halign(gtk::Align::Start);
    mcp_socket_hint.add_css_class("dim-label");
    mcp_socket_hint.set_wrap(true);
    mcp_socket_hint.set_max_width_chars(60);
    integration_box.append(&integration_label);
    integration_box.append(&mcp_socket_check);
    integration_box.append(&mcp_socket_hint);
    stack.add_titled(&integration_box, Some("integration"), "Integration");

    // ── Models section (only when ai-inference feature is enabled) ─────────
    #[cfg(feature = "ai-inference")]
    {
        use crate::media::bg_removal_cache::{
            find_model_path, model_download_dir, MODEL_DOWNLOAD_URL, MODEL_FILENAME,
        };

        let models_box = GBox::new(Orientation::Vertical, 10);
        models_box.set_margin_start(8);
        models_box.set_margin_end(8);
        models_box.set_margin_top(8);
        let models_label = Label::new(Some("Models"));
        models_label.set_halign(gtk::Align::Start);
        models_label.add_css_class("title-4");
        models_box.append(&models_label);

        // MODNet status row.
        let modnet_row = GBox::new(Orientation::Horizontal, 8);
        let modnet_name = Label::new(Some("MODNet (Background Removal)"));
        modnet_name.set_halign(gtk::Align::Start);
        modnet_name.set_hexpand(true);
        let status_label = Label::new(None);
        status_label.set_halign(gtk::Align::End);

        let has_model = find_model_path().is_some();
        if has_model {
            status_label.set_text("✓ Installed");
            status_label.add_css_class("success");
        } else {
            status_label.set_text("Not installed");
            status_label.add_css_class("dim-label");
        }
        modnet_row.append(&modnet_name);
        modnet_row.append(&status_label);
        models_box.append(&modnet_row);

        let modnet_hint = Label::new(Some(
            "MODNet is used for AI-powered background removal on video clips. \
             The model file (~25 MB) will be downloaded to your local data directory.",
        ));
        modnet_hint.set_halign(gtk::Align::Start);
        modnet_hint.add_css_class("dim-label");
        modnet_hint.set_wrap(true);
        modnet_hint.set_max_width_chars(60);
        models_box.append(&modnet_hint);

        // Download button + progress bar.
        let download_btn = gtk::Button::with_label(if has_model {
            "Re-download Model"
        } else {
            "Download Model"
        });
        download_btn.set_halign(gtk::Align::Start);
        let progress_bar = gtk::ProgressBar::new();
        progress_bar.set_visible(false);
        progress_bar.set_hexpand(true);

        let bg_cache = _bg_removal_cache.clone();
        let status_label_c = status_label.clone();
        let progress_bar_c = progress_bar.clone();
        let download_btn_c = download_btn.clone();
        download_btn.connect_clicked(move |_| {
            let dest_dir = model_download_dir();
            let _ = std::fs::create_dir_all(&dest_dir);
            let dest = dest_dir.join(MODEL_FILENAME);
            let partial = dest_dir.join(format!("{MODEL_FILENAME}.partial"));
            let url = MODEL_DOWNLOAD_URL.to_string();

            progress_bar_c.set_visible(true);
            progress_bar_c.set_fraction(0.0);
            progress_bar_c.set_text(Some("Downloading…"));
            progress_bar_c.set_show_text(true);
            download_btn_c.set_sensitive(false);
            status_label_c.set_text("Downloading…");

            // result: None = still running, Some(true) = success, Some(false) = failure.
            let result = std::sync::Arc::new(std::sync::Mutex::new(None::<bool>));
            let result_w = result.clone();
            std::thread::spawn(move || {
                let ok = std::process::Command::new("curl")
                    .args(["-L", "-o", &partial.to_string_lossy(), &url])
                    .status()
                    .map(|s| s.success())
                    .unwrap_or(false);
                if ok {
                    let _ = std::fs::rename(&partial, &dest);
                } else {
                    let _ = std::fs::remove_file(&partial);
                }
                *result_w.lock().unwrap() = Some(ok);
            });

            let bg_cache_c = bg_cache.clone();
            let status_label_cc = status_label_c.clone();
            let progress_bar_cc = progress_bar_c.clone();
            let download_btn_cc = download_btn_c.clone();
            glib::timeout_add_local(std::time::Duration::from_millis(500), move || {
                let done = result.lock().unwrap().clone();
                match done {
                    None => glib::ControlFlow::Continue,
                    Some(true) => {
                        progress_bar_cc.set_fraction(1.0);
                        progress_bar_cc.set_text(Some("Done"));
                        status_label_cc.set_text("✓ Installed");
                        download_btn_cc.set_label("Re-download Model");
                        download_btn_cc.set_sensitive(true);
                        bg_cache_c.borrow_mut().refresh_model_path();
                        glib::ControlFlow::Break
                    }
                    Some(false) => {
                        progress_bar_cc.set_text(Some("Download failed"));
                        progress_bar_cc.set_fraction(0.0);
                        status_label_cc.set_text("Download failed");
                        download_btn_cc.set_sensitive(true);
                        glib::ControlFlow::Break
                    }
                }
            });
        });
        models_box.append(&download_btn);
        models_box.append(&progress_bar);

        // ── Whisper STT model status ─────────────────────────────────────
        models_box.append(&Separator::new(Orientation::Horizontal));

        let stt_row = GBox::new(Orientation::Horizontal, 8);
        let stt_name = Label::new(Some("Whisper (Speech-to-Text)"));
        stt_name.set_halign(gtk::Align::Start);
        stt_name.set_hexpand(true);
        let stt_status = Label::new(None);
        stt_status.set_halign(gtk::Align::End);

        let has_stt = crate::media::stt_cache::find_stt_model_path().is_some();
        if has_stt {
            stt_status.set_text("✓ Installed");
            stt_status.add_css_class("success");
        } else {
            stt_status.set_text("Not installed");
            stt_status.add_css_class("dim-label");
        }
        stt_row.append(&stt_name);
        stt_row.append(&stt_status);
        models_box.append(&stt_row);

        let stt_hint = Label::new(None);
        stt_hint.set_markup(
            "Whisper is used for speech-to-text subtitle generation. \
             Download a GGML model (e.g. ggml-base.en.bin, ~150 MB) from \
             <a href=\"https://huggingface.co/ggerganov/whisper.cpp/tree/main\">huggingface.co/ggerganov/whisper.cpp</a> \
             and place it in:",
        );
        stt_hint.set_halign(gtk::Align::Start);
        stt_hint.add_css_class("dim-label");
        stt_hint.set_wrap(true);
        stt_hint.set_max_width_chars(60);
        models_box.append(&stt_hint);

        let stt_model_dir = crate::media::stt_cache::stt_model_dir();
        let stt_dir_str = stt_model_dir.to_string_lossy();
        let stt_path_label = Label::new(None);
        stt_path_label.set_markup(&format!(
            "<a href=\"file://{}\">{}</a>",
            glib::markup_escape_text(&stt_dir_str),
            glib::markup_escape_text(&stt_dir_str),
        ));
        stt_path_label.set_halign(gtk::Align::Start);
        stt_path_label.add_css_class("monospace");
        models_box.append(&stt_path_label);

        stack.add_titled(&models_box, Some("models"), "Models");
    }

    // When ai-inference is NOT enabled, still show a Models tab with STT info.
    #[cfg(not(feature = "ai-inference"))]
    {
        let models_box = GBox::new(Orientation::Vertical, 10);
        models_box.set_margin_start(8);
        models_box.set_margin_end(8);
        models_box.set_margin_top(8);
        let models_label = Label::new(Some("Models"));
        models_label.set_halign(gtk::Align::Start);
        models_label.add_css_class("title-4");
        models_box.append(&models_label);

        let stt_row = GBox::new(Orientation::Horizontal, 8);
        let stt_name = Label::new(Some("Whisper (Speech-to-Text)"));
        stt_name.set_halign(gtk::Align::Start);
        stt_name.set_hexpand(true);
        let stt_status = Label::new(None);
        stt_status.set_halign(gtk::Align::End);
        let has_stt = crate::media::stt_cache::find_stt_model_path().is_some();
        if has_stt {
            stt_status.set_text("✓ Installed");
            stt_status.add_css_class("success");
        } else {
            stt_status.set_text("Not installed");
            stt_status.add_css_class("dim-label");
        }
        stt_row.append(&stt_name);
        stt_row.append(&stt_status);
        models_box.append(&stt_row);

        let stt_hint = Label::new(None);
        stt_hint.set_markup(
            "Whisper is used for speech-to-text subtitle generation. \
             Download a GGML model (e.g. ggml-base.en.bin, ~150 MB) from \
             <a href=\"https://huggingface.co/ggerganov/whisper.cpp/tree/main\">huggingface.co/ggerganov/whisper.cpp</a> \
             and place it in:",
        );
        stt_hint.set_halign(gtk::Align::Start);
        stt_hint.add_css_class("dim-label");
        stt_hint.set_wrap(true);
        stt_hint.set_max_width_chars(60);
        models_box.append(&stt_hint);

        let stt_model_dir = crate::media::stt_cache::stt_model_dir();
        let stt_dir_str = stt_model_dir.to_string_lossy();
        let stt_path_label = Label::new(None);
        stt_path_label.set_markup(&format!(
            "<a href=\"file://{}\">{}</a>",
            glib::markup_escape_text(&stt_dir_str),
            glib::markup_escape_text(&stt_dir_str),
        ));
        stt_path_label.set_halign(gtk::Align::Start);
        stt_path_label.add_css_class("monospace");
        models_box.append(&stt_path_label);

        stack.add_titled(&models_box, Some("models"), "Models");
    }

    body.append(&sidebar);
    body.append(&stack);
    dialog.content_area().append(&body);

    dialog.connect_response(move |d, resp| {
        if resp == ResponseType::Accept {
            let mut new_state = PreferencesState {
                hardware_acceleration_enabled: hw_accel.is_active(),
                playback_priority: PlaybackPriority::from_str(
                    playback_priority.active_id().as_deref().unwrap_or("smooth"),
                ),
                source_playback_priority: PlaybackPriority::from_str(
                    source_playback_priority
                        .active_id()
                        .as_deref()
                        .unwrap_or("smooth"),
                ),
                proxy_mode: current.proxy_mode.clone(),
                last_non_off_proxy_mode: current.last_non_off_proxy_mode.clone(),
                persist_proxies_next_to_original_media: persist_proxies_check.is_active(),
                show_waveform_on_video: waveform_video_check.is_active(),
                show_timeline_preview: timeline_preview_check.is_active(),
                source_monitor_auto_link_av: source_monitor_auto_link_av_check.is_active(),
                show_track_audio_levels: current.show_track_audio_levels,
                mcp_socket_enabled: mcp_socket_check.is_active(),
                gsk_renderer: GskRenderer::from_str(
                    gsk_renderer.active_id().as_deref().unwrap_or("auto"),
                ),
                preview_quality: PreviewQuality::from_str(
                    preview_quality.active_id().as_deref().unwrap_or("full"),
                ),
                experimental_preview_optimizations: experimental_check.is_active(),
                realtime_preview: realtime_check.is_active(),
                background_prerender: background_prerender_check.is_active(),
                prerender_preset: current.prerender_preset.clone(),
                prerender_crf: current.prerender_crf,
                persist_prerenders_next_to_project_file: persist_prerenders_check.is_active(),
                preview_luts: preview_luts_check.is_active(),
                crossfade_enabled: crossfade_enabled_check.is_active(),
                crossfade_curve: CrossfadeCurve::from_str(
                    crossfade_curve
                        .active_id()
                        .as_deref()
                        .unwrap_or("equal_power"),
                ),
                crossfade_duration_ns: (crossfade_duration_ms.value().round() as u64)
                    .saturating_mul(1_000_000),
                duck_enabled: current.duck_enabled,
                duck_amount_db: current.duck_amount_db,
                backup_enabled: backup_enabled_check.is_active(),
                backup_max_versions: backup_max_versions_spin.value() as usize,
            };
            new_state.set_proxy_mode(ProxyMode::from_str(
                proxy_mode.active_id().as_deref().unwrap_or("off"),
            ));
            new_state.set_prerender_quality(
                PrerenderEncodingPreset::from_str(
                    prerender_preset
                        .active_id()
                        .as_deref()
                        .unwrap_or("veryfast"),
                ),
                clamp_prerender_crf(
                    prerender_crf
                        .value()
                        .round()
                        .clamp(MIN_PRERENDER_CRF as f64, MAX_PRERENDER_CRF as f64)
                        as u32,
                ),
            );
            on_save(new_state);
        }
        d.close();
    });
    dialog.present();
}
