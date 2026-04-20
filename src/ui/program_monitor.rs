use crate::media::program_player::{ProgramPlayer, ScopeFrame};
use crate::media::reference_still::DecodedStill;
use crate::model::project::{FrameRate, TimecodeBurninPosition};
use crate::ui::colors::{LUMA_B, LUMA_G, LUMA_R};
use crate::ui::timecode;
use crate::ui_state::AspectMaskPreset;

/// Discrete zoom levels for the program monitor zoom in/out buttons.
const PROGRAM_MONITOR_ZOOM_LEVELS: &[f64] = &[0.25, 0.5, 0.75, 1.0, 1.5, 2.0, 3.0, 4.0];
const PROGRAM_MONITOR_CANVAS_BASE_CSS_CLASSES: &[&str] = &["preview-video"];
use gtk4::prelude::*;
use gtk4::{
    self as gtk, AspectFrame, Box as GBox, Button, CheckButton, DrawingArea, DropDown,
    EventControllerScroll, EventControllerScrollFlags, FlowBox, FlowBoxChild, GestureClick,
    GestureDrag, Label, MenuButton, Orientation, Overlay, Picture, Popover, ScrolledWindow,
    StringList,
};
use std::cell::{Cell, RefCell};
use std::rc::Rc;

/// Flat summary of a reference still shown in the Program Monitor popover
/// strip. UI-only; the canonical data lives in `Project::reference_stills`.
#[derive(Clone, Default)]
pub struct ReferenceStillSummary {
    pub id: String,
    pub label: String,
    /// Decoded thumbnail pixel data. `None` when the still is missing on disk
    /// (placeholder slot rendered instead).
    pub thumbnail: Option<Rc<DecodedStill>>,
}

/// Style info for a subtitle line in the program monitor overlay.
#[derive(Clone)]
pub struct SubtitleLine {
    /// Individual words with their active/inactive state.
    /// If non-empty, words are rendered individually with highlighting.
    /// If empty, `text` is rendered as a single block.
    pub words: Vec<SubtitleWordDisplay>,
    /// Fallback full text (used when words is empty).
    pub text: String,
    /// Text color as (r, g, b, a) 0.0–1.0.
    pub color: (f64, f64, f64, f64),
    /// Highlight color for the active word.
    pub highlight_color: (f64, f64, f64, f64),
    /// Independent stroke colour for the active-word stroke highlight.
    /// Defaults to `highlight_color` when the user hasn't picked one
    /// explicitly, so legacy behaviour (single shared colour) is
    /// preserved unless they go out of their way to set it.
    pub highlight_stroke_color: (f64, f64, f64, f64),
    /// Highlight flags (multi-effect).
    pub highlight_flags: crate::model::clip::SubtitleHighlightFlags,
    /// Outline color.
    pub outline_color: (f64, f64, f64, f64),
    /// Outline width in pts.
    pub outline_width: f64,
    /// Background box enabled.
    pub bg_box: bool,
    /// Background box color.
    pub bg_box_color: (f64, f64, f64, f64),
    /// Normalized font description used for preview rendering/fallback.
    pub font_desc: String,
    /// Vertical position: 0.0 (top) to 1.0 (bottom), mapped to the subtitle line's
    /// top/center/bottom anchor the same way export does.
    pub position_y: f64,
    /// Base style: bold applied to all text.
    pub subtitle_bold: bool,
    /// Base style: italic applied to all text.
    pub subtitle_italic: bool,
    /// Base style: underline applied to all text.
    pub subtitle_underline: bool,
    /// Base style: shadow applied to all text.
    pub subtitle_shadow: bool,
    /// Shadow color as (r, g, b, a) 0.0–1.0.
    pub subtitle_shadow_color: (f64, f64, f64, f64),
    /// Shadow offset (x, y).
    pub subtitle_shadow_offset: (f64, f64),
    /// Background highlight color for active word.
    pub bg_highlight_color: (f64, f64, f64, f64),
}

/// A single word to display, with active (highlighted) flag.
#[derive(Clone)]
pub struct SubtitleWordDisplay {
    pub text: String,
    pub active: bool,
}

impl Default for SubtitleLine {
    fn default() -> Self {
        Self {
            words: Vec::new(),
            text: String::new(),
            color: (1.0, 1.0, 1.0, 1.0),
            highlight_color: (1.0, 1.0, 0.0, 1.0),
            highlight_stroke_color: (1.0, 1.0, 0.0, 1.0),
            highlight_flags: crate::model::clip::SubtitleHighlightFlags::default(),
            outline_color: (0.0, 0.0, 0.0, 0.9),
            outline_width: 2.0,
            bg_box: true,
            bg_box_color: (0.0, 0.0, 0.0, 0.6),
            font_desc: crate::media::title_font::DEFAULT_SUBTITLE_FONT_DESC.to_string(),
            position_y: 0.85,
            subtitle_bold: false,
            subtitle_italic: false,
            subtitle_underline: false,
            subtitle_shadow: false,
            subtitle_shadow_color: (0.0, 0.0, 0.0, 0.667),
            subtitle_shadow_offset: (1.5, 1.5),
            bg_highlight_color: (1.0, 1.0, 0.0, 0.5),
        }
    }
}

fn cairo_slant_from_pango(style: pango::Style) -> gtk::cairo::FontSlant {
    match style {
        pango::Style::Italic => gtk::cairo::FontSlant::Italic,
        pango::Style::Oblique => gtk::cairo::FontSlant::Oblique,
        _ => gtk::cairo::FontSlant::Normal,
    }
}

fn cairo_weight_from_pango(weight: pango::Weight) -> gtk::cairo::FontWeight {
    match weight {
        pango::Weight::Semibold
        | pango::Weight::Bold
        | pango::Weight::Ultrabold
        | pango::Weight::Heavy
        | pango::Weight::Ultraheavy => gtk::cairo::FontWeight::Bold,
        _ => gtk::cairo::FontWeight::Normal,
    }
}

fn subtitle_preview_scale_factor(height: f64) -> f64 {
    (height / 1080.0).max(0.01)
}

fn subtitle_preview_outline_width(outline_width: f64, height: f64) -> f64 {
    let scaled = outline_width * subtitle_preview_scale_factor(height);
    if outline_width > 0.0 {
        scaled.max(1.0)
    } else {
        0.0
    }
}

fn subtitle_preview_box_padding(height: f64) -> (f64, f64, f64) {
    let scale = subtitle_preview_scale_factor(height);
    let pad_x = (8.0 * scale).clamp(2.0, 12.0);
    let pad_y = (4.0 * scale).clamp(1.0, 6.0);
    let radius = (4.0 * scale).clamp(1.0, 8.0);
    (pad_x, pad_y, radius)
}

fn subtitle_preview_underline_metrics(font_size: f64) -> (f64, f64) {
    let thickness = (font_size * 0.06).clamp(1.0, 4.0);
    let offset = (font_size * 0.12).clamp(1.0, 8.0);
    (thickness, offset)
}

fn subtitle_preview_stroke_width(height: f64) -> f64 {
    (4.0 * subtitle_preview_scale_factor(height)).max(1.0)
}

fn subtitle_preview_baseline_y(
    position_y: f64,
    canvas_height: f64,
    text_y_bearing: f64,
    text_height: f64,
) -> f64 {
    let pos_y = position_y.clamp(0.05, 0.95);
    let anchor_y = pos_y * canvas_height;
    if pos_y < 0.33 {
        anchor_y - text_y_bearing
    } else if pos_y < 0.66 {
        anchor_y - (text_y_bearing + text_height / 2.0)
    } else {
        anchor_y - (text_y_bearing + text_height)
    }
}

/// Snapshot of the values shown in the Program Monitor HUD overlay
/// (timecode, frame #, fps, canvas resolution, and cumulative dropped
/// frames). The caller supplies a getter closure so the HUD can reflect
/// whatever the current project / player state is without holding a
/// borrow on the heavier state objects.
#[derive(Clone)]
pub struct HudStats {
    pub playhead_ns: u64,
    pub frame_rate: FrameRate,
    pub width: u32,
    pub height: u32,
    pub dropped_frames: u64,
}

impl Default for HudStats {
    fn default() -> Self {
        Self {
            playhead_ns: 0,
            frame_rate: FrameRate {
                numerator: 24,
                denominator: 1,
            },
            width: 1920,
            height: 1080,
            dropped_frames: 0,
        }
    }
}

/// Transform parameters for a clip (crop, rotation, flip).
/// Kept here so other modules can reference it without a separate file.
#[derive(Clone, Copy, Default)]
pub struct ClipTransform {
    pub crop_left: i32,
    pub crop_right: i32,
    pub crop_top: i32,
    pub crop_bottom: i32,
    pub rotate: i32, // 0, 90, 180, 270
    pub flip_h: bool,
    pub flip_v: bool,
}

fn style_program_monitor_canvas_base(widget: &impl IsA<gtk::Widget>) {
    for class in PROGRAM_MONITOR_CANVAS_BASE_CSS_CLASSES {
        widget.add_css_class(class);
    }
}

/// Build the program monitor widget.
/// Returns `(widget, pos_label, speed_label, picture_a, picture_b, vu_meter, peak_cell, canvas_frame, safe_area_setter, false_color_setter, zebra_setter, hud_setter, hud_redraw, aspect_mask_setter, frame_updater, subtitle_text_setter)`.
/// - `safe_area_setter(enabled)` — toggle safe-area guide overlay.
/// - `false_color_setter(enabled)` — toggle false-color luminance overlay.
/// - `zebra_setter(enabled, threshold)` — toggle zebra overexposure overlay; threshold is 0.0–1.0.
/// - `hud_setter(enabled)` — toggle HUD overlay (timecode/frame/fps/resolution/dropped).
/// - `hud_redraw()` — request a HUD redraw; call from the position poll so the HUD ticks.
/// - `aspect_mask_setter(preset)` — select a delivery-format letterbox/pillarbox preset on the Program Monitor.
/// - `frame_updater(frame)` — push a new 320×180 RGBA scope frame; triggers overlay redraw.
/// - `subtitle_text_setter(lines)` — set current subtitle lines for overlay display with per-clip styling.
pub fn build_program_monitor(
    _program_player: Rc<RefCell<ProgramPlayer>>,
    paintable_a: gdk4::Paintable,
    paintable_b: gdk4::Paintable,
    canvas_width: u32,
    canvas_height: u32,
    on_stop: impl Fn() + 'static,
    on_play_pause: impl Fn() + 'static,
    on_toggle_popout: impl Fn() + 'static,
    on_go_to_timecode: impl Fn() + 'static,
    transform_overlay_da: Option<DrawingArea>,
    initial_show_safe_areas: bool,
    on_safe_area_changed: impl Fn(bool) + 'static,
    initial_show_false_color: bool,
    on_false_color_changed: impl Fn(bool) + 'static,
    initial_show_zebra: bool,
    initial_zebra_threshold: f64,
    on_zebra_changed: impl Fn(bool, f64) + 'static,
    initial_show_hud: bool,
    on_hud_changed: impl Fn(bool) + 'static,
    hud_stats_getter: impl Fn() -> HudStats + 'static,
    initial_aspect_mask: AspectMaskPreset,
    on_aspect_mask_changed: impl Fn(AspectMaskPreset) + 'static,
    initial_timecode_burnin_enabled: bool,
    initial_timecode_burnin_position: TimecodeBurninPosition,
    on_timecode_burnin_changed: impl Fn(bool, TimecodeBurninPosition) + 'static,
    // Optional extra button to append to the Program Monitor header
    // controls row (e.g. the Loudness Radar popover toggle). When `None`
    // the header looks exactly as before.
    extra_header_button: Option<gtk::Widget>,
    // A/B compare wipe parameters (Program Monitor polish: reference-still
    // pin + split-view). The wipe is off by default; when enabled, the
    // active reference still is painted on the right side of the vertical
    // midline over the live preview. Midline position is a percent (0..100)
    // of the canvas width. See docs/ROADMAP.md → "Program Monitor polish".
    initial_ab_enabled: bool,
    initial_ab_midline: f64,
    initial_stills_summary: Vec<ReferenceStillSummary>,
    initial_active_still_id: Option<String>,
    on_ab_enabled_changed: impl Fn(bool) + 'static,
    on_ab_midline_changed: impl Fn(f64) + 'static,
    on_capture_still: impl Fn() + 'static,
    on_select_still: impl Fn(Option<String>) + 'static,
    on_delete_still: impl Fn(String) + 'static,
    on_rename_still: impl Fn(String, String) + 'static,
) -> (
    GBox,
    Label,
    Label,
    Picture,
    Picture,
    DrawingArea,
    Rc<Cell<[f64; 2]>>,
    AspectFrame,
    Rc<dyn Fn(bool)>,
    Rc<dyn Fn(bool)>,
    Rc<dyn Fn(bool, f64)>,
    Rc<dyn Fn(bool)>,
    Rc<dyn Fn()>,
    Rc<dyn Fn(AspectMaskPreset)>,
    Rc<dyn Fn(ScopeFrame)>,
    Rc<dyn Fn(Vec<SubtitleLine>)>,
    // A/B setters — see Program Monitor polish section in ROADMAP.
    Rc<dyn Fn(bool)>,                                        // ab_enabled_setter
    Rc<dyn Fn(f64)>,                                         // ab_midline_setter
    Rc<dyn Fn(Option<Rc<DecodedStill>>)>,                    // ab_reference_setter
    Rc<dyn Fn(Vec<ReferenceStillSummary>, Option<String>)>, // stills_strip_setter
    // Timecode burn-in setter. Called on project load + after the
    // Project Settings dialog writes new values so the overlay
    // reflects the stored state. Monitor keeps its own shared cell
    // for the draw_func; window.rs owns persistence (FCPXML + model).
    Rc<dyn Fn(bool, TimecodeBurninPosition)>,
) {
    let root = GBox::new(Orientation::Vertical, 0);
    root.set_hexpand(true);
    root.set_vexpand(true);
    root.add_css_class("preview-panel");

    // Header
    let title_bar = GBox::new(Orientation::Vertical, 4);
    title_bar.add_css_class("preview-header");
    title_bar.set_margin_start(8);
    title_bar.set_margin_end(8);
    title_bar.set_margin_top(4);
    title_bar.set_margin_bottom(4);

    let status_row = GBox::new(Orientation::Horizontal, 8);
    status_row.set_hexpand(true);

    let label = Label::new(Some("Program Monitor"));
    label.add_css_class("dim-label");
    status_row.append(&label);

    let spacer = gtk::Separator::new(Orientation::Horizontal);
    spacer.set_hexpand(true);
    status_row.append(&spacer);

    // J/K/L shuttle rate indicator — shown/hidden by window.rs.
    let speed_label = Label::new(None);
    speed_label.add_css_class("timecode");
    speed_label.set_visible(false);
    status_row.append(&speed_label);

    let pos_label = Label::new(Some("00:00:00:00"));
    pos_label.add_css_class("timecode");
    pos_label.set_width_chars(11);
    status_row.append(&pos_label);

    title_bar.append(&status_row);

    let controls_row = GBox::new(Orientation::Horizontal, 8);
    controls_row.set_hexpand(true);

    let btn_go_to = Button::with_label("Go To");
    btn_go_to.set_tooltip_text(Some("Jump playhead to a timecode"));
    btn_go_to.connect_clicked(move |_| on_go_to_timecode());
    controls_row.append(&btn_go_to);

    let btn_popout = Button::with_label("Pop Out / Dock");
    btn_popout.connect_clicked(move |_| on_toggle_popout());
    controls_row.append(&btn_popout);

    let on_safe_area_changed = Rc::new(on_safe_area_changed);
    let safe_area_btn = CheckButton::with_label("Safe Areas");
    safe_area_btn.set_active(initial_show_safe_areas);

    let on_false_color_changed = Rc::new(on_false_color_changed);
    let false_color_btn = CheckButton::with_label("False Color");
    false_color_btn.set_active(initial_show_false_color);
    false_color_btn.set_tooltip_text(Some(
        "False color overlay: maps luminance to a color spectrum for exposure evaluation",
    ));

    let on_zebra_changed = Rc::new(on_zebra_changed);
    let zebra_btn = CheckButton::with_label("Zebra");
    zebra_btn.set_active(initial_show_zebra);
    zebra_btn.set_tooltip_text(Some(
        "Zebra stripes: diagonal lines on regions exceeding the exposure threshold (default 90%)",
    ));

    let on_hud_changed = Rc::new(on_hud_changed);
    let hud_btn = CheckButton::with_label("HUD");
    hud_btn.set_active(initial_show_hud);
    hud_btn.set_tooltip_text(Some(
        "HUD overlay: timecode, frame number, fps, resolution, and dropped-frame count (Shift+H)",
    ));

    // Aspect-ratio mask preset dropdown — letterboxes/pillarboxes the
    // canvas to a delivery-format target. `None` disables the overlay.
    let on_aspect_mask_changed = Rc::new(on_aspect_mask_changed);
    let aspect_mask_label_row = GBox::new(Orientation::Horizontal, 6);
    let aspect_mask_label = Label::new(Some("Aspect mask"));
    aspect_mask_label.set_xalign(0.0);
    aspect_mask_label.set_hexpand(true);
    let aspect_mask_strings: Vec<&'static str> = AspectMaskPreset::ALL
        .iter()
        .map(|p| p.label())
        .collect();
    let aspect_mask_model = StringList::new(&aspect_mask_strings);
    let aspect_mask_dropdown = DropDown::new(Some(aspect_mask_model), None::<gtk::Expression>);
    aspect_mask_dropdown.set_tooltip_text(Some(
        "Preview a delivery-format aspect ratio by letterboxing the Program Monitor canvas",
    ));
    let initial_mask_index = AspectMaskPreset::ALL
        .iter()
        .position(|p| *p == initial_aspect_mask)
        .unwrap_or(0) as u32;
    aspect_mask_dropdown.set_selected(initial_mask_index);
    aspect_mask_label_row.append(&aspect_mask_label);
    aspect_mask_label_row.append(&aspect_mask_dropdown);

    // Timecode burn-in — draws a timecode pill over the program monitor
    // at the configured position. Persisted on the project so export
    // bakes it into output pixels via drawtext. Delivery specs frequently
    // require burned timecode for review masters.
    let on_timecode_burnin_changed = Rc::new(on_timecode_burnin_changed);
    let tc_burnin_header_row = GBox::new(Orientation::Horizontal, 6);
    let tc_burnin_label = Label::new(Some("Timecode burn-in"));
    tc_burnin_label.set_xalign(0.0);
    tc_burnin_label.set_hexpand(true);
    tc_burnin_label.add_css_class("dim-label");
    let tc_burnin_check = CheckButton::new();
    tc_burnin_check.set_active(initial_timecode_burnin_enabled);
    tc_burnin_check.set_tooltip_text(Some(
        "Render a timecode pill on the Program Monitor and burn it into exports",
    ));
    tc_burnin_header_row.append(&tc_burnin_label);
    tc_burnin_header_row.append(&tc_burnin_check);

    let tc_burnin_position_row = GBox::new(Orientation::Horizontal, 6);
    let tc_burnin_position_label = Label::new(Some("Position"));
    tc_burnin_position_label.set_xalign(0.0);
    tc_burnin_position_label.set_hexpand(true);
    let tc_burnin_pos_strings: Vec<&'static str> = TimecodeBurninPosition::ALL
        .iter()
        .map(|p| p.label())
        .collect();
    let tc_burnin_pos_model = StringList::new(&tc_burnin_pos_strings);
    let tc_burnin_dropdown = DropDown::new(Some(tc_burnin_pos_model), None::<gtk::Expression>);
    tc_burnin_dropdown.set_tooltip_text(Some("Where the timecode pill is drawn on the canvas"));
    let initial_tc_burnin_idx = TimecodeBurninPosition::ALL
        .iter()
        .position(|p| *p == initial_timecode_burnin_position)
        .unwrap_or(4) as u32;
    tc_burnin_dropdown.set_selected(initial_tc_burnin_idx);
    tc_burnin_position_row.append(&tc_burnin_position_label);
    tc_burnin_position_row.append(&tc_burnin_dropdown);

    // "Overlays" dropdown — pops up a small panel with the three check items.
    let overlays_popover_box = GBox::new(Orientation::Vertical, 4);
    overlays_popover_box.set_margin_top(8);
    overlays_popover_box.set_margin_bottom(8);
    overlays_popover_box.set_margin_start(12);
    overlays_popover_box.set_margin_end(12);
    let subtitle_overlay_btn = CheckButton::with_label("Subtitles");
    subtitle_overlay_btn.set_active(true);
    subtitle_overlay_btn
        .set_tooltip_text(Some("Show/hide subtitle overlay in the Program Monitor"));

    overlays_popover_box.append(&safe_area_btn);
    overlays_popover_box.append(&false_color_btn);
    overlays_popover_box.append(&zebra_btn);
    overlays_popover_box.append(&hud_btn);
    overlays_popover_box.append(&subtitle_overlay_btn);
    overlays_popover_box.append(&aspect_mask_label_row);
    overlays_popover_box.append(&tc_burnin_header_row);
    overlays_popover_box.append(&tc_burnin_position_row);

    // ── Reference stills section (A/B compare wipe) ──
    let ref_stills_separator = gtk::Separator::new(Orientation::Horizontal);
    ref_stills_separator.set_margin_top(6);
    ref_stills_separator.set_margin_bottom(2);
    overlays_popover_box.append(&ref_stills_separator);

    let ref_stills_header_row = GBox::new(Orientation::Horizontal, 6);
    let ref_stills_header_label = Label::new(Some("Reference stills"));
    ref_stills_header_label.set_xalign(0.0);
    ref_stills_header_label.set_hexpand(true);
    ref_stills_header_label.add_css_class("dim-label");
    let ab_compare_btn = CheckButton::with_label("A/B compare");
    ab_compare_btn.set_active(initial_ab_enabled);
    ab_compare_btn.set_tooltip_text(Some(
        "Toggle the vertical A/B wipe overlay between the live Program Monitor frame and the active reference still. Drag the midline to slide the wipe.",
    ));
    ref_stills_header_row.append(&ref_stills_header_label);
    ref_stills_header_row.append(&ab_compare_btn);
    overlays_popover_box.append(&ref_stills_header_row);

    let ref_stills_capture_row = GBox::new(Orientation::Horizontal, 6);
    let ref_stills_capture_btn = Button::with_label("📷 Capture current frame");
    ref_stills_capture_btn.set_tooltip_text(Some(
        "Capture the current Program Monitor frame as a reference still (up to 4 per project)",
    ));
    ref_stills_capture_btn.set_hexpand(true);
    ref_stills_capture_row.append(&ref_stills_capture_btn);
    overlays_popover_box.append(&ref_stills_capture_row);

    let ref_stills_empty_hint = Label::new(Some(
        "No reference stills yet — capture one to enable A/B compare.",
    ));
    ref_stills_empty_hint.set_xalign(0.0);
    ref_stills_empty_hint.add_css_class("dim-label");
    ref_stills_empty_hint.set_wrap(true);
    ref_stills_empty_hint.set_max_width_chars(30);
    ref_stills_empty_hint.set_margin_top(4);
    overlays_popover_box.append(&ref_stills_empty_hint);

    let ref_stills_strip = FlowBox::new();
    ref_stills_strip.set_selection_mode(gtk::SelectionMode::None);
    ref_stills_strip.set_homogeneous(false);
    ref_stills_strip.set_min_children_per_line(1);
    ref_stills_strip.set_max_children_per_line(4);
    ref_stills_strip.set_column_spacing(4);
    ref_stills_strip.set_row_spacing(4);
    ref_stills_strip.set_margin_top(4);
    ref_stills_strip.set_visible(!initial_stills_summary.is_empty());
    ref_stills_empty_hint.set_visible(initial_stills_summary.is_empty());
    overlays_popover_box.append(&ref_stills_strip);

    let overlays_popover = Popover::new();
    overlays_popover.set_child(Some(&overlays_popover_box));
    overlays_popover.set_autohide(true);

    let overlays_menu_btn = MenuButton::new();
    overlays_menu_btn.set_label("Overlays");
    overlays_menu_btn.set_popover(Some(&overlays_popover));
    overlays_menu_btn.set_tooltip_text(Some("Toggle Safe Areas, False Color, and Zebra overlays"));
    controls_row.append(&overlays_menu_btn);

    // Optional caller-provided extra header button (e.g. the Loudness
    // Radar popover toggle). Added right after the Overlays menu so the
    // audio + video monitoring controls sit together.
    if let Some(ref w) = extra_header_button {
        controls_row.append(w);
    }

    let controls_spacer = gtk::Separator::new(Orientation::Horizontal);
    controls_spacer.set_hexpand(true);
    controls_row.append(&controls_spacer);

    // Zoom controls: − / zoom% label / + / Fit
    // These are appended to the title bar AFTER we build apply_zoom (below), so we
    // defer the connections and store the label in a variable.
    let zoom_out_btn = Button::with_label("−");
    zoom_out_btn.set_tooltip_text(Some("Zoom out preview (Ctrl+Scroll)"));
    let zoom_label = Label::new(Some("100%"));
    zoom_label.set_width_chars(5);
    zoom_label.add_css_class("timecode");
    let zoom_in_btn = Button::with_label("+");
    zoom_in_btn.set_tooltip_text(Some("Zoom in preview (Ctrl+Scroll)"));
    let zoom_fit_btn = Button::with_label("Fit");
    zoom_fit_btn.set_tooltip_text(Some("Reset zoom to fit"));
    controls_row.append(&zoom_out_btn);
    controls_row.append(&zoom_label);
    controls_row.append(&zoom_in_btn);
    controls_row.append(&zoom_fit_btn);

    title_bar.append(&controls_row);
    root.append(&title_bar);

    // Video display: GtkOverlay composites picture_b as the bottom layer and
    // picture_a as the top layer. Opacities are updated each poll tick.
    let picture_a = Picture::new();
    picture_a.set_paintable(Some(&paintable_a));
    picture_a.set_hexpand(true);
    picture_a.set_vexpand(true);
    picture_a.set_halign(gtk::Align::Fill);
    picture_a.set_valign(gtk::Align::Fill);
    picture_a.set_can_shrink(true);
    picture_a.set_size_request(1, 1);
    picture_a.set_content_fit(gtk::ContentFit::Contain);
    picture_a.add_css_class("preview-video");
    picture_a.add_css_class("preview-video-overlay");

    let picture_b = Picture::new();
    picture_b.set_paintable(Some(&paintable_b));
    picture_b.set_hexpand(true);
    picture_b.set_vexpand(true);
    picture_b.set_halign(gtk::Align::Fill);
    picture_b.set_valign(gtk::Align::Fill);
    picture_b.set_can_shrink(true);
    picture_b.set_size_request(1, 1);
    picture_b.set_content_fit(gtk::ContentFit::Contain);
    picture_b.add_css_class("preview-video");
    picture_b.set_opacity(0.0); // hidden until a lower layer/transition is active

    // Inner overlay: composites picture_b (bottom) and picture_a (top).
    // The transform DA is NOT added here — it lives on the outer overlay so it can
    // draw handles outside the canvas boundary (e.g. when scale > 1.0).
    let overlay = Overlay::new();
    let overlay_base = GBox::new(Orientation::Vertical, 0);
    overlay_base.set_hexpand(true);
    overlay_base.set_vexpand(true);
    overlay_base.set_size_request(1, 1);
    // Keep a visible canvas inside the AspectFrame even when window.rs hides
    // both video layers on empty timelines to avoid showing stale decoded frames.
    style_program_monitor_canvas_base(&overlay_base);
    overlay.set_child(Some(&overlay_base));
    overlay.add_overlay(&picture_b);
    overlay.add_overlay(&picture_a);
    let safe_area_visible = Rc::new(Cell::new(initial_show_safe_areas));
    let safe_area_da = DrawingArea::new();
    safe_area_da.set_hexpand(true);
    safe_area_da.set_vexpand(true);
    safe_area_da.set_halign(gtk::Align::Fill);
    safe_area_da.set_valign(gtk::Align::Fill);
    safe_area_da.set_can_target(false);
    {
        let safe_area_visible = safe_area_visible.clone();
        safe_area_da.set_draw_func(move |_da, cr, width, height| {
            if !safe_area_visible.get() || width <= 0 || height <= 0 {
                return;
            }
            let w = width as f64;
            let h = height as f64;
            cr.set_source_rgba(1.0, 1.0, 1.0, 0.72);
            cr.set_line_width(1.0);
            for scale in [0.9_f64, 0.8_f64] {
                let rw = w * scale;
                let rh = h * scale;
                let x = (w - rw) * 0.5;
                let y = (h - rh) * 0.5;
                cr.rectangle(x, y, rw, rh);
                let _ = cr.stroke();
            }
        });
    }
    overlay.add_overlay(&safe_area_da);
    overlay.set_measure_overlay(&picture_b, false);
    overlay.set_measure_overlay(&picture_a, false);
    overlay.set_measure_overlay(&safe_area_da, false);

    // Shared scope frame for false-color and zebra draw_funcs (320×180 RGBA).
    // Updated via the returned frame_updater callback each poll tick.
    let overlay_frame: Rc<RefCell<Option<ScopeFrame>>> = Rc::new(RefCell::new(None));

    // False-color overlay: maps each pixel's luminance to a colour spectrum to
    // help evaluate exposure without guessing from a standard video image.
    let false_color_visible = Rc::new(Cell::new(initial_show_false_color));
    let false_color_da = DrawingArea::new();
    false_color_da.set_hexpand(true);
    false_color_da.set_vexpand(true);
    false_color_da.set_halign(gtk::Align::Fill);
    false_color_da.set_valign(gtk::Align::Fill);
    false_color_da.set_can_target(false);
    {
        let fc_visible = false_color_visible.clone();
        let fc_frame = overlay_frame.clone();
        false_color_da.set_draw_func(move |_da, cr, width, height| {
            if !fc_visible.get() || width <= 0 || height <= 0 {
                return;
            }
            let guard = fc_frame.borrow();
            let Some(ref frame) = *guard else { return };
            let fw = frame.width as f64;
            let fh = frame.height as f64;
            let sw = width as f64 / fw;
            let sh = height as f64 / fh;
            let data = &frame.data;
            for fy in 0..frame.height {
                for fx in 0..frame.width {
                    let base = (fy * frame.width + fx) * 4;
                    if base + 3 >= data.len() {
                        break;
                    }
                    let r = data[base] as f64 / 255.0;
                    let g = data[base + 1] as f64 / 255.0;
                    let b = data[base + 2] as f64 / 255.0;
                    let luma = LUMA_R * r + LUMA_G * g + LUMA_B * b;
                    let (fr, fg, fb) = false_color_luma(luma);
                    cr.set_source_rgb(fr, fg, fb);
                    cr.rectangle(fx as f64 * sw, fy as f64 * sh, sw + 0.5, sh + 0.5);
                    cr.fill().ok();
                }
            }
        });
    }
    overlay.add_overlay(&false_color_da);
    overlay.set_measure_overlay(&false_color_da, false);

    // Zebra overlay: diagonal yellow stripes on pixels above the threshold.
    let zebra_visible = Rc::new(Cell::new(initial_show_zebra));
    let zebra_threshold_cell = Rc::new(Cell::new(initial_zebra_threshold));
    let zebra_da = DrawingArea::new();
    zebra_da.set_hexpand(true);
    zebra_da.set_vexpand(true);
    zebra_da.set_halign(gtk::Align::Fill);
    zebra_da.set_valign(gtk::Align::Fill);
    zebra_da.set_can_target(false);
    {
        let z_visible = zebra_visible.clone();
        let z_thresh = zebra_threshold_cell.clone();
        let z_frame = overlay_frame.clone();
        zebra_da.set_draw_func(move |_da, cr, width, height| {
            if !z_visible.get() || width <= 0 || height <= 0 {
                return;
            }
            let guard = z_frame.borrow();
            let Some(ref frame) = *guard else { return };
            let threshold = z_thresh.get();
            let fw = frame.width as f64;
            let fh = frame.height as f64;
            let sw = width as f64 / fw;
            let sh = height as f64 / fh;
            let data = &frame.data;
            for fy in 0..frame.height {
                for fx in 0..frame.width {
                    let base = (fy * frame.width + fx) * 4;
                    if base + 3 >= data.len() {
                        break;
                    }
                    let r = data[base] as f64 / 255.0;
                    let g = data[base + 1] as f64 / 255.0;
                    let b = data[base + 2] as f64 / 255.0;
                    let luma = LUMA_R * r + LUMA_G * g + LUMA_B * b;
                    if luma >= threshold && (fx + fy) % 8 < 4 {
                        cr.set_source_rgba(1.0, 0.85, 0.0, 0.85);
                        cr.rectangle(fx as f64 * sw, fy as f64 * sh, sw + 0.5, sh + 0.5);
                        cr.fill().ok();
                    }
                }
            }
        });
    }
    overlay.add_overlay(&zebra_da);
    overlay.set_measure_overlay(&zebra_da, false);

    // HUD overlay: top-left info panel with timecode, frame #, fps, resolution,
    // and cumulative dropped-frame count. State lives in `hud_visible`, and the
    // `hud_stats_getter` closure is called each draw to pull fresh values.
    let hud_visible = Rc::new(Cell::new(initial_show_hud));
    let hud_stats_getter: Rc<dyn Fn() -> HudStats> = Rc::new(hud_stats_getter);
    let hud_da = DrawingArea::new();
    hud_da.set_hexpand(true);
    hud_da.set_vexpand(true);
    hud_da.set_halign(gtk::Align::Fill);
    hud_da.set_valign(gtk::Align::Fill);
    hud_da.set_can_target(false);
    {
        let hud_visible = hud_visible.clone();
        let hud_stats_getter = hud_stats_getter.clone();
        hud_da.set_draw_func(move |_da, cr, width, height| {
            if !hud_visible.get() || width <= 0 || height <= 0 {
                return;
            }
            let stats = hud_stats_getter();
            let nominal = timecode::nominal_fps(&stats.frame_rate).max(1);
            let fps_num = u128::from(stats.frame_rate.numerator.max(1));
            let fps_den = u128::from(stats.frame_rate.denominator.max(1));
            let total_frames =
                (u128::from(stats.playhead_ns) * fps_num) / (1_000_000_000u128 * fps_den);
            let fps_label = if stats.frame_rate.denominator <= 1 {
                format!("{}", nominal)
            } else {
                format!("{:.2}", stats.frame_rate.as_f64())
            };
            let lines = [
                format!(
                    "TC   {}",
                    timecode::format_ns_as_timecode(stats.playhead_ns, &stats.frame_rate)
                ),
                format!("FRM  {}", total_frames),
                format!("FPS  {}", fps_label),
                format!("RES  {}×{}", stats.width, stats.height),
                format!("DROP {}", stats.dropped_frames),
            ];

            let pad = 10.0;
            let line_h = 15.0;
            let font_size = 12.0;
            cr.select_font_face(
                "monospace",
                gtk::cairo::FontSlant::Normal,
                gtk::cairo::FontWeight::Normal,
            );
            cr.set_font_size(font_size);

            let mut text_w: f64 = 0.0;
            for line in &lines {
                if let Ok(ext) = cr.text_extents(line) {
                    if ext.width() > text_w {
                        text_w = ext.width();
                    }
                }
            }
            let box_w = text_w + pad * 2.0;
            let box_h = line_h * lines.len() as f64 + pad * 2.0;
            let x = 12.0;
            let y = 12.0;

            // Dark translucent rounded background.
            let radius = 6.0;
            cr.set_source_rgba(0.0, 0.0, 0.0, 0.62);
            cr.new_sub_path();
            cr.arc(
                x + box_w - radius,
                y + radius,
                radius,
                -std::f64::consts::FRAC_PI_2,
                0.0,
            );
            cr.arc(
                x + box_w - radius,
                y + box_h - radius,
                radius,
                0.0,
                std::f64::consts::FRAC_PI_2,
            );
            cr.arc(
                x + radius,
                y + box_h - radius,
                radius,
                std::f64::consts::FRAC_PI_2,
                std::f64::consts::PI,
            );
            cr.arc(
                x + radius,
                y + radius,
                radius,
                std::f64::consts::PI,
                3.0 * std::f64::consts::FRAC_PI_2,
            );
            cr.close_path();
            let _ = cr.fill();

            cr.set_source_rgba(1.0, 1.0, 1.0, 0.92);
            for (i, line) in lines.iter().enumerate() {
                let ty = y + pad + line_h * (i as f64 + 1.0) - 3.0;
                cr.move_to(x + pad, ty);
                let _ = cr.show_text(line);
            }
        });
    }
    overlay.add_overlay(&hud_da);
    overlay.set_measure_overlay(&hud_da, false);

    // Aspect-ratio mask overlay: darkens the canvas regions outside the
    // selected delivery-format target rectangle. When the active preset
    // is `None` (the default) the draw func early-outs so the overlay is
    // effectively free.
    let aspect_mask_preset: Rc<Cell<AspectMaskPreset>> = Rc::new(Cell::new(initial_aspect_mask));
    let aspect_mask_da = DrawingArea::new();
    aspect_mask_da.set_hexpand(true);
    aspect_mask_da.set_vexpand(true);
    aspect_mask_da.set_halign(gtk::Align::Fill);
    aspect_mask_da.set_valign(gtk::Align::Fill);
    aspect_mask_da.set_can_target(false);
    {
        let aspect_mask_preset = aspect_mask_preset.clone();
        aspect_mask_da.set_draw_func(move |_da, cr, width, height| {
            if width <= 0 || height <= 0 {
                return;
            }
            let Some(target_ratio) = aspect_mask_preset.get().ratio() else {
                return;
            };
            let w = width as f64;
            let h = height as f64;
            let canvas_ratio = w / h;

            // Compute the inner target-ratio rectangle centered in the canvas.
            // target_ratio > canvas_ratio → target is wider than canvas, so the
            //   inner rect fills the canvas width and is shorter → letterbox
            //   bars on top/bottom.
            // target_ratio < canvas_ratio → target is narrower than canvas, so
            //   the inner rect fills the canvas height and is narrower →
            //   pillarbox bars on left/right.
            let (inner_w, inner_h) = if target_ratio > canvas_ratio {
                (w, w / target_ratio)
            } else {
                (h * target_ratio, h)
            };
            let inner_x = (w - inner_w) * 0.5;
            let inner_y = (h - inner_h) * 0.5;

            // Skip drawing when the target effectively matches the canvas
            // (sub-pixel difference). Avoids a faint 1px guide line on the
            // same-ratio preset.
            if (inner_w - w).abs() < 1.0 && (inner_h - h).abs() < 1.0 {
                return;
            }

            // Fill the four letterbox/pillarbox bands with translucent black.
            // Using individual rectangles (not a cut-out path) keeps the draw
            // simple and avoids fill-rule gotchas across Cairo versions.
            cr.set_source_rgba(0.0, 0.0, 0.0, 0.75);
            if inner_y > 0.5 {
                cr.rectangle(0.0, 0.0, w, inner_y);
                let _ = cr.fill();
                cr.rectangle(0.0, inner_y + inner_h, w, h - (inner_y + inner_h));
                let _ = cr.fill();
            }
            if inner_x > 0.5 {
                cr.rectangle(0.0, inner_y, inner_x, inner_h);
                let _ = cr.fill();
                cr.rectangle(inner_x + inner_w, inner_y, w - (inner_x + inner_w), inner_h);
                let _ = cr.fill();
            }

            // 1px guide line around the target rect, matching the Safe Areas
            // overlay treatment so the two overlays read as a family.
            cr.set_source_rgba(1.0, 1.0, 1.0, 0.32);
            cr.set_line_width(1.0);
            cr.rectangle(inner_x, inner_y, inner_w, inner_h);
            let _ = cr.stroke();
        });
    }
    overlay.add_overlay(&aspect_mask_da);
    overlay.set_measure_overlay(&aspect_mask_da, false);

    // Timecode burn-in overlay — draws a rounded monospace pill at the
    // configured corner. The HUD getter already provides playhead ns +
    // frame rate which we reuse so the overlay stays in sync with the
    // HUD and exported burn-in (drawtext).
    let tc_burnin_enabled_state = Rc::new(Cell::new(initial_timecode_burnin_enabled));
    let tc_burnin_position_state: Rc<Cell<TimecodeBurninPosition>> =
        Rc::new(Cell::new(initial_timecode_burnin_position));
    let tc_burnin_da = DrawingArea::new();
    tc_burnin_da.set_hexpand(true);
    tc_burnin_da.set_vexpand(true);
    tc_burnin_da.set_halign(gtk::Align::Fill);
    tc_burnin_da.set_valign(gtk::Align::Fill);
    tc_burnin_da.set_can_target(false);
    {
        let enabled = tc_burnin_enabled_state.clone();
        let position = tc_burnin_position_state.clone();
        let stats_getter = hud_stats_getter.clone();
        tc_burnin_da.set_draw_func(move |_da, cr, width, height| {
            if !enabled.get() || width <= 0 || height <= 0 {
                return;
            }
            let stats = stats_getter();
            let text = timecode::format_ns_as_timecode(stats.playhead_ns, &stats.frame_rate);

            let w = width as f64;
            let h = height as f64;
            let short_edge = w.min(h);
            let font_size = (short_edge * 0.035).clamp(12.0, 28.0);
            let pad_x = font_size * 0.7;
            let pad_y = font_size * 0.35;
            let margin = (short_edge * 0.02).max(6.0);

            cr.select_font_face(
                "monospace",
                gtk::cairo::FontSlant::Normal,
                gtk::cairo::FontWeight::Bold,
            );
            cr.set_font_size(font_size);
            let ext = match cr.text_extents(&text) {
                Ok(e) => e,
                Err(_) => return,
            };
            let pill_w = ext.width() + pad_x * 2.0;
            let pill_h = font_size + pad_y * 2.0;
            let (x, y) = position.get().anchor(w, h, pill_w, pill_h, margin);

            let radius = (pill_h * 0.35).min(10.0);
            cr.set_source_rgba(0.0, 0.0, 0.0, 0.72);
            cr.new_sub_path();
            cr.arc(
                x + pill_w - radius,
                y + radius,
                radius,
                -std::f64::consts::FRAC_PI_2,
                0.0,
            );
            cr.arc(
                x + pill_w - radius,
                y + pill_h - radius,
                radius,
                0.0,
                std::f64::consts::FRAC_PI_2,
            );
            cr.arc(
                x + radius,
                y + pill_h - radius,
                radius,
                std::f64::consts::FRAC_PI_2,
                std::f64::consts::PI,
            );
            cr.arc(
                x + radius,
                y + radius,
                radius,
                std::f64::consts::PI,
                3.0 * std::f64::consts::FRAC_PI_2,
            );
            cr.close_path();
            let _ = cr.fill();

            cr.set_source_rgba(1.0, 1.0, 1.0, 0.95);
            let text_x = x + pad_x - ext.x_bearing();
            let text_y = y + pad_y - ext.y_bearing();
            cr.move_to(text_x, text_y);
            let _ = cr.show_text(&text);
        });
    }
    overlay.add_overlay(&tc_burnin_da);
    overlay.set_measure_overlay(&tc_burnin_da, false);

    // A/B compare overlay — vertical wipe painting the selected reference
    // still over the right-hand side of the canvas, with a draggable midline.
    // Shared state the draw_func + drag gesture both read:
    let ab_enabled: Rc<Cell<bool>> = Rc::new(Cell::new(initial_ab_enabled));
    let ab_midline: Rc<Cell<f64>> = Rc::new(Cell::new(initial_ab_midline.clamp(0.0, 100.0)));
    let ab_surface: Rc<RefCell<Option<gtk::cairo::ImageSurface>>> = Rc::new(RefCell::new(None));
    let ab_surface_dims: Rc<Cell<(i32, i32)>> = Rc::new(Cell::new((0, 0)));

    let ab_da = DrawingArea::new();
    ab_da.set_hexpand(true);
    ab_da.set_vexpand(true);
    ab_da.set_halign(gtk::Align::Fill);
    ab_da.set_valign(gtk::Align::Fill);
    // Accept input so the midline drag gesture receives events.
    ab_da.set_can_target(true);
    {
        let ab_enabled_draw = ab_enabled.clone();
        let ab_midline_draw = ab_midline.clone();
        let ab_surface_draw = ab_surface.clone();
        let ab_dims_draw = ab_surface_dims.clone();
        ab_da.set_draw_func(move |_da, cr, width, height| {
            if !ab_enabled_draw.get() || width <= 0 || height <= 0 {
                return;
            }
            let w = width as f64;
            let h = height as f64;
            let mid_frac = (ab_midline_draw.get() * 0.01).clamp(0.0, 1.0);
            let mid_x = (w * mid_frac).round();

            // Paint the reference still on the right half only, letterboxed into
            // the canvas so the aspect ratio is preserved even if the reference
            // was captured at a different resolution.
            if let Some(ref surface) = *ab_surface_draw.borrow() {
                let (sw, sh) = ab_dims_draw.get();
                if sw > 0 && sh > 0 {
                    cr.save().ok();
                    // Clip to the right strip.
                    cr.rectangle(mid_x, 0.0, w - mid_x, h);
                    let _ = cr.clip();
                    // Letterbox-fit the reference into the canvas.
                    let src_ratio = sw as f64 / sh as f64;
                    let dst_ratio = w / h;
                    let (draw_w, draw_h) = if src_ratio > dst_ratio {
                        (w, w / src_ratio)
                    } else {
                        (h * src_ratio, h)
                    };
                    let draw_x = (w - draw_w) * 0.5;
                    let draw_y = (h - draw_h) * 0.5;
                    cr.translate(draw_x, draw_y);
                    cr.scale(draw_w / sw as f64, draw_h / sh as f64);
                    let _ = cr.set_source_surface(surface, 0.0, 0.0);
                    cr.paint().ok();
                    cr.restore().ok();
                } else {
                    // Dimensions not yet recorded — draw a dim placeholder.
                    cr.set_source_rgba(0.05, 0.05, 0.05, 0.6);
                    cr.rectangle(mid_x, 0.0, w - mid_x, h);
                    let _ = cr.fill();
                }
            } else {
                // Compare is enabled but no reference is loaded. Dim the right
                // side so the user sees the feature is "armed" but empty.
                cr.set_source_rgba(0.05, 0.05, 0.05, 0.4);
                cr.rectangle(mid_x, 0.0, w - mid_x, h);
                let _ = cr.fill();
            }

            // Midline: thin black under, 1-px white on top, plus triangular
            // pointer tabs at the vertical centre for drag affordance.
            cr.set_line_width(3.0);
            cr.set_source_rgba(0.0, 0.0, 0.0, 0.85);
            cr.move_to(mid_x + 0.5, 0.0);
            cr.line_to(mid_x + 0.5, h);
            let _ = cr.stroke();
            cr.set_line_width(1.0);
            cr.set_source_rgba(1.0, 1.0, 1.0, 0.95);
            cr.move_to(mid_x + 0.5, 0.0);
            cr.line_to(mid_x + 0.5, h);
            let _ = cr.stroke();

            // Drag handle diamond at vertical center.
            let cy = h * 0.5;
            let r = 8.0_f64;
            cr.set_source_rgba(0.0, 0.0, 0.0, 0.85);
            cr.move_to(mid_x, cy - r);
            cr.line_to(mid_x + r, cy);
            cr.line_to(mid_x, cy + r);
            cr.line_to(mid_x - r, cy);
            cr.close_path();
            let _ = cr.fill_preserve();
            cr.set_source_rgba(1.0, 1.0, 1.0, 0.95);
            cr.set_line_width(1.0);
            let _ = cr.stroke();
        });
    }

    // Midline drag gesture — hit within ±8 px of the current midline grabs it.
    {
        let drag = GestureDrag::new();
        drag.set_button(1);
        let ab_enabled_drag = ab_enabled.clone();
        let ab_midline_drag = ab_midline.clone();
        let ab_da_inner = ab_da.clone();
        let on_ab_midline_changed = Rc::new(on_ab_midline_changed);

        // Per-drag state: (start_percent, total_width_px, active)
        let drag_state: Rc<Cell<(f64, f64, bool)>> = Rc::new(Cell::new((50.0, 1.0, false)));
        {
            let drag_state = drag_state.clone();
            let ab_enabled_drag = ab_enabled_drag.clone();
            let ab_midline_drag = ab_midline_drag.clone();
            let ab_da_inner = ab_da_inner.clone();
            drag.connect_drag_begin(move |gesture, sx, _sy| {
                if !ab_enabled_drag.get() {
                    drag_state.set((0.0, 1.0, false));
                    gesture.set_state(gtk::EventSequenceState::Denied);
                    return;
                }
                let w = ab_da_inner.width().max(1) as f64;
                let mid_x = w * (ab_midline_drag.get() * 0.01).clamp(0.0, 1.0);
                if (sx - mid_x).abs() > 10.0 {
                    drag_state.set((0.0, w, false));
                    gesture.set_state(gtk::EventSequenceState::Denied);
                    return;
                }
                drag_state.set((ab_midline_drag.get(), w, true));
                gesture.set_state(gtk::EventSequenceState::Claimed);
            });
        }
        {
            let drag_state = drag_state.clone();
            let ab_midline_drag = ab_midline_drag.clone();
            let ab_da_inner = ab_da_inner.clone();
            let on_ab_midline_changed = on_ab_midline_changed.clone();
            drag.connect_drag_update(move |_gesture, off_x, _off_y| {
                let (start, total_w, active) = drag_state.get();
                if !active || total_w <= 0.0 {
                    return;
                }
                let delta_pct = (off_x / total_w) * 100.0;
                let new_pct = (start + delta_pct).clamp(0.0, 100.0);
                ab_midline_drag.set(new_pct);
                ab_da_inner.queue_draw();
                on_ab_midline_changed(new_pct);
            });
        }
        {
            let drag_state = drag_state.clone();
            drag.connect_drag_end(move |_gesture, _off_x, _off_y| {
                let (_, w, _) = drag_state.get();
                drag_state.set((0.0, w, false));
            });
        }
        ab_da.add_controller(drag);
    }

    overlay.add_overlay(&ab_da);
    overlay.set_measure_overlay(&ab_da, false);

    // Subtitle overlay: displays current subtitle lines with per-clip styling.
    let subtitle_lines: Rc<RefCell<Vec<SubtitleLine>>> = Rc::new(RefCell::new(Vec::new()));
    let subtitle_visible: Rc<Cell<bool>> = Rc::new(Cell::new(true));
    let subtitle_da = DrawingArea::new();
    subtitle_da.set_hexpand(true);
    subtitle_da.set_vexpand(true);
    subtitle_da.set_halign(gtk::Align::Fill);
    subtitle_da.set_valign(gtk::Align::Fill);
    subtitle_da.set_can_target(false);
    {
        let sl = subtitle_lines.clone();
        let sv = subtitle_visible.clone();
        subtitle_da.set_draw_func(move |_da, cr, width, height| {
            if !sv.get() {
                return;
            }
            let guard = sl.borrow();
            if guard.is_empty() || width <= 0 || height <= 0 {
                return;
            }
            let w = width as f64;
            let h = height as f64;
            for line in guard.iter() {
                // Scale font: Pango pts → pixels (×4/3), then proportional to preview height.
                // Matches the export scaling: font_size * 4/3 * (out_h / 1080).
                let desc = pango::FontDescription::from_string(&line.font_desc);
                let base_size_points = if desc.size() > 0 {
                    desc.size() as f64 / pango::SCALE as f64
                } else {
                    24.0
                };
                let font_size = (base_size_points * (4.0 / 3.0) * h / 1080.0).clamp(10.0, 72.0);
                let face = desc
                    .family()
                    .map(|family| family.trim().to_string())
                    .filter(|family| !family.is_empty())
                    .unwrap_or_else(|| "Sans".to_string());
                let slant = cairo_slant_from_pango(desc.style());
                let weight = cairo_weight_from_pango(desc.weight());
                let face = if face.is_empty() {
                    "Sans".to_string()
                } else {
                    face
                };
                cr.select_font_face(face.as_str(), slant, weight);
                cr.set_font_size(font_size);

                // Build the display string and measure it.
                let display_text = if !line.words.is_empty() {
                    line.words
                        .iter()
                        .map(|w| w.text.as_str())
                        .collect::<Vec<_>>()
                        .join(" ")
                } else {
                    line.text.clone()
                };
                if display_text.is_empty() {
                    continue;
                }

                let te = cr
                    .text_extents(&display_text)
                    .unwrap_or_else(|_| cr.text_extents("M").unwrap());
                let ve = cr
                    .text_extents("Ag")
                    .unwrap_or_else(|_| cr.text_extents("M").unwrap());
                let text_y_bearing = ve.y_bearing().min(te.y_bearing());
                let text_height = ve.height().max(te.height());
                let tx = (w - te.width()) / 2.0 - te.x_bearing();
                let ty =
                    subtitle_preview_baseline_y(line.position_y, h, text_y_bearing, text_height);

                // Background box.
                if line.bg_box {
                    let (pad_x, pad_y, radius) = subtitle_preview_box_padding(h);
                    let (br, bg, bb, ba) = line.bg_box_color;
                    cr.set_source_rgba(br, bg, bb, ba);
                    let box_x = tx + te.x_bearing() - pad_x;
                    let box_y = ty + text_y_bearing - pad_y;
                    let box_w = te.width() + pad_x * 2.0;
                    let box_h = text_height + pad_y * 2.0;
                    cr.new_sub_path();
                    cr.arc(
                        box_x + box_w - radius,
                        box_y + radius,
                        radius,
                        -std::f64::consts::FRAC_PI_2,
                        0.0,
                    );
                    cr.arc(
                        box_x + box_w - radius,
                        box_y + box_h - radius,
                        radius,
                        0.0,
                        std::f64::consts::FRAC_PI_2,
                    );
                    cr.arc(
                        box_x + radius,
                        box_y + box_h - radius,
                        radius,
                        std::f64::consts::FRAC_PI_2,
                        std::f64::consts::PI,
                    );
                    cr.arc(
                        box_x + radius,
                        box_y + radius,
                        radius,
                        std::f64::consts::PI,
                        3.0 * std::f64::consts::FRAC_PI_2,
                    );
                    cr.close_path();
                    cr.fill().ok();
                }

                // Outline for the full text.
                if line.outline_width > 0.0 {
                    let (or, og, ob, oa) = line.outline_color;
                    let outline_width = subtitle_preview_outline_width(line.outline_width, h);
                    cr.set_source_rgba(or, og, ob, oa);
                    cr.set_line_width(outline_width);
                    let _ = cr.move_to(tx, ty);
                    cr.text_path(&display_text);
                    cr.stroke().ok();
                }

                // Render words individually with highlighting, or as a single block.
                if !line.words.is_empty() && !line.highlight_flags.is_none() {
                    let mut word_x = tx;
                    let space_w = cr
                        .text_extents(" ")
                        .map(|e| e.x_advance())
                        .unwrap_or(font_size * 0.3);
                    for (i, word) in line.words.iter().enumerate() {
                        if i > 0 {
                            word_x += space_w;
                        }
                        let we = cr
                            .text_extents(&word.text)
                            .unwrap_or_else(|_| cr.text_extents("M").unwrap());

                        // Base styles: shadow for all words
                        if line.subtitle_shadow {
                            let (sr, sg, sb, sa) = line.subtitle_shadow_color;
                            let (sox, soy) = line.subtitle_shadow_offset;
                            cr.set_source_rgba(sr, sg, sb, sa);
                            let _ = cr.move_to(word_x + sox, ty + soy);
                            let _ = cr.show_text(&word.text);
                        }

                        if word.active {
                            // Multi-flag highlight rendering for active word
                            let flags = &line.highlight_flags;

                            // Shadow highlight
                            if flags.shadow {
                                let (sr, sg, sb, sa) = line.subtitle_shadow_color;
                                cr.set_source_rgba(sr, sg, sb, sa);
                                let _ = cr.move_to(word_x + 2.0, ty + 2.0);
                                let _ = cr.show_text(&word.text);
                            }

                            // Background highlight
                            if flags.background {
                                let (bgr, bgg, bgb, bga) = line.bg_highlight_color;
                                cr.set_source_rgba(bgr, bgg, bgb, bga);
                                let pad = font_size * 0.1;
                                let _ = cr.rectangle(
                                    word_x - pad,
                                    ty - font_size + pad,
                                    we.x_advance() + pad * 2.0,
                                    font_size + pad,
                                );
                                cr.fill().ok();
                            }

                            // Stroke highlight — uses its own colour so
                            // the karaoke stroke can differ from the
                            // karaoke text fill (e.g. yellow text with
                            // black stroke). Falls back to the
                            // highlight colour for legacy projects.
                            if flags.stroke {
                                let (hr, hg, hb, ha) = line.highlight_stroke_color;
                                cr.set_source_rgba(hr, hg, hb, ha);
                                cr.set_line_width(subtitle_preview_stroke_width(h));
                                let _ = cr.move_to(word_x, ty);
                                cr.text_path(&word.text);
                                cr.stroke().ok();
                            }

                            // Bold highlight (faux bold via offset draw)
                            if flags.bold {
                                let (tr, tg, tb, ta) = line.color;
                                cr.set_source_rgba(tr, tg, tb, ta);
                                let _ = cr.move_to(word_x + 0.5, ty);
                                let _ = cr.show_text(&word.text);
                            }

                            // Color highlight
                            if flags.color {
                                let (hr, hg, hb, ha) = line.highlight_color;
                                cr.set_source_rgba(hr, hg, hb, ha);
                            } else {
                                let (tr, tg, tb, ta) = line.color;
                                cr.set_source_rgba(tr, tg, tb, ta);
                            }

                            // Draw main text
                            let _ = cr.move_to(word_x, ty);
                            let _ = cr.show_text(&word.text);

                            // Underline highlight
                            if flags.underline {
                                let (underline_thickness, underline_offset) =
                                    subtitle_preview_underline_metrics(font_size);
                                cr.set_line_width(underline_thickness);
                                let _ = cr.move_to(word_x, ty + underline_offset);
                                let _ = cr.line_to(word_x + we.x_advance(), ty + underline_offset);
                                cr.stroke().ok();
                            }
                        } else {
                            // Non-active word: base styles only
                            let (tr, tg, tb, ta) = line.color;
                            cr.set_source_rgba(tr, tg, tb, ta);

                            // Base bold
                            if line.subtitle_bold {
                                let _ = cr.move_to(word_x + 0.5, ty);
                                let _ = cr.show_text(&word.text);
                            }

                            let _ = cr.move_to(word_x, ty);
                            let _ = cr.show_text(&word.text);

                            // Base underline
                            if line.subtitle_underline {
                                let (underline_thickness, underline_offset) =
                                    subtitle_preview_underline_metrics(font_size);
                                cr.set_line_width(underline_thickness);
                                let _ = cr.move_to(word_x, ty + underline_offset);
                                let _ = cr.line_to(word_x + we.x_advance(), ty + underline_offset);
                                cr.stroke().ok();
                            }
                        }
                        word_x += we.x_advance();
                    }
                } else {
                    // Single-block rendering (no word-level highlight).

                    // Base shadow
                    if line.subtitle_shadow {
                        let (sr, sg, sb, sa) = line.subtitle_shadow_color;
                        let (sox, soy) = line.subtitle_shadow_offset;
                        cr.set_source_rgba(sr, sg, sb, sa);
                        let _ = cr.move_to(tx + sox, ty + soy);
                        let _ = cr.show_text(&display_text);
                    }

                    let (tr, tg, tb, ta) = line.color;
                    cr.set_source_rgba(tr, tg, tb, ta);

                    // Base bold (faux)
                    if line.subtitle_bold {
                        let _ = cr.move_to(tx + 0.5, ty);
                        let _ = cr.show_text(&display_text);
                    }

                    let _ = cr.move_to(tx, ty);
                    let _ = cr.show_text(&display_text);

                    // Base underline
                    if line.subtitle_underline {
                        let te = cr
                            .text_extents(&display_text)
                            .unwrap_or_else(|_| cr.text_extents("M").unwrap());
                        let (underline_thickness, underline_offset) =
                            subtitle_preview_underline_metrics(font_size);
                        cr.set_line_width(underline_thickness);
                        let _ = cr.move_to(tx, ty + underline_offset);
                        let _ = cr.line_to(tx + te.x_advance(), ty + underline_offset);
                        cr.stroke().ok();
                    }
                }
            }
        });
    }
    overlay.add_overlay(&subtitle_da);
    overlay.set_measure_overlay(&subtitle_da, false);

    overlay.set_hexpand(true);
    overlay.set_vexpand(true);

    // AspectFrame constrains the video display area to the canvas ratio (e.g. 16:9).
    // This makes ContentFit::Contain on picture_a/b letterbox/pillarbox clips whose
    // native aspect ratio differs from the canvas (e.g. a 21:9 clip on a 16:9 canvas).
    let canvas_ratio = if canvas_height > 0 {
        canvas_width as f32 / canvas_height as f32
    } else {
        16.0 / 9.0
    };
    let canvas_frame = AspectFrame::new(0.5, 0.5, canvas_ratio, false);
    canvas_frame.set_child(Some(&overlay));
    canvas_frame.set_hexpand(true);
    canvas_frame.set_vexpand(true);

    // Outer overlay: canvas_frame (primary child) + transform DA (overlay child).
    // The outer overlay fills the scroll viewport, giving the transform DA the full
    // viewport area to draw in. This means handles drawn outside the canvas boundary
    // (e.g. bounding box corners when scale > 1.0) are visible when the user zooms
    // out the program monitor. The canvas position computed by video_rect() in the
    // transform overlay matches canvas_frame's layout exactly, because AspectFrame
    // centres its child using the same letterbox geometry as video_rect().
    let outer_overlay = Overlay::new();
    outer_overlay.set_child(Some(&canvas_frame));
    outer_overlay.set_hexpand(true);
    outer_overlay.set_vexpand(true);
    // Keep a clone of the transform DA so apply_zoom can queue_draw() after each zoom.
    let transform_da_for_zoom: Option<DrawingArea> = transform_overlay_da.as_ref().cloned();
    if let Some(da) = transform_overlay_da {
        da.set_hexpand(true);
        da.set_vexpand(true);
        da.set_halign(gtk::Align::Fill);
        da.set_valign(gtk::Align::Fill);
        outer_overlay.add_overlay(&da);
    }

    let zoom_level: Rc<Cell<f64>> = Rc::new(Cell::new(1.0));
    // Natural (fit) size of the canvas_frame recorded the moment we first leave zoom=1.0.
    // At zoom=1.0 with hexpand=true the canvas_frame fills the scroll viewport exactly,
    // so canvas_frame.width()/height() at that instant is the correct "100%" baseline.
    let fit_w: Rc<Cell<i32>> = Rc::new(Cell::new(0));
    let fit_h: Rc<Cell<i32>> = Rc::new(Cell::new(0));

    let scroll = ScrolledWindow::new();
    scroll.set_policy(gtk::PolicyType::Automatic, gtk::PolicyType::Automatic);
    scroll.set_child(Some(&outer_overlay));
    scroll.set_hexpand(true);
    scroll.set_vexpand(true);

    // apply_zoom: when leaving zoom=1.0, records the natural canvas_frame size as baseline.
    // At non-1.0 zoom, disables hexpand/vexpand so the frame can grow beyond viewport.
    let zoom_levels: &[f64] = PROGRAM_MONITOR_ZOOM_LEVELS;
    let apply_zoom = {
        let canvas_frame = canvas_frame.clone();
        let scroll = scroll.clone();
        let zoom_level = zoom_level.clone();
        let fit_w = fit_w.clone();
        let fit_h = fit_h.clone();
        let transform_da_zoom = transform_da_for_zoom.clone();
        let safe_area_da = safe_area_da.clone();
        move |new_z: f64| {
            let z = zoom_levels
                .iter()
                .cloned()
                .fold(f64::INFINITY, |best, z| {
                    if (z - new_z).abs() < (best - new_z).abs() {
                        z
                    } else {
                        best
                    }
                })
                .clamp(0.25, 4.0);

            // When transitioning away from 1.0, snapshot the natural canvas_frame size.
            if (zoom_level.get() - 1.0).abs() < 0.01 && (z - 1.0).abs() > 0.01 {
                let mut fw = canvas_frame.width();
                let mut fh = canvas_frame.height();
                if fw <= 0 || fh <= 0 {
                    fw = scroll.width();
                    fh = scroll.height();
                }
                if fw <= 0 || fh <= 0 {
                    fw = canvas_width.max(1) as i32;
                    fh = canvas_height.max(1) as i32;
                }
                fit_w.set(fw);
                fit_h.set(fh);
            }
            zoom_level.set(z);

            if (z - 1.0).abs() < 0.01 {
                // Fit: let the canvas_frame expand naturally to fill the scroll viewport.
                canvas_frame.set_hexpand(true);
                canvas_frame.set_vexpand(true);
                canvas_frame.set_halign(gtk::Align::Fill);
                canvas_frame.set_valign(gtk::Align::Fill);
                canvas_frame.set_size_request(-1, -1);
            } else {
                let fw = fit_w.get();
                let fh = fit_h.get();
                if fw > 0 && fh > 0 {
                    canvas_frame.set_hexpand(false);
                    canvas_frame.set_vexpand(false);
                    canvas_frame.set_size_request((fw as f64 * z) as i32, (fh as f64 * z) as i32);
                    if z < 1.0 {
                        // Center the smaller canvas in the scroll viewport (no scrollbars).
                        canvas_frame.set_halign(gtk::Align::Center);
                        canvas_frame.set_valign(gtk::Align::Center);
                    } else {
                        // Allow content to overflow so ScrolledWindow can scroll.
                        canvas_frame.set_halign(gtk::Align::Fill);
                        canvas_frame.set_valign(gtk::Align::Fill);
                    }
                }
            }
            // Redraw the transform overlay so the canvas border/vignette
            // repositions immediately after any zoom change.
            if let Some(ref da) = transform_da_zoom {
                da.queue_draw();
            }
            safe_area_da.queue_draw();
        }
    };
    let apply_zoom = Rc::new(apply_zoom);

    // Ctrl+Scroll adjusts zoom
    {
        let az = apply_zoom.clone();
        let zoom_level = zoom_level.clone();
        let lbl = zoom_label.clone();
        let ctrl_scroll = EventControllerScroll::new(
            EventControllerScrollFlags::VERTICAL | EventControllerScrollFlags::DISCRETE,
        );
        ctrl_scroll.connect_scroll(move |ec, _dx, dy| {
            let mods = ec.current_event_state();
            if mods.contains(gdk4::ModifierType::CONTROL_MASK) {
                let step = if dy < 0.0 { 1_isize } else { -1_isize };
                let z = zoom_level.get();
                let idx = zoom_levels
                    .iter()
                    .position(|&l| (l - z).abs() < 0.01)
                    .unwrap_or(3);
                let new_idx =
                    (idx as isize + step).clamp(0, zoom_levels.len() as isize - 1) as usize;
                let new_z = zoom_levels[new_idx];
                az(new_z);
                lbl.set_label(&format!("{}%", (new_z * 100.0) as u32));
                return gtk4::glib::Propagation::Stop;
            }
            gtk4::glib::Propagation::Proceed
        });
        scroll.add_controller(ctrl_scroll);
    }

    root.append(&scroll);

    // Wire zoom buttons now that apply_zoom is defined
    {
        let az = apply_zoom.clone();
        let zl = zoom_level.clone();
        let lbl = zoom_label.clone();
        zoom_out_btn.connect_clicked(move |_| {
            let z = zl.get();
            let idx = PROGRAM_MONITOR_ZOOM_LEVELS
                .iter()
                .position(|&l| (l - z).abs() < 0.01)
                .unwrap_or(3);
            let new_idx = idx.saturating_sub(1);
            let new_z = PROGRAM_MONITOR_ZOOM_LEVELS[new_idx];
            az(new_z);
            lbl.set_label(&format!("{}%", (new_z * 100.0) as u32));
        });
    }
    {
        let az = apply_zoom.clone();
        let zl = zoom_level.clone();
        let lbl = zoom_label.clone();
        zoom_in_btn.connect_clicked(move |_| {
            let z = zl.get();
            let idx = PROGRAM_MONITOR_ZOOM_LEVELS
                .iter()
                .position(|&l| (l - z).abs() < 0.01)
                .unwrap_or(3);
            let new_idx = (idx + 1).min(PROGRAM_MONITOR_ZOOM_LEVELS.len() - 1);
            let new_z = PROGRAM_MONITOR_ZOOM_LEVELS[new_idx];
            az(new_z);
            lbl.set_label(&format!("{}%", (new_z * 100.0) as u32));
        });
    }
    {
        let az = apply_zoom.clone();
        let lbl = zoom_label.clone();
        zoom_fit_btn.connect_clicked(move |_| {
            az(1.0);
            lbl.set_label("100%");
        });
    }
    // Update zoom label from Ctrl+Scroll via zoom_level cell poll in size-allocate
    // is not needed — buttons update it directly. For Ctrl+Scroll we update in the
    // scroll handler above by re-reading zoom_level in the label callbacks below.

    // Transport controls
    let controls = GBox::new(Orientation::Horizontal, 8);
    controls.add_css_class("transport-bar");
    controls.set_halign(gtk::Align::Center);
    controls.set_margin_top(6);
    controls.set_margin_bottom(6);

    let btn_play = Button::with_label("▶ Play");
    btn_play.connect_clicked(move |_| on_play_pause());
    controls.append(&btn_play);

    let btn_stop = Button::with_label("■ Stop");
    btn_stop.connect_clicked(move |_| on_stop());
    controls.append(&btn_stop);

    root.append(&controls);

    // VU meter: two vertical bars (L/R) showing audio peak level in dBFS.
    let (vu_meter, peak_cell) = build_vu_meter();
    let vu_bar = GBox::new(Orientation::Horizontal, 0);
    vu_bar.set_halign(gtk::Align::End);
    vu_bar.set_valign(gtk::Align::Fill);
    vu_bar.set_margin_end(4);
    vu_bar.append(&vu_meter);

    // Place VU meter at the end of the header controls.
    controls_row.append(&vu_bar);

    let safe_area_setter: Rc<dyn Fn(bool)> = {
        let safe_area_visible = safe_area_visible.clone();
        let safe_area_da = safe_area_da.clone();
        let safe_area_btn = safe_area_btn.clone();
        Rc::new(move |enabled: bool| {
            safe_area_visible.set(enabled);
            if safe_area_btn.is_active() != enabled {
                safe_area_btn.set_active(enabled);
            }
            safe_area_da.queue_draw();
        })
    };
    {
        let safe_area_setter = safe_area_setter.clone();
        let on_safe_area_changed = on_safe_area_changed.clone();
        safe_area_btn.connect_toggled(move |btn| {
            let enabled = btn.is_active();
            safe_area_setter(enabled);
            on_safe_area_changed(enabled);
        });
    }

    let false_color_setter: Rc<dyn Fn(bool)> = {
        let fc_visible = false_color_visible.clone();
        let fc_da = false_color_da.clone();
        let fc_btn = false_color_btn.clone();
        Rc::new(move |enabled: bool| {
            fc_visible.set(enabled);
            if fc_btn.is_active() != enabled {
                fc_btn.set_active(enabled);
            }
            fc_da.queue_draw();
        })
    };
    {
        let false_color_setter = false_color_setter.clone();
        let on_false_color_changed = on_false_color_changed.clone();
        false_color_btn.connect_toggled(move |btn| {
            let enabled = btn.is_active();
            false_color_setter(enabled);
            on_false_color_changed(enabled);
        });
    }

    let zebra_setter: Rc<dyn Fn(bool, f64)> = {
        let z_visible = zebra_visible.clone();
        let z_thresh = zebra_threshold_cell.clone();
        let z_da = zebra_da.clone();
        let z_btn = zebra_btn.clone();
        Rc::new(move |enabled: bool, threshold: f64| {
            z_visible.set(enabled);
            z_thresh.set(threshold);
            if z_btn.is_active() != enabled {
                z_btn.set_active(enabled);
            }
            z_da.queue_draw();
        })
    };
    {
        let zebra_setter = zebra_setter.clone();
        let on_zebra_changed = on_zebra_changed.clone();
        let z_thresh = zebra_threshold_cell.clone();
        zebra_btn.connect_toggled(move |btn| {
            let enabled = btn.is_active();
            zebra_setter(enabled, z_thresh.get());
            on_zebra_changed(enabled, z_thresh.get());
        });
    }

    let hud_setter: Rc<dyn Fn(bool)> = {
        let hud_visible = hud_visible.clone();
        let hud_da = hud_da.clone();
        let hud_btn = hud_btn.clone();
        Rc::new(move |enabled: bool| {
            hud_visible.set(enabled);
            if hud_btn.is_active() != enabled {
                hud_btn.set_active(enabled);
            }
            hud_da.queue_draw();
        })
    };
    {
        let hud_setter = hud_setter.clone();
        let on_hud_changed = on_hud_changed.clone();
        hud_btn.connect_toggled(move |btn| {
            let enabled = btn.is_active();
            hud_setter(enabled);
            on_hud_changed(enabled);
        });
    }
    let hud_redraw: Rc<dyn Fn()> = {
        let hud_visible = hud_visible.clone();
        let hud_da = hud_da.clone();
        Rc::new(move || {
            if hud_visible.get() {
                hud_da.queue_draw();
            }
        })
    };

    // Aspect-mask dropdown → setter round-trip. `updating` guards against
    // the selection-notify callback feeding back into itself when the
    // setter programmatically updates the dropdown index.
    let aspect_mask_updating: Rc<Cell<bool>> = Rc::new(Cell::new(false));
    let aspect_mask_setter: Rc<dyn Fn(AspectMaskPreset)> = {
        let aspect_mask_preset = aspect_mask_preset.clone();
        let aspect_mask_da = aspect_mask_da.clone();
        let aspect_mask_dropdown = aspect_mask_dropdown.clone();
        let aspect_mask_updating = aspect_mask_updating.clone();
        Rc::new(move |preset: AspectMaskPreset| {
            aspect_mask_preset.set(preset);
            let idx = AspectMaskPreset::ALL
                .iter()
                .position(|p| *p == preset)
                .unwrap_or(0) as u32;
            if aspect_mask_dropdown.selected() != idx {
                aspect_mask_updating.set(true);
                aspect_mask_dropdown.set_selected(idx);
                aspect_mask_updating.set(false);
            }
            aspect_mask_da.queue_draw();
        })
    };
    {
        let aspect_mask_setter = aspect_mask_setter.clone();
        let on_aspect_mask_changed = on_aspect_mask_changed.clone();
        let aspect_mask_updating = aspect_mask_updating.clone();
        // Dismiss the Overlays popover after a pick. Nested GTK4 popovers
        // (the DropDown opens its own popover inside this one) can leave the
        // outer popover's autohide stuck after the child popover closes, so
        // we popdown explicitly — which is also the right UX since the user
        // has made their selection.
        let overlays_popover_close = overlays_popover.clone();
        aspect_mask_dropdown.connect_selected_notify(move |dd| {
            if aspect_mask_updating.get() {
                return;
            }
            let idx = dd.selected() as usize;
            let preset = AspectMaskPreset::ALL
                .get(idx)
                .copied()
                .unwrap_or(AspectMaskPreset::None);
            aspect_mask_setter(preset);
            on_aspect_mask_changed(preset);
            overlays_popover_close.popdown();
        });
    }

    // Wire subtitle overlay checkbox toggle.
    {
        let sv = subtitle_visible.clone();
        let da = subtitle_da.clone();
        subtitle_overlay_btn.connect_toggled(move |btn| {
            sv.set(btn.is_active());
            da.queue_draw();
        });
    }

    // subtitle_text_setter: update the current subtitle overlay lines.
    let subtitle_text_setter: Rc<dyn Fn(Vec<SubtitleLine>)> = {
        let sl = subtitle_lines.clone();
        let da = subtitle_da.clone();
        Rc::new(move |lines: Vec<SubtitleLine>| {
            *sl.borrow_mut() = lines;
            da.queue_draw();
        })
    };

    // frame_updater: push a new scope frame to the false-color and zebra overlays.
    let frame_updater: Rc<dyn Fn(ScopeFrame)> = {
        let fc_da = false_color_da.clone();
        let z_da = zebra_da.clone();
        let of = overlay_frame.clone();
        let fc_vis = false_color_visible.clone();
        let z_vis = zebra_visible.clone();
        Rc::new(move |frame: ScopeFrame| {
            *of.borrow_mut() = Some(frame);
            if fc_vis.get() {
                fc_da.queue_draw();
            }
            if z_vis.get() {
                z_da.queue_draw();
            }
        })
    };

    // ── A/B compare setters + capture button wiring ──
    let on_ab_enabled_changed = Rc::new(on_ab_enabled_changed);
    let ab_enabled_setter: Rc<dyn Fn(bool)> = {
        let ab_enabled = ab_enabled.clone();
        let ab_da = ab_da.clone();
        let ab_compare_btn = ab_compare_btn.clone();
        Rc::new(move |enabled: bool| {
            ab_enabled.set(enabled);
            if ab_compare_btn.is_active() != enabled {
                ab_compare_btn.set_active(enabled);
            }
            ab_da.queue_draw();
        })
    };
    {
        let ab_enabled_setter = ab_enabled_setter.clone();
        let on_ab_enabled_changed = on_ab_enabled_changed.clone();
        ab_compare_btn.connect_toggled(move |btn| {
            let enabled = btn.is_active();
            ab_enabled_setter(enabled);
            on_ab_enabled_changed(enabled);
        });
    }

    let ab_midline_setter: Rc<dyn Fn(f64)> = {
        let ab_midline = ab_midline.clone();
        let ab_da = ab_da.clone();
        Rc::new(move |pct: f64| {
            let clamped = pct.clamp(0.0, 100.0);
            if (ab_midline.get() - clamped).abs() > f64::EPSILON {
                ab_midline.set(clamped);
                ab_da.queue_draw();
            }
        })
    };

    let ab_reference_setter: Rc<dyn Fn(Option<Rc<DecodedStill>>)> = {
        let ab_surface = ab_surface.clone();
        let ab_surface_dims = ab_surface_dims.clone();
        let ab_da = ab_da.clone();
        Rc::new(move |decoded: Option<Rc<DecodedStill>>| {
            let new_surface = match decoded {
                None => None,
                Some(dec) => {
                    if dec.width == 0 || dec.height == 0 {
                        None
                    } else {
                        ab_surface_dims.set((dec.width as i32, dec.height as i32));
                        decoded_to_surface(&dec).ok()
                    }
                }
            };
            if new_surface.is_none() {
                ab_surface_dims.set((0, 0));
            }
            *ab_surface.borrow_mut() = new_surface;
            ab_da.queue_draw();
        })
    };

    // Capture button wiring — the caller handles ScopeFrame grabbing, disk
    // write, and project mutation. The UI layer just forwards the click.
    let on_capture_still = Rc::new(on_capture_still);
    {
        let on_capture_still = on_capture_still.clone();
        ref_stills_capture_btn.connect_clicked(move |_| {
            on_capture_still();
        });
    }

    // ── Stills strip: rebuild on every list change ──
    let on_select_still = Rc::new(on_select_still);
    let on_delete_still = Rc::new(on_delete_still);
    let on_rename_still = Rc::new(on_rename_still);
    // Nested GTK4 popovers (context menu inside Overlays popover inside the
    // MenuButton popover) can leave the outer Overlays popover's autohide
    // stuck after the inner menu closes. Same issue as the aspect-mask
    // dropdown earlier in this file — explicitly popdown the Overlays
    // popover after any destructive still action so the user gets clear
    // feedback that their action took effect.
    let close_overlays_popover: Rc<dyn Fn()> = {
        let overlays_popover = overlays_popover.clone();
        Rc::new(move || {
            overlays_popover.popdown();
        })
    };

    let stills_strip_setter: Rc<dyn Fn(Vec<ReferenceStillSummary>, Option<String>)> = {
        let ref_stills_strip = ref_stills_strip.clone();
        let ref_stills_empty_hint = ref_stills_empty_hint.clone();
        let ref_stills_capture_btn = ref_stills_capture_btn.clone();
        let on_select_still = on_select_still.clone();
        let on_delete_still = on_delete_still.clone();
        let on_rename_still = on_rename_still.clone();
        let close_overlays_popover = close_overlays_popover.clone();
        Rc::new(move |stills: Vec<ReferenceStillSummary>, active_id: Option<String>| {
            // Clear existing children.
            while let Some(child) = ref_stills_strip.first_child() {
                ref_stills_strip.remove(&child);
            }
            if stills.is_empty() {
                ref_stills_strip.set_visible(false);
                ref_stills_empty_hint.set_visible(true);
            } else {
                ref_stills_strip.set_visible(true);
                ref_stills_empty_hint.set_visible(false);
            }
            // Cap the capture button when at max.
            ref_stills_capture_btn.set_sensitive(stills.len() < 4);

            for still in stills.iter() {
                let cell = build_reference_still_cell(
                    still,
                    active_id.as_deref() == Some(still.id.as_str()),
                    on_select_still.clone(),
                    on_delete_still.clone(),
                    on_rename_still.clone(),
                    close_overlays_popover.clone(),
                );
                ref_stills_strip.insert(&cell, -1);
            }
        })
    };

    // Apply the initial stills list so the strip reflects the loaded project.
    stills_strip_setter(initial_stills_summary, initial_active_still_id);

    // ── Timecode burn-in setter + control wiring ──
    let tc_burnin_updating: Rc<Cell<bool>> = Rc::new(Cell::new(false));
    let timecode_burnin_setter: Rc<dyn Fn(bool, TimecodeBurninPosition)> = {
        let enabled_state = tc_burnin_enabled_state.clone();
        let position_state = tc_burnin_position_state.clone();
        let tc_burnin_da = tc_burnin_da.clone();
        let tc_burnin_check = tc_burnin_check.clone();
        let tc_burnin_dropdown = tc_burnin_dropdown.clone();
        let updating = tc_burnin_updating.clone();
        Rc::new(move |enabled: bool, pos: TimecodeBurninPosition| {
            enabled_state.set(enabled);
            position_state.set(pos);
            updating.set(true);
            if tc_burnin_check.is_active() != enabled {
                tc_burnin_check.set_active(enabled);
            }
            let idx = TimecodeBurninPosition::ALL
                .iter()
                .position(|p| *p == pos)
                .unwrap_or(4) as u32;
            if tc_burnin_dropdown.selected() != idx {
                tc_burnin_dropdown.set_selected(idx);
            }
            updating.set(false);
            tc_burnin_da.queue_draw();
        })
    };
    {
        let enabled_state = tc_burnin_enabled_state.clone();
        let position_state = tc_burnin_position_state.clone();
        let tc_burnin_da = tc_burnin_da.clone();
        let on_changed = on_timecode_burnin_changed.clone();
        let updating = tc_burnin_updating.clone();
        tc_burnin_check.connect_toggled(move |btn| {
            if updating.get() {
                return;
            }
            let enabled = btn.is_active();
            enabled_state.set(enabled);
            tc_burnin_da.queue_draw();
            on_changed(enabled, position_state.get());
        });
    }
    {
        let enabled_state = tc_burnin_enabled_state.clone();
        let position_state = tc_burnin_position_state.clone();
        let tc_burnin_da = tc_burnin_da.clone();
        let on_changed = on_timecode_burnin_changed.clone();
        let updating = tc_burnin_updating.clone();
        let overlays_popover_close = overlays_popover.clone();
        tc_burnin_dropdown.connect_selected_notify(move |dd| {
            if updating.get() {
                return;
            }
            let idx = dd.selected() as usize;
            let pos = TimecodeBurninPosition::ALL
                .get(idx)
                .copied()
                .unwrap_or_default();
            position_state.set(pos);
            tc_burnin_da.queue_draw();
            on_changed(enabled_state.get(), pos);
            // Dismiss the Overlays popover after a pick (same nested-popover
            // autohide workaround used for the aspect-mask dropdown).
            overlays_popover_close.popdown();
        });
    }

    // Drive a steady redraw of the burn-in overlay by piggybacking on the HUD
    // redraw timer the caller already ticks. `hud_redraw` below already drives
    // `hud_da.queue_draw()`; we want the burn-in timecode to advance at the
    // same cadence whenever the overlay is visible, so extend that closure.
    let hud_redraw: Rc<dyn Fn()> = {
        let inner = hud_redraw.clone();
        let tc_da = tc_burnin_da.clone();
        let tc_enabled = tc_burnin_enabled_state.clone();
        Rc::new(move || {
            inner();
            if tc_enabled.get() {
                tc_da.queue_draw();
            }
        })
    };

    (
        root,
        pos_label,
        speed_label,
        picture_a,
        picture_b,
        vu_meter,
        peak_cell,
        canvas_frame,
        safe_area_setter,
        false_color_setter,
        zebra_setter,
        hud_setter,
        hud_redraw,
        aspect_mask_setter,
        frame_updater,
        subtitle_text_setter,
        ab_enabled_setter,
        ab_midline_setter,
        ab_reference_setter,
        stills_strip_setter,
        timecode_burnin_setter,
    )
}

/// Convert a decoded RGBA reference still into a Cairo ARGB32 ImageSurface
/// suitable for `cr.set_source_surface()`. On little-endian hosts Cairo
/// stores ARgb32 as [B, G, R, A] in memory, so we byte-swap on load.
fn decoded_to_surface(dec: &DecodedStill) -> anyhow::Result<gtk::cairo::ImageSurface> {
    let w = dec.width as i32;
    let h = dec.height as i32;
    let stride = gtk::cairo::Format::ARgb32
        .stride_for_width(dec.width)
        .map_err(|_| anyhow::anyhow!("stride error"))? as usize;
    let mut surface = gtk::cairo::ImageSurface::create(gtk::cairo::Format::ARgb32, w, h)
        .map_err(|_| anyhow::anyhow!("surface create failed"))?;
    {
        let mut buf = surface
            .data()
            .map_err(|_| anyhow::anyhow!("surface data error"))?;
        let width = dec.width as usize;
        let height = dec.height as usize;
        for row in 0..height {
            let src_row = row * width * 4;
            let dst_row = row * stride;
            for col in 0..width {
                let s = src_row + col * 4;
                let d = dst_row + col * 4;
                if s + 3 < dec.rgba.len() && d + 3 < buf.len() {
                    buf[d] = dec.rgba[s + 2]; // B
                    buf[d + 1] = dec.rgba[s + 1]; // G
                    buf[d + 2] = dec.rgba[s]; // R
                    buf[d + 3] = dec.rgba[s + 3]; // A
                }
            }
        }
    }
    Ok(surface)
}

/// Build a single strip cell for a reference still (thumbnail + label + right-
/// click context menu). Clicking the cell asks the caller to activate this
/// still as the A/B reference.
fn build_reference_still_cell(
    still: &ReferenceStillSummary,
    is_active: bool,
    on_select: Rc<dyn Fn(Option<String>)>,
    on_delete: Rc<dyn Fn(String)>,
    on_rename: Rc<dyn Fn(String, String)>,
    close_overlays_popover: Rc<dyn Fn()>,
) -> FlowBoxChild {
    const CELL_W: i32 = 120;
    const CELL_H: i32 = 68;

    let container = GBox::new(Orientation::Vertical, 2);
    container.set_size_request(CELL_W, CELL_H + 18);

    // Thumbnail paint area.
    let thumb_da = DrawingArea::new();
    thumb_da.set_content_width(CELL_W);
    thumb_da.set_content_height(CELL_H);
    let thumb_surface = still
        .thumbnail
        .as_ref()
        .and_then(|dec| decoded_to_surface(dec).ok());
    let is_missing = still.thumbnail.is_none();
    let highlight_active = is_active;
    thumb_da.set_draw_func(move |_da, cr, width, height| {
        let w = width as f64;
        let h = height as f64;
        cr.set_source_rgb(0.08, 0.08, 0.08);
        cr.rectangle(0.0, 0.0, w, h);
        let _ = cr.fill();
        if let Some(ref surface) = thumb_surface {
            let sw = surface.width() as f64;
            let sh = surface.height() as f64;
            if sw > 0.0 && sh > 0.0 {
                let src_ratio = sw / sh;
                let dst_ratio = w / h;
                let (draw_w, draw_h) = if src_ratio > dst_ratio {
                    (w, w / src_ratio)
                } else {
                    (h * src_ratio, h)
                };
                let draw_x = (w - draw_w) * 0.5;
                let draw_y = (h - draw_h) * 0.5;
                cr.save().ok();
                cr.translate(draw_x, draw_y);
                cr.scale(draw_w / sw, draw_h / sh);
                let _ = cr.set_source_surface(surface, 0.0, 0.0);
                cr.paint().ok();
                cr.restore().ok();
            }
        } else if is_missing {
            cr.set_source_rgb(0.6, 0.25, 0.25);
            cr.set_line_width(1.0);
            let pad = 6.0;
            cr.rectangle(pad, pad, w - pad * 2.0, h - pad * 2.0);
            let _ = cr.stroke();
            cr.move_to(pad, pad);
            cr.line_to(w - pad, h - pad);
            let _ = cr.stroke();
            cr.move_to(w - pad, pad);
            cr.line_to(pad, h - pad);
            let _ = cr.stroke();
        }
        // Active highlight border.
        if highlight_active {
            cr.set_source_rgb(0.95, 0.75, 0.15);
            cr.set_line_width(3.0);
            cr.rectangle(1.5, 1.5, w - 3.0, h - 3.0);
            let _ = cr.stroke();
        }
    });
    container.append(&thumb_da);

    // Label row.
    let label_text = if still.label.trim().is_empty() {
        "(untitled)".to_string()
    } else {
        still.label.clone()
    };
    let label = Label::new(Some(&label_text));
    label.set_xalign(0.5);
    label.set_max_width_chars(14);
    label.set_ellipsize(pango::EllipsizeMode::End);
    label.add_css_class("dim-label");
    container.append(&label);

    // Left-click selects this still as the A/B reference.
    {
        let id = still.id.clone();
        let on_select = on_select.clone();
        let click = GestureClick::new();
        click.set_button(1);
        click.connect_pressed(move |_gesture, _n, _x, _y| {
            on_select(Some(id.clone()));
        });
        container.add_controller(click);
    }

    // Right-click → context menu (Rename / Delete).
    {
        let id = still.id.clone();
        let label_text = still.label.clone();
        let on_delete = on_delete.clone();
        let on_rename = on_rename.clone();
        let close_overlays_popover = close_overlays_popover.clone();
        let click = GestureClick::new();
        click.set_button(3);
        let container_weak = container.downgrade();
        click.connect_pressed(move |_gesture, _n, x, y| {
            let close_overlays_for_menu = close_overlays_popover.clone();
            let Some(container) = container_weak.upgrade() else {
                return;
            };
            let popover = Popover::new();
            popover.set_parent(&container);
            popover.set_has_arrow(false);
            popover.set_autohide(true);
            popover.set_pointing_to(Some(&gtk::gdk::Rectangle::new(
                x as i32, y as i32, 1, 1,
            )));
            // Popovers created via `set_parent` must be explicitly unparented
            // when they close, otherwise they linger in the widget tree and
            // subsequent clicks can fall through without dismissing the menu.
            popover.connect_closed(|pop| {
                pop.unparent();
            });
            let menu_box = GBox::new(Orientation::Vertical, 2);
            menu_box.set_margin_start(4);
            menu_box.set_margin_end(4);
            menu_box.set_margin_top(4);
            menu_box.set_margin_bottom(4);

            let rename_btn = Button::with_label("Rename…");
            rename_btn.add_css_class("flat");
            let delete_btn = Button::with_label("Delete");
            delete_btn.add_css_class("flat");

            {
                let id = id.clone();
                let label_text = label_text.clone();
                let on_rename = on_rename.clone();
                let container = container.clone();
                let popover_weak = popover.downgrade();
                let close_overlays = close_overlays_for_menu.clone();
                rename_btn.connect_clicked(move |_| {
                    if let Some(p) = popover_weak.upgrade() {
                        p.popdown();
                    }
                    // Close the parent Overlays popover too — the rename
                    // prompt opens a popover anchored to the cell, and a
                    // third-level popover inside the stuck-autohide Overlays
                    // menu is visually confusing. The user can reopen
                    // Overlays after rename if they need more tweaks.
                    close_overlays();
                    prompt_rename_reference_still(
                        &container,
                        id.clone(),
                        label_text.clone(),
                        on_rename.clone(),
                    );
                });
            }
            {
                // The delete handler triggers a strip rebuild, which destroys
                // the widget tree this popover is parented to. Defer the
                // actual delete to the next idle tick so the popover's
                // popdown animation + `connect_closed` unparent land before
                // the parent cell goes away. Also close the outer Overlays
                // popover so the user gets clear feedback and GTK doesn't
                // leave the nested-autohide state stuck.
                let id = id.clone();
                let on_delete = on_delete.clone();
                let popover_weak = popover.downgrade();
                let close_overlays = close_overlays_for_menu.clone();
                delete_btn.connect_clicked(move |_| {
                    if let Some(p) = popover_weak.upgrade() {
                        p.popdown();
                    }
                    close_overlays();
                    let id = id.clone();
                    let on_delete = on_delete.clone();
                    gtk::glib::idle_add_local_once(move || {
                        on_delete(id);
                    });
                });
            }
            menu_box.append(&rename_btn);
            menu_box.append(&delete_btn);
            popover.set_child(Some(&menu_box));
            popover.popup();
        });
        container.add_controller(click);
    }

    let child = FlowBoxChild::new();
    child.set_child(Some(&container));
    child.set_focusable(false);
    child
}

/// Minimal rename prompt using a transient Popover with an Entry. Avoids the
/// deprecated gtk::Dialog path.
fn prompt_rename_reference_still(
    anchor: &impl IsA<gtk::Widget>,
    id: String,
    current: String,
    on_rename: Rc<dyn Fn(String, String)>,
) {
    let popover = Popover::new();
    popover.set_parent(anchor);
    popover.set_has_arrow(true);
    popover.set_autohide(true);
    popover.connect_closed(|pop| {
        pop.unparent();
    });
    let vbox = GBox::new(Orientation::Vertical, 4);
    vbox.set_margin_start(6);
    vbox.set_margin_end(6);
    vbox.set_margin_top(6);
    vbox.set_margin_bottom(6);
    let entry = gtk::Entry::new();
    entry.set_text(&current);
    entry.set_placeholder_text(Some("Reference still name"));
    entry.set_width_chars(20);
    let apply_btn = Button::with_label("Rename");
    apply_btn.add_css_class("suggested-action");
    vbox.append(&entry);
    vbox.append(&apply_btn);
    popover.set_child(Some(&vbox));
    {
        let entry = entry.clone();
        let id = id.clone();
        let on_rename = on_rename.clone();
        let popover_weak = popover.downgrade();
        apply_btn.connect_clicked(move |_| {
            let new_label = entry.text().to_string();
            on_rename(id.clone(), new_label);
            if let Some(p) = popover_weak.upgrade() {
                p.popdown();
            }
        });
    }
    {
        let entry_apply = apply_btn.clone();
        entry.connect_activate(move |_| {
            entry_apply.emit_clicked();
        });
    }
    popover.popup();
    entry.grab_focus();
}

#[cfg(test)]
mod tests {
    use super::{
        subtitle_preview_baseline_y, subtitle_preview_box_padding, subtitle_preview_outline_width,
        subtitle_preview_stroke_width, subtitle_preview_underline_metrics,
        PROGRAM_MONITOR_CANVAS_BASE_CSS_CLASSES,
    };

    #[test]
    fn subtitle_outline_scales_with_preview_height() {
        assert!((subtitle_preview_outline_width(2.5, 1080.0) - 2.5).abs() < 1e-6);
        assert!((subtitle_preview_outline_width(2.5, 540.0) - 1.25).abs() < 1e-6);
    }

    #[test]
    fn subtitle_box_padding_scales_with_preview_height() {
        let (pad_x, pad_y, radius) = subtitle_preview_box_padding(540.0);
        assert!((pad_x - 4.0).abs() < 1e-6);
        assert!((pad_y - 2.0).abs() < 1e-6);
        assert!((radius - 2.0).abs() < 1e-6);
    }

    #[test]
    fn subtitle_underline_metrics_scale_with_font_size() {
        let (thickness, offset) = subtitle_preview_underline_metrics(32.0);
        assert!((thickness - 1.92).abs() < 1e-6);
        assert!((offset - 3.84).abs() < 1e-6);
    }

    #[test]
    fn subtitle_stroke_width_scales_with_preview_height() {
        assert!((subtitle_preview_stroke_width(1080.0) - 4.0).abs() < 1e-6);
        assert!((subtitle_preview_stroke_width(540.0) - 2.0).abs() < 1e-6);
    }

    #[test]
    fn subtitle_baseline_anchors_to_top_center_and_bottom_regions() {
        let y_bearing = -18.0;
        let text_height = 22.0;

        let top_baseline = subtitle_preview_baseline_y(0.2, 100.0, y_bearing, text_height);
        let top_edge = top_baseline + y_bearing;
        assert!((top_edge - 20.0).abs() < 1e-6);

        let center_baseline = subtitle_preview_baseline_y(0.5, 100.0, y_bearing, text_height);
        let center_y = center_baseline + y_bearing + text_height / 2.0;
        assert!((center_y - 50.0).abs() < 1e-6);

        let bottom_baseline = subtitle_preview_baseline_y(0.85, 100.0, y_bearing, text_height);
        let bottom_edge = bottom_baseline + y_bearing + text_height;
        assert!((bottom_edge - 85.0).abs() < 1e-6);
    }

    #[test]
    fn canvas_base_keeps_preview_background_class_for_empty_timelines() {
        assert!(PROGRAM_MONITOR_CANVAS_BASE_CSS_CLASSES.contains(&"preview-video"));
    }
}

/// Build a VU meter DrawingArea showing L/R audio peak levels in dBFS.
/// Returns `(drawing_area, peak_cell)` where the caller writes `[left_db, right_db]`
/// into `peak_cell` and calls `drawing_area.queue_draw()` each poll tick.
///
/// Scale: 0 dBFS at top, -60 dBFS at bottom.
/// Zones: green (< -18 dBFS), yellow (-18 to -6 dBFS), red (> -6 dBFS).
pub fn build_vu_meter() -> (DrawingArea, Rc<Cell<[f64; 2]>>) {
    let peak_cell: Rc<Cell<[f64; 2]>> = Rc::new(Cell::new([-60.0, -60.0]));
    let da = DrawingArea::new();
    da.set_content_width(36);
    da.set_content_height(80);
    da.set_valign(gtk::Align::Center);

    let pc = peak_cell.clone();
    da.set_draw_func(move |_da, cr, width, height| {
        let [left_db, right_db] = pc.get();
        let bar_w = (width as f64 / 2.0 - 2.0).max(4.0);
        // dBFS → fraction of bar height (0.0 = bottom, 1.0 = top).
        let db_to_frac = |db: f64| -> f64 { ((db + 60.0) / 60.0).clamp(0.0, 1.0) };
        // Draw background.
        cr.set_source_rgb(0.13, 0.13, 0.13);
        cr.rectangle(0.0, 0.0, width as f64, height as f64);
        let _ = cr.fill();

        for (ch, db) in [(0, left_db), (1, right_db)] {
            let x = ch as f64 * (bar_w + 2.0) + 1.0;
            let frac = db_to_frac(db);
            let bar_h = frac * height as f64;
            let y_top = height as f64 - bar_h;

            // Green zone: below -18 dBFS
            let green_frac = db_to_frac(-18.0);
            let green_h = (green_frac * height as f64).min(bar_h);
            if green_h > 0.0 {
                let (r, g, b) = crate::ui::colors::COLOR_LEVEL_GOOD;
                cr.set_source_rgb(r, g, b);
                cr.rectangle(x, height as f64 - green_h, bar_w, green_h);
                let _ = cr.fill();
            }

            // Yellow zone: -18 to -6 dBFS
            let yellow_frac = db_to_frac(-6.0);
            let yellow_top = green_frac * height as f64;
            let yellow_h =
                ((yellow_frac - green_frac) * height as f64).min((bar_h - green_h).max(0.0));
            if yellow_h > 0.0 {
                let (r, g, b) = crate::ui::colors::COLOR_LEVEL_WARN;
                cr.set_source_rgb(r, g, b);
                cr.rectangle(x, height as f64 - yellow_top - yellow_h, bar_w, yellow_h);
                let _ = cr.fill();
            }

            // Red zone: above -6 dBFS
            let red_top = yellow_frac * height as f64;
            let red_h = (bar_h - red_top).max(0.0);
            if red_h > 0.0 {
                let (r, g, b) = crate::ui::colors::COLOR_LEVEL_CLIP;
                cr.set_source_rgb(r, g, b);
                cr.rectangle(x, y_top, bar_w, red_h);
                let _ = cr.fill();
            }

            // Zone boundary markers (thin dark lines at -18 and -6 dBFS).
            cr.set_source_rgb(0.05, 0.05, 0.05);
            cr.set_line_width(1.0);
            for marker_db in [-18.0_f64, -6.0_f64] {
                let my = height as f64 - db_to_frac(marker_db) * height as f64;
                cr.move_to(x, my);
                cr.line_to(x + bar_w, my);
                let _ = cr.stroke();
            }
            let _ = ch; // suppress unused warning
        }
    });

    (da, peak_cell)
}

pub fn format_timecode(ns: u64, frame_rate: &FrameRate) -> String {
    timecode::format_ns_as_timecode(ns, frame_rate)
}

/// Map a luminance value (0.0–1.0) to an RGB triple using the standard
/// false-color exposure palette:
///   deep purple → blue → cyan → green (correct) → yellow → orange → red → white
fn false_color_luma(luma: f64) -> (f64, f64, f64) {
    if luma < 0.04 {
        (0.30, 0.0, 0.50) // deep purple — clipped/crushed black
    } else if luma < 0.20 {
        (0.0, 0.0, 0.90) // blue — underexposed shadow
    } else if luma < 0.45 {
        (0.0, 0.75, 0.75) // cyan — low midtone
    } else if luma < 0.60 {
        (0.0, 0.80, 0.0) // green — correctly exposed midtone ✓
    } else if luma < 0.70 {
        (1.0, 1.0, 0.0) // yellow — high midtone
    } else if luma < 0.90 {
        (1.0, 0.50, 0.0) // orange — overexposed highlight
    } else if luma < 0.97 {
        (1.0, 0.0, 0.0) // red — near clip
    } else {
        (1.0, 1.0, 1.0) // white — clipped white
    }
}
