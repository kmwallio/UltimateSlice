use crate::ui_state::{GskRenderer, PlaybackPriority, PreferencesState, PreviewQuality, ProxyMode};
use gtk4::prelude::*;
use gtk4::{
    self as gtk, Box as GBox, CheckButton, Label, Orientation, ResponseType, Stack, StackSidebar,
};
use std::rc::Rc;

const THIRD_PARTY_COMPONENTS: &str = "\
Third-party crates and libraries:\n\
• gtk4-rs / gdk4 / gio / glib — LGPL-2.1-or-later\n\
• GStreamer + gstreamer-rs bindings — LGPL-2.1-or-later\n\
• quick-xml — MIT\n\
• serde / serde_json — MIT OR Apache-2.0\n\
• uuid — MIT OR Apache-2.0\n\
• anyhow / thiserror / log / env_logger — MIT OR Apache-2.0\n\
• rustfft — MIT OR Apache-2.0\n\
• ort (ONNX Runtime) — MIT OR Apache-2.0\n\
• ndarray — MIT OR Apache-2.0\n\
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

    let proxy_label = Label::new(Some("Proxy preview mode"));
    proxy_label.set_halign(gtk::Align::Start);
    let proxy_mode = gtk4::ComboBoxText::new();
    proxy_mode.append(Some("off"), "Off (use original media)");
    proxy_mode.append(Some("half_res"), "Half resolution");
    proxy_mode.append(Some("quarter_res"), "Quarter resolution");
    proxy_mode.set_active_id(Some(current.proxy_mode.as_str()));
    proxy_mode.set_halign(gtk::Align::Start);
    let proxy_hint = Label::new(Some("Generate lightweight proxy files for smoother preview playback. Export always uses original media."));
    proxy_hint.set_halign(gtk::Align::Start);
    proxy_hint.add_css_class("dim-label");
    proxy_hint.set_wrap(true);
    proxy_hint.set_max_width_chars(60);
    playback_box.append(&proxy_label);
    playback_box.append(&proxy_mode);
    playback_box.append(&proxy_hint);

    let preview_luts_check = CheckButton::with_label("Preview LUTs (Proxy Off mode)");
    preview_luts_check.set_active(current.preview_luts);
    preview_luts_check.set_halign(gtk::Align::Start);
    let preview_luts_hint = Label::new(Some("When Proxy mode is Off, render project-size LUT-baked preview media for LUT-assigned clips."));
    preview_luts_hint.set_halign(gtk::Align::Start);
    preview_luts_hint.add_css_class("dim-label");
    preview_luts_hint.set_wrap(true);
    preview_luts_hint.set_max_width_chars(60);
    playback_box.append(&preview_luts_check);
    playback_box.append(&preview_luts_hint);

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

    let background_prerender_check = CheckButton::with_label("Background prerender");
    background_prerender_check.set_active(current.background_prerender);
    background_prerender_check.set_halign(gtk::Align::Start);
    let background_prerender_hint = Label::new(Some("Renders upcoming complex overlap sections (3+ video tracks) to temporary disk clips in the background and uses them when available. Falls back to normal playback when unavailable."));
    background_prerender_hint.set_halign(gtk::Align::Start);
    background_prerender_hint.add_css_class("dim-label");
    background_prerender_hint.set_wrap(true);
    background_prerender_hint.set_max_width_chars(60);
    playback_box.append(&background_prerender_check);
    playback_box.append(&background_prerender_hint);

    stack.add_titled(&playback_box, Some("playback"), "Playback");

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

    // ── Models section — hidden until we have a self-hosted model with
    //    secure distribution (see ROADMAP.md). ──────────────────────────────
    // {
    //     use crate::media::bg_removal_cache::{find_model_path, model_download_dir, MODEL_DOWNLOAD_URL, MODEL_FILENAME};
    //     ... download UI ...
    //     stack.add_titled(&models_box, Some("models"), "Models");
    // }

    body.append(&sidebar);
    body.append(&stack);
    dialog.content_area().append(&body);

    dialog.connect_response(move |d, resp| {
        if resp == ResponseType::Accept {
            on_save(PreferencesState {
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
                proxy_mode: ProxyMode::from_str(proxy_mode.active_id().as_deref().unwrap_or("off")),
                show_waveform_on_video: waveform_video_check.is_active(),
                show_timeline_preview: timeline_preview_check.is_active(),
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
                preview_luts: preview_luts_check.is_active(),
            });
        }
        d.close();
    });
    dialog.present();
}
