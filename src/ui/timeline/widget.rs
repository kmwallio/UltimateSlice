use gtk4::prelude::*;
use gtk4::{self as gtk, DrawingArea, GestureClick, GestureDrag, EventControllerKey, EventControllerScroll};
use glib;
use std::cell::RefCell;
use std::rc::Rc;
use crate::model::project::Project;
use crate::model::track::TrackKind;
use crate::undo::{EditHistory, DeleteClipCommand, MoveClipCommand, SplitClipCommand, TrimClipCommand, TrimOutCommand};

const TRACK_HEIGHT: f64 = 60.0;
const TRACK_LABEL_WIDTH: f64 = 80.0;
const RULER_HEIGHT: f64 = 24.0;
const PIXELS_PER_SECOND_DEFAULT: f64 = 100.0;
const NS_PER_SECOND: f64 = 1_000_000_000.0;
/// Pixels from clip edge that activate trim mode
const TRIM_HANDLE_PX: f64 = 10.0;

#[derive(Debug, Clone, PartialEq)]
pub enum ActiveTool {
    Select,
    Razor,
}

/// What a drag gesture is currently doing
#[derive(Debug, Clone)]
enum DragOp {
    None,
    /// Moving a clip: (clip_id, track_id, original_timeline_start, drag_offset_in_clip_ns)
    MoveClip {
        clip_id: String,
        track_id: String,
        original_start: u64,
        clip_offset_ns: u64,
    },
    /// Trimming the in-point of a clip
    TrimIn {
        clip_id: String,
        track_id: String,
        original_source_in: u64,
        original_timeline_start: u64,
    },
    /// Trimming the out-point of a clip
    TrimOut {
        clip_id: String,
        track_id: String,
        original_source_out: u64,
    },
}

/// Shared state for the timeline widget
pub struct TimelineState {
    pub project: Rc<RefCell<Project>>,
    pub history: EditHistory,
    pub active_tool: ActiveTool,
    pub pixels_per_second: f64,
    pub scroll_offset: f64,
    pub playhead_ns: u64,
    pub selected_clip_id: Option<String>,
    pub selected_track_id: Option<String>,
    drag_op: DragOp,
    /// Callback fired when user seeks — use Rc so it can be cloned out before releasing the RefMut
    pub on_seek: Option<Rc<dyn Fn(u64)>>,
    /// Callback fired when project changes — use Rc so it can be cloned out before releasing the RefMut
    pub on_project_changed: Option<Rc<dyn Fn()>>,
    /// Callback fired when the user presses Space to toggle play/pause
    pub on_play_pause: Option<Rc<dyn Fn()>>,
    /// Called when a clip is dropped from the media browser: (source_path, duration_ns, track_idx, timeline_start_ns)
    pub on_drop_clip: Option<Rc<dyn Fn(String, u64, usize, u64)>>,
}

impl TimelineState {
    pub fn new(project: Rc<RefCell<Project>>) -> Self {
        Self {
            project,
            history: EditHistory::new(),
            active_tool: ActiveTool::Select,
            pixels_per_second: PIXELS_PER_SECOND_DEFAULT,
            scroll_offset: 0.0,
            playhead_ns: 0,
            selected_clip_id: None,
            selected_track_id: None,
            drag_op: DragOp::None,
            on_seek: None,
            on_project_changed: None,
            on_play_pause: None,
            on_drop_clip: None,
        }
    }

    pub fn ns_to_x(&self, ns: u64) -> f64 {
        TRACK_LABEL_WIDTH + (ns as f64 / NS_PER_SECOND) * self.pixels_per_second - self.scroll_offset
    }

    pub fn x_to_ns(&self, x: f64) -> u64 {
        let secs = (x - TRACK_LABEL_WIDTH + self.scroll_offset) / self.pixels_per_second;
        (secs.max(0.0) * NS_PER_SECOND) as u64
    }

    pub fn undo(&mut self) {
        let mut proj = self.project.borrow_mut();
        self.history.undo(&mut proj);
    }

    pub fn redo(&mut self) {
        let mut proj = self.project.borrow_mut();
        self.history.redo(&mut proj);
    }

    /// Delete the currently selected clip
    pub fn delete_selected(&mut self) {
        let Some(ref clip_id) = self.selected_clip_id.clone() else { return };
        let (found_clip, found_track_id) = {
            let proj = self.project.borrow();
            let mut found = None;
            for track in &proj.tracks {
                if let Some(clip) = track.clips.iter().find(|c| &c.id == clip_id) {
                    found = Some((clip.clone(), track.id.clone()));
                    break;
                }
            }
            found.unzip()
        };
        if let (Some(clip), Some(track_id)) = (found_clip, found_track_id) {
            let mut proj = self.project.borrow_mut();
            self.history.execute(Box::new(DeleteClipCommand { clip, track_id }), &mut proj);
        }
        self.selected_clip_id = None;
        self.selected_track_id = None;
    }

    /// Razor cut the selected clip (or any clip at playhead) at the playhead position
    pub fn razor_cut_at_playhead(&mut self) {
        let playhead = self.playhead_ns;
        let (clip_to_cut, track_id) = {
            let proj = self.project.borrow();
            let mut found = None;
            'outer: for track in &proj.tracks {
                for clip in &track.clips {
                    if clip.timeline_start < playhead && clip.timeline_end() > playhead {
                        found = Some((clip.clone(), track.id.clone()));
                        break 'outer;
                    }
                }
            }
            match found {
                Some((c, t)) => (Some(c), Some(t)),
                None => (None, None),
            }
        };
        if let (Some(orig), Some(track_id)) = (clip_to_cut, track_id) {
            let cut_offset = playhead - orig.timeline_start;
            let right_source_in = orig.source_in + cut_offset;

            let mut right_clip = orig.clone();
            right_clip.id = uuid::Uuid::new_v4().to_string();
            right_clip.source_in = right_source_in;
            right_clip.timeline_start = playhead;

            let cmd = SplitClipCommand {
                original_clip: orig,
                track_id,
                split_ns: playhead,
                right_clip,
            };
            let mut proj = self.project.borrow_mut();
            self.history.execute(Box::new(cmd), &mut proj);
        }
    }

    /// Find which clip and track are at a given (x, y) coordinate.
    /// Also returns whether x is near the in-edge or out-edge (for trimming).
    fn hit_test(&self, x: f64, y: f64) -> Option<HitResult> {
        let track_idx = if y > RULER_HEIGHT {
            ((y - RULER_HEIGHT) / TRACK_HEIGHT) as usize
        } else {
            return None;
        };

        let proj = self.project.borrow();
        let track = proj.tracks.get(track_idx)?;
        for clip in &track.clips {
            let cx = self.ns_to_x(clip.timeline_start);
            let cw = (clip.duration() as f64 / NS_PER_SECOND) * self.pixels_per_second;

            if x >= cx && x <= cx + cw {
                let zone = if x - cx < TRIM_HANDLE_PX {
                    HitZone::TrimIn
                } else if (cx + cw) - x < TRIM_HANDLE_PX {
                    HitZone::TrimOut
                } else {
                    HitZone::Body
                };
                return Some(HitResult {
                    clip_id: clip.id.clone(),
                    track_id: track.id.clone(),
                    track_idx,
                    zone,
                });
            }
        }
        None
    }
}

struct HitResult {
    clip_id: String,
    track_id: String,
    track_idx: usize,
    zone: HitZone,
}

#[derive(Debug, Clone, PartialEq)]
enum HitZone {
    TrimIn,
    TrimOut,
    Body,
}

/// Build and return the timeline `DrawingArea` widget.
pub fn build_timeline(state: Rc<RefCell<TimelineState>>) -> DrawingArea {
    let area = DrawingArea::new();
    area.set_vexpand(false);
    area.set_hexpand(true);
    area.set_content_height((RULER_HEIGHT + TRACK_HEIGHT * 4.0) as i32);
    area.set_focusable(true);

    let thumb_cache = Rc::new(RefCell::new(
        crate::media::thumb_cache::ThumbnailCache::new()
    ));

    let wave_cache = Rc::new(RefCell::new(
        crate::media::waveform_cache::WaveformCache::new()
    ));

    // Drawing
    {
        let state = state.clone();
        let thumb_cache = thumb_cache.clone();
        let wave_cache = wave_cache.clone();
        area.set_draw_func(move |_area, cr, width, height| {
            let mut tcache = thumb_cache.borrow_mut();
            tcache.poll();
            let mut wcache = wave_cache.borrow_mut();
            wcache.poll();
            draw_timeline(cr, width, height, &state.borrow(), &mut tcache, &mut wcache);
        });
    }

    // Expose thumb_cache poll + queue_draw via a 200ms timer so new thumbnails/waveforms
    // trigger a repaint even when the user isn't moving the mouse.
    {
        let area_weak = area.downgrade();
        let thumb_cache = thumb_cache.clone();
        let wave_cache = wave_cache.clone();
        glib::timeout_add_local(std::time::Duration::from_millis(200), move || {
            let thumbs_done = thumb_cache.borrow_mut().poll();
            wave_cache.borrow_mut().poll();
            if thumbs_done {
                if let Some(a) = area_weak.upgrade() { a.queue_draw(); }
            }
            glib::ControlFlow::Continue
        });
    }

    // ── Click: seek / select / razor ────────────────────────────────────────
    let click = GestureClick::new();
    click.set_button(0); // all buttons
    {
        let state = state.clone();
        let area_weak = area.downgrade();
        click.connect_pressed(move |gesture, _n_press, x, y| {
            let button = gesture.current_button();
            let mut st = state.borrow_mut();

            if y < RULER_HEIGHT {
                // Click in ruler → seek
                let ns = st.x_to_ns(x);
                st.playhead_ns = ns;
                let seek_cb = st.on_seek.clone();
                drop(st);
                if let Some(cb) = seek_cb { cb(ns); }
            } else if button == 1 {
                match st.active_tool.clone() {
                    ActiveTool::Razor => {
                        // Razor cut at click position
                        let ns = st.x_to_ns(x);
                        st.playhead_ns = ns;
                        let seek_cb = st.on_seek.clone();
                        st.razor_cut_at_playhead();
                        let proj_cb = st.on_project_changed.clone();
                        drop(st);
                        if let Some(cb) = seek_cb { cb(ns); }
                        if let Some(cb) = proj_cb { cb(); }
                    }
                    ActiveTool::Select => {
                        // Select clip
                        let hit = st.hit_test(x, y);
                        match hit {
                            Some(h) => {
                                st.selected_clip_id = Some(h.clip_id);
                                st.selected_track_id = Some(h.track_id);
                            }
                            None => {
                                st.selected_clip_id = None;
                                st.selected_track_id = None;
                            }
                        }
                        drop(st);
                    }
                }
            } else if button == 3 {
                // Right-click → context actions (for now: delete selected)
                let hit = st.hit_test(x, y);
                if let Some(h) = hit {
                    st.selected_clip_id = Some(h.clip_id);
                    st.selected_track_id = Some(h.track_id);
                }
                drop(st);
                // delete_selected called via keyboard (Delete key)
            } else {
                drop(st);
            }

            if let Some(a) = area_weak.upgrade() { a.queue_draw(); }
        });
    }
    area.add_controller(click);

    // ── Drag: move or trim clips ────────────────────────────────────────────
    let drag = GestureDrag::new();
    {
        let state = state.clone();
        let area_weak = area.downgrade();

        drag.connect_drag_begin({
            let state = state.clone();
            move |_gesture, x, y| {
                let mut st = state.borrow_mut();
                if y < RULER_HEIGHT {
                    // Start ruler scrub immediately on drag-begin.
                    let ns = st.x_to_ns(x);
                    st.playhead_ns = ns;
                    let seek_cb = st.on_seek.clone();
                    drop(st);
                    if let Some(cb) = seek_cb { cb(ns); }
                    return;
                }
                if st.active_tool != ActiveTool::Select { return; }

                let hit = st.hit_test(x, y);
                if let Some(h) = hit {
                    // Extract clip data before mutating st (avoids borrow conflict)
                    let clip_data = {
                        let proj = st.project.borrow();
                        proj.tracks.iter()
                            .flat_map(|t| t.clips.iter())
                            .find(|c| c.id == h.clip_id)
                            .map(|c| (c.timeline_start, c.source_in, c.source_out))
                    };
                    if let Some((tl_start, src_in, src_out)) = clip_data {
                        let offset_ns = st.x_to_ns(x).saturating_sub(tl_start);
                        st.drag_op = match h.zone {
                            HitZone::Body => DragOp::MoveClip {
                                clip_id: h.clip_id.clone(),
                                track_id: h.track_id.clone(),
                                original_start: tl_start,
                                clip_offset_ns: offset_ns,
                            },
                            HitZone::TrimIn => DragOp::TrimIn {
                                clip_id: h.clip_id.clone(),
                                track_id: h.track_id.clone(),
                                original_source_in: src_in,
                                original_timeline_start: tl_start,
                            },
                            HitZone::TrimOut => DragOp::TrimOut {
                                clip_id: h.clip_id.clone(),
                                track_id: h.track_id.clone(),
                                original_source_out: src_out,
                            },
                        };
                        st.selected_clip_id = Some(h.clip_id);
                        st.selected_track_id = Some(h.track_id);
                    }
                }
            }
        });

        drag.connect_drag_update({
            let state = state.clone();
            let area_weak = area_weak.clone();
            move |gesture, offset_x, _offset_y| {
                let (start_x, _) = gesture.start_point().unwrap_or((0.0, 0.0));
                let current_x = start_x + offset_x;
                let (_, start_y) = gesture.start_point().unwrap_or((0.0, 0.0));

                if start_y < RULER_HEIGHT {
                    // Continuous ruler scrubbing while dragging.
                    let mut st = state.borrow_mut();
                    let ns = st.x_to_ns(current_x);
                    st.playhead_ns = ns;
                    let seek_cb = st.on_seek.clone();
                    drop(st);
                    if let Some(cb) = seek_cb { cb(ns); }
                    if let Some(a) = area_weak.upgrade() { a.queue_draw(); }
                    return;
                }

                let current_ns = {
                    let st = state.borrow();
                    st.x_to_ns(current_x)
                };

                let st = state.borrow_mut();
                let drag_op = st.drag_op.clone();
                match drag_op {
                    DragOp::MoveClip { ref clip_id, ref track_id, clip_offset_ns, .. } => {
                        let raw_start = current_ns.saturating_sub(clip_offset_ns);
                        // ── Snap to clip edges ──────────────────────────────
                        let snap_ns = (10.0 / st.pixels_per_second * NS_PER_SECOND) as u64;
                        let (clip_dur, snap_start) = {
                            let proj = st.project.borrow();
                            // Collect all edge times from OTHER clips
                            let edges: Vec<u64> = proj.tracks.iter()
                                .flat_map(|t| t.clips.iter())
                                .filter(|c| &c.id != clip_id)
                                .flat_map(|c| [c.timeline_start, c.timeline_end()])
                                .collect();
                            let this_dur = proj.tracks.iter()
                                .flat_map(|t| t.clips.iter())
                                .find(|c| &c.id == clip_id)
                                .map(|c| c.duration())
                                .unwrap_or(0);
                            // Snap clip start edge
                            let by_start = edges.iter().copied()
                                .filter(|&e| (e as i64 - raw_start as i64).unsigned_abs() < snap_ns)
                                .min_by_key(|&e| (e as i64 - raw_start as i64).unsigned_abs());
                            // Snap clip end edge
                            let by_end = edges.iter().copied()
                                .filter(|&e| {
                                    let end = raw_start + this_dur;
                                    (e as i64 - end as i64).unsigned_abs() < snap_ns
                                })
                                .min_by_key(|&e| {
                                    let end = raw_start + this_dur;
                                    (e as i64 - end as i64).unsigned_abs()
                                })
                                .map(|e| e.saturating_sub(this_dur));
                            let snapped = match (by_start, by_end) {
                                (Some(a), Some(b)) => {
                                    let da = (a as i64 - raw_start as i64).unsigned_abs();
                                    let db = (b as i64 - raw_start as i64).unsigned_abs();
                                    if da <= db { a } else { b }
                                }
                                (Some(a), None) => a,
                                (None, Some(b)) => b,
                                (None, None) => raw_start,
                            };
                            (this_dur, snapped)
                        };
                        let _ = clip_dur; // used above
                        let mut proj = st.project.borrow_mut();
                        if let Some(track) = proj.tracks.iter_mut().find(|t| &t.id == track_id) {
                            if let Some(clip) = track.clips.iter_mut().find(|c| &c.id == clip_id) {
                                clip.timeline_start = snap_start;
                            }
                        }
                    }
                    DragOp::TrimIn { ref clip_id, ref track_id, original_source_in, original_timeline_start } => {
                        let drag_ns = current_ns.saturating_sub(original_timeline_start);
                        // Snap the new timeline_start to nearby clip edges
                        let snap_ns = (10.0 / st.pixels_per_second * NS_PER_SECOND) as u64;
                        let new_start_raw = original_timeline_start + drag_ns;
                        let snapped_start = {
                            let proj = st.project.borrow();
                            let edges: Vec<u64> = proj.tracks.iter()
                                .flat_map(|t| t.clips.iter())
                                .filter(|c| &c.id != clip_id)
                                .flat_map(|c| [c.timeline_start, c.timeline_end()])
                                .collect();
                            edges.iter().copied()
                                .filter(|&e| (e as i64 - new_start_raw as i64).unsigned_abs() < snap_ns)
                                .min_by_key(|&e| (e as i64 - new_start_raw as i64).unsigned_abs())
                                .unwrap_or(new_start_raw)
                        };
                        let snapped_drag = snapped_start.saturating_sub(original_timeline_start);
                        let mut proj = st.project.borrow_mut();
                        if let Some(track) = proj.tracks.iter_mut().find(|t| &t.id == track_id) {
                            if let Some(clip) = track.clips.iter_mut().find(|c| &c.id == clip_id) {
                                let new_source_in = original_source_in + snapped_drag;
                                if new_source_in < clip.source_out.saturating_sub(1_000_000) {
                                    clip.source_in = new_source_in;
                                    clip.timeline_start = original_timeline_start + snapped_drag;
                                }
                            }
                        }
                    }
                    DragOp::TrimOut { ref clip_id, ref track_id, .. } => {
                        // Snap the out-point to nearby clip edges
                        let snap_ns = (10.0 / st.pixels_per_second * NS_PER_SECOND) as u64;
                        let snapped_ns = {
                            let proj = st.project.borrow();
                            let edges: Vec<u64> = proj.tracks.iter()
                                .flat_map(|t| t.clips.iter())
                                .filter(|c| &c.id != clip_id)
                                .flat_map(|c| [c.timeline_start, c.timeline_end()])
                                .collect();
                            edges.iter().copied()
                                .filter(|&e| (e as i64 - current_ns as i64).unsigned_abs() < snap_ns)
                                .min_by_key(|&e| (e as i64 - current_ns as i64).unsigned_abs())
                                .unwrap_or(current_ns)
                        };
                        let mut proj = st.project.borrow_mut();
                        if let Some(track) = proj.tracks.iter_mut().find(|t| &t.id == track_id) {
                            if let Some(clip) = track.clips.iter_mut().find(|c| &c.id == clip_id) {
                                if snapped_ns > clip.source_in + 1_000_000 {
                                    let offset = snapped_ns.saturating_sub(clip.timeline_start);
                                    clip.source_out = clip.source_in + offset;
                                }
                            }
                        }
                    }
                    DragOp::None => {}
                }

                if let Some(a) = area_weak.upgrade() { a.queue_draw(); }
            }
        });

        drag.connect_drag_end({
            let state = state.clone();
            move |_, _, _| {
                let mut st = state.borrow_mut();
                let drag_op = std::mem::replace(&mut st.drag_op, DragOp::None);

                // Commit drag to undo history
                match drag_op {
                    DragOp::MoveClip { ref clip_id, ref track_id, original_start, .. } => {
                        let new_start = {
                            let proj = st.project.borrow();
                            proj.tracks.iter()
                                .find(|t| &t.id == track_id)
                                .and_then(|t| t.clips.iter().find(|c| &c.id == clip_id))
                                .map(|c| c.timeline_start)
                        };
                        if let Some(new_start) = new_start {
                            if new_start != original_start {
                                let cmd = MoveClipCommand {
                                    clip_id: clip_id.clone(),
                                    from_track_id: track_id.clone(),
                                    to_track_id: track_id.clone(),
                                    old_timeline_start: original_start,
                                    new_timeline_start: new_start,
                                };
                                // Don't re-execute (already applied live), just push to history
                                st.history.undo_stack.push(Box::new(cmd));
                                st.history.redo_stack.clear();
                                st.project.borrow_mut().dirty = true;
                            }
                        }
                    }
                    DragOp::TrimIn { ref clip_id, ref track_id, original_source_in, original_timeline_start } => {
                        let (new_si, new_ts) = {
                            let proj = st.project.borrow();
                            proj.tracks.iter()
                                .find(|t| &t.id == track_id)
                                .and_then(|t| t.clips.iter().find(|c| &c.id == clip_id))
                                .map(|c| (c.source_in, c.timeline_start))
                                .unwrap_or((original_source_in, original_timeline_start))
                        };
                        if new_si != original_source_in {
                            let cmd = TrimClipCommand {
                                clip_id: clip_id.clone(),
                                track_id: track_id.clone(),
                                old_source_in: original_source_in,
                                new_source_in: new_si,
                                old_timeline_start: original_timeline_start,
                                new_timeline_start: new_ts,
                            };
                            st.history.undo_stack.push(Box::new(cmd));
                            st.history.redo_stack.clear();
                            st.project.borrow_mut().dirty = true;
                        }
                    }
                    DragOp::TrimOut { ref clip_id, ref track_id, original_source_out } => {
                        let new_so = {
                            let proj = st.project.borrow();
                            proj.tracks.iter()
                                .find(|t| &t.id == track_id)
                                .and_then(|t| t.clips.iter().find(|c| &c.id == clip_id))
                                .map(|c| c.source_out)
                                .unwrap_or(original_source_out)
                        };
                        if new_so != original_source_out {
                            let cmd = TrimOutCommand {
                                clip_id: clip_id.clone(),
                                track_id: track_id.clone(),
                                old_source_out: original_source_out,
                                new_source_out: new_so,
                            };
                            st.history.undo_stack.push(Box::new(cmd));
                            st.history.redo_stack.clear();
                            st.project.borrow_mut().dirty = true;
                        }
                    }
                    DragOp::None => {}
                }

                let proj_cb = st.on_project_changed.clone();
                drop(st);
                if let Some(cb) = proj_cb { cb(); }
            }
        });
    }
    area.add_controller(drag);

    // ── Keyboard shortcuts ──────────────────────────────────────────────────
    let key_ctrl = EventControllerKey::new();
    {
        let state = state.clone();
        let area_weak = area.downgrade();
        key_ctrl.connect_key_pressed(move |_, key, _, modifiers| {
            use gtk::gdk::Key;
            let ctrl = modifiers.contains(gtk::gdk::ModifierType::CONTROL_MASK);
            let shift = modifiers.contains(gtk::gdk::ModifierType::SHIFT_MASK);
            let mut st = state.borrow_mut();

            // Track whether we need to fire on_project_changed after releasing the borrow
            let mut notify_project = false;

            let handled = match key {
                Key::z if ctrl && !shift => { st.undo(); notify_project = true; true }
                Key::z if ctrl && shift  => { st.redo(); notify_project = true; true }
                Key::y if ctrl           => { st.redo(); notify_project = true; true }
                Key::Delete | Key::BackSpace => { st.delete_selected(); notify_project = true; true }
                Key::space => {
                    let pp_cb = st.on_play_pause.clone();
                    drop(st);
                    if let Some(cb) = pp_cb { cb(); }
                    if let Some(a) = area_weak.upgrade() { a.queue_draw(); }
                    return glib::Propagation::Stop;
                }
                Key::b | Key::B => {
                    // B = Blade/Razor
                    st.active_tool = if st.active_tool == ActiveTool::Razor {
                        ActiveTool::Select
                    } else {
                        ActiveTool::Razor
                    };
                    true
                }
                Key::Escape => {
                    st.active_tool = ActiveTool::Select;
                    true
                }
                Key::question | Key::slash => {
                    // Show keyboard shortcut reference
                    drop(st);
                    if let Some(a) = area_weak.upgrade() {
                        if let Some(win) = a.root().and_then(|r| r.downcast::<gtk::Window>().ok()) {
                            show_shortcuts_dialog(&win);
                        }
                    }
                    return glib::Propagation::Stop;
                }
                _ => false,
            };

            let proj_cb = if notify_project { st.on_project_changed.clone() } else { None };
            if handled {
                if let Some(a) = area_weak.upgrade() { a.queue_draw(); }
            }
            drop(st);
            if let Some(cb) = proj_cb { cb(); }

            if handled {
                glib::Propagation::Stop
            } else {
                glib::Propagation::Proceed
            }
        });
    }
    area.add_controller(key_ctrl);

    // ── Scroll wheel: zoom ──────────────────────────────────────────────────
    let scroll = EventControllerScroll::new(gtk::EventControllerScrollFlags::BOTH_AXES);
    {
        let state = state.clone();
        let area_weak = area.downgrade();
        scroll.connect_scroll(move |ctrl, dx, dy| {
            let mut st = state.borrow_mut();
            let ctrl_held = ctrl.current_event_state()
                .contains(gtk::gdk::ModifierType::CONTROL_MASK);
            // Ctrl+scroll OR pure vertical scroll = zoom
            // Horizontal scroll (dx dominant, e.g. Shift+scroll or trackpad) = pan
            if dx.abs() > dy.abs() {
                // Horizontal pan
                st.scroll_offset = (st.scroll_offset + dx * 20.0).max(0.0);
            } else if ctrl_held || dy.abs() > 0.0 {
                // Zoom (Ctrl+scroll or plain vertical scroll)
                let factor = if dy < 0.0 { 1.1 } else { 0.9 };
                st.pixels_per_second = (st.pixels_per_second * factor).clamp(10.0, 2000.0);
            }
            if let Some(a) = area_weak.upgrade() { a.queue_draw(); }
            glib::Propagation::Stop
        });
    }
    area.add_controller(scroll);

    // ── Drop target: accept clips dragged from media browser ────────────────
    {
        use gtk4::DropTarget;
        let drop = DropTarget::new(glib::Type::STRING, gdk4::DragAction::COPY);
        let state = state.clone();
        let area_weak = area.downgrade();
        drop.connect_drop(move |_target, value, x, y| {
            let payload = match value.get::<String>() {
                Ok(s) => s,
                Err(_) => return false,
            };
            // Payload format: "{source_path}|{duration_ns}"
            let mut parts = payload.splitn(2, '|');
            let source_path = match parts.next() { Some(p) => p.to_string(), None => return false };
            let duration_ns: u64 = parts.next().and_then(|s| s.parse().ok()).unwrap_or(0);

            let (track_idx, timeline_start_ns) = {
                let st = state.borrow();
                let track_row_idx = if y > RULER_HEIGHT {
                    ((y - RULER_HEIGHT) / TRACK_HEIGHT) as usize
                } else { 0 };
                let tns = st.x_to_ns(x);
                (track_row_idx, tns)
            };

            let cb = state.borrow().on_drop_clip.clone();
            if let Some(cb) = cb {
                cb(source_path, duration_ns, track_idx, timeline_start_ns);
            }
            if let Some(a) = area_weak.upgrade() { a.queue_draw(); }
            true
        });
        area.add_controller(drop);
    }

    area
}

/// Cairo drawing of the entire timeline
fn draw_timeline(
    cr: &gtk::cairo::Context,
    width: i32,
    height: i32,
    st: &TimelineState,
    cache: &mut crate::media::thumb_cache::ThumbnailCache,
    wcache: &mut crate::media::waveform_cache::WaveformCache,
) {
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
        draw_track_row(cr, w, y, track, st, cache, wcache);
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

    // Tool indicator
    if st.active_tool == ActiveTool::Razor {
        cr.set_source_rgb(1.0, 0.8, 0.0);
        cr.set_font_size(12.0);
        let _ = cr.move_to(TRACK_LABEL_WIDTH + 8.0, RULER_HEIGHT + 16.0);
        let _ = cr.show_text("✂ Razor (B to toggle)");
    }
}

fn draw_ruler(cr: &gtk::cairo::Context, width: f64, st: &TimelineState) {
    cr.set_source_rgb(0.2, 0.2, 0.22);
    cr.rectangle(0.0, 0.0, width, RULER_HEIGHT);
    cr.fill().ok();

    cr.set_source_rgb(0.6, 0.6, 0.6);
    cr.set_line_width(1.0);
    cr.set_font_size(10.0);

    let visible_secs = (width - TRACK_LABEL_WIDTH) / st.pixels_per_second;
    let start_sec = st.scroll_offset / st.pixels_per_second;
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
    cache: &mut crate::media::thumb_cache::ThumbnailCache,
    wcache: &mut crate::media::waveform_cache::WaveformCache,
) {
    let (r, g, b) = match track.kind {
        TrackKind::Video => (0.16, 0.16, 0.18),
        TrackKind::Audio => (0.14, 0.16, 0.18),
    };
    cr.set_source_rgb(r, g, b);
    cr.rectangle(TRACK_LABEL_WIDTH, y, width - TRACK_LABEL_WIDTH, TRACK_HEIGHT);
    cr.fill().ok();

    cr.set_source_rgb(0.22, 0.22, 0.25);
    cr.rectangle(0.0, y, TRACK_LABEL_WIDTH, TRACK_HEIGHT);
    cr.fill().ok();

    cr.set_source_rgb(0.8, 0.8, 0.8);
    cr.set_font_size(11.0);
    let _ = cr.move_to(6.0, y + TRACK_HEIGHT / 2.0 + 4.0);
    let _ = cr.show_text(&track.label);

    cr.set_source_rgb(0.1, 0.1, 0.12);
    cr.set_line_width(1.0);
    cr.move_to(0.0, y + TRACK_HEIGHT);
    cr.line_to(width, y + TRACK_HEIGHT);
    cr.stroke().ok();

    for clip in &track.clips {
        draw_clip(cr, y, clip, track, st, cache, wcache);
    }
}

fn draw_clip(
    cr: &gtk::cairo::Context,
    track_y: f64,
    clip: &crate::model::clip::Clip,
    track: &crate::model::track::Track,
    st: &TimelineState,
    cache: &mut crate::media::thumb_cache::ThumbnailCache,
    wcache: &mut crate::media::waveform_cache::WaveformCache,
) {
    let cx = st.ns_to_x(clip.timeline_start);
    let cw = (clip.duration() as f64 / NS_PER_SECOND) * st.pixels_per_second;
    let cy = track_y + 2.0;
    let ch = TRACK_HEIGHT - 4.0;

    if cx + cw < TRACK_LABEL_WIDTH || cx > 4000.0 { return; }

    let is_selected = st.selected_clip_id.as_deref() == Some(&clip.id);

    let (r, g, b) = match track.kind {
        TrackKind::Video => (0.17, 0.47, 0.85),
        TrackKind::Audio => (0.18, 0.65, 0.45),
    };
    cr.set_source_rgb(r, g, b);
    rounded_rect(cr, cx, cy, cw.max(4.0), ch, 4.0);
    cr.fill().ok();

    // ── Thumbnail strip for video clips ──────────────────────────────────
    if track.kind == TrackKind::Video && cw > 20.0 {
        // Request thumb at the clip's source in-point
        cache.request(&clip.source_path, clip.source_in);
        if let Some(surf) = cache.get(&clip.source_path, clip.source_in) {
            cr.save().ok();
            // Clip to the clip rectangle (with 1px inset)
            rounded_rect(cr, cx + 1.0, cy + 1.0, cw - 2.0, ch - 2.0, 3.0);
            cr.clip();

            let thumb_aspect = 160.0_f64 / 90.0_f64;
            let tile_h = ch - 2.0;
            let tile_w = tile_h * thumb_aspect;
            let scale = tile_h / 90.0;

            // Tile thumbnails across the clip width
            let mut tile_x = cx + 1.0;
            while tile_x < cx + cw - 1.0 {
                cr.save().ok();
                cr.translate(tile_x, cy + 1.0);
                cr.scale(scale, scale);
                cr.set_source_surface(surf, 0.0, 0.0).ok();
                cr.paint_with_alpha(0.75).ok();
                cr.restore().ok();
                tile_x += tile_w;
            }
            cr.restore().ok();

            // Re-draw the clip colour as a semi-transparent overlay so the
            // label remains readable on top of the thumbnail.
            cr.set_source_rgba(r, g, b, 0.35);
            rounded_rect(cr, cx, cy, cw.max(4.0), ch, 4.0);
            cr.fill().ok();
        }
    }

    // ── Waveform for audio clips ───────────────────────────────────────────
    if track.kind == TrackKind::Audio && cw > 8.0 {
        wcache.request(&clip.source_path);
        let px_count = cw as usize;
        if let Some(peaks) = wcache.get_peaks(&clip.source_path, clip.source_in, clip.source_out, px_count) {
            cr.save().ok();
            rounded_rect(cr, cx + 1.0, cy + 1.0, cw - 2.0, ch - 2.0, 3.0);
            cr.clip();

            let mid = cy + ch / 2.0;
            cr.set_source_rgba(1.0, 1.0, 1.0, 0.6);
            cr.set_line_width(1.0);

            for (i, &peak) in peaks.iter().enumerate() {
                let px = cx + i as f64;
                let half_h = (peak as f64 * (ch / 2.0 - 2.0)).max(1.0);
                cr.move_to(px + 0.5, mid - half_h);
                cr.line_to(px + 0.5, mid + half_h);
            }
            cr.stroke().ok();
            cr.restore().ok();
        }
    }

    if is_selected {
        cr.set_source_rgb(1.0, 0.85, 0.0);
        cr.set_line_width(2.0);
        rounded_rect(cr, cx, cy, cw.max(4.0), ch, 4.0);
        cr.stroke().ok();

        // Draw trim handles (lighter shaded edges)
        cr.set_source_rgba(1.0, 1.0, 1.0, 0.3);
        cr.rectangle(cx, cy, TRIM_HANDLE_PX, ch);
        cr.fill().ok();
        cr.rectangle(cx + cw - TRIM_HANDLE_PX, cy, TRIM_HANDLE_PX, ch);
        cr.fill().ok();
    }

    if cw > 30.0 {
        cr.set_source_rgb(1.0, 1.0, 1.0);
        cr.set_font_size(11.0);
        let _ = cr.move_to(cx + 6.0, cy + ch / 2.0 + 4.0);
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
    let target_px = 80.0;
    let raw = target_px / pixels_per_second;
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
    if h > 0 { format!("{h}:{m:02}:{s:02}") } else { format!("{m}:{s:02}") }
}


pub fn show_shortcuts_dialog(parent: &gtk::Window) {
    use gtk4::{Dialog, Label, Orientation, ResponseType, ScrolledWindow};
    let dialog = Dialog::builder()
        .title("Keyboard Shortcuts")
        .transient_for(parent)
        .modal(true)
        .default_width(480)
        .default_height(500)
        .build();
    dialog.add_button("Close", ResponseType::Close);

    let shortcuts: &[(&str, &str)] = &[
        ("Space",          "Play / Pause"),
        ("I",              "Set In-point"),
        ("O",              "Set Out-point"),
        ("J",              "Shuttle reverse (1×/2×/4×)"),
        ("K",              "Stop shuttle / Pause"),
        ("L",              "Shuttle forward (1×/2×/4×)"),
        ("← / →",          "Step one frame back / forward"),
        ("B",              "Toggle Razor (Blade) tool"),
        ("Escape",         "Switch to Select tool"),
        ("Delete / Bksp",  "Delete selected clip"),
        ("Ctrl+Z",         "Undo"),
        ("Ctrl+Y / Ctrl+Shift+Z", "Redo"),
        ("Scroll",         "Zoom timeline (vertical scroll)"),
        ("Scroll (H)",     "Pan timeline (horizontal scroll)"),
        ("? / /",          "Show this help"),
    ];

    let vbox = gtk4::Box::new(Orientation::Vertical, 12);
    vbox.set_margin_start(20);
    vbox.set_margin_end(20);
    vbox.set_margin_top(16);
    vbox.set_margin_bottom(16);

    for (key, desc) in shortcuts {
        let row = gtk4::Box::new(Orientation::Horizontal, 0);
        row.set_spacing(12);

        let key_lbl = Label::new(Some(key));
        key_lbl.set_width_chars(28);
        key_lbl.set_xalign(1.0);
        key_lbl.add_css_class("shortcut-key");

        let desc_lbl = Label::new(Some(desc));
        desc_lbl.set_xalign(0.0);

        row.append(&key_lbl);
        row.append(&desc_lbl);
        vbox.append(&row);
    }

    let scroll = ScrolledWindow::new();
    scroll.set_child(Some(&vbox));
    scroll.set_vexpand(true);
    dialog.content_area().append(&scroll);

    dialog.connect_response(|d, _| d.destroy());
    dialog.present();
}
