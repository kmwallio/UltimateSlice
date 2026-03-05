use crate::model::clip::{Clip, ClipKind};
use crate::model::project::Project;
use crate::model::track::TrackKind;
use crate::undo::{
    EditHistory, MoveClipCommand, ReorderTrackCommand, SetTrackClipsCommand, SplitClipCommand,
    TrimClipCommand, TrimOutCommand,
};
use glib;
use gtk4::prelude::*;
use gtk4::{
    self as gtk, DrawingArea, EventControllerKey, EventControllerScroll, GestureClick, GestureDrag,
};
use std::cell::RefCell;
use std::collections::HashSet;
use std::rc::Rc;

const TRACK_HEIGHT: f64 = 60.0;
const TRACK_LABEL_WIDTH: f64 = 110.0;
const TRACK_LABEL_METER_WIDTH: f64 = 18.0;
const RULER_HEIGHT: f64 = 24.0;
const PIXELS_PER_SECOND_DEFAULT: f64 = 100.0;
const NS_PER_SECOND: f64 = 1_000_000_000.0;
/// Pixels from clip edge that activate trim mode
const TRIM_HANDLE_PX: f64 = 10.0;

#[derive(Debug, Clone, PartialEq)]
pub enum ActiveTool {
    Select,
    Razor,
    Ripple,
    Roll,
    Slip,
    Slide,
}

/// What a drag gesture is currently doing
#[derive(Debug, Clone)]
enum DragOp {
    None,
    /// Moving a clip (possibly across tracks of the same kind)
    MoveClip {
        clip_id: String,
        original_track_id: String,
        current_track_id: String,
        original_start: u64,
        clip_offset_ns: u64,
        original_track_clips: Vec<Clip>,
        move_clip_ids: Vec<String>,
        original_member_starts: Vec<(String, u64)>,
        original_tracks: Vec<(String, Vec<Clip>)>,
    },
    /// Trimming the in-point of a clip
    TrimIn {
        clip_id: String,
        track_id: String,
        original_source_in: u64,
        original_timeline_start: u64,
        original_track_clips: Vec<Clip>,
    },
    /// Trimming the out-point of a clip
    TrimOut {
        clip_id: String,
        track_id: String,
        original_source_out: u64,
        original_track_clips: Vec<Clip>,
    },
    /// Roll edit between two clips
    Roll {
        left_clip_id: String,
        right_clip_id: String,
        track_id: String,
        original_left_out: u64,
        original_right_in: u64,
        original_right_start: u64,
    },
    /// Slip edit: shift source window without moving clip on timeline
    Slip {
        clip_id: String,
        track_id: String,
        original_source_in: u64,
        original_source_out: u64,
        drag_start_ns: u64,
    },
    /// Slide edit: move clip on timeline, adjusting neighbor edit points
    Slide {
        clip_id: String,
        track_id: String,
        original_start: u64,
        drag_start_ns: u64,
        left_clip_id: Option<String>,
        original_left_out: Option<u64>,
        right_clip_id: Option<String>,
        original_right_in: Option<u64>,
        original_right_start: Option<u64>,
    },
    /// Reordering a track by dragging its label
    ReorderTrack {
        track_idx: usize,
        target_idx: usize,
    },
}

#[derive(Debug, Clone)]
struct TimelineClipboard {
    clip: Clip,
    source_track_id: String,
}

#[derive(Debug, Clone)]
struct MarqueeSelection {
    start_x: f64,
    start_y: f64,
    current_x: f64,
    current_y: f64,
    additive: bool,
    base_ids: HashSet<String>,
    base_primary: Option<String>,
    base_track_id: Option<String>,
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
    /// Scroll offset at the start of a ruler pan-drag
    ruler_pan_start_offset: f64,
    /// Callback fired when user seeks — use Rc so it can be cloned out before releasing the RefMut
    pub on_seek: Option<Rc<dyn Fn(u64)>>,
    /// Callback fired when project changes — use Rc so it can be cloned out before releasing the RefMut
    pub on_project_changed: Option<Rc<dyn Fn()>>,
    /// Callback fired when the user presses Space to toggle play/pause
    pub on_play_pause: Option<Rc<dyn Fn()>>,
    /// Callback to pause/resume background thumbnail+waveform extraction during playback.
    /// Called with `true` when playback starts, `false` when it stops.
    pub on_extraction_pause: Option<Rc<dyn Fn(bool)>>,
    /// Called when a clip is dropped from the media browser: (source_path, duration_ns, track_idx, timeline_start_ns)
    pub on_drop_clip: Option<Rc<dyn Fn(String, u64, usize, u64)>>,
    /// Lightweight callback fired immediately when clip selection changes (no pipeline rebuild).
    /// Called with the new selected_clip_id (or None if deselected).
    pub on_clip_selected: Option<Rc<dyn Fn(Option<String>)>>,
    /// Gap-free timeline behavior toggle (track-local ripple).
    pub magnetic_mode: bool,
    /// Hover preview while dragging a transition: (left_clip_id, right_clip_id).
    hover_transition_pair: Option<(String, String)>,
    /// Show audio waveforms overlaid on video clips in the timeline.
    pub show_waveform_on_video: bool,
    /// Show thumbnail preview strips on timeline video clips.
    pub show_timeline_preview: bool,
    /// When true, the timeline is loading a project and interaction is suppressed.
    pub loading: bool,
    /// Per-track stereo audio peaks (dBFS) keyed by track index.
    pub track_audio_peak_db: Vec<[f64; 2]>,
    /// Show/hide per-track audio meters in track labels.
    pub show_track_audio_levels: bool,
    /// Single-clip timeline clipboard payload for copy/paste operations.
    clipboard: Option<TimelineClipboard>,
    /// Multi-selection set (primary selection remains in `selected_clip_id`).
    selected_clip_ids: HashSet<String>,
    /// Anchor clip used for Shift+click range selection.
    selection_anchor_clip_id: Option<String>,
    /// Active marquee-selection drag state.
    marquee_selection: Option<MarqueeSelection>,
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
            ruler_pan_start_offset: 0.0,
            on_seek: None,
            on_project_changed: None,
            on_play_pause: None,
            on_extraction_pause: None,
            on_drop_clip: None,
            on_clip_selected: None,
            magnetic_mode: false,
            hover_transition_pair: None,
            show_waveform_on_video: false,
            show_timeline_preview: true,
            loading: false,
            track_audio_peak_db: Vec::new(),
            show_track_audio_levels: true,
            clipboard: None,
            selected_clip_ids: HashSet::new(),
            selection_anchor_clip_id: None,
            marquee_selection: None,
        }
    }

    pub fn ns_to_x(&self, ns: u64) -> f64 {
        TRACK_LABEL_WIDTH + (ns as f64 / NS_PER_SECOND) * self.pixels_per_second
            - self.scroll_offset
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
        self.delete_selected_internal(self.magnetic_mode, if self.magnetic_mode {
            "Delete clip(s) (magnetic)"
        } else {
            "Delete clip(s)"
        });
    }

    pub fn ripple_delete_selected(&mut self) {
        self.delete_selected_internal(true, "Ripple delete");
    }

    fn delete_selected_internal(&mut self, compact: bool, label: &str) {
        let Some(_primary_clip_id) = self.selected_clip_id.clone() else {
            return;
        };
        let mut target_ids = self.selected_ids_or_primary();
        target_ids = self.expand_with_group_members(&target_ids);
        if target_ids.is_empty() {
            return;
        }
        let track_updates = {
            let proj = self.project.borrow();
            proj.tracks
                .iter()
                .filter_map(|t| {
                    let old_clips = t.clips.clone();
                    let mut new_clips: Vec<Clip> = old_clips
                        .iter()
                        .filter(|c| !target_ids.contains(&c.id))
                        .cloned()
                        .collect();
                    if compact {
                        compact_gap_free_clips(&mut new_clips);
                    }
                    if new_clips != old_clips {
                        Some((t.id.clone(), old_clips, new_clips))
                    } else {
                        None
                    }
                })
                .collect::<Vec<_>>()
        };
        if !track_updates.is_empty() {
            let mut proj = self.project.borrow_mut();
            for (track_id, old_clips, new_clips) in track_updates {
                let cmd = SetTrackClipsCommand {
                    track_id,
                    old_clips,
                    new_clips,
                    label: label.to_string(),
                };
                self.history.execute(Box::new(cmd), &mut proj);
            }
        }
        target_ids.clear();
        self.clear_clip_selection();
    }

    pub fn group_selected_clips(&mut self) -> bool {
        let target_ids = self.selected_ids_or_primary();
        if target_ids.len() < 2 {
            return false;
        }
        let group_id = uuid::Uuid::new_v4().to_string();
        let track_updates = {
            let proj = self.project.borrow();
            proj.tracks
                .iter()
                .filter_map(|t| {
                    let old_clips = t.clips.clone();
                    let mut new_clips = old_clips.clone();
                    let mut changed = false;
                    for clip in &mut new_clips {
                        if target_ids.contains(&clip.id) && clip.group_id.as_deref() != Some(&group_id) {
                            clip.group_id = Some(group_id.clone());
                            changed = true;
                        }
                    }
                    if changed {
                        Some((t.id.clone(), old_clips, new_clips))
                    } else {
                        None
                    }
                })
                .collect::<Vec<_>>()
        };
        if track_updates.is_empty() {
            return false;
        }
        let mut proj = self.project.borrow_mut();
        for (track_id, old_clips, new_clips) in track_updates {
            let cmd = SetTrackClipsCommand {
                track_id,
                old_clips,
                new_clips,
                label: "Group clips".to_string(),
            };
            self.history.execute(Box::new(cmd), &mut proj);
        }
        true
    }

    pub fn ungroup_selected_clips(&mut self) -> bool {
        let target_ids = self.selected_ids_or_primary();
        if target_ids.is_empty() {
            return false;
        }
        let target_groups: HashSet<String> = {
            let proj = self.project.borrow();
            proj.tracks
                .iter()
                .flat_map(|t| t.clips.iter())
                .filter(|c| target_ids.contains(&c.id))
                .filter_map(|c| c.group_id.clone())
                .collect()
        };
        if target_groups.is_empty() {
            return false;
        }
        let track_updates = {
            let proj = self.project.borrow();
            proj.tracks
                .iter()
                .filter_map(|t| {
                    let old_clips = t.clips.clone();
                    let mut new_clips = old_clips.clone();
                    let mut changed = false;
                    for clip in &mut new_clips {
                        if clip
                            .group_id
                            .as_deref()
                            .is_some_and(|gid| target_groups.contains(gid))
                        {
                            clip.group_id = None;
                            changed = true;
                        }
                    }
                    if changed {
                        Some((t.id.clone(), old_clips, new_clips))
                    } else {
                        None
                    }
                })
                .collect::<Vec<_>>()
        };
        if track_updates.is_empty() {
            return false;
        }
        let mut proj = self.project.borrow_mut();
        for (track_id, old_clips, new_clips) in track_updates {
            let cmd = SetTrackClipsCommand {
                track_id,
                old_clips,
                new_clips,
                label: "Ungroup clips".to_string(),
            };
            self.history.execute(Box::new(cmd), &mut proj);
        }
        true
    }

    pub fn copy_selected_to_clipboard(&mut self) -> bool {
        let Some(clip_id) = self.selected_clip_id.clone() else {
            return false;
        };
        let copied = {
            let proj = self.project.borrow();
            proj.tracks.iter().find_map(|track| {
                track
                    .clips
                    .iter()
                    .find(|c| c.id == clip_id)
                    .cloned()
                    .map(|clip| TimelineClipboard {
                        clip,
                        source_track_id: track.id.clone(),
                    })
            })
        };
        if let Some(payload) = copied {
            self.clipboard = Some(payload);
            true
        } else {
            false
        }
    }

    pub fn paste_insert_from_clipboard(&mut self) -> bool {
        let Some(payload) = self.clipboard.clone() else {
            return false;
        };
        let clip_duration = payload.clip.duration();
        if clip_duration == 0 {
            return false;
        }
        let target_kind = clip_kind_to_track_kind(&payload.clip.kind);
        let target_track_id = {
            let proj = self.project.borrow();
            if let Some(ref selected_tid) = self.selected_track_id {
                if proj
                    .tracks
                    .iter()
                    .any(|t| t.id == *selected_tid && t.kind == target_kind)
                {
                    Some(selected_tid.clone())
                } else {
                    None
                }
            } else {
                None
            }
            .or_else(|| {
                proj.tracks
                    .iter()
                    .find(|t| t.id == payload.source_track_id && t.kind == target_kind)
                    .map(|t| t.id.clone())
            })
            .or_else(|| {
                proj.tracks
                    .iter()
                    .find(|t| t.kind == target_kind)
                    .map(|t| t.id.clone())
            })
        };
        let Some(target_track_id) = target_track_id else {
            return false;
        };

        let playhead = self.playhead_ns;
        let mut pasted = payload.clip.clone();
        pasted.id = uuid::Uuid::new_v4().to_string();
        pasted.timeline_start = playhead;
        let pasted_id = pasted.id.clone();

        let (old_clips, new_clips) = {
            let proj = self.project.borrow();
            let Some(track) = proj.tracks.iter().find(|t| t.id == target_track_id) else {
                return false;
            };
            let old_clips = track.clips.clone();
            let mut new_clips = old_clips.clone();
            for clip in &mut new_clips {
                if clip.timeline_start >= playhead {
                    clip.timeline_start += clip_duration;
                }
            }
            new_clips.push(pasted);
            new_clips.sort_by_key(|c| c.timeline_start);
            if self.magnetic_mode {
                compact_gap_free_clips(&mut new_clips);
            }
            (old_clips, new_clips)
        };
        if old_clips == new_clips {
            return false;
        }
        let cmd = SetTrackClipsCommand {
            track_id: target_track_id.clone(),
            old_clips,
            new_clips,
            label: "Paste insert clip".to_string(),
        };
        let mut proj = self.project.borrow_mut();
        self.history.execute(Box::new(cmd), &mut proj);
        drop(proj);
        self.set_single_clip_selection(pasted_id, target_track_id);
        true
    }

    pub fn paste_attributes_from_clipboard(&mut self) -> bool {
        let Some(payload) = self.clipboard.clone() else {
            return false;
        };
        let Some(selected_clip_id) = self.selected_clip_id.clone() else {
            return false;
        };

        let (track_id, old_clips, mut new_clips) = {
            let proj = self.project.borrow();
            let Some(track) = proj
                .tracks
                .iter()
                .find(|t| t.clips.iter().any(|c| c.id == selected_clip_id))
            else {
                return false;
            };
            let Some(target_idx) = track.clips.iter().position(|c| c.id == selected_clip_id) else {
                return false;
            };
            if track.clips[target_idx].kind != payload.clip.kind {
                return false;
            }
            let old_clips = track.clips.clone();
            let mut new_clips = old_clips.clone();
            if !apply_pasted_attributes(&mut new_clips[target_idx], &payload.clip) {
                return false;
            }
            (track.id.clone(), old_clips, new_clips)
        };
        if old_clips == new_clips {
            return false;
        }
        let cmd = SetTrackClipsCommand {
            track_id,
            old_clips,
            new_clips: std::mem::take(&mut new_clips),
            label: "Paste clip attributes".to_string(),
        };
        let mut proj = self.project.borrow_mut();
        self.history.execute(Box::new(cmd), &mut proj);
        true
    }

    fn clear_clip_selection(&mut self) {
        self.selected_clip_id = None;
        self.selected_clip_ids.clear();
        self.selection_anchor_clip_id = None;
    }

    fn set_single_clip_selection(&mut self, clip_id: String, track_id: String) {
        let mut ids = HashSet::new();
        ids.insert(clip_id.clone());
        self.set_selection_with_primary(clip_id, track_id, ids);
    }

    fn set_selection_with_primary(
        &mut self,
        primary_clip_id: String,
        track_id: String,
        mut selected_ids: HashSet<String>,
    ) {
        selected_ids.insert(primary_clip_id.clone());
        self.selected_clip_ids = selected_ids;
        self.selected_clip_id = Some(primary_clip_id.clone());
        self.selection_anchor_clip_id = Some(primary_clip_id);
        self.selected_track_id = Some(track_id);
    }

    fn is_clip_selected(&self, clip_id: &str) -> bool {
        if self.selected_clip_id.is_none() {
            return false;
        }
        if self.selected_clip_ids.is_empty() {
            self.selected_clip_id.as_deref() == Some(clip_id)
        } else {
            self.selected_clip_ids.contains(clip_id)
        }
    }

    fn select_all_clips(&mut self) -> bool {
        let mut all_ids = Vec::new();
        let mut primary: Option<(String, String)> = None;
        {
            let proj = self.project.borrow();
            for track in &proj.tracks {
                for clip in &track.clips {
                    if primary.is_none() {
                        primary = Some((clip.id.clone(), track.id.clone()));
                    }
                    all_ids.push(clip.id.clone());
                }
            }
        }
        let Some((primary_id, primary_track_id)) = primary else {
            return false;
        };
        self.selected_clip_ids = all_ids.into_iter().collect();
        self.selected_clip_id = Some(primary_id.clone());
        self.selected_track_id = Some(primary_track_id);
        self.selection_anchor_clip_id = Some(primary_id);
        true
    }

    fn selected_ids_or_primary(&self) -> HashSet<String> {
        if !self.selected_clip_ids.is_empty() {
            return self.selected_clip_ids.clone();
        }
        let mut ids = HashSet::new();
        if let Some(id) = self.selected_clip_id.clone() {
            ids.insert(id);
        }
        ids
    }

    fn expand_with_group_members(&self, ids: &HashSet<String>) -> HashSet<String> {
        if ids.is_empty() {
            return HashSet::new();
        }
        let group_ids: HashSet<String> = {
            let proj = self.project.borrow();
            proj.tracks
                .iter()
                .flat_map(|t| t.clips.iter())
                .filter(|c| ids.contains(&c.id))
                .filter_map(|c| c.group_id.clone())
                .collect()
        };
        if group_ids.is_empty() {
            return ids.clone();
        }
        let mut expanded = ids.clone();
        {
            let proj = self.project.borrow();
            for clip in proj.tracks.iter().flat_map(|t| t.clips.iter()) {
                if clip
                    .group_id
                    .as_deref()
                    .is_some_and(|gid| group_ids.contains(gid))
                {
                    expanded.insert(clip.id.clone());
                }
            }
        }
        expanded
    }

    fn grouped_clip_ids_for_clip(&self, clip_id: &str) -> Vec<String> {
        let mut ids = HashSet::new();
        ids.insert(clip_id.to_string());
        self.expand_with_group_members(&ids).into_iter().collect()
    }

    fn shift_select_range_to(&mut self, track_id: &str, to_clip_id: &str) -> bool {
        let anchor = self
            .selection_anchor_clip_id
            .clone()
            .or_else(|| self.selected_clip_id.clone())
            .unwrap_or_else(|| to_clip_id.to_string());
        let range_ids = {
            let proj = self.project.borrow();
            let Some(track) = proj.tracks.iter().find(|t| t.id == track_id) else {
                return false;
            };
            let mut clips = track.clips.clone();
            clips.sort_by_key(|c| c.timeline_start);
            let Some(a_idx) = clips.iter().position(|c| c.id == anchor) else {
                return false;
            };
            let Some(b_idx) = clips.iter().position(|c| c.id == to_clip_id) else {
                return false;
            };
            let (start, end) = if a_idx <= b_idx {
                (a_idx, b_idx)
            } else {
                (b_idx, a_idx)
            };
            clips[start..=end]
                .iter()
                .map(|c| c.id.clone())
                .collect::<Vec<_>>()
        };
        if range_ids.is_empty() {
            return false;
        }
        for id in range_ids {
            self.selected_clip_ids.insert(id);
        }
        self.selected_clip_id = Some(to_clip_id.to_string());
        self.selected_track_id = Some(track_id.to_string());
        true
    }

    fn toggle_clip_selection(&mut self, clip_id: &str, track_id: &str) -> bool {
        if self.selected_clip_ids.contains(clip_id) {
            self.selected_clip_ids.remove(clip_id);
            if self.selected_clip_id.as_deref() == Some(clip_id) {
                self.selected_clip_id = self.selected_clip_ids.iter().next().cloned();
            }
            if self.selected_clip_id.is_none() {
                self.selection_anchor_clip_id = None;
            }
        } else {
            self.selected_clip_ids.insert(clip_id.to_string());
            self.selected_clip_id = Some(clip_id.to_string());
            if self.selection_anchor_clip_id.is_none() {
                self.selection_anchor_clip_id = Some(clip_id.to_string());
            }
            self.selected_track_id = Some(track_id.to_string());
        }
        true
    }

    fn begin_marquee_selection(&mut self, x: f64, y: f64, additive: bool) {
        let base_ids = if additive {
            if self.selected_clip_ids.is_empty() {
                self.selected_clip_id
                    .clone()
                    .map(|id| {
                        let mut s = HashSet::new();
                        s.insert(id);
                        s
                    })
                    .unwrap_or_default()
            } else {
                self.selected_clip_ids.clone()
            }
        } else {
            HashSet::new()
        };
        self.marquee_selection = Some(MarqueeSelection {
            start_x: x,
            start_y: y,
            current_x: x,
            current_y: y,
            additive,
            base_ids,
            base_primary: self.selected_clip_id.clone(),
            base_track_id: self.selected_track_id.clone(),
        });
    }

    fn update_marquee_selection(&mut self, current_x: f64, current_y: f64) -> bool {
        let Some(mut marquee) = self.marquee_selection.clone() else {
            return false;
        };
        marquee.current_x = current_x;
        marquee.current_y = current_y;
        let hits = self.clips_in_rect(marquee.start_x, marquee.start_y, current_x, current_y);
        let mut next_ids = if marquee.additive {
            marquee.base_ids.clone()
        } else {
            HashSet::new()
        };
        for (id, _) in &hits {
            next_ids.insert(id.clone());
        }
        self.selected_clip_ids = next_ids;
        if self.selected_clip_ids.is_empty() {
            if marquee.additive {
                self.selected_clip_id = marquee.base_primary.clone();
                self.selected_track_id = marquee.base_track_id.clone();
            } else {
                self.selected_clip_id = None;
            }
        } else if let Some((primary_id, primary_track_id)) = hits.last().cloned() {
            self.selected_clip_id = Some(primary_id);
            self.selected_track_id = Some(primary_track_id);
        } else if let Some(primary_id) = self.selected_clip_ids.iter().next().cloned() {
            self.selected_clip_id = Some(primary_id);
        }
        self.marquee_selection = Some(marquee);
        true
    }

    fn end_marquee_selection(&mut self) {
        self.marquee_selection = None;
        if self.selected_clip_id.is_none() {
            self.selection_anchor_clip_id = None;
        } else if self.selection_anchor_clip_id.is_none() {
            self.selection_anchor_clip_id = self.selected_clip_id.clone();
        }
    }

    fn clips_in_rect(&self, x0: f64, y0: f64, x1: f64, y1: f64) -> Vec<(String, String)> {
        let left = x0.min(x1).max(TRACK_LABEL_WIDTH);
        let right = x0.max(x1);
        let top = y0.min(y1).max(RULER_HEIGHT);
        let bottom = y0.max(y1);
        if right <= left || bottom <= top {
            return Vec::new();
        }
        let proj = self.project.borrow();
        let mut hits = Vec::new();
        for (track_idx, track) in proj.tracks.iter().enumerate() {
            let track_y = RULER_HEIGHT + track_idx as f64 * TRACK_HEIGHT;
            let clip_top = track_y + 2.0;
            let clip_bottom = clip_top + TRACK_HEIGHT - 4.0;
            if clip_bottom < top || clip_top > bottom {
                continue;
            }
            for clip in &track.clips {
                let cx = self.ns_to_x(clip.timeline_start);
                let cw = (clip.duration() as f64 / NS_PER_SECOND) * self.pixels_per_second;
                let clip_left = cx;
                let clip_right = cx + cw;
                if clip_right >= left
                    && clip_left <= right
                    && clip_bottom >= top
                    && clip_top <= bottom
                {
                    hits.push((clip.id.clone(), track.id.clone()));
                }
            }
        }
        hits
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

        // Special handling for Roll tool: find adjacent clips at cursor
        if self.active_tool == ActiveTool::Roll {
            let ns = self.x_to_ns(x);
            // Use larger threshold for detection?
            let threshold_ns = (TRIM_HANDLE_PX / self.pixels_per_second * NS_PER_SECOND) as u64;

            for clip in &track.clips {
                let end = clip.timeline_end();
                if ns.abs_diff(end) < threshold_ns {
                    // This clip is the candidate "left" clip. Check for a "right" clip starting exactly at `end`.
                    if let Some(right) = track.clips.iter().find(|c| c.timeline_start == end) {
                        return Some(HitResult {
                            clip_id: clip.id.clone(),
                            track_id: track.id.clone(),
                            track_idx,
                            zone: HitZone::Roll,
                            other_clip_id: Some(right.id.clone()),
                        });
                    }
                }
            }
        }

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
                    other_clip_id: None,
                });
            }
        }
        None
    }
}

fn clip_kind_to_track_kind(kind: &ClipKind) -> TrackKind {
    match kind {
        ClipKind::Audio => TrackKind::Audio,
        ClipKind::Video | ClipKind::Image => TrackKind::Video,
    }
}

fn apply_pasted_attributes(target: &mut Clip, source: &Clip) -> bool {
    let before = target.clone();
    target.brightness = source.brightness;
    target.contrast = source.contrast;
    target.saturation = source.saturation;
    target.denoise = source.denoise;
    target.sharpness = source.sharpness;
    target.volume = source.volume;
    target.pan = source.pan;
    target.speed = source.speed;
    target.crop_left = source.crop_left;
    target.crop_right = source.crop_right;
    target.crop_top = source.crop_top;
    target.crop_bottom = source.crop_bottom;
    target.rotate = source.rotate;
    target.flip_h = source.flip_h;
    target.flip_v = source.flip_v;
    target.title_text = source.title_text.clone();
    target.title_font = source.title_font.clone();
    target.title_color = source.title_color;
    target.title_x = source.title_x;
    target.title_y = source.title_y;
    target.transition_after = source.transition_after.clone();
    target.transition_after_ns = source.transition_after_ns;
    target.lut_path = source.lut_path.clone();
    target.scale = source.scale;
    target.opacity = source.opacity;
    target.position_x = source.position_x;
    target.position_y = source.position_y;
    target.shadows = source.shadows;
    target.midtones = source.midtones;
    target.highlights = source.highlights;
    target.reverse = source.reverse;
    before != *target
}

struct HitResult {
    clip_id: String,
    track_id: String,
    track_idx: usize,
    zone: HitZone,
    other_clip_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
enum HitZone {
    TrimIn,
    TrimOut,
    Body,
    Roll,
}

/// Build and return the timeline `DrawingArea` widget.
pub fn build_timeline(state: Rc<RefCell<TimelineState>>) -> DrawingArea {
    let area = DrawingArea::new();
    area.set_vexpand(false);
    area.set_hexpand(true);
    area.set_content_height((RULER_HEIGHT + TRACK_HEIGHT * 4.0) as i32);
    area.set_focusable(true);

    let thumb_cache = Rc::new(RefCell::new(
        crate::media::thumb_cache::ThumbnailCache::new(),
    ));

    let wave_cache = Rc::new(RefCell::new(
        crate::media::waveform_cache::WaveformCache::new(),
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
                if let Some(a) = area_weak.upgrade() {
                    a.queue_draw();
                }
            }
            glib::ControlFlow::Continue
        });
    }

    // Wire on_extraction_pause: pauses/resumes thumb+waveform extraction during playback.
    {
        let thumb_cache = thumb_cache.clone();
        let wave_cache = wave_cache.clone();
        state.borrow_mut().on_extraction_pause = Some(Rc::new(move |paused: bool| {
            thumb_cache.borrow_mut().set_extraction_paused(paused);
            wave_cache.borrow_mut().set_extraction_paused(paused);
        }));
    }

    // ── Click: seek / select / razor ────────────────────────────────────────
    let click = GestureClick::new();
    click.set_button(0); // all buttons
    {
        let state = state.clone();
        let area_weak = area.downgrade();
        click.connect_pressed(move |gesture, _n_press, x, y| {
            // Grab keyboard focus so Delete/Backspace etc. work immediately
            if let Some(a) = area_weak.upgrade() {
                a.grab_focus();
            }
            // Ignore clicks while a project is loading to prevent freezes.
            if state.borrow().loading {
                return;
            }
            let button = gesture.current_button();
            let mut st = state.borrow_mut();

            if y < RULER_HEIGHT {
                if button == 3 {
                    // Right-click in ruler → remove nearest marker within 8px
                    let ns = st.x_to_ns(x);
                    let threshold = (8.0 / st.pixels_per_second * NS_PER_SECOND) as u64;
                    let to_remove = {
                        let proj = st.project.borrow();
                        proj.markers
                            .iter()
                            .filter(|m| m.position_ns.abs_diff(ns) <= threshold)
                            .min_by_key(|m| m.position_ns.abs_diff(ns))
                            .map(|m| m.id.clone())
                    };
                    if let Some(id) = to_remove {
                        st.project.borrow_mut().remove_marker(&id);
                        let proj_cb = st.on_project_changed.clone();
                        drop(st);
                        if let Some(cb) = proj_cb {
                            cb();
                        }
                    }
                } else {
                    // Left-click in ruler → seek
                    let ns = st.x_to_ns(x);
                    st.playhead_ns = ns;
                    let seek_cb = st.on_seek.clone();
                    drop(st);
                    if let Some(cb) = seek_cb {
                        cb(ns);
                    }
                }
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
                        if let Some(cb) = seek_cb {
                            cb(ns);
                        }
                        if let Some(cb) = proj_cb {
                            cb();
                        }
                    }
                    ActiveTool::Select
                    | ActiveTool::Ripple
                    | ActiveTool::Roll
                    | ActiveTool::Slip
                    | ActiveTool::Slide => {
                        let mods = gesture.current_event_state();
                        let shift = mods.contains(gtk::gdk::ModifierType::SHIFT_MASK);
                        let ctrl_or_meta = mods.contains(gtk::gdk::ModifierType::CONTROL_MASK)
                            || mods.contains(gtk::gdk::ModifierType::META_MASK);
                        // Select clip
                        let hit = st.hit_test(x, y);
                        match hit {
                            Some(h) => {
                                if shift {
                                    if !st.shift_select_range_to(&h.track_id, &h.clip_id) {
                                        st.set_single_clip_selection(h.clip_id, h.track_id);
                                    }
                                } else if ctrl_or_meta {
                                    st.toggle_clip_selection(&h.clip_id, &h.track_id);
                                } else {
                                    st.set_single_clip_selection(h.clip_id, h.track_id);
                                }
                            }
                            None => {
                                if !shift && !ctrl_or_meta {
                                    st.clear_clip_selection();
                                    // Click on empty track area still selects the track
                                    if y > RULER_HEIGHT {
                                        let track_idx = ((y - RULER_HEIGHT) / TRACK_HEIGHT) as usize;
                                        let tid = {
                                            let proj = st.project.borrow();
                                            proj.tracks.get(track_idx).map(|t| t.id.clone())
                                        };
                                        st.selected_track_id = tid;
                                    } else {
                                        st.selected_track_id = None;
                                    }
                                } else {
                                }
                            }
                        }
                        // Immediately notify inspector of the new selection without
                        // rebuilding the program player (which would clear all GStreamer slots).
                        let sel_cb = st.on_clip_selected.clone();
                        let new_sel = st.selected_clip_id.clone();
                        drop(st);
                        if let Some(cb) = sel_cb {
                            cb(new_sel);
                        }
                    }
                }
            } else if button == 3 {
                // Right-click on transition marker → remove transition.
                let track_idx = if y > RULER_HEIGHT {
                    ((y - RULER_HEIGHT) / TRACK_HEIGHT) as usize
                } else {
                    usize::MAX
                };
                let ns = st.x_to_ns(x);
                let threshold_ns = ((12.0 / st.pixels_per_second) * NS_PER_SECOND as f64) as u64;
                let transition_hit = {
                    let proj = st.project.borrow();
                    proj.tracks.get(track_idx).and_then(|track| {
                        track
                            .clips
                            .iter()
                            .filter(|c| !c.transition_after.is_empty() && c.transition_after_ns > 0)
                            .filter_map(|c| {
                                let diff = c.timeline_end().abs_diff(ns);
                                if diff <= threshold_ns {
                                    Some((
                                        c.id.clone(),
                                        track.id.clone(),
                                        c.transition_after.clone(),
                                        c.transition_after_ns,
                                        diff,
                                    ))
                                } else {
                                    None
                                }
                            })
                            .min_by_key(|(_, _, _, _, diff)| *diff)
                            .map(
                                |(clip_id, track_id, old_transition, old_transition_ns, _)| {
                                    (clip_id, track_id, old_transition, old_transition_ns)
                                },
                            )
                    })
                };

                if let Some((clip_id, track_id, old_transition, old_transition_ns)) = transition_hit
                {
                    let cmd = crate::undo::SetClipTransitionCommand {
                        clip_id,
                        track_id,
                        old_transition,
                        old_transition_ns,
                        new_transition: String::new(),
                        new_transition_ns: 0,
                    };
                    let project_rc = st.project.clone();
                    let mut proj = project_rc.borrow_mut();
                    st.history.execute(Box::new(cmd), &mut proj);
                    let proj_cb = st.on_project_changed.clone();
                    drop(proj);
                    drop(st);
                    if let Some(cb) = proj_cb {
                        cb();
                    }
                } else {
                    // Right-click → context actions (for now: select clip for Delete key)
                    let hit = st.hit_test(x, y);
                    if let Some(h) = hit {
                        st.set_single_clip_selection(h.clip_id, h.track_id);
                    }
                    let sel_cb = st.on_clip_selected.clone();
                    let new_sel = st.selected_clip_id.clone();
                    drop(st);
                    if let Some(cb) = sel_cb {
                        cb(new_sel);
                    }
                    // delete_selected called via keyboard (Delete key)
                }
            } else {
                drop(st);
            }

            if let Some(a) = area_weak.upgrade() {
                a.queue_draw();
            }
        });
    }
    let click_ref = click.clone();
    area.add_controller(click);

    // ── Drag: move or trim clips ────────────────────────────────────────────
    let drag = GestureDrag::new();
    {
        let state = state.clone();
        let area_weak = area.downgrade();

        drag.connect_drag_begin({
            let state = state.clone();
            let area_weak = area_weak.clone();
            move |gesture, x, y| {
                if state.borrow().loading {
                    return;
                }
                let mut st = state.borrow_mut();
                if y < RULER_HEIGHT {
                    // On drag-begin in ruler: record start offset for panning;
                    // also seek playhead to clicked position.
                    let ns = st.x_to_ns(x);
                    st.playhead_ns = ns;
                    st.ruler_pan_start_offset = st.scroll_offset;
                    let seek_cb = st.on_seek.clone();
                    drop(st);
                    if let Some(cb) = seek_cb {
                        cb(ns);
                    }
                    if let Some(a) = area_weak.upgrade() {
                        a.queue_draw();
                    }
                    return;
                }
                if !matches!(
                    st.active_tool,
                    ActiveTool::Select | ActiveTool::Ripple | ActiveTool::Slip | ActiveTool::Slide
                ) {
                    return;
                }

                let hit = st.hit_test(x, y);
                if let Some(h) = hit {
                    // Extract clip data before mutating st (avoids borrow conflict)
                    let (clip_data, track_snapshot) = {
                        let proj = st.project.borrow();
                        let clip_data = proj
                            .tracks
                            .iter()
                            .flat_map(|t| t.clips.iter())
                            .find(|c| c.id == h.clip_id)
                            .map(|c| (c.timeline_start, c.source_in, c.source_out));
                        let track_snapshot = proj
                            .tracks
                            .iter()
                            .find(|t| t.id == h.track_id)
                            .map(|t| t.clips.clone())
                            .unwrap_or_default();
                        (clip_data, track_snapshot)
                    };
                    if let Some((tl_start, src_in, src_out)) = clip_data {
                        let offset_ns = st.x_to_ns(x).saturating_sub(tl_start);
                        let click_ns = st.x_to_ns(x);
                        let move_clip_ids = st.grouped_clip_ids_for_clip(&h.clip_id);
                        let move_clip_set: HashSet<String> = move_clip_ids.iter().cloned().collect();
                        let (original_member_starts, original_tracks) = {
                            let proj = st.project.borrow();
                            let original_member_starts = proj
                                .tracks
                                .iter()
                                .flat_map(|t| t.clips.iter())
                                .filter(|c| move_clip_set.contains(&c.id))
                                .map(|c| (c.id.clone(), c.timeline_start))
                                .collect::<Vec<_>>();
                            let original_tracks = proj
                                .tracks
                                .iter()
                                .filter(|t| t.clips.iter().any(|c| move_clip_set.contains(&c.id)))
                                .map(|t| (t.id.clone(), t.clips.clone()))
                                .collect::<Vec<_>>();
                            (original_member_starts, original_tracks)
                        };
                        st.drag_op = match h.zone {
                            HitZone::Body if st.active_tool == ActiveTool::Slip => DragOp::Slip {
                                clip_id: h.clip_id.clone(),
                                track_id: h.track_id.clone(),
                                original_source_in: src_in,
                                original_source_out: src_out,
                                drag_start_ns: click_ns,
                            },
                            HitZone::Body if st.active_tool == ActiveTool::Slide => {
                                // Find left and right neighbors on same track
                                let mut sorted = track_snapshot.clone();
                                sorted.sort_by_key(|c| c.timeline_start);
                                let clip_idx = sorted.iter().position(|c| c.id == h.clip_id);
                                let left = clip_idx.and_then(|i| {
                                    if i > 0 {
                                        Some(&sorted[i - 1])
                                    } else {
                                        None
                                    }
                                });
                                let right = clip_idx.and_then(|i| sorted.get(i + 1));
                                DragOp::Slide {
                                    clip_id: h.clip_id.clone(),
                                    track_id: h.track_id.clone(),
                                    original_start: tl_start,
                                    drag_start_ns: click_ns,
                                    left_clip_id: left.map(|c| c.id.clone()),
                                    original_left_out: left.map(|c| c.source_out),
                                    right_clip_id: right.map(|c| c.id.clone()),
                                    original_right_in: right.map(|c| c.source_in),
                                    original_right_start: right.map(|c| c.timeline_start),
                                }
                            }
                            HitZone::Body => DragOp::MoveClip {
                                clip_id: h.clip_id.clone(),
                                original_track_id: h.track_id.clone(),
                                current_track_id: h.track_id.clone(),
                                original_start: tl_start,
                                clip_offset_ns: offset_ns,
                                original_track_clips: track_snapshot.clone(),
                                move_clip_ids: move_clip_ids.clone(),
                                original_member_starts,
                                original_tracks,
                            },
                            HitZone::TrimIn => DragOp::TrimIn {
                                clip_id: h.clip_id.clone(),
                                track_id: h.track_id.clone(),
                                original_source_in: src_in,
                                original_timeline_start: tl_start,
                                original_track_clips: track_snapshot.clone(),
                            },
                            HitZone::TrimOut => DragOp::TrimOut {
                                clip_id: h.clip_id.clone(),
                                track_id: h.track_id.clone(),
                                original_source_out: src_out,
                                original_track_clips: track_snapshot,
                            },
                            HitZone::Roll => {
                                // For Roll, we need data for both clips.
                                // h.clip_id is LEFT clip. h.other_clip_id is RIGHT clip.
                                if let Some(right_id) = h.other_clip_id {
                                    let proj = st.project.borrow();
                                    let right_data = proj
                                        .tracks
                                        .iter()
                                        .flat_map(|t| t.clips.iter())
                                        .find(|c| c.id == right_id)
                                        .map(|c| (c.source_in, c.timeline_start));

                                    if let Some((right_in, right_start)) = right_data {
                                        DragOp::Roll {
                                            left_clip_id: h.clip_id.clone(),
                                            right_clip_id: right_id,
                                            track_id: h.track_id.clone(),
                                            original_left_out: src_out,
                                            original_right_in: right_in,
                                            original_right_start: right_start,
                                        }
                                    } else {
                                        DragOp::None
                                    }
                                } else {
                                    DragOp::None
                                }
                            }
                        };
                        if move_clip_ids.len() > 1 {
                            st.set_selection_with_primary(h.clip_id, h.track_id, move_clip_set);
                        } else {
                            st.set_single_clip_selection(h.clip_id, h.track_id);
                        }
                    }
                } else if x < TRACK_LABEL_WIDTH && y > RULER_HEIGHT {
                    // Drag started in track label area → track reorder
                    let track_idx = ((y - RULER_HEIGHT) / TRACK_HEIGHT) as usize;
                    let track_count = st.project.borrow().tracks.len();
                    if track_idx < track_count {
                        st.drag_op = DragOp::ReorderTrack {
                            track_idx,
                            target_idx: track_idx,
                        };
                    }
                } else if st.active_tool == ActiveTool::Select && y > RULER_HEIGHT {
                    let mods = gesture.current_event_state();
                    let additive = mods.contains(gtk::gdk::ModifierType::CONTROL_MASK)
                        || mods.contains(gtk::gdk::ModifierType::META_MASK);
                    st.begin_marquee_selection(x, y, additive);
                }
            }
        });

        drag.connect_drag_update({
            let state = state.clone();
            let area_weak = area_weak.clone();
            move |gesture, offset_x, offset_y| {
                let (start_x, start_y) = gesture.start_point().unwrap_or((0.0, 0.0));
                let current_x = start_x + offset_x;
                let current_y = start_y + offset_y;
                let button = gesture.current_button();

                if start_y < RULER_HEIGHT {
                    if button == 2 || button == 3 {
                        // Middle/right drag on ruler = pan timeline.
                        let mut st = state.borrow_mut();
                        st.scroll_offset = (st.ruler_pan_start_offset - offset_x).max(0.0);
                        if let Some(a) = area_weak.upgrade() {
                            a.queue_draw();
                        }
                    } else {
                        // Left drag on ruler = continuous scrubbing.
                        let mut st = state.borrow_mut();
                        let ns = st.x_to_ns(current_x);
                        st.playhead_ns = ns;
                        let seek_cb = st.on_seek.clone();
                        drop(st);
                        if let Some(cb) = seek_cb {
                            cb(ns);
                        }
                        if let Some(a) = area_weak.upgrade() {
                            a.queue_draw();
                        }
                    }
                    return;
                }

                let current_ns = {
                    let st = state.borrow();
                    st.x_to_ns(current_x)
                };

                let mut st = state.borrow_mut();
                let drag_op = st.drag_op.clone();
                match drag_op {
                    DragOp::MoveClip {
                        ref clip_id,
                        ref current_track_id,
                        original_start,
                        clip_offset_ns,
                        ref move_clip_ids,
                        ref original_member_starts,
                        ..
                    } => {
                        let grouped_move = move_clip_ids.len() > 1;
                        let raw_start = current_ns.saturating_sub(clip_offset_ns);
                        if grouped_move {
                            let move_set: HashSet<String> = move_clip_ids.iter().cloned().collect();
                            let snap_ns = (10.0 / st.pixels_per_second * NS_PER_SECOND) as u64;
                            let snap_start = {
                                let proj = st.project.borrow();
                                let edges: Vec<u64> = proj
                                    .tracks
                                    .iter()
                                    .flat_map(|t| t.clips.iter())
                                    .filter(|c| !move_set.contains(&c.id))
                                    .flat_map(|c| [c.timeline_start, c.timeline_end()])
                                    .collect();
                                let this_dur = proj
                                    .tracks
                                    .iter()
                                    .flat_map(|t| t.clips.iter())
                                    .find(|c| &c.id == clip_id)
                                    .map(|c| c.duration())
                                    .unwrap_or(0);
                                let by_start = edges
                                    .iter()
                                    .copied()
                                    .filter(|&e| (e as i64 - raw_start as i64).unsigned_abs() < snap_ns)
                                    .min_by_key(|&e| (e as i64 - raw_start as i64).unsigned_abs());
                                let by_end = edges
                                    .iter()
                                    .copied()
                                    .filter(|&e| {
                                        let end = raw_start + this_dur;
                                        (e as i64 - end as i64).unsigned_abs() < snap_ns
                                    })
                                    .min_by_key(|&e| {
                                        let end = raw_start + this_dur;
                                        (e as i64 - end as i64).unsigned_abs()
                                    })
                                    .map(|e| e.saturating_sub(this_dur));
                                match (by_start, by_end) {
                                    (Some(a), Some(b)) => {
                                        let da = (a as i64 - raw_start as i64).unsigned_abs();
                                        let db = (b as i64 - raw_start as i64).unsigned_abs();
                                        if da <= db { a } else { b }
                                    }
                                    (Some(a), None) => a,
                                    (None, Some(b)) => b,
                                    (None, None) => raw_start,
                                }
                            };
                            let delta = snap_start as i64 - original_start as i64;
                            let mut proj = st.project.borrow_mut();
                            for (member_id, member_start) in original_member_starts {
                                if let Some(clip) = proj
                                    .tracks
                                    .iter_mut()
                                    .flat_map(|t| t.clips.iter_mut())
                                    .find(|c| c.id == *member_id)
                                {
                                    clip.timeline_start = (i128::from(*member_start) + i128::from(delta))
                                        .max(0) as u64;
                                }
                            }
                            for track in &mut proj.tracks {
                                if track.clips.iter().any(|c| move_set.contains(&c.id)) {
                                    track.sort_clips();
                                }
                            }
                        } else {
                            // ── Determine target track from y position ──────────
                            let target_track_idx = if current_y > RULER_HEIGHT {
                                ((current_y - RULER_HEIGHT) / TRACK_HEIGHT) as usize
                            } else {
                                0
                            };
                            let (target_track_id, same_kind) = {
                                let proj = st.project.borrow();
                                let cur_kind = proj
                                    .tracks
                                    .iter()
                                    .find(|t| &t.id == current_track_id)
                                    .map(|t| t.kind.clone());
                                match proj.tracks.get(target_track_idx) {
                                    Some(target)
                                        if Some(&target.kind) == cur_kind.as_ref()
                                            && target.id != *current_track_id =>
                                    {
                                        (Some(target.id.clone()), true)
                                    }
                                    _ => (None, false),
                                }
                            };
                            // Move clip between tracks if target is valid and different
                            if same_kind {
                                if let Some(ref new_tid) = target_track_id {
                                    let mut proj = st.project.borrow_mut();
                                    let extracted = {
                                        let from =
                                            proj.tracks.iter_mut().find(|t| &t.id == current_track_id);
                                        from.and_then(|t| {
                                            let pos = t.clips.iter().position(|c| &c.id == clip_id);
                                            pos.map(|i| t.clips.remove(i))
                                        })
                                    };
                                    if let Some(mut clip) = extracted {
                                        clip.timeline_start = raw_start;
                                        if let Some(to_track) =
                                            proj.tracks.iter_mut().find(|t| &t.id == new_tid)
                                        {
                                            to_track.add_clip(clip);
                                        }
                                    }
                                    drop(proj);
                                    // Update drag_op with new current_track_id
                                    if let DragOp::MoveClip {
                                        ref mut current_track_id,
                                        ..
                                    } = st.drag_op
                                    {
                                        *current_track_id = new_tid.clone();
                                    }
                                    st.selected_track_id = Some(new_tid.clone());
                                }
                            }

                            // ── Snap to clip edges ──────────────────────────────
                            let active_track_id = if let DragOp::MoveClip {
                                ref current_track_id,
                                ..
                            } = st.drag_op
                            {
                                current_track_id.clone()
                            } else {
                                String::new()
                            };

                            let snap_ns = (10.0 / st.pixels_per_second * NS_PER_SECOND) as u64;
                            let (_clip_dur, snap_start) = {
                                let proj = st.project.borrow();
                                let edges: Vec<u64> = proj
                                    .tracks
                                    .iter()
                                    .flat_map(|t| t.clips.iter())
                                    .filter(|c| &c.id != clip_id)
                                    .flat_map(|c| [c.timeline_start, c.timeline_end()])
                                    .collect();
                                let this_dur = proj
                                    .tracks
                                    .iter()
                                    .flat_map(|t| t.clips.iter())
                                    .find(|c| &c.id == clip_id)
                                    .map(|c| c.duration())
                                    .unwrap_or(0);
                                let by_start = edges
                                    .iter()
                                    .copied()
                                    .filter(|&e| (e as i64 - raw_start as i64).unsigned_abs() < snap_ns)
                                    .min_by_key(|&e| (e as i64 - raw_start as i64).unsigned_abs());
                                let by_end = edges
                                    .iter()
                                    .copied()
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
                            let mut proj = st.project.borrow_mut();
                            if let Some(track) =
                                proj.tracks.iter_mut().find(|t| t.id == active_track_id)
                            {
                                if let Some(clip) = track.clips.iter_mut().find(|c| &c.id == clip_id)
                                {
                                    clip.timeline_start = snap_start;
                                }
                            }
                        }
                    }
                    DragOp::TrimIn {
                        ref clip_id,
                        ref track_id,
                        original_source_in,
                        original_timeline_start,
                        ref original_track_clips,
                    } => {
                        let drag_ns = current_ns as i64 - original_timeline_start as i64;
                        // Snap the new timeline_start to nearby clip edges
                        let snap_ns = (10.0 / st.pixels_per_second * NS_PER_SECOND) as i64;
                        let new_start_raw =
                            (original_timeline_start as i64 + drag_ns).max(0) as u64;

                        let snapped_start = if st.active_tool == ActiveTool::Ripple {
                            // Ripple mode: we can push clips, but let's still snap to edges for precision
                            // We shouldn't snap to OURSELF or clips we are pushing?
                            // For simplicity, snap to anything but self.
                            let proj = st.project.borrow();
                            let edges: Vec<u64> = proj
                                .tracks
                                .iter()
                                .flat_map(|t| t.clips.iter())
                                .filter(|c| &c.id != clip_id)
                                .flat_map(|c| [c.timeline_start, c.timeline_end()])
                                .collect();
                            edges
                                .iter()
                                .copied()
                                .filter(|&e| (e as i64 - new_start_raw as i64).abs() < snap_ns)
                                .min_by_key(|&e| (e as i64 - new_start_raw as i64).abs())
                                .unwrap_or(new_start_raw)
                        } else {
                            // Standard TrimIn: constrained by adjacent clips?
                            // Current logic didn't constrain, just snapped.
                            let proj = st.project.borrow();
                            let edges: Vec<u64> = proj
                                .tracks
                                .iter()
                                .flat_map(|t| t.clips.iter())
                                .filter(|c| &c.id != clip_id)
                                .flat_map(|c| [c.timeline_start, c.timeline_end()])
                                .collect();
                            edges
                                .iter()
                                .copied()
                                .filter(|&e| (e as i64 - new_start_raw as i64).abs() < snap_ns)
                                .min_by_key(|&e| (e as i64 - new_start_raw as i64).abs())
                                .unwrap_or(new_start_raw)
                        };

                        let snapped_drag = snapped_start as i64 - original_timeline_start as i64;

                        let mut proj = st.project.borrow_mut();
                        if let Some(track) = proj.tracks.iter_mut().find(|t| &t.id == track_id) {
                            // 1. Update the trimmed clip
                            let mut new_ts = original_timeline_start;
                            if let Some(clip) = track.clips.iter_mut().find(|c| &c.id == clip_id) {
                                let new_source_in =
                                    (original_source_in as i64 + snapped_drag).max(0) as u64;
                                // Check valid duration (source_in < source_out)
                                if new_source_in < clip.source_out.saturating_sub(1_000_000) {
                                    clip.source_in = new_source_in;
                                    clip.timeline_start =
                                        (original_timeline_start as i64 + snapped_drag).max(0)
                                            as u64;
                                    new_ts = clip.timeline_start;
                                }
                            }

                            // 2. If Ripple, shift subsequent clips
                            if st.active_tool == ActiveTool::Ripple {
                                let threshold = original_timeline_start;
                                let actual_delta = new_ts as i64 - original_timeline_start as i64;

                                for clip in &mut track.clips {
                                    if clip.id == *clip_id {
                                        continue;
                                    }
                                    // Use original positions to avoid drift
                                    if let Some(orig) =
                                        original_track_clips.iter().find(|c| c.id == clip.id)
                                    {
                                        if orig.timeline_start > threshold {
                                            let new_pos =
                                                (orig.timeline_start as i64 + actual_delta).max(0)
                                                    as u64;
                                            clip.timeline_start = new_pos;
                                        }
                                    }
                                }
                            }
                        }
                    }
                    DragOp::TrimOut {
                        ref clip_id,
                        ref track_id,
                        original_source_out: _,
                        ref original_track_clips,
                    } => {
                        // Snap the out-point to nearby clip edges
                        let snap_ns = (10.0 / st.pixels_per_second * NS_PER_SECOND) as u64;
                        let snapped_ns = {
                            let proj = st.project.borrow();
                            let edges: Vec<u64> = proj
                                .tracks
                                .iter()
                                .flat_map(|t| t.clips.iter())
                                .filter(|c| &c.id != clip_id)
                                .flat_map(|c| [c.timeline_start, c.timeline_end()])
                                .collect();
                            edges
                                .iter()
                                .copied()
                                .filter(|&e| {
                                    (e as i64 - current_ns as i64).unsigned_abs() < snap_ns
                                })
                                .min_by_key(|&e| (e as i64 - current_ns as i64).unsigned_abs())
                                .unwrap_or(current_ns)
                        };
                        let mut proj = st.project.borrow_mut();
                        if let Some(track) = proj.tracks.iter_mut().find(|t| &t.id == track_id) {
                            // Find original clip data to compute stable delta
                            if let Some(orig_clip) =
                                original_track_clips.iter().find(|c| &c.id == clip_id)
                            {
                                // Calculate new source_out based on original start
                                let new_timeline_end = snapped_ns;
                                let tl_start = orig_clip.timeline_start;

                                if new_timeline_end > tl_start + 1_000_000 {
                                    let new_dur = new_timeline_end - tl_start;
                                    let new_source_out = orig_clip.source_in + new_dur;

                                    // Update target clip
                                    if let Some(clip) =
                                        track.clips.iter_mut().find(|c| &c.id == clip_id)
                                    {
                                        clip.source_out = new_source_out;
                                    }

                                    // Ripple Logic
                                    if st.active_tool == ActiveTool::Ripple {
                                        let old_dur = orig_clip.duration();
                                        let delta = new_dur as i64 - old_dur as i64;
                                        let threshold = orig_clip.timeline_end(); // Original end

                                        // Update subsequent clips
                                        for clip in &mut track.clips {
                                            // Find this clip in original_track_clips to get its base start
                                            if let Some(orig_other) = original_track_clips
                                                .iter()
                                                .find(|c| c.id == clip.id)
                                            {
                                                if orig_other.timeline_start >= threshold {
                                                    let new_start =
                                                        (orig_other.timeline_start as i64 + delta)
                                                            .max(0)
                                                            as u64;
                                                    clip.timeline_start = new_start;
                                                }
                                            }
                                        }
                                    }
                                }
                            } else {
                                // Fallback if original not found (shouldn't happen)
                                if let Some(clip) =
                                    track.clips.iter_mut().find(|c| &c.id == clip_id)
                                {
                                    if snapped_ns > clip.source_in + 1_000_000 {
                                        let offset = snapped_ns.saturating_sub(clip.timeline_start);
                                        clip.source_out = clip.source_in + offset;
                                    }
                                }
                            }
                        }
                    }
                    DragOp::Roll {
                        left_clip_id,
                        right_clip_id,
                        track_id,
                        original_left_out: _,
                        original_right_in,
                        original_right_start,
                    } => {
                        let current_ns = st.x_to_ns(current_x);
                        let drag_ns = current_ns as i64 - original_right_start as i64;
                        let new_cut_pos = (original_right_start as i64 + drag_ns).max(0) as u64;

                        let mut proj = st.project.borrow_mut();
                        if let Some(track) = proj.tracks.iter_mut().find(|t| &t.id == &track_id) {
                            // Find left start to ensure we don't go past it
                            let left_start = track
                                .clips
                                .iter()
                                .find(|c| &c.id == &left_clip_id)
                                .map(|c| c.timeline_start)
                                .unwrap_or(0);

                            // Simple update:
                            if new_cut_pos > left_start + 1_000_000 {
                                // Update Left
                                if let Some(left) =
                                    track.clips.iter_mut().find(|c| &c.id == &left_clip_id)
                                {
                                    let new_dur = new_cut_pos - left.timeline_start;
                                    left.source_out = left.source_in + new_dur;
                                }
                                // Update Right
                                if let Some(right) =
                                    track.clips.iter_mut().find(|c| &c.id == &right_clip_id)
                                {
                                    // Right source_in increases if we move cut right.
                                    let new_right_in =
                                        (original_right_in as i64 + drag_ns).max(0) as u64;
                                    // Basic bounds check (simplified)
                                    // if new_right_in < right.source_out.saturating_sub(1_000_000) {
                                    right.source_in = new_right_in;
                                    right.timeline_start = new_cut_pos;
                                    // }
                                }
                            }
                        }
                    }
                    DragOp::Slip {
                        ref clip_id,
                        ref track_id,
                        original_source_in,
                        original_source_out,
                        drag_start_ns,
                    } => {
                        let delta = current_ns as i64 - drag_start_ns as i64;
                        let new_source_in = (original_source_in as i64 + delta).max(0) as u64;
                        let new_source_out = (original_source_out as i64 + delta)
                            .max(new_source_in as i64 + 1_000_000)
                            as u64;
                        let mut proj = st.project.borrow_mut();
                        if let Some(track) = proj.tracks.iter_mut().find(|t| &t.id == track_id) {
                            if let Some(clip) = track.clips.iter_mut().find(|c| &c.id == clip_id) {
                                clip.source_in = new_source_in;
                                clip.source_out = new_source_out;
                            }
                        }
                    }
                    DragOp::Slide {
                        ref clip_id,
                        ref track_id,
                        original_start,
                        drag_start_ns,
                        ref left_clip_id,
                        original_left_out,
                        ref right_clip_id,
                        original_right_in,
                        original_right_start,
                    } => {
                        let delta = current_ns as i64 - drag_start_ns as i64;
                        let new_start = (original_start as i64 + delta).max(0) as u64;
                        let mut proj = st.project.borrow_mut();
                        if let Some(track) = proj.tracks.iter_mut().find(|t| &t.id == track_id) {
                            // Validate neighbors can absorb the delta
                            let left_ok = if let (Some(ref lid), Some(orig_out)) =
                                (left_clip_id, original_left_out)
                            {
                                let new_out = (orig_out as i64 + delta).max(0) as u64;
                                let left_in = track
                                    .clips
                                    .iter()
                                    .find(|c| &c.id == lid)
                                    .map(|c| c.source_in)
                                    .unwrap_or(0);
                                new_out > left_in + 1_000_000
                            } else {
                                true
                            };
                            let right_ok = if let (Some(ref rid), Some(orig_in), Some(_orig_rs)) =
                                (right_clip_id, original_right_in, original_right_start)
                            {
                                let new_in = (orig_in as i64 + delta).max(0) as u64;
                                let right_out = track
                                    .clips
                                    .iter()
                                    .find(|c| &c.id == rid)
                                    .map(|c| c.source_out)
                                    .unwrap_or(u64::MAX);
                                new_in + 1_000_000 < right_out
                            } else {
                                true
                            };
                            if left_ok && right_ok {
                                if let Some(clip) =
                                    track.clips.iter_mut().find(|c| &c.id == clip_id)
                                {
                                    clip.timeline_start = new_start;
                                }
                                if let (Some(ref lid), Some(orig_out)) =
                                    (left_clip_id, original_left_out)
                                {
                                    if let Some(left) =
                                        track.clips.iter_mut().find(|c| &c.id == lid)
                                    {
                                        left.source_out = (orig_out as i64 + delta).max(0) as u64;
                                    }
                                }
                                if let (Some(ref rid), Some(orig_in), Some(orig_rs)) =
                                    (right_clip_id, original_right_in, original_right_start)
                                {
                                    if let Some(right) =
                                        track.clips.iter_mut().find(|c| &c.id == rid)
                                    {
                                        right.source_in = (orig_in as i64 + delta).max(0) as u64;
                                        right.timeline_start =
                                            (orig_rs as i64 + delta).max(0) as u64;
                                    }
                                }
                            }
                        }
                    }
                    DragOp::ReorderTrack { track_idx, .. } => {
                        let new_target = if current_y > RULER_HEIGHT {
                            let idx = ((current_y - RULER_HEIGHT) / TRACK_HEIGHT) as usize;
                            let count = st.project.borrow().tracks.len();
                            idx.min(count.saturating_sub(1))
                        } else {
                            0
                        };
                        if let DragOp::ReorderTrack {
                            ref mut target_idx, ..
                        } = st.drag_op
                        {
                            *target_idx = new_target;
                        }
                    }
                    DragOp::None => {
                        if st.marquee_selection.is_some() {
                            st.update_marquee_selection(current_x, current_y);
                        }
                    }
                }

                if let Some(a) = area_weak.upgrade() {
                    a.queue_draw();
                }
            }
        });

        drag.connect_drag_end({
            let state = state.clone();
            move |_, _, _| {
                let mut st = state.borrow_mut();
                let drag_op = std::mem::replace(&mut st.drag_op, DragOp::None);
                let should_notify_project = !matches!(&drag_op, DragOp::None);
                let had_marquee = st.marquee_selection.is_some();
                if had_marquee {
                    st.end_marquee_selection();
                }
                let magnetic_mode = st.magnetic_mode;

                // Commit drag to undo history
                match drag_op {
                    DragOp::MoveClip {
                        ref clip_id,
                        ref original_track_id,
                        ref current_track_id,
                        original_start,
                        original_track_clips,
                        move_clip_ids,
                        original_tracks,
                        ..
                    } => {
                        let grouped_move = move_clip_ids.len() > 1;
                        if grouped_move {
                            let track_updates = {
                                let proj = st.project.borrow();
                                original_tracks
                                    .iter()
                                    .filter_map(|(track_id, old_clips)| {
                                        let new_clips = proj
                                            .tracks
                                            .iter()
                                            .find(|t| t.id == *track_id)
                                            .map(|t| t.clips.clone())
                                            .unwrap_or_default();
                                        if &new_clips != old_clips {
                                            Some((track_id.clone(), old_clips.clone(), new_clips))
                                        } else {
                                            None
                                        }
                                    })
                                    .collect::<Vec<_>>()
                            };
                            if !track_updates.is_empty() {
                                let project = st.project.clone();
                                let mut proj = project.borrow_mut();
                                for (track_id, old_clips, new_clips) in track_updates {
                                    let cmd = SetTrackClipsCommand {
                                        track_id,
                                        old_clips,
                                        new_clips,
                                        label: "Move grouped clips".to_string(),
                                    };
                                    st.history.execute(Box::new(cmd), &mut proj);
                                }
                            }
                        } else {
                            let cross_track = original_track_id != current_track_id;
                            if magnetic_mode {
                            // Compact the current (destination) track
                            let mut new_clips = {
                                let proj = st.project.borrow();
                                proj.tracks
                                    .iter()
                                    .find(|t| &t.id == current_track_id)
                                    .map(|t| t.clips.clone())
                                    .unwrap_or_default()
                            };
                            compact_gap_free_clips(&mut new_clips);
                            if cross_track {
                                // Also compact the original (source) track
                                let mut orig_clips_now = {
                                    let proj = st.project.borrow();
                                    proj.tracks
                                        .iter()
                                        .find(|t| &t.id == original_track_id)
                                        .map(|t| t.clips.clone())
                                        .unwrap_or_default()
                                };
                                compact_gap_free_clips(&mut orig_clips_now);
                                // Apply both compacted states
                                {
                                    let mut proj = st.project.borrow_mut();
                                    if let Some(t) =
                                        proj.tracks.iter_mut().find(|t| &t.id == current_track_id)
                                    {
                                        t.clips = new_clips;
                                    }
                                    if let Some(t) =
                                        proj.tracks.iter_mut().find(|t| &t.id == original_track_id)
                                    {
                                        t.clips = orig_clips_now;
                                    }
                                    proj.dirty = true;
                                }
                            } else if new_clips != original_track_clips {
                                let cmd = SetTrackClipsCommand {
                                    track_id: current_track_id.clone(),
                                    old_clips: original_track_clips,
                                    new_clips,
                                    label: "Move clip (magnetic)".to_string(),
                                };
                                let project = st.project.clone();
                                let mut proj = project.borrow_mut();
                                st.history.execute(Box::new(cmd), &mut proj);
                            }
                            // For cross-track magnetic, we clear redo (complex multi-track undo is out of scope for v1)
                            if cross_track {
                                st.history.redo_stack.clear();
                            }
                            } else {
                                let new_start = {
                                    let proj = st.project.borrow();
                                    proj.tracks
                                        .iter()
                                        .find(|t| &t.id == current_track_id)
                                        .and_then(|t| t.clips.iter().find(|c| &c.id == clip_id))
                                        .map(|c| c.timeline_start)
                                };
                                if let Some(new_start) = new_start {
                                    if new_start != original_start || cross_track {
                                        let cmd = MoveClipCommand {
                                            clip_id: clip_id.clone(),
                                            from_track_id: original_track_id.clone(),
                                            to_track_id: current_track_id.clone(),
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
                        }
                    }
                    DragOp::TrimIn {
                        ref clip_id,
                        ref track_id,
                        original_source_in,
                        original_timeline_start,
                        original_track_clips,
                    } => {
                        if magnetic_mode {
                            let mut new_clips = {
                                let proj = st.project.borrow();
                                proj.tracks
                                    .iter()
                                    .find(|t| &t.id == track_id)
                                    .map(|t| t.clips.clone())
                                    .unwrap_or_default()
                            };
                            compact_gap_free_clips(&mut new_clips);
                            if new_clips != original_track_clips {
                                let cmd = SetTrackClipsCommand {
                                    track_id: track_id.clone(),
                                    old_clips: original_track_clips,
                                    new_clips,
                                    label: "Trim clip (magnetic)".to_string(),
                                };
                                let project = st.project.clone();
                                let mut proj = project.borrow_mut();
                                st.history.execute(Box::new(cmd), &mut proj);
                            }
                        } else {
                            let (new_si, new_ts) = {
                                let proj = st.project.borrow();
                                proj.tracks
                                    .iter()
                                    .find(|t| &t.id == track_id)
                                    .and_then(|t| t.clips.iter().find(|c| &c.id == clip_id))
                                    .map(|c| (c.source_in, c.timeline_start))
                                    .unwrap_or((original_source_in, original_timeline_start))
                            };

                            if st.active_tool == ActiveTool::Ripple {
                                if new_si != original_source_in {
                                    // Delta: if new_ts > original_ts, delta is positive (clips shift right)
                                    // If we trimmed from left (shortened), source_in increased.
                                    // timeline_start increased. delta = new_ts - original_ts.
                                    // If we extended to left, source_in decreased. timeline_start decreased.
                                    let delta = new_ts as i64 - original_timeline_start as i64;

                                    let cmd = crate::undo::RippleTrimInCommand {
                                        clip_id: clip_id.clone(),
                                        track_id: track_id.clone(),
                                        old_source_in: original_source_in,
                                        new_source_in: new_si,
                                        old_timeline_start: original_timeline_start,
                                        new_timeline_start: new_ts,
                                        delta,
                                    };
                                    st.history.undo_stack.push(Box::new(cmd));
                                    st.history.redo_stack.clear();
                                    st.project.borrow_mut().dirty = true;
                                }
                            } else if new_si != original_source_in {
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
                    }
                    DragOp::TrimOut {
                        ref clip_id,
                        ref track_id,
                        original_source_out,
                        ref original_track_clips,
                    } => {
                        if st.active_tool == ActiveTool::Ripple {
                            let new_source_out = {
                                let proj = st.project.borrow();
                                proj.tracks
                                    .iter()
                                    .find(|t| &t.id == track_id)
                                    .and_then(|t| t.clips.iter().find(|c| &c.id == clip_id))
                                    .map(|c| c.source_out)
                            };
                            if let Some(new_out) = new_source_out {
                                if new_out != original_source_out {
                                    let delta = if let Some(orig) =
                                        original_track_clips.iter().find(|c| &c.id == clip_id)
                                    {
                                        let old_dur = orig.duration();
                                        // New duration using original source_in
                                        let new_dur = new_out - orig.source_in;
                                        new_dur as i64 - old_dur as i64
                                    } else {
                                        0
                                    };

                                    let cmd = crate::undo::RippleTrimOutCommand {
                                        clip_id: clip_id.clone(),
                                        track_id: track_id.clone(),
                                        old_source_out: original_source_out,
                                        new_source_out: new_out,
                                        delta,
                                    };
                                    st.history.undo_stack.push(Box::new(cmd));
                                    st.history.redo_stack.clear();
                                    st.project.borrow_mut().dirty = true;
                                }
                            }
                        } else if magnetic_mode {
                            let mut new_clips = {
                                let proj = st.project.borrow();
                                proj.tracks
                                    .iter()
                                    .find(|t| &t.id == track_id)
                                    .map(|t| t.clips.clone())
                                    .unwrap_or_default()
                            };
                            compact_gap_free_clips(&mut new_clips);
                            if new_clips != *original_track_clips {
                                let cmd = SetTrackClipsCommand {
                                    track_id: track_id.clone(),
                                    old_clips: original_track_clips.clone(),
                                    new_clips,
                                    label: "Trim out-point (magnetic)".to_string(),
                                };
                                let project = st.project.clone();
                                let mut proj = project.borrow_mut();
                                st.history.execute(Box::new(cmd), &mut proj);
                            }
                        } else {
                            let new_so = {
                                let proj = st.project.borrow();
                                proj.tracks
                                    .iter()
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
                    }
                    DragOp::Roll {
                        ref left_clip_id,
                        ref right_clip_id,
                        ref track_id,
                        original_left_out,
                        original_right_in,
                        original_right_start,
                    } => {
                        let (new_left_out, new_right_in, new_right_start) = {
                            let proj = st.project.borrow();
                            if let Some(track) = proj.tracks.iter().find(|t| &t.id == track_id) {
                                let left_out = track
                                    .clips
                                    .iter()
                                    .find(|c| &c.id == left_clip_id)
                                    .map(|c| c.source_out)
                                    .unwrap_or(original_left_out);
                                let (right_in, right_start) = track
                                    .clips
                                    .iter()
                                    .find(|c| &c.id == right_clip_id)
                                    .map(|c| (c.source_in, c.timeline_start))
                                    .unwrap_or((original_right_in, original_right_start));
                                (left_out, right_in, right_start)
                            } else {
                                (original_left_out, original_right_in, original_right_start)
                            }
                        };

                        if new_left_out != original_left_out || new_right_in != original_right_in {
                            let cmd = crate::undo::RollEditCommand {
                                left_clip_id: left_clip_id.clone(),
                                right_clip_id: right_clip_id.clone(),
                                track_id: track_id.clone(),
                                old_left_out: original_left_out,
                                new_left_out: new_left_out,
                                old_right_in: original_right_in,
                                new_right_in: new_right_in,
                                old_right_start: original_right_start,
                                new_right_start: new_right_start,
                            };
                            st.history.undo_stack.push(Box::new(cmd));
                            st.history.redo_stack.clear();
                            st.project.borrow_mut().dirty = true;
                        }
                    }
                    DragOp::Slip {
                        ref clip_id,
                        ref track_id,
                        original_source_in,
                        original_source_out,
                        ..
                    } => {
                        let (new_si, new_so) = {
                            let proj = st.project.borrow();
                            proj.tracks
                                .iter()
                                .find(|t| &t.id == track_id)
                                .and_then(|t| t.clips.iter().find(|c| &c.id == clip_id))
                                .map(|c| (c.source_in, c.source_out))
                                .unwrap_or((original_source_in, original_source_out))
                        };
                        if new_si != original_source_in {
                            let cmd = crate::undo::SlipClipCommand {
                                clip_id: clip_id.clone(),
                                track_id: track_id.clone(),
                                old_source_in: original_source_in,
                                old_source_out: original_source_out,
                                new_source_in: new_si,
                                new_source_out: new_so,
                            };
                            st.history.undo_stack.push(Box::new(cmd));
                            st.history.redo_stack.clear();
                            st.project.borrow_mut().dirty = true;
                        }
                    }
                    DragOp::Slide {
                        ref clip_id,
                        ref track_id,
                        original_start,
                        ref left_clip_id,
                        original_left_out,
                        ref right_clip_id,
                        original_right_in,
                        original_right_start,
                        ..
                    } => {
                        let proj = st.project.borrow();
                        let track = proj.tracks.iter().find(|t| &t.id == track_id);
                        let new_start = track
                            .and_then(|t| t.clips.iter().find(|c| &c.id == clip_id))
                            .map(|c| c.timeline_start)
                            .unwrap_or(original_start);
                        let new_left_out = left_clip_id.as_ref().and_then(|lid| {
                            track
                                .and_then(|t| t.clips.iter().find(|c| &c.id == lid))
                                .map(|c| c.source_out)
                        });
                        let new_right_in = right_clip_id.as_ref().and_then(|rid| {
                            track
                                .and_then(|t| t.clips.iter().find(|c| &c.id == rid))
                                .map(|c| c.source_in)
                        });
                        let new_right_start = right_clip_id.as_ref().and_then(|rid| {
                            track
                                .and_then(|t| t.clips.iter().find(|c| &c.id == rid))
                                .map(|c| c.timeline_start)
                        });
                        drop(proj);
                        if new_start != original_start {
                            let cmd = crate::undo::SlideClipCommand {
                                clip_id: clip_id.clone(),
                                track_id: track_id.clone(),
                                old_start: original_start,
                                new_start,
                                left_clip_id: left_clip_id.clone(),
                                old_left_out: original_left_out,
                                new_left_out,
                                right_clip_id: right_clip_id.clone(),
                                old_right_in: original_right_in,
                                new_right_in,
                                old_right_start: original_right_start,
                                new_right_start,
                            };
                            st.history.undo_stack.push(Box::new(cmd));
                            st.history.redo_stack.clear();
                            st.project.borrow_mut().dirty = true;
                        }
                    }
                    DragOp::ReorderTrack {
                        track_idx,
                        target_idx,
                    } => {
                        if track_idx != target_idx {
                            let track_count = st.project.borrow().tracks.len();
                            if track_idx < track_count && target_idx < track_count {
                                let cmd = ReorderTrackCommand {
                                    from_index: track_idx,
                                    to_index: target_idx,
                                };
                                let project = st.project.clone();
                                let mut proj = project.borrow_mut();
                                st.history.execute(Box::new(cmd), &mut proj);
                            }
                        }
                    }
                    DragOp::None => {}
                }

                let proj_cb = if should_notify_project {
                    st.on_project_changed.clone()
                } else {
                    None
                };
                let sel_cb = if had_marquee {
                    st.on_clip_selected.clone()
                } else {
                    None
                };
                let new_sel = st.selected_clip_id.clone();
                drop(st);
                if let Some(cb) = proj_cb {
                    cb();
                }
                if let Some(cb) = sel_cb {
                    cb(new_sel);
                }
            }
        });
    }
    area.add_controller(drag.clone());
    // group_with requires both gestures to already be on the same widget.
    drag.group_with(&click_ref);

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
            let mut notify_selection = false;

            let handled = match key {
                Key::z if ctrl && !shift => {
                    st.undo();
                    notify_project = true;
                    true
                }
                Key::z if ctrl && shift => {
                    st.redo();
                    notify_project = true;
                    true
                }
                Key::y if ctrl => {
                    st.redo();
                    notify_project = true;
                    true
                }
                Key::a | Key::A if ctrl => {
                    let changed = st.select_all_clips();
                    if changed {
                        notify_selection = true;
                    }
                    changed
                }
                Key::c | Key::C if ctrl => st.copy_selected_to_clipboard(),
                Key::v | Key::V if ctrl && shift => {
                    let changed = st.paste_attributes_from_clipboard();
                    if changed {
                        notify_project = true;
                    }
                    changed
                }
                Key::v | Key::V if ctrl => {
                    let changed = st.paste_insert_from_clipboard();
                    if changed {
                        notify_project = true;
                    }
                    changed
                }
                Key::g | Key::G if ctrl && shift => {
                    let changed = st.ungroup_selected_clips();
                    if changed {
                        notify_project = true;
                    }
                    changed
                }
                Key::g | Key::G if ctrl => {
                    let changed = st.group_selected_clips();
                    if changed {
                        notify_project = true;
                    }
                    changed
                }
                Key::Delete | Key::BackSpace if shift => {
                    st.ripple_delete_selected();
                    notify_project = true;
                    true
                }
                Key::Delete | Key::BackSpace => {
                    st.delete_selected();
                    notify_project = true;
                    true
                }
                Key::space => {
                    let pp_cb = st.on_play_pause.clone();
                    drop(st);
                    if let Some(cb) = pp_cb {
                        cb();
                    }
                    if let Some(a) = area_weak.upgrade() {
                        a.queue_draw();
                    }
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
                Key::r | Key::R => {
                    // R = Ripple Edit
                    st.active_tool = if st.active_tool == ActiveTool::Ripple {
                        ActiveTool::Select
                    } else {
                        ActiveTool::Ripple
                    };
                    true
                }
                Key::e | Key::E => {
                    // E = Roll Edit
                    st.active_tool = if st.active_tool == ActiveTool::Roll {
                        ActiveTool::Select
                    } else {
                        ActiveTool::Roll
                    };
                    true
                }
                Key::y | Key::Y if !ctrl => {
                    // Y = Slip Edit
                    st.active_tool = if st.active_tool == ActiveTool::Slip {
                        ActiveTool::Select
                    } else {
                        ActiveTool::Slip
                    };
                    true
                }
                Key::u | Key::U if !ctrl => {
                    // U = Slide Edit
                    st.active_tool = if st.active_tool == ActiveTool::Slide {
                        ActiveTool::Select
                    } else {
                        ActiveTool::Slide
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

            let proj_cb = if notify_project {
                st.on_project_changed.clone()
            } else {
                None
            };
            let sel_cb = if notify_selection {
                st.on_clip_selected.clone()
            } else {
                None
            };
            let new_sel = st.selected_clip_id.clone();
            if handled {
                if let Some(a) = area_weak.upgrade() {
                    a.queue_draw();
                }
            }
            drop(st);
            if let Some(cb) = proj_cb {
                cb();
            }
            if let Some(cb) = sel_cb {
                cb(new_sel);
            }

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
            let ctrl_held = ctrl
                .current_event_state()
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
            if let Some(a) = area_weak.upgrade() {
                a.queue_draw();
            }
            glib::Propagation::Stop
        });
    }
    area.add_controller(scroll);

    // ── Drop target: accept clips dragged from media browser ────────────────
    {
        use gtk4::DropTarget;
        let drop_target = DropTarget::new(glib::Type::STRING, gdk4::DragAction::COPY);
        let state = state.clone();
        let area_weak = area.downgrade();
        {
            let state = state.clone();
            let area_weak = area_weak.clone();
            drop_target.connect_motion(move |_target, x, y| {
                let mut st = state.borrow_mut();
                let track_idx = if y > RULER_HEIGHT {
                    ((y - RULER_HEIGHT) / TRACK_HEIGHT) as usize
                } else {
                    usize::MAX
                };
                let tns = st.x_to_ns(x);
                let threshold_ns = ((12.0 / st.pixels_per_second) * NS_PER_SECOND as f64) as u64;
                let pair = {
                    let proj = st.project.borrow();
                    proj.tracks.get(track_idx).and_then(|track| {
                        let mut best: Option<(String, String, u64)> = None;
                        for i in 0..track.clips.len().saturating_sub(1) {
                            let left = &track.clips[i];
                            let right = &track.clips[i + 1];
                            let diff = left.timeline_end().abs_diff(tns);
                            if diff <= threshold_ns {
                                match best {
                                    Some((_, _, d)) if d <= diff => {}
                                    _ => best = Some((left.id.clone(), right.id.clone(), diff)),
                                }
                            }
                        }
                        best.map(|(l, r, _)| (l, r))
                    })
                };
                if st.hover_transition_pair != pair {
                    st.hover_transition_pair = pair;
                    if let Some(a) = area_weak.upgrade() {
                        a.queue_draw();
                    }
                }
                gdk4::DragAction::COPY
            });
        }
        {
            let state = state.clone();
            let area_weak = area_weak.clone();
            drop_target.connect_leave(move |_| {
                let mut st = state.borrow_mut();
                if st.hover_transition_pair.take().is_some() {
                    if let Some(a) = area_weak.upgrade() {
                        a.queue_draw();
                    }
                }
            });
        }
        drop_target.connect_drop(move |_target, value, x, y| {
            let payload = match value.get::<String>() {
                Ok(s) => s,
                Err(_) => return false,
            };
            if let Some(transition_kind) = payload.strip_prefix("transition:") {
                let mut st = state.borrow_mut();
                let track_idx = if y > RULER_HEIGHT {
                    ((y - RULER_HEIGHT) / TRACK_HEIGHT) as usize
                } else {
                    0
                };
                let tns = st.x_to_ns(x);
                let threshold_ns = ((12.0 / st.pixels_per_second) * NS_PER_SECOND as f64) as u64;
                let candidate = {
                    let proj = st.project.borrow();
                    let track = match proj.tracks.get(track_idx) {
                        Some(t) => t,
                        None => return false,
                    };
                    let mut best: Option<(String, String, String, u64)> = None;
                    let mut best_diff = u64::MAX;
                    for (i, clip) in track.clips.iter().enumerate() {
                        if i + 1 >= track.clips.len() {
                            continue;
                        }
                        let end = clip.timeline_end();
                        let diff = end.abs_diff(tns);
                        if diff <= threshold_ns && diff < best_diff {
                            best = Some((
                                track.id.clone(),
                                clip.id.clone(),
                                clip.transition_after.clone(),
                                clip.transition_after_ns,
                            ));
                            best_diff = diff;
                        }
                    }
                    best
                };
                if let Some((track_id, clip_id, old_transition, old_transition_ns)) = candidate {
                    let cmd = crate::undo::SetClipTransitionCommand {
                        clip_id,
                        track_id,
                        old_transition,
                        old_transition_ns,
                        new_transition: transition_kind.to_string(),
                        new_transition_ns: 500_000_000,
                    };
                    let project_rc = st.project.clone();
                    let mut proj = project_rc.borrow_mut();
                    st.history.execute(Box::new(cmd), &mut proj);
                    let cb = st.on_project_changed.clone();
                    st.hover_transition_pair = None;
                    drop(proj);
                    drop(st);
                    if let Some(cb) = cb {
                        cb();
                    }
                } else {
                    st.hover_transition_pair = None;
                    drop(st);
                }
            } else {
                // Payload format: "{source_path}|{duration_ns}"
                let mut parts = payload.splitn(2, '|');
                let source_path = match parts.next() {
                    Some(p) => p.to_string(),
                    None => return false,
                };
                let duration_ns: u64 = parts.next().and_then(|s| s.parse().ok()).unwrap_or(0);

                let (track_idx, timeline_start_ns) = {
                    let st = state.borrow();
                    let track_row_idx = if y > RULER_HEIGHT {
                        ((y - RULER_HEIGHT) / TRACK_HEIGHT) as usize
                    } else {
                        0
                    };
                    let tns = st.x_to_ns(x);
                    (track_row_idx, tns)
                };

                let cb = state.borrow().on_drop_clip.clone();
                if let Some(cb) = cb {
                    cb(source_path, duration_ns, track_idx, timeline_start_ns);
                }
                state.borrow_mut().hover_transition_pair = None;
            }
            if let Some(a) = area_weak.upgrade() {
                a.queue_draw();
            }
            true
        });
        area.add_controller(drop_target);
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
        draw_track_row(cr, w, y, i, track, st, cache, wcache);
    }

    // Playhead (clipped to content area so it doesn't overdraw track labels)
    let ph_x = st.ns_to_x(st.playhead_ns);
    cr.save().ok();
    cr.rectangle(TRACK_LABEL_WIDTH, 0.0, w - TRACK_LABEL_WIDTH, h);
    cr.clip();
    cr.set_source_rgb(1.0, 0.3, 0.3);
    cr.set_line_width(2.0);
    cr.move_to(ph_x, 0.0);
    cr.line_to(ph_x, h);
    cr.stroke().ok();
    cr.restore().ok();

    // Playhead triangle at top (also clipped)
    if ph_x >= TRACK_LABEL_WIDTH {
        cr.set_source_rgb(1.0, 0.3, 0.3);
        cr.move_to(ph_x - 6.0, 0.0);
        cr.line_to(ph_x + 6.0, 0.0);
        cr.line_to(ph_x, 12.0);
        cr.fill().ok();
    }

    // Tool indicator
    let tool_label = match st.active_tool {
        ActiveTool::Razor => Some("✂ Razor (B to toggle)"),
        ActiveTool::Slip => Some("↔ Slip (Y to toggle)"),
        ActiveTool::Slide => Some("⇔ Slide (U to toggle)"),
        _ => None,
    };
    if let Some(label) = tool_label {
        cr.set_source_rgb(1.0, 0.8, 0.0);
        cr.set_font_size(12.0);
        let _ = cr.move_to(TRACK_LABEL_WIDTH + 8.0, RULER_HEIGHT + 16.0);
        let _ = cr.show_text(label);
    }
    if st.magnetic_mode {
        cr.set_source_rgb(0.55, 0.95, 0.65);
        cr.set_font_size(12.0);
        let y = if tool_label.is_some() {
            RULER_HEIGHT + 32.0
        } else {
            RULER_HEIGHT + 16.0
        };
        let _ = cr.move_to(TRACK_LABEL_WIDTH + 8.0, y);
        let _ = cr.show_text("[Magnetic]");
    }

    // Track reorder drop indicator
    if let DragOp::ReorderTrack {
        track_idx,
        target_idx,
    } = &st.drag_op
    {
        if track_idx != target_idx {
            let indicator_y = RULER_HEIGHT
                + *target_idx as f64 * TRACK_HEIGHT
                + if target_idx > track_idx {
                    TRACK_HEIGHT
                } else {
                    0.0
                };
            cr.set_source_rgba(0.2, 0.7, 1.0, 0.9);
            cr.set_line_width(3.0);
            cr.move_to(0.0, indicator_y);
            cr.line_to(w, indicator_y);
            cr.stroke().ok();
        }
    }

    if let Some(m) = &st.marquee_selection {
        let left = m.start_x.min(m.current_x).max(TRACK_LABEL_WIDTH);
        let right = m.start_x.max(m.current_x);
        let top = m.start_y.min(m.current_y).max(RULER_HEIGHT);
        let bottom = m.start_y.max(m.current_y);
        if right > left && bottom > top {
            cr.set_source_rgba(0.25, 0.55, 1.0, 0.18);
            cr.rectangle(left, top, right - left, bottom - top);
            cr.fill().ok();
            cr.set_source_rgba(0.45, 0.75, 1.0, 0.95);
            cr.set_line_width(1.5);
            cr.rectangle(left, top, right - left, bottom - top);
            cr.stroke().ok();
        }
    }
}

fn compact_gap_free_clips(clips: &mut Vec<Clip>) {
    clips.sort_by_key(|c| c.timeline_start);
    let mut cursor = 0_u64;
    for clip in clips.iter_mut() {
        clip.timeline_start = cursor;
        cursor = clip.timeline_end();
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
    let major_tick_interval = choose_tick_interval(st.pixels_per_second);
    let major_tick_px = major_tick_interval * st.pixels_per_second;
    let subdivisions = choose_tick_subdivisions(major_tick_px);
    let tick_interval = major_tick_interval / subdivisions as f64;
    let first_tick = (start_sec / tick_interval).floor() * tick_interval;
    let show_mid_labels = subdivisions >= 4 && (major_tick_px / 2.0) >= 70.0;
    let midpoint_step = (subdivisions / 2).max(1) as i64;

    let mut t = first_tick;
    let mut tick_index = (first_tick / tick_interval).round() as i64;
    while t <= start_sec + visible_secs + tick_interval {
        let x = TRACK_LABEL_WIDTH + (t - start_sec) * st.pixels_per_second;
        if x >= TRACK_LABEL_WIDTH && x <= width {
            let major_mod = tick_index.rem_euclid(subdivisions as i64);
            let is_major = major_mod == 0;
            let is_mid = !is_major && show_mid_labels && major_mod.rem_euclid(midpoint_step) == 0;
            let tick_height = if is_major {
                8.0
            } else if is_mid {
                6.0
            } else {
                4.0
            };
            cr.move_to(x, RULER_HEIGHT - tick_height);
            cr.line_to(x, RULER_HEIGHT);
            cr.stroke().ok();
            if is_major || is_mid {
                let label_interval = if is_major {
                    major_tick_interval
                } else {
                    major_tick_interval / 2.0
                };
                let label = format_timecode(t, label_interval);
                let _ = cr.move_to(x + 2.0, RULER_HEIGHT - 10.0);
                let _ = cr.show_text(&label);
            }
        }
        t += tick_interval;
        tick_index += 1;
    }

    // Draw timeline markers (chapter points)
    {
        let proj = st.project.borrow();
        cr.set_font_size(9.0);
        for marker in &proj.markers {
            let mx = st.ns_to_x(marker.position_ns);
            if mx < TRACK_LABEL_WIDTH || mx > width {
                continue;
            }
            let r = ((marker.color >> 24) & 0xFF) as f64 / 255.0;
            let g = ((marker.color >> 16) & 0xFF) as f64 / 255.0;
            let b = ((marker.color >> 8) & 0xFF) as f64 / 255.0;
            cr.set_source_rgb(r, g, b);
            // Triangle pointing down from ruler top
            cr.move_to(mx, 2.0);
            cr.line_to(mx - 5.0, 12.0);
            cr.line_to(mx + 5.0, 12.0);
            cr.close_path();
            cr.fill().ok();
            // Vertical line through ruler
            cr.set_line_width(1.0);
            cr.move_to(mx, 12.0);
            cr.line_to(mx, RULER_HEIGHT);
            cr.stroke().ok();
            // Label
            if !marker.label.is_empty() {
                cr.set_source_rgba(r, g, b, 0.9);
                let _ = cr.move_to(mx + 3.0, RULER_HEIGHT - 2.0);
                let _ = cr.show_text(&marker.label);
            }
        }
    }

    cr.set_source_rgb(0.25, 0.25, 0.28);
    cr.rectangle(0.0, 0.0, TRACK_LABEL_WIDTH, RULER_HEIGHT);
    cr.fill().ok();
}

fn draw_track_row(
    cr: &gtk::cairo::Context,
    width: f64,
    y: f64,
    track_idx: usize,
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
    cr.rectangle(
        TRACK_LABEL_WIDTH,
        y,
        width - TRACK_LABEL_WIDTH,
        TRACK_HEIGHT,
    );
    cr.fill().ok();

    // Draw clips first (they may overlap into label column before clipping)
    for clip in &track.clips {
        draw_clip(cr, width, y, clip, track, st, cache, wcache);
    }

    // Draw transition markers (clip -> next clip) after clip bodies.
    for clip in &track.clips {
        if clip.transition_after_ns > 0 && !clip.transition_after.is_empty() {
            let ex = st.ns_to_x(clip.timeline_end());
            let marker_w = 10.0;
            cr.set_source_rgba(0.85, 0.85, 1.0, 0.75);
            cr.rectangle(ex - marker_w / 2.0, y + 4.0, marker_w, TRACK_HEIGHT - 8.0);
            cr.fill().ok();
        }
    }

    // Draw label column on top so it stays visible when timeline is scrolled
    let is_active = st.selected_track_id.as_deref() == Some(&track.id);
    if is_active {
        cr.set_source_rgb(0.28, 0.28, 0.32);
    } else {
        cr.set_source_rgb(0.22, 0.22, 0.25);
    }
    cr.rectangle(0.0, y, TRACK_LABEL_WIDTH, TRACK_HEIGHT);
    cr.fill().ok();

    // Active track accent bar
    if is_active {
        cr.set_source_rgb(0.3, 0.55, 0.95);
        cr.rectangle(0.0, y, 3.0, TRACK_HEIGHT);
        cr.fill().ok();
    }

    cr.set_source_rgb(0.8, 0.8, 0.8);
    cr.set_font_size(11.0);
    let _ = cr.move_to(6.0, y + TRACK_HEIGHT / 2.0 + 4.0);
    let _ = cr.show_text(&track.label);

    if st.show_track_audio_levels {
        let meter_x = TRACK_LABEL_WIDTH - TRACK_LABEL_METER_WIDTH - 6.0;
        let meter_y = y + 8.0;
        let meter_h = TRACK_HEIGHT - 16.0;
        let [left_db, right_db] = st
            .track_audio_peak_db
            .get(track_idx)
            .copied()
            .unwrap_or([-60.0, -60.0]);
        draw_track_label_meter(
            cr,
            meter_x,
            meter_y,
            TRACK_LABEL_METER_WIDTH,
            meter_h,
            left_db,
            right_db,
        );
    }

    cr.set_source_rgb(0.1, 0.1, 0.12);
    cr.set_line_width(1.0);
    cr.move_to(0.0, y + TRACK_HEIGHT);
    cr.line_to(width, y + TRACK_HEIGHT);
    cr.stroke().ok();
}

fn draw_track_label_meter(
    cr: &gtk::cairo::Context,
    x: f64,
    y: f64,
    width: f64,
    height: f64,
    left_db: f64,
    right_db: f64,
) {
    let db_to_frac = |db: f64| -> f64 { ((db + 60.0) / 60.0).clamp(0.0, 1.0) };
    cr.set_source_rgb(0.12, 0.12, 0.14);
    cr.rectangle(x, y, width, height);
    cr.fill().ok();

    let gap = 2.0;
    let bar_w = ((width - gap) / 2.0).max(2.0);
    for (ch, db) in [(0usize, left_db), (1usize, right_db)] {
        let bx = x + ch as f64 * (bar_w + gap);
        let frac = db_to_frac(db);
        let bar_h = frac * height;
        let top = y + (height - bar_h);

        let green_frac = db_to_frac(-18.0);
        let green_h = (green_frac * height).min(bar_h);
        if green_h > 0.0 {
            cr.set_source_rgb(0.2, 0.8, 0.2);
            cr.rectangle(bx, y + height - green_h, bar_w, green_h);
            cr.fill().ok();
        }

        let yellow_frac = db_to_frac(-6.0);
        let yellow_h = ((yellow_frac - green_frac) * height).min((bar_h - green_h).max(0.0));
        if yellow_h > 0.0 {
            cr.set_source_rgb(0.9, 0.85, 0.1);
            cr.rectangle(
                bx,
                y + height - green_frac * height - yellow_h,
                bar_w,
                yellow_h,
            );
            cr.fill().ok();
        }

        let red_h = (bar_h - yellow_frac * height).max(0.0);
        if red_h > 0.0 {
            cr.set_source_rgb(0.9, 0.2, 0.1);
            cr.rectangle(bx, top, bar_w, red_h);
            cr.fill().ok();
        }
    }
}

fn draw_clip(
    cr: &gtk::cairo::Context,
    view_width: f64,
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

    if cx + cw < TRACK_LABEL_WIDTH || cx > view_width {
        return;
    }

    let is_selected = st.is_clip_selected(&clip.id);
    let is_transition_hover = st
        .hover_transition_pair
        .as_ref()
        .map(|(l, r)| clip.id == *l || clip.id == *r)
        .unwrap_or(false);

    let (r, g, b) = match track.kind {
        TrackKind::Video => (0.17, 0.47, 0.85),
        TrackKind::Audio => (0.18, 0.65, 0.45),
    };
    cr.set_source_rgb(r, g, b);
    rounded_rect(cr, cx, cy, cw.max(4.0), ch, 4.0);
    cr.fill().ok();

    // ── Thumbnail strip for video clips ──────────────────────────────────
    if track.kind == TrackKind::Video && cw > 20.0 {
        const THUMB_ASPECT: f64 = 160.0 / 90.0;
        const MAX_THUMB_TILES_PER_CLIP: usize = 6;
        const MAX_NEW_THUMB_REQUESTS_PER_CLIP_PER_DRAW: usize = 2;

        let inner_x = cx + 1.0;
        let inner_y = cy + 1.0;
        let inner_w = (cw - 2.0).max(0.0);
        let inner_h = (ch - 2.0).max(0.0);

        if inner_w > 1.0 && inner_h > 1.0 {
            let src_span = clip.source_out.saturating_sub(clip.source_in);
            let scale_y = inner_h / 90.0;

            cr.save().ok();
            rounded_rect(cr, inner_x, inner_y, inner_w, inner_h, 3.0);
            cr.clip();

            if st.show_timeline_preview {
                let nominal_tile_w = (inner_h * THUMB_ASPECT).max(1.0);
                let approx_tiles = (inner_w / nominal_tile_w).ceil().max(1.0) as usize;
                let tile_count = approx_tiles.min(MAX_THUMB_TILES_PER_CLIP).max(1);
                let mut requested_this_draw = 0usize;

                for i in 0..tile_count {
                    let f0 = i as f64 / tile_count as f64;
                    let f1 = (i + 1) as f64 / tile_count as f64;
                    let x0 = inner_x + f0 * inner_w;
                    let x1 = inner_x + f1 * inner_w;
                    let draw_w = (x1 - x0).max(1.0);
                    let mid = (f0 + f1) * 0.5;

                    let src_offset = if src_span <= 1 {
                        0
                    } else {
                        ((mid * src_span as f64) as u64).min(src_span - 1)
                    };
                    let sample_time = clip.source_in + src_offset;

                    if let Some(surf) = cache.get(&clip.source_path, sample_time) {
                        cr.save().ok();
                        cr.rectangle(x0, inner_y, draw_w, inner_h);
                        cr.clip();
                        cr.translate(x0, inner_y);
                        cr.scale(draw_w / 160.0, scale_y);
                        cr.set_source_surface(surf, 0.0, 0.0).ok();
                        cr.paint_with_alpha(0.75).ok();
                        cr.restore().ok();
                    } else if requested_this_draw < MAX_NEW_THUMB_REQUESTS_PER_CLIP_PER_DRAW {
                        cache.request(&clip.source_path, sample_time);
                        requested_this_draw += 1;
                    }
                }
            } else {
                let draw_w = ((inner_h * THUMB_ASPECT).max(1.0)).min((inner_w * 0.5).max(1.0));
                let mut requested_this_draw = 0usize;
                let start_time = clip.source_in;
                let end_time = clip.source_out.saturating_sub(1).max(clip.source_in);
                let endpoints = [
                    (inner_x, start_time),
                    (inner_x + inner_w - draw_w, end_time),
                ];
                for (x0, sample_time) in endpoints {
                    if let Some(surf) = cache.get(&clip.source_path, sample_time) {
                        cr.save().ok();
                        cr.rectangle(x0, inner_y, draw_w, inner_h);
                        cr.clip();
                        cr.translate(x0, inner_y);
                        cr.scale(draw_w / 160.0, scale_y);
                        cr.set_source_surface(surf, 0.0, 0.0).ok();
                        cr.paint_with_alpha(0.75).ok();
                        cr.restore().ok();
                    } else if requested_this_draw < MAX_NEW_THUMB_REQUESTS_PER_CLIP_PER_DRAW {
                        cache.request(&clip.source_path, sample_time);
                        requested_this_draw += 1;
                    }
                }
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
        // Only compute peaks for the visible portion of the clip to avoid
        // allocating/iterating over tens of thousands of off-screen pixels.
        let vis_x0 = cx.max(TRACK_LABEL_WIDTH);
        let vis_x1 = (cx + cw).min(view_width);
        let vis_px = (vis_x1 - vis_x0).ceil().max(0.0) as usize;
        if vis_px > 0 {
            let src_span_ns = clip.source_out.saturating_sub(clip.source_in) as f64;
            let frac0 = ((vis_x0 - cx) / cw).clamp(0.0, 1.0);
            let frac1 = ((vis_x1 - cx) / cw).clamp(0.0, 1.0);
            let vis_src_in = clip.source_in + (frac0 * src_span_ns) as u64;
            let vis_src_out = clip.source_in + (frac1 * src_span_ns) as u64;
            if let Some(peaks) =
                wcache.get_peaks(&clip.source_path, vis_src_in, vis_src_out, vis_px)
            {
                cr.save().ok();
                rounded_rect(cr, cx + 1.0, cy + 1.0, cw - 2.0, ch - 2.0, 3.0);
                cr.clip();
                let mid = cy + ch / 2.0;
                cr.set_line_width(1.0);
                let vol = (clip.volume as f64).max(0.0);
                draw_waveform_batched(cr, &peaks, vis_x0, mid, ch / 2.0 - 2.0, vol, 0.85);
                cr.restore().ok();
            }
        }
    }

    // ── Waveform overlay for video clips (when preference enabled) ────────
    if track.kind == TrackKind::Video && st.show_waveform_on_video && cw > 8.0 {
        wcache.request(&clip.source_path);
        let vis_x0 = cx.max(TRACK_LABEL_WIDTH);
        let vis_x1 = (cx + cw).min(view_width);
        let vis_px = (vis_x1 - vis_x0).ceil().max(0.0) as usize;
        if vis_px > 0 {
            let src_span_ns = clip.source_out.saturating_sub(clip.source_in) as f64;
            let frac0 = ((vis_x0 - cx) / cw).clamp(0.0, 1.0);
            let frac1 = ((vis_x1 - cx) / cw).clamp(0.0, 1.0);
            let vis_src_in = clip.source_in + (frac0 * src_span_ns) as u64;
            let vis_src_out = clip.source_in + (frac1 * src_span_ns) as u64;
            if let Some(peaks) =
                wcache.get_peaks(&clip.source_path, vis_src_in, vis_src_out, vis_px)
            {
                let wave_h = (ch * 0.40).max(6.0);
                let wave_y = cy + ch - wave_h - 1.0;
                cr.save().ok();
                rounded_rect(cr, cx + 1.0, wave_y, cw - 2.0, wave_h, 2.0);
                cr.clip();
                cr.set_source_rgba(0.0, 0.0, 0.0, 0.45);
                cr.paint().ok();
                cr.set_line_width(1.0);
                let wave_mid = wave_y + wave_h / 2.0;
                let vol = (clip.volume as f64).max(0.0);
                draw_waveform_batched(cr, &peaks, vis_x0, wave_mid, wave_h / 2.0 - 1.0, vol, 0.9);
                cr.restore().ok();
            }
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
    } else if is_transition_hover {
        cr.set_source_rgba(0.55, 0.85, 1.0, 0.95);
        cr.set_line_width(2.0);
        rounded_rect(cr, cx, cy, cw.max(4.0), ch, 4.0);
        cr.stroke().ok();
    }

    if cw > 30.0 {
        cr.set_source_rgb(1.0, 1.0, 1.0);
        cr.set_font_size(11.0);
        let _ = cr.move_to(cx + 6.0, cy + ch / 2.0 + 4.0);
        let _ = cr.show_text(&clip.label);

        // Speed badge: show e.g. "2×" or "0.5×" when speed ≠ 1.0, and "◀" when reversed
        let has_speed_badge = (clip.speed - 1.0).abs() > 0.01 || clip.reverse;
        if has_speed_badge && cw > 60.0 {
            let badge = if clip.reverse {
                if (clip.speed - 1.0).abs() > 0.01 {
                    let speed_str = if clip.speed == clip.speed.floor() {
                        format!("{}×", clip.speed as u32)
                    } else {
                        format!("{:.2}×", clip.speed)
                    };
                    format!("◀ {speed_str}")
                } else {
                    "◀".to_string()
                }
            } else if clip.speed == clip.speed.floor() {
                format!("{}×", clip.speed as u32)
            } else {
                format!("{:.2}×", clip.speed)
            };
            cr.set_font_size(10.0);
            if let Ok(ext) = cr.text_extents(&badge) {
                let bx = cx + cw - ext.width() - 6.0;
                let by = cy + 14.0;
                // Badge background
                cr.set_source_rgba(0.0, 0.0, 0.0, 0.55);
                rounded_rect(cr, bx - 2.0, by - 11.0, ext.width() + 4.0, 14.0, 2.0);
                cr.fill().ok();
                cr.set_source_rgb(1.0, 0.9, 0.2);
                let _ = cr.move_to(bx, by);
                let _ = cr.show_text(&badge);
            }
        }

        // LUT badge: small "LUT" indicator when a LUT file is assigned
        if clip
            .lut_path
            .as_ref()
            .map(|p| !p.is_empty())
            .unwrap_or(false)
            && cw > 80.0
        {
            let badge = "LUT";
            cr.set_font_size(10.0);
            if let Ok(ext) = cr.text_extents(badge) {
                // Place to the left of the speed/reverse badge (or at the right edge)
                let speed_offset = if has_speed_badge { 36.0 } else { 0.0 };
                let bx = cx + cw - ext.width() - 6.0 - speed_offset;
                let by = cy + 14.0;
                cr.set_source_rgba(0.0, 0.0, 0.0, 0.55);
                rounded_rect(cr, bx - 2.0, by - 11.0, ext.width() + 4.0, 14.0, 2.0);
                cr.fill().ok();
                cr.set_source_rgb(0.4, 0.8, 1.0);
                let _ = cr.move_to(bx, by);
                let _ = cr.show_text(badge);
            }
        }
    }
}

fn rounded_rect(cr: &gtk::cairo::Context, x: f64, y: f64, w: f64, h: f64, r: f64) {
    cr.new_sub_path();
    cr.arc(
        x + r,
        y + r,
        r,
        std::f64::consts::PI,
        3.0 * std::f64::consts::PI / 2.0,
    );
    cr.arc(x + w - r, y + r, r, 3.0 * std::f64::consts::PI / 2.0, 0.0);
    cr.arc(x + w - r, y + h - r, r, 0.0, std::f64::consts::PI / 2.0);
    cr.arc(
        x + r,
        y + h - r,
        r,
        std::f64::consts::PI / 2.0,
        std::f64::consts::PI,
    );
    cr.close_path();
}

/// Map a normalized peak amplitude (0.0–1.0) to an RGB color for waveform display.
/// Zones mirror the VU meter: green (quiet), yellow (moderate), red (loud).
/// Draw a waveform using batched strokes grouped by color band.
///
/// Instead of issuing one `stroke()` per pixel (thousands of calls), this groups
/// consecutive lines by their color band (green / yellow / red) and draws each
/// band as a single Cairo path.  Reduces per-clip waveform draw from O(n)
/// stroke ops to exactly 3.
fn draw_waveform_batched(
    cr: &gtk::cairo::Context,
    peaks: &[f32],
    cx: f64,
    mid_y: f64,
    half_range: f64,
    vol: f64,
    alpha: f64,
) {
    // Color bands: (threshold, r, g, b)
    const BANDS: [(f64, f64, f64, f64); 3] = [
        (0.0, 0.30, 0.90, 0.40),   // green  — quiet  (< −18 dBFS)
        (0.126, 0.95, 0.85, 0.15), // yellow — moderate
        (0.5, 0.95, 0.25, 0.15),   // red    — loud   (≥ −6 dBFS)
    ];

    // Classify each peak into a band and compute its geometry.
    // We build paths per-band to minimize stroke() calls.
    struct Line {
        x: f64,
        y1: f64,
        y2: f64,
    }
    let mut green: Vec<Line> = Vec::new();
    let mut yellow: Vec<Line> = Vec::new();
    let mut red: Vec<Line> = Vec::new();

    for (i, &peak) in peaks.iter().enumerate() {
        let scaled = (peak as f64 * vol).clamp(0.0, 1.0);
        let half_h = (scaled * half_range).max(1.0);
        let x = cx + i as f64 + 0.5;
        let line = Line {
            x,
            y1: mid_y - half_h,
            y2: mid_y + half_h,
        };
        if scaled >= 0.5 {
            red.push(line);
        } else if scaled >= 0.126 {
            yellow.push(line);
        } else {
            green.push(line);
        }
    }

    for (lines, band_idx) in [(&green, 0usize), (&yellow, 1), (&red, 2)] {
        if lines.is_empty() {
            continue;
        }
        let (_, r, g, b) = BANDS[band_idx];
        cr.set_source_rgba(r, g, b, alpha);
        for line in lines {
            cr.move_to(line.x, line.y1);
            cr.line_to(line.x, line.y2);
        }
        cr.stroke().ok();
    }
}

fn waveform_color(peak: f64) -> (f64, f64, f64) {
    if peak >= 0.5 {
        (0.95, 0.25, 0.15) // red  — loud (≥ −6 dBFS)
    } else if peak >= 0.126 {
        (0.95, 0.85, 0.15) // yellow — moderate (−18 to −6 dBFS)
    } else {
        (0.30, 0.90, 0.40) // green — quiet (< −18 dBFS)
    }
}

fn choose_tick_interval(pixels_per_second: f64) -> f64 {
    let target_px = 80.0;
    let raw = target_px / pixels_per_second;
    for &nice in &[0.1, 0.5, 1.0, 5.0, 10.0, 30.0, 60.0, 300.0, 600.0] {
        if raw <= nice {
            return nice;
        }
    }
    600.0
}

fn choose_tick_subdivisions(major_tick_px: f64) -> usize {
    if major_tick_px >= 320.0 {
        8
    } else if major_tick_px >= 160.0 {
        4
    } else if major_tick_px >= 90.0 {
        2
    } else {
        1
    }
}

fn format_timecode(secs: f64, label_interval: f64) -> String {
    let total_ms = (secs.max(0.0) * 1000.0).round() as u64;
    let h = total_ms / 3_600_000;
    let m = (total_ms % 3_600_000) / 60_000;
    let s = (total_ms % 60_000) / 1000;
    let ms = total_ms % 1000;

    if label_interval >= 1.0 {
        if h > 0 {
            format!("{h}:{m:02}:{s:02}")
        } else {
            format!("{m}:{s:02}")
        }
    } else if label_interval >= 0.1 {
        let tenths = ms / 100;
        if h > 0 {
            format!("{h}:{m:02}:{s:02}.{tenths}")
        } else {
            format!("{m}:{s:02}.{tenths}")
        }
    } else {
        let hundredths = ms / 10;
        if h > 0 {
            format!("{h}:{m:02}:{s:02}.{hundredths:02}")
        } else {
            format!("{m}:{s:02}.{hundredths:02}")
        }
    }
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
        ("Space", "Play / Pause"),
        ("I", "Set In-point"),
        ("O", "Set Out-point"),
        ("J", "Shuttle reverse (1×/2×/4×)"),
        ("K", "Stop shuttle / Pause"),
        ("L", "Shuttle forward (1×/2×/4×)"),
        ("← / →", "Step one frame back / forward"),
        (
            "Arrow keys (Transform Overlay)",
            "Nudge selected clip position (0.01)",
        ),
        (
            "Shift+Arrow (Transform Overlay)",
            "Nudge selected clip position (0.1)",
        ),
        (
            "+ / - (Transform Overlay)",
            "Increase / decrease selected clip scale",
        ),
        ("B", "Toggle Razor (Blade) tool"),
        ("R", "Toggle Ripple edit tool"),
        ("E", "Toggle Roll edit tool"),
        ("Y", "Toggle Slip edit tool"),
        ("U", "Toggle Slide edit tool"),
        ("Escape", "Switch to Select tool"),
        ("Delete / Bksp", "Delete selected clip(s)"),
        ("Shift+Delete / Shift+Bksp", "Ripple delete selected clip(s)"),
        ("Shift+Click (Timeline)", "Range-select clips on the same track"),
        ("Ctrl/Cmd+Click (Timeline)", "Toggle clip in current selection"),
        ("Ctrl+A", "Select all timeline clips"),
        ("Drag empty timeline body", "Marquee-select intersecting clips"),
        ("M", "Add marker at playhead"),
        ("Right-click ruler", "Remove nearest marker"),
        ("Right-click transition", "Remove transition at boundary"),
        ("Ctrl+,", "Open Preferences"),
        ("Ctrl+Z", "Undo"),
        ("Ctrl+Y / Ctrl+Shift+Z", "Redo"),
        ("Ctrl+C", "Copy selected timeline clip"),
        ("Ctrl+V", "Paste insert clip at playhead"),
        ("Ctrl+Shift+V", "Paste copied clip attributes"),
        ("Ctrl+G", "Group selected clips"),
        ("Ctrl+Shift+G", "Ungroup selected clips"),
        ("Scroll", "Zoom timeline (vertical scroll)"),
        ("Scroll (H)", "Pan timeline (horizontal scroll)"),
        ("? / /", "Show this help"),
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
