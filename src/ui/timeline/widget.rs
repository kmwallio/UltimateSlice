use gtk4::prelude::*;
use gtk4::{self as gtk, DrawingArea, GestureClick, GestureDrag, EventControllerKey};
use glib;
use std::cell::RefCell;
use std::rc::Rc;
use crate::model::project::Project;
use crate::model::track::TrackKind;

const TRACK_HEIGHT: f64 = 60.0;
const TRACK_LABEL_WIDTH: f64 = 80.0;
const RULER_HEIGHT: f64 = 24.0;
const PIXELS_PER_SECOND_DEFAULT: f64 = 100.0;
const NS_PER_SECOND: f64 = 1_000_000_000.0;

/// Shared state for the timeline widget
pub struct TimelineState {
    pub project: Rc<RefCell<Project>>,
    pub pixels_per_second: f64,
    pub scroll_offset: f64, // horizontal scroll in pixels
    pub playhead_ns: u64,
    pub selected_clip_id: Option<String>,
    /// Callback fired when user seeks (clicks in ruler or drags playhead)
    pub on_seek: Option<Box<dyn Fn(u64)>>,
}

impl TimelineState {
    pub fn new(project: Rc<RefCell<Project>>) -> Self {
        Self {
            project,
            pixels_per_second: PIXELS_PER_SECOND_DEFAULT,
            scroll_offset: 0.0,
            playhead_ns: 0,
            selected_clip_id: None,
            on_seek: None,
        }
    }

    pub fn ns_to_x(&self, ns: u64) -> f64 {
        TRACK_LABEL_WIDTH + (ns as f64 / NS_PER_SECOND) * self.pixels_per_second - self.scroll_offset
    }

    pub fn x_to_ns(&self, x: f64) -> u64 {
        let secs = (x - TRACK_LABEL_WIDTH + self.scroll_offset) / self.pixels_per_second;
        (secs.max(0.0) * NS_PER_SECOND) as u64
    }
}

/// Build and return the timeline `DrawingArea` widget.
pub fn build_timeline(state: Rc<RefCell<TimelineState>>) -> DrawingArea {
    let area = DrawingArea::new();
    area.set_vexpand(false);
    area.set_hexpand(true);
    area.set_content_height(
        (RULER_HEIGHT + TRACK_HEIGHT * 4.0) as i32, // reserve space for a few tracks
    );

    // Drawing
    {
        let state = state.clone();
        area.set_draw_func(move |_area, cr, width, height| {
            draw_timeline(cr, width, height, &state.borrow());
        });
    }

    // Click to seek (in ruler region) or select clip
    let click = GestureClick::new();
    {
        let state = state.clone();
        let area_weak = area.downgrade();
        click.connect_pressed(move |_gesture, _n_press, x, y| {
            let mut st = state.borrow_mut();
            if y < RULER_HEIGHT {
                // Seek
                let ns = st.x_to_ns(x);
                st.playhead_ns = ns;
                if let Some(ref cb) = st.on_seek {
                    cb(ns);
                }
            } else {
                // Clip selection — find the clip_id first, then set it (avoids borrow conflict)
                let track_idx = ((y - RULER_HEIGHT) / TRACK_HEIGHT) as usize;
                let found_id = {
                    let proj = st.project.borrow();
                    proj.tracks.get(track_idx).and_then(|track| {
                        track.clips.iter().find(|clip| {
                            let cx = st.ns_to_x(clip.timeline_start);
                            let cw = (clip.duration() as f64 / NS_PER_SECOND) * st.pixels_per_second;
                            x >= cx && x <= cx + cw
                        }).map(|clip| clip.id.clone())
                    })
                };
                if found_id.is_some() {
                    st.selected_clip_id = found_id;
                }
            }
            if let Some(area) = area_weak.upgrade() {
                area.queue_draw();
            }
        });
    }
    area.add_controller(click);

    // Scroll wheel → zoom
    let scroll = gtk::EventControllerScroll::new(
        gtk::EventControllerScrollFlags::VERTICAL,
    );
    {
        let state = state.clone();
        let area_weak = area.downgrade();
        scroll.connect_scroll(move |_ctrl, _dx, dy| {
            let mut st = state.borrow_mut();
            let factor = if dy < 0.0 { 1.1 } else { 0.9 };
            st.pixels_per_second = (st.pixels_per_second * factor).clamp(10.0, 2000.0);
            if let Some(area) = area_weak.upgrade() {
                area.queue_draw();
            }
            glib::Propagation::Stop
        });
    }
    area.add_controller(scroll);

    area
}

/// Cairo drawing of the entire timeline
fn draw_timeline(cr: &gtk::cairo::Context, width: i32, height: i32, st: &TimelineState) {
    let w = width as f64;
    let h = height as f64;

    // Background
    cr.set_source_rgb(0.13, 0.13, 0.15);
    cr.paint().ok();

    // Ruler
    draw_ruler(cr, w, st);

    // Tracks
    let proj = st.project.borrow();
    for (i, track) in proj.tracks.iter().enumerate() {
        let y = RULER_HEIGHT + i as f64 * TRACK_HEIGHT;
        draw_track_row(cr, w, y, track, st);
    }

    // Playhead
    let ph_x = st.ns_to_x(st.playhead_ns);
    cr.set_source_rgb(1.0, 0.3, 0.3);
    cr.set_line_width(2.0);
    cr.move_to(ph_x, 0.0);
    cr.line_to(ph_x, h);
    cr.stroke().ok();

    // Playhead triangle at top
    cr.set_source_rgb(1.0, 0.3, 0.3);
    cr.move_to(ph_x - 6.0, 0.0);
    cr.line_to(ph_x + 6.0, 0.0);
    cr.line_to(ph_x, 12.0);
    cr.fill().ok();
}

fn draw_ruler(cr: &gtk::cairo::Context, width: f64, st: &TimelineState) {
    // Ruler background
    cr.set_source_rgb(0.2, 0.2, 0.22);
    cr.rectangle(0.0, 0.0, width, RULER_HEIGHT);
    cr.fill().ok();

    // Tick marks
    cr.set_source_rgb(0.6, 0.6, 0.6);
    cr.set_line_width(1.0);
    cr.set_font_size(10.0);

    let visible_secs = (width - TRACK_LABEL_WIDTH) / st.pixels_per_second;
    let start_sec = st.scroll_offset / st.pixels_per_second;

    // Choose tick spacing
    let tick_interval = choose_tick_interval(st.pixels_per_second);
    let first_tick = (start_sec / tick_interval).floor() * tick_interval;

    let mut t = first_tick;
    while t <= start_sec + visible_secs + tick_interval {
        let x = TRACK_LABEL_WIDTH + (t - start_sec) * st.pixels_per_second;
        if x >= TRACK_LABEL_WIDTH && x <= width {
            cr.move_to(x, RULER_HEIGHT - 8.0);
            cr.line_to(x, RULER_HEIGHT);
            cr.stroke().ok();

            let label = format_timecode(t);
            let _ = cr.move_to(x + 2.0, RULER_HEIGHT - 10.0);
            let _ = cr.show_text(&label);
        }
        t += tick_interval;
    }

    // Track label area separator
    cr.set_source_rgb(0.25, 0.25, 0.28);
    cr.rectangle(0.0, 0.0, TRACK_LABEL_WIDTH, RULER_HEIGHT);
    cr.fill().ok();
}

fn draw_track_row(
    cr: &gtk::cairo::Context,
    width: f64,
    y: f64,
    track: &crate::model::track::Track,
    st: &TimelineState,
) {
    // Track background
    let (r, g, b) = match track.kind {
        TrackKind::Video => (0.16, 0.16, 0.18),
        TrackKind::Audio => (0.14, 0.16, 0.18),
    };
    cr.set_source_rgb(r, g, b);
    cr.rectangle(TRACK_LABEL_WIDTH, y, width - TRACK_LABEL_WIDTH, TRACK_HEIGHT);
    cr.fill().ok();

    // Track label panel
    cr.set_source_rgb(0.22, 0.22, 0.25);
    cr.rectangle(0.0, y, TRACK_LABEL_WIDTH, TRACK_HEIGHT);
    cr.fill().ok();

    cr.set_source_rgb(0.8, 0.8, 0.8);
    cr.set_font_size(11.0);
    let _ = cr.move_to(6.0, y + TRACK_HEIGHT / 2.0 + 4.0);
    let _ = cr.show_text(&track.label);

    // Track separator line
    cr.set_source_rgb(0.1, 0.1, 0.12);
    cr.set_line_width(1.0);
    cr.move_to(0.0, y + TRACK_HEIGHT);
    cr.line_to(width, y + TRACK_HEIGHT);
    cr.stroke().ok();

    // Clips
    for clip in &track.clips {
        draw_clip(cr, y, clip, track, st);
    }
}

fn draw_clip(
    cr: &gtk::cairo::Context,
    track_y: f64,
    clip: &crate::model::clip::Clip,
    track: &crate::model::track::Track,
    st: &TimelineState,
) {
    let cx = st.ns_to_x(clip.timeline_start);
    let cw = (clip.duration() as f64 / NS_PER_SECOND) * st.pixels_per_second;
    let cy = track_y + 2.0;
    let ch = TRACK_HEIGHT - 4.0;

    if cx + cw < TRACK_LABEL_WIDTH || cx > 3000.0 {
        return; // off-screen
    }

    let is_selected = st.selected_clip_id.as_deref() == Some(&clip.id);

    // Clip body
    let (r, g, b) = match track.kind {
        TrackKind::Video => (0.17, 0.47, 0.85),
        TrackKind::Audio => (0.18, 0.65, 0.45),
    };
    cr.set_source_rgb(r, g, b);
    rounded_rect(cr, cx, cy, cw.max(4.0), ch, 4.0);
    cr.fill().ok();

    // Selection outline
    if is_selected {
        cr.set_source_rgb(1.0, 0.85, 0.0);
        cr.set_line_width(2.0);
        rounded_rect(cr, cx, cy, cw.max(4.0), ch, 4.0);
        cr.stroke().ok();
    }

    // Clip label
    if cw > 30.0 {
        cr.set_source_rgb(1.0, 1.0, 1.0);
        cr.set_font_size(11.0);
        let _ = cr.move_to(cx + 6.0, cy + ch / 2.0 + 4.0);
        // Truncate label to fit
        let _ = cr.show_text(&clip.label);
    }
}

fn rounded_rect(cr: &gtk::cairo::Context, x: f64, y: f64, w: f64, h: f64, r: f64) {
    cr.new_sub_path();
    cr.arc(x + r, y + r, r, std::f64::consts::PI, 3.0 * std::f64::consts::PI / 2.0);
    cr.arc(x + w - r, y + r, r, 3.0 * std::f64::consts::PI / 2.0, 0.0);
    cr.arc(x + w - r, y + h - r, r, 0.0, std::f64::consts::PI / 2.0);
    cr.arc(x + r, y + h - r, r, std::f64::consts::PI / 2.0, std::f64::consts::PI);
    cr.close_path();
}

fn choose_tick_interval(pixels_per_second: f64) -> f64 {
    // How many seconds between ticks to keep labels readable (~80px apart)
    let target_px = 80.0;
    let raw = target_px / pixels_per_second;
    // Round to a nice value
    for &nice in &[0.1, 0.5, 1.0, 5.0, 10.0, 30.0, 60.0, 300.0, 600.0] {
        if raw <= nice { return nice; }
    }
    600.0
}

fn format_timecode(secs: f64) -> String {
    let secs = secs.max(0.0) as u64;
    let h = secs / 3600;
    let m = (secs % 3600) / 60;
    let s = secs % 60;
    if h > 0 {
        format!("{h}:{m:02}:{s:02}")
    } else {
        format!("{m}:{s:02}")
    }
}
