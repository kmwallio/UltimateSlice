use crate::ui_state::{
    clamp_prerender_crf, AutoScrollMode, CrossfadeCurve, GskRenderer, PlaybackPriority,
    PreferencesState, PrerenderEncodingPreset, PreviewQuality, ProxyMode, MAX_PRERENDER_CRF,
    MIN_PRERENDER_CRF,
};
use gtk4::prelude::*;
use gtk4::{
    self as gtk, Box as GBox, CheckButton, Label, Orientation, ResponseType, Separator, Stack,
    StackSidebar,
};
use std::cell::RefCell;
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
• fuzzy-matcher — MIT\n\
• whisper-rs — Unlicense\n\
• ort (ONNX Runtime) — MIT OR Apache-2.0\n\
• ndarray — MIT OR Apache-2.0\n\
• tokenizers (Hugging Face) — Apache-2.0\n\
• hound (WAV I/O) — Apache-2.0\n\
• resvg / usvg / tiny-skia — MIT OR Apache-2.0\n\
• tempfile — MIT OR Apache-2.0\n\
• FFmpeg (export/runtime tooling) — LGPL-2.1-or-later (built with GPL options in Flatpak)\n\
• x264 (Flatpak build) — GPL-2.0-or-later\n\
\n\
User-installed AI models (not bundled — see Models pane):\n\
• MODNet — photographic portrait matting (background removal). Apache-2.0\n\
• Whisper (GGML) — speech-to-text. MIT\n\
• MusicGen-small — text-to-music generation. CC-BY-NC-4.0 (research/non-commercial)\n\
• RIFE — Real-time Intermediate Flow Estimation, used for AI slow-motion frame interpolation. MIT\n\
\n\
See Cargo.toml/Cargo.lock and io.github.kmwallio.ultimateslice.yml for full dependency details.";

const LICENSE_NOTICE: &str = "\
UltimateSlice project license: GPL-3.0-or-later.\n\
\n\
This application uses third-party open-source crates and libraries.\n\
Please review each dependency license in Cargo.lock, the Flatpak manifest,\n\
and upstream project repositories for complete terms and notices.";

/// Returns true if a directory exists and contains at least one file.
fn dir_has_files(dir: &std::path::Path) -> bool {
    dir.is_dir()
        && std::fs::read_dir(dir)
            .map(|mut rd| rd.next().is_some())
            .unwrap_or(false)
}

/// Append a "Generated Files" section to the Models page if any generated
/// audio directories (voiceovers, AI music) contain files.
fn append_generated_files_section(container: &GBox) {
    let vo_dir = crate::media::voiceover::voiceover_cache_dir();
    let music_dir = crate::media::music_gen::music_gen_cache_dir();
    let has_vo = dir_has_files(&vo_dir);
    let has_music = dir_has_files(&music_dir);

    if !has_vo && !has_music {
        return;
    }

    container.append(&Separator::new(Orientation::Horizontal));

    let title = Label::new(Some("Generated Files"));
    title.set_halign(gtk::Align::Start);
    title.add_css_class("title-4");
    container.append(&title);

    if has_vo {
        let row = GBox::new(Orientation::Vertical, 2);
        let name = Label::new(Some("Voiceover Recordings"));
        name.set_halign(gtk::Align::Start);
        row.append(&name);

        let dir_str = vo_dir.to_string_lossy();
        let path_label = Label::new(None);
        path_label.set_markup(&format!(
            "<a href=\"file://{}\">{}</a>",
            glib::markup_escape_text(&dir_str),
            glib::markup_escape_text(&dir_str),
        ));
        path_label.set_halign(gtk::Align::Start);
        path_label.add_css_class("monospace");
        path_label.add_css_class("dim-label");
        row.append(&path_label);
        container.append(&row);
    }

    if has_music {
        let row = GBox::new(Orientation::Vertical, 2);
        let name = Label::new(Some("Generated Music"));
        name.set_halign(gtk::Align::Start);
        row.append(&name);

        let dir_str = music_dir.to_string_lossy();
        let path_label = Label::new(None);
        path_label.set_markup(&format!(
            "<a href=\"file://{}\">{}</a>",
            glib::markup_escape_text(&dir_str),
            glib::markup_escape_text(&dir_str),
        ));
        path_label.set_halign(gtk::Align::Start);
        path_label.add_css_class("monospace");
        path_label.add_css_class("dim-label");
        row.append(&path_label);
        container.append(&row);
    }
}

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

    // Wrap each stack page in a ScrolledWindow so long tabs (Models, Integration)
    // scroll in place instead of growing the dialog beyond the screen.
    let wrap_scroll = |child: &GBox| -> gtk::ScrolledWindow {
        let s = gtk::ScrolledWindow::new();
        s.set_vexpand(true);
        s.set_hexpand(true);
        s.set_hscrollbar_policy(gtk::PolicyType::Never);
        s.set_child(Some(child));
        s
    };

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
    stack.add_titled(&wrap_scroll(&general_box), Some("general"), "General");

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

    stack.add_titled(&wrap_scroll(&playback_box), Some("playback"), "Playback");

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

    stack.add_titled(&wrap_scroll(&proxies_box), Some("proxies"), "Proxies");

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
    let autoscroll_row = GBox::new(Orientation::Horizontal, 8);
    let autoscroll_label = Label::new(Some("Follow playhead during playback:"));
    autoscroll_label.set_halign(gtk::Align::Start);
    let autoscroll_combo = gtk4::ComboBoxText::new();
    autoscroll_combo.append(Some("page"), "Page");
    autoscroll_combo.append(Some("smooth"), "Smooth");
    autoscroll_combo.append(Some("off"), "Off");
    autoscroll_combo.set_active_id(Some(current.timeline_autoscroll.as_str()));
    autoscroll_combo.set_halign(gtk::Align::Start);
    autoscroll_row.append(&autoscroll_label);
    autoscroll_row.append(&autoscroll_combo);
    let autoscroll_hint = Label::new(Some(
        "Page jumps the view forward when the playhead reaches the right edge. Smooth slides the view to keep the playhead near the right. Off never moves the view automatically. In all modes, auto-scroll is paused briefly while you scroll or drag.",
    ));
    autoscroll_hint.set_halign(gtk::Align::Start);
    autoscroll_hint.add_css_class("dim-label");
    autoscroll_hint.set_wrap(true);
    autoscroll_hint.set_max_width_chars(60);
    timeline_box.append(&autoscroll_row);
    timeline_box.append(&autoscroll_hint);
    let source_monitor_auto_link_av_check =
        CheckButton::with_label("Auto-link source monitor A/V placements");
    source_monitor_auto_link_av_check.set_active(current.source_monitor_auto_link_av);
    source_monitor_auto_link_av_check.set_halign(gtk::Align::Start);
    let source_monitor_auto_link_av_hint = Label::new(Some(
        "When enabled, Append/Insert/Overwrite and timeline drag/drop place linked video+audio clips when matching tracks exist. The video clip is muted and audio comes from the dedicated audio clip.",
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

    // ── Loudness Radar target ─────────────────────────────────────────
    let loudness_header = Label::new(Some("Loudness target"));
    loudness_header.set_halign(gtk::Align::Start);
    loudness_header.add_css_class("title-5");
    timeline_box.append(&loudness_header);
    let loudness_preset_combo = gtk4::ComboBoxText::new();
    loudness_preset_combo.append(Some("ebu_r128"), "EBU R128 (−23 LUFS)");
    loudness_preset_combo.append(Some("atsc_a85"), "ATSC A/85 (−24 LUFS)");
    loudness_preset_combo.append(Some("netflix"), "Netflix (−27 LUFS)");
    loudness_preset_combo.append(Some("apple_pod"), "Apple Podcasts (−16 LUFS)");
    loudness_preset_combo.append(Some("streaming"), "Streaming (−14 LUFS)");
    loudness_preset_combo.append(Some("custom"), "Custom");
    loudness_preset_combo.set_active_id(Some(&current.loudness_target_preset));
    loudness_preset_combo.set_halign(gtk::Align::Start);
    let loudness_custom_spin = gtk4::SpinButton::with_range(-30.0, 0.0, 0.1);
    loudness_custom_spin.set_value(current.loudness_target_lufs);
    loudness_custom_spin.set_digits(1);
    loudness_custom_spin.set_halign(gtk::Align::Start);
    loudness_custom_spin.set_sensitive(current.loudness_target_preset == "custom");
    {
        let spin = loudness_custom_spin.clone();
        loudness_preset_combo.connect_changed(move |combo| {
            let id = combo
                .active_id()
                .unwrap_or_else(|| "ebu_r128".into())
                .to_string();
            spin.set_sensitive(id == "custom");
            if let Some(lufs) = crate::ui_state::loudness_target_preset_to_lufs(&id) {
                spin.set_value(lufs);
            }
        });
    }
    let loudness_hint = Label::new(Some(
        "Target for the Loudness Radar (Program Monitor → Loudness button). The matching LUFS value is applied when the user clicks Normalize to Target.",
    ));
    loudness_hint.set_halign(gtk::Align::Start);
    loudness_hint.add_css_class("dim-label");
    loudness_hint.set_wrap(true);
    loudness_hint.set_max_width_chars(60);
    timeline_box.append(&Label::new(Some("Target preset")));
    timeline_box.append(&loudness_preset_combo);
    timeline_box.append(&Label::new(Some("Custom target (LUFS)")));
    timeline_box.append(&loudness_custom_spin);
    timeline_box.append(&loudness_hint);

    // ── Voice enhance cache size cap ──
    let voice_enhance_cap_label = Label::new(Some("Voice enhance cache cap (GiB)"));
    voice_enhance_cap_label.set_halign(gtk::Align::Start);
    voice_enhance_cap_label.set_margin_top(12);
    let voice_enhance_cap_spin = gtk4::SpinButton::with_range(0.5, 50.0, 0.5);
    voice_enhance_cap_spin.set_value(current.voice_enhance_cache_cap_gib);
    voice_enhance_cap_spin.set_digits(1);
    voice_enhance_cap_spin.set_halign(gtk::Align::Start);
    let voice_enhance_cap_hint = Label::new(Some(
        "Soft cap on the per-user voice-enhance prerender cache. \
         When the cache exceeds this size, the least-recently-modified \
         files are deleted to make room. Default 2 GiB.",
    ));
    voice_enhance_cap_hint.set_halign(gtk::Align::Start);
    voice_enhance_cap_hint.add_css_class("dim-label");
    voice_enhance_cap_hint.set_wrap(true);
    voice_enhance_cap_hint.set_max_width_chars(60);
    timeline_box.append(&voice_enhance_cap_label);
    timeline_box.append(&voice_enhance_cap_spin);
    timeline_box.append(&voice_enhance_cap_hint);

    stack.add_titled(&wrap_scroll(&timeline_box), Some("timeline"), "Timeline");

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
    stack.add_titled(
        &wrap_scroll(&integration_box),
        Some("integration"),
        "Integration",
    );

    // Shared handle so the AI-backend dropdown (created inside the
    // Models page block below) can be read by the dialog response
    // handler further down when building the new PreferencesState.
    // Kept out here, outside the `#[cfg]` block, because the
    // response handler is always compiled and needs *something* to
    // reference — when `ai-inference` is off the handle just stays
    // `None` and the response handler falls back to the existing
    // value.
    #[allow(deprecated)]
    let ai_backend_combo_handle: Rc<RefCell<Option<gtk::ComboBoxText>>> =
        Rc::new(RefCell::new(None));
    let background_ai_indexing_check = CheckButton::with_label("AI index in background");
    background_ai_indexing_check.set_active(current.background_ai_indexing);
    background_ai_indexing_check.set_halign(gtk::Align::Start);

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

        // ── AI Acceleration ──────────────────────────────────────────────
        // User picks the ONNX Runtime execution backend used by every
        // AI-inference code path (MODNet, RIFE, MusicGen, SAM). Only
        // backends actually compiled into this binary are selectable;
        // the others are shown as disabled so the user can see what
        // the current build supports. "Auto" lets ort fall back to
        // whichever compiled-in backend loads at runtime.
        {
            use crate::media::ai_providers::{detect_backends, set_current_backend, AiBackend};
            let accel_label = Label::new(Some("AI Acceleration"));
            accel_label.set_halign(gtk::Align::Start);
            accel_label.add_css_class("heading");
            models_box.append(&accel_label);

            let report = detect_backends();

            let accel_row = GBox::new(Orientation::Horizontal, 8);
            let accel_name = Label::new(Some("Backend"));
            accel_name.set_halign(gtk::Align::Start);
            accel_name.set_hexpand(true);
            accel_row.append(&accel_name);

            // We use plain gtk::ComboBoxText here (matching the rest
            // of this Preferences dialog — DropDown would be nicer
            // but would require a different idiom). Populate with
            // only the compiled-in backends plus a leading Auto row.
            #[allow(deprecated)]
            let backend_combo = gtk::ComboBoxText::new();
            #[allow(deprecated)]
            {
                backend_combo.append(Some(AiBackend::Auto.as_id()), AiBackend::Auto.label());
                for b in [
                    AiBackend::Cuda,
                    AiBackend::Rocm,
                    AiBackend::OpenVino,
                    AiBackend::WebGpu,
                    AiBackend::Cpu,
                ] {
                    if report.compiled_in.contains(&b) {
                        let label = if report.runtime_available.contains(&b) {
                            b.label().to_string()
                        } else {
                            // Backend was compiled in but the
                            // runtime library isn't currently loadable
                            // (e.g. ai-cuda build running on a
                            // machine with no NVIDIA driver).
                            format!("{} (unavailable)", b.label())
                        };
                        backend_combo.append(Some(b.as_id()), &label);
                    }
                }
                let current_id = current.ai_backend.clone();
                backend_combo.set_active_id(Some(&current_id));
            }
            accel_row.append(&backend_combo);
            models_box.append(&accel_row);

            // Status line showing what was actually detected on this
            // machine, e.g. "Available: NVIDIA CUDA, CPU".
            let accel_status = Label::new(Some(&report.describe()));
            accel_status.set_halign(gtk::Align::Start);
            accel_status.add_css_class("dim-label");
            accel_status.set_wrap(true);
            accel_status.set_max_width_chars(60);
            models_box.append(&accel_status);

            let accel_hint = Label::new(Some(
                "Backend used for MODNet background removal, RIFE \
                 frame interpolation, MusicGen, and SAM segmentation. \
                 Changing this takes effect on the next inference job \
                 — no restart required. WebGPU is the recommended \
                 cross-vendor default (works on Intel Arc, AMD, and \
                 NVIDIA via Vulkan, with prebuilt binaries). CUDA uses \
                 prebuilts and needs only the CUDA toolkit. ROCm and \
                 OpenVINO give the best performance on AMD and Intel \
                 respectively but require a source-built ONNX Runtime \
                 — see docs/gpu/README.md for build instructions.",
            ));
            accel_hint.set_halign(gtk::Align::Start);
            accel_hint.add_css_class("dim-label");
            accel_hint.set_wrap(true);
            accel_hint.set_max_width_chars(60);
            models_box.append(&accel_hint);

            // Changing the dropdown takes effect on the process-wide
            // `ai_providers` singleton immediately — next inference
            // job picks it up without waiting for Save. The Save
            // handler below additionally reads the current selection
            // and persists it into PreferencesState so it survives
            // restart. (Cancel leaves the live singleton changed
            // but the persisted state untouched, which matches how
            // other "live preview" preference widgets behave.)
            #[allow(deprecated)]
            backend_combo.connect_changed(move |combo| {
                if let Some(id) = combo.active_id() {
                    let backend = AiBackend::from_id(id.as_str());
                    set_current_backend(backend);
                }
            });

            // Expose the combo to the dialog response handler via a
            // shared handle so Save can read the current selection.
            ai_backend_combo_handle.replace(Some(backend_combo));

            models_box.append(&Separator::new(Orientation::Horizontal));
        }

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

        models_box.append(&background_ai_indexing_check);

        let background_ai_indexing_hint = Label::new(Some(
            "Automatically builds transcript-search data for audio-backed library items after import/open when Whisper is available. Runs one clip at a time in the background and stays off by default.",
        ));
        background_ai_indexing_hint.set_halign(gtk::Align::Start);
        background_ai_indexing_hint.add_css_class("dim-label");
        background_ai_indexing_hint.set_wrap(true);
        background_ai_indexing_hint.set_max_width_chars(60);
        models_box.append(&background_ai_indexing_hint);

        // ── MusicGen model status ─────────────────────────────────────
        models_box.append(&Separator::new(Orientation::Horizontal));

        let musicgen_row = GBox::new(Orientation::Horizontal, 8);
        let musicgen_name = Label::new(Some("MusicGen (AI Music Generation)"));
        musicgen_name.set_halign(gtk::Align::Start);
        musicgen_name.set_hexpand(true);
        let musicgen_status = Label::new(None);
        musicgen_status.set_halign(gtk::Align::End);

        let has_musicgen = crate::media::music_gen::find_model_dir().is_some();
        if has_musicgen {
            musicgen_status.set_text("\u{2713} Installed");
            musicgen_status.add_css_class("success");
        } else {
            musicgen_status.set_text("Not installed");
            musicgen_status.add_css_class("dim-label");
        }
        musicgen_row.append(&musicgen_name);
        musicgen_row.append(&musicgen_status);
        models_box.append(&musicgen_row);

        let musicgen_hint = Label::new(None);
        musicgen_hint.set_markup(
            "MusicGen generates music from text prompts. \
             Download the ONNX models (~900 MB) from \
             <a href=\"https://huggingface.co/Xenova/musicgen-small/tree/main/onnx\">huggingface.co/Xenova/musicgen-small</a> \
             and place them in:",
        );
        musicgen_hint.set_halign(gtk::Align::Start);
        musicgen_hint.add_css_class("dim-label");
        musicgen_hint.set_wrap(true);
        musicgen_hint.set_max_width_chars(60);
        models_box.append(&musicgen_hint);

        let musicgen_dir = crate::media::music_gen::model_install_dir();
        let musicgen_dir_str = musicgen_dir.to_string_lossy();
        let musicgen_path_label = Label::new(None);
        musicgen_path_label.set_markup(&format!(
            "<a href=\"file://{}\">{}</a>",
            glib::markup_escape_text(&musicgen_dir_str),
            glib::markup_escape_text(&musicgen_dir_str),
        ));
        musicgen_path_label.set_halign(gtk::Align::Start);
        musicgen_path_label.add_css_class("monospace");
        models_box.append(&musicgen_path_label);

        let musicgen_files_hint = Label::new(Some(
            "Required files: text_encoder.onnx, decoder_model_merged.onnx,\n\
             encodec_decode.onnx, tokenizer.json",
        ));
        musicgen_files_hint.set_halign(gtk::Align::Start);
        musicgen_files_hint.add_css_class("dim-label");
        musicgen_files_hint.set_wrap(true);
        musicgen_files_hint.set_max_width_chars(60);
        models_box.append(&musicgen_files_hint);

        // ── RIFE frame interpolation model status ──────────────────────
        models_box.append(&Separator::new(Orientation::Horizontal));

        let rife_row = GBox::new(Orientation::Horizontal, 8);
        let rife_name = Label::new(Some("RIFE (AI Slow-Motion Frame Interpolation)"));
        rife_name.set_halign(gtk::Align::Start);
        rife_name.set_hexpand(true);
        let rife_status = Label::new(None);
        rife_status.set_halign(gtk::Align::End);

        let has_rife = crate::media::frame_interp_cache::find_model_path().is_some();
        if has_rife {
            rife_status.set_text("\u{2713} Installed");
            rife_status.add_css_class("success");
        } else {
            rife_status.set_text("Not installed");
            rife_status.add_css_class("dim-label");
        }
        rife_row.append(&rife_name);
        rife_row.append(&rife_status);
        models_box.append(&rife_row);

        let rife_hint = Label::new(None);
        rife_hint.set_markup(
            "RIFE synthesizes intermediate frames for high-quality \
             slow-motion. Enable on a clip via Inspector → Speed → \
             Slow-Motion Interpolation → AI Interpolation.\n\n\
             Practical-RIFE's official distribution ships PyTorch \
             <tt>flownet.pkl</tt> checkpoints, not ONNX — <tt>ort</tt> \
             can't load <tt>.pkl</tt> directly, so a one-time export \
             step is required. There's no official RIFE ONNX exporter; \
             you have two options:\n\n\
             • <b>Community pre-exported ONNX</b> (lowest friction): \
             search HuggingFace for \"rife onnx\" and find a matching \
             Practical-RIFE version (v4.14 / v4.18 / v4.22 etc. are \
             commonly available). Different versions have slightly \
             different architectures — use one that matches the \
             <tt>.pkl</tt> version you have.\n\n\
             • <b>Export from PyTorch yourself</b>: write a ~30-line \
             Python script using <tt>torch.onnx.export</tt> with \
             <tt>opset_version=17</tt> or higher (required for \
             <tt>grid_sample</tt> support, which RIFE uses). The \
             exporter must produce a 6-channel input (img0 RGB + \
             img1 RGB concatenated) plus a scalar timestep — the \
             existing UltimateSlice inference code expects this \
             shape.\n\n\
             See <a href=\"https://github.com/hzwer/Practical-RIFE\">github.com/hzwer/Practical-RIFE</a> \
             for the upstream project. Place the exported ONNX \
             file in:",
        );
        rife_hint.set_halign(gtk::Align::Start);
        rife_hint.add_css_class("dim-label");
        rife_hint.set_wrap(true);
        rife_hint.set_max_width_chars(60);
        models_box.append(&rife_hint);

        let rife_dir = crate::media::frame_interp_cache::model_install_dir();
        let rife_dir_str = rife_dir.to_string_lossy();
        let rife_path_label = Label::new(None);
        rife_path_label.set_markup(&format!(
            "<a href=\"file://{}\">{}</a>",
            glib::markup_escape_text(&rife_dir_str),
            glib::markup_escape_text(&rife_dir_str),
        ));
        rife_path_label.set_halign(gtk::Align::Start);
        rife_path_label.add_css_class("monospace");
        models_box.append(&rife_path_label);

        let rife_files_hint = Label::new(Some(
            "Accepted filenames: rife.onnx (preferred) or model.onnx. \
             The dropdown entry appears automatically once the file is \
             present — no restart required.",
        ));
        rife_files_hint.set_halign(gtk::Align::Start);
        rife_files_hint.add_css_class("dim-label");
        rife_files_hint.set_wrap(true);
        rife_files_hint.set_max_width_chars(60);
        models_box.append(&rife_files_hint);

        // ── Segment Anything 3.1 (Meta) model status ──────────────────
        // Phase 1 ships install detection + install instructions. The
        // "Generate with SAM" button in the Inspector that actually
        // consumes the model lands in Phase 2. This row gives users a
        // place to start downloading the model today so it's ready
        // when the button arrives.
        models_box.append(&Separator::new(Orientation::Horizontal));

        use crate::media::sam_cache;

        let sam_row = GBox::new(Orientation::Horizontal, 8);
        let sam_name = Label::new(Some(sam_cache::DISPLAY_NAME));
        sam_name.set_halign(gtk::Align::Start);
        sam_name.set_hexpand(true);
        let sam_status = Label::new(None);
        sam_status.set_halign(gtk::Align::End);

        let initial_status = sam_cache::install_status();
        // sam_status text / color class are set by the shared
        // render_sam_status closure below, which handles all four
        // SamInstallStatus variants consistently between the initial
        // render and the Refresh button re-render.
        sam_row.append(&sam_name);
        sam_row.append(&sam_status);
        models_box.append(&sam_row);

        let sam_hint = Label::new(None);
        sam_hint.set_markup(&format!(
            "Segment Anything 3.1 is Meta's unified detection + \
             segmentation + tracking model, used by the upcoming \
             interactive masking and object-tracking features. \
             {license}. See the upstream project at \
             <a href=\"{url}\">{url_display}</a> for checkpoint \
             downloads and ONNX export instructions. Place the \
             exported ONNX files in:",
            license = sam_cache::LICENSE_SUMMARY,
            url = sam_cache::UPSTREAM_URL,
            url_display = glib::markup_escape_text(sam_cache::UPSTREAM_URL),
        ));
        sam_hint.set_halign(gtk::Align::Start);
        sam_hint.add_css_class("dim-label");
        sam_hint.set_wrap(true);
        sam_hint.set_max_width_chars(60);
        models_box.append(&sam_hint);

        let sam_dir = sam_cache::model_install_dir();
        let sam_dir_str = sam_dir.to_string_lossy();
        let sam_path_label = Label::new(None);
        sam_path_label.set_markup(&format!(
            "<a href=\"file://{}\">{}</a>",
            glib::markup_escape_text(&sam_dir_str),
            glib::markup_escape_text(&sam_dir_str),
        ));
        sam_path_label.set_halign(gtk::Align::Start);
        sam_path_label.add_css_class("monospace");
        models_box.append(&sam_path_label);

        // Required-files hint, annotated with per-file status so a
        // user mid-download can see exactly what's still missing.
        let sam_files_hint = Label::new(None);
        sam_files_hint.set_halign(gtk::Align::Start);
        sam_files_hint.add_css_class("dim-label");
        sam_files_hint.add_css_class("monospace");
        sam_files_hint.set_wrap(true);
        sam_files_hint.set_max_width_chars(60);
        models_box.append(&sam_files_hint);

        // ── Export step block (conditionally visible) ─────────────
        // Shown when SAM detects a `.pt` checkpoint but no ONNX
        // files yet. Surfaces the exact pip-install + export command
        // the user needs to run, parameterized with the detected
        // checkpoint path. Hidden otherwise so the UI doesn't clutter
        // the clean "Installed" and "Not installed" states.
        let export_block = GBox::new(Orientation::Vertical, 6);
        export_block.set_margin_top(4);

        let export_heading = Label::new(Some("ONNX export step"));
        export_heading.set_halign(gtk::Align::Start);
        export_heading.add_css_class("heading");
        export_block.append(&export_heading);

        let export_explain = Label::new(None);
        export_explain.set_markup(&format!(
            "You downloaded the raw PyTorch checkpoint. <tt>ort</tt> can't \
             load <tt>.pt</tt> files directly, so you'll need to run a \
             one-time export to produce the three ONNX files above. The \
             recommended tool is \
             <a href=\"{url}\"><tt>{pip}</tt></a>:",
            url = sam_cache::EXPORTER_UPSTREAM_URL,
            pip = sam_cache::EXPORTER_PIP_NAME,
        ));
        export_explain.set_halign(gtk::Align::Start);
        export_explain.add_css_class("dim-label");
        export_explain.set_wrap(true);
        export_explain.set_max_width_chars(60);
        export_block.append(&export_explain);

        let export_command_label = Label::new(None);
        export_command_label.set_halign(gtk::Align::Start);
        export_command_label.add_css_class("monospace");
        export_command_label.set_selectable(true);
        export_command_label.set_wrap(true);
        export_command_label.set_max_width_chars(80);
        export_block.append(&export_command_label);

        let export_after_note = Label::new(Some(
            "Then click \"Refresh Status\" below — the ONNX files \
             will be detected automatically and the row will flip \
             to ✓ Installed. SAM 3.1's Object Multiplex variant is \
             new (March 2026); if samexporter rejects the checkpoint, \
             fall back to downloading the plain SAM 3 checkpoint and \
             exporting that instead.",
        ));
        export_after_note.set_halign(gtk::Align::Start);
        export_after_note.add_css_class("dim-label");
        export_after_note.set_wrap(true);
        export_after_note.set_max_width_chars(60);
        export_block.append(&export_after_note);

        models_box.append(&export_block);

        // Refresh button — users who install the model while the
        // Preferences dialog is open can re-probe without closing
        // and reopening.
        let sam_refresh_btn = gtk::Button::with_label("Refresh Status");
        sam_refresh_btn.set_halign(gtk::Align::Start);
        models_box.append(&sam_refresh_btn);

        // Shared status-rendering closure. Takes a `&SamInstallStatus`
        // and updates every widget on the row: status label text +
        // color class, per-file checklist, export-block visibility,
        // and (if applicable) the export command text interpolated
        // with the detected .pt path. Called once at init time and
        // again from the Refresh button click handler.
        let render_sam_status = {
            let sam_status = sam_status.clone();
            let sam_files_hint = sam_files_hint.clone();
            let export_block = export_block.clone();
            let export_command_label = export_command_label.clone();
            let sam_dir = sam_cache::model_install_dir();
            Rc::new(move |status: &sam_cache::SamInstallStatus| {
                sam_status.set_text(&status.short_label());
                sam_status.remove_css_class("success");
                sam_status.remove_css_class("warning");
                sam_status.remove_css_class("dim-label");
                match status {
                    sam_cache::SamInstallStatus::Installed => {
                        sam_status.add_css_class("success");
                    }
                    sam_cache::SamInstallStatus::Partial { .. }
                    | sam_cache::SamInstallStatus::PtCheckpointOnly { .. } => {
                        sam_status.add_css_class("warning");
                    }
                    sam_cache::SamInstallStatus::NotInstalled => {
                        sam_status.add_css_class("dim-label");
                    }
                }

                // Per-file ONNX checklist, rebuilt against the current
                // install dir so the Refresh button actually refreshes.
                let mut lines: Vec<String> = Vec::new();
                lines.push("Required files:".to_string());
                for filename in sam_cache::REQUIRED_FILES {
                    let marker = if sam_dir.join(filename).is_file() {
                        "✓"
                    } else {
                        "✗"
                    };
                    lines.push(format!("  {marker} {filename}"));
                }
                if let sam_cache::SamInstallStatus::Partial { missing } = status {
                    lines.push(format!(
                        "\n{} of {} files present — still need: {}",
                        sam_cache::REQUIRED_FILES.len() - missing.len(),
                        sam_cache::REQUIRED_FILES.len(),
                        missing.join(", ")
                    ));
                }
                sam_files_hint.set_text(&lines.join("\n"));

                // Export-block visibility + command interpolation.
                if let sam_cache::SamInstallStatus::PtCheckpointOnly { pt_path } = status {
                    export_block.set_visible(true);
                    let pt_display = pt_path.to_string_lossy();
                    let out_display = sam_dir.to_string_lossy();
                    let command = format!(
                        "pip install samexporter torch\n\
                         python -m samexporter.export_sam3 \\\n    \
                         --checkpoint {pt} \\\n    \
                         --output_dir {out} \\\n    \
                         --opset 18",
                        pt = pt_display,
                        out = out_display
                    );
                    export_command_label.set_text(&command);
                } else {
                    export_block.set_visible(false);
                }
            })
        };

        // Initial render.
        (render_sam_status)(&initial_status);

        // Wire up Refresh click → re-probe + re-render.
        let render_sam_status_click = render_sam_status.clone();
        sam_refresh_btn.connect_clicked(move |_| {
            let new_status = sam_cache::install_status();
            (render_sam_status_click)(&new_status);
        });

        append_generated_files_section(&models_box);

        stack.add_titled(&wrap_scroll(&models_box), Some("models"), "Models");
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

        models_box.append(&background_ai_indexing_check);

        let background_ai_indexing_hint = Label::new(Some(
            "Automatically builds transcript-search data for audio-backed library items after import/open when Whisper is available. Runs one clip at a time in the background and stays off by default.",
        ));
        background_ai_indexing_hint.set_halign(gtk::Align::Start);
        background_ai_indexing_hint.add_css_class("dim-label");
        background_ai_indexing_hint.set_wrap(true);
        background_ai_indexing_hint.set_max_width_chars(60);
        models_box.append(&background_ai_indexing_hint);

        // ── MusicGen model status ─────────────────────────────────────
        models_box.append(&Separator::new(Orientation::Horizontal));

        let musicgen_row = GBox::new(Orientation::Horizontal, 8);
        let musicgen_name = Label::new(Some("MusicGen (AI Music Generation)"));
        musicgen_name.set_halign(gtk::Align::Start);
        musicgen_name.set_hexpand(true);
        let musicgen_status = Label::new(None);
        musicgen_status.set_halign(gtk::Align::End);

        let has_musicgen = crate::media::music_gen::find_model_dir().is_some();
        if has_musicgen {
            musicgen_status.set_text("\u{2713} Installed");
            musicgen_status.add_css_class("success");
        } else {
            musicgen_status.set_text("Not installed");
            musicgen_status.add_css_class("dim-label");
        }
        musicgen_row.append(&musicgen_name);
        musicgen_row.append(&musicgen_status);
        models_box.append(&musicgen_row);

        let musicgen_hint = Label::new(None);
        musicgen_hint.set_markup(
            "MusicGen generates music from text prompts. \
             Download the ONNX models (~900 MB) from \
             <a href=\"https://huggingface.co/Xenova/musicgen-small/tree/main/onnx\">huggingface.co/Xenova/musicgen-small</a> \
             and place them in:",
        );
        musicgen_hint.set_halign(gtk::Align::Start);
        musicgen_hint.add_css_class("dim-label");
        musicgen_hint.set_wrap(true);
        musicgen_hint.set_max_width_chars(60);
        models_box.append(&musicgen_hint);

        let musicgen_dir = crate::media::music_gen::model_install_dir();
        let musicgen_dir_str = musicgen_dir.to_string_lossy();
        let musicgen_path_label = Label::new(None);
        musicgen_path_label.set_markup(&format!(
            "<a href=\"file://{}\">{}</a>",
            glib::markup_escape_text(&musicgen_dir_str),
            glib::markup_escape_text(&musicgen_dir_str),
        ));
        musicgen_path_label.set_halign(gtk::Align::Start);
        musicgen_path_label.add_css_class("monospace");
        models_box.append(&musicgen_path_label);

        let musicgen_files_hint = Label::new(Some(
            "Required files: text_encoder.onnx, decoder_model_merged.onnx,\n\
             encodec_decode.onnx, tokenizer.json",
        ));
        musicgen_files_hint.set_halign(gtk::Align::Start);
        musicgen_files_hint.add_css_class("dim-label");
        musicgen_files_hint.set_wrap(true);
        musicgen_files_hint.set_max_width_chars(60);
        models_box.append(&musicgen_files_hint);

        append_generated_files_section(&models_box);

        stack.add_titled(&wrap_scroll(&models_box), Some("models"), "Models");
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
                timeline_autoscroll: AutoScrollMode::from_str(
                    autoscroll_combo.active_id().as_deref().unwrap_or("page"),
                ),
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
                background_ai_indexing: background_ai_indexing_check.is_active(),
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
                loudness_target_preset: loudness_preset_combo
                    .active_id()
                    .map(|id| id.to_string())
                    .unwrap_or_else(|| "ebu_r128".to_string()),
                loudness_target_lufs: loudness_custom_spin.value(),
                voice_enhance_cache_cap_gib: voice_enhance_cap_spin.value().clamp(0.5, 50.0),
                // AI backend is edited via its own dropdown on the
                // Models page. If that dropdown exists (i.e. the
                // `ai-inference` feature is enabled *and* the Models
                // page constructed it), read its current selection;
                // otherwise fall back to whatever was in `current`
                // so we don't clobber a valid value.
                ai_backend: {
                    #[allow(deprecated)]
                    let combo_ref = ai_backend_combo_handle.borrow();
                    match combo_ref.as_ref().and_then(|c| c.active_id()) {
                        Some(id) => id.to_string(),
                        None => current.ai_backend.clone(),
                    }
                },
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
