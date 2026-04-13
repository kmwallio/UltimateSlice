use gtk4::glib;
/// Interactive transform overlay drawn over the program monitor video.
///
/// Shows a bounding box + 4 corner handles representing the clip's scale and position.
/// - Drag a **corner handle** to change the clip's zoom scale.
/// - Drag **inside** the bounding box to pan (position X/Y).
///
/// The overlay is a transparent `DrawingArea` layered on top of the GtkPicture
/// via GtkOverlay.  It calls `on_change(scale, position_x, position_y)` each
/// time the user moves the mouse.
use gtk4::prelude::*;
use gtk4::{self as gtk, DrawingArea};
use std::cell::{Cell, RefCell};
use std::rc::Rc;

/// Radius (px) of the drawn corner handle circles.
const HANDLE_R: f64 = TRANSFORM_HANDLE_RADIUS_PX;
/// Hit-test radius for corner handles (a bit larger than drawn for ease of use).
const HANDLE_HIT: f64 = TRANSFORM_HANDLE_HIT_RADIUS_PX;

/// Drawn radius (px) of the transform overlay's corner/edge handle circles.
pub const TRANSFORM_HANDLE_RADIUS_PX: f64 = 7.0;
/// Hit-test radius (px) for the transform overlay's corner/edge handles. Larger
/// than the drawn radius so the handle is easier to grab without pixel-perfect
/// aim.
pub const TRANSFORM_HANDLE_HIT_RADIUS_PX: f64 = 16.0;
/// Inspector crop slider max (pixels).
///
/// Sourced from `crate::model::transform_bounds::CROP_MAX_PX_I32` so the
/// slider, the transform-overlay drag clamps, the model-level keyframe
/// clamp, the runtime evaluators in `program_player`, and the export
/// keyframe evaluators all share a single source of truth.
const CROP_MAX: i32 = crate::model::transform_bounds::CROP_MAX_PX_I32;

#[derive(Clone, Copy, PartialEq)]
enum Handle {
    None,
    Rotate,
    TopLeft,
    TopRight,
    BottomLeft,
    BottomRight,
    CropLeft,
    CropRight,
    CropTop,
    CropBottom,
    /// Drag inside the bounding box → pan.
    Pan,
    /// Drag a bezier path anchor point (index into path.points).
    MaskPathAnchor(usize),
    /// Drag the incoming tangent handle of a path point.
    MaskPathHandleIn(usize),
    /// Drag the outgoing tangent handle of a path point.
    MaskPathHandleOut(usize),
    /// Drag inside the tracking region to reposition it.
    TrackingPan,
    TrackingTopLeft,
    TrackingTopRight,
    TrackingBottomLeft,
    TrackingBottomRight,
    /// Drag to capture a SAM box prompt. Used when the overlay is in
    /// "SAM prompt mode" (Phase 2b/3). On drag_end the captured
    /// widget-space rectangle is converted to normalized clip-local
    /// coordinates and forwarded to the installed `sam_prompt_callback`.
    SamPromptBox,
    /// Drag to draw a freehand stroke in a ClipKind::Drawing clip.
    Drawing,
}

struct DragState {
    handle: Handle,
    start_wx: f64,
    start_wy: f64,
    start_scale: f64,
    start_px: f64,
    start_py: f64,
    start_crop_left: i32,
    start_crop_right: i32,
    start_crop_top: i32,
    start_crop_bottom: i32,
    proj_w: u32,
    proj_h: u32,
    /// Video rect cached at drag start.
    vx: f64,
    vy: f64,
    vw: f64,
    vh: f64,
    /// Snapshot of the dragged path point at drag start.
    start_path_point: Option<crate::model::clip::BezierPoint>,
    start_tracking_cx: f64,
    start_tracking_cy: f64,
    start_tracking_width: f64,
    start_tracking_height: f64,
    /// Color for the current drawing stroke (0xRRGGBBAA).
    pub drawing_color: u32,
    /// Width for the current drawing stroke.
    pub drawing_width: f64,
}

pub struct TransformOverlay {
    pub drawing_area: DrawingArea,
    pub active_tool: Rc<Cell<crate::ui::timeline::ActiveTool>>,
    scale: Rc<Cell<f64>>,
    position_x: Rc<Cell<f64>>,
    position_y: Rc<Cell<f64>>,
    rotation: Rc<Cell<f64>>,
    crop_left: Rc<Cell<i32>>,
    crop_right: Rc<Cell<i32>>,
    crop_top: Rc<Cell<i32>>,
    crop_bottom: Rc<Cell<i32>>,
    selected: Rc<Cell<bool>>,
    proj_w: Rc<Cell<u32>>,
    proj_h: Rc<Cell<u32>>,
    picture: Rc<RefCell<Option<gtk4::Picture>>>,
    /// The AspectFrame widget that constrains the canvas area.
    /// When set, the draw/drag functions query its bounds in the DA's coordinate
    /// space via `Widget::compute_bounds()` instead of calling `video_rect()` on
    /// the full DA size.  This gives correct results when the DA is larger than the
    /// canvas (e.g. when the outer overlay covers the full scroll viewport and the
    /// canvas_frame is smaller due to zoom < 1.0).
    canvas_widget: Rc<RefCell<Option<gtk4::Widget>>>,
    // Shape mask state for overlay drawing.
    mask_enabled: Rc<Cell<bool>>,
    mask_shape: Rc<Cell<u8>>, // 0=rect, 1=ellipse
    mask_cx: Rc<Cell<f64>>,
    mask_cy: Rc<Cell<f64>>,
    mask_hw: Rc<Cell<f64>>,
    mask_hh: Rc<Cell<f64>>,
    mask_rotation: Rc<Cell<f64>>,
    mask_path_points: Rc<RefCell<Vec<crate::model::clip::BezierPoint>>>,
    adjustment_mode: Rc<Cell<bool>>,
    content_inset_x: Rc<Cell<f64>>,
    content_inset_y: Rc<Cell<f64>>,
    tracking_region_enabled: Rc<Cell<bool>>,
    tracking_region_editing: Rc<Cell<bool>>,
    tracking_center_x: Rc<Cell<f64>>,
    tracking_center_y: Rc<Cell<f64>>,
    tracking_width: Rc<Cell<f64>>,
    tracking_height: Rc<Cell<f64>>,
    tracking_rotation: Rc<Cell<f64>>,
    // ── SAM box prompt capture (Phase 2b/3) ──────────────────────────
    /// When `true` the overlay's gesture suppresses all normal
    /// hit-testing and instead captures a box prompt drag for SAM.
    /// Toggled by `enter_sam_prompt_mode` / `exit_sam_prompt_mode`.
    sam_prompt_mode: Rc<Cell<bool>>,
    /// Widget-space `(x, y)` where the user pressed the mouse button
    /// to begin the prompt drag. `None` when not currently dragging.
    sam_prompt_start: Rc<Cell<Option<(f64, f64)>>>,
    /// Widget-space `(x, y)` of the current drag position — updated
    /// continuously by drag_update so the draw function can render
    /// a live preview rectangle.
    sam_prompt_current: Rc<Cell<Option<(f64, f64)>>>,
    /// Callback to invoke with the captured normalized clip-local
    /// box `(x1, y1, x2, y2)` on drag_end. Cleared after firing or
    /// on `exit_sam_prompt_mode`. Installed by `enter_sam_prompt_mode`.
    sam_prompt_callback: Rc<RefCell<Option<Box<dyn Fn(f64, f64, f64, f64)>>>>,
    // ── Drawing tool state ──────────────────────────────────────────
    /// Current brush color as 0xRRGGBBAA. Applied to the next stroke/shape.
    drawing_color: Rc<Cell<u32>>,
    /// Current brush width in pixels (relative to 1080p height).
    drawing_width: Rc<Cell<f64>>,
    /// Which shape kind the Draw tool will commit on mouse-up.
    drawing_kind: Rc<Cell<crate::model::clip::DrawingKind>>,
    /// Optional fill color for Rectangle/Ellipse. `None` = stroke only.
    drawing_fill: Rc<Cell<Option<u32>>>,
    /// Snapshot of the drawing items under the playhead. The app
    /// pushes this via `set_current_drawing_items` so the overlay
    /// can hit-test clicks and paint the selected item's highlight
    /// without having to round-trip to the project model on every
    /// frame.
    drawing_items_snapshot: Rc<RefCell<Vec<crate::model::clip::DrawingItem>>>,
    /// Index (into `drawing_items_snapshot`) of the currently
    /// selected item, or `None` when no click-to-select is active.
    selected_drawing_item: Rc<Cell<Option<usize>>>,
}

impl TransformOverlay {
    /// Create a new overlay.  `on_change(scale, position_x, position_y)` is
    /// called whenever the user adjusts scale or position via drag.
    pub fn new(
        on_change: impl Fn(f64, f64, f64) + 'static,
        on_rotate_change: impl Fn(i32) + 'static,
        on_crop_change: impl Fn(i32, i32, i32, i32) + 'static,
        on_drag_begin: impl Fn() + 'static,
        on_drag_end: impl Fn() + 'static,
        on_mask_path_change: impl Fn(&[crate::model::clip::BezierPoint]) + 'static,
        on_mask_path_dbl_click: impl Fn(&[crate::model::clip::BezierPoint]) + 'static,
        on_tracking_region_change: impl Fn(f64, f64, f64, f64) + 'static,
        on_drawing_finish: impl Fn(crate::model::clip::DrawingItem) + 'static,
        // `Some(idx)` = delete the specific item at that index in
        // the overlay's current drawing snapshot; `None` = the
        // pre-selection LIFO behaviour (pop the most-recent item
        // in the drawing clip under the playhead).
        on_drawing_delete_at: impl Fn(Option<usize>) + 'static,
        active_tool: crate::ui::timeline::ActiveTool,
    ) -> Self {
        let active_tool = Rc::new(Cell::new(active_tool));
        let on_drawing_delete_at = Rc::new(on_drawing_delete_at);
        let scale = Rc::new(Cell::new(1.0_f64));
        let position_x = Rc::new(Cell::new(0.0_f64));
        let position_y = Rc::new(Cell::new(0.0_f64));
        let rotation = Rc::new(Cell::new(0.0_f64));
        let crop_left = Rc::new(Cell::new(0_i32));
        let crop_right = Rc::new(Cell::new(0_i32));
        let crop_top = Rc::new(Cell::new(0_i32));
        let crop_bottom = Rc::new(Cell::new(0_i32));
        let selected = Rc::new(Cell::new(false));
        let proj_w = Rc::new(Cell::new(1920_u32));
        let proj_h = Rc::new(Cell::new(1080_u32));
        // Letterbox inset fractions (0.0–0.5) for each side.
        // Used to shrink the clip bounding box to the video content area.
        let content_inset_x = Rc::new(Cell::new(0.0_f64));
        let content_inset_y = Rc::new(Cell::new(0.0_f64));
        let picture: Rc<RefCell<Option<gtk4::Picture>>> = Rc::new(RefCell::new(None));
        let canvas_widget: Rc<RefCell<Option<gtk4::Widget>>> = Rc::new(RefCell::new(None));
        let mask_enabled = Rc::new(Cell::new(false));
        let mask_shape = Rc::new(Cell::new(0u8));
        let mask_cx = Rc::new(Cell::new(0.5f64));
        let mask_cy = Rc::new(Cell::new(0.5f64));
        let mask_hw = Rc::new(Cell::new(0.25f64));
        let mask_hh = Rc::new(Cell::new(0.25f64));
        let mask_rotation = Rc::new(Cell::new(0.0f64));
        let mask_path_points: Rc<RefCell<Vec<crate::model::clip::BezierPoint>>> =
            Rc::new(RefCell::new(Vec::new()));
        let adjustment_mode = Rc::new(Cell::new(false));
        let tracking_region_enabled = Rc::new(Cell::new(false));
        let tracking_region_editing = Rc::new(Cell::new(false));
        let tracking_center_x = Rc::new(Cell::new(0.5f64));
        let tracking_center_y = Rc::new(Cell::new(0.5f64));
        let tracking_width = Rc::new(Cell::new(0.25f64));
        let tracking_height = Rc::new(Cell::new(0.25f64));
        let tracking_rotation = Rc::new(Cell::new(0.0f64));
        // SAM box-prompt capture state (Phase 2b/3).
        let sam_prompt_mode = Rc::new(Cell::new(false));
        let sam_prompt_start: Rc<Cell<Option<(f64, f64)>>> = Rc::new(Cell::new(None));
        let sam_prompt_current: Rc<Cell<Option<(f64, f64)>>> = Rc::new(Cell::new(None));
        let sam_prompt_callback: Rc<RefCell<Option<Box<dyn Fn(f64, f64, f64, f64)>>>> =
            Rc::new(RefCell::new(None));

        let current_drawing_points: Rc<RefCell<Vec<(f64, f64)>>> =
            Rc::new(RefCell::new(Vec::new()));
        let drawing_color = Rc::new(Cell::new(0xFF0000FF)); // Default red
        let drawing_width = Rc::new(Cell::new(5.0)); // Default 5px
        let drawing_kind = Rc::new(Cell::new(crate::model::clip::DrawingKind::Stroke));
        let drawing_fill: Rc<Cell<Option<u32>>> = Rc::new(Cell::new(None));
        let drawing_items_snapshot: Rc<RefCell<Vec<crate::model::clip::DrawingItem>>> =
            Rc::new(RefCell::new(Vec::new()));
        let selected_drawing_item: Rc<Cell<Option<usize>>> = Rc::new(Cell::new(None));
        let on_drawing_finish = Rc::new(on_drawing_finish);

        let da = DrawingArea::new();
        da.set_hexpand(true);
        da.set_vexpand(true);
        da.set_can_target(true);
        da.set_focusable(true);

        // Draw function ----------------------------------------------------
        {
            let scale = scale.clone();
            let position_x = position_x.clone();
            let position_y = position_y.clone();
            let rotation = rotation.clone();
            let crop_left = crop_left.clone();
            let crop_right = crop_right.clone();
            let crop_top = crop_top.clone();
            let crop_bottom = crop_bottom.clone();
            let selected = selected.clone();
            let proj_w = proj_w.clone();
            let proj_h = proj_h.clone();
            let content_inset_x = content_inset_x.clone();
            let content_inset_y = content_inset_y.clone();
            let picture = picture.clone();
            let canvas_widget = canvas_widget.clone();
            let mask_enabled = mask_enabled.clone();
            let mask_shape = mask_shape.clone();
            let mask_cx = mask_cx.clone();
            let mask_cy = mask_cy.clone();
            let mask_hw = mask_hw.clone();
            let mask_hh = mask_hh.clone();
            let mask_rotation_d = mask_rotation.clone();
            let mask_path_points_clone = mask_path_points.clone();
            let adjustment_mode = adjustment_mode.clone();
            let tracking_region_enabled = tracking_region_enabled.clone();
            let tracking_region_editing = tracking_region_editing.clone();
            let tracking_center_x = tracking_center_x.clone();
            let tracking_center_y = tracking_center_y.clone();
            let tracking_width = tracking_width.clone();
            let tracking_height = tracking_height.clone();
            let tracking_rotation_d = tracking_rotation.clone();
            let sam_prompt_mode_draw = sam_prompt_mode.clone();
            let sam_prompt_start_draw = sam_prompt_start.clone();
            let sam_prompt_current_draw = sam_prompt_current.clone();

            let drawing_points = current_drawing_points.clone();
            let drawing_color_d = drawing_color.clone();
            let drawing_width_d = drawing_width.clone();
            let drawing_kind_draw = drawing_kind.clone();
            let drawing_fill_draw = drawing_fill.clone();
            let active_tool_draw = active_tool.clone();
            let drawing_items_draw = drawing_items_snapshot.clone();
            let selected_drawing_draw = selected_drawing_item.clone();

            da.set_draw_func(move |da, cr, ww, wh| {
                let draw_active =
                    active_tool_draw.get() == crate::ui::timeline::ActiveTool::Draw;
                if !selected.get() && !draw_active {
                    return;
                }
                // Always use project dimensions for the canvas boundary.
                // The canvas border represents what will be exported, not the clip's native size.
                let _ = &picture; // kept for potential future use
                let (vx, vy, vw, vh) =
                    canvas_video_rect(da, &canvas_widget, ww, wh, proj_w.get(), proj_h.get());

                // ── Live Drawing preview ────────────────────────
                let pts = drawing_points.borrow();
                if !pts.is_empty() {
                    use crate::model::clip::DrawingKind;
                    let color = drawing_color_d.get();
                    let (cr_r, cr_g, cr_b, cr_a) = (
                        ((color >> 24) & 0xFF) as f64 / 255.0,
                        ((color >> 16) & 0xFF) as f64 / 255.0,
                        ((color >> 8) & 0xFF) as f64 / 255.0,
                        (color & 0xFF) as f64 / 255.0,
                    );
                    cr.set_source_rgba(cr_r, cr_g, cr_b, cr_a);
                    cr.set_line_width(drawing_width_d.get());
                    cr.set_line_cap(gtk4::cairo::LineCap::Round);
                    cr.set_line_join(gtk4::cairo::LineJoin::Round);
                    let kind = drawing_kind_draw.get();
                    let p0 = pts[0];
                    let p1 = *pts.last().unwrap();
                    match kind {
                        DrawingKind::Stroke => {
                            for (i, (wx, wy)) in pts.iter().enumerate() {
                                if i == 0 {
                                    cr.move_to(*wx, *wy);
                                } else {
                                    cr.line_to(*wx, *wy);
                                }
                            }
                            let _ = cr.stroke();
                        }
                        DrawingKind::Rectangle => {
                            let x = p0.0.min(p1.0);
                            let y = p0.1.min(p1.1);
                            let rw = (p0.0 - p1.0).abs();
                            let rh = (p0.1 - p1.1).abs();
                            cr.rectangle(x, y, rw, rh);
                            if let Some(fc) = drawing_fill_draw.get() {
                                let (fr, fg, fb, fa) = (
                                    ((fc >> 24) & 0xFF) as f64 / 255.0,
                                    ((fc >> 16) & 0xFF) as f64 / 255.0,
                                    ((fc >> 8) & 0xFF) as f64 / 255.0,
                                    (fc & 0xFF) as f64 / 255.0,
                                );
                                cr.set_source_rgba(fr, fg, fb, fa);
                                let _ = cr.fill_preserve();
                                cr.set_source_rgba(cr_r, cr_g, cr_b, cr_a);
                            }
                            let _ = cr.stroke();
                        }
                        DrawingKind::Ellipse => {
                            let x0 = p0.0.min(p1.0);
                            let y0 = p0.1.min(p1.1);
                            let rw = (p0.0 - p1.0).abs().max(1.0);
                            let rh = (p0.1 - p1.1).abs().max(1.0);
                            let cx = x0 + rw * 0.5;
                            let cy = y0 + rh * 0.5;
                            cr.save().ok();
                            cr.translate(cx, cy);
                            cr.scale(rw * 0.5, rh * 0.5);
                            cr.arc(0.0, 0.0, 1.0, 0.0, std::f64::consts::TAU);
                            cr.restore().ok();
                            if let Some(fc) = drawing_fill_draw.get() {
                                let (fr, fg, fb, fa) = (
                                    ((fc >> 24) & 0xFF) as f64 / 255.0,
                                    ((fc >> 16) & 0xFF) as f64 / 255.0,
                                    ((fc >> 8) & 0xFF) as f64 / 255.0,
                                    (fc & 0xFF) as f64 / 255.0,
                                );
                                cr.set_source_rgba(fr, fg, fb, fa);
                                let _ = cr.fill_preserve();
                                cr.set_source_rgba(cr_r, cr_g, cr_b, cr_a);
                            }
                            let _ = cr.stroke();
                        }
                        DrawingKind::Arrow => {
                            cr.move_to(p0.0, p0.1);
                            cr.line_to(p1.0, p1.1);
                            let _ = cr.stroke();
                            let dx = p1.0 - p0.0;
                            let dy = p1.1 - p0.1;
                            let len = (dx * dx + dy * dy).sqrt().max(1.0);
                            let ux = dx / len;
                            let uy = dy / len;
                            let head = (drawing_width_d.get() * 6.0).max(10.0);
                            let (ca, sa) =
                                (25f64.to_radians().cos(), 25f64.to_radians().sin());
                            let lxa = p1.0 - head * (ux * ca - uy * sa);
                            let lya = p1.1 - head * (uy * ca + ux * sa);
                            let rxa = p1.0 - head * (ux * ca + uy * sa);
                            let rya = p1.1 - head * (uy * ca - ux * sa);
                            cr.move_to(p1.0, p1.1);
                            cr.line_to(lxa, lya);
                            cr.line_to(rxa, rya);
                            cr.close_path();
                            let _ = cr.fill();
                        }
                    }
                }

                // ── Selected drawing item highlight ─────────────
                if let Some(sel_idx) = selected_drawing_draw.get() {
                    let items = drawing_items_draw.borrow();
                    if let Some(item) = items.get(sel_idx) {
                        use crate::model::clip::DrawingKind;
                        let (cx0, cy0, cx1, cy1) = match item.kind {
                            DrawingKind::Stroke => {
                                let mut min_x = f64::MAX;
                                let mut min_y = f64::MAX;
                                let mut max_x = f64::MIN;
                                let mut max_y = f64::MIN;
                                for (nx, ny) in &item.points {
                                    let x = vx + nx * vw;
                                    let y = vy + ny * vh;
                                    min_x = min_x.min(x);
                                    min_y = min_y.min(y);
                                    max_x = max_x.max(x);
                                    max_y = max_y.max(y);
                                }
                                (min_x, min_y, max_x, max_y)
                            }
                            _ => {
                                let p0 = item.points[0];
                                let p1 = *item.points.last().unwrap();
                                let a = (vx + p0.0 * vw, vy + p0.1 * vh);
                                let b = (vx + p1.0 * vw, vy + p1.1 * vh);
                                (a.0.min(b.0), a.1.min(b.1), a.0.max(b.0), a.1.max(b.1))
                            }
                        };
                        let pad = 4.0;
                        cr.save().ok();
                        cr.set_source_rgba(0.2, 0.8, 1.0, 0.9);
                        cr.set_line_width(1.5);
                        cr.set_dash(&[6.0, 4.0], 0.0);
                        cr.rectangle(
                            cx0 - pad,
                            cy0 - pad,
                            (cx1 - cx0) + pad * 2.0,
                            (cy1 - cy0) + pad * 2.0,
                        );
                        let _ = cr.stroke();
                        cr.restore().ok();
                    }
                }

                // Always draw: dark vignette + canvas border
                draw_outside_vignette(cr, ww as f64, wh as f64, vx, vy, vw, vh);
                draw_frame_border(cr, vx, vy, vw, vh);

                // ── Background-encode feedback ──────────────────
                // Visible in *any* tool while a drawing animation
                // WebM is baking; keeps users from thinking the
                // static PNG they see is the final render.
                if !draw_active
                    && crate::media::drawing_render::drawing_encode_is_pending()
                {
                    let note = "Baking drawing animation…";
                    cr.select_font_face(
                        "Sans",
                        gtk4::cairo::FontSlant::Normal,
                        gtk4::cairo::FontWeight::Bold,
                    );
                    cr.set_font_size(12.0);
                    let ext = cr.text_extents(note).unwrap_or(
                        gtk4::cairo::TextExtents::new(0.0, 0.0, 180.0, 12.0, 0.0, 0.0),
                    );
                    let pill_w = ext.width() + 24.0;
                    let pill_h = ext.height() + 12.0;
                    let px = vx + 12.0;
                    let py = vy + 12.0;
                    cr.set_source_rgba(0.0, 0.0, 0.0, 0.72);
                    cr.rectangle(px, py, pill_w, pill_h);
                    let _ = cr.fill();
                    cr.set_source_rgba(0.2, 0.8, 1.0, 0.95);
                    cr.move_to(px + 12.0, py + 6.0 + ext.height());
                    let _ = cr.show_text(note);
                }

                // ── Draw-tool HUD: show current brush state ─────
                if draw_active {
                    use crate::model::clip::DrawingKind;
                    let kind_label = match drawing_kind_draw.get() {
                        DrawingKind::Stroke => "Stroke",
                        DrawingKind::Rectangle => "Rectangle",
                        DrawingKind::Ellipse => "Ellipse",
                        DrawingKind::Arrow => "Arrow",
                    };
                    let color = drawing_color_d.get();
                    let w = drawing_width_d.get();
                    let fill_text = match drawing_fill_draw.get() {
                        Some(_) => " +fill",
                        None => "",
                    };
                    // Signal a background WebM bake in flight so the
                    // user knows the static PNG they're seeing isn't
                    // the final state.
                    let baking_text =
                        if crate::media::drawing_render::drawing_encode_is_pending() {
                            "   • baking animation…"
                        } else {
                            ""
                        };
                    let label = format!(
                        "Draw [{kind_label}]  •  #{:06X}  •  {w:.0}px{fill_text}{baking_text}   (1/2/3/4 pick shape · click to select · Del removes)",
                        color >> 8
                    );
                    // Dark pill in the canvas top-left.
                    let pad_x = 12.0;
                    let pad_y = 6.0;
                    cr.select_font_face(
                        "Sans",
                        gtk4::cairo::FontSlant::Normal,
                        gtk4::cairo::FontWeight::Bold,
                    );
                    cr.set_font_size(12.0);
                    let extents = cr
                        .text_extents(&label)
                        .unwrap_or(gtk4::cairo::TextExtents::new(
                            0.0, 0.0, 200.0, 12.0, 0.0, 0.0,
                        ));
                    let pill_w = extents.width() + pad_x * 2.0;
                    let pill_h = extents.height() + pad_y * 2.0;
                    let pill_x = vx + 12.0;
                    let pill_y = vy + 12.0;
                    cr.set_source_rgba(0.0, 0.0, 0.0, 0.72);
                    cr.rectangle(pill_x, pill_y, pill_w, pill_h);
                    let _ = cr.fill();
                    // Color chip.
                    let chip = (pill_h - pad_y * 2.0).max(8.0);
                    let chip_x = pill_x + pad_x * 0.4;
                    let chip_y = pill_y + (pill_h - chip) * 0.5;
                    cr.set_source_rgba(
                        ((color >> 24) & 0xFF) as f64 / 255.0,
                        ((color >> 16) & 0xFF) as f64 / 255.0,
                        ((color >> 8) & 0xFF) as f64 / 255.0,
                        (color & 0xFF) as f64 / 255.0,
                    );
                    cr.rectangle(chip_x, chip_y, chip, chip);
                    let _ = cr.fill();
                    cr.set_source_rgba(1.0, 1.0, 1.0, 0.95);
                    cr.move_to(
                        pill_x + pad_x + chip + 6.0,
                        pill_y + pad_y + extents.height(),
                    );
                    let _ = cr.show_text(&label);
                }
                // Only draw clip handles when clip doesn't fill the canvas exactly
                let s = scale.get();
                let px = position_x.get();
                let py = position_y.get();
                draw_overlay(
                    cr,
                    vx,
                    vy,
                    vw,
                    vh,
                    s,
                    px,
                    py,
                    adjustment_mode.get(),
                    rotation.get(),
                    crop_left.get(),
                    crop_right.get(),
                    crop_top.get(),
                    crop_bottom.get(),
                    proj_w.get(),
                    proj_h.get(),
                    content_inset_x.get(),
                    content_inset_y.get(),
                );
                // Draw mask outline if mask is enabled.
                if mask_enabled.get() {
                    draw_mask_outline(
                        cr,
                        vx,
                        vy,
                        vw,
                        vh,
                        s,
                        px,
                        py,
                        adjustment_mode.get(),
                        rotation.get(),
                        mask_shape.get(),
                        mask_cx.get(),
                        mask_cy.get(),
                        mask_hw.get(),
                        mask_hh.get(),
                        mask_rotation_d.get(),
                        &mask_path_points_clone.borrow(),
                    );
                }
                if tracking_region_enabled.get() {
                    draw_tracking_region_outline(
                        cr,
                        vx,
                        vy,
                        vw,
                        vh,
                        s,
                        px,
                        py,
                        rotation.get(),
                        tracking_center_x.get(),
                        tracking_center_y.get(),
                        tracking_width.get(),
                        tracking_height.get(),
                        tracking_rotation_d.get(),
                        tracking_region_editing.get(),
                    );
                }
                // ── SAM prompt box live preview ─────────────────
                // Draw a semi-transparent blue rectangle between
                // the drag start and the current cursor position
                // while the user is drawing a SAM prompt box.
                if sam_prompt_mode_draw.get() {
                    if let (Some((sx, sy)), Some((cx, cy))) = (
                        sam_prompt_start_draw.get(),
                        sam_prompt_current_draw.get(),
                    ) {
                        let rx = sx.min(cx);
                        let ry = sy.min(cy);
                        let rw = (cx - sx).abs();
                        let rh = (cy - sy).abs();
                        // Semi-transparent blue fill.
                        cr.set_source_rgba(0.2, 0.5, 1.0, 0.25);
                        cr.rectangle(rx, ry, rw, rh);
                        let _ = cr.fill();
                        // Blue outline.
                        cr.set_source_rgba(0.2, 0.5, 1.0, 0.85);
                        cr.set_line_width(1.5);
                        cr.rectangle(rx, ry, rw, rh);
                        let _ = cr.stroke();
                    }
                }
            });
        }

        // Drag gesture -----------------------------------------------------
        let drag_state: Rc<RefCell<Option<DragState>>> = Rc::new(RefCell::new(None));
        let on_change = Rc::new(on_change);
        let on_rotate_change = Rc::new(on_rotate_change);
        let on_crop_change = Rc::new(on_crop_change);
        let on_drag_begin = Rc::new(on_drag_begin);
        let on_mask_path_change = Rc::new(on_mask_path_change);
        let on_tracking_region_change = Rc::new(on_tracking_region_change);

        let gesture = gtk::GestureDrag::new();
        gesture.set_button(1); // left button only

        // drag_begin: hit-test → choose handle
        {
            let scale = scale.clone();
            let position_x = position_x.clone();
            let position_y = position_y.clone();
            let rotation = rotation.clone();
            let crop_left = crop_left.clone();
            let crop_right = crop_right.clone();
            let crop_top = crop_top.clone();
            let crop_bottom = crop_bottom.clone();
            let selected = selected.clone();
            let proj_w = proj_w.clone();
            let proj_h = proj_h.clone();
            let content_inset_x = content_inset_x.clone();
            let content_inset_y = content_inset_y.clone();
            let picture = picture.clone();
            let drag_state = drag_state.clone();
            let on_drag_begin = on_drag_begin.clone();
            let da_ref = da.clone();
            let canvas_widget = canvas_widget.clone();
            let adjustment_mode = adjustment_mode.clone();
            let mask_enabled_d = mask_enabled.clone();
            let mask_shape_d = mask_shape.clone();
            let mask_path_points_d = mask_path_points.clone();
            let tracking_region_enabled_d = tracking_region_enabled.clone();
            let tracking_region_editing_d = tracking_region_editing.clone();
            let tracking_center_x_d = tracking_center_x.clone();
            let tracking_center_y_d = tracking_center_y.clone();
            let tracking_width_d = tracking_width.clone();
            let tracking_height_d = tracking_height.clone();
            let tracking_rotation_d = tracking_rotation.clone();
            let sam_prompt_mode_d = sam_prompt_mode.clone();
            let sam_prompt_start_d = sam_prompt_start.clone();
            let sam_prompt_current_d = sam_prompt_current.clone();

            let active_tool_d = active_tool.clone();
            let drawing_points_d = current_drawing_points.clone();
            let drawing_color_d = drawing_color.clone();
            let drawing_width_d = drawing_width.clone();

            gesture.connect_drag_begin(move |_g, sx, sy| {
                // Draw tool always captures — no clip selection
                // required because drawing can create a new clip at
                // the playhead if none exists.
                let draw_active = active_tool_d.get() == crate::ui::timeline::ActiveTool::Draw;
                if !draw_active && !selected.get() {
                    return;
                }
                da_ref.grab_focus();

                // ── Draw Tool handling ──────────────────────────
                if draw_active {
                    drawing_points_d.borrow_mut().clear();
                    drawing_points_d.borrow_mut().push((sx, sy));
                    on_drag_begin();
                    *drag_state.borrow_mut() = Some(DragState {
                        handle: Handle::Drawing,
                        start_wx: sx,
                        start_wy: sy,
                        start_scale: scale.get(),
                        start_px: position_x.get(),
                        start_py: position_y.get(),
                        start_crop_left: crop_left.get(),
                        start_crop_right: crop_right.get(),
                        start_crop_top: crop_top.get(),
                        start_crop_bottom: crop_bottom.get(),
                        proj_w: proj_w.get(),
                        proj_h: proj_h.get(),
                        vx: 0.0,
                        vy: 0.0,
                        vw: 0.0,
                        vh: 0.0,
                        start_path_point: None,
                        start_tracking_cx: 0.0,
                        start_tracking_cy: 0.0,
                        start_tracking_width: 0.0,
                        start_tracking_height: 0.0,
                        drawing_color: drawing_color_d.get(),
                        drawing_width: drawing_width_d.get(),
                    });
                    // Need video rect for normalized coords later
                    let ww = da_ref.width();
                    let wh = da_ref.height();
                    let (vx_full, vy_full, vw_full, vh_full) = canvas_video_rect(
                        &da_ref,
                        &canvas_widget,
                        ww,
                        wh,
                        proj_w.get(),
                        proj_h.get(),
                    );
                    let ix = content_inset_x.get();
                    let iy = content_inset_y.get();
                    if let Some(ds) = drag_state.borrow_mut().as_mut() {
                        ds.vx = vx_full + vw_full * ix;
                        ds.vy = vy_full + vh_full * iy;
                        ds.vw = vw_full * (1.0 - 2.0 * ix);
                        ds.vh = vh_full * (1.0 - 2.0 * iy);
                    }
                    da_ref.queue_draw();
                    return;
                }

                // SAM prompt mode short-circuits the whole hit-test
                // chain. The user wants to draw a rectangle anywhere
                // over the clip — we don't care about existing
                // handles. Capture (sx, sy) as the drag start, seed
                // the current drag position, and install a minimal
                // DragState with `Handle::SamPromptBox` so
                // drag_update/drag_end can dispatch on it.
                if sam_prompt_mode_d.get() {
                    sam_prompt_start_d.set(Some((sx, sy)));
                    sam_prompt_current_d.set(Some((sx, sy)));
                    on_drag_begin();
                    *drag_state.borrow_mut() = Some(DragState {
                        handle: Handle::SamPromptBox,
                        start_wx: sx,
                        start_wy: sy,
                        start_scale: scale.get(),
                        start_px: position_x.get(),
                        start_py: position_y.get(),
                        start_crop_left: crop_left.get(),
                        start_crop_right: crop_right.get(),
                        start_crop_top: crop_top.get(),
                        start_crop_bottom: crop_bottom.get(),
                        proj_w: proj_w.get(),
                        proj_h: proj_h.get(),
                        // Video-rect is needed in drag_end for the
                        // widget → normalized coord conversion, so
                        // cache it here the same way the normal
                        // path below does.
                        vx: 0.0,
                        vy: 0.0,
                        vw: 0.0,
                        vh: 0.0,
                        start_path_point: None,
                        start_tracking_cx: 0.0,
                        start_tracking_cy: 0.0,
                        start_tracking_width: 0.0,
                        start_tracking_height: 0.0,
                        drawing_color: drawing_color_d.get(),
                        drawing_width: drawing_width_d.get(),
                    });
                    // Fill in the cached video rect now — it'll be
                    // read by drag_end to build the normalized box.
                    let ww = da_ref.width();
                    let wh = da_ref.height();
                    let (vx_full, vy_full, vw_full, vh_full) = canvas_video_rect(
                        &da_ref,
                        &canvas_widget,
                        ww,
                        wh,
                        proj_w.get(),
                        proj_h.get(),
                    );
                    let ix = content_inset_x.get();
                    let iy = content_inset_y.get();
                    if let Some(ds) = drag_state.borrow_mut().as_mut() {
                        ds.vx = vx_full + vw_full * ix;
                        ds.vy = vy_full + vh_full * iy;
                        ds.vw = vw_full * (1.0 - 2.0 * ix);
                        ds.vh = vh_full * (1.0 - 2.0 * iy);
                    }
                    da_ref.queue_draw();
                    return;
                }
                let ww = da_ref.width();
                let wh = da_ref.height();
                let _ = &picture; // kept for potential future use
                let (vx_full, vy_full, vw_full, vh_full) =
                    canvas_video_rect(&da_ref, &canvas_widget, ww, wh, proj_w.get(), proj_h.get());
                // Shrink to content area (excluding letterbox)
                let ix = content_inset_x.get();
                let iy = content_inset_y.get();
                let vx = vx_full + vw_full * ix;
                let vy = vy_full + vh_full * iy;
                let vw = vw_full * (1.0 - 2.0 * ix);
                let vh = vh_full * (1.0 - 2.0 * iy);
                let s = scale.get();
                let px = position_x.get();
                let py = position_y.get();
                let rot_rad = (-rotation.get()).to_radians();

                let adjustment_mode = adjustment_mode.get();
                let (cx, cy, clip_w, clip_h, left, top) =
                    clip_canvas_geometry(vx, vy, vw, vh, s, px, py, adjustment_mode);
                let hw = clip_w / 2.0;
                let hh = clip_h / 2.0;
                let right = left + clip_w;
                let bottom = top + clip_h;
                let (crop_l_px, crop_r_px, crop_t_px, crop_b_px) = crop_insets_to_overlay_px(
                    crop_left.get(),
                    crop_right.get(),
                    crop_top.get(),
                    crop_bottom.get(),
                    proj_w.get(),
                    proj_h.get(),
                    right - left,
                    bottom - top,
                );
                let crop_left_x = left + crop_l_px;
                let crop_right_x = right - crop_r_px;
                let crop_top_y = top + crop_t_px;
                let crop_bottom_y = bottom - crop_b_px;

                let to_world = |lx: f64, ly: f64| -> (f64, f64) {
                    rotate_point_about(cx + lx, cy + ly, cx, cy, rot_rad)
                };
                let corners = [
                    {
                        let (x, y) = to_world(-hw, -hh);
                        (x, y, Handle::TopLeft)
                    },
                    {
                        let (x, y) = to_world(hw, -hh);
                        (x, y, Handle::TopRight)
                    },
                    {
                        let (x, y) = to_world(-hw, hh);
                        (x, y, Handle::BottomLeft)
                    },
                    {
                        let (x, y) = to_world(hw, hh);
                        (x, y, Handle::BottomRight)
                    },
                ];
                let rotate_handle = {
                    let (x, y) = to_world(0.0, -hh - 24.0);
                    (x, y, Handle::Rotate)
                };
                let crop_edges = [
                    (
                        {
                            let (x, _) = to_world(
                                ((crop_left_x + crop_right_x) / 2.0) - cx,
                                crop_top_y - cy,
                            );
                            x
                        },
                        {
                            let (_, y) = to_world(
                                ((crop_left_x + crop_right_x) / 2.0) - cx,
                                crop_top_y - cy,
                            );
                            y
                        },
                        Handle::CropTop,
                    ),
                    (
                        {
                            let (x, _) = to_world(
                                ((crop_left_x + crop_right_x) / 2.0) - cx,
                                crop_bottom_y - cy,
                            );
                            x
                        },
                        {
                            let (_, y) = to_world(
                                ((crop_left_x + crop_right_x) / 2.0) - cx,
                                crop_bottom_y - cy,
                            );
                            y
                        },
                        Handle::CropBottom,
                    ),
                    (
                        {
                            let (x, _) = to_world(
                                crop_left_x - cx,
                                ((crop_top_y + crop_bottom_y) / 2.0) - cy,
                            );
                            x
                        },
                        {
                            let (_, y) = to_world(
                                crop_left_x - cx,
                                ((crop_top_y + crop_bottom_y) / 2.0) - cy,
                            );
                            y
                        },
                        Handle::CropLeft,
                    ),
                    (
                        {
                            let (x, _) = to_world(
                                crop_right_x - cx,
                                ((crop_top_y + crop_bottom_y) / 2.0) - cy,
                            );
                            x
                        },
                        {
                            let (_, y) = to_world(
                                crop_right_x - cx,
                                ((crop_top_y + crop_bottom_y) / 2.0) - cy,
                            );
                            y
                        },
                        Handle::CropRight,
                    ),
                ];

                let mut handle = Handle::None;
                let mut start_path_point: Option<crate::model::clip::BezierPoint> = None;
                let mut start_tracking_cx = tracking_center_x_d.get();
                let mut start_tracking_cy = tracking_center_y_d.get();
                let mut start_tracking_width = tracking_width_d.get();
                let mut start_tracking_height = tracking_height_d.get();

                // Tracking region hit-test takes priority while region editing is active.
                if tracking_region_enabled_d.get() && tracking_region_editing_d.get() {
                    let clip_cx_w = vx + vw / 2.0 + px * vw * (1.0 - s) / 2.0;
                    let clip_cy_w = vy + vh / 2.0 + py * vh * (1.0 - s) / 2.0;
                    let clip_w = vw * s;
                    let clip_h = vh * s;
                    let clip_left_w = clip_cx_w - clip_w / 2.0;
                    let clip_top_w = clip_cy_w - clip_h / 2.0;
                    let clip_rot = (-rotation.get()).to_radians();
                    let region_cx = clip_left_w + tracking_center_x_d.get() * clip_w;
                    let region_cy = clip_top_w + tracking_center_y_d.get() * clip_h;
                    let region_hw = tracking_width_d.get() * clip_w;
                    let region_hh = tracking_height_d.get() * clip_h;
                    let region_rot = tracking_rotation_d.get().to_radians();
                    let region_to_world = |lx: f64, ly: f64| -> (f64, f64) {
                        let (rx, ry) = rotate_point_about(
                            region_cx + lx,
                            region_cy + ly,
                            region_cx,
                            region_cy,
                            region_rot,
                        );
                        rotate_point_about(rx, ry, clip_cx_w, clip_cy_w, clip_rot)
                    };
                    let region_corners = [
                        {
                            let (x, y) = region_to_world(-region_hw, -region_hh);
                            (x, y, Handle::TrackingTopLeft)
                        },
                        {
                            let (x, y) = region_to_world(region_hw, -region_hh);
                            (x, y, Handle::TrackingTopRight)
                        },
                        {
                            let (x, y) = region_to_world(-region_hw, region_hh);
                            (x, y, Handle::TrackingBottomLeft)
                        },
                        {
                            let (x, y) = region_to_world(region_hw, region_hh);
                            (x, y, Handle::TrackingBottomRight)
                        },
                    ];
                    for (hx, hy, h) in &region_corners {
                        let d = ((sx - hx).powi(2) + (sy - hy).powi(2)).sqrt();
                        if d <= HANDLE_HIT {
                            handle = *h;
                            break;
                        }
                    }
                    if handle == Handle::None {
                        let (ux, uy) = unrotate_point_about(sx, sy, clip_cx_w, clip_cy_w, clip_rot);
                        let (ux, uy) =
                            unrotate_point_about(ux, uy, region_cx, region_cy, region_rot);
                        let inside_x = ux >= region_cx - region_hw && ux <= region_cx + region_hw;
                        let inside_y = uy >= region_cy - region_hh && uy <= region_cy + region_hh;
                        if inside_x && inside_y {
                            handle = Handle::TrackingPan;
                        }
                    }
                }

                // Path mask point hit-test (highest priority when path mask is active).
                if handle == Handle::None && mask_enabled_d.get() && mask_shape_d.get() == 2 {
                    let pts = mask_path_points_d.borrow();
                    if pts.len() >= 3 {
                        // Compute clip region for mapping normalized→widget coords.
                        let (clip_cx_w, clip_cy_w, clip_w, clip_h, clip_left_w, clip_top_w) =
                            clip_canvas_geometry(vx, vy, vw, vh, s, px, py, adjustment_mode);
                        // The drawing applies clip rotation around clip_cx_w/clip_cy_w,
                        // so drawn positions are rotated.  Map normalized→widget using
                        // the same rotation so hit-test matches drawn positions.
                        let clip_rot = (-rotation.get()).to_radians();
                        let map_pt = |nx: f64, ny: f64| -> (f64, f64) {
                            let wx = clip_left_w + nx * clip_w;
                            let wy = clip_top_w + ny * clip_h;
                            rotate_point_about(wx, wy, clip_cx_w, clip_cy_w, clip_rot)
                        };

                        // Check handles first (they are smaller targets, test first).
                        for (i, p) in pts.iter().enumerate() {
                            if p.handle_in_x.abs() > 1e-6 || p.handle_in_y.abs() > 1e-6 {
                                let (hx, hy) = map_pt(p.x + p.handle_in_x, p.y + p.handle_in_y);
                                if ((sx - hx).powi(2) + (sy - hy).powi(2)).sqrt() <= HANDLE_HIT {
                                    handle = Handle::MaskPathHandleIn(i);
                                    start_path_point = Some(p.clone());
                                    break;
                                }
                            }
                            if p.handle_out_x.abs() > 1e-6 || p.handle_out_y.abs() > 1e-6 {
                                let (hx, hy) = map_pt(p.x + p.handle_out_x, p.y + p.handle_out_y);
                                if ((sx - hx).powi(2) + (sy - hy).powi(2)).sqrt() <= HANDLE_HIT {
                                    handle = Handle::MaskPathHandleOut(i);
                                    start_path_point = Some(p.clone());
                                    break;
                                }
                            }
                        }
                        // Then check anchor points.
                        if handle == Handle::None {
                            for (i, p) in pts.iter().enumerate() {
                                let (ax, ay) = map_pt(p.x, p.y);
                                if ((sx - ax).powi(2) + (sy - ay).powi(2)).sqrt() <= HANDLE_HIT {
                                    handle = Handle::MaskPathAnchor(i);
                                    start_path_point = Some(p.clone());
                                    break;
                                }
                            }
                        }
                    }
                }

                // Transform handle hit-tests (only if no path handle was hit).
                if handle == Handle::None {
                    {
                        let d = ((sx - rotate_handle.0).powi(2) + (sy - rotate_handle.1).powi(2))
                            .sqrt();
                        if d <= HANDLE_HIT {
                            handle = rotate_handle.2;
                        }
                    }
                    if handle == Handle::None {
                        for (hx, hy, h) in &corners {
                            let d = ((sx - hx).powi(2) + (sy - hy).powi(2)).sqrt();
                            if d <= HANDLE_HIT {
                                handle = *h;
                                break;
                            }
                        }
                    }
                    if handle == Handle::None {
                        for (hx, hy, h) in &crop_edges {
                            let d = ((sx - hx).powi(2) + (sy - hy).powi(2)).sqrt();
                            if d <= HANDLE_HIT {
                                handle = *h;
                                break;
                            }
                        }
                    }

                    if handle == Handle::None {
                        // Inside the clip bounds → pan
                        let (lx, ly) = unrotate_point_about(sx, sy, cx, cy, rot_rad);
                        if lx >= left && lx <= right && ly >= top && ly <= bottom {
                            handle = Handle::Pan;
                        }
                    }
                } // end: if handle == Handle::None (path mask guard)

                if handle != Handle::None {
                    on_drag_begin();
                    *drag_state.borrow_mut() = Some(DragState {
                        handle,
                        start_wx: sx,
                        start_wy: sy,
                        start_scale: s,
                        start_px: px,
                        start_py: py,
                        start_crop_left: crop_left.get(),
                        start_crop_right: crop_right.get(),
                        start_crop_top: crop_top.get(),
                        start_crop_bottom: crop_bottom.get(),
                        proj_w: proj_w.get(),
                        proj_h: proj_h.get(),
                        vx,
                        vy,
                        vw,
                        vh,
                        start_path_point,
                        start_tracking_cx,
                        start_tracking_cy,
                        start_tracking_width,
                        start_tracking_height,
                        drawing_color: drawing_color_d.get(),
                        drawing_width: drawing_width_d.get(),
                    });
                }
            });
        }

        // drag_update: apply delta
        {
            let scale = scale.clone();
            let position_x = position_x.clone();
            let position_y = position_y.clone();
            let rotation_for_drag = rotation.clone();
            let crop_left = crop_left.clone();
            let crop_right = crop_right.clone();
            let crop_top = crop_top.clone();
            let crop_bottom = crop_bottom.clone();
            let drag_state = drag_state.clone();
            let on_change = on_change.clone();
            let on_rotate_change = on_rotate_change.clone();
            let on_crop_change = on_crop_change.clone();
            let on_mask_path_change = on_mask_path_change.clone();
            let on_tracking_region_change = on_tracking_region_change.clone();
            let mask_path_points_drag = mask_path_points.clone();
            let mask_enabled_drag = mask_enabled.clone();
            let adjustment_mode = adjustment_mode.clone();
            let tracking_region_enabled_drag = tracking_region_enabled.clone();
            let tracking_region_editing_drag = tracking_region_editing.clone();
            let sam_prompt_current_drag = sam_prompt_current.clone();
            let tracking_center_x_drag = tracking_center_x.clone();
            let tracking_center_y_drag = tracking_center_y.clone();
            let tracking_width_drag = tracking_width.clone();
            let tracking_height_drag = tracking_height.clone();
            let tracking_rotation_drag = tracking_rotation.clone();
            let da_ref = da.clone();

            let drawing_points_drag = current_drawing_points.clone();

            gesture.connect_drag_update(move |g, off_x, off_y| {
                let ds_borrow = drag_state.borrow_mut();
                let Some(ref ds) = *ds_borrow else {
                    return;
                };

                // ── Draw Tool handling ──────────────────────────
                if ds.handle == Handle::Drawing {
                    let cur_x = ds.start_wx + off_x;
                    let cur_y = ds.start_wy + off_y;
                    drawing_points_drag.borrow_mut().push((cur_x, cur_y));
                    da_ref.queue_draw();
                    return;
                }

                let rot_rad = (-rotation_for_drag.get()).to_radians();
                let local_dx = off_x * rot_rad.cos() + off_y * rot_rad.sin();
                let local_dy = -off_x * rot_rad.sin() + off_y * rot_rad.cos();

                match ds.handle {
                    Handle::Drawing => {
                        // Drawing drag is handled separately above via drag_update_drawing.
                    }
                    Handle::Rotate => {
                        let (clip_cx, clip_cy, _, _, _, _) = clip_canvas_geometry(
                            ds.vx,
                            ds.vy,
                            ds.vw,
                            ds.vh,
                            ds.start_scale,
                            ds.start_px,
                            ds.start_py,
                            adjustment_mode.get(),
                        );
                        let cur_x = ds.start_wx + off_x;
                        let cur_y = ds.start_wy + off_y;
                        let mut deg = ((cur_y - clip_cy).atan2(cur_x - clip_cx).to_degrees()
                            + 90.0)
                            .rem_euclid(360.0);
                        if deg > 180.0 {
                            deg -= 360.0;
                        }
                        let deg = -(deg.round().clamp(
                            crate::model::transform_bounds::ROTATE_MIN_DEG,
                            crate::model::transform_bounds::ROTATE_MAX_DEG,
                        ));
                        rotation_for_drag.set(deg);
                        on_rotate_change(deg as i32);
                    }
                    Handle::Pan => {
                        // Position sensitivity: d(pos_x) = off_x / (vw*(1-scale)/2)
                        // This gives 1:1 pixel movement of the clip centre in canvas space.
                        // For scale>1 the denominator is negative, so dragging right decreases
                        // pos_x — which is correct because at scale>1, pos_x controls which
                        // part of the clip is visible (higher pos_x = viewport panned right
                        // = clip appears shifted left).
                        //
                        // Clamp at POSITION_MIN/POSITION_MAX so users can drag a
                        // clip fully off-canvas; the rendering math
                        // (`apply_zoom_to_slot` and the export
                        // `build_scale_translate_filter`) already handles overflow
                        // by cropping/padding past the frame edges.
                        use crate::model::transform_bounds::{POSITION_MAX, POSITION_MIN};
                        let h_range =
                            clip_position_range(ds.vw, ds.start_scale, adjustment_mode.get());
                        let v_range =
                            clip_position_range(ds.vh, ds.start_scale, adjustment_mode.get());
                        let new_px = if h_range.abs() > 0.5 {
                            (ds.start_px + off_x / h_range).clamp(POSITION_MIN, POSITION_MAX)
                        } else {
                            ds.start_px
                        };
                        let new_py = if v_range.abs() > 0.5 {
                            (ds.start_py + off_y / v_range).clamp(POSITION_MIN, POSITION_MAX)
                        } else {
                            ds.start_py
                        };
                        position_x.set(new_px);
                        position_y.set(new_py);
                        on_change(scale.get(), new_px, new_py);
                    }
                    Handle::TopLeft
                    | Handle::TopRight
                    | Handle::BottomLeft
                    | Handle::BottomRight => {
                        // Scale: ratio of distance from clip centre to current vs. start.
                        // Holding Shift uses constrained scaling (same X/Y scale factor).
                        let (clip_cx, clip_cy, _, _, _, _) = clip_canvas_geometry(
                            ds.vx,
                            ds.vy,
                            ds.vw,
                            ds.vh,
                            ds.start_scale,
                            ds.start_px,
                            ds.start_py,
                            adjustment_mode.get(),
                        );
                        let cur_x = ds.start_wx + off_x;
                        let cur_y = ds.start_wy + off_y;
                        let orig_dx = (ds.start_wx - clip_cx).abs();
                        let orig_dy = (ds.start_wy - clip_cy).abs();
                        let now_dx = (cur_x - clip_cx).abs();
                        let now_dy = (cur_y - clip_cy).abs();
                        if orig_dx > 1.0 || orig_dy > 1.0 {
                            let sx = if orig_dx > 1.0 { now_dx / orig_dx } else { 1.0 };
                            let sy = if orig_dy > 1.0 { now_dy / orig_dy } else { 1.0 };
                            let shift = g
                                .current_event_state()
                                .contains(gtk::gdk::ModifierType::SHIFT_MASK);
                            let factor = if shift { sx.max(sy) } else { (sx + sy) * 0.5 };
                            let new_s = (ds.start_scale * factor).clamp(
                                crate::model::transform_bounds::SCALE_MIN,
                                crate::model::transform_bounds::SCALE_MAX,
                            );
                            scale.set(new_s);
                            on_change(new_s, position_x.get(), position_y.get());
                        }
                    }
                    Handle::CropLeft => {
                        let clip_w = (ds.vw * ds.start_scale).max(1.0);
                        let delta = (local_dx * ds.proj_w as f64 / clip_w).round() as i32;
                        let mut new_left = ds.start_crop_left + delta;
                        let max_left = (ds.proj_w as i32 - 2 - crop_right.get()).clamp(0, CROP_MAX);
                        new_left = new_left.clamp(0, max_left);
                        crop_left.set(new_left);
                        on_crop_change(
                            new_left,
                            crop_right.get(),
                            crop_top.get(),
                            crop_bottom.get(),
                        );
                    }
                    Handle::CropRight => {
                        let clip_w = (ds.vw * ds.start_scale).max(1.0);
                        let delta = (-local_dx * ds.proj_w as f64 / clip_w).round() as i32;
                        let mut new_right = ds.start_crop_right + delta;
                        let max_right = (ds.proj_w as i32 - 2 - crop_left.get()).clamp(0, CROP_MAX);
                        new_right = new_right.clamp(0, max_right);
                        crop_right.set(new_right);
                        on_crop_change(
                            crop_left.get(),
                            new_right,
                            crop_top.get(),
                            crop_bottom.get(),
                        );
                    }
                    Handle::CropTop => {
                        let clip_h = (ds.vh * ds.start_scale).max(1.0);
                        let delta = (local_dy * ds.proj_h as f64 / clip_h).round() as i32;
                        let mut new_top = ds.start_crop_top + delta;
                        let max_top = (ds.proj_h as i32 - 2 - crop_bottom.get()).clamp(0, CROP_MAX);
                        new_top = new_top.clamp(0, max_top);
                        crop_top.set(new_top);
                        on_crop_change(
                            crop_left.get(),
                            crop_right.get(),
                            new_top,
                            crop_bottom.get(),
                        );
                    }
                    Handle::CropBottom => {
                        let clip_h = (ds.vh * ds.start_scale).max(1.0);
                        let delta = (-local_dy * ds.proj_h as f64 / clip_h).round() as i32;
                        let mut new_bottom = ds.start_crop_bottom + delta;
                        let max_bottom = (ds.proj_h as i32 - 2 - crop_top.get()).clamp(0, CROP_MAX);
                        new_bottom = new_bottom.clamp(0, max_bottom);
                        crop_bottom.set(new_bottom);
                        on_crop_change(
                            crop_left.get(),
                            crop_right.get(),
                            crop_top.get(),
                            new_bottom,
                        );
                    }
                    Handle::TrackingPan => {
                        if tracking_region_enabled_drag.get() && tracking_region_editing_drag.get()
                        {
                            let clip_w = (ds.vw * ds.start_scale).max(1.0);
                            let clip_h = (ds.vh * ds.start_scale).max(1.0);
                            let new_cx = (ds.start_tracking_cx + local_dx / clip_w)
                                .clamp(ds.start_tracking_width, 1.0 - ds.start_tracking_width);
                            let new_cy = (ds.start_tracking_cy + local_dy / clip_h)
                                .clamp(ds.start_tracking_height, 1.0 - ds.start_tracking_height);
                            tracking_center_x_drag.set(new_cx);
                            tracking_center_y_drag.set(new_cy);
                            on_tracking_region_change(
                                new_cx,
                                new_cy,
                                tracking_width_drag.get(),
                                tracking_height_drag.get(),
                            );
                        }
                    }
                    Handle::TrackingTopLeft
                    | Handle::TrackingTopRight
                    | Handle::TrackingBottomLeft
                    | Handle::TrackingBottomRight => {
                        if tracking_region_enabled_drag.get() && tracking_region_editing_drag.get()
                        {
                            let clip_cx = ds.vx
                                + ds.vw / 2.0
                                + ds.start_px * ds.vw * (1.0 - ds.start_scale) / 2.0;
                            let clip_cy = ds.vy
                                + ds.vh / 2.0
                                + ds.start_py * ds.vh * (1.0 - ds.start_scale) / 2.0;
                            let clip_w = (ds.vw * ds.start_scale).max(1.0);
                            let clip_h = (ds.vh * ds.start_scale).max(1.0);
                            let clip_left = clip_cx - clip_w / 2.0;
                            let clip_top = clip_cy - clip_h / 2.0;
                            let region_cx = clip_left + ds.start_tracking_cx * clip_w;
                            let region_cy = clip_top + ds.start_tracking_cy * clip_h;
                            let cur_x = ds.start_wx + off_x;
                            let cur_y = ds.start_wy + off_y;
                            let (cur_x, cur_y) =
                                unrotate_point_about(cur_x, cur_y, clip_cx, clip_cy, rot_rad);
                            let (cur_x, cur_y) = unrotate_point_about(
                                cur_x,
                                cur_y,
                                region_cx,
                                region_cy,
                                tracking_rotation_drag.get().to_radians(),
                            );
                            let max_half_width = ds
                                .start_tracking_cx
                                .min(1.0 - ds.start_tracking_cx)
                                .max(0.05);
                            let max_half_height = ds
                                .start_tracking_cy
                                .min(1.0 - ds.start_tracking_cy)
                                .max(0.05);
                            let new_width =
                                ((cur_x - region_cx).abs() / clip_w).clamp(0.05, max_half_width);
                            let new_height =
                                ((cur_y - region_cy).abs() / clip_h).clamp(0.05, max_half_height);
                            tracking_width_drag.set(new_width);
                            tracking_height_drag.set(new_height);
                            on_tracking_region_change(
                                tracking_center_x_drag.get(),
                                tracking_center_y_drag.get(),
                                new_width,
                                new_height,
                            );
                        }
                    }
                    Handle::MaskPathAnchor(idx)
                    | Handle::MaskPathHandleIn(idx)
                    | Handle::MaskPathHandleOut(idx) => {
                        if let Some(ref start_pt) = ds.start_path_point {
                            // Convert pixel offset to normalized coords, accounting for
                            // clip rotation by using rotation-adjusted local deltas.
                            let clip_w = ds.vw * ds.start_scale;
                            let clip_h = ds.vh * ds.start_scale;
                            if clip_w > 1.0 && clip_h > 1.0 {
                                let dnx = local_dx / clip_w;
                                let dny = local_dy / clip_h;
                                let mut pts = mask_path_points_drag.borrow_mut();
                                if idx < pts.len() {
                                    match ds.handle {
                                        Handle::MaskPathAnchor(_) => {
                                            pts[idx].x = (start_pt.x + dnx).clamp(0.0, 1.0);
                                            pts[idx].y = (start_pt.y + dny).clamp(0.0, 1.0);
                                        }
                                        Handle::MaskPathHandleIn(_) => {
                                            pts[idx].handle_in_x = start_pt.handle_in_x + dnx;
                                            pts[idx].handle_in_y = start_pt.handle_in_y + dny;
                                        }
                                        Handle::MaskPathHandleOut(_) => {
                                            pts[idx].handle_out_x = start_pt.handle_out_x + dnx;
                                            pts[idx].handle_out_y = start_pt.handle_out_y + dny;
                                        }
                                        _ => {}
                                    }
                                    let pts_clone = pts.clone();
                                    drop(pts);
                                    on_mask_path_change(&pts_clone);
                                }
                            }
                        }
                    }
                    Handle::SamPromptBox => {
                        // Track the current drag position so the draw
                        // function can render a live preview rectangle.
                        // The actual conversion to a normalized box
                        // and the callback invocation happen in
                        // drag_end.
                        sam_prompt_current_drag
                            .set(Some((ds.start_wx + off_x, ds.start_wy + off_y)));
                    }
                    Handle::None => {}
                }
                drop(ds_borrow);
                da_ref.queue_draw();
            });
        }

        // drag_end: clear state and notify for a final preview refresh.
        {
            let drag_state = drag_state.clone();
            let on_drag_end = Rc::new(on_drag_end);
            let sam_prompt_mode_end = sam_prompt_mode.clone();
            let sam_prompt_start_end = sam_prompt_start.clone();
            let sam_prompt_current_end = sam_prompt_current.clone();
            let sam_prompt_callback_end = sam_prompt_callback.clone();
            let scale_end = scale.clone();
            let position_x_end = position_x.clone();
            let position_y_end = position_y.clone();
            let rotation_end = rotation.clone();
            let adjustment_mode_end = adjustment_mode.clone();
            let da_end = da.clone();
            let drawing_points_end = current_drawing_points.clone();
            let on_drawing_finish_end = on_drawing_finish.clone();
            let drawing_kind_end = drawing_kind.clone();
            let drawing_fill_end = drawing_fill.clone();
            let drawing_items_snapshot_end = drawing_items_snapshot.clone();
            let selected_drawing_item_end = selected_drawing_item.clone();

            gesture.connect_drag_end(move |_g, _ox, _oy| {
                // Capture the completed handle BEFORE clearing
                // drag_state so the SAM branch below can read the
                // cached video rect out of it.
                let completed = drag_state.borrow_mut().take();

                if let Some(ds) = completed.as_ref() {
                    // ── Draw Tool handling ──────────────────────
                    if ds.handle == Handle::Drawing {
                        // Take the collected points out of the cell so
                        // we never hold an immutable borrow at the
                        // same time as the clear() below (RefCell
                        // would panic — see past bug).
                        let collected: Vec<(f64, f64)> =
                            std::mem::take(&mut *drawing_points_end.borrow_mut());
                        // A press-release without measurable motion is
                        // a click, not a stroke — route it through
                        // hit-testing so the user can select an
                        // existing drawing item for per-item Delete.
                        let moved = collected.len() > 1
                            || collected
                                .last()
                                .map(|last| {
                                    (last.0 - ds.start_wx).abs() > 3.0
                                        || (last.1 - ds.start_wy).abs() > 3.0
                                })
                                .unwrap_or(false);
                        if !moved {
                            let click_wx = collected.first().map(|p| p.0).unwrap_or(ds.start_wx);
                            let click_wy = collected.first().map(|p| p.1).unwrap_or(ds.start_wy);
                            let items = drawing_items_snapshot_end.borrow();
                            // Iterate in reverse so the top-most
                            // (last-drawn) hit wins.
                            let hit = items.iter().enumerate().rev().find_map(|(i, it)| {
                                drawing_item_hit(it, click_wx, click_wy, ds.vx, ds.vy, ds.vw, ds.vh)
                                    .then_some(i)
                            });
                            selected_drawing_item_end.set(hit);
                            drop(items);
                            da_end.queue_draw();
                            on_drag_end();
                            return;
                        }
                        if collected.len() >= 2 {
                            use crate::model::clip::DrawingKind;
                            let kind = drawing_kind_end.get();
                            // Shapes only need the first and last point.
                            // Freehand strokes keep the full path.
                            let norm_pts: Vec<(f64, f64)> = match kind {
                                DrawingKind::Stroke => collected
                                    .iter()
                                    .map(|(wx, wy)| ((wx - ds.vx) / ds.vw, (wy - ds.vy) / ds.vh))
                                    .collect(),
                                DrawingKind::Rectangle
                                | DrawingKind::Ellipse
                                | DrawingKind::Arrow => {
                                    let first = collected.first().copied().unwrap_or((0.0, 0.0));
                                    let last = collected.last().copied().unwrap_or(first);
                                    vec![
                                        ((first.0 - ds.vx) / ds.vw, (first.1 - ds.vy) / ds.vh),
                                        ((last.0 - ds.vx) / ds.vw, (last.1 - ds.vy) / ds.vh),
                                    ]
                                }
                            };
                            let fill = match kind {
                                DrawingKind::Rectangle | DrawingKind::Ellipse => {
                                    drawing_fill_end.get()
                                }
                                _ => None,
                            };
                            on_drawing_finish_end(crate::model::clip::DrawingItem {
                                kind,
                                points: norm_pts,
                                color: ds.drawing_color,
                                width: ds.drawing_width,
                                fill_color: fill,
                            });
                        }
                        da_end.queue_draw();
                        on_drag_end();
                        return;
                    }

                    if ds.handle == Handle::SamPromptBox {
                        // Widget-space start and end positions for
                        // the captured prompt. `start` comes from the
                        // cached sam_prompt_start cell, and `end` is
                        // computed from start + gesture offset (the
                        // drag_update path kept sam_prompt_current in
                        // sync, but we also have _ox/_oy from GTK
                        // here — prefer sam_prompt_current since it's
                        // what's on screen).
                        let start = sam_prompt_start_end.get();
                        let end = sam_prompt_current_end.get();
                        if let (Some((sx, sy)), Some((ex, ey))) = (start, end) {
                            // Click-vs-drag threshold. A tiny drag
                            // becomes a point prompt — emulated as a
                            // small tight box around the click point,
                            // since SAM 3's decoder prefers tight
                            // exemplars (see Phase 2a constraint #1).
                            let dx = ex - sx;
                            let dy = ey - sy;
                            let click_threshold_px = 4.0;
                            let (wx1, wy1, wx2, wy2) =
                                if (dx * dx + dy * dy).sqrt() < click_threshold_px {
                                    // Point click → small 8 px
                                    // square centred on (sx, sy). At
                                    // source scale this is the
                                    // equivalent of a point prompt.
                                    (sx - 4.0, sy - 4.0, sx + 4.0, sy + 4.0)
                                } else {
                                    // Normalize corners so x1 < x2
                                    // and y1 < y2.
                                    (sx.min(ex), sy.min(ey), sx.max(ex), sy.max(ey))
                                };

                            // Widget-space → normalized clip-local
                            // coordinates using the cached video
                            // rect + current clip transform.
                            let (clip_cx_w, clip_cy_w, clip_w, clip_h, clip_left_w, clip_top_w) =
                                clip_canvas_geometry(
                                    ds.vx,
                                    ds.vy,
                                    ds.vw,
                                    ds.vh,
                                    scale_end.get(),
                                    position_x_end.get(),
                                    position_y_end.get(),
                                    adjustment_mode_end.get(),
                                );
                            let clip_rot = (-rotation_end.get()).to_radians();
                            let to_norm = |wx: f64, wy: f64| -> (f64, f64) {
                                let (ux, uy) =
                                    unrotate_point_about(wx, wy, clip_cx_w, clip_cy_w, clip_rot);
                                let nx = if clip_w > 0.5 {
                                    (ux - clip_left_w) / clip_w
                                } else {
                                    0.5
                                };
                                let ny = if clip_h > 0.5 {
                                    (uy - clip_top_w) / clip_h
                                } else {
                                    0.5
                                };
                                (nx.clamp(0.0, 1.0), ny.clamp(0.0, 1.0))
                            };
                            // Unrotating the corners individually can
                            // produce a box that's not axis-aligned
                            // in normalized space if the clip is
                            // rotated — compute all four corners,
                            // then take the axis-aligned bounding
                            // box of them.
                            let (n_tl_x, n_tl_y) = to_norm(wx1, wy1);
                            let (n_tr_x, n_tr_y) = to_norm(wx2, wy1);
                            let (n_bl_x, n_bl_y) = to_norm(wx1, wy2);
                            let (n_br_x, n_br_y) = to_norm(wx2, wy2);
                            let nx1 = n_tl_x.min(n_tr_x).min(n_bl_x).min(n_br_x);
                            let ny1 = n_tl_y.min(n_tr_y).min(n_bl_y).min(n_br_y);
                            let nx2 = n_tl_x.max(n_tr_x).max(n_bl_x).max(n_br_x);
                            let ny2 = n_tl_y.max(n_tr_y).max(n_bl_y).max(n_br_y);

                            // Fire the one-shot callback. Take it
                            // out so the same callback can't run
                            // twice. The Inspector's closure handles
                            // the rest (job spawn, button state).
                            let cb = sam_prompt_callback_end.borrow_mut().take();
                            if let Some(cb) = cb {
                                cb(nx1, ny1, nx2, ny2);
                            }
                        }
                        // Always exit prompt mode on drag_end, even
                        // if the callback never fired (e.g. zero-area
                        // box). The Inspector re-enters it on the
                        // next button click.
                        sam_prompt_mode_end.set(false);
                        sam_prompt_start_end.set(None);
                        sam_prompt_current_end.set(None);
                        da_end.set_cursor_from_name(None);
                        da_end.queue_draw();
                        on_drag_end();
                        return;
                    }
                }

                on_drag_end();
            });
        }

        da.add_controller(gesture);

        // Double-click gesture for path point add/delete.
        {
            let mask_enabled_dc = mask_enabled.clone();
            let mask_shape_dc = mask_shape.clone();
            let mask_path_points_dc = mask_path_points.clone();
            let selected_dc = selected.clone();
            let scale_dc = scale.clone();
            let position_x_dc = position_x.clone();
            let position_y_dc = position_y.clone();
            let adjustment_mode_dc = adjustment_mode.clone();
            let proj_w_dc = proj_w.clone();
            let proj_h_dc = proj_h.clone();
            let rotation_dc = rotation.clone();
            let da_dc = da.clone();
            let canvas_widget_dc = canvas_widget.clone();
            let on_mask_path_dbl_click = Rc::new(on_mask_path_dbl_click);

            let dbl_click = gtk::GestureClick::new();
            dbl_click.set_button(1);
            dbl_click.connect_pressed(move |_g, n_press, cx, cy| {
                if n_press != 2 {
                    return;
                }
                if !selected_dc.get() || !mask_enabled_dc.get() || mask_shape_dc.get() != 2 {
                    return;
                }

                let ww = da_dc.width();
                let wh = da_dc.height();
                let (vx, vy, vw, vh) = canvas_video_rect(
                    &da_dc,
                    &canvas_widget_dc,
                    ww,
                    wh,
                    proj_w_dc.get(),
                    proj_h_dc.get(),
                );
                let s = scale_dc.get();
                let px = position_x_dc.get();
                let py = position_y_dc.get();

                let (clip_cx_w, clip_cy_w, clip_w, clip_h, clip_left_w, clip_top_w) =
                    clip_canvas_geometry(vx, vy, vw, vh, s, px, py, adjustment_mode_dc.get());
                let clip_rot = (-rotation_dc.get()).to_radians();

                // Forward transform: normalized → widget (matches draw_mask_outline).
                // draw_mask_outline does: translate(clip_cx, clip_cy) → rotate → translate(-clip_cx, -clip_cy)
                // then map_x = clip_left + nx * clip_w.
                // Net effect: rotate(clip_left + nx*clip_w, clip_top + ny*clip_h) around (clip_cx, clip_cy).
                let map_pt = |nx: f64, ny: f64| -> (f64, f64) {
                    let wx = clip_left_w + nx * clip_w;
                    let wy = clip_top_w + ny * clip_h;
                    rotate_point_about(wx, wy, clip_cx_w, clip_cy_w, clip_rot)
                };

                // Inverse transform: widget → normalized.
                // Unrotate click around clip center, then reverse the linear mapping.
                let (ucx, ucy) = unrotate_point_about(cx, cy, clip_cx_w, clip_cy_w, clip_rot);
                let unmap_x = (ucx - clip_left_w) / clip_w;
                let unmap_y = (ucy - clip_top_w) / clip_h;

                let mut pts = mask_path_points_dc.borrow_mut();
                if pts.len() < 3 {
                    return;
                }

                // Check if double-click is on an existing anchor → delete it.
                let mut delete_idx = None;
                for (i, p) in pts.iter().enumerate() {
                    let (ax, ay) = map_pt(p.x, p.y);
                    if ((cx - ax).powi(2) + (cy - ay).powi(2)).sqrt() <= HANDLE_HIT {
                        delete_idx = Some(i);
                        break;
                    }
                }

                if let Some(idx) = delete_idx {
                    if pts.len() > 3 {
                        pts.remove(idx);
                        let pts_clone = pts.clone();
                        drop(pts);
                        on_mask_path_dbl_click(&pts_clone);
                        da_dc.queue_draw();
                    }
                } else {
                    // Not on an anchor → insert a new point on the nearest path segment.
                    let nx = unmap_x.clamp(0.0, 1.0);
                    let ny = unmap_y.clamp(0.0, 1.0);

                    // Find the segment closest to the click in normalized space.
                    // For each segment (i → i+1), subdivide the bezier curve and
                    // compute the minimum distance from the click to the polyline.
                    let n = pts.len();
                    let mut best_seg = n - 1; // default: insert before closure segment
                    let mut best_dist = f64::MAX;
                    for i in 0..n {
                        let a = &pts[i];
                        let b = &pts[(i + 1) % n];
                        let p0 = (a.x, a.y);
                        let cp1 = (a.x + a.handle_out_x, a.y + a.handle_out_y);
                        let cp2 = (b.x + b.handle_in_x, b.y + b.handle_in_y);
                        let p3 = (b.x, b.y);
                        // Sample a few points along the bezier segment.
                        for step in 0..=10 {
                            let t = step as f64 / 10.0;
                            let mt = 1.0 - t;
                            let sx = mt * mt * mt * p0.0
                                + 3.0 * mt * mt * t * cp1.0
                                + 3.0 * mt * t * t * cp2.0
                                + t * t * t * p3.0;
                            let sy = mt * mt * mt * p0.1
                                + 3.0 * mt * mt * t * cp1.1
                                + 3.0 * mt * t * t * cp2.1
                                + t * t * t * p3.1;
                            let d = (nx - sx).powi(2) + (ny - sy).powi(2);
                            if d < best_dist {
                                best_dist = d;
                                best_seg = i;
                            }
                        }
                    }

                    // Insert after the start anchor of the closest segment.
                    let insert_idx = best_seg + 1;
                    pts.insert(
                        insert_idx,
                        crate::model::clip::BezierPoint {
                            x: nx,
                            y: ny,
                            handle_in_x: 0.0,
                            handle_in_y: 0.0,
                            handle_out_x: 0.0,
                            handle_out_y: 0.0,
                        },
                    );
                    let pts_clone = pts.clone();
                    drop(pts);
                    on_mask_path_dbl_click(&pts_clone);
                    da_dc.queue_draw();
                }
            });
            da.add_controller(dbl_click);
        }

        {
            let scale = scale.clone();
            let position_x = position_x.clone();
            let position_y = position_y.clone();
            let selected = selected.clone();
            let on_change = on_change.clone();
            let da_ref = da.clone();
            let sam_prompt_mode_key = sam_prompt_mode.clone();
            let sam_prompt_start_key = sam_prompt_start.clone();
            let sam_prompt_current_key = sam_prompt_current.clone();
            let sam_prompt_callback_key = sam_prompt_callback.clone();
            let active_tool_key = active_tool.clone();
            let drawing_kind_key = drawing_kind.clone();
            let da_redraw_key = da.clone();
            let on_drawing_delete_at_key = on_drawing_delete_at.clone();
            let selected_item_key = selected_drawing_item.clone();
            let key_ctrl = gtk::EventControllerKey::new();
            key_ctrl.connect_key_pressed(move |_, key, _, mods| {
                use gtk::gdk::{Key, ModifierType};

                // Draw-tool shape kind selection: 1/2/3/4 pick
                // Stroke/Rectangle/Ellipse/Arrow. Delete removes the
                // last-committed drawing item (Undo is still available
                // for finer-grained reverts).
                if active_tool_key.get() == crate::ui::timeline::ActiveTool::Draw {
                    use crate::model::clip::DrawingKind;
                    let new_kind = match key {
                        Key::_1 | Key::KP_1 => Some(DrawingKind::Stroke),
                        Key::_2 | Key::KP_2 => Some(DrawingKind::Rectangle),
                        Key::_3 | Key::KP_3 => Some(DrawingKind::Ellipse),
                        Key::_4 | Key::KP_4 => Some(DrawingKind::Arrow),
                        _ => None,
                    };
                    if let Some(k) = new_kind {
                        drawing_kind_key.set(k);
                        da_redraw_key.queue_draw();
                        return glib::Propagation::Stop;
                    }
                    if matches!(key, Key::Delete | Key::BackSpace | Key::KP_Delete) {
                        // Selected-item delete if the user picked one
                        // via click; otherwise fall through to LIFO
                        // (the `None` arm on the callback).
                        let target = selected_item_key.get();
                        on_drawing_delete_at_key(target);
                        selected_item_key.set(None);
                        da_redraw_key.queue_draw();
                        return glib::Propagation::Stop;
                    }
                }

                // Escape cancels SAM prompt mode regardless of clip
                // selection state — the user may press Escape before
                // selecting anything.
                if key == Key::Escape && sam_prompt_mode_key.get() {
                    sam_prompt_mode_key.set(false);
                    sam_prompt_start_key.set(None);
                    sam_prompt_current_key.set(None);
                    sam_prompt_callback_key.borrow_mut().take();
                    da_ref.set_cursor_from_name(None);
                    da_ref.queue_draw();
                    return glib::Propagation::Stop;
                }

                if !selected.get() {
                    return glib::Propagation::Proceed;
                }
                let shift = mods.contains(ModifierType::SHIFT_MASK);
                let mut handled = false;
                use crate::model::transform_bounds::{
                    POSITION_MAX, POSITION_MIN, SCALE_MAX, SCALE_MIN,
                };
                match key {
                    Key::Left => {
                        position_x.set(
                            (position_x.get() - if shift { 0.1 } else { 0.01 })
                                .clamp(POSITION_MIN, POSITION_MAX),
                        );
                        handled = true;
                    }
                    Key::Right => {
                        position_x.set(
                            (position_x.get() + if shift { 0.1 } else { 0.01 })
                                .clamp(POSITION_MIN, POSITION_MAX),
                        );
                        handled = true;
                    }
                    Key::Up => {
                        position_y.set(
                            (position_y.get() - if shift { 0.1 } else { 0.01 })
                                .clamp(POSITION_MIN, POSITION_MAX),
                        );
                        handled = true;
                    }
                    Key::Down => {
                        position_y.set(
                            (position_y.get() + if shift { 0.1 } else { 0.01 })
                                .clamp(POSITION_MIN, POSITION_MAX),
                        );
                        handled = true;
                    }
                    Key::plus | Key::equal | Key::KP_Add => {
                        scale.set(
                            (scale.get() + if shift { 0.10 } else { 0.05 })
                                .clamp(SCALE_MIN, SCALE_MAX),
                        );
                        handled = true;
                    }
                    Key::minus | Key::underscore | Key::KP_Subtract => {
                        scale.set(
                            (scale.get() - if shift { 0.10 } else { 0.05 })
                                .clamp(SCALE_MIN, SCALE_MAX),
                        );
                        handled = true;
                    }
                    _ => {}
                }
                if handled {
                    on_change(scale.get(), position_x.get(), position_y.get());
                    da_ref.queue_draw();
                    glib::Propagation::Stop
                } else {
                    glib::Propagation::Proceed
                }
            });
            da.add_controller(key_ctrl);
        }

        TransformOverlay {
            drawing_area: da,
            active_tool,
            scale,
            position_x,
            position_y,
            rotation,
            crop_left,
            crop_right,
            crop_top,
            crop_bottom,
            selected,
            proj_w,
            proj_h,
            picture,
            canvas_widget,
            mask_enabled,
            mask_shape,
            mask_cx,
            mask_cy,
            mask_hw,
            mask_hh,
            mask_rotation,
            mask_path_points,
            adjustment_mode,
            content_inset_x,
            content_inset_y,
            tracking_region_enabled,
            tracking_region_editing,
            tracking_center_x,
            tracking_center_y,
            tracking_width,
            tracking_height,
            tracking_rotation,
            sam_prompt_mode,
            sam_prompt_start,
            sam_prompt_current,
            sam_prompt_callback,
            drawing_color,
            drawing_width,
            drawing_kind,
            drawing_fill,
            drawing_items_snapshot,
            selected_drawing_item,
        }
    }

    /// Push the current drawing clip's items into the overlay so
    /// hit-test clicks and the selected-item highlight have something
    /// to reference. The app calls this after each project change /
    /// playhead move; an empty Vec clears the selection as a side
    /// effect.
    pub fn set_current_drawing_items(&self, items: &[crate::model::clip::DrawingItem]) {
        let mut slot = self.drawing_items_snapshot.borrow_mut();
        if *slot != items {
            *slot = items.to_vec();
            // Clear a stale selection when the backing list changes
            // out from under us.
            if let Some(idx) = self.selected_drawing_item.get() {
                if idx >= slot.len() {
                    self.selected_drawing_item.set(None);
                }
            }
            drop(slot);
            self.drawing_area.queue_draw();
        }
    }

    /// Current per-item selection (`None` when nothing is selected).
    pub fn selected_drawing_item(&self) -> Option<usize> {
        self.selected_drawing_item.get()
    }

    /// Set the Draw tool's brush color (0xRRGGBBAA). Applied to
    /// subsequent strokes/shapes.
    pub fn set_drawing_color(&self, color: u32) {
        self.drawing_color.set(color);
    }

    /// Set the Draw tool's brush width in pixels (relative to 1080p).
    pub fn set_drawing_width(&self, width: f64) {
        self.drawing_width.set(width.max(0.5));
    }

    /// Set which shape kind the Draw tool commits on mouse-up.
    pub fn set_drawing_kind(&self, kind: crate::model::clip::DrawingKind) {
        self.drawing_kind.set(kind);
    }

    /// Set the optional fill color for Rectangle/Ellipse shapes.
    /// `None` means stroke-only.
    pub fn set_drawing_fill(&self, color: Option<u32>) {
        self.drawing_fill.set(color);
    }

    /// Update the currently-active tool so the overlay's gesture
    /// router knows whether to enter Draw capture mode. Must be
    /// called whenever `TimelineState.active_tool` changes.
    pub fn set_active_tool(&self, tool: crate::ui::timeline::ActiveTool) {
        self.active_tool.set(tool);
        // Crosshair cursor in Draw mode is the clearest user-visible
        // signal that the tool toggled; default arrow otherwise.
        let cursor_name = if tool == crate::ui::timeline::ActiveTool::Draw {
            Some("crosshair")
        } else {
            None
        };
        self.drawing_area.set_cursor_from_name(cursor_name);
        self.drawing_area.queue_draw();
    }

    /// Give the overlay access to the AspectFrame that constrains the canvas area.
    /// When set, canvas rect computation uses `Widget::compute_bounds()` for pixel-
    /// perfect alignment at all program monitor zoom levels.
    pub fn set_canvas_widget(&self, w: gtk4::Widget) {
        *self.canvas_widget.borrow_mut() = Some(w);
    }

    /// Give the overlay access to the GtkPicture so it can query the actual
    /// paintable dimensions (used by ContentFit::Contain) for pixel-perfect alignment.
    pub fn set_picture(&self, p: gtk4::Picture) {
        *self.picture.borrow_mut() = Some(p);
    }

    /// Update the displayed transform values (e.g. when inspector sliders change).
    pub fn set_transform(&self, s: f64, px: f64, py: f64) {
        self.scale.set(s);
        self.position_x.set(px);
        self.position_y.set(py);
        self.drawing_area.queue_draw();
    }

    /// Update displayed rotation value in degrees.
    pub fn set_rotation(&self, rot: i32) {
        self.rotation.set(rot as f64);
        self.drawing_area.queue_draw();
    }

    /// Update overlay crop values (in source pixels).
    pub fn set_crop(&self, cl: i32, cr: i32, ct: i32, cb: i32) {
        self.crop_left.set(cl.clamp(0, CROP_MAX));
        self.crop_right.set(cr.clamp(0, CROP_MAX));
        self.crop_top.set(ct.clamp(0, CROP_MAX));
        self.crop_bottom.set(cb.clamp(0, CROP_MAX));
        self.drawing_area.queue_draw();
    }

    /// Show or hide handles (true when a clip is selected).
    pub fn set_clip_selected(&self, selected: bool) {
        self.selected.set(selected);
        self.drawing_area.queue_draw();
    }

    /// Set project resolution so the video-rect aspect ratio is correct.
    pub fn set_project_dimensions(&self, w: u32, h: u32) {
        self.proj_w.set(w);
        self.proj_h.set(h);
    }

    /// Set letterbox inset fractions (0.0–0.5 per side).
    /// Used to shrink the clip bounding box to the actual video content area
    /// when the source aspect ratio differs from the project.
    pub fn set_content_inset(&self, inset_x: f64, inset_y: f64) {
        self.content_inset_x.set(inset_x);
        self.content_inset_y.set(inset_y);
    }

    /// Update mask overlay state.
    pub fn set_mask(
        &self,
        enabled: bool,
        shape: u8,
        cx: f64,
        cy: f64,
        hw: f64,
        hh: f64,
        rotation: f64,
        path_points: Option<&[crate::model::clip::BezierPoint]>,
    ) {
        self.mask_enabled.set(enabled);
        self.mask_shape.set(shape);
        self.mask_cx.set(cx);
        self.mask_cy.set(cy);
        self.mask_hw.set(hw);
        self.mask_hh.set(hh);
        self.mask_rotation.set(rotation);
        if let Some(pts) = path_points {
            *self.mask_path_points.borrow_mut() = pts.to_vec();
        } else {
            self.mask_path_points.borrow_mut().clear();
        }
        self.drawing_area.queue_draw();
    }

    pub fn set_adjustment_mode(&self, enabled: bool) {
        self.adjustment_mode.set(enabled);
        self.drawing_area.queue_draw();
    }

    pub fn set_tracking_region(
        &self,
        enabled: bool,
        editing: bool,
        center_x: f64,
        center_y: f64,
        width: f64,
        height: f64,
        rotation_deg: f64,
    ) {
        self.tracking_region_enabled.set(enabled);
        self.tracking_region_editing.set(editing && enabled);
        self.tracking_center_x.set(center_x.clamp(0.0, 1.0));
        self.tracking_center_y.set(center_y.clamp(0.0, 1.0));
        self.tracking_width.set(width.clamp(0.05, 0.5));
        self.tracking_height.set(height.clamp(0.05, 0.5));
        self.tracking_rotation.set(rotation_deg.clamp(
            crate::model::transform_bounds::ROTATE_MIN_DEG,
            crate::model::transform_bounds::ROTATE_MAX_DEG,
        ));
        self.drawing_area.queue_draw();
    }

    pub fn is_tracking_editing(&self) -> bool {
        self.tracking_region_editing.get()
    }

    // ── SAM box-prompt mode (Phase 2b/3) ────────────────────────

    /// Enter SAM box-prompt capture mode. All normal handle
    /// interactions are suspended; the next drag gesture captures a
    /// rectangle that is converted to normalized clip-local
    /// coordinates and passed to `on_captured(x1, y1, x2, y2)`.
    ///
    /// The mode auto-clears after one completed drag or on Escape.
    /// Single clicks (< 4 px drag) produce a small point-emulated
    /// box (see Phase 2a constraint #3 in sam-work.md).
    pub fn enter_sam_prompt_mode(&self, on_captured: impl Fn(f64, f64, f64, f64) + 'static) {
        self.sam_prompt_mode.set(true);
        self.sam_prompt_start.set(None);
        self.sam_prompt_current.set(None);
        *self.sam_prompt_callback.borrow_mut() = Some(Box::new(on_captured));
        self.drawing_area.set_cursor_from_name(Some("crosshair"));
        self.drawing_area.queue_draw();
    }

    /// Cancel SAM prompt mode without firing the callback. Used by
    /// the Inspector to restore the button when the user navigates
    /// away or the clip selection changes mid-prompt.
    pub fn exit_sam_prompt_mode(&self) {
        self.sam_prompt_mode.set(false);
        self.sam_prompt_start.set(None);
        self.sam_prompt_current.set(None);
        self.sam_prompt_callback.borrow_mut().take();
        self.drawing_area.set_cursor_from_name(None);
        self.drawing_area.queue_draw();
    }

    /// Returns `true` if the overlay is currently in SAM prompt
    /// capture mode (waiting for a drag or Escape).
    pub fn is_sam_prompt_active(&self) -> bool {
        self.sam_prompt_mode.get()
    }
}

// ── Helper functions ──────────────────────────────────────────────────────────

/// Query the actual paintable intrinsic dimensions from the GtkPicture.
/// Kept for future use if re-alignment with the paintable is needed.
#[allow(dead_code)]
fn paintable_dims(
    picture: &Rc<RefCell<Option<gtk4::Picture>>>,
    proj_w: u32,
    proj_h: u32,
) -> (u32, u32) {
    if let Some(ref p) = *picture.borrow() {
        if let Some(paintable) = p.paintable() {
            let iw = paintable.intrinsic_width();
            let ih = paintable.intrinsic_height();
            if iw > 0 && ih > 0 {
                return (iw as u32, ih as u32);
            }
        }
    }
    (proj_w, proj_h)
}

/// Compute the canvas video rect in the DrawingArea's coordinate space.
///
/// If a `canvas_widget` is set (the AspectFrame that constrains the canvas),
/// queries its actual bounds in the DA's coordinate space via `compute_bounds()`
/// and then letterboxes the canvas ratio within those bounds.  This gives correct
/// results when the DA is larger than the canvas_widget (e.g. when the outer overlay
/// fills the full scroll viewport and the canvas_frame is smaller due to zoom < 1.0).
///
/// Falls back to `video_rect()` on the full DA if compute_bounds() fails.
/// Distance from `(px, py)` to the line segment `(x0, y0)-(x1, y1)`.
fn point_to_segment_distance(px: f64, py: f64, x0: f64, y0: f64, x1: f64, y1: f64) -> f64 {
    let dx = x1 - x0;
    let dy = y1 - y0;
    let len2 = dx * dx + dy * dy;
    if len2 < 1e-6 {
        let ex = px - x0;
        let ey = py - y0;
        return (ex * ex + ey * ey).sqrt();
    }
    let t = (((px - x0) * dx + (py - y0) * dy) / len2).clamp(0.0, 1.0);
    let fx = x0 + t * dx;
    let fy = y0 + t * dy;
    let ex = px - fx;
    let ey = py - fy;
    (ex * ex + ey * ey).sqrt()
}

/// Hit-test a single drawing item against a widget-space click.
/// Normalized item points are scaled to the video rect
/// `(vx, vy, vw, vh)` so the test matches what the user sees.
/// `tol` is the widget-pixel tolerance for stroke-only hits.
fn drawing_item_hit(
    item: &crate::model::clip::DrawingItem,
    click_wx: f64,
    click_wy: f64,
    vx: f64,
    vy: f64,
    vw: f64,
    vh: f64,
) -> bool {
    use crate::model::clip::DrawingKind;
    let scale_ref = (vh / 1080.0).max(1e-3);
    let tol = (item.width * scale_ref).max(4.0) * 1.8;
    let to_widget = |(nx, ny): (f64, f64)| (vx + nx * vw, vy + ny * vh);
    match item.kind {
        DrawingKind::Stroke => {
            if item.points.len() < 2 {
                return false;
            }
            item.points.windows(2).any(|pair| {
                let a = to_widget(pair[0]);
                let b = to_widget(pair[1]);
                point_to_segment_distance(click_wx, click_wy, a.0, a.1, b.0, b.1) <= tol
            })
        }
        DrawingKind::Rectangle => {
            let p0 = to_widget(item.points[0]);
            let p1 = to_widget(*item.points.last().unwrap());
            let x0 = p0.0.min(p1.0);
            let y0 = p0.1.min(p1.1);
            let x1 = p0.0.max(p1.0);
            let y1 = p0.1.max(p1.1);
            if item.fill_color.is_some() {
                click_wx >= x0 && click_wx <= x1 && click_wy >= y0 && click_wy <= y1
            } else {
                let on_top =
                    (click_wy - y0).abs() <= tol && click_wx >= x0 - tol && click_wx <= x1 + tol;
                let on_bot =
                    (click_wy - y1).abs() <= tol && click_wx >= x0 - tol && click_wx <= x1 + tol;
                let on_left =
                    (click_wx - x0).abs() <= tol && click_wy >= y0 - tol && click_wy <= y1 + tol;
                let on_right =
                    (click_wx - x1).abs() <= tol && click_wy >= y0 - tol && click_wy <= y1 + tol;
                on_top || on_bot || on_left || on_right
            }
        }
        DrawingKind::Ellipse => {
            let p0 = to_widget(item.points[0]);
            let p1 = to_widget(*item.points.last().unwrap());
            let cx = (p0.0 + p1.0) * 0.5;
            let cy = (p0.1 + p1.1) * 0.5;
            let rx = ((p1.0 - p0.0).abs() * 0.5).max(1.0);
            let ry = ((p1.1 - p0.1).abs() * 0.5).max(1.0);
            let dx = (click_wx - cx) / rx;
            let dy = (click_wy - cy) / ry;
            let d = (dx * dx + dy * dy).sqrt();
            let rel_tol = tol / rx.min(ry).max(1.0);
            if item.fill_color.is_some() {
                d <= 1.0 + rel_tol
            } else {
                (d - 1.0).abs() <= rel_tol
            }
        }
        DrawingKind::Arrow => {
            let a = to_widget(item.points[0]);
            let b = to_widget(*item.points.last().unwrap());
            point_to_segment_distance(click_wx, click_wy, a.0, a.1, b.0, b.1) <= tol
        }
    }
}

fn canvas_video_rect(
    da: &DrawingArea,
    canvas_widget: &Rc<RefCell<Option<gtk4::Widget>>>,
    ww: i32,
    wh: i32,
    proj_w: u32,
    proj_h: u32,
) -> (f64, f64, f64, f64) {
    if let Some(ref cw) = *canvas_widget.borrow() {
        if let Some(bounds) = cw.compute_bounds(da) {
            let cfx = bounds.x() as f64;
            let cfy = bounds.y() as f64;
            let cfw = bounds.width() as f64;
            let cfh = bounds.height() as f64;
            if cfw > 0.0 && cfh > 0.0 {
                return video_rect_within(cfx, cfy, cfw, cfh, proj_w, proj_h);
            }
        }
    }
    video_rect(ww, wh, proj_w, proj_h)
}

/// Letterbox `proj_w × proj_h` within the box `(bx, by, bw, bh)`.
/// Returns `(vx, vy, vw, vh)` — the canvas rect in the outer DA coordinate space.
fn video_rect_within(bx: f64, by: f64, bw: f64, bh: f64, pw: u32, ph: u32) -> (f64, f64, f64, f64) {
    if bw <= 0.0 || bh <= 0.0 || pw == 0 || ph == 0 {
        return (bx, by, bw.max(1.0), bh.max(1.0));
    }
    let vid_asp = pw as f64 / ph as f64;
    let box_asp = bw / bh;
    let (vw, vh) = if vid_asp > box_asp {
        (bw, bw / vid_asp)
    } else {
        (bh * vid_asp, bh)
    };
    let vx = bx + (bw - vw) / 2.0;
    let vy = by + (bh - vh) / 2.0;
    (vx, vy, vw, vh)
}

/// Compute the video letterbox rect `(x, y, w, h)` inside a widget of size
/// `(ww × wh)` for a project of resolution `pw × ph` (ContentFit::Contain).
fn video_rect(ww: i32, wh: i32, pw: u32, ph: u32) -> (f64, f64, f64, f64) {
    let ww = ww as f64;
    let wh = wh as f64;
    if ww <= 0.0 || wh <= 0.0 || pw == 0 || ph == 0 {
        return (0.0, 0.0, ww.max(1.0), wh.max(1.0));
    }
    let vid_asp = pw as f64 / ph as f64;
    let wid_asp = ww / wh;
    let (vw, vh) = if vid_asp > wid_asp {
        (ww, ww / vid_asp)
    } else {
        (wh * vid_asp, wh)
    };
    let vx = (ww - vw) / 2.0;
    let vy = (wh - vh) / 2.0;
    (vx, vy, vw, vh)
}

/// Darken the areas outside the canvas rect so it's immediately obvious what
/// is in-frame (will be exported) vs. out-of-frame.
fn draw_outside_vignette(
    cr: &gtk4::cairo::Context,
    ww: f64,
    wh: f64,
    vx: f64,
    vy: f64,
    vw: f64,
    vh: f64,
) {
    cr.save().ok();
    cr.set_source_rgba(0.0, 0.0, 0.0, 0.35);
    // Fill the four rects surrounding the canvas rect
    // Top strip
    if vy > 0.0 {
        cr.rectangle(0.0, 0.0, ww, vy);
        cr.fill().ok();
    }
    // Bottom strip
    if vy + vh < wh {
        cr.rectangle(0.0, vy + vh, ww, wh - (vy + vh));
        cr.fill().ok();
    }
    // Left strip (between top/bottom strips)
    if vx > 0.0 {
        cr.rectangle(0.0, vy, vx, vh);
        cr.fill().ok();
    }
    // Right strip
    if vx + vw < ww {
        cr.rectangle(vx + vw, vy, ww - (vx + vw), vh);
        cr.fill().ok();
    }
    cr.restore().ok();
}

/// Draw the export frame border — a prominent solid rectangle showing exactly
/// what will be included in the exported video.  Drawn with a dark shadow line
/// and a bright accent line so it reads on both light and dark backgrounds.
fn draw_frame_border(cr: &gtk4::cairo::Context, vx: f64, vy: f64, vw: f64, vh: f64) {
    // Shadow (1 px outset, semi-transparent black)
    cr.save().ok();
    cr.set_source_rgba(0.0, 0.0, 0.0, 0.7);
    cr.set_line_width(3.0);
    cr.rectangle(vx - 0.5, vy - 0.5, vw + 1.0, vh + 1.0);
    cr.stroke().ok();
    cr.restore().ok();

    // Bright accent border
    cr.save().ok();
    cr.set_source_rgba(1.0, 0.95, 0.3, 0.95);
    cr.set_line_width(1.5);
    cr.rectangle(vx, vy, vw, vh);
    cr.stroke().ok();
    cr.restore().ok();

    // Corner tick marks (small L-shapes at each corner, 10 px long)
    let tick = 10.0_f64;
    let corners = [
        (vx, vy, tick, 0.0, 0.0, tick),             // top-left
        (vx + vw, vy, -tick, 0.0, 0.0, tick),       // top-right
        (vx, vy + vh, tick, 0.0, 0.0, -tick),       // bottom-left
        (vx + vw, vy + vh, -tick, 0.0, 0.0, -tick), // bottom-right
    ];
    cr.save().ok();
    cr.set_source_rgba(1.0, 1.0, 1.0, 1.0);
    cr.set_line_width(2.5);
    for (cx, cy, hx, _, _, vy_off) in &corners {
        // Horizontal arm
        cr.move_to(*cx, *cy);
        cr.line_to(cx + hx, *cy);
        cr.stroke().ok();
        // Vertical arm
        cr.move_to(*cx, *cy);
        cr.line_to(*cx, cy + vy_off);
        cr.stroke().ok();
    }
    cr.restore().ok();
}

/// Draw the clip bounding box, corner scale handles, center pan dot, and scale label.
fn crop_insets_to_overlay_px(
    crop_left: i32,
    crop_right: i32,
    crop_top: i32,
    crop_bottom: i32,
    proj_w: u32,
    proj_h: u32,
    clip_w: f64,
    clip_h: f64,
) -> (f64, f64, f64, f64) {
    let pw = proj_w.max(1) as f64;
    let ph = proj_h.max(1) as f64;
    let left = (crop_left.max(0) as f64 / pw) * clip_w;
    let right = (crop_right.max(0) as f64 / pw) * clip_w;
    let top = (crop_top.max(0) as f64 / ph) * clip_h;
    let bottom = (crop_bottom.max(0) as f64 / ph) * clip_h;
    (left, right, top, bottom)
}

fn rotate_point_about(x: f64, y: f64, cx: f64, cy: f64, rad: f64) -> (f64, f64) {
    let dx = x - cx;
    let dy = y - cy;
    let xr = dx * rad.cos() - dy * rad.sin();
    let yr = dx * rad.sin() + dy * rad.cos();
    (cx + xr, cy + yr)
}

fn unrotate_point_about(x: f64, y: f64, cx: f64, cy: f64, rad: f64) -> (f64, f64) {
    rotate_point_about(x, y, cx, cy, -rad)
}

fn draw_tracking_region_outline(
    cr: &gtk4::cairo::Context,
    vx: f64,
    vy: f64,
    vw: f64,
    vh: f64,
    scale: f64,
    pos_x: f64,
    pos_y: f64,
    clip_rotation_deg: f64,
    center_x: f64,
    center_y: f64,
    width: f64,
    height: f64,
    rotation_deg: f64,
    editing: bool,
) {
    let clip_cx = vx + vw / 2.0 + pos_x * vw * (1.0 - scale) / 2.0;
    let clip_cy = vy + vh / 2.0 + pos_y * vh * (1.0 - scale) / 2.0;
    let clip_w = vw * scale;
    let clip_h = vh * scale;
    let clip_left = clip_cx - clip_w / 2.0;
    let clip_top = clip_cy - clip_h / 2.0;
    let region_cx = clip_left + center_x * clip_w;
    let region_cy = clip_top + center_y * clip_h;
    let region_hw = width * clip_w;
    let region_hh = height * clip_h;
    let clip_rot = (-clip_rotation_deg).to_radians();
    let region_rot = rotation_deg.to_radians();

    let map_pt = |lx: f64, ly: f64| -> (f64, f64) {
        let (rx, ry) = rotate_point_about(
            region_cx + lx,
            region_cy + ly,
            region_cx,
            region_cy,
            region_rot,
        );
        rotate_point_about(rx, ry, clip_cx, clip_cy, clip_rot)
    };
    let corners = [
        map_pt(-region_hw, -region_hh),
        map_pt(region_hw, -region_hh),
        map_pt(region_hw, region_hh),
        map_pt(-region_hw, region_hh),
    ];

    cr.save().ok();
    cr.set_source_rgba(0.25, 1.0, 0.5, 0.95);
    cr.set_line_width(2.0);
    cr.move_to(corners[0].0, corners[0].1);
    for point in corners.iter().skip(1) {
        cr.line_to(point.0, point.1);
    }
    cr.close_path();
    cr.stroke().ok();
    if editing {
        for (x, y) in &corners {
            cr.arc(*x, *y, HANDLE_R, 0.0, std::f64::consts::TAU);
            cr.fill().ok();
        }
    }
    cr.restore().ok();
}

fn clip_canvas_geometry(
    vx: f64,
    vy: f64,
    vw: f64,
    vh: f64,
    scale: f64,
    pos_x: f64,
    pos_y: f64,
    adjustment_mode: bool,
) -> (f64, f64, f64, f64, f64, f64) {
    let scale = scale.max(1e-6);
    let (cx, cy, clip_w, clip_h) = if adjustment_mode {
        let (cx, cy, clip_w, clip_h) =
            crate::media::adjustment_scope::adjustment_canvas_geometry(vw, vh, scale, pos_x, pos_y);
        (vx + cx, vy + cy, clip_w, clip_h)
    } else {
        (
            vx + vw / 2.0 + pos_x * vw * (1.0 - scale) / 2.0,
            vy + vh / 2.0 + pos_y * vh * (1.0 - scale) / 2.0,
            vw * scale,
            vh * scale,
        )
    };
    let left = cx - clip_w / 2.0;
    let top = cy - clip_h / 2.0;
    (cx, cy, clip_w, clip_h, left, top)
}

fn clip_position_range(axis_size: f64, scale: f64, adjustment_mode: bool) -> f64 {
    if adjustment_mode {
        axis_size / 2.0
    } else {
        axis_size * (1.0 - scale) / 2.0
    }
}

fn draw_overlay(
    cr: &gtk4::cairo::Context,
    vx_full: f64,
    vy_full: f64,
    vw_full: f64,
    vh_full: f64,
    scale: f64,
    pos_x: f64,
    pos_y: f64,
    adjustment_mode: bool,
    rotation_deg: f64,
    crop_left: i32,
    crop_right: i32,
    crop_top: i32,
    crop_bottom: i32,
    proj_w: u32,
    proj_h: u32,
    content_inset_x: f64,
    content_inset_y: f64,
) {
    // Shrink the video rect to the content area (excluding letterbox).
    let vx = vx_full + vw_full * content_inset_x;
    let vy = vy_full + vh_full * content_inset_y;
    let vw = vw_full * (1.0 - 2.0 * content_inset_x);
    let vh = vh_full * (1.0 - 2.0 * content_inset_y);

    // Clip centre and half-extents in widget coords.
    // GStreamer's videobox pads/crops (1-scale)*pw*(1+pos_x)/2 on the left, so the
    // clip centre = canvas_centre + pos_x * canvas_half * (1-scale).
    // This formula is valid for both zoom-in (scale>1) and zoom-out (scale<1).
    let (cx, cy, clip_w, clip_h, left, top) =
        clip_canvas_geometry(vx, vy, vw, vh, scale, pos_x, pos_y, adjustment_mode);
    let hw = clip_w / 2.0;
    let hh = clip_h / 2.0;

    let right = left + clip_w;
    let bottom = top + clip_h;
    let rot_rad = (-rotation_deg).to_radians();
    let (crop_l_px, crop_r_px, crop_t_px, crop_b_px) = crop_insets_to_overlay_px(
        crop_left,
        crop_right,
        crop_top,
        crop_bottom,
        proj_w,
        proj_h,
        right - left,
        bottom - top,
    );
    let crop_left_x = left + crop_l_px;
    let crop_right_x = right - crop_r_px;
    let crop_top_y = top + crop_t_px;
    let crop_bottom_y = bottom - crop_b_px;
    let to_world = |x: f64, y: f64| -> (f64, f64) { rotate_point_about(x, y, cx, cy, rot_rad) };

    // Clip bounding box (white dashed)
    cr.save().ok();
    cr.set_source_rgba(1.0, 1.0, 1.0, 0.80);
    cr.set_line_width(1.5);
    cr.set_dash(&[6.0, 4.0], 0.0);
    let (tlx, tly) = to_world(left, top);
    let (trx, try_) = to_world(right, top);
    let (brx, bry) = to_world(right, bottom);
    let (blx, bly) = to_world(left, bottom);
    cr.move_to(tlx, tly);
    cr.line_to(trx, try_);
    cr.line_to(brx, bry);
    cr.line_to(blx, bly);
    cr.close_path();
    cr.stroke().ok();
    cr.restore().ok();

    // Corner scale handles
    for (hx, hy) in &[(tlx, tly), (trx, try_), (blx, bly), (brx, bry)] {
        cr.save().ok();
        cr.arc(*hx, *hy, HANDLE_R, 0.0, std::f64::consts::TAU);
        cr.set_source_rgba(1.0, 1.0, 1.0, 0.95);
        cr.fill_preserve().ok();
        cr.set_source_rgba(0.25, 0.55, 1.0, 1.0);
        cr.set_line_width(1.5);
        cr.stroke().ok();
        cr.restore().ok();
    }

    // Rotation handle (top-center) connected to clip box
    let (rot_hx, rot_hy) = to_world(cx, top - 24.0);
    let (rot_ax, rot_ay) = to_world(cx, top);
    cr.save().ok();
    cr.move_to(rot_ax, rot_ay);
    cr.line_to(rot_hx, rot_hy);
    cr.set_source_rgba(1.0, 0.65, 0.2, 0.95);
    cr.set_line_width(1.5);
    cr.stroke().ok();
    cr.arc(rot_hx, rot_hy, HANDLE_R, 0.0, std::f64::consts::TAU);
    cr.set_source_rgba(1.0, 0.75, 0.2, 0.98);
    cr.fill_preserve().ok();
    cr.set_source_rgba(0.55, 0.25, 0.0, 1.0);
    cr.set_line_width(1.2);
    cr.stroke().ok();
    cr.restore().ok();

    // Crop rectangle and edge midpoint handles
    cr.save().ok();
    cr.set_source_rgba(0.3, 0.95, 0.45, 0.95);
    cr.set_line_width(1.5);
    cr.set_dash(&[5.0, 3.0], 0.0);
    let (ctlx, ctly) = to_world(crop_left_x, crop_top_y);
    let (ctrx, ctry) = to_world(crop_right_x, crop_top_y);
    let (cbrx, cbry) = to_world(crop_right_x, crop_bottom_y);
    let (cblx, cbly) = to_world(crop_left_x, crop_bottom_y);
    cr.move_to(ctlx, ctly);
    cr.line_to(ctrx, ctry);
    cr.line_to(cbrx, cbry);
    cr.line_to(cblx, cbly);
    cr.close_path();
    cr.stroke().ok();
    cr.restore().ok();
    for (hx, hy) in &[
        to_world((crop_left_x + crop_right_x) / 2.0, crop_top_y),
        to_world((crop_left_x + crop_right_x) / 2.0, crop_bottom_y),
        to_world(crop_left_x, (crop_top_y + crop_bottom_y) / 2.0),
        to_world(crop_right_x, (crop_top_y + crop_bottom_y) / 2.0),
    ] {
        cr.save().ok();
        cr.rectangle(*hx - 6.0, *hy - 6.0, 12.0, 12.0);
        cr.set_source_rgba(0.3, 0.95, 0.45, 0.95);
        cr.fill_preserve().ok();
        cr.set_source_rgba(0.0, 0.35, 0.1, 1.0);
        cr.set_line_width(1.2);
        cr.stroke().ok();
        cr.restore().ok();
    }

    // Centre pan dot
    cr.save().ok();
    cr.arc(cx, cy, 4.5, 0.0, std::f64::consts::TAU);
    cr.set_source_rgba(1.0, 1.0, 1.0, 0.90);
    cr.fill_preserve().ok();
    cr.set_source_rgba(0.25, 0.55, 1.0, 1.0);
    cr.set_line_width(1.0);
    cr.stroke().ok();
    cr.restore().ok();

    // Scale label near top-right of the video frame
    cr.save().ok();
    cr.select_font_face(
        "Sans",
        gtk4::cairo::FontSlant::Normal,
        gtk4::cairo::FontWeight::Bold,
    );
    cr.set_font_size(11.0);
    let label = format!("{scale:.2}×  {rotation_deg:.0}°");
    let te = match cr.text_extents(&label) {
        Ok(te) => te,
        Err(_) => return,
    };
    let tx = vx + vw - te.width() - 12.0;
    let ty = vy + 8.0;
    // Dark pill background
    cr.set_source_rgba(0.0, 0.0, 0.0, 0.60);
    cr.rectangle(tx - 4.0, ty - 1.0, te.width() + 8.0, te.height() + 4.0);
    cr.fill().ok();
    cr.set_source_rgba(1.0, 0.95, 0.3, 1.0);
    cr.move_to(tx, ty + te.height() + 1.0);
    cr.show_text(&label).ok();
    cr.restore().ok();
}

/// Draw a dashed cyan outline for the mask shape.
///
/// The mask is defined in clip-local normalized coordinates (0..1).
/// The mask probe runs before zoom/position in the GStreamer pipeline,
/// so we must map through the clip's transform to get canvas-space
/// coordinates for the overlay.
fn draw_mask_outline(
    cr: &gtk4::cairo::Context,
    vx: f64,
    vy: f64,
    vw: f64,
    vh: f64,
    clip_scale: f64,
    clip_px: f64,
    clip_py: f64,
    adjustment_mode: bool,
    clip_rotation_deg: f64,
    shape: u8,
    mask_cx: f64,
    mask_cy: f64,
    mask_hw: f64,
    mask_hh: f64,
    mask_rotation_deg: f64,
    path_points: &[crate::model::clip::BezierPoint],
) {
    // The clip occupies a region within the canvas defined by its scale/position.
    // Clip centre in widget coords (same formula as draw_overlay):
    let (clip_cx, clip_cy, clip_w, clip_h, clip_left, clip_top) = clip_canvas_geometry(
        vx,
        vy,
        vw,
        vh,
        clip_scale,
        clip_px,
        clip_py,
        adjustment_mode,
    );

    // Map mask normalized coords (0..1 within the clip frame) to widget coords.
    let cx = clip_left + mask_cx * clip_w;
    let cy = clip_top + mask_cy * clip_h;
    let hw = mask_hw * clip_w;
    let hh = mask_hh * clip_h;

    let clip_rot_rad = (-clip_rotation_deg).to_radians();
    let mask_rot_rad = mask_rotation_deg.to_radians();

    cr.save().ok();
    cr.set_source_rgba(0.0, 0.85, 0.95, 0.9); // cyan
    cr.set_line_width(2.0);
    cr.set_dash(&[8.0, 4.0], 0.0);

    // First apply clip rotation around clip centre, then mask rotation around mask centre.
    // We combine by translating to clip centre, rotating by clip rotation,
    // then translating to mask centre offset, then rotating by mask rotation.
    cr.translate(clip_cx, clip_cy);
    cr.rotate(clip_rot_rad);

    if shape == 2 && path_points.len() >= 3 {
        // Bezier path: draw in clip-local space (no mask rotation/centre offset needed).
        // Undo the centre offset — path points are in clip-local 0..1 coords.
        cr.translate(-clip_cx, -clip_cy);

        let map_x = |nx: f64| -> f64 { clip_left + nx * clip_w };
        let map_y = |ny: f64| -> f64 { clip_top + ny * clip_h };

        let p0 = &path_points[0];
        cr.move_to(map_x(p0.x), map_y(p0.y));

        for i in 0..path_points.len() {
            let a = &path_points[i];
            let b = &path_points[(i + 1) % path_points.len()];
            cr.curve_to(
                map_x(a.x + a.handle_out_x),
                map_y(a.y + a.handle_out_y),
                map_x(b.x + b.handle_in_x),
                map_y(b.y + b.handle_in_y),
                map_x(b.x),
                map_y(b.y),
            );
        }
        cr.close_path();
        cr.stroke().ok();

        // Draw anchor points as small cyan filled squares
        for p in path_points {
            let px = map_x(p.x);
            let py = map_y(p.y);
            cr.rectangle(px - 4.0, py - 4.0, 8.0, 8.0);
            cr.fill().ok();

            // Draw handle lines if non-zero
            if p.handle_in_x.abs() > 1e-6 || p.handle_in_y.abs() > 1e-6 {
                cr.set_dash(&[], 0.0);
                cr.move_to(px, py);
                cr.line_to(map_x(p.x + p.handle_in_x), map_y(p.y + p.handle_in_y));
                cr.stroke().ok();
                cr.arc(
                    map_x(p.x + p.handle_in_x),
                    map_y(p.y + p.handle_in_y),
                    3.0,
                    0.0,
                    std::f64::consts::TAU,
                );
                cr.fill().ok();
                cr.set_dash(&[8.0, 4.0], 0.0);
            }
            if p.handle_out_x.abs() > 1e-6 || p.handle_out_y.abs() > 1e-6 {
                cr.set_dash(&[], 0.0);
                cr.move_to(px, py);
                cr.line_to(map_x(p.x + p.handle_out_x), map_y(p.y + p.handle_out_y));
                cr.stroke().ok();
                cr.arc(
                    map_x(p.x + p.handle_out_x),
                    map_y(p.y + p.handle_out_y),
                    3.0,
                    0.0,
                    std::f64::consts::TAU,
                );
                cr.fill().ok();
                cr.set_dash(&[8.0, 4.0], 0.0);
            }
        }
    } else {
        cr.translate(cx - clip_cx, cy - clip_cy);
        cr.rotate(mask_rot_rad);

        if shape == 1 {
            // Ellipse
            cr.save().ok();
            cr.scale(hw.max(0.1), hh.max(0.1));
            cr.arc(0.0, 0.0, 1.0, 0.0, std::f64::consts::TAU);
            cr.restore().ok();
            cr.stroke().ok();
        } else {
            // Rectangle
            cr.rectangle(-hw, -hh, hw * 2.0, hh * 2.0);
            cr.stroke().ok();
        }

        // Draw center crosshair.
        cr.set_dash(&[], 0.0);
        cr.set_line_width(1.0);
        let ch = 8.0;
        cr.move_to(-ch, 0.0);
        cr.line_to(ch, 0.0);
        cr.move_to(0.0, -ch);
        cr.line_to(0.0, ch);
        cr.stroke().ok();
    }

    cr.restore().ok();
}
