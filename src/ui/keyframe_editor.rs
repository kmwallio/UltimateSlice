use crate::model::clip::{Clip, KeyframeInterpolation, NumericKeyframe, Phase1KeyframeProperty};
use crate::model::project::Project;
use crate::ui::timeline::TimelineState;
use crate::undo::SetTrackClipsCommand;
use gtk4::prelude::*;
use gtk4::{self as gtk, Box as GBox, Button, DrawingArea, Label, Orientation};
use std::cell::RefCell;
use std::collections::HashSet;
use std::rc::Rc;

const HEADER_H: f64 = 24.0;
const ROW_H: f64 = 26.0;
const LABEL_W: f64 = 110.0;
const H_PADDING: f64 = 8.0;
const KEYFRAME_HIT_RADIUS: f64 = 7.0;

#[derive(Clone, Copy)]
struct LaneDef {
    property: Phase1KeyframeProperty,
    label: &'static str,
    color: (f64, f64, f64),
}

const LANE_DEFS: [LaneDef; 12] = [
    LaneDef {
        property: Phase1KeyframeProperty::Scale,
        label: "Scale",
        color: (1.0, 0.83, 0.30),
    },
    LaneDef {
        property: Phase1KeyframeProperty::Opacity,
        label: "Opacity",
        color: (0.94, 0.94, 0.94),
    },
    LaneDef {
        property: Phase1KeyframeProperty::PositionX,
        label: "Position X",
        color: (0.45, 0.90, 1.0),
    },
    LaneDef {
        property: Phase1KeyframeProperty::PositionY,
        label: "Position Y",
        color: (0.82, 0.62, 1.0),
    },
    LaneDef {
        property: Phase1KeyframeProperty::Volume,
        label: "Volume",
        color: (0.55, 1.0, 0.62),
    },
    LaneDef {
        property: Phase1KeyframeProperty::Pan,
        label: "Pan",
        color: (1.0, 0.66, 0.36),
    },
    LaneDef {
        property: Phase1KeyframeProperty::Speed,
        label: "Speed",
        color: (0.98, 0.82, 0.35),
    },
    LaneDef {
        property: Phase1KeyframeProperty::Rotate,
        label: "Rotate",
        color: (1.0, 0.55, 0.85),
    },
    LaneDef {
        property: Phase1KeyframeProperty::CropLeft,
        label: "Crop Left",
        color: (1.0, 0.76, 0.36),
    },
    LaneDef {
        property: Phase1KeyframeProperty::CropRight,
        label: "Crop Right",
        color: (1.0, 0.72, 0.33),
    },
    LaneDef {
        property: Phase1KeyframeProperty::CropTop,
        label: "Crop Top",
        color: (1.0, 0.69, 0.30),
    },
    LaneDef {
        property: Phase1KeyframeProperty::CropBottom,
        label: "Crop Bottom",
        color: (1.0, 0.65, 0.28),
    },
];

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
struct KeyframePoint {
    property: Phase1KeyframeProperty,
    local_time_ns: u64,
}

#[derive(Clone)]
struct DragState {
    track_id: String,
    clip_id: String,
    property: Phase1KeyframeProperty,
    source_local_ns: u64,
    current_local_ns: u64,
    start_x: f64,
    original_track_clips: Vec<Clip>,
}

#[derive(Clone)]
struct EditorState {
    visible_properties: HashSet<Phase1KeyframeProperty>,
    selected_points: HashSet<KeyframePoint>,
    primary_point: Option<KeyframePoint>,
    drag: Option<DragState>,
    zoom_x: f64,
    scroll_px: f64,
}

impl Default for EditorState {
    fn default() -> Self {
        Self {
            visible_properties: LANE_DEFS.iter().map(|lane| lane.property).collect(),
            selected_points: HashSet::new(),
            primary_point: None,
            drag: None,
            zoom_x: 1.0,
            scroll_px: 0.0,
        }
    }
}

#[derive(Clone)]
pub struct KeyframeEditorView {
    area: DrawingArea,
    state: Rc<RefCell<EditorState>>,
}

impl KeyframeEditorView {
    pub fn queue_redraw(&self) {
        self.area.queue_draw();
    }

    pub fn clear_selection(&self) {
        let mut state = self.state.borrow_mut();
        state.selected_points.clear();
        state.primary_point = None;
        state.drag = None;
    }
}

fn visible_lanes(state: &EditorState) -> Vec<LaneDef> {
    LANE_DEFS
        .iter()
        .copied()
        .filter(|lane| state.visible_properties.contains(&lane.property))
        .collect()
}

fn timeline_x_bounds(width: f64) -> (f64, f64) {
    let left = LABEL_W + H_PADDING;
    let right = (width - H_PADDING).max(left + 1.0);
    (left, right)
}

fn max_scroll_px(width: f64, zoom_x: f64) -> f64 {
    let (left, right) = timeline_x_bounds(width);
    let viewport = (right - left).max(1.0);
    (viewport * zoom_x.max(1.0) - viewport).max(0.0)
}

fn clamp_scroll_px(scroll_px: f64, width: f64, zoom_x: f64) -> f64 {
    scroll_px.clamp(0.0, max_scroll_px(width, zoom_x))
}

fn local_ns_to_x(local_time_ns: u64, duration_ns: u64, width: f64, zoom_x: f64, scroll_px: f64) -> f64 {
    let (left, right) = timeline_x_bounds(width);
    if duration_ns == 0 {
        return left;
    }
    let viewport = (right - left).max(1.0);
    let content = viewport * zoom_x.max(1.0);
    let frac = local_time_ns as f64 / duration_ns as f64;
    left + frac * content - clamp_scroll_px(scroll_px, width, zoom_x)
}

fn x_to_local_ns(x: f64, duration_ns: u64, width: f64, zoom_x: f64, scroll_px: f64) -> u64 {
    let (left, right) = timeline_x_bounds(width);
    let viewport = (right - left).max(1.0);
    if duration_ns == 0 {
        return 0;
    }
    let content = viewport * zoom_x.max(1.0);
    let scroll = clamp_scroll_px(scroll_px, width, zoom_x);
    let frac = ((x - left + scroll) / content).clamp(0.0, 1.0);
    (frac * duration_ns as f64).round() as u64
}

fn lane_index_at_y(y: f64, visible_lane_count: usize) -> Option<usize> {
    if y < HEADER_H {
        return None;
    }
    let row = ((y - HEADER_H) / ROW_H).floor() as isize;
    if row < 0 || row as usize >= visible_lane_count {
        None
    } else {
        Some(row as usize)
    }
}

fn interp_to_dropdown_idx(interp: KeyframeInterpolation) -> u32 {
    match interp {
        KeyframeInterpolation::Linear => 0,
        KeyframeInterpolation::EaseIn => 1,
        KeyframeInterpolation::EaseOut => 2,
        KeyframeInterpolation::EaseInOut => 3,
    }
}

fn dropdown_idx_to_interp(idx: u32) -> KeyframeInterpolation {
    match idx {
        1 => KeyframeInterpolation::EaseIn,
        2 => KeyframeInterpolation::EaseOut,
        3 => KeyframeInterpolation::EaseInOut,
        _ => KeyframeInterpolation::Linear,
    }
}

fn move_keyframe_in_property(
    clip: &mut Clip,
    property: Phase1KeyframeProperty,
    from_local_ns: u64,
    to_local_ns: u64,
) -> bool {
    if from_local_ns == to_local_ns {
        return false;
    }
    let keyframes = clip.keyframes_for_phase1_property_mut(property);
    let Some(idx) = keyframes.iter().position(|kf| kf.time_ns == from_local_ns) else {
        return false;
    };
    let moved = keyframes.remove(idx);
    if let Some(existing) = keyframes.iter_mut().find(|kf| kf.time_ns == to_local_ns) {
        existing.value = moved.value;
        existing.interpolation = moved.interpolation;
    } else {
        keyframes.push(NumericKeyframe {
            time_ns: to_local_ns,
            value: moved.value,
            interpolation: moved.interpolation,
        });
        keyframes.sort_by_key(|kf| kf.time_ns);
    }
    true
}

fn selected_clip_location(project: &Project, clip_id: &str) -> Option<(usize, usize, String)> {
    for (track_idx, track) in project.tracks.iter().enumerate() {
        if let Some(clip_idx) = track.clips.iter().position(|c| c.id == clip_id) {
            return Some((track_idx, clip_idx, track.id.clone()));
        }
    }
    None
}

fn update_area_height(area: &DrawingArea, state: &Rc<RefCell<EditorState>>) {
    let lane_count = visible_lanes(&state.borrow()).len().max(1);
    let height = HEADER_H + ROW_H * lane_count as f64 + H_PADDING;
    area.set_content_height(height.ceil() as i32);
}

fn phase1_property_value_bounds(property: Phase1KeyframeProperty) -> (f64, f64) {
    match property {
        Phase1KeyframeProperty::PositionX | Phase1KeyframeProperty::PositionY => (-1.0, 1.0),
        Phase1KeyframeProperty::Scale => (0.1, 4.0),
        Phase1KeyframeProperty::Opacity => (0.0, 1.0),
        Phase1KeyframeProperty::Brightness => (-1.0, 1.0),
        Phase1KeyframeProperty::Contrast => (0.0, 2.0),
        Phase1KeyframeProperty::Saturation => (0.0, 2.0),
        Phase1KeyframeProperty::Temperature => (2000.0, 10000.0),
        Phase1KeyframeProperty::Tint => (-1.0, 1.0),
        Phase1KeyframeProperty::Volume => (0.0, 4.0),
        Phase1KeyframeProperty::Pan => (-1.0, 1.0),
        Phase1KeyframeProperty::Speed => (0.05, 16.0),
        Phase1KeyframeProperty::Rotate => (-180.0, 180.0),
        Phase1KeyframeProperty::CropLeft
        | Phase1KeyframeProperty::CropRight
        | Phase1KeyframeProperty::CropTop
        | Phase1KeyframeProperty::CropBottom => (0.0, 500.0),
    }
}

fn lane_y_for_value(row_y: f64, property: Phase1KeyframeProperty, value: f64) -> f64 {
    let (min_v, max_v) = phase1_property_value_bounds(property);
    let denom = (max_v - min_v).max(1e-9);
    let t = ((value - min_v) / denom).clamp(0.0, 1.0);
    let y_top = row_y + 4.0;
    let y_bottom = row_y + ROW_H - 4.0;
    y_bottom - t * (y_bottom - y_top)
}

fn selected_clip_playhead_fraction(project: &Project, timeline_state: &TimelineState) -> f64 {
    let Some(clip_id) = timeline_state.selected_clip_id.as_ref() else {
        return 0.5;
    };
    let Some(clip) = project
        .tracks
        .iter()
        .flat_map(|track| track.clips.iter())
        .find(|clip| &clip.id == clip_id)
    else {
        return 0.5;
    };
    let duration_ns = clip.duration();
    if duration_ns == 0 {
        return 0.5;
    }
    (clip.local_timeline_position_ns(timeline_state.playhead_ns) as f64 / duration_ns as f64).clamp(0.0, 1.0)
}

pub fn build_keyframe_editor(
    project: Rc<RefCell<Project>>,
    timeline_state: Rc<RefCell<TimelineState>>,
    on_project_changed: Rc<dyn Fn()>,
    on_seek: Rc<dyn Fn(u64)>,
) -> (GBox, Rc<KeyframeEditorView>) {
    let root = GBox::new(Orientation::Vertical, 6);
    root.set_margin_top(4);
    root.set_margin_bottom(4);

    let title = Label::new(Some("Keyframes"));
    title.add_css_class("browser-header");
    title.set_halign(gtk::Align::Start);
    root.append(&title);

    let controls = GBox::new(Orientation::Horizontal, 6);
    let prev_btn = Button::with_label("◀ Prev");
    prev_btn.add_css_class("small-btn");
    prev_btn.set_tooltip_text(Some("Jump to previous keyframe (Alt+Left)"));
    let next_btn = Button::with_label("Next ▶");
    next_btn.add_css_class("small-btn");
    next_btn.set_tooltip_text(Some("Jump to next keyframe (Alt+Right)"));
    let add_btn = Button::with_label("Add @ Playhead");
    add_btn.add_css_class("small-btn");
    let remove_btn = Button::with_label("Remove");
    remove_btn.add_css_class("small-btn");
    let apply_interp_btn = Button::with_label("Apply Interp");
    apply_interp_btn.add_css_class("small-btn");
    let zoom_out_btn = Button::with_label("−");
    zoom_out_btn.add_css_class("small-btn");
    let zoom_reset_btn = Button::with_label("100%");
    zoom_reset_btn.add_css_class("small-btn");
    zoom_reset_btn.set_tooltip_text(Some("Reset dopesheet zoom"));
    let zoom_in_btn = Button::with_label("+");
    zoom_in_btn.add_css_class("small-btn");
    let interp_dropdown =
        gtk::DropDown::from_strings(&["Linear", "Ease In", "Ease Out", "Ease In/Out"]);
    interp_dropdown.set_selected(0);
    interp_dropdown.set_tooltip_text(Some("Interpolation for added/selected keyframes"));
    interp_dropdown.set_hexpand(true);
    controls.append(&prev_btn);
    controls.append(&next_btn);
    controls.append(&add_btn);
    controls.append(&remove_btn);
    controls.append(&interp_dropdown);
    controls.append(&apply_interp_btn);
    controls.append(&zoom_out_btn);
    controls.append(&zoom_reset_btn);
    controls.append(&zoom_in_btn);
    root.append(&controls);

    let lanes_box = gtk::FlowBox::new();
    lanes_box.set_selection_mode(gtk::SelectionMode::None);
    lanes_box.set_max_children_per_line(4);
    lanes_box.set_row_spacing(4);
    lanes_box.set_column_spacing(6);
    root.append(&lanes_box);

    let area = DrawingArea::new();
    area.set_hexpand(true);
    area.set_vexpand(true);
    area.set_content_width(480);
    area.set_focusable(true);
    root.append(&area);

    let state = Rc::new(RefCell::new(EditorState::default()));
    let view = Rc::new(KeyframeEditorView {
        area: area.clone(),
        state: state.clone(),
    });

    let apply_zoom: Rc<dyn Fn(f64)> = Rc::new({
        let project = project.clone();
        let timeline_state = timeline_state.clone();
        let state = state.clone();
        let area = area.clone();
        let zoom_reset_btn = zoom_reset_btn.clone();
        move |new_zoom: f64| {
            let width = area.width().max(1) as f64;
            let mut st = state.borrow_mut();
            let old_zoom = st.zoom_x;
            let target_zoom = new_zoom.clamp(1.0, 16.0);
            if (target_zoom - old_zoom).abs() < f64::EPSILON {
                return;
            }
            let old_scroll = st.scroll_px;
            let anchor_frac = {
                let proj = project.borrow();
                let ts = timeline_state.borrow();
                selected_clip_playhead_fraction(&proj, &ts)
            };
            let (x0, x1) = timeline_x_bounds(width);
            let viewport = (x1 - x0).max(1.0);
            let old_content = viewport * old_zoom;
            let new_content = viewport * target_zoom;
            let anchor_screen_offset = anchor_frac * old_content - old_scroll;

            st.zoom_x = target_zoom;
            st.scroll_px = clamp_scroll_px(
                anchor_frac * new_content - anchor_screen_offset,
                width,
                target_zoom,
            );
            zoom_reset_btn.set_label(&format!("{:.0}%", target_zoom * 100.0));
            drop(st);
            area.queue_draw();
        }
    });

    zoom_out_btn.connect_clicked({
        let state = state.clone();
        let apply_zoom = apply_zoom.clone();
        move |_| {
            let z = state.borrow().zoom_x;
            apply_zoom(z / 1.25);
        }
    });
    zoom_in_btn.connect_clicked({
        let state = state.clone();
        let apply_zoom = apply_zoom.clone();
        move |_| {
            let z = state.borrow().zoom_x;
            apply_zoom(z * 1.25);
        }
    });
    zoom_reset_btn.connect_clicked({
        let apply_zoom = apply_zoom.clone();
        move |_| apply_zoom(1.0)
    });

    // ── Prev / Next keyframe navigation ──────────────────────────────────
    prev_btn.connect_clicked({
        let project = project.clone();
        let timeline_state = timeline_state.clone();
        let on_seek = on_seek.clone();
        let area = area.clone();
        move |_| {
            let (clip_id, playhead) = {
                let st = timeline_state.borrow();
                (st.selected_clip_id.clone(), st.playhead_ns)
            };
            let Some(clip_id) = clip_id else { return };
            let proj = project.borrow();
            if let Some(ns) = proj
                .tracks
                .iter()
                .flat_map(|t| t.clips.iter())
                .find(|c| c.id == clip_id)
                .and_then(|clip| {
                    let local = clip.local_timeline_position_ns(playhead);
                    clip.prev_keyframe_local_ns(local)
                        .map(|lt| clip.timeline_start.saturating_add(lt))
                })
            {
                drop(proj);
                on_seek(ns);
                area.queue_draw();
            }
        }
    });
    next_btn.connect_clicked({
        let project = project.clone();
        let timeline_state = timeline_state.clone();
        let on_seek = on_seek.clone();
        let area = area.clone();
        move |_| {
            let (clip_id, playhead) = {
                let st = timeline_state.borrow();
                (st.selected_clip_id.clone(), st.playhead_ns)
            };
            let Some(clip_id) = clip_id else { return };
            let proj = project.borrow();
            if let Some(ns) = proj
                .tracks
                .iter()
                .flat_map(|t| t.clips.iter())
                .find(|c| c.id == clip_id)
                .and_then(|clip| {
                    let local = clip.local_timeline_position_ns(playhead);
                    clip.next_keyframe_local_ns(local)
                        .map(|lt| clip.timeline_start.saturating_add(lt))
                })
            {
                drop(proj);
                on_seek(ns);
                area.queue_draw();
            }
        }
    });

    update_area_height(&area, &state);

    for lane in LANE_DEFS {
        let toggle = gtk::CheckButton::with_label(lane.label);
        toggle.set_active(true);
        toggle.add_css_class("small-btn");
        lanes_box.insert(&toggle, -1);
        let state_for_toggle = state.clone();
        let area_for_toggle = area.clone();
        let view_for_toggle = view.clone();
        toggle.connect_toggled(move |btn| {
            {
                let mut st = state_for_toggle.borrow_mut();
                if btn.is_active() {
                    st.visible_properties.insert(lane.property);
                } else {
                    st.visible_properties.remove(&lane.property);
                    st.selected_points
                        .retain(|point| point.property != lane.property);
                    if st.primary_point.map(|point| point.property) == Some(lane.property) {
                        st.primary_point = None;
                        st.drag = None;
                    }
                }
            }
            update_area_height(&area_for_toggle, &state_for_toggle);
            view_for_toggle.queue_redraw();
        });
    }

    area.set_draw_func({
        let project = project.clone();
        let timeline_state = timeline_state.clone();
        let state = state.clone();
        move |_area, cr, ww, wh| {
            let width = ww.max(1) as f64;
            let height = wh.max(1) as f64;
            let st = state.borrow();
            let lanes = visible_lanes(&st);
            let primary = st.primary_point;
            let selected_points = st.selected_points.clone();
            let zoom_x = st.zoom_x;
            let scroll_px = st.scroll_px;
            drop(st);

            cr.set_source_rgb(0.10, 0.10, 0.10);
            cr.rectangle(0.0, 0.0, width, height);
            cr.fill().ok();

            let (clip_id_opt, playhead_ns) = {
                let ts = timeline_state.borrow();
                (ts.selected_clip_id.clone(), ts.playhead_ns)
            };
            let Some(clip_id) = clip_id_opt else {
                cr.set_source_rgb(0.8, 0.8, 0.8);
                cr.select_font_face(
                    "Sans",
                    gtk::cairo::FontSlant::Normal,
                    gtk::cairo::FontWeight::Normal,
                );
                cr.set_font_size(12.0);
                cr.move_to(12.0, 26.0);
                let _ = cr.show_text("Select a clip to edit keyframes.");
                return;
            };

            let proj = project.borrow();
            let Some((track_idx, clip_idx, _track_id)) = selected_clip_location(&proj, &clip_id)
            else {
                cr.set_source_rgb(0.8, 0.8, 0.8);
                cr.set_font_size(12.0);
                cr.move_to(12.0, 26.0);
                let _ = cr.show_text("Selected clip is not available.");
                return;
            };
            let clip = &proj.tracks[track_idx].clips[clip_idx];
            let duration_ns = clip.duration().max(1);

            cr.set_source_rgba(0.17, 0.17, 0.17, 0.95);
            cr.rectangle(0.0, 0.0, width, HEADER_H);
            cr.fill().ok();

            let (x0, x1) = timeline_x_bounds(width);
            cr.set_source_rgba(0.55, 0.55, 0.55, 0.9);
            cr.set_font_size(10.0);
            for step in 0..=4 {
                let t = step as f64 / 4.0;
                let x = x0 + t * (x1 - x0);
                let ns = x_to_local_ns(x, duration_ns, width, zoom_x, scroll_px);
                let sec = ns as f64 / 1_000_000_000.0;
                cr.move_to(x + 2.0, 14.0);
                let _ = cr.show_text(&format!("{sec:.2}s"));
                cr.set_source_rgba(0.35, 0.35, 0.35, 0.9);
                cr.rectangle(x, HEADER_H - 4.0, 1.0, height - HEADER_H);
                cr.fill().ok();
                cr.set_source_rgba(0.55, 0.55, 0.55, 0.9);
            }

            for (row_idx, lane) in lanes.iter().enumerate() {
                let row_y = HEADER_H + row_idx as f64 * ROW_H;
                let selected_lane = primary.map(|point| point.property) == Some(lane.property);
                if selected_lane {
                    cr.set_source_rgba(0.18, 0.23, 0.30, 0.95);
                } else if row_idx % 2 == 0 {
                    cr.set_source_rgba(0.14, 0.14, 0.14, 0.95);
                } else {
                    cr.set_source_rgba(0.12, 0.12, 0.12, 0.95);
                }
                cr.rectangle(0.0, row_y, width, ROW_H);
                cr.fill().ok();

                cr.set_source_rgba(0.85, 0.85, 0.85, 0.95);
                cr.set_font_size(11.0);
                cr.move_to(8.0, row_y + 17.0);
                let _ = cr.show_text(lane.label);

                let center_y = row_y + ROW_H * 0.5;
                cr.set_source_rgba(0.25, 0.25, 0.25, 0.9);
                cr.set_line_width(1.0);
                cr.move_to(x0, center_y);
                cr.line_to(x1, center_y);
                cr.stroke().ok();

                let lane_keyframes = clip.keyframes_for_phase1_property(lane.property);
                if !lane_keyframes.is_empty() {
                    let default_value = clip.default_value_for_phase1_property(lane.property);
                    let samples = (((x1 - x0) / 7.0).round() as usize).clamp(24, 180);
                    cr.set_source_rgba(lane.color.0, lane.color.1, lane.color.2, 0.45);
                    cr.set_line_width(1.35);
                    for i in 0..samples {
                        let frac = if samples <= 1 {
                            0.0
                        } else {
                            i as f64 / (samples - 1) as f64
                        };
                        let x = x0 + frac * (x1 - x0);
                        let local_ns = x_to_local_ns(x, duration_ns, width, zoom_x, scroll_px);
                        let value = Clip::evaluate_keyframed_value(
                            lane_keyframes,
                            local_ns,
                            default_value,
                        );
                        let y = lane_y_for_value(row_y, lane.property, value);
                        if i == 0 {
                            cr.move_to(x, y);
                        } else {
                            cr.line_to(x, y);
                        }
                    }
                    cr.stroke().ok();
                }

                for keyframe in lane_keyframes {
                    let local = keyframe.time_ns.min(duration_ns);
                    let x = local_ns_to_x(local, duration_ns, width, zoom_x, scroll_px);
                    if x < x0 - 10.0 || x > x1 + 10.0 {
                        continue;
                    }
                    let y = lane_y_for_value(row_y, lane.property, keyframe.value);
                    let point = KeyframePoint {
                        property: lane.property,
                        local_time_ns: local,
                    };
                    let is_selected = selected_points.contains(&point);
                    let radius = if is_selected { 5.4 } else { 4.2 };
                    cr.set_source_rgba(lane.color.0, lane.color.1, lane.color.2, 0.95);
                    cr.arc(x, y, radius, 0.0, std::f64::consts::TAU);
                    cr.fill().ok();
                    cr.set_source_rgba(
                        if is_selected { 1.0 } else { 0.1 },
                        if is_selected { 0.90 } else { 0.1 },
                        if is_selected { 0.25 } else { 0.1 },
                        if is_selected { 0.95 } else { 0.7 },
                    );
                    cr.set_line_width(if is_selected { 1.4 } else { 1.0 });
                    cr.arc(x, y, radius, 0.0, std::f64::consts::TAU);
                    cr.stroke().ok();
                }
            }

            let local_playhead = clip
                .local_timeline_position_ns(playhead_ns)
                .min(duration_ns);
            let px = local_ns_to_x(local_playhead, duration_ns, width, zoom_x, scroll_px);
            cr.set_source_rgba(0.95, 0.30, 0.30, 0.95);
            cr.set_line_width(1.5);
            cr.move_to(px, HEADER_H);
            cr.line_to(px, HEADER_H + lanes.len() as f64 * ROW_H);
            cr.stroke().ok();
        }
    });

    let click = gtk::GestureClick::new();
    click.set_button(1);
    area.add_controller(click.clone());
    click.connect_pressed({
        let project = project.clone();
        let timeline_state = timeline_state.clone();
        let state = state.clone();
        let area = area.clone();
        let interp_dropdown = interp_dropdown.clone();
        move |gesture, _n_press, x, y| {
            let _ = area.grab_focus();
            let (clip_id_opt, _playhead_ns) = {
                let ts = timeline_state.borrow();
                (ts.selected_clip_id.clone(), ts.playhead_ns)
            };
            let Some(clip_id) = clip_id_opt else {
                return;
            };

            let width = area.width().max(1) as f64;
            let mut state_mut = state.borrow_mut();
            let lanes = visible_lanes(&state_mut);
            let zoom_x = state_mut.zoom_x;
            let scroll_px = state_mut.scroll_px;
            let Some((track_idx, clip_idx, _track_id)) = ({
                let proj = project.borrow();
                selected_clip_location(&proj, &clip_id)
            }) else {
                state_mut.selected_points.clear();
                state_mut.primary_point = None;
                state_mut.drag = None;
                area.queue_draw();
                return;
            };

            let proj = project.borrow();
            let clip = &proj.tracks[track_idx].clips[clip_idx];
            let duration_ns = clip.duration().max(1);

            let mut hit: Option<KeyframePoint> = None;
            for (row_idx, lane) in lanes.iter().enumerate() {
                let row_y = HEADER_H + row_idx as f64 * ROW_H;
                for keyframe in clip.keyframes_for_phase1_property(lane.property) {
                    let local = keyframe.time_ns.min(duration_ns);
                    let kx = local_ns_to_x(local, duration_ns, width, zoom_x, scroll_px);
                    let ky = lane_y_for_value(row_y, lane.property, keyframe.value);
                    let dx = x - kx;
                    let dy = y - ky;
                    if dx * dx + dy * dy <= KEYFRAME_HIT_RADIUS * KEYFRAME_HIT_RADIUS {
                        hit = Some(KeyframePoint {
                            property: lane.property,
                            local_time_ns: local,
                        });
                        interp_dropdown
                            .set_selected(interp_to_dropdown_idx(keyframe.interpolation));
                        break;
                    }
                }
                if hit.is_some() {
                    break;
                }
            }

            let mods = gesture.current_event_state();
            let additive = mods.contains(gtk::gdk::ModifierType::CONTROL_MASK)
                || mods.contains(gtk::gdk::ModifierType::META_MASK);
            let range = mods.contains(gtk::gdk::ModifierType::SHIFT_MASK);

            if let Some(point) = hit {
                if range {
                    if let Some(anchor) = state_mut.primary_point {
                        if anchor.property == point.property {
                            let mut points = if additive {
                                state_mut.selected_points.clone()
                            } else {
                                HashSet::new()
                            };
                            let low = anchor.local_time_ns.min(point.local_time_ns);
                            let high = anchor.local_time_ns.max(point.local_time_ns);
                            for kf in clip.keyframes_for_phase1_property(point.property) {
                                let local = kf.time_ns.min(duration_ns);
                                if local >= low && local <= high {
                                    points.insert(KeyframePoint {
                                        property: point.property,
                                        local_time_ns: local,
                                    });
                                }
                            }
                            state_mut.selected_points = points;
                            state_mut.primary_point = Some(point);
                        } else {
                            state_mut.selected_points.clear();
                            state_mut.selected_points.insert(point);
                            state_mut.primary_point = Some(point);
                        }
                    } else {
                        state_mut.selected_points.clear();
                        state_mut.selected_points.insert(point);
                        state_mut.primary_point = Some(point);
                    }
                } else if additive {
                    if !state_mut.selected_points.insert(point) {
                        state_mut.selected_points.remove(&point);
                    }
                    state_mut.primary_point = Some(point);
                } else {
                    state_mut.selected_points.clear();
                    state_mut.selected_points.insert(point);
                    state_mut.primary_point = Some(point);
                }
            } else if let Some(lane_idx) = lane_index_at_y(y, lanes.len()) {
                state_mut.selected_points.clear();
                state_mut.primary_point = Some(KeyframePoint {
                    property: lanes[lane_idx].property,
                    local_time_ns: x_to_local_ns(x, duration_ns, width, zoom_x, scroll_px),
                });
                state_mut.drag = None;
            } else {
                state_mut.selected_points.clear();
                state_mut.primary_point = None;
                state_mut.drag = None;
            }
            area.queue_draw();
        }
    });

    let drag = gtk::GestureDrag::new();
    area.add_controller(drag.clone());
    drag.connect_drag_begin({
        let project = project.clone();
        let timeline_state = timeline_state.clone();
        let state = state.clone();
        let area = area.clone();
        move |_gesture, x, y| {
            let (clip_id_opt, _playhead_ns) = {
                let ts = timeline_state.borrow();
                (ts.selected_clip_id.clone(), ts.playhead_ns)
            };
            let Some(clip_id) = clip_id_opt else {
                return;
            };
            let width = area.width().max(1) as f64;

            let lanes = {
                let st = state.borrow();
                visible_lanes(&st)
            };
            let Some(lane_idx) = lane_index_at_y(y, lanes.len()) else {
                return;
            };
            let lane = lanes[lane_idx];

            let mut state_mut = state.borrow_mut();
            let Some(primary_point) = state_mut.primary_point else {
                return;
            };
            let zoom_x = state_mut.zoom_x;
            let scroll_px = state_mut.scroll_px;
            if primary_point.property != lane.property {
                return;
            }

            let proj = project.borrow();
            let Some((track_idx, clip_idx, track_id)) = selected_clip_location(&proj, &clip_id)
            else {
                return;
            };
            let clip = &proj.tracks[track_idx].clips[clip_idx];
            let duration_ns = clip.duration().max(1);
            let kx = local_ns_to_x(
                primary_point.local_time_ns.min(duration_ns),
                duration_ns,
                width,
                zoom_x,
                scroll_px,
            );
            let Some(keyframe) = clip
                .keyframes_for_phase1_property(primary_point.property)
                .iter()
                .find(|kf| kf.time_ns.min(duration_ns) == primary_point.local_time_ns)
            else {
                return;
            };
            let ky = lane_y_for_value(HEADER_H + lane_idx as f64 * ROW_H, primary_point.property, keyframe.value);
            let dx = x - kx;
            let dy = y - ky;
            if dx * dx + dy * dy > KEYFRAME_HIT_RADIUS * KEYFRAME_HIT_RADIUS {
                return;
            }

            state_mut.drag = Some(DragState {
                track_id,
                clip_id,
                property: primary_point.property,
                source_local_ns: primary_point.local_time_ns,
                current_local_ns: primary_point.local_time_ns,
                start_x: x,
                original_track_clips: proj.tracks[track_idx].clips.clone(),
            });
        }
    });

    drag.connect_drag_update({
        let project = project.clone();
        let state = state.clone();
        let area = area.clone();
        move |_gesture, dx, _dy| {
            let mut state_mut = state.borrow_mut();
            let Some(drag_state) = state_mut.drag.clone() else {
                return;
            };
            let width = area.width().max(1) as f64;
            let zoom_x = state_mut.zoom_x;
            let scroll_px = state_mut.scroll_px;
            let duration_ns = drag_state
                .original_track_clips
                .iter()
                .find(|clip| clip.id == drag_state.clip_id)
                .map(|clip| clip.duration())
                .unwrap_or(0)
                .max(1);
            let target_local_ns = x_to_local_ns(
                drag_state.start_x + dx,
                duration_ns,
                width,
                zoom_x,
                scroll_px,
            );
            if target_local_ns == drag_state.current_local_ns {
                return;
            }

            {
                let mut proj = project.borrow_mut();
                if let Some(track) = proj.tracks.iter_mut().find(|t| t.id == drag_state.track_id) {
                    if let Some(current_idx) =
                        track.clips.iter().position(|c| c.id == drag_state.clip_id)
                    {
                        if let Some(original_clip) = drag_state
                            .original_track_clips
                            .iter()
                            .find(|clip| clip.id == drag_state.clip_id)
                            .cloned()
                        {
                            let mut updated = original_clip;
                            if move_keyframe_in_property(
                                &mut updated,
                                drag_state.property,
                                drag_state.source_local_ns,
                                target_local_ns,
                            ) {
                                track.clips[current_idx] = updated;
                                proj.dirty = true;
                            }
                        }
                    }
                }
            }

            if let Some(drag) = state_mut.drag.as_mut() {
                drag.current_local_ns = target_local_ns;
            }
            let old_point = KeyframePoint {
                property: drag_state.property,
                local_time_ns: drag_state.current_local_ns,
            };
            let new_point = KeyframePoint {
                property: drag_state.property,
                local_time_ns: target_local_ns,
            };
            if state_mut.selected_points.remove(&old_point) {
                state_mut.selected_points.insert(new_point);
            }
            if state_mut.selected_points.is_empty() {
                state_mut.selected_points.insert(new_point);
            }
            state_mut.primary_point = Some(new_point);
            area.queue_draw();
        }
    });

    drag.connect_drag_end({
        let project = project.clone();
        let timeline_state = timeline_state.clone();
        let state = state.clone();
        let on_project_changed = on_project_changed.clone();
        let area = area.clone();
        move |_gesture, _x, _y| {
            let drag_state_opt = {
                let mut st = state.borrow_mut();
                st.drag.take()
            };
            let Some(drag_state) = drag_state_opt else {
                return;
            };

            let new_track_clips = {
                let proj = project.borrow();
                proj.tracks
                    .iter()
                    .find(|t| t.id == drag_state.track_id)
                    .map(|t| t.clips.clone())
                    .unwrap_or_default()
            };
            if new_track_clips != drag_state.original_track_clips {
                let cmd = SetTrackClipsCommand {
                    track_id: drag_state.track_id.clone(),
                    old_clips: drag_state.original_track_clips,
                    new_clips: new_track_clips,
                    label: "Move keyframe".to_string(),
                };
                {
                    let mut ts = timeline_state.borrow_mut();
                    let mut proj = project.borrow_mut();
                    ts.history.execute(Box::new(cmd), &mut proj);
                }
                on_project_changed();
            }
            area.queue_draw();
        }
    });

    let remove_selected_keyframe: Rc<dyn Fn()> = {
        let project = project.clone();
        let timeline_state = timeline_state.clone();
        let state = state.clone();
        let on_project_changed = on_project_changed.clone();
        let area = area.clone();
        Rc::new(move || {
            let clip_id_opt = timeline_state.borrow().selected_clip_id.clone();
            let Some(clip_id) = clip_id_opt else {
                return;
            };
            let points = {
                let st = state.borrow();
                if !st.selected_points.is_empty() {
                    st.selected_points.iter().copied().collect::<Vec<_>>()
                } else if let Some(primary) = st.primary_point {
                    vec![primary]
                } else {
                    Vec::new()
                }
            };
            if points.is_empty() {
                return;
            }
            let mut changed = false;
            {
                let mut proj = project.borrow_mut();
                let Some((track_idx, clip_idx, track_id)) = selected_clip_location(&proj, &clip_id)
                else {
                    return;
                };
                let old_clips = proj.tracks[track_idx].clips.clone();
                let clip = &mut proj.tracks[track_idx].clips[clip_idx];
                let mut removed_any = false;
                for point in &points {
                    let timeline_target = clip.timeline_start.saturating_add(point.local_time_ns);
                    if clip.remove_phase1_keyframe_at_timeline_ns(point.property, timeline_target) {
                        removed_any = true;
                    }
                }
                if removed_any {
                    let new_clips = proj.tracks[track_idx].clips.clone();
                    let cmd = SetTrackClipsCommand {
                        track_id,
                        old_clips,
                        new_clips,
                        label: "Remove keyframe".to_string(),
                    };
                    timeline_state
                        .borrow_mut()
                        .history
                        .execute(Box::new(cmd), &mut proj);
                    changed = true;
                }
            }
            if changed {
                let mut st = state.borrow_mut();
                st.selected_points.clear();
                st.primary_point = None;
                on_project_changed();
            }
            area.queue_draw();
        })
    };

    let nudge_selected_keyframe: Rc<dyn Fn(i64)> = {
        let project = project.clone();
        let timeline_state = timeline_state.clone();
        let state = state.clone();
        let on_project_changed = on_project_changed.clone();
        let area = area.clone();
        Rc::new(move |delta_ns: i64| {
            if delta_ns == 0 {
                return;
            }
            let clip_id_opt = timeline_state.borrow().selected_clip_id.clone();
            let Some(clip_id) = clip_id_opt else {
                return;
            };
            let points = {
                let st = state.borrow();
                if !st.selected_points.is_empty() {
                    st.selected_points.iter().copied().collect::<Vec<_>>()
                } else if let Some(primary) = st.primary_point {
                    vec![primary]
                } else {
                    Vec::new()
                }
            };
            if points.is_empty() {
                return;
            }

            let mut changed = false;
            let mut moved_points: Vec<(KeyframePoint, KeyframePoint)> = Vec::new();
            {
                let mut proj = project.borrow_mut();
                let Some((track_idx, clip_idx, track_id)) = selected_clip_location(&proj, &clip_id)
                else {
                    return;
                };
                let old_clips = proj.tracks[track_idx].clips.clone();
                let clip = &mut proj.tracks[track_idx].clips[clip_idx];
                let duration_ns = clip.duration().max(1);
                for point in &points {
                    let target_local = (i128::from(point.local_time_ns) + i128::from(delta_ns))
                        .clamp(0, i128::from(duration_ns)) as u64;
                    if target_local == point.local_time_ns {
                        continue;
                    }
                    if move_keyframe_in_property(
                        clip,
                        point.property,
                        point.local_time_ns,
                        target_local,
                    ) {
                        changed = true;
                        moved_points.push((
                            *point,
                            KeyframePoint {
                                property: point.property,
                                local_time_ns: target_local,
                            },
                        ));
                    }
                }
                if changed {
                    let new_clips = proj.tracks[track_idx].clips.clone();
                    let cmd = SetTrackClipsCommand {
                        track_id,
                        old_clips,
                        new_clips,
                        label: "Nudge keyframe".to_string(),
                    };
                    timeline_state
                        .borrow_mut()
                        .history
                        .execute(Box::new(cmd), &mut proj);
                }
            }
            if changed {
                let mut st = state.borrow_mut();
                let mut next_points = st.selected_points.clone();
                for (old_point, new_point) in &moved_points {
                    next_points.remove(old_point);
                    next_points.insert(*new_point);
                }
                if next_points.is_empty() {
                    for (_, new_point) in &moved_points {
                        next_points.insert(*new_point);
                    }
                }
                st.selected_points = next_points;
                if let Some(primary) = st.primary_point {
                    if let Some((_, replacement)) =
                        moved_points.iter().find(|(old, _)| *old == primary)
                    {
                        st.primary_point = Some(*replacement);
                    }
                }
                on_project_changed();
            }
            area.queue_draw();
        })
    };

    add_btn.connect_clicked({
        let project = project.clone();
        let timeline_state = timeline_state.clone();
        let state = state.clone();
        let interp_dropdown = interp_dropdown.clone();
        let on_project_changed = on_project_changed.clone();
        let area = area.clone();
        move |_| {
            let (clip_id_opt, playhead_ns) = {
                let ts = timeline_state.borrow();
                (ts.selected_clip_id.clone(), ts.playhead_ns)
            };
            let Some(clip_id) = clip_id_opt else {
                return;
            };
            let property = {
                let st = state.borrow();
                if let Some(primary) = st.primary_point {
                    primary.property
                } else {
                    visible_lanes(&st)
                        .first()
                        .map(|lane| lane.property)
                        .unwrap_or(Phase1KeyframeProperty::Scale)
                }
            };
            let interpolation = dropdown_idx_to_interp(interp_dropdown.selected());

            let mut changed = false;
            let selected_local;
            {
                let mut proj = project.borrow_mut();
                let Some((track_idx, clip_idx, track_id)) = selected_clip_location(&proj, &clip_id)
                else {
                    return;
                };
                let old_clips = proj.tracks[track_idx].clips.clone();
                let clip = &mut proj.tracks[track_idx].clips[clip_idx];
                let value = clip.value_for_phase1_property_at_timeline_ns(property, playhead_ns);
                selected_local = clip.upsert_phase1_keyframe_at_timeline_ns_with_interp(
                    property,
                    playhead_ns,
                    value,
                    interpolation,
                );
                let new_clips = proj.tracks[track_idx].clips.clone();
                if new_clips != old_clips {
                    let cmd = SetTrackClipsCommand {
                        track_id,
                        old_clips,
                        new_clips,
                        label: "Add keyframe".to_string(),
                    };
                    timeline_state
                        .borrow_mut()
                        .history
                        .execute(Box::new(cmd), &mut proj);
                    changed = true;
                }
            }
            {
                let mut st = state.borrow_mut();
                let point = KeyframePoint {
                    property,
                    local_time_ns: selected_local,
                };
                st.selected_points.clear();
                st.selected_points.insert(point);
                st.primary_point = Some(point);
            }
            if changed {
                on_project_changed();
            }
            area.queue_draw();
        }
    });

    remove_btn.connect_clicked({
        let remove_selected_keyframe = remove_selected_keyframe.clone();
        move |_| remove_selected_keyframe()
    });

    apply_interp_btn.connect_clicked({
        let project = project.clone();
        let timeline_state = timeline_state.clone();
        let state = state.clone();
        let interp_dropdown = interp_dropdown.clone();
        let on_project_changed = on_project_changed.clone();
        let area = area.clone();
        move |_| {
            let points = {
                let st = state.borrow();
                if !st.selected_points.is_empty() {
                    st.selected_points.iter().copied().collect::<Vec<_>>()
                } else if let Some(primary) = st.primary_point {
                    vec![primary]
                } else {
                    Vec::new()
                }
            };
            if points.is_empty() {
                return;
            }
            let clip_id_opt = timeline_state.borrow().selected_clip_id.clone();
            let Some(clip_id) = clip_id_opt else {
                return;
            };
            let interpolation = dropdown_idx_to_interp(interp_dropdown.selected());
            let mut changed = false;
            {
                let mut proj = project.borrow_mut();
                let Some((track_idx, clip_idx, track_id)) = selected_clip_location(&proj, &clip_id)
                else {
                    return;
                };
                let old_clips = proj.tracks[track_idx].clips.clone();
                let clip = &mut proj.tracks[track_idx].clips[clip_idx];
                let mut changed_any = false;
                for point in &points {
                    let keyframes = clip.keyframes_for_phase1_property_mut(point.property);
                    if let Some(kf) = keyframes
                        .iter_mut()
                        .find(|kf| kf.time_ns == point.local_time_ns)
                    {
                        if kf.interpolation != interpolation {
                            kf.interpolation = interpolation;
                            changed_any = true;
                        }
                    }
                }
                if changed_any {
                    let new_clips = proj.tracks[track_idx].clips.clone();
                    let cmd = SetTrackClipsCommand {
                        track_id,
                        old_clips,
                        new_clips,
                        label: "Change keyframe interpolation".to_string(),
                    };
                    timeline_state
                        .borrow_mut()
                        .history
                        .execute(Box::new(cmd), &mut proj);
                    changed = true;
                }
            }
            if changed {
                on_project_changed();
            }
            area.queue_draw();
        }
    });

    let key_ctrl = gtk::EventControllerKey::new();
    key_ctrl.connect_key_pressed({
        let project = project.clone();
        let remove_selected_keyframe = remove_selected_keyframe.clone();
        let nudge_selected_keyframe = nudge_selected_keyframe.clone();
        move |_, key, _, mods| {
            use gtk::gdk::{Key, ModifierType};
            match key {
                Key::Delete | Key::BackSpace => {
                    remove_selected_keyframe();
                    gtk::glib::Propagation::Stop
                }
                Key::Left | Key::Right => {
                    let frame_ns = project.borrow().frame_rate.frame_duration_ns() as i64;
                    let multiplier = if mods.contains(ModifierType::SHIFT_MASK) {
                        10
                    } else {
                        1
                    };
                    let step = frame_ns.saturating_mul(multiplier);
                    let delta = if key == Key::Left { -step } else { step };
                    nudge_selected_keyframe(delta);
                    gtk::glib::Propagation::Stop
                }
                _ => gtk::glib::Propagation::Proceed,
            }
        }
    });
    area.add_controller(key_ctrl);

    let scroll_ctrl = gtk::EventControllerScroll::new(gtk::EventControllerScrollFlags::BOTH_AXES);
    scroll_ctrl.connect_scroll({
        let state = state.clone();
        let area = area.clone();
        let apply_zoom = apply_zoom.clone();
        move |ec, dx, dy| {
            let mods = ec.current_event_state();
            if mods.contains(gtk::gdk::ModifierType::CONTROL_MASK) {
                let primary = if dy.abs() > f64::EPSILON { dy } else { dx };
                if primary.abs() > f64::EPSILON {
                    let z = state.borrow().zoom_x;
                    let factor = if primary < 0.0 { 1.1 } else { 0.9 };
                    apply_zoom(z * factor);
                }
                return gtk::glib::Propagation::Stop;
            }

            let pan_units = if dx.abs() > dy.abs() { dx } else { dy };
            if pan_units.abs() > f64::EPSILON {
                let width = area.width().max(1) as f64;
                let mut st = state.borrow_mut();
                st.scroll_px = clamp_scroll_px(st.scroll_px + pan_units * 28.0, width, st.zoom_x);
                drop(st);
                area.queue_draw();
            }
            gtk::glib::Propagation::Stop
        }
    });
    area.add_controller(scroll_ctrl);

    (root, view)
}
