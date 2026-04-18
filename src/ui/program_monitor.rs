use crate::media::program_player::{ProgramPlayer, ScopeFrame};
use crate::model::project::FrameRate;
use crate::ui::colors::{LUMA_B, LUMA_G, LUMA_R};
use crate::ui::timecode;
use crate::ui_state::AspectMaskPreset;

/// Discrete zoom levels for the program monitor zoom in/out buttons.
const PROGRAM_MONITOR_ZOOM_LEVELS: &[f64] = &[0.25, 0.5, 0.75, 1.0, 1.5, 2.0, 3.0, 4.0];
const PROGRAM_MONITOR_CANVAS_BASE_CSS_CLASSES: &[&str] = &["preview-video"];
use gtk4::prelude::*;
use gtk4::{
    self as gtk, AspectFrame, Box as GBox, Button, CheckButton, DrawingArea, DropDown,
    EventControllerScroll, EventControllerScrollFlags, Label, MenuButton, Orientation, Overlay,
    Picture, Popover, ScrolledWindow, StringList,
};
use std::cell::{Cell, RefCell};
use std::rc::Rc;

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
    // Optional extra button to append to the Program Monitor header
    // controls row (e.g. the Loudness Radar popover toggle). When `None`
    // the header looks exactly as before.
    extra_header_button: Option<gtk::Widget>,
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
    )
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
