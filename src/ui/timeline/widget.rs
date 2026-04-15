use crate::media::ltc::LtcChannelSelection;
use crate::model::clip::{Clip, ClipKind, KeyframeInterpolation};
use crate::model::project::{FrameRate, Project};
use crate::model::through_edit::detect_track_through_edit_boundaries;
use crate::model::track::TrackKind;
use crate::model::transition::{
    validate_track_transition_request, OutgoingTransition, TransitionAlignment,
    DEFAULT_TRANSITION_DURATION_NS,
};
use crate::undo::{
    EditHistory, JoinThroughEditCommand, MoveClipCommand, ReorderTrackCommand,
    SetMultipleTracksClipsCommand, SetTrackClipsCommand, SplitClipCommand, TrackClipsChange,
    TrimClipCommand, TrimOutCommand,
};
use glib;
use gtk4::prelude::*;
use gtk4::{
    self as gtk, DrawingArea, EventControllerKey, EventControllerScroll, GestureClick, GestureDrag,
};
use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;

const TRACK_HEIGHT: f64 = 60.0;
const TRACK_HEIGHT_SMALL: f64 = 44.0;
const TRACK_HEIGHT_LARGE: f64 = 84.0;
const TRACK_LABEL_WIDTH: f64 = 140.0;
const TRACK_LABEL_METER_WIDTH: f64 = 18.0;
const TRACK_LABEL_SOLO_BADGE_WIDTH: f64 = 16.0;
const TRACK_LABEL_SOLO_BADGE_HEIGHT: f64 = 14.0;
/// Badge constants reused for Mute and Lock badges (same size as Solo/Duck).
const TRACK_LABEL_BADGE_WIDTH: f64 = 16.0;
const TRACK_LABEL_BADGE_HEIGHT: f64 = 14.0;
const RULER_HEIGHT: f64 = 24.0;
const PIXELS_PER_SECOND_DEFAULT: f64 = 100.0;
use crate::units::NS_PER_SECOND_F as NS_PER_SECOND;
/// Pixels from clip edge that activate trim mode
const TRIM_HANDLE_PX: f64 = 10.0;
/// Snap tolerance (in pixels) used by clip drag/trim to lock onto neighbouring
/// clip edges and the playhead. Converted to nanoseconds via the current
/// `pixels_per_second` zoom level at each call site.
const SNAP_TOLERANCE_PX: f64 = 10.0;
/// Font size for the timeline ruler tick labels.
const RULER_FONT_SIZE: f64 = 10.0;
/// Font size for marker labels drawn on the ruler.
const MARKER_FONT_SIZE: f64 = 9.0;
/// Lower clamp for the dynamic clip-label font size (8 px is the smallest size
/// that stays legible when track height shrinks).
const TRACK_LABEL_FONT_SIZE_MIN: f64 = 8.0;
/// Upper clamp for the dynamic clip-label font size (above ~16 px the label
/// looks oversized vs. the surrounding chrome).
const TRACK_LABEL_FONT_SIZE_MAX: f64 = 16.0;
const MUSIC_GEN_MIN_DURATION_NS: u64 = crate::units::NS_PER_SECOND;
const MUSIC_GEN_MAX_DURATION_NS: u64 = 30 * MUSIC_GEN_MIN_DURATION_NS;

fn track_row_height(track: &crate::model::track::Track) -> f64 {
    match track.height_preset {
        crate::model::track::TrackHeightPreset::Small => TRACK_HEIGHT_SMALL,
        crate::model::track::TrackHeightPreset::Medium => TRACK_HEIGHT,
        crate::model::track::TrackHeightPreset::Large => TRACK_HEIGHT_LARGE,
    }
}

/// Returns logical track indices in top-to-bottom visual order.
///
/// The professional NLE convention: video tracks stack upward (higher-numbered
/// video tracks visually above lower-numbered), and audio tracks stack downward
/// (higher-numbered audio tracks visually below). The boundary between video
/// and audio sits in the middle of the timeline.
///
/// Concretely: video tracks are emitted in *reverse* of their logical order,
/// then audio tracks in their normal logical order. Relative order within
/// each kind is preserved, even when the underlying vector is interleaved.
fn visual_order(tracks: &[crate::model::track::Track]) -> Vec<usize> {
    let mut video: Vec<usize> = tracks
        .iter()
        .enumerate()
        .filter(|(_, t)| t.is_video())
        .map(|(i, _)| i)
        .collect();
    video.reverse();
    let audio: Vec<usize> = tracks
        .iter()
        .enumerate()
        .filter(|(_, t)| t.is_audio())
        .map(|(i, _)| i)
        .collect();
    let mut out = Vec::with_capacity(tracks.len());
    out.extend(video);
    out.extend(audio);
    out
}

/// Position of `logical_idx` in the visual (top-to-bottom) stack, or `None`
/// if `logical_idx` is out of bounds.
fn logical_to_visual(tracks: &[crate::model::track::Track], logical_idx: usize) -> Option<usize> {
    visual_order(tracks).iter().position(|&i| i == logical_idx)
}

fn track_row_top(project: &crate::model::project::Project, track_idx: usize) -> f64 {
    track_row_top_in_tracks(&project.tracks, track_idx)
}

fn timeline_content_height(project: &crate::model::project::Project) -> f64 {
    project.tracks.iter().map(track_row_height).sum::<f64>()
}

fn timeline_content_height_for_tracks(tracks: &[crate::model::track::Track]) -> f64 {
    tracks.iter().map(track_row_height).sum::<f64>()
}

fn ruler_hit_test(st: &TimelineState, y: f64) -> bool {
    let _ = st;
    y >= 0.0 && y < RULER_HEIGHT
}

fn track_row_top_in_tracks(tracks: &[crate::model::track::Track], track_idx: usize) -> f64 {
    let order = visual_order(tracks);
    let visual_pos = order.iter().position(|&i| i == track_idx);
    let mut y = 0.0;
    if let Some(vp) = visual_pos {
        for &i in order.iter().take(vp) {
            y += track_row_height(&tracks[i]);
        }
    }
    y
}

fn track_index_at_y_in_tracks(tracks: &[crate::model::track::Track], y: f64) -> Option<usize> {
    if y < 0.0 {
        return None;
    }
    let mut row_top = 0.0;
    for &logical_idx in &visual_order(tracks) {
        let h = track_row_height(&tracks[logical_idx]);
        if y >= row_top && y < row_top + h {
            return Some(logical_idx);
        }
        row_top += h;
    }
    None
}

fn track_index_at_y_in_project(project: &crate::model::project::Project, y: f64) -> Option<usize> {
    track_index_at_y_in_tracks(&project.tracks, y)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActiveTool {
    Select,
    Razor,
    Ripple,
    Roll,
    Slip,
    Slide,
    /// Vector drawing tool for ClipKind::Drawing.
    Draw,
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
    /// Moving one or more keyframe time-columns (all lanes at selected times) on a clip.
    MoveKeyframeColumns {
        clip_id: String,
        track_id: String,
        original_track_clips: Vec<Clip>,
        original_selected_local_times: Vec<u64>,
        anchor_local_ns: u64,
    },
}

#[derive(Debug, Clone)]
struct TimelineClipboard {
    clip: Clip,
    source_track_id: String,
}

/// Clipboard holding only color-grading static values (no keyframes).
#[derive(Debug, Clone)]
pub struct ColorGradeClipboard {
    pub brightness: f32,
    pub contrast: f32,
    pub saturation: f32,
    pub temperature: f32,
    pub tint: f32,
    pub exposure: f32,
    pub black_point: f32,
    pub shadows: f32,
    pub midtones: f32,
    pub highlights: f32,
    pub highlights_warmth: f32,
    pub highlights_tint: f32,
    pub midtones_warmth: f32,
    pub midtones_tint: f32,
    pub shadows_warmth: f32,
    pub shadows_tint: f32,
    pub denoise: f32,
    pub sharpness: f32,
    pub blur: f32,
    pub lut_paths: Vec<String>,
}

impl ColorGradeClipboard {
    /// Extract color grading values from a clip.
    pub fn from_clip(clip: &Clip) -> Self {
        Self {
            brightness: clip.brightness,
            contrast: clip.contrast,
            saturation: clip.saturation,
            temperature: clip.temperature,
            tint: clip.tint,
            exposure: clip.exposure,
            black_point: clip.black_point,
            shadows: clip.shadows,
            midtones: clip.midtones,
            highlights: clip.highlights,
            highlights_warmth: clip.highlights_warmth,
            highlights_tint: clip.highlights_tint,
            midtones_warmth: clip.midtones_warmth,
            midtones_tint: clip.midtones_tint,
            shadows_warmth: clip.shadows_warmth,
            shadows_tint: clip.shadows_tint,
            denoise: clip.denoise,
            sharpness: clip.sharpness,
            blur: clip.blur,
            lut_paths: clip.lut_paths.clone(),
        }
    }

    /// Apply color grading values to a target clip. Returns true if anything changed.
    pub fn apply_to(&self, target: &mut Clip) -> bool {
        let before = target.clone();
        target.brightness = self.brightness;
        target.contrast = self.contrast;
        target.saturation = self.saturation;
        target.temperature = self.temperature;
        target.tint = self.tint;
        target.exposure = self.exposure;
        target.black_point = self.black_point;
        target.shadows = self.shadows;
        target.midtones = self.midtones;
        target.highlights = self.highlights;
        target.highlights_warmth = self.highlights_warmth;
        target.highlights_tint = self.highlights_tint;
        target.midtones_warmth = self.midtones_warmth;
        target.midtones_tint = self.midtones_tint;
        target.shadows_warmth = self.shadows_warmth;
        target.shadows_tint = self.shadows_tint;
        target.denoise = self.denoise;
        target.sharpness = self.sharpness;
        target.blur = self.blur;
        target.lut_paths = self.lut_paths.clone();
        before != *target
    }
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

#[derive(Debug, Clone)]
struct KeyframeMarqueeSelection {
    clip_id: String,
    track_id: String,
    start_local_ns: u64,
    current_local_ns: u64,
    additive: bool,
    base_times: HashSet<u64>,
}

#[derive(Debug, Clone)]
pub struct MusicGenerationTarget {
    pub track_id: String,
    pub timeline_start_ns: u64,
    pub timeline_end_ns: Option<u64>,
}

impl MusicGenerationTarget {
    pub fn requested_duration_ns(&self) -> Option<u64> {
        self.timeline_end_ns
            .map(|end| end.saturating_sub(self.timeline_start_ns))
    }
}

#[derive(Debug, Clone)]
struct MusicGenerationRegionDraft {
    track_id: String,
    start_ns: u64,
    current_ns: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum MusicGenerationOverlayStatus {
    Pending,
    Failed,
}

#[derive(Debug, Clone)]
struct MusicGenerationOverlay {
    job_id: String,
    track_id: String,
    start_ns: u64,
    end_ns: u64,
    status: MusicGenerationOverlayStatus,
    error: Option<String>,
}

/// Where an in-progress drag or trim is currently snapping to. Rendered as a
/// vertical guideline + badge in `draw_timeline`.
#[derive(Clone, Debug)]
pub struct SnapHit {
    /// Timeline position of the snap line, in root-timeline ns (matches the
    /// coordinate space `ns_to_x` expects).
    pub position_ns: u64,
    /// Short label rendered in the badge.
    pub label: &'static str,
}

/// Pick the nearest candidate to `desired_ns` within `snap_ns` tolerance.
/// Returns the (possibly adjusted) ns value and the snap hit when one fired.
fn snap_to_candidates(
    desired_ns: i64,
    snap_ns: i64,
    candidates: &[(u64, &'static str)],
) -> (i64, Option<SnapHit>) {
    let mut best: Option<(u64, &'static str, i64)> = None;
    for &(cand, label) in candidates {
        let delta = (cand as i64 - desired_ns).abs();
        if delta < snap_ns {
            match best {
                Some((_, _, best_abs)) if best_abs <= delta => {}
                _ => best = Some((cand, label, delta)),
            }
        }
    }
    match best {
        Some((cand, label, _)) => (
            cand as i64,
            Some(SnapHit {
                position_ns: cand,
                label,
            }),
        ),
        None => (desired_ns, None),
    }
}

/// Shared state for the timeline widget
pub struct TimelineState {
    pub project: Rc<RefCell<Project>>,
    pub history: EditHistory,
    pub active_tool: ActiveTool,
    pub pixels_per_second: f64,
    pub scroll_offset: f64,
    pub vertical_scroll_offset: f64,
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
    /// Called when files are dropped from an external file manager: (file_paths, track_idx, timeline_start_ns)
    pub on_drop_external_files: Option<Rc<dyn Fn(Vec<String>, usize, u64)>>,
    /// Lightweight callback fired immediately when clip selection changes (no pipeline rebuild).
    /// Called with the new selected_clip_id (or None if deselected).
    pub on_clip_selected: Option<Rc<dyn Fn(Option<String>)>>,
    /// Callback fired when user requests audio sync: Vec<(clip_id, source_path, source_in, source_out, timeline_start, track_id)>
    pub on_sync_audio: Option<Rc<dyn Fn(Vec<(String, String, u64, u64, u64, String)>)>>,
    /// Callback fired when user requests sync & replace audio (same args as on_sync_audio).
    pub on_sync_replace_audio: Option<Rc<dyn Fn(Vec<(String, String, u64, u64, u64, String)>)>>,
    /// Callback fired when user requests silence removal: (clip_id, track_id, source_path, source_in, source_out, noise_db, min_duration)
    pub on_remove_silent_parts: Option<Rc<dyn Fn(String, String, String, u64, u64, f64, f64)>>,
    pub on_detect_scene_cuts: Option<Rc<dyn Fn(String, String, String, u64, u64, f64)>>,
    pub on_convert_ltc_to_timecode:
        Option<Rc<dyn Fn(String, LtcChannelSelection, Option<FrameRate>)>>,
    pub on_generate_music: Option<Rc<dyn Fn(MusicGenerationTarget)>>,
    /// Lightweight status callback for the MusicGen region workflow.
    pub on_music_generation_status: Option<Rc<dyn Fn(String)>>,
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
    /// Color-grade-only clipboard for copy/paste color grading between clips.
    pub color_grade_clipboard: Option<ColorGradeClipboard>,
    /// Multi-selection set (primary selection remains in `selected_clip_id`).
    selected_clip_ids: HashSet<String>,
    /// Anchor clip used for Shift+click range selection.
    selection_anchor_clip_id: Option<String>,
    /// Active marquee-selection drag state.
    marquee_selection: Option<MarqueeSelection>,
    /// Active keyframe-column selection keyed by clip id.
    selected_keyframe_local_times: HashMap<String, HashSet<u64>>,
    /// Active keyframe-column marquee selection drag state.
    keyframe_marquee_selection: Option<KeyframeMarqueeSelection>,
    /// Track id currently armed for one-shot MusicGen region drawing.
    music_generation_armed_track_id: Option<String>,
    /// Active in-progress MusicGen region drag.
    music_generation_region_draft: Option<MusicGenerationRegionDraft>,
    /// Inline timeline overlays for pending/failed music generation jobs.
    music_generation_overlays: Vec<MusicGenerationOverlay>,
    /// Callback fired when the active tool changes (via keyboard shortcut).
    pub on_tool_changed: Option<Rc<dyn Fn(ActiveTool)>>,
    /// Set of source paths currently resolved as missing/offline.
    pub missing_media_paths: HashSet<String>,
    /// Callback fired when user presses the match-color shortcut (Ctrl+Alt+M).
    pub on_match_color: Option<Rc<dyn Fn()>>,
    /// Callback fired when user presses the match-frame shortcut (F).
    pub on_match_frame: Option<Rc<dyn Fn()>>,
    /// Callback fired to create a multicam clip (triggers audio sync in background).
    /// Args: Vec<(clip_id, source_path, source_in, source_out, timeline_start, track_id)>
    pub on_create_multicam: Option<Rc<dyn Fn(Vec<(String, String, u64, u64, u64, String)>)>>,
    /// Navigation stack for compound clip drill-down editing.
    /// Empty = editing root project timeline.
    /// Each entry is the clip ID of a compound clip being edited.
    pub compound_nav_stack: Vec<String>,
    /// Saved scroll offset from root level, restored on exit_compound.
    pub compound_saved_scroll: Option<f64>,
    /// Current snap target while a drag/trim is active. Cleared on drag end
    /// and on any motion frame that did not produce a snap. Drives the snap
    /// guideline + badge overlay in `draw_timeline`.
    pub active_snap_hit: Option<SnapHit>,
    /// How the timeline view follows the playhead during playback.
    pub timeline_autoscroll: crate::ui_state::AutoScrollMode,
    /// While set to a future `Instant`, auto-scroll is suspended so the user's
    /// manual scroll/pan wins. Bumped whenever the user scrolls or pan-drags.
    pub user_scroll_cooldown_until: Option<std::time::Instant>,
    /// Weak reference to the minimap DrawingArea (if wired).
    /// Used to queue_draw the minimap whenever the main timeline repaints.
    pub minimap_widget: Option<glib::object::WeakRef<DrawingArea>>,
}

impl TimelineState {
    pub fn new(project: Rc<RefCell<Project>>) -> Self {
        Self {
            project,
            history: EditHistory::new(),
            active_tool: ActiveTool::Select,
            pixels_per_second: PIXELS_PER_SECOND_DEFAULT,
            scroll_offset: 0.0,
            vertical_scroll_offset: 0.0,
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
            on_drop_external_files: None,
            on_clip_selected: None,
            on_sync_audio: None,
            on_sync_replace_audio: None,
            on_remove_silent_parts: None,
            on_detect_scene_cuts: None,
            on_convert_ltc_to_timecode: None,
            on_generate_music: None,
            on_music_generation_status: None,
            magnetic_mode: false,
            hover_transition_pair: None,
            show_waveform_on_video: false,
            show_timeline_preview: true,
            loading: false,
            track_audio_peak_db: Vec::new(),
            show_track_audio_levels: true,
            clipboard: None,
            color_grade_clipboard: None,
            selected_clip_ids: HashSet::new(),
            selection_anchor_clip_id: None,
            marquee_selection: None,
            selected_keyframe_local_times: HashMap::new(),
            keyframe_marquee_selection: None,
            music_generation_armed_track_id: None,
            music_generation_region_draft: None,
            music_generation_overlays: Vec::new(),
            on_tool_changed: None,
            missing_media_paths: HashSet::new(),
            on_match_color: None,
            on_match_frame: None,
            on_create_multicam: None,
            compound_nav_stack: Vec::new(),
            compound_saved_scroll: None,
            timeline_autoscroll: crate::ui_state::AutoScrollMode::default(),
            user_scroll_cooldown_until: None,
            active_snap_hit: None,
            minimap_widget: None,
        }
    }

    /// Adjust `scroll_offset` so the playhead stays visible during playback.
    ///
    /// Call from the playback tick with the widget's allocated width. Suppressed
    /// while the user is mid-drag or inside the short cooldown after a manual
    /// scroll/pan so the view does not fight user input.
    pub fn apply_playhead_autoscroll(&mut self, viewport_width: f64) {
        use crate::ui_state::AutoScrollMode;
        if self.timeline_autoscroll == AutoScrollMode::Off {
            return;
        }
        if !matches!(self.drag_op, DragOp::None) {
            return;
        }
        if let Some(t) = self.user_scroll_cooldown_until {
            if std::time::Instant::now() < t {
                return;
            }
            self.user_scroll_cooldown_until = None;
        }
        let usable_w = (viewport_width - TRACK_LABEL_WIDTH).max(100.0);
        let ph_abs =
            (self.playhead_ns as f64 / NS_PER_SECOND) * self.pixels_per_second + TRACK_LABEL_WIDTH;
        let ph_x = ph_abs - self.scroll_offset;
        match self.timeline_autoscroll {
            AutoScrollMode::Page => {
                let right_edge = TRACK_LABEL_WIDTH + usable_w;
                let margin = 40.0;
                if ph_x > right_edge - margin || ph_x < TRACK_LABEL_WIDTH {
                    let target_local_x = TRACK_LABEL_WIDTH + usable_w * 0.125;
                    self.scroll_offset = (ph_abs - target_local_x).max(0.0);
                }
            }
            AutoScrollMode::Smooth => {
                let target_local_x = TRACK_LABEL_WIDTH + usable_w * 0.75;
                let delta = ph_x - target_local_x;
                if delta > 0.0 {
                    let step = delta.min(usable_w * 0.5);
                    self.scroll_offset = (self.scroll_offset + step).max(0.0);
                } else if ph_x < TRACK_LABEL_WIDTH {
                    let target_local_x = TRACK_LABEL_WIDTH + usable_w * 0.125;
                    self.scroll_offset = (ph_abs - target_local_x).max(0.0);
                }
            }
            AutoScrollMode::Off => {}
        }
    }

    pub fn source_is_missing(&self, source_path: &str) -> bool {
        self.missing_media_paths.contains(source_path)
    }

    pub fn ns_to_x(&self, ns: u64) -> f64 {
        TRACK_LABEL_WIDTH + (ns as f64 / NS_PER_SECOND) * self.pixels_per_second
            - self.scroll_offset
    }

    pub fn x_to_ns(&self, x: f64) -> u64 {
        let secs = (x - TRACK_LABEL_WIDTH + self.scroll_offset) / self.pixels_per_second;
        (secs.max(0.0) * NS_PER_SECOND) as u64
    }

    /// Fire `on_project_changed` with no `TimelineState` borrow held.
    ///
    /// **Why this exists:** GTK4 callbacks run inside `extern "C"` trampolines
    /// that cannot unwind, so a panic — including a `RefCell` double-borrow
    /// from re-entering `state.borrow()` while a `borrow_mut()` is still
    /// active — is a hard process abort. The `on_project_changed` closure
    /// (defined in `window.rs`) re-borrows the same `Rc<RefCell<TimelineState>>`
    /// to read `selected_clip_id`, so calling it while a `borrow_mut()` is
    /// live is fatal.
    ///
    /// This helper does the borrow → clone → drop → call dance atomically:
    /// it takes a *shared reference* to the `Rc` (so no caller borrow is
    /// required), opens a brief shared `borrow()` to clone the `Rc<dyn Fn()>`
    /// callback, drops the borrow before invoking the closure, and is a
    /// no-op when the callback is unset.
    ///
    /// **Calling rule:** the caller must release any outstanding
    /// `borrow_mut()` (e.g. via `drop(st)`) **before** calling this helper.
    /// See `docs/ARCHITECTURE.md` "Critical Rules for GTK4 + RefCell".
    pub fn notify_project_changed(state: &Rc<RefCell<Self>>) {
        let cb = state.borrow().on_project_changed.clone();
        if let Some(cb) = cb {
            cb();
        }
    }

    fn track_index_at_y(&self, y: f64) -> Option<usize> {
        let project = self.project.borrow();
        let editing_tracks = self.resolve_editing_tracks(&project);
        // Convert widget y to content y accounting for vertical scroll
        track_index_at_y_in_tracks(
            editing_tracks,
            y + self.vertical_scroll_offset - self.breadcrumb_bar_height(),
        )
    }

    pub fn arm_music_generation_region(&mut self, track_id: String) {
        self.selected_track_id = Some(track_id.clone());
        self.music_generation_armed_track_id = Some(track_id);
        self.music_generation_region_draft = None;
        self.clear_failed_music_generation_overlays();
    }

    pub fn cancel_music_generation_region(&mut self) -> bool {
        let had_draft = self.music_generation_region_draft.take().is_some();
        let had_arm = self.music_generation_armed_track_id.take().is_some();
        had_draft || had_arm
    }

    fn begin_music_generation_region_drag(&mut self, x: f64, y: f64) -> bool {
        if x < TRACK_LABEL_WIDTH || self.hit_test(x, y).is_some() {
            return false;
        }
        let Some(armed_track_id) = self.music_generation_armed_track_id.clone() else {
            return false;
        };
        let Some(track_idx) = self.track_index_at_y(y) else {
            return false;
        };
        let proj = self.project.borrow();
        let editing_tracks = self.resolve_editing_tracks(&proj);
        let Some(track) = editing_tracks.get(track_idx) else {
            return false;
        };
        if track.kind != TrackKind::Audio || track.id != armed_track_id {
            return false;
        }
        let start_ns = self.x_to_ns(x);
        self.selected_track_id = Some(track.id.clone());
        self.music_generation_region_draft = Some(MusicGenerationRegionDraft {
            track_id: track.id.clone(),
            start_ns,
            current_ns: start_ns,
        });
        true
    }

    fn update_music_generation_region_drag(&mut self, current_x: f64) -> bool {
        let current_ns = self.x_to_ns(current_x);
        if let Some(ref mut draft) = self.music_generation_region_draft {
            draft.current_ns = current_ns;
            true
        } else {
            false
        }
    }

    fn music_generation_region_overlaps_clips(
        &self,
        track_id: &str,
        start_ns: u64,
        end_ns: u64,
    ) -> bool {
        let proj = self.project.borrow();
        let Some(track) = proj.track_ref(track_id) else {
            return true;
        };
        track
            .clips
            .iter()
            .any(|clip| start_ns < clip.timeline_end() && end_ns > clip.timeline_start)
    }

    fn finish_music_generation_region_drag(
        &mut self,
    ) -> Option<Result<MusicGenerationTarget, String>> {
        let draft = self.music_generation_region_draft.take()?;
        let start_ns = draft.start_ns.min(draft.current_ns);
        let end_ns = draft.start_ns.max(draft.current_ns);
        let duration_ns = end_ns.saturating_sub(start_ns);
        if duration_ns < MUSIC_GEN_MIN_DURATION_NS {
            return Some(Err(
                "MusicGen regions must be at least 1 second long.".to_string()
            ));
        }
        if duration_ns > MUSIC_GEN_MAX_DURATION_NS {
            return Some(Err(
                "MusicGen regions currently support up to 30 seconds.".to_string()
            ));
        }
        if self.music_generation_region_overlaps_clips(&draft.track_id, start_ns, end_ns) {
            return Some(Err(
                "Music region must stay in empty audio-track space.".to_string()
            ));
        }
        self.music_generation_armed_track_id = None;
        Some(Ok(MusicGenerationTarget {
            track_id: draft.track_id,
            timeline_start_ns: start_ns,
            timeline_end_ns: Some(end_ns),
        }))
    }

    pub fn clear_failed_music_generation_overlays(&mut self) -> bool {
        let before = self.music_generation_overlays.len();
        self.music_generation_overlays
            .retain(|overlay| overlay.status != MusicGenerationOverlayStatus::Failed);
        before != self.music_generation_overlays.len()
    }

    pub fn add_pending_music_generation_overlay(
        &mut self,
        job_id: String,
        track_id: String,
        start_ns: u64,
        end_ns: u64,
    ) {
        if end_ns <= start_ns {
            return;
        }
        self.clear_failed_music_generation_overlays();
        self.music_generation_overlays
            .retain(|overlay| overlay.job_id != job_id);
        self.music_generation_overlays.push(MusicGenerationOverlay {
            job_id,
            track_id,
            start_ns,
            end_ns,
            status: MusicGenerationOverlayStatus::Pending,
            error: None,
        });
    }

    pub fn resolve_music_generation_overlay_success(&mut self, job_id: &str) -> bool {
        let before = self.music_generation_overlays.len();
        self.music_generation_overlays
            .retain(|overlay| overlay.job_id != job_id);
        before != self.music_generation_overlays.len()
    }

    pub fn mark_music_generation_overlay_failed(&mut self, job_id: &str, error: String) -> bool {
        if let Some(overlay) = self
            .music_generation_overlays
            .iter_mut()
            .find(|overlay| overlay.job_id == job_id)
        {
            overlay.status = MusicGenerationOverlayStatus::Failed;
            overlay.error = Some(error);
            true
        } else {
            false
        }
    }

    fn solo_badge_hit_track_index(&self, x: f64, y: f64) -> Option<usize> {
        let track_idx = self.track_index_at_y(y)?;
        let project = self.project.borrow();
        let editing_tracks = self.resolve_editing_tracks(&project);
        let row_y =
            track_row_top_in_tracks(editing_tracks, track_idx) + self.breadcrumb_bar_height();
        let badge_x = track_label_solo_badge_x(self.show_track_audio_levels);
        let badge_y = row_y + 6.0;
        let content_y = y + self.vertical_scroll_offset;
        if x >= badge_x
            && x <= badge_x + TRACK_LABEL_SOLO_BADGE_WIDTH
            && content_y >= badge_y
            && content_y <= badge_y + TRACK_LABEL_SOLO_BADGE_HEIGHT
        {
            Some(track_idx)
        } else {
            None
        }
    }

    fn duck_badge_hit_track_index(&self, x: f64, y: f64) -> Option<usize> {
        let track_idx = self.track_index_at_y(y)?;
        let project = self.project.borrow();
        let editing_tracks = self.resolve_editing_tracks(&project);
        let track = editing_tracks.get(track_idx)?;
        if track.kind != TrackKind::Audio {
            return None;
        }
        let row_y =
            track_row_top_in_tracks(editing_tracks, track_idx) + self.breadcrumb_bar_height();
        let badge_x = track_label_duck_badge_x(self.show_track_audio_levels);
        let badge_y = row_y + 6.0;
        let content_y = y + self.vertical_scroll_offset;
        if x >= badge_x
            && x <= badge_x + TRACK_LABEL_BADGE_WIDTH
            && content_y >= badge_y
            && content_y <= badge_y + TRACK_LABEL_BADGE_HEIGHT
        {
            Some(track_idx)
        } else {
            None
        }
    }

    fn toggle_track_duck_by_index(&mut self, track_idx: usize) -> bool {
        let (track_id, old_duck) = {
            let proj = self.project.borrow();
            let Some(track) = proj.tracks.get(track_idx) else {
                return false;
            };
            (track.id.clone(), track.duck)
        };
        let mut proj = self.project.borrow_mut();
        self.history.execute(
            Box::new(crate::undo::set_track_duck_cmd(
                track_id, old_duck, !old_duck,
            )),
            &mut proj,
        );
        true
    }

    fn mute_badge_hit_track_index(&self, x: f64, y: f64) -> Option<usize> {
        let track_idx = self.track_index_at_y(y)?;
        let project = self.project.borrow();
        let editing_tracks = self.resolve_editing_tracks(&project);
        let row_y =
            track_row_top_in_tracks(editing_tracks, track_idx) + self.breadcrumb_bar_height();
        let badge_x = track_label_mute_badge_x(self.show_track_audio_levels);
        let badge_y = row_y + 6.0;
        let content_y = y + self.vertical_scroll_offset;
        if x >= badge_x
            && x <= badge_x + TRACK_LABEL_BADGE_WIDTH
            && content_y >= badge_y
            && content_y <= badge_y + TRACK_LABEL_BADGE_HEIGHT
        {
            Some(track_idx)
        } else {
            None
        }
    }

    fn lock_badge_hit_track_index(&self, x: f64, y: f64) -> Option<usize> {
        let track_idx = self.track_index_at_y(y)?;
        let project = self.project.borrow();
        let editing_tracks = self.resolve_editing_tracks(&project);
        let row_y =
            track_row_top_in_tracks(editing_tracks, track_idx) + self.breadcrumb_bar_height();
        let badge_x = track_label_lock_badge_x(self.show_track_audio_levels);
        let badge_y = row_y + 6.0;
        let content_y = y + self.vertical_scroll_offset;
        if x >= badge_x
            && x <= badge_x + TRACK_LABEL_BADGE_WIDTH
            && content_y >= badge_y
            && content_y <= badge_y + TRACK_LABEL_BADGE_HEIGHT
        {
            Some(track_idx)
        } else {
            None
        }
    }

    fn toggle_track_mute_by_index(&mut self, track_idx: usize) -> bool {
        let (track_id, old_muted) = {
            let proj = self.project.borrow();
            let Some(track) = proj.tracks.get(track_idx) else {
                return false;
            };
            (track.id.clone(), track.muted)
        };
        let mut proj = self.project.borrow_mut();
        self.history.execute(
            Box::new(crate::undo::set_track_muted_cmd(
                track_id, old_muted, !old_muted,
            )),
            &mut proj,
        );
        true
    }

    fn toggle_track_lock_by_index(&mut self, track_idx: usize) -> bool {
        let (track_id, old_locked) = {
            let proj = self.project.borrow();
            let Some(track) = proj.tracks.get(track_idx) else {
                return false;
            };
            (track.id.clone(), track.locked)
        };
        let mut proj = self.project.borrow_mut();
        self.history.execute(
            Box::new(crate::undo::set_track_locked_cmd(
                track_id,
                old_locked,
                !old_locked,
            )),
            &mut proj,
        );
        true
    }

    fn toggle_selected_track_mute(&mut self) -> bool {
        let Some(track_id) = self.selected_track_id.clone() else {
            return false;
        };
        let old_muted = {
            let proj = self.project.borrow();
            let Some(track) = proj.tracks.iter().find(|t| t.id == track_id) else {
                return false;
            };
            track.muted
        };
        let mut proj = self.project.borrow_mut();
        self.history.execute(
            Box::new(crate::undo::set_track_muted_cmd(
                track_id, old_muted, !old_muted,
            )),
            &mut proj,
        );
        true
    }

    fn toggle_selected_track_lock(&mut self) -> bool {
        let Some(track_id) = self.selected_track_id.clone() else {
            return false;
        };
        let old_locked = {
            let proj = self.project.borrow();
            let Some(track) = proj.tracks.iter().find(|t| t.id == track_id) else {
                return false;
            };
            track.locked
        };
        let mut proj = self.project.borrow_mut();
        self.history.execute(
            Box::new(crate::undo::set_track_locked_cmd(
                track_id,
                old_locked,
                !old_locked,
            )),
            &mut proj,
        );
        true
    }

    fn set_track_color_label_by_index(
        &mut self,
        track_idx: usize,
        color: crate::model::track::TrackColorLabel,
    ) -> bool {
        let (track_id, old_color) = {
            let proj = self.project.borrow();
            let Some(track) = proj.tracks.get(track_idx) else {
                return false;
            };
            (track.id.clone(), track.color_label)
        };
        if old_color == color {
            return false;
        }
        let mut proj = self.project.borrow_mut();
        self.history.execute(
            Box::new(crate::undo::set_track_color_label_cmd(
                track_id, old_color, color,
            )),
            &mut proj,
        );
        true
    }

    fn toggle_track_solo_by_index(&mut self, track_idx: usize) -> bool {
        let (track_id, old_solo) = {
            let proj = self.project.borrow();
            let Some(track) = proj.tracks.get(track_idx) else {
                return false;
            };
            (track.id.clone(), track.soloed)
        };
        self.selected_track_id = Some(track_id.clone());
        let mut proj = self.project.borrow_mut();
        self.history.execute(
            Box::new(crate::undo::set_track_solo_cmd(
                track_id, old_solo, !old_solo,
            )),
            &mut proj,
        );
        true
    }

    fn toggle_selected_track_solo(&mut self) -> bool {
        let Some(track_id) = self.selected_track_id.clone() else {
            return false;
        };
        let old_solo = {
            let proj = self.project.borrow();
            let Some(track) = proj.tracks.iter().find(|t| t.id == track_id) else {
                return false;
            };
            track.soloed
        };
        let mut proj = self.project.borrow_mut();
        self.history.execute(
            Box::new(crate::undo::set_track_solo_cmd(
                track_id, old_solo, !old_solo,
            )),
            &mut proj,
        );
        true
    }

    /// Rename a track by ID. Returns true if the label actually changed
    /// (after trimming whitespace). Pushed through the undo history so
    /// Ctrl+Z reverts the rename.
    fn rename_track(&mut self, track_id: &str, new_label: String) -> bool {
        let new_label = new_label.trim().to_string();
        if new_label.is_empty() {
            return false;
        }
        let old_label = {
            let proj = self.project.borrow();
            let Some(track) = proj.track_ref(track_id) else {
                return false;
            };
            track.label.clone()
        };
        if old_label == new_label {
            return false;
        }
        self.selected_track_id = Some(track_id.to_string());
        let mut proj = self.project.borrow_mut();
        self.history.execute(
            Box::new(crate::undo::set_track_label_cmd(
                track_id.to_string(),
                old_label,
                new_label,
            )),
            &mut proj,
        );
        true
    }

    fn set_track_height_preset_by_index(
        &mut self,
        track_idx: usize,
        preset: crate::model::track::TrackHeightPreset,
    ) -> bool {
        let mut proj = self.project.borrow_mut();
        let Some(track) = proj.tracks.get_mut(track_idx) else {
            return false;
        };
        if track.height_preset == preset {
            return false;
        }
        track.height_preset = preset;
        self.selected_track_id = Some(track.id.clone());
        proj.dirty = true;
        true
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
        self.delete_selected_internal(
            self.magnetic_mode,
            if self.magnetic_mode {
                "Delete clip(s) (magnetic)"
            } else {
                "Delete clip(s)"
            },
        );
    }

    pub fn ripple_delete_selected(&mut self) {
        self.delete_selected_internal(true, "Ripple delete");
    }

    /// Delete a clip-local word range from a clip's transcript and ripple-shift
    /// any clips that started after the original clip's right edge.
    ///
    /// `word_start_ns` and `word_end_ns` are in clip-local 1× time (0 = the
    /// clip's `source_in`, matching how `SubtitleWord::start_ns` is stored).
    /// The method:
    ///
    /// 1. Splits the source clip into a left half ending at `word_start_ns`
    ///    and a right half starting at `word_end_ns`, rebasing keyframes and
    ///    subtitles on each side via `retain_*_in_local_range`.
    /// 2. Slides the right half left so it sits flush against the left half
    ///    (closing the deleted span).
    /// 3. Subtracts the deleted timeline span (`(word_end - word_start) /
    ///    speed`) from every other clip on the same track whose
    ///    `timeline_start` was at or after the original clip's `timeline_end`,
    ///    leaving intentional gaps elsewhere on the track untouched.
    /// 4. Commits the new clip list as a single `SetTrackClipsCommand` so the
    ///    whole edit is one undo entry.
    ///
    /// Compound clips work transparently: `project.find_track_id_for_clip` and
    /// `project.track_mut` walk into nested `compound_tracks`.
    ///
    /// Returns `true` if a change was applied. The caller is responsible for
    /// invoking `TimelineState::notify_project_changed` after dropping its
    /// borrow (matching the pattern used by `ripple_delete_selected`).
    pub fn delete_transcript_word_range(
        &mut self,
        clip_id: &str,
        word_start_ns: u64,
        word_end_ns: u64,
    ) -> bool {
        if word_end_ns <= word_start_ns {
            return false;
        }
        let mut proj = self.project.borrow_mut();
        let Some(track_id) = proj.find_track_id_for_clip(clip_id) else {
            return false;
        };
        // Read the original clip + full clip list inside a scoped block so the
        // immutable borrow of `proj` is released before we hand `&mut proj` to
        // `history.execute` below.
        let (original, original_pos, old_clips) = {
            let Some(track) = proj.track_ref(&track_id) else {
                return false;
            };
            let Some(pos) = track.clips.iter().position(|c| c.id == clip_id) else {
                return false;
            };
            let original = track.clips[pos].clone();
            let old_clips = track.clips.clone();
            (original, pos, old_clips)
        };

        // Local 1× duration (the upper bound of subtitle/keyframe times).
        let original_local_duration = original.source_out.saturating_sub(original.source_in);
        // Clamp the requested word range to the clip's actual local duration.
        let word_start_ns = word_start_ns.min(original_local_duration);
        let word_end_ns = word_end_ns.min(original_local_duration);
        if word_end_ns <= word_start_ns {
            return false;
        }

        // Speed-aware timeline offsets. `clip.duration()` divides by speed for
        // us, but we need the offsets of arbitrary local times, not just the
        // full duration, so we apply the same `/ speed` ourselves. Guard
        // against zero/negative speed (shouldn't happen but stay defensive).
        let speed = if original.speed > 0.0 {
            original.speed
        } else {
            1.0
        };
        let cut_a_local_timeline = (word_start_ns as f64 / speed as f64) as u64;
        let cut_b_local_timeline = (word_end_ns as f64 / speed as f64) as u64;
        let deleted_timeline_span = cut_b_local_timeline.saturating_sub(cut_a_local_timeline);
        if deleted_timeline_span == 0 {
            return false;
        }

        let original_timeline_end = original.timeline_end();

        // Build the left half — ends exactly where the deletion begins.
        let mut left = original.clone();
        left.source_out = original.source_in.saturating_add(word_start_ns);
        left.retain_subtitles_in_local_range(0, word_start_ns);
        left.retain_keyframes_in_local_range(0, word_start_ns);

        // Build the right half — fresh id, sliced source window, and
        // pre-shifted timeline_start so it sits immediately after `left`.
        let mut right = original.clone();
        right.id = uuid::Uuid::new_v4().to_string();
        right.source_in = original.source_in.saturating_add(word_end_ns);
        right.timeline_start = original.timeline_start.saturating_add(cut_a_local_timeline);
        right.retain_subtitles_in_local_range(word_end_ns, original_local_duration);
        right.retain_keyframes_in_local_range(word_end_ns, original_local_duration);

        // Walk the existing clip list, replace the original with [left,
        // right], and ripple-shift any clip that started at or after the
        // original's timeline_end. Other clips (including any positioned
        // before the original) are left untouched.
        let mut new_clips: Vec<Clip> = Vec::with_capacity(old_clips.len() + 1);
        for (idx, c) in old_clips.iter().enumerate() {
            if idx == original_pos {
                // Skip clips with no remaining content on either side. The
                // most common path keeps both, but a delete that covers the
                // entire clip should still produce a valid edit (drops the
                // clip outright).
                if left.source_out > left.source_in {
                    new_clips.push(left.clone());
                }
                if right.source_out > right.source_in {
                    new_clips.push(right.clone());
                }
            } else if c.timeline_start >= original_timeline_end {
                let mut moved = c.clone();
                moved.timeline_start = c.timeline_start.saturating_sub(deleted_timeline_span);
                new_clips.push(moved);
            } else {
                new_clips.push(c.clone());
            }
        }

        if new_clips == old_clips {
            return false;
        }

        let cmd = SetTrackClipsCommand {
            track_id: track_id.clone(),
            old_clips,
            new_clips,
            label: "Delete transcript range".to_string(),
        };
        self.history.execute(Box::new(cmd), &mut proj);
        true
    }

    fn delete_selected_internal(&mut self, compact: bool, label: &str) {
        let Some(_primary_clip_id) = self.selected_clip_id.clone() else {
            return;
        };
        let mut target_ids = self.selected_ids_or_primary();
        target_ids = self.expand_with_related_members(&target_ids);
        if target_ids.is_empty() {
            return;
        }
        let track_updates = {
            let proj = self.project.borrow();
            let editing_tracks = self.resolve_editing_tracks(&proj);
            editing_tracks
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
            let editing_tracks = self.resolve_editing_tracks(&proj);
            editing_tracks
                .iter()
                .filter_map(|t| {
                    let old_clips = t.clips.clone();
                    let mut new_clips = old_clips.clone();
                    let mut changed = false;
                    for clip in &mut new_clips {
                        if target_ids.contains(&clip.id)
                            && clip.group_id.as_deref() != Some(&group_id)
                        {
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
            let editing_tracks = self.resolve_editing_tracks(&proj);
            editing_tracks
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
            let editing_tracks = self.resolve_editing_tracks(&proj);
            editing_tracks
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

    pub fn link_selected_clips(&mut self) -> bool {
        let target_ids = self.selected_ids_or_primary();
        if target_ids.len() < 2 {
            return false;
        }
        let link_group_id = uuid::Uuid::new_v4().to_string();
        let track_updates = {
            let proj = self.project.borrow();
            let editing_tracks = self.resolve_editing_tracks(&proj);
            editing_tracks
                .iter()
                .filter_map(|t| {
                    let old_clips = t.clips.clone();
                    let mut new_clips = old_clips.clone();
                    let mut changed = false;
                    for clip in &mut new_clips {
                        if target_ids.contains(&clip.id)
                            && clip.link_group_id.as_deref() != Some(&link_group_id)
                        {
                            clip.link_group_id = Some(link_group_id.clone());
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
                label: "Link clips".to_string(),
            };
            self.history.execute(Box::new(cmd), &mut proj);
        }
        true
    }

    pub fn unlink_selected_clips(&mut self) -> bool {
        let target_ids = self.selected_ids_or_primary();
        if target_ids.is_empty() {
            return false;
        }
        let target_link_groups: HashSet<String> = {
            let proj = self.project.borrow();
            let editing_tracks = self.resolve_editing_tracks(&proj);
            editing_tracks
                .iter()
                .flat_map(|t| t.clips.iter())
                .filter(|c| target_ids.contains(&c.id))
                .filter_map(|c| c.link_group_id.clone())
                .collect()
        };
        if target_link_groups.is_empty() {
            return false;
        }
        let track_updates = {
            let proj = self.project.borrow();
            let editing_tracks = self.resolve_editing_tracks(&proj);
            editing_tracks
                .iter()
                .filter_map(|t| {
                    let old_clips = t.clips.clone();
                    let mut new_clips = old_clips.clone();
                    let mut changed = false;
                    for clip in &mut new_clips {
                        if clip
                            .link_group_id
                            .as_deref()
                            .is_some_and(|gid| target_link_groups.contains(gid))
                        {
                            clip.link_group_id = None;
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
                label: "Unlink clips".to_string(),
            };
            self.history.execute(Box::new(cmd), &mut proj);
        }
        true
    }

    fn selected_group_ids(&self) -> HashSet<String> {
        let target_ids = self.selected_ids_or_primary();
        if target_ids.is_empty() {
            return HashSet::new();
        }
        let proj = self.project.borrow();
        let editing_tracks = self.resolve_editing_tracks(&proj);
        editing_tracks
            .iter()
            .flat_map(|t| t.clips.iter())
            .filter(|c| target_ids.contains(&c.id))
            .filter_map(|c| c.group_id.clone())
            .collect()
    }

    pub fn align_selected_groups_by_timecode(&mut self) -> bool {
        let target_groups = self.selected_group_ids();
        if target_groups.is_empty() {
            return false;
        }

        let assignments = {
            let proj = self.project.borrow();
            let primary_selected = self.selected_clip_id.as_deref();
            let mut assignments: HashMap<String, u64> = HashMap::new();

            for group_id in &target_groups {
                let members: Vec<_> = proj
                    .tracks
                    .iter()
                    .flat_map(|track| track.clips.iter())
                    .filter(|clip| clip.group_id.as_deref() == Some(group_id.as_str()))
                    .filter_map(|clip| {
                        clip.source_timecode_start_ns()
                            .map(|source_timecode_start_ns| {
                                (
                                    clip.id.clone(),
                                    clip.timeline_start,
                                    source_timecode_start_ns,
                                )
                            })
                    })
                    .collect();

                if members.len() < 2 {
                    continue;
                }

                let anchor = members
                    .iter()
                    .find(|(clip_id, _, _)| Some(clip_id.as_str()) == primary_selected)
                    .or_else(|| {
                        members.iter().min_by_key(
                            |(_, timeline_start, source_timecode_start_ns)| {
                                (*source_timecode_start_ns, *timeline_start)
                            },
                        )
                    });
                let Some((_, anchor_timeline_start, anchor_source_timecode_start_ns)) = anchor
                else {
                    continue;
                };

                let mut proposed: Vec<(String, i128)> = members
                    .iter()
                    .map(|(clip_id, _, source_timecode_start_ns)| {
                        (
                            clip_id.clone(),
                            i128::from(*anchor_timeline_start)
                                + i128::from(*source_timecode_start_ns)
                                - i128::from(*anchor_source_timecode_start_ns),
                        )
                    })
                    .collect();

                if let Some(min_start) = proposed.iter().map(|(_, start)| *start).min() {
                    if min_start < 0 {
                        let shift = -min_start;
                        for (_, start) in &mut proposed {
                            *start += shift;
                        }
                    }
                }

                for (clip_id, start) in proposed {
                    assignments.insert(clip_id, start.max(0) as u64);
                }
            }

            assignments
        };

        if assignments.is_empty() {
            return false;
        }

        let track_updates = {
            let proj = self.project.borrow();
            let editing_tracks = self.resolve_editing_tracks(&proj);
            editing_tracks
                .iter()
                .filter_map(|track| {
                    let old_clips = track.clips.clone();
                    let mut new_clips = old_clips.clone();
                    let mut changed = false;
                    for clip in &mut new_clips {
                        if let Some(new_start) = assignments.get(&clip.id) {
                            if clip.timeline_start != *new_start {
                                clip.timeline_start = *new_start;
                                changed = true;
                            }
                        }
                    }
                    if changed {
                        Some((track.id.clone(), old_clips, new_clips))
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
                label: "Align grouped clips by timecode".to_string(),
            };
            self.history.execute(Box::new(cmd), &mut proj);
        }
        true
    }

    fn can_link_selected_clips(&self) -> bool {
        self.selected_ids_or_primary().len() >= 2
    }

    fn can_unlink_selected_clips(&self) -> bool {
        let target_ids = self.selected_ids_or_primary();
        if target_ids.is_empty() {
            return false;
        }
        let proj = self.project.borrow();
        let editing_tracks = self.resolve_editing_tracks(&proj);
        editing_tracks
            .iter()
            .flat_map(|t| t.clips.iter())
            .any(|clip| {
                target_ids.contains(&clip.id)
                    && clip
                        .link_group_id
                        .as_ref()
                        .map(|gid| !gid.is_empty())
                        .unwrap_or(false)
            })
    }

    fn can_align_selected_groups_by_timecode(&self) -> bool {
        let target_groups = self.selected_group_ids();
        if target_groups.is_empty() {
            return false;
        }

        let proj = self.project.borrow();
        let mut found_group = false;
        for group_id in &target_groups {
            let members: Vec<_> = proj
                .tracks
                .iter()
                .flat_map(|track| track.clips.iter())
                .filter(|clip| clip.group_id.as_deref() == Some(group_id.as_str()))
                .collect();
            if members.len() < 2 {
                continue;
            }
            found_group = true;
            if members
                .iter()
                .any(|clip| clip.source_timecode_start_ns().is_none())
            {
                return false;
            }
        }
        found_group
    }

    /// Returns true when 2+ selected clips could be synced by audio.
    fn can_sync_selected_clips_by_audio(&self) -> bool {
        let ids = self.selected_ids_or_primary();
        if ids.len() < 2 {
            return false;
        }
        // All selected clips need a source_path (always true) — no further validation needed;
        // actual audio availability is checked at extraction time.
        true
    }

    fn can_remove_silent_parts(&self) -> bool {
        let ids = self.selected_ids_or_primary();
        if ids.len() != 1 {
            return false;
        }
        let clip_id = ids.iter().next().unwrap();
        let proj = self.project.borrow();
        let editing_tracks = self.resolve_editing_tracks(&proj);
        editing_tracks
            .iter()
            .flat_map(|t| t.clips.iter())
            .find(|c| &c.id == clip_id)
            .map(|c| c.kind != ClipKind::Image)
            .unwrap_or(false)
    }

    fn can_detect_scene_cuts(&self) -> bool {
        let ids = self.selected_ids_or_primary();
        if ids.len() != 1 {
            return false;
        }
        let clip_id = ids.iter().next().unwrap();
        let proj = self.project.borrow();
        let editing_tracks = self.resolve_editing_tracks(&proj);
        editing_tracks
            .iter()
            .flat_map(|t| t.clips.iter())
            .find(|c| &c.id == clip_id)
            .map(|c| {
                c.kind != ClipKind::Image
                    && c.kind != ClipKind::Title
                    && c.kind != ClipKind::Adjustment
            })
            .unwrap_or(false)
    }

    fn can_convert_ltc_to_timecode(&self) -> bool {
        if self.on_convert_ltc_to_timecode.is_none() {
            return false;
        }
        let ids = self.selected_ids_or_primary();
        if ids.len() != 1 {
            return false;
        }
        let Some(clip_id) = ids.iter().next() else {
            return false;
        };
        let proj = self.project.borrow();
        proj.clip_ref(clip_id)
            .map(|clip| {
                !clip.source_path.is_empty()
                    && clip.source_out > clip.source_in
                    && matches!(clip.kind, ClipKind::Video | ClipKind::Audio)
            })
            .unwrap_or(false)
    }

    fn clip_context_menu_actionability(&self) -> ClipContextMenuActionability {
        ClipContextMenuActionability {
            join_through_edit: self.can_join_selected_through_edit(),
            freeze_frame: self.can_create_freeze_frame_at_playhead(),
            link_selected: self.can_link_selected_clips(),
            unlink_selected: self.can_unlink_selected_clips(),
            align_grouped: self.can_align_selected_groups_by_timecode(),
            sync_audio: self.can_sync_selected_clips_by_audio(),
            sync_replace_audio: self.can_sync_selected_clips_by_audio(),
            remove_silent_parts: self.can_remove_silent_parts(),
            detect_scene_cuts: self.can_detect_scene_cuts(),
            convert_ltc: self.can_convert_ltc_to_timecode(),
            split_stereo: {
                let ids = self.selected_ids_or_primary();
                ids.len() == 1
            },
            create_compound: self.can_create_compound(),
            break_apart_compound: self.can_break_apart_compound(),
            create_multicam: self.can_create_multicam(),
        }
    }

    fn selected_joinable_through_edit_boundary(&self) -> Option<(String, Clip, Clip)> {
        let selected_ids = self.selected_ids_or_primary();
        if selected_ids.is_empty() {
            return None;
        }
        let proj = self.project.borrow();
        let editing_tracks = self.resolve_editing_tracks(&proj);
        let mut candidates = Vec::new();
        for track in editing_tracks {
            for boundary in detect_track_through_edit_boundaries(track) {
                if !selected_ids.contains(&boundary.left_clip_id)
                    && !selected_ids.contains(&boundary.right_clip_id)
                {
                    continue;
                }
                let Some(left) = track.clips.get(boundary.left_clip_index) else {
                    continue;
                };
                let Some(right) = track.clips.get(boundary.right_clip_index) else {
                    continue;
                };
                if left.id != boundary.left_clip_id || right.id != boundary.right_clip_id {
                    continue;
                }
                if !through_edit_metadata_compatible(left, right) {
                    continue;
                }
                candidates.push((track.id.clone(), left.clone(), right.clone()));
            }
        }
        if candidates.len() == 1 {
            candidates.into_iter().next()
        } else {
            None
        }
    }

    fn can_join_selected_through_edit(&self) -> bool {
        self.selected_joinable_through_edit_boundary().is_some()
    }

    pub fn join_selected_through_edit(&mut self) -> bool {
        let Some((track_id, left_clip, right_clip)) =
            self.selected_joinable_through_edit_boundary()
        else {
            return false;
        };
        let merged_clip = merge_through_edit_clips(&left_clip, &right_clip);
        let merged_clip_id = merged_clip.id.clone();
        let (old_clips, mut new_clips) = {
            let proj = self.project.borrow();
            let editing_tracks = self.resolve_editing_tracks(&proj);
            let Some(track) = editing_tracks.iter().find(|track| track.id == track_id) else {
                return false;
            };
            let old_clips = track.clips.clone();
            let mut new_clips = Vec::with_capacity(old_clips.len().saturating_sub(1));
            let mut replaced_left = false;
            let mut removed_right = false;
            for clip in &old_clips {
                if clip.id == right_clip.id {
                    removed_right = true;
                    continue;
                }
                if clip.id == left_clip.id {
                    replaced_left = true;
                    new_clips.push(merged_clip.clone());
                } else {
                    new_clips.push(clip.clone());
                }
            }
            if !replaced_left || !removed_right {
                return false;
            }
            new_clips.sort_by_key(|clip| clip.timeline_start);
            if old_clips == new_clips {
                return false;
            }
            (old_clips, new_clips)
        };
        let cmd = JoinThroughEditCommand {
            track_id: track_id.clone(),
            old_clips,
            new_clips: std::mem::take(&mut new_clips),
        };
        let mut proj = self.project.borrow_mut();
        self.history.execute(Box::new(cmd), &mut proj);
        drop(proj);
        self.set_single_clip_selection(merged_clip_id, track_id);
        true
    }

    fn selected_video_clip_at_playhead(&self) -> Option<(String, Clip)> {
        let selected_clip_id = self.selected_clip_id.as_deref()?;
        let playhead_ns = self.playhead_ns;
        let proj = self.project.borrow();
        let editing_tracks = self.resolve_editing_tracks(&proj);
        editing_tracks.iter().find_map(|track| {
            track
                .clips
                .iter()
                .find(|clip| {
                    clip.id == selected_clip_id
                        && clip.kind == ClipKind::Video
                        && playhead_ns >= clip.timeline_start
                        && playhead_ns <= clip.timeline_end()
                })
                .cloned()
                .map(|clip| (track.id.clone(), clip))
        })
    }

    fn can_create_freeze_frame_at_playhead(&self) -> bool {
        self.selected_video_clip_at_playhead().is_some()
    }

    pub fn create_freeze_frame_from_selected_at_playhead(&mut self, hold_duration_ns: u64) -> bool {
        if hold_duration_ns == 0 {
            return false;
        }
        let Some((track_id, selected_clip)) = self.selected_video_clip_at_playhead() else {
            return false;
        };
        let playhead_ns = self.playhead_ns;
        let freeze_source_ns = freeze_source_from_playhead(&selected_clip, playhead_ns);
        let mut freeze_clip = selected_clip.clone();
        freeze_clip.id = uuid::Uuid::new_v4().to_string();
        let freeze_clip_id = freeze_clip.id.clone();
        freeze_clip.kind = ClipKind::Video;
        freeze_clip.timeline_start = playhead_ns;
        freeze_clip.source_in = freeze_source_ns;
        freeze_clip.source_out = freeze_source_ns.saturating_add(1);
        freeze_clip.volume = 0.0;
        freeze_clip.freeze_frame = true;
        freeze_clip.freeze_frame_source_ns = Some(freeze_source_ns);
        freeze_clip.freeze_frame_hold_duration_ns = Some(hold_duration_ns);
        freeze_clip.group_id = None;
        freeze_clip.link_group_id = None;
        freeze_clip.clear_outgoing_transition();

        let mut changes = {
            let proj = self.project.borrow();
            let editing_tracks = self.resolve_editing_tracks(&proj);
            if editing_tracks.iter().all(|t| t.id != track_id) {
                return false;
            }
            let mut changes = Vec::new();
            let mut handled_selected = false;

            for track in editing_tracks {
                let old_clips = track.clips.clone();
                let mut new_clips = Vec::with_capacity(old_clips.len() + 2);
                let mut track_changed = false;
                let is_selected_track = track.id == track_id;

                for clip in &old_clips {
                    if is_selected_track && clip.id == selected_clip.id {
                        handled_selected = true;
                        track_changed = true;
                        if playhead_ns <= clip.timeline_start {
                            let mut shifted = clip.clone();
                            shifted.timeline_start =
                                shifted.timeline_start.saturating_add(hold_duration_ns);
                            new_clips.push(shifted);
                        } else if playhead_ns >= clip.timeline_end() {
                            let mut left_clip = clip.clone();
                            left_clip.clear_outgoing_transition();
                            new_clips.push(left_clip);
                        } else {
                            let cut_offset = playhead_ns.saturating_sub(clip.timeline_start);
                            let mut left_clip = clip.clone();
                            left_clip.source_out = left_clip.source_in.saturating_add(cut_offset);
                            left_clip.clear_outgoing_transition();
                            new_clips.push(left_clip);

                            let mut right_clip = clip.clone();
                            right_clip.id = uuid::Uuid::new_v4().to_string();
                            right_clip.source_in = right_clip.source_in.saturating_add(cut_offset);
                            right_clip.timeline_start =
                                playhead_ns.saturating_add(hold_duration_ns);
                            new_clips.push(right_clip);
                        }
                        continue;
                    }

                    if !is_selected_track
                        && playhead_ns > clip.timeline_start
                        && playhead_ns < clip.timeline_end()
                    {
                        let cut_offset = playhead_ns.saturating_sub(clip.timeline_start);
                        let mut left_clip = clip.clone();
                        left_clip.source_out = left_clip.source_in.saturating_add(cut_offset);
                        let mut right_clip = clip.clone();
                        right_clip.id = uuid::Uuid::new_v4().to_string();
                        right_clip.source_in = right_clip.source_in.saturating_add(cut_offset);
                        right_clip.timeline_start = playhead_ns.saturating_add(hold_duration_ns);
                        new_clips.push(left_clip);
                        new_clips.push(right_clip);
                        track_changed = true;
                        continue;
                    }

                    let mut adjusted = clip.clone();
                    if adjusted.timeline_start >= playhead_ns {
                        adjusted.timeline_start =
                            adjusted.timeline_start.saturating_add(hold_duration_ns);
                        track_changed = true;
                    }
                    new_clips.push(adjusted);
                }

                if is_selected_track {
                    new_clips.push(freeze_clip.clone());
                    track_changed = true;
                }
                if !track_changed {
                    continue;
                }
                new_clips.sort_by_key(|c| c.timeline_start);
                if new_clips != old_clips {
                    changes.push(TrackClipsChange {
                        track_id: track.id.clone(),
                        old_clips,
                        new_clips,
                    });
                }
            }

            if !handled_selected {
                return false;
            }
            changes
        };
        if changes.is_empty() {
            return false;
        }
        let cmd = SetMultipleTracksClipsCommand {
            changes: std::mem::take(&mut changes),
            label: "Create freeze frame".to_string(),
        };
        let mut proj = self.project.borrow_mut();
        self.history.execute(Box::new(cmd), &mut proj);
        drop(proj);
        self.set_single_clip_selection(freeze_clip_id, track_id);
        true
    }

    /// Collect selected clip info for audio sync (2+ clips required).
    pub fn collect_selected_clips_for_sync(
        &self,
    ) -> Option<Vec<(String, String, u64, u64, u64, String)>> {
        let ids = self.selected_ids_or_primary();
        if ids.len() < 2 {
            return None;
        }
        let proj = self.project.borrow();
        let clip_infos: Vec<(String, String, u64, u64, u64, String)> = proj
            .tracks
            .iter()
            .flat_map(|t| {
                t.clips.iter().filter(|c| ids.contains(&c.id)).map(|c| {
                    (
                        c.id.clone(),
                        c.source_path.clone(),
                        c.source_in,
                        c.source_out,
                        c.timeline_start,
                        t.id.clone(),
                    )
                })
            })
            .collect();
        drop(proj);
        if clip_infos.len() < 2 {
            None
        } else {
            Some(clip_infos)
        }
    }

    /// Collect clip info for audio sync and fire the callback.
    fn sync_selected_clips_by_audio(&self) {
        if let Some(clip_infos) = self.collect_selected_clips_for_sync() {
            if let Some(ref cb) = self.on_sync_audio {
                cb(clip_infos);
            }
        }
    }

    pub fn copy_selected_to_clipboard(&mut self) -> bool {
        let Some(clip_id) = self.selected_clip_id.clone() else {
            return false;
        };
        let copied = {
            let proj = self.project.borrow();
            let editing_tracks = self.resolve_editing_tracks(&proj);
            editing_tracks.iter().find_map(|track| {
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
            let editing_tracks = self.resolve_editing_tracks(&proj);
            if let Some(ref selected_tid) = self.selected_track_id {
                if editing_tracks
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
                editing_tracks
                    .iter()
                    .find(|t| t.id == payload.source_track_id && t.kind == target_kind)
                    .map(|t| t.id.clone())
            })
            .or_else(|| {
                editing_tracks
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
            let editing_tracks = self.resolve_editing_tracks(&proj);
            let Some(track) = editing_tracks.iter().find(|t| t.id == target_track_id) else {
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
            let editing_tracks = self.resolve_editing_tracks(&proj);
            let Some(track) = editing_tracks
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

    /// Copy color grading values from the selected clip into the color-grade clipboard.
    pub fn copy_color_grade(&mut self) -> bool {
        let Some(clip_id) = self.selected_clip_id.clone() else {
            return false;
        };
        let grade = {
            let proj = self.project.borrow();
            let editing_tracks = self.resolve_editing_tracks(&proj);
            editing_tracks.iter().find_map(|track| {
                track
                    .clips
                    .iter()
                    .find(|c| c.id == clip_id)
                    .map(|clip| ColorGradeClipboard::from_clip(clip))
            })
        };
        if let Some(payload) = grade {
            self.color_grade_clipboard = Some(payload);
            true
        } else {
            false
        }
    }

    /// Paste color grading values from the color-grade clipboard onto the selected clip.
    pub fn paste_color_grade(&mut self) -> bool {
        let Some(grade) = self.color_grade_clipboard.clone() else {
            return false;
        };
        let Some(selected_clip_id) = self.selected_clip_id.clone() else {
            return false;
        };

        let (track_id, old_clips, mut new_clips) = {
            let proj = self.project.borrow();
            let editing_tracks = self.resolve_editing_tracks(&proj);
            let Some(track) = editing_tracks
                .iter()
                .find(|t| t.clips.iter().any(|c| c.id == selected_clip_id))
            else {
                return false;
            };
            let Some(target_idx) = track.clips.iter().position(|c| c.id == selected_clip_id) else {
                return false;
            };
            let old_clips = track.clips.clone();
            let mut new_clips = old_clips.clone();
            if !grade.apply_to(&mut new_clips[target_idx]) {
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
            label: "Paste color grade".to_string(),
        };
        let mut proj = self.project.borrow_mut();
        self.history.execute(Box::new(cmd), &mut proj);
        true
    }

    fn clear_clip_selection(&mut self) {
        self.selected_clip_id = None;
        self.selected_clip_ids.clear();
        self.selection_anchor_clip_id = None;
        self.clear_keyframe_selection();
    }

    fn set_single_clip_selection(&mut self, clip_id: String, track_id: String) {
        let mut ids = HashSet::new();
        ids.insert(clip_id.clone());
        self.set_selection_with_primary(clip_id, track_id, ids);
    }

    fn prepare_clip_context_menu_selection(&mut self, clip_id: &str, track_id: &str) -> bool {
        if self.is_clip_selected(clip_id) {
            false
        } else {
            self.set_single_clip_selection(clip_id.to_string(), track_id.to_string());
            true
        }
    }

    fn set_selection_with_primary(
        &mut self,
        primary_clip_id: String,
        track_id: String,
        mut selected_ids: HashSet<String>,
    ) {
        selected_ids.insert(primary_clip_id.clone());
        self.selected_clip_ids = self.expand_with_link_members(&selected_ids);
        self.selected_clip_id = Some(primary_clip_id.clone());
        self.selection_anchor_clip_id = Some(primary_clip_id);
        self.selected_track_id = Some(track_id);
        self.clear_keyframe_selection();
    }

    fn clear_keyframe_selection(&mut self) {
        self.selected_keyframe_local_times.clear();
    }

    fn selected_keyframe_local_times_for_clip(&self, clip_id: &str) -> HashSet<u64> {
        self.selected_keyframe_local_times
            .get(clip_id)
            .cloned()
            .unwrap_or_default()
    }

    fn has_selected_keyframes(&self) -> bool {
        self.selected_keyframe_local_times
            .values()
            .any(|times| !times.is_empty())
    }

    fn set_single_keyframe_selection(&mut self, clip_id: String, local_time_ns: u64) {
        self.clear_keyframe_selection();
        let mut times = HashSet::new();
        times.insert(local_time_ns);
        self.selected_keyframe_local_times.insert(clip_id, times);
    }

    fn is_keyframe_local_time_selected(&self, clip_id: &str, local_time_ns: u64) -> bool {
        self.selected_keyframe_local_times
            .get(clip_id)
            .map(|times| times.contains(&local_time_ns))
            .unwrap_or(false)
    }

    fn toggle_keyframe_selection(&mut self, clip_id: &str, local_time_ns: u64) {
        let times = self
            .selected_keyframe_local_times
            .entry(clip_id.to_string())
            .or_default();
        if !times.insert(local_time_ns) {
            times.remove(&local_time_ns);
        }
        if times.is_empty() {
            self.selected_keyframe_local_times.remove(clip_id);
        }
    }

    fn begin_keyframe_marquee_selection(
        &mut self,
        clip_id: String,
        track_id: String,
        start_local_ns: u64,
        additive: bool,
    ) {
        let base_times = if additive {
            self.selected_keyframe_local_times_for_clip(&clip_id)
        } else {
            HashSet::new()
        };
        self.keyframe_marquee_selection = Some(KeyframeMarqueeSelection {
            clip_id,
            track_id,
            start_local_ns,
            current_local_ns: start_local_ns,
            additive,
            base_times,
        });
    }

    fn update_keyframe_marquee_selection(&mut self, current_local_ns: u64) -> bool {
        let Some(mut marquee) = self.keyframe_marquee_selection.clone() else {
            return false;
        };
        marquee.current_local_ns = current_local_ns;
        let low = marquee.start_local_ns.min(current_local_ns);
        let high = marquee.start_local_ns.max(current_local_ns);
        let times_in_range = {
            let proj = self.project.borrow();
            let editing_tracks = self.resolve_editing_tracks(&proj);
            editing_tracks
                .iter()
                .find(|t| t.id == marquee.track_id)
                .and_then(|track| track.clips.iter().find(|c| c.id == marquee.clip_id))
                .map(|clip| collect_keyframe_local_times_in_range(clip, low, high))
                .unwrap_or_default()
        };
        let mut next_times = if marquee.additive {
            marquee.base_times.clone()
        } else {
            HashSet::new()
        };
        next_times.extend(times_in_range);
        if next_times.is_empty() {
            self.selected_keyframe_local_times.remove(&marquee.clip_id);
        } else {
            self.selected_keyframe_local_times
                .insert(marquee.clip_id.clone(), next_times);
        }
        self.keyframe_marquee_selection = Some(marquee);
        true
    }

    fn end_keyframe_marquee_selection(&mut self) {
        self.keyframe_marquee_selection = None;
    }

    fn delete_selected_keyframes(&mut self) -> bool {
        if self.selected_keyframe_local_times.is_empty() {
            return false;
        }
        let mut changed = false;
        let mut proj = self.project.borrow_mut();
        let editing_tracks = self.resolve_editing_tracks_mut(&mut proj);
        for track in editing_tracks.iter_mut() {
            for clip in &mut track.clips {
                let Some(times) = self.selected_keyframe_local_times.get(&clip.id) else {
                    continue;
                };
                let mut ordered_times = times.iter().copied().collect::<Vec<_>>();
                ordered_times.sort_unstable();
                for local_time_ns in ordered_times {
                    if clip.remove_all_phase1_keyframes_at_local_ns(local_time_ns) > 0 {
                        changed = true;
                    }
                }
            }
        }
        if changed {
            proj.dirty = true;
        }
        changed
    }

    fn set_selected_keyframe_interpolation(
        &mut self,
        interpolation: KeyframeInterpolation,
    ) -> bool {
        if self.selected_keyframe_local_times.is_empty() {
            return false;
        }
        let mut changed = false;
        let mut proj = self.project.borrow_mut();
        let editing_tracks = self.resolve_editing_tracks_mut(&mut proj);
        for track in editing_tracks.iter_mut() {
            for clip in &mut track.clips {
                let Some(times) = self.selected_keyframe_local_times.get(&clip.id) else {
                    continue;
                };
                let mut ordered_times = times.iter().copied().collect::<Vec<_>>();
                ordered_times.sort_unstable();
                for local_time_ns in ordered_times {
                    if clip
                        .set_phase1_keyframe_interpolation_at_local_ns(local_time_ns, interpolation)
                        > 0
                    {
                        changed = true;
                    }
                }
            }
        }
        if changed {
            proj.dirty = true;
        }
        changed
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
            let editing_tracks = self.resolve_editing_tracks(&proj);
            for track in editing_tracks {
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

    fn select_clips_from_playhead(&mut self, forward: bool) -> bool {
        let prev_primary = self.selected_clip_id.clone();
        let prev_ids = self.selected_ids_or_primary();
        let playhead_ns = self.playhead_ns;
        let mut matches: Vec<(String, String, u64)> = Vec::new();
        {
            let proj = self.project.borrow();
            let editing_tracks = self.resolve_editing_tracks(&proj);
            for track in editing_tracks {
                for clip in &track.clips {
                    let include = if forward {
                        clip.timeline_end() > playhead_ns
                    } else {
                        clip.timeline_start < playhead_ns
                    };
                    if include {
                        matches.push((clip.id.clone(), track.id.clone(), clip.timeline_start));
                    }
                }
            }
        }
        if matches.is_empty() {
            let changed = !prev_ids.is_empty() || prev_primary.is_some();
            self.clear_clip_selection();
            self.selected_track_id = None;
            return changed;
        }
        if forward {
            matches.sort_by_key(|(_, _, start)| *start);
        } else {
            matches.sort_by(|a, b| b.2.cmp(&a.2));
        }
        let (primary_id, primary_track_id, _) = matches[0].clone();
        let selected_ids: HashSet<String> = matches.iter().map(|(id, _, _)| id.clone()).collect();
        let expanded_ids = self.expand_with_link_members(&selected_ids);
        let changed =
            prev_primary.as_deref() != Some(primary_id.as_str()) || prev_ids != expanded_ids;
        self.set_selection_with_primary(primary_id, primary_track_id, selected_ids);
        changed
    }

    pub fn select_clips_forward_from_playhead(&mut self) -> bool {
        self.select_clips_from_playhead(true)
    }

    pub fn select_clips_backward_from_playhead(&mut self) -> bool {
        self.select_clips_from_playhead(false)
    }

    pub fn selected_ids_or_primary(&self) -> HashSet<String> {
        if !self.selected_clip_ids.is_empty() {
            return self.selected_clip_ids.clone();
        }
        let mut ids = HashSet::new();
        if let Some(id) = self.selected_clip_id.clone() {
            ids.insert(id);
        }
        ids
    }

    /// Returns true when 2+ clips are selected (compound creation requires multiple clips).
    /// Returns true when the timeline is showing a compound clip's internal tracks
    /// (drill-down mode).
    pub fn is_editing_compound(&self) -> bool {
        !self.compound_nav_stack.is_empty()
    }

    /// Height of the compound breadcrumb bar (22px when inside a compound, 0 otherwise).
    pub fn breadcrumb_bar_height(&self) -> f64 {
        if self.is_editing_compound() {
            22.0
        } else {
            0.0
        }
    }

    /// Translate the root-timeline playhead into the timebase shown while
    /// drill-editing a compound clip. At root level this is a no-op.
    ///
    /// The compound editor always shows the full internal timeline, so 0 is
    /// the first frame of the compound contents even when the parent clip has
    /// a non-zero `source_in` window.
    pub fn editing_playhead_ns(&self) -> u64 {
        if self.compound_nav_stack.is_empty() {
            return self.playhead_ns;
        }
        let proj = self.project.borrow();
        let mut playhead = self.playhead_ns;
        let mut tracks: &[crate::model::track::Track] = &proj.tracks;
        for compound_id in &self.compound_nav_stack {
            let found = tracks
                .iter()
                .flat_map(|t| t.clips.iter())
                .find(|c| c.id == *compound_id && c.is_compound());
            if let Some(compound) = found {
                playhead = playhead.saturating_sub(compound.timeline_start);
                if let Some(ref inner) = compound.compound_tracks {
                    tracks = inner;
                } else {
                    break;
                }
            } else {
                break;
            }
        }
        playhead
    }

    /// Convert a compound-internal position (0-based, matching the internal
    /// clips' timeline_start values) to root-timeline coordinates.
    /// Returns `internal_ns` unchanged when not inside a compound.
    pub fn root_playhead_from_internal_ns(&self, internal_ns: u64) -> u64 {
        if self.compound_nav_stack.is_empty() {
            return internal_ns;
        }
        let proj = self.project.borrow();
        let mut entries: Vec<u64> = Vec::new();
        let mut tracks: &[crate::model::track::Track] = &proj.tracks;
        for compound_id in &self.compound_nav_stack {
            let found = tracks
                .iter()
                .flat_map(|t| t.clips.iter())
                .find(|c| c.id == *compound_id && c.is_compound());
            if let Some(compound) = found {
                entries.push(compound.timeline_start);
                if let Some(ref inner) = compound.compound_tracks {
                    tracks = inner;
                } else {
                    break;
                }
            } else {
                break;
            }
        }
        // Reverse: add back each compound's timeline_start
        let mut playhead = internal_ns;
        for timeline_start in entries.iter().rev() {
            playhead += timeline_start;
        }
        playhead
    }

    /// Alias for `editing_playhead_ns()` when call sites want to emphasize
    /// that the returned position is in full compound-internal time.
    pub fn internal_playhead_ns(&self) -> u64 {
        self.editing_playhead_ns()
    }

    /// Set playhead from a visual/compound-internal position.
    /// Translates to root coordinates so `playhead_ns` stays in root space.
    /// The returned value is the compound-internal position (for `on_seek`).
    pub fn set_playhead_visual(&mut self, visual_ns: u64) -> u64 {
        self.playhead_ns = self.root_playhead_from_internal_ns(visual_ns);
        visual_ns
    }

    /// Resolve the currently-editing tracks based on the navigation stack.
    /// Returns project.tracks when at root level, or the innermost compound
    /// clip's internal tracks when drilled in.
    pub fn resolve_editing_tracks<'a>(
        &self,
        proj: &'a Project,
    ) -> &'a [crate::model::track::Track] {
        let mut tracks: &[crate::model::track::Track] = &proj.tracks;
        for compound_id in &self.compound_nav_stack {
            let found = tracks
                .iter()
                .flat_map(|t| t.clips.iter())
                .find(|c| c.id == *compound_id && c.is_compound());
            if let Some(compound) = found {
                if let Some(ref inner) = compound.compound_tracks {
                    tracks = inner;
                } else {
                    break;
                }
            } else {
                break;
            }
        }
        tracks
    }

    /// Resolve the currently-editing tracks mutably.
    /// Walks the nav stack through compound clips to find the innermost
    /// compound's internal tracks; returns `project.tracks` when at root.
    pub fn resolve_editing_tracks_mut<'a>(
        &self,
        proj: &'a mut Project,
    ) -> &'a mut Vec<crate::model::track::Track> {
        if self.compound_nav_stack.is_empty() {
            return &mut proj.tracks;
        }
        let mut tracks: *mut Vec<crate::model::track::Track> = &mut proj.tracks;
        for compound_id in &self.compound_nav_stack {
            // Safety: we're walking a tree structure via raw pointer to avoid
            // borrow checker issues with nested mutable references.  Each step
            // narrows the pointer to a deeper Vec<Track>.
            unsafe {
                let found = (*tracks)
                    .iter_mut()
                    .flat_map(|t| t.clips.iter_mut())
                    .find(|c| c.id == *compound_id && c.is_compound());
                if let Some(compound) = found {
                    if let Some(ref mut inner) = compound.compound_tracks {
                        tracks = inner as *mut Vec<crate::model::track::Track>;
                    } else {
                        break;
                    }
                } else {
                    break;
                }
            }
        }
        unsafe { &mut *tracks }
    }

    /// Enter a compound clip for drill-down editing.
    pub fn enter_compound(&mut self, clip_id: String) {
        // Save scroll state so we can restore it on exit.
        self.compound_saved_scroll = Some(self.scroll_offset);
        self.compound_nav_stack.push(clip_id);
        self.selected_clip_id = None;
        self.selected_clip_ids.clear();
        self.selected_track_id = None;
        // Reset scroll so the internal timeline starts at the left edge.
        self.scroll_offset = 0.0;
    }

    /// Recompute compound clip duration after internal edits.
    /// Should be called after any mutation to compound internal tracks.
    pub fn sync_compound_duration(&self) {
        if self.compound_nav_stack.is_empty() {
            return;
        }
        let mut proj = self.project.borrow_mut();
        for compound_id in &self.compound_nav_stack {
            if let Some(clip) = proj.clip_mut(compound_id) {
                let new_dur = clip.compound_duration();
                if new_dur > 0 {
                    clip.source_out = new_dur;
                }
            }
        }
    }

    /// Navigate back one level in the compound drill-down stack.
    pub fn exit_compound(&mut self) {
        self.compound_nav_stack.pop();
        self.selected_clip_id = None;
        self.selected_clip_ids.clear();
        self.selected_track_id = None;
        // Restore saved scroll position when returning to root.
        if self.compound_nav_stack.is_empty() {
            if let Some(saved) = self.compound_saved_scroll.take() {
                self.scroll_offset = saved;
            }
        }
    }

    /// Navigate back to the root project timeline.
    pub fn exit_compound_to_root(&mut self) {
        self.compound_nav_stack.clear();
        self.selected_clip_id = None;
        self.selected_clip_ids.clear();
        self.selected_track_id = None;
        if let Some(saved) = self.compound_saved_scroll.take() {
            self.scroll_offset = saved;
        }
    }

    /// Get breadcrumb labels for the current navigation path.
    pub fn compound_breadcrumb_labels(&self) -> Vec<String> {
        let proj = self.project.borrow();
        let mut labels = vec!["Project".to_string()];
        let mut tracks: &[crate::model::track::Track] = &proj.tracks;
        for compound_id in &self.compound_nav_stack {
            let found = tracks
                .iter()
                .flat_map(|t| t.clips.iter())
                .find(|c| c.id == *compound_id && c.is_compound());
            if let Some(compound) = found {
                labels.push(compound.label.clone());
                if let Some(ref inner) = compound.compound_tracks {
                    tracks = inner;
                } else {
                    break;
                }
            } else {
                break;
            }
        }
        labels
    }

    /// Set the multi-selection to the given clip IDs (used by MCP).
    pub fn set_selected_clip_ids(&mut self, ids: HashSet<String>) {
        self.selected_clip_id = ids.iter().next().cloned();
        self.selected_clip_ids = ids;
    }

    fn can_create_compound(&self) -> bool {
        let ids = self.selected_ids_or_primary();
        ids.len() >= 2
    }

    /// Returns true when 2+ video clips are selected (multicam needs multiple camera angles).
    fn can_create_multicam(&self) -> bool {
        let ids = self.selected_ids_or_primary();
        if ids.len() < 2 {
            return false;
        }
        // All selected clips must be video (not audio, title, adjustment, etc.)
        let proj = self.project.borrow();
        let editing_tracks = self.resolve_editing_tracks(&proj);
        let video_count = editing_tracks
            .iter()
            .flat_map(|t| t.clips.iter())
            .filter(|c| ids.contains(&c.id))
            .filter(|c| c.kind == ClipKind::Video || c.kind == ClipKind::Image)
            .count();
        video_count >= 2 && video_count == ids.len()
    }

    /// Request multicam clip creation. This fires the on_create_multicam callback
    /// which runs audio sync in a background thread. The actual multicam clip is
    /// created when the sync results arrive.
    pub fn request_create_multicam(&mut self) -> bool {
        let ids = self.selected_ids_or_primary();
        if ids.len() < 2 {
            return false;
        }
        let clip_infos: Vec<(String, String, u64, u64, u64, String)> = {
            let proj = self.project.borrow();
            let editing_tracks = self.resolve_editing_tracks(&proj);
            editing_tracks
                .iter()
                .flat_map(|t| {
                    let tid = t.id.clone();
                    t.clips.iter().map(move |c| (c.clone(), tid.clone()))
                })
                .filter(|(c, _)| ids.contains(&c.id))
                .map(|(c, tid)| {
                    (
                        c.id.clone(),
                        c.source_path.clone(),
                        c.source_in,
                        c.source_out,
                        c.timeline_start,
                        tid,
                    )
                })
                .collect()
        };
        if clip_infos.len() < 2 {
            return false;
        }
        if let Some(ref cb) = self.on_create_multicam {
            cb(clip_infos);
            true
        } else {
            false
        }
    }

    /// Check if the selected clip is a multicam clip and the playhead is within it.
    fn selected_multicam_context(&self) -> Option<(String, u64)> {
        let clip_id = self.selected_clip_id.as_ref()?;
        let proj = self.project.borrow();
        let clip = proj.clip_ref(clip_id)?;
        if !clip.is_multicam() {
            return None;
        }
        let playhead = self.playhead_ns;
        if playhead >= clip.timeline_start && playhead < clip.timeline_end() {
            let local = playhead.saturating_sub(clip.timeline_start);
            Some((clip_id.clone(), local))
        } else {
            None
        }
    }

    /// Insert an angle switch at the current playhead for the selected multicam clip.
    pub fn insert_multicam_angle_switch(&mut self, angle_index: usize) -> bool {
        let (clip_id, local_pos) = match self.selected_multicam_context() {
            Some(ctx) => ctx,
            None => return false,
        };
        let (track_id, old_clips) = {
            let proj = self.project.borrow();
            let editing_tracks = self.resolve_editing_tracks(&proj);
            let found = editing_tracks
                .iter()
                .find(|t| t.clips.iter().any(|c| c.id == clip_id));
            match found {
                Some(t) => (t.id.clone(), t.clips.clone()),
                None => return false,
            }
        };
        {
            let mut proj = self.project.borrow_mut();
            if let Some(clip) = proj.clip_mut(&clip_id) {
                let num_angles = clip.multicam_angles.as_ref().map(|a| a.len()).unwrap_or(0);
                if angle_index >= num_angles {
                    return false;
                }
                clip.insert_angle_switch(local_pos, angle_index);
            }
        }
        let new_clips = {
            let proj = self.project.borrow();
            proj.track_ref(&track_id)
                .map(|t| t.clips.clone())
                .unwrap_or_default()
        };
        let cmd = Box::new(crate::undo::SetTrackClipsCommand {
            track_id,
            old_clips,
            new_clips,
            label: format!("Switch to Angle {}", angle_index + 1),
        });
        self.history.undo_stack.push(cmd);
        self.history.redo_stack.clear();
        true
    }

    /// Returns true when exactly one compound clip is selected.
    fn can_break_apart_compound(&self) -> bool {
        let ids = self.selected_ids_or_primary();
        if ids.len() != 1 {
            return false;
        }
        let clip_id = ids.iter().next().unwrap();
        let proj = self.project.borrow();
        let editing_tracks = self.resolve_editing_tracks(&proj);
        editing_tracks
            .iter()
            .flat_map(|t| t.clips.iter())
            .any(|c| c.id == *clip_id && c.is_compound())
    }

    /// Create a compound clip from the current selection.
    /// Returns `true` if the compound was created successfully.
    pub fn create_compound_from_selection(&mut self) -> bool {
        let ids = self.selected_ids_or_primary();
        if ids.len() < 2 {
            return false;
        }

        let proj = self.project.borrow();
        let editing_tracks = self.resolve_editing_tracks(&proj);

        // Collect selected clips with their track info
        let mut selected_clips: Vec<(Clip, String, usize, crate::model::track::TrackKind)> =
            Vec::new();
        for (t_idx, track) in editing_tracks.iter().enumerate() {
            for clip in &track.clips {
                if ids.contains(&clip.id) {
                    selected_clips.push((clip.clone(), track.id.clone(), t_idx, track.kind));
                }
            }
        }
        if selected_clips.len() < 2 {
            return false;
        }

        // Compute bounding time range
        let earliest_start = selected_clips
            .iter()
            .map(|(c, _, _, _)| c.timeline_start)
            .min()
            .unwrap_or(0);
        let latest_end = selected_clips
            .iter()
            .map(|(c, _, _, _)| c.timeline_end())
            .max()
            .unwrap_or(0);

        // Build internal tracks for the compound clip.
        // Group clips by their original track kind and index.
        use std::collections::BTreeMap;
        let mut track_groups: BTreeMap<usize, Vec<Clip>> = BTreeMap::new();
        let mut track_meta: std::collections::HashMap<
            usize,
            (String, crate::model::track::TrackKind),
        > = std::collections::HashMap::new();
        for (mut clip, _track_id, t_idx, t_kind) in selected_clips.iter().cloned() {
            // Rebase clip timeline_start relative to compound start
            clip.timeline_start = clip.timeline_start.saturating_sub(earliest_start);
            track_groups.entry(t_idx).or_default().push(clip);
            track_meta.entry(t_idx).or_insert((String::new(), t_kind));
        }

        let mut internal_tracks = Vec::new();
        for (t_idx, clips) in &track_groups {
            let (_, t_kind) = track_meta[t_idx];
            let label = format!(
                "{} {}",
                if t_kind == crate::model::track::TrackKind::Video {
                    "Video"
                } else {
                    "Audio"
                },
                internal_tracks
                    .iter()
                    .filter(|t: &&crate::model::track::Track| t.kind == t_kind)
                    .count()
                    + 1
            );
            let mut track = if t_kind == crate::model::track::TrackKind::Video {
                crate::model::track::Track::new_video(label)
            } else {
                crate::model::track::Track::new_audio(label)
            };
            track.clips = clips.clone();
            internal_tracks.push(track);
        }

        // Find the topmost affected video track for compound clip placement
        let placement_track_idx = selected_clips
            .iter()
            .filter(|(_, _, _, k)| *k == crate::model::track::TrackKind::Video)
            .map(|(_, _, idx, _)| *idx)
            .min()
            .unwrap_or_else(|| {
                selected_clips
                    .iter()
                    .map(|(_, _, idx, _)| *idx)
                    .min()
                    .unwrap_or(0)
            });

        let placement_track_id = editing_tracks[placement_track_idx].id.clone();

        // Build undo changes: snapshot old clips for each affected track, then new clips
        let affected_track_ids: std::collections::HashSet<String> = selected_clips
            .iter()
            .map(|(_, tid, _, _)| tid.clone())
            .collect();

        let mut changes = Vec::new();
        for track in editing_tracks {
            if !affected_track_ids.contains(&track.id) {
                continue;
            }
            let old_clips = track.clips.clone();
            let mut new_clips: Vec<Clip> = track
                .clips
                .iter()
                .filter(|c| !ids.contains(&c.id))
                .cloned()
                .collect();
            // Add the compound clip to the placement track
            if track.id == placement_track_id {
                let compound = Clip::new_compound(earliest_start, internal_tracks.clone());
                new_clips.push(compound);
                new_clips.sort_by_key(|c| c.timeline_start);
            }
            changes.push(TrackClipsChange {
                track_id: track.id.clone(),
                old_clips,
                new_clips,
            });
        }

        drop(proj);

        // Execute via undo system
        let cmd = Box::new(SetMultipleTracksClipsCommand {
            changes,
            label: "Create Compound Clip".to_string(),
        });
        {
            let mut proj = self.project.borrow_mut();
            self.history.execute(cmd, &mut proj);
        }

        // Clear selection
        self.selected_clip_id = None;
        self.selected_clip_ids.clear();
        self.selected_track_id = None;

        true
    }

    /// Break apart the selected compound clip, restoring its internal clips
    /// to the timeline. Returns `true` if successful.
    pub fn break_apart_compound(&mut self) -> bool {
        let ids = self.selected_ids_or_primary();
        if ids.len() != 1 {
            return false;
        }
        let compound_id = ids.iter().next().unwrap().clone();

        let proj = self.project.borrow();
        let editing_tracks = self.resolve_editing_tracks(&proj);

        // Find the compound clip and its track
        let mut compound_clip: Option<Clip> = None;
        let mut compound_track_id: Option<String> = None;
        let mut compound_track_idx: Option<usize> = None;
        for (t_idx, track) in editing_tracks.iter().enumerate() {
            if let Some(clip) = track.clips.iter().find(|c| c.id == compound_id) {
                if !clip.is_compound() {
                    return false;
                }
                compound_clip = Some(clip.clone());
                compound_track_id = Some(track.id.clone());
                compound_track_idx = Some(t_idx);
                break;
            }
        }
        let Some(compound) = compound_clip else {
            return false;
        };
        let compound_track_id = compound_track_id.unwrap();
        let _compound_track_idx = compound_track_idx.unwrap();
        if compound.compound_tracks.is_none() {
            return false;
        }
        let flattened_children =
            crate::model::compound_flattening::flatten_compound_children(&compound, 0);

        // Build changes: remove compound from its track, add internal clips back
        let mut changes = Vec::new();

        // First, handle the compound's own track
        let track = editing_tracks
            .iter()
            .find(|t| t.id == compound_track_id)
            .unwrap();
        let old_clips = track.clips.clone();
        let mut new_clips: Vec<Clip> = track
            .clips
            .iter()
            .filter(|c| c.id != compound_id)
            .cloned()
            .collect();

        // Add rebased internal video clips to compound's track
        for child in &flattened_children {
            if !child.is_audio_track {
                new_clips.push(child.clip.clone());
            }
        }
        new_clips.sort_by_key(|c| c.timeline_start);
        changes.push(TrackClipsChange {
            track_id: compound_track_id.clone(),
            old_clips,
            new_clips,
        });

        // Handle internal audio clips — find first audio track or skip
        let audio_clips: Vec<Clip> = flattened_children
            .into_iter()
            .filter(|child| child.is_audio_track)
            .map(|child| child.clip)
            .collect();

        if !audio_clips.is_empty() {
            if let Some(audio_track) = editing_tracks.iter().find(|t| t.is_audio()) {
                let old_clips = audio_track.clips.clone();
                let mut new_clips = old_clips.clone();
                new_clips.extend(audio_clips);
                new_clips.sort_by_key(|c| c.timeline_start);
                changes.push(TrackClipsChange {
                    track_id: audio_track.id.clone(),
                    old_clips,
                    new_clips,
                });
            }
        }

        drop(proj);

        let cmd = Box::new(SetMultipleTracksClipsCommand {
            changes,
            label: "Break Apart Compound Clip".to_string(),
        });
        {
            let mut proj = self.project.borrow_mut();
            self.history.execute(cmd, &mut proj);
        }

        self.selected_clip_id = None;
        self.selected_clip_ids.clear();
        self.selected_track_id = None;

        true
    }

    /// Build a single audition clip from the selected video/image clips on
    /// the same track. The first selected clip's timeline_start anchors the
    /// audition; the first selected clip becomes the active take. Returns
    /// `true` when the operation runs.
    pub fn create_audition_from_selection(&mut self) -> bool {
        let ids = self.selected_ids_or_primary();
        if ids.len() < 2 {
            return false;
        }
        let proj = self.project.borrow();
        let editing_tracks = self.resolve_editing_tracks(&proj);
        let mut hits: Vec<(Clip, String)> = Vec::new();
        for track in editing_tracks {
            for clip in &track.clips {
                if ids.contains(&clip.id) {
                    hits.push((clip.clone(), track.id.clone()));
                }
            }
        }
        if hits.len() < 2 {
            return false;
        }
        // All selected clips must be on the same track and the same kind
        // (Video / Image / Audio). Refuse mixed selections.
        let first_track = hits[0].1.clone();
        if hits.iter().any(|(_, t)| t != &first_track) {
            return false;
        }
        let first_kind = hits[0].0.kind.clone();
        let homogeneous = hits.iter().all(|(c, _)| c.kind == first_kind);
        if !homogeneous {
            return false;
        }
        if !matches!(
            first_kind,
            crate::model::clip::ClipKind::Video
                | crate::model::clip::ClipKind::Image
                | crate::model::clip::ClipKind::Audio
        ) {
            return false;
        }
        // Sort by timeline_start so the earliest clip is the audition anchor
        // and the active take.
        hits.sort_by_key(|(c, _)| c.timeline_start);
        let anchor_start = hits[0].0.timeline_start;
        let takes: Vec<crate::model::clip::AuditionTake> = hits
            .iter()
            .map(|(c, _)| crate::model::clip::AuditionTake {
                id: uuid::Uuid::new_v4().to_string(),
                label: c.label.clone(),
                source_path: c.source_path.clone(),
                source_in: c.source_in,
                source_out: c.source_out,
                source_timecode_base_ns: c.source_timecode_base_ns,
                media_duration_ns: c.media_duration_ns,
            })
            .collect();
        let audition = Clip::new_audition(anchor_start, takes, 0);
        // Build a SetMultipleTracksClipsCommand that drops the selected
        // clips from their track and inserts the audition in the same slot.
        let mut changes = Vec::new();
        for track in self.resolve_editing_tracks(&proj) {
            if track.id != first_track {
                continue;
            }
            let old_clips = track.clips.clone();
            let mut new_clips: Vec<Clip> = track
                .clips
                .iter()
                .filter(|c| !ids.contains(&c.id))
                .cloned()
                .collect();
            new_clips.push(audition.clone());
            new_clips.sort_by_key(|c| c.timeline_start);
            changes.push(TrackClipsChange {
                track_id: track.id.clone(),
                old_clips,
                new_clips,
            });
        }
        drop(proj);
        let cmd = Box::new(SetMultipleTracksClipsCommand {
            changes,
            label: "Create Audition".to_string(),
        });
        {
            let mut proj = self.project.borrow_mut();
            self.history.execute(cmd, &mut proj);
        }
        // Select the new audition clip so the Inspector immediately shows
        // the takes list.
        self.selected_clip_id = Some(audition.id.clone());
        self.selected_clip_ids.clear();
        self.selected_clip_ids.insert(audition.id);
        true
    }

    /// Finalize the currently selected audition clip — collapse it to a
    /// normal clip referencing only the active take. Returns `true` on
    /// success.
    pub fn finalize_selected_audition(&mut self) -> bool {
        let cid = self.selected_clip_id.clone();
        let Some(cid) = cid else { return false };
        let snapshot = self.project.borrow().clip_ref(&cid).cloned();
        if !snapshot.as_ref().map(|c| c.is_audition()).unwrap_or(false) {
            return false;
        }
        let cmd = Box::new(crate::undo::FinalizeAuditionCommand {
            clip_id: cid,
            before_snapshot: snapshot,
        });
        let mut proj = self.project.borrow_mut();
        self.history.execute(cmd, &mut proj);
        true
    }

    fn expand_with_link_members(&self, ids: &HashSet<String>) -> HashSet<String> {
        if ids.is_empty() {
            return HashSet::new();
        }
        let link_group_ids: HashSet<String> = {
            let proj = self.project.borrow();
            let editing_tracks = self.resolve_editing_tracks(&proj);
            editing_tracks
                .iter()
                .flat_map(|t| t.clips.iter())
                .filter(|c| ids.contains(&c.id))
                .filter_map(|c| c.link_group_id.clone())
                .collect()
        };
        if link_group_ids.is_empty() {
            return ids.clone();
        }
        let mut expanded = ids.clone();
        let proj = self.project.borrow();
        let editing_tracks = self.resolve_editing_tracks(&proj);
        for clip in editing_tracks.iter().flat_map(|t| t.clips.iter()) {
            if clip
                .link_group_id
                .as_deref()
                .is_some_and(|gid| link_group_ids.contains(gid))
            {
                expanded.insert(clip.id.clone());
            }
        }
        expanded
    }

    fn expand_with_group_members(&self, ids: &HashSet<String>) -> HashSet<String> {
        if ids.is_empty() {
            return HashSet::new();
        }
        let group_ids: HashSet<String> = {
            let proj = self.project.borrow();
            let editing_tracks = self.resolve_editing_tracks(&proj);
            editing_tracks
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
            let editing_tracks = self.resolve_editing_tracks(&proj);
            for clip in editing_tracks.iter().flat_map(|t| t.clips.iter()) {
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

    fn expand_with_related_members(&self, ids: &HashSet<String>) -> HashSet<String> {
        let mut expanded = ids.clone();
        loop {
            let next = self.expand_with_link_members(&self.expand_with_group_members(&expanded));
            if next.len() == expanded.len() {
                return expanded;
            }
            expanded = next;
        }
    }

    fn move_clip_ids_for_drag(&self, clip_id: &str) -> Vec<String> {
        let selected_ids = self.selected_ids_or_primary();
        let mut base_ids = HashSet::new();
        if selected_ids.contains(clip_id) && !selected_ids.is_empty() {
            base_ids = selected_ids;
        } else {
            base_ids.insert(clip_id.to_string());
        }
        self.expand_with_related_members(&base_ids)
            .into_iter()
            .collect()
    }

    fn grouped_peer_highlight_ids(&self) -> HashSet<String> {
        let selected_ids = self.selected_ids_or_primary();
        if selected_ids.is_empty() {
            return HashSet::new();
        }
        let group_ids: HashSet<String> = {
            let proj = self.project.borrow();
            let editing_tracks = self.resolve_editing_tracks(&proj);
            editing_tracks
                .iter()
                .flat_map(|t| t.clips.iter())
                .filter(|c| selected_ids.contains(&c.id))
                .filter_map(|c| c.group_id.clone())
                .collect()
        };
        if group_ids.is_empty() {
            return HashSet::new();
        }
        let proj = self.project.borrow();
        let editing_tracks = self.resolve_editing_tracks(&proj);
        editing_tracks
            .iter()
            .flat_map(|t| t.clips.iter())
            .filter(|c| {
                c.group_id
                    .as_deref()
                    .is_some_and(|gid| group_ids.contains(gid))
            })
            .filter(|c| !selected_ids.contains(&c.id))
            .map(|c| c.id.clone())
            .collect()
    }

    fn linked_peer_highlight_ids(&self) -> HashSet<String> {
        let selected_ids = self.selected_ids_or_primary();
        let Some(primary_id) = self.selected_clip_id.as_ref() else {
            return HashSet::new();
        };
        if selected_ids.is_empty() {
            return HashSet::new();
        }
        let link_group_ids: HashSet<String> = {
            let proj = self.project.borrow();
            let editing_tracks = self.resolve_editing_tracks(&proj);
            editing_tracks
                .iter()
                .flat_map(|t| t.clips.iter())
                .filter(|c| selected_ids.contains(&c.id))
                .filter_map(|c| c.link_group_id.clone())
                .collect()
        };
        if link_group_ids.is_empty() {
            return HashSet::new();
        }
        let proj = self.project.borrow();
        let editing_tracks = self.resolve_editing_tracks(&proj);
        editing_tracks
            .iter()
            .flat_map(|t| t.clips.iter())
            .filter(|c| selected_ids.contains(&c.id))
            .filter(|c| &c.id != primary_id)
            .filter(|c| {
                c.link_group_id
                    .as_deref()
                    .is_some_and(|gid| link_group_ids.contains(gid))
            })
            .map(|c| c.id.clone())
            .collect()
    }

    fn shift_select_range_to(&mut self, track_id: &str, to_clip_id: &str) -> bool {
        let anchor = self
            .selection_anchor_clip_id
            .clone()
            .or_else(|| self.selected_clip_id.clone())
            .unwrap_or_else(|| to_clip_id.to_string());
        let range_ids = {
            let proj = self.project.borrow();
            let editing_tracks = self.resolve_editing_tracks(&proj);
            let Some(track) = editing_tracks.iter().find(|t| t.id == track_id) else {
                return false;
            };
            let Some(clicked_clip) = track.clips.iter().find(|c| c.id == to_clip_id) else {
                return false;
            };
            let clicked_start = clicked_clip.timeline_start;
            let Some(anchor_track) = editing_tracks
                .iter()
                .find(|t| t.clips.iter().any(|c| c.id == anchor))
            else {
                return false;
            };
            if anchor_track.id == track_id {
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
            } else {
                let Some(anchor_clip) = anchor_track.clips.iter().find(|c| c.id == anchor) else {
                    return false;
                };
                let range_start = anchor_clip.timeline_start.min(clicked_start);
                let range_end = anchor_clip.timeline_start.max(clicked_start);
                editing_tracks
                    .iter()
                    .flat_map(|t| t.clips.iter())
                    .filter(|c| c.timeline_end() >= range_start && c.timeline_start <= range_end)
                    .map(|c| c.id.clone())
                    .collect::<Vec<_>>()
            }
        };
        if range_ids.is_empty() {
            return false;
        }
        for id in range_ids {
            self.selected_clip_ids.insert(id);
        }
        self.selected_clip_ids = self.expand_with_link_members(&self.selected_clip_ids.clone());
        self.selected_clip_id = Some(to_clip_id.to_string());
        self.selected_track_id = Some(track_id.to_string());
        true
    }

    fn toggle_clip_selection(&mut self, clip_id: &str, track_id: &str) -> bool {
        if self.selected_clip_ids.contains(clip_id) {
            let mut target_ids = HashSet::new();
            target_ids.insert(clip_id.to_string());
            let linked_ids = self.expand_with_link_members(&target_ids);
            self.selected_clip_ids.retain(|id| !linked_ids.contains(id));
            if self.selected_clip_id.as_deref() == Some(clip_id) {
                self.selected_clip_id = self.selected_clip_ids.iter().next().cloned();
            }
            if self
                .selection_anchor_clip_id
                .as_ref()
                .is_some_and(|anchor| linked_ids.contains(anchor))
            {
                self.selection_anchor_clip_id = self.selected_clip_ids.iter().next().cloned();
            } else if self.selected_clip_id.is_none() {
                self.selection_anchor_clip_id = None;
            }
        } else {
            let mut target_ids = HashSet::new();
            target_ids.insert(clip_id.to_string());
            self.selected_clip_ids
                .extend(self.expand_with_link_members(&target_ids));
            self.selected_clip_id = Some(clip_id.to_string());
            if self.selection_anchor_clip_id.is_none() {
                self.selection_anchor_clip_id = Some(clip_id.to_string());
            }
            self.selected_track_id = Some(track_id.to_string());
        }
        true
    }

    fn select_clip_with_modifiers(
        &mut self,
        clip_id: &str,
        track_id: &str,
        shift: bool,
        ctrl_or_meta: bool,
    ) {
        if ctrl_or_meta {
            self.toggle_clip_selection(clip_id, track_id);
        } else if shift {
            if !self.shift_select_range_to(track_id, clip_id) {
                self.set_single_clip_selection(clip_id.to_string(), track_id.to_string());
            }
        } else {
            self.set_single_clip_selection(clip_id.to_string(), track_id.to_string());
        }
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
        next_ids = self.expand_with_link_members(&next_ids);
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
        // Convert widget y to content y (accounting for vertical scroll)
        let top = y0.min(y1).max(0.0) + self.vertical_scroll_offset;
        let bottom = y0.max(y1) + self.vertical_scroll_offset;
        if right <= left || bottom <= top {
            return Vec::new();
        }
        let proj = self.project.borrow();
        let editing_tracks = self.resolve_editing_tracks(&proj);
        let bc = self.breadcrumb_bar_height();
        let mut hits = Vec::new();
        for (track_idx, track) in editing_tracks.iter().enumerate() {
            let track_y = track_row_top_in_tracks(editing_tracks, track_idx) + bc;
            let clip_top = track_y + 2.0;
            let clip_bottom = clip_top + track_row_height(track) - 4.0;
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
    /// Razor cut the clip at the playhead position.  When `track_idx` is
    /// `Some`, only the specified track is considered; otherwise all tracks
    /// are searched (first match wins).
    pub fn razor_cut_at_playhead_on_track(&mut self, track_idx: Option<usize>) {
        let playhead = self.editing_playhead_ns();
        let (clip_to_cut, track_id) = {
            let proj = self.project.borrow();
            let editing_tracks = self.resolve_editing_tracks(&proj);
            let mut found = None;
            let tracks: Box<dyn Iterator<Item = (usize, &crate::model::track::Track)>> =
                if let Some(idx) = track_idx {
                    Box::new(editing_tracks.get(idx).into_iter().map(move |t| (idx, t)))
                } else {
                    Box::new(editing_tracks.iter().enumerate())
                };
            for (_i, track) in tracks {
                for clip in &track.clips {
                    if clip.timeline_start < playhead && clip.timeline_end() > playhead {
                        found = Some((clip.clone(), track.id.clone()));
                        break;
                    }
                }
                if found.is_some() {
                    break;
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

            // Filter subtitles: right clip keeps only segments that start at or after the cut point.
            right_clip
                .subtitle_segments
                .retain(|s| s.start_ns >= right_source_in);

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

    fn keyframe_marker_hit(&self, x: f64, y: f64) -> Option<KeyframeMarkerHit> {
        let track_idx = self.track_index_at_y(y)?;
        let proj = self.project.borrow();
        let editing_tracks = self.resolve_editing_tracks(&proj);
        let track = editing_tracks.get(track_idx)?;
        let mut row_top = 0.0;
        for (i, t) in editing_tracks.iter().enumerate() {
            if i == track_idx {
                break;
            }
            row_top += track_row_height(t);
        }
        let ch = track_row_height(track);
        let cy = row_top;
        // Convert widget y to content y for comparison against row positions
        let content_y = y + self.vertical_scroll_offset - self.breadcrumb_bar_height();
        let hit_tolerance = 4.0; // px tolerance for x
        for clip in &track.clips {
            let cx = self.ns_to_x(clip.timeline_start);
            let cw = (clip.duration() as f64 / NS_PER_SECOND) * self.pixels_per_second;
            if x < cx || x > cx + cw {
                continue;
            }
            let duration_ns = clip.duration();
            if duration_ns == 0 || cw <= 10.0 || ch <= 10.0 {
                continue;
            }
            let marker_h = (ch * 0.22).clamp(4.0, 8.0);
            let row_pitch = 3.4;
            let base_y = cy + 3.0;
            let keyframe_lanes = clip_keyframe_lanes(clip);
            for (row, (keyframes, _property_label, _color)) in keyframe_lanes.iter().enumerate() {
                let marker_y = base_y + row as f64 * row_pitch;
                if content_y < marker_y || content_y > marker_y + marker_h {
                    continue;
                }
                for kf in *keyframes {
                    let local_time_ns = kf.time_ns.min(duration_ns);
                    let frac = local_time_ns as f64 / duration_ns as f64;
                    let marker_x = cx + frac * cw;
                    if (x - marker_x).abs() <= hit_tolerance {
                        let timeline_ns = clip.timeline_start.saturating_add(local_time_ns);
                        return Some(KeyframeMarkerHit {
                            clip_id: clip.id.clone(),
                            track_id: track.id.clone(),
                            clip_label: clip.label.clone(),
                            local_time_ns,
                            timeline_ns,
                            impacted_properties: collect_keyframe_property_labels_at_local_time(
                                clip,
                                local_time_ns,
                            ),
                        });
                    }
                }
            }
        }
        None
    }

    fn keyframe_marker_tooltip_text(&self, x: f64, y: f64) -> Option<String> {
        let hit = self.keyframe_marker_hit(x, y)?;
        let timeline_secs = hit.timeline_ns as f64 / NS_PER_SECOND;
        let timeline_label = format_timecode(timeline_secs, 0.1);
        let impacted = if hit.impacted_properties.is_empty() {
            "Unknown".to_string()
        } else {
            hit.impacted_properties.join(", ")
        };
        Some(format!(
            "Clip: {}\nKeyframe at {}\nImpacts: {}",
            hit.clip_label, timeline_label, impacted
        ))
    }

    /// Find which clip and track are at a given (x, y) coordinate.
    /// Also returns whether x is near the in-edge or out-edge (for trimming).
    fn hit_test(&self, x: f64, y: f64) -> Option<HitResult> {
        let track_idx = self.track_index_at_y(y)?;

        let proj = self.project.borrow();
        let editing_tracks = self.resolve_editing_tracks(&proj);
        let track = editing_tracks.get(track_idx)?;

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
        ClipKind::Video
        | ClipKind::Image
        | ClipKind::Title
        | ClipKind::Adjustment
        | ClipKind::Compound
        | ClipKind::Multicam
        | ClipKind::Audition
        | ClipKind::Drawing => TrackKind::Video,
    }
}

fn freeze_source_from_playhead(clip: &Clip, playhead_ns: u64) -> u64 {
    let source_duration = clip.source_duration();
    if source_duration == 0 {
        return clip.source_in;
    }
    let timeline_duration = clip.duration().max(1);
    let local_timeline_ns = playhead_ns
        .saturating_sub(clip.timeline_start)
        .min(timeline_duration.saturating_sub(1));
    let mut source_offset =
        ((local_timeline_ns as f64 / timeline_duration as f64) * source_duration as f64) as u64;
    source_offset = source_offset.min(source_duration.saturating_sub(1));
    if clip.reverse {
        clip.source_out
            .saturating_sub(1)
            .saturating_sub(source_offset)
    } else {
        clip.source_in
            .saturating_add(source_offset)
            .min(clip.source_out.saturating_sub(1))
    }
}

#[allow(deprecated)]
pub fn open_freeze_frame_dialog(state: Rc<RefCell<TimelineState>>, area: DrawingArea) {
    if !state.borrow().can_create_freeze_frame_at_playhead() {
        return;
    }

    let parent = area.root().and_then(|r| r.downcast::<gtk::Window>().ok());
    let dialog = gtk::Dialog::builder()
        .title("Create Freeze Frame")
        .default_width(360)
        .modal(true)
        .build();
    dialog.set_transient_for(parent.as_ref());
    dialog.add_button("Cancel", gtk::ResponseType::Cancel);
    dialog.add_button("Create", gtk::ResponseType::Accept);

    let body = gtk::Box::new(gtk::Orientation::Vertical, 8);
    body.set_margin_start(16);
    body.set_margin_end(16);
    body.set_margin_top(16);
    body.set_margin_bottom(16);

    let hold_label = gtk::Label::new(Some("Hold duration (seconds):"));
    hold_label.set_halign(gtk::Align::Start);
    let hold_spin = gtk::SpinButton::with_range(0.1, 60.0, 0.1);
    hold_spin.set_digits(1);
    hold_spin.set_value(2.0);
    hold_spin.set_halign(gtk::Align::Start);
    hold_spin.set_hexpand(false);
    let hint = gtk::Label::new(Some("Freeze clips are video-only and silent by default."));
    hint.set_halign(gtk::Align::Start);
    hint.add_css_class("dim-label");

    body.append(&hold_label);
    body.append(&hold_spin);
    body.append(&hint);
    dialog.content_area().append(&body);

    dialog.connect_response(move |d, resp| {
        if resp == gtk::ResponseType::Accept {
            let hold_duration_ns = (hold_spin.value().max(0.1) * NS_PER_SECOND).round() as u64;
            let mut st = state.borrow_mut();
            let changed = st.create_freeze_frame_from_selected_at_playhead(hold_duration_ns);
            let sel_cb = if changed {
                st.on_clip_selected.clone()
            } else {
                None
            };
            let new_sel = st.selected_clip_id.clone();
            drop(st);

            if changed {
                TimelineState::notify_project_changed(&state);
                if let Some(cb) = sel_cb {
                    cb(new_sel);
                }
                area.queue_draw();
            }
        }
        d.close();
    });

    dialog.present();
}

#[allow(deprecated)]
pub fn open_remove_silent_parts_dialog(state: Rc<RefCell<TimelineState>>) {
    if !state.borrow().can_remove_silent_parts() {
        return;
    }

    // Gather clip info before showing dialog
    let (clip_id, track_id, source_path, source_in, source_out) = {
        let st = state.borrow();
        let ids = st.selected_ids_or_primary();
        let clip_id = ids.iter().next().unwrap().clone();
        let proj = st.project.borrow();
        let editing_tracks = st.resolve_editing_tracks(&proj);
        let mut info = None;
        for track in editing_tracks {
            if let Some(c) = track.clips.iter().find(|c| c.id == clip_id) {
                info = Some((
                    c.id.clone(),
                    track.id.clone(),
                    c.source_path.clone(),
                    c.source_in,
                    c.source_out,
                ));
                break;
            }
        }
        match info {
            Some(i) => i,
            None => return,
        }
    };

    let dialog = gtk::Dialog::builder()
        .title("Remove Silent Parts")
        .default_width(360)
        .modal(true)
        .build();
    dialog.add_button("Cancel", gtk::ResponseType::Cancel);
    dialog.add_button("Remove", gtk::ResponseType::Accept);

    let body = gtk::Box::new(gtk::Orientation::Vertical, 8);
    body.set_margin_start(16);
    body.set_margin_end(16);
    body.set_margin_top(16);
    body.set_margin_bottom(16);

    let noise_label = gtk::Label::new(Some("Silence threshold (dBFS):"));
    noise_label.set_halign(gtk::Align::Start);
    let noise_row = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    let noise_spin = gtk::SpinButton::with_range(-60.0, -10.0, 1.0);
    noise_spin.set_digits(0);
    noise_spin.set_value(-50.0);
    noise_spin.set_halign(gtk::Align::Start);
    noise_spin.set_hexpand(false);
    let suggest_btn = gtk::Button::with_label("Suggest");
    suggest_btn.set_tooltip_text(Some(
        "Analyze the clip's noise floor with ffmpeg astats and pick a threshold automatically",
    ));
    let suggest_status = gtk::Label::new(None);
    suggest_status.add_css_class("dim-label");
    suggest_status.set_halign(gtk::Align::Start);
    noise_row.append(&noise_spin);
    noise_row.append(&suggest_btn);
    noise_row.append(&suggest_status);

    let dur_label = gtk::Label::new(Some("Minimum silence duration (seconds):"));
    dur_label.set_halign(gtk::Align::Start);
    let dur_spin = gtk::SpinButton::with_range(0.1, 5.0, 0.1);
    dur_spin.set_digits(1);
    dur_spin.set_value(0.5);
    dur_spin.set_halign(gtk::Align::Start);
    dur_spin.set_hexpand(false);

    let hint = gtk::Label::new(Some(
        "Audio below the threshold is considered silence (VU meter scale).\n\
         Green zone starts at \u{2212}18 dBFS. Try \u{2212}50 for speech, \u{2212}40 for noisy rooms.\n\
         Click Suggest to auto-pick from the clip's measured noise floor.",
    ));
    hint.set_halign(gtk::Align::Start);
    hint.add_css_class("dim-label");

    body.append(&noise_label);
    body.append(&noise_row);
    body.append(&dur_label);
    body.append(&dur_spin);
    body.append(&hint);
    dialog.content_area().append(&body);

    // Wire Suggest button — runs ffmpeg astats synchronously and updates the
    // threshold spin button. Uses the same helper as the inspector's voice
    // isolation Suggest button.
    {
        let noise_spin_c = noise_spin.clone();
        let suggest_status_c = suggest_status.clone();
        let source_path_c = source_path.clone();
        let source_in_c = source_in;
        let source_out_c = source_out;
        suggest_btn.connect_clicked(move |btn| {
            btn.set_sensitive(false);
            suggest_status_c.set_text("Analyzing\u{2026}");
            // Process pending GTK events so the status label paints before we block.
            while gtk::glib::MainContext::default().iteration(false) {}
            match crate::media::export::suggest_silence_threshold_db(
                &source_path_c,
                source_in_c,
                source_out_c,
            ) {
                Ok(db) => {
                    noise_spin_c.set_value(db as f64);
                    suggest_status_c.set_text(&format!("Suggested: {db:.0} dBFS"));
                }
                Err(e) => {
                    log::warn!("remove silent parts: suggest failed: {e}");
                    suggest_status_c.set_text("Analysis failed");
                }
            }
            btn.set_sensitive(true);
        });
    }

    dialog.connect_response(move |d, resp| {
        if resp == gtk::ResponseType::Accept {
            let noise_db = noise_spin.value();
            let min_duration = dur_spin.value().max(0.1);
            let st = state.borrow();
            if let Some(ref cb) = st.on_remove_silent_parts {
                cb(
                    clip_id.clone(),
                    track_id.clone(),
                    source_path.clone(),
                    source_in,
                    source_out,
                    noise_db,
                    min_duration,
                );
            }
        }
        d.close();
    });

    dialog.present();
}

#[allow(deprecated)]
pub fn open_detect_scene_cuts_dialog(state: Rc<RefCell<TimelineState>>) {
    if !state.borrow().can_detect_scene_cuts() {
        return;
    }

    let (clip_id, track_id, source_path, source_in, source_out) = {
        let st = state.borrow();
        let ids = st.selected_ids_or_primary();
        let clip_id = ids.iter().next().unwrap().clone();
        let proj = st.project.borrow();
        let editing_tracks = st.resolve_editing_tracks(&proj);
        let mut info = None;
        for track in editing_tracks {
            if let Some(c) = track.clips.iter().find(|c| c.id == clip_id) {
                info = Some((
                    c.id.clone(),
                    track.id.clone(),
                    c.source_path.clone(),
                    c.source_in,
                    c.source_out,
                ));
                break;
            }
        }
        match info {
            Some(i) => i,
            None => return,
        }
    };

    let dialog = gtk::Dialog::builder()
        .title("Detect Scene Cuts")
        .default_width(360)
        .modal(true)
        .build();
    dialog.add_button("Cancel", gtk::ResponseType::Cancel);
    dialog.add_button("Detect", gtk::ResponseType::Accept);

    let body = gtk::Box::new(gtk::Orientation::Vertical, 8);
    body.set_margin_start(16);
    body.set_margin_end(16);
    body.set_margin_top(16);
    body.set_margin_bottom(16);

    let threshold_label = gtk::Label::new(Some("Scene change threshold:"));
    threshold_label.set_halign(gtk::Align::Start);
    let threshold_spin = gtk::SpinButton::with_range(1.0, 50.0, 1.0);
    threshold_spin.set_digits(0);
    threshold_spin.set_value(10.0);
    threshold_spin.set_halign(gtk::Align::Start);
    threshold_spin.set_hexpand(false);

    let hint = gtk::Label::new(Some(
        "Lower values detect more cuts (including subtle changes).\n\
         Try 10 for obvious scene changes, 5 for subtle cuts.",
    ));
    hint.set_halign(gtk::Align::Start);
    hint.add_css_class("dim-label");

    body.append(&threshold_label);
    body.append(&threshold_spin);
    body.append(&hint);
    dialog.content_area().append(&body);

    dialog.connect_response(move |d, resp| {
        if resp == gtk::ResponseType::Accept {
            let threshold = threshold_spin.value();
            let st = state.borrow();
            if let Some(ref cb) = st.on_detect_scene_cuts {
                cb(
                    clip_id.clone(),
                    track_id.clone(),
                    source_path.clone(),
                    source_in,
                    source_out,
                    threshold,
                );
            }
        }
        d.close();
    });

    dialog.present();
}

#[allow(deprecated)]
pub fn open_convert_ltc_dialog(state: Rc<RefCell<TimelineState>>) {
    if !state.borrow().can_convert_ltc_to_timecode() {
        return;
    }

    let (clip_id, clip_label, default_frame_rate_label) = {
        let st = state.borrow();
        let ids = st.selected_ids_or_primary();
        let Some(clip_id) = ids.iter().next().cloned() else {
            return;
        };
        let proj = st.project.borrow();
        let Some(clip) = proj.clip_ref(&clip_id) else {
            return;
        };
        let clip_label = if clip.label.trim().is_empty() {
            clip.source_path.clone()
        } else {
            clip.label.clone()
        };
        let default_frame_rate_label = format!("{:.3} fps", proj.frame_rate.as_f64());
        (clip_id, clip_label, default_frame_rate_label)
    };

    let dialog = gtk::Dialog::builder()
        .title("Convert LTC Audio to Timecode")
        .default_width(420)
        .modal(true)
        .build();
    dialog.add_button("Cancel", gtk::ResponseType::Cancel);
    dialog.add_button("Convert", gtk::ResponseType::Accept);

    let body = gtk::Box::new(gtk::Orientation::Vertical, 8);
    body.set_margin_start(16);
    body.set_margin_end(16);
    body.set_margin_top(16);
    body.set_margin_bottom(16);

    let intro = gtk::Label::new(Some(&format!(
        "Decode LTC from \"{clip_label}\" and store the result as source timecode metadata."
    )));
    intro.set_halign(gtk::Align::Start);
    intro.set_wrap(true);

    let channel_label = gtk::Label::new(Some("LTC source channel:"));
    channel_label.set_halign(gtk::Align::Start);
    let channel_model = gtk::StringList::new(&[
        LtcChannelSelection::Auto.label(),
        LtcChannelSelection::Left.label(),
        LtcChannelSelection::Right.label(),
        LtcChannelSelection::MonoMix.label(),
    ]);
    let channel_dropdown = gtk::DropDown::new(Some(channel_model), gtk4::Expression::NONE);
    channel_dropdown.set_selected(0);
    channel_dropdown.set_halign(gtk::Align::Start);

    let frame_rate_label = gtk::Label::new(Some("Timecode frame rate:"));
    frame_rate_label.set_halign(gtk::Align::Start);
    let frame_rate_model = gtk::StringList::new(&[
        "Project / Source Default",
        "23.976 fps",
        "24 fps",
        "25 fps",
        "29.97 fps",
        "30 fps",
    ]);
    let frame_rate_dropdown = gtk::DropDown::new(Some(frame_rate_model), gtk4::Expression::NONE);
    frame_rate_dropdown.set_selected(0);
    frame_rate_dropdown.set_halign(gtk::Align::Start);

    let hint = gtk::Label::new(Some(&format!(
        "When LTC is isolated on the left or right side, the opposite side will be played on both speakers. Default frame rate: {default_frame_rate_label}."
    )));
    hint.set_halign(gtk::Align::Start);
    hint.set_wrap(true);
    hint.add_css_class("dim-label");

    body.append(&intro);
    body.append(&channel_label);
    body.append(&channel_dropdown);
    body.append(&frame_rate_label);
    body.append(&frame_rate_dropdown);
    body.append(&hint);
    dialog.content_area().append(&body);

    dialog.connect_response(move |d, response| {
        if response == gtk::ResponseType::Accept {
            let channel = match channel_dropdown.selected() {
                1 => LtcChannelSelection::Left,
                2 => LtcChannelSelection::Right,
                3 => LtcChannelSelection::MonoMix,
                _ => LtcChannelSelection::Auto,
            };
            let frame_rate = match frame_rate_dropdown.selected() {
                1 => Some(FrameRate {
                    numerator: 24_000,
                    denominator: 1_001,
                }),
                2 => Some(FrameRate {
                    numerator: 24,
                    denominator: 1,
                }),
                3 => Some(FrameRate {
                    numerator: 25,
                    denominator: 1,
                }),
                4 => Some(FrameRate {
                    numerator: 30_000,
                    denominator: 1_001,
                }),
                5 => Some(FrameRate {
                    numerator: 30,
                    denominator: 1,
                }),
                _ => None,
            };
            let callback = state.borrow().on_convert_ltc_to_timecode.clone();
            if let Some(callback) = callback {
                callback(clip_id.clone(), channel, frame_rate);
            }
        }
        d.close();
    });

    dialog.present();
}

fn apply_pasted_attributes(target: &mut Clip, source: &Clip) -> bool {
    let before = target.clone();
    // Color grading (primary)
    target.brightness = source.brightness;
    target.contrast = source.contrast;
    target.saturation = source.saturation;
    target.temperature = source.temperature;
    target.tint = source.tint;
    // Color grading (extended)
    target.exposure = source.exposure;
    target.black_point = source.black_point;
    target.shadows = source.shadows;
    target.midtones = source.midtones;
    target.highlights = source.highlights;
    target.highlights_warmth = source.highlights_warmth;
    target.highlights_tint = source.highlights_tint;
    target.midtones_warmth = source.midtones_warmth;
    target.midtones_tint = source.midtones_tint;
    target.shadows_warmth = source.shadows_warmth;
    target.shadows_tint = source.shadows_tint;
    // Enhancement
    target.denoise = source.denoise;
    target.sharpness = source.sharpness;
    target.blur = source.blur;
    target.blur_keyframes = source.blur_keyframes.clone();
    target.lut_paths = source.lut_paths.clone();
    // Audio
    target.volume = source.volume;
    target.pan = source.pan;
    // Video effects
    target.speed = source.speed;
    target.crop_left = source.crop_left;
    target.crop_right = source.crop_right;
    target.crop_top = source.crop_top;
    target.crop_bottom = source.crop_bottom;
    target.rotate = source.rotate;
    target.flip_h = source.flip_h;
    target.flip_v = source.flip_v;
    // Title/Text overlay
    target.title_text = source.title_text.clone();
    target.title_font = source.title_font.clone();
    target.title_color = source.title_color;
    target.title_x = source.title_x;
    target.title_y = source.title_y;
    // Transitions
    target.outgoing_transition = source.outgoing_transition.clone();
    // Transform
    target.scale = source.scale;
    target.opacity = source.opacity;
    target.position_x = source.position_x;
    target.position_y = source.position_y;
    target.blend_mode = source.blend_mode;
    // Reverse & Freeze-frame
    target.reverse = source.reverse;
    target.freeze_frame = source.freeze_frame;
    target.freeze_frame_source_ns = source.freeze_frame_source_ns;
    target.freeze_frame_hold_duration_ns = source.freeze_frame_hold_duration_ns;
    // Chroma key & BG removal
    target.chroma_key_enabled = source.chroma_key_enabled;
    target.chroma_key_color = source.chroma_key_color;
    target.chroma_key_tolerance = source.chroma_key_tolerance;
    target.chroma_key_softness = source.chroma_key_softness;
    target.bg_removal_enabled = source.bg_removal_enabled;
    target.bg_removal_threshold = source.bg_removal_threshold;
    // Frei0r effects — clone with fresh UUIDs
    target.frei0r_effects = source
        .frei0r_effects
        .iter()
        .map(|e| {
            let mut new_effect = e.clone();
            new_effect.id = uuid::Uuid::new_v4().to_string();
            new_effect
        })
        .collect();
    before != *target
}

fn through_edit_metadata_compatible(left: &Clip, right: &Clip) -> bool {
    let mut left_norm = left.clone();
    left_norm.id.clear();
    left_norm.source_in = 0;
    left_norm.source_out = 0;
    left_norm.timeline_start = 0;
    left_norm.clear_outgoing_transition();

    let mut right_norm = right.clone();
    right_norm.id.clear();
    right_norm.source_in = 0;
    right_norm.source_out = 0;
    right_norm.timeline_start = 0;
    right_norm.clear_outgoing_transition();

    left_norm == right_norm
}

fn merge_through_edit_clips(left: &Clip, right: &Clip) -> Clip {
    let mut merged = left.clone();
    merged.source_out = right.source_out;
    merged.outgoing_transition = right.outgoing_transition.clone();
    merged
}

struct HitResult {
    clip_id: String,
    track_id: String,
    #[allow(dead_code)]
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

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct ClipContextMenuActionability {
    join_through_edit: bool,
    freeze_frame: bool,
    link_selected: bool,
    unlink_selected: bool,
    align_grouped: bool,
    sync_audio: bool,
    sync_replace_audio: bool,
    remove_silent_parts: bool,
    detect_scene_cuts: bool,
    convert_ltc: bool,
    split_stereo: bool,
    create_compound: bool,
    break_apart_compound: bool,
    create_multicam: bool,
}

impl ClipContextMenuActionability {
    fn any(self) -> bool {
        self.join_through_edit
            || self.freeze_frame
            || self.link_selected
            || self.unlink_selected
            || self.align_grouped
            || self.sync_audio
            || self.sync_replace_audio
            || self.remove_silent_parts
            || self.detect_scene_cuts
            || self.convert_ltc
            || self.split_stereo
            || self.create_compound
            || self.break_apart_compound
            || self.create_multicam
    }
}

fn apply_clip_context_menu_actionability(
    btn_join_through_edit: &gtk::Button,
    btn_freeze_frame: &gtk::Button,
    btn_link_selected: &gtk::Button,
    btn_unlink_selected: &gtk::Button,
    btn_align_grouped: &gtk::Button,
    btn_sync_audio: &gtk::Button,
    btn_sync_replace_audio: &gtk::Button,
    btn_remove_silent_parts: &gtk::Button,
    btn_detect_scene_cuts: &gtk::Button,
    btn_convert_ltc: &gtk::Button,
    btn_split_stereo: &gtk::Button,
    btn_create_compound: &gtk::Button,
    btn_break_apart_compound: &gtk::Button,
    btn_create_multicam: &gtk::Button,
    actionability: ClipContextMenuActionability,
) -> bool {
    let set_state = |button: &gtk::Button, actionable: bool| {
        button.set_visible(actionable);
        button.set_sensitive(actionable);
    };
    set_state(btn_join_through_edit, actionability.join_through_edit);
    set_state(btn_freeze_frame, actionability.freeze_frame);
    set_state(btn_link_selected, actionability.link_selected);
    set_state(btn_unlink_selected, actionability.unlink_selected);
    set_state(btn_align_grouped, actionability.align_grouped);
    set_state(btn_sync_audio, actionability.sync_audio);
    set_state(btn_sync_replace_audio, actionability.sync_replace_audio);
    set_state(btn_remove_silent_parts, actionability.remove_silent_parts);
    set_state(btn_detect_scene_cuts, actionability.detect_scene_cuts);
    set_state(btn_convert_ltc, actionability.convert_ltc);
    set_state(btn_split_stereo, actionability.split_stereo);
    set_state(btn_create_compound, actionability.create_compound);
    set_state(btn_break_apart_compound, actionability.break_apart_compound);
    set_state(btn_create_multicam, actionability.create_multicam);
    actionability.any()
}

/// Build and return the scrollable track-stack `DrawingArea`.
pub fn build_timeline(
    state: Rc<RefCell<TimelineState>>,
    ruler_area: Rc<RefCell<Option<DrawingArea>>>,
) -> DrawingArea {
    let area = DrawingArea::new();
    area.set_vexpand(true);
    area.set_hexpand(true);
    area.set_halign(gtk::Align::Fill);
    area.set_focusable(true);
    area.set_has_tooltip(true);
    {
        let state = state.clone();
        area.connect_query_tooltip(move |_area, x, y, _keyboard_mode, tooltip| {
            let text = state
                .borrow()
                .keyframe_marker_tooltip_text(x as f64, y as f64);
            if let Some(text) = text {
                tooltip.set_text(Some(&text));
                true
            } else {
                false
            }
        });
    }

    let thumb_cache = Rc::new(RefCell::new(
        crate::media::thumb_cache::ThumbnailCache::new(),
    ));

    let wave_cache = Rc::new(RefCell::new(
        crate::media::waveform_cache::WaveformCache::new(),
    ));

    let clip_context_pop = gtk::Popover::new();
    clip_context_pop.set_parent(&area);
    clip_context_pop.set_autohide(true);
    let clip_context_box = gtk::Box::new(gtk::Orientation::Vertical, 2);
    clip_context_box.set_margin_start(4);
    clip_context_box.set_margin_end(4);
    clip_context_box.set_margin_top(4);
    clip_context_box.set_margin_bottom(4);
    let btn_join_through_edit = gtk::Button::with_label("Join Through Edit");
    btn_join_through_edit.add_css_class("flat");
    btn_join_through_edit
        .set_tooltip_text(Some("Join selected through-edit boundary (Ctrl+Shift+B)"));
    let btn_freeze_frame = gtk::Button::with_label("Create Freeze Frame…");
    btn_freeze_frame.add_css_class("flat");
    btn_freeze_frame.set_tooltip_text(Some(
        "Create a freeze-frame clip from the selected clip (Shift+F)",
    ));
    let btn_link_selected = gtk::Button::with_label("Link Selected Clips");
    btn_link_selected.add_css_class("flat");
    btn_link_selected.set_tooltip_text(Some("Link the current selection (Ctrl+L)"));
    let btn_unlink_selected = gtk::Button::with_label("Unlink Selected Clips");
    btn_unlink_selected.add_css_class("flat");
    btn_unlink_selected.set_tooltip_text(Some("Unlink the current selection (Ctrl+Shift+L)"));
    let btn_align_grouped = gtk::Button::with_label("Align Grouped Clips by Timecode");
    btn_align_grouped.add_css_class("flat");
    btn_align_grouped.set_tooltip_text(Some(
        "Align grouped clips using stored source timecode metadata when available",
    ));
    let btn_sync_audio = gtk::Button::with_label("Sync Selected Clips by Audio");
    btn_sync_audio.add_css_class("flat");
    btn_sync_audio.set_tooltip_text(Some(
        "Align selected clips using audio cross-correlation (requires 2+ clips with audio)",
    ));
    let btn_sync_replace_audio = gtk::Button::with_label("Sync & Replace Audio");
    btn_sync_replace_audio.add_css_class("flat");
    btn_sync_replace_audio.set_tooltip_text(Some(
        "Sync by audio, then link clips and mute camera audio so external audio replaces it",
    ));
    let btn_remove_silent_parts = gtk::Button::with_label("Remove Silent Parts\u{2026}");
    btn_remove_silent_parts.add_css_class("flat");
    btn_remove_silent_parts.set_tooltip_text(Some(
        "Detect and remove silent segments from this clip using ffmpeg silencedetect",
    ));
    clip_context_box.append(&btn_join_through_edit);
    clip_context_box.append(&btn_freeze_frame);
    clip_context_box.append(&btn_link_selected);
    clip_context_box.append(&btn_unlink_selected);
    clip_context_box.append(&btn_align_grouped);
    clip_context_box.append(&btn_sync_audio);
    clip_context_box.append(&btn_sync_replace_audio);
    clip_context_box.append(&btn_remove_silent_parts);
    let btn_detect_scene_cuts = gtk::Button::with_label("Detect Scene Cuts\u{2026}");
    btn_detect_scene_cuts.add_css_class("flat");
    btn_detect_scene_cuts.set_tooltip_text(Some(
        "Detect scene/shot changes and split this clip at each cut point using ffmpeg scdet",
    ));
    clip_context_box.append(&btn_detect_scene_cuts);
    let btn_convert_ltc = gtk::Button::with_label("Convert LTC Audio to Timecode\u{2026}");
    btn_convert_ltc.add_css_class("flat");
    btn_convert_ltc.set_tooltip_text(Some(
        "Decode LTC from the selected clip and store it as source timecode metadata",
    ));
    clip_context_box.append(&btn_convert_ltc);
    let btn_split_stereo = gtk::Button::with_label("Split Stereo to Mono Tracks");
    btn_split_stereo.add_css_class("flat");
    btn_split_stereo.set_tooltip_text(Some(
        "Create left and right mono clips from selected stereo audio on separate tracks",
    ));
    clip_context_box.append(&btn_split_stereo);
    let btn_create_compound = gtk::Button::with_label("Create Compound Clip");
    btn_create_compound.add_css_class("flat");
    btn_create_compound.set_tooltip_text(Some(
        "Nest selected clips into a single compound clip (Alt+G)",
    ));
    let btn_break_apart_compound = gtk::Button::with_label("Break Apart Compound Clip");
    btn_break_apart_compound.add_css_class("flat");
    btn_break_apart_compound
        .set_tooltip_text(Some("Expand compound clip back into its constituent clips"));
    clip_context_box.append(&btn_create_compound);
    clip_context_box.append(&btn_break_apart_compound);
    let btn_create_multicam = gtk::Button::with_label("Create Multicam Clip");
    btn_create_multicam.add_css_class("flat");
    btn_create_multicam.set_tooltip_text(Some(
        "Sync selected clips by audio and create a multicam clip (Alt+M)",
    ));
    clip_context_box.append(&btn_create_multicam);
    let btn_create_audition = gtk::Button::with_label("Create Audition from Selection");
    btn_create_audition.add_css_class("flat");
    btn_create_audition.set_tooltip_text(Some(
        "Group selected clips into a single audition clip with one active take",
    ));
    clip_context_box.append(&btn_create_audition);
    let btn_finalize_audition = gtk::Button::with_label("Finalize Audition");
    btn_finalize_audition.add_css_class("flat");
    btn_finalize_audition.set_tooltip_text(Some(
        "Collapse this audition to a normal clip using only the active take",
    ));
    clip_context_box.append(&btn_finalize_audition);
    // Script-to-Timeline: Re-order by Script
    let btn_reorder_by_script = gtk::Button::with_label("Re-order by Script");
    btn_reorder_by_script.add_css_class("flat");
    btn_reorder_by_script.set_tooltip_text(Some(
        "Re-sort clips on this track by screenplay scene order",
    ));
    clip_context_box.append(&btn_reorder_by_script);

    clip_context_pop.set_child(Some(&clip_context_box));

    let track_context_pop = gtk::Popover::new();
    track_context_pop.set_parent(&area);
    track_context_pop.set_autohide(true);
    let track_context_box = gtk::Box::new(gtk::Orientation::Vertical, 2);
    track_context_box.set_margin_start(4);
    track_context_box.set_margin_end(4);
    track_context_box.set_margin_top(4);
    track_context_box.set_margin_bottom(4);
    let btn_track_height_small = gtk::Button::with_label("Track Height: Small");
    let btn_track_height_medium = gtk::Button::with_label("Track Height: Medium");
    let btn_track_height_large = gtk::Button::with_label("Track Height: Large");
    let btn_add_adjustment_layer = gtk::Button::with_label("Add Adjustment Layer");
    for btn in [
        &btn_track_height_small,
        &btn_track_height_medium,
        &btn_track_height_large,
    ] {
        btn.add_css_class("flat");
        track_context_box.append(btn);
    }
    let btn_rename_track = gtk::Button::with_label("Rename\u{2026}");
    btn_rename_track.add_css_class("flat");
    track_context_box.append(&btn_rename_track);
    btn_add_adjustment_layer.add_css_class("flat");
    track_context_box.append(&btn_add_adjustment_layer);
    let btn_generate_music = gtk::Button::with_label("Generate Music\u{2026}");
    btn_generate_music.add_css_class("flat");
    btn_generate_music.set_tooltip_text(Some(
        "Generate music from a text prompt using MusicGen AI and place on this track",
    ));
    track_context_box.append(&btn_generate_music);
    let btn_generate_music_region = gtk::Button::with_label("Generate Music Region\u{2026}");
    btn_generate_music_region.add_css_class("flat");
    btn_generate_music_region.set_tooltip_text(Some(
        "Arm a one-shot drag on empty audio-track space to define a MusicGen region (1–30s)",
    ));
    track_context_box.append(&btn_generate_music_region);

    // --- Track Color submenu ---
    let color_label_section = gtk::Box::new(gtk::Orientation::Vertical, 0);
    let color_separator = gtk::Separator::new(gtk::Orientation::Horizontal);
    color_separator.set_margin_top(4);
    color_separator.set_margin_bottom(2);
    color_label_section.append(&color_separator);
    let color_header = gtk::Label::new(Some("Track Color"));
    color_header.set_halign(gtk::Align::Start);
    color_header.add_css_class("dim-label");
    color_header.set_margin_start(6);
    color_label_section.append(&color_header);
    let color_row = gtk::Box::new(gtk::Orientation::Horizontal, 2);
    color_row.set_margin_start(4);
    color_row.set_margin_top(2);
    color_row.set_margin_bottom(2);
    let track_color_buttons: Vec<(gtk::Button, crate::model::track::TrackColorLabel)> =
        crate::model::track::TrackColorLabel::ALL
            .iter()
            .map(|&color| {
                let btn = gtk::Button::new();
                btn.set_size_request(18, 18);
                btn.add_css_class("flat");
                if color == crate::model::track::TrackColorLabel::None {
                    btn.set_label("⊘");
                    btn.set_tooltip_text(Some("None"));
                } else {
                    btn.set_label("●");
                    btn.set_tooltip_text(Some(color.label()));
                }
                color_row.append(&btn);
                (btn, color)
            })
            .collect();
    color_label_section.append(&color_row);
    track_context_box.append(&color_label_section);

    track_context_pop.set_child(Some(&track_context_box));
    let track_context_track_idx: Rc<RefCell<Option<usize>>> = Rc::new(RefCell::new(None));

    // Track rename popover (text Entry + Done button). Opened from the
    // "Rename…" item in the track context menu, or from a double-click on
    // the track label area. The track ID is held in `rename_target_track_id`
    // for the duration the popover is open.
    let rename_pop = gtk::Popover::new();
    rename_pop.set_parent(&area);
    rename_pop.set_autohide(true);
    let rename_box = gtk::Box::new(gtk::Orientation::Horizontal, 4);
    rename_box.set_margin_start(4);
    rename_box.set_margin_end(4);
    rename_box.set_margin_top(4);
    rename_box.set_margin_bottom(4);
    let rename_entry = gtk::Entry::new();
    rename_entry.set_width_chars(16);
    rename_entry.set_activates_default(true);
    let rename_done_btn = gtk::Button::with_label("Done");
    rename_done_btn.add_css_class("suggested-action");
    rename_done_btn.set_receives_default(true);
    rename_box.append(&rename_entry);
    rename_box.append(&rename_done_btn);
    rename_pop.set_child(Some(&rename_box));
    rename_pop.set_default_widget(Some(&rename_done_btn));
    let rename_target_track_id: Rc<RefCell<Option<String>>> = Rc::new(RefCell::new(None));

    // Clear pending state if the popover is dismissed without committing.
    {
        let rename_target_track_id = rename_target_track_id.clone();
        rename_pop.connect_closed(move |_| {
            rename_target_track_id.borrow_mut().take();
        });
    }

    // Commit handler — used by both the Done button and Entry activation.
    let commit_rename: Rc<dyn Fn()> = {
        let state = state.clone();
        let area_weak = area.downgrade();
        let rename_pop_weak = rename_pop.downgrade();
        let rename_entry = rename_entry.clone();
        let rename_target_track_id = rename_target_track_id.clone();
        Rc::new(move || {
            let new_label = rename_entry.text().to_string();
            let track_id = rename_target_track_id.borrow_mut().take();
            if let Some(track_id) = track_id {
                let mut st = state.borrow_mut();
                let changed = st.rename_track(&track_id, new_label);
                drop(st);
                if changed {
                    TimelineState::notify_project_changed(&state);
                    if let Some(a) = area_weak.upgrade() {
                        a.queue_draw();
                    }
                }
            }
            if let Some(pop) = rename_pop_weak.upgrade() {
                pop.popdown();
            }
        })
    };

    {
        let commit_rename = commit_rename.clone();
        rename_done_btn.connect_clicked(move |_| commit_rename());
    }
    {
        let commit_rename = commit_rename.clone();
        rename_entry.connect_activate(move |_| commit_rename());
    }

    // Helper closure: open the rename popover for a given track id, anchored
    // at a screen-space rectangle on the timeline drawing area.
    let open_rename_popover: Rc<dyn Fn(String, gtk::gdk::Rectangle)> = {
        let state = state.clone();
        let rename_pop = rename_pop.clone();
        let rename_entry = rename_entry.clone();
        let rename_target_track_id = rename_target_track_id.clone();
        Rc::new(move |track_id: String, anchor: gtk::gdk::Rectangle| {
            let current_label = {
                let st = state.borrow();
                let proj = st.project.borrow();
                proj.track_ref(&track_id).map(|t| t.label.clone())
            };
            let Some(current_label) = current_label else {
                return;
            };
            *rename_target_track_id.borrow_mut() = Some(track_id);
            rename_entry.set_text(&current_label);
            rename_entry.select_region(0, -1);
            rename_pop.set_pointing_to(Some(&anchor));
            rename_pop.popup();
            // Defer focus until the popover is realized so the entry actually
            // receives the cursor.
            let entry_weak = rename_entry.downgrade();
            glib::idle_add_local_once(move || {
                if let Some(entry) = entry_weak.upgrade() {
                    entry.grab_focus();
                    entry.select_region(0, -1);
                }
            });
        })
    };

    {
        let state = state.clone();
        let area = area.clone();
        let pop_weak = clip_context_pop.downgrade();
        let area_weak = area.downgrade();
        btn_join_through_edit.connect_clicked(move |_| {
            let mut st = state.borrow_mut();
            let changed = st.join_selected_through_edit();
            let sel_cb = if changed {
                st.on_clip_selected.clone()
            } else {
                None
            };
            let new_sel = st.selected_clip_id.clone();
            drop(st);
            if let Some(pop) = pop_weak.upgrade() {
                pop.popdown();
            }
            if changed {
                TimelineState::notify_project_changed(&state);
                if let Some(cb) = sel_cb {
                    cb(new_sel);
                }
                if let Some(a) = area_weak.upgrade() {
                    a.queue_draw();
                }
            }
        });
    }

    {
        let state = state.clone();
        let area = area.clone();
        let pop_weak = clip_context_pop.downgrade();
        btn_freeze_frame.connect_clicked(move |_| {
            if let Some(pop) = pop_weak.upgrade() {
                pop.popdown();
            }
            open_freeze_frame_dialog(state.clone(), area.clone());
        });
    }

    {
        let state = state.clone();
        let area_weak = area.downgrade();
        let pop_weak = clip_context_pop.downgrade();
        btn_link_selected.connect_clicked(move |_| {
            let mut st = state.borrow_mut();
            let changed = st.link_selected_clips();
            drop(st);
            if let Some(pop) = pop_weak.upgrade() {
                pop.popdown();
            }
            if changed {
                TimelineState::notify_project_changed(&state);
                if let Some(a) = area_weak.upgrade() {
                    a.queue_draw();
                }
            }
        });
    }

    {
        let state = state.clone();
        let area_weak = area.downgrade();
        let pop_weak = clip_context_pop.downgrade();
        btn_unlink_selected.connect_clicked(move |_| {
            let mut st = state.borrow_mut();
            let changed = st.unlink_selected_clips();
            drop(st);
            if let Some(pop) = pop_weak.upgrade() {
                pop.popdown();
            }
            if changed {
                TimelineState::notify_project_changed(&state);
                if let Some(a) = area_weak.upgrade() {
                    a.queue_draw();
                }
            }
        });
    }

    {
        let state = state.clone();
        let area_weak = area.downgrade();
        let pop_weak = clip_context_pop.downgrade();
        btn_align_grouped.connect_clicked(move |_| {
            let mut st = state.borrow_mut();
            let changed = st.align_selected_groups_by_timecode();
            drop(st);
            if let Some(pop) = pop_weak.upgrade() {
                pop.popdown();
            }
            if changed {
                TimelineState::notify_project_changed(&state);
                if let Some(a) = area_weak.upgrade() {
                    a.queue_draw();
                }
            }
        });
    }

    {
        let state = state.clone();
        let pop_weak = clip_context_pop.downgrade();
        btn_sync_audio.connect_clicked(move |_| {
            let st = state.borrow();
            st.sync_selected_clips_by_audio();
            drop(st);
            if let Some(pop) = pop_weak.upgrade() {
                pop.popdown();
            }
        });
    }

    {
        let state = state.clone();
        let pop_weak = clip_context_pop.downgrade();
        btn_sync_replace_audio.connect_clicked(move |_| {
            let st = state.borrow();
            // Collect the same clip data as sync_selected_clips_by_audio,
            // but fire the replace callback instead.
            let cb = st.on_sync_replace_audio.clone();
            let data = st.collect_selected_clips_for_sync();
            drop(st);
            if let Some(pop) = pop_weak.upgrade() {
                pop.popdown();
            }
            if let (Some(cb), Some(data)) = (cb, data) {
                cb(data);
            }
        });
    }

    {
        let state = state.clone();
        let pop_weak = clip_context_pop.downgrade();
        btn_remove_silent_parts.connect_clicked(move |_| {
            if let Some(pop) = pop_weak.upgrade() {
                pop.popdown();
            }
            open_remove_silent_parts_dialog(state.clone());
        });
    }

    {
        let state = state.clone();
        let pop_weak = clip_context_pop.downgrade();
        btn_detect_scene_cuts.connect_clicked(move |_| {
            if let Some(pop) = pop_weak.upgrade() {
                pop.popdown();
            }
            open_detect_scene_cuts_dialog(state.clone());
        });
    }

    {
        let state = state.clone();
        let pop_weak = clip_context_pop.downgrade();
        btn_convert_ltc.connect_clicked(move |_| {
            if let Some(pop) = pop_weak.upgrade() {
                pop.popdown();
            }
            open_convert_ltc_dialog(state.clone());
        });
    }

    {
        let state = state.clone();
        let pop_weak = clip_context_pop.downgrade();
        let area_weak = area.downgrade();
        btn_split_stereo.connect_clicked(move |_| {
            if let Some(pop) = pop_weak.upgrade() {
                pop.popdown();
            }
            let mut st = state.borrow_mut();
            let ids = st.selected_ids_or_primary();
            let clip_id = match ids.into_iter().next() {
                Some(id) => id,
                None => return,
            };
            let changed = {
                let mut proj = st.project.borrow_mut();
                // Find the clip and its track.
                let clip_data = proj
                    .tracks
                    .iter()
                    .enumerate()
                    .flat_map(|(ti, t)| t.clips.iter().map(move |c| (ti, c)))
                    .find(|(_, c)| c.id == clip_id)
                    .map(|(ti, c)| (ti, c.clone()));
                if let Some((track_idx, original)) = clip_data {
                    let track_label = proj.tracks[track_idx].label.clone();
                    // Set original to Left channel.
                    if let Some(clip) = proj.tracks[track_idx]
                        .clips
                        .iter_mut()
                        .find(|c| c.id == clip_id)
                    {
                        clip.audio_channel_mode = crate::model::clip::AudioChannelMode::Left;
                    }
                    // Create a clone for Right channel on a new audio track.
                    let mut right_clip = original.clone();
                    right_clip.id = uuid::Uuid::new_v4().to_string();
                    right_clip.audio_channel_mode = crate::model::clip::AudioChannelMode::Right;
                    let mut new_track =
                        crate::model::track::Track::new_audio(format!("{track_label} (R)"));
                    new_track.add_clip(right_clip);
                    // Rename original track to indicate Left.
                    proj.tracks[track_idx].label = format!("{track_label} (L)");
                    proj.tracks.push(new_track);
                    proj.dirty = true;
                    true
                } else {
                    false
                }
            };
            drop(st);
            if changed {
                TimelineState::notify_project_changed(&state);
            }
            if let Some(a) = area_weak.upgrade() {
                a.queue_draw();
            }
        });
    }

    // ── Create Compound Clip button ──
    {
        let state = state.clone();
        let pop_weak = clip_context_pop.downgrade();
        let area_weak = area.downgrade();
        btn_create_compound.connect_clicked(move |_| {
            if let Some(pop) = pop_weak.upgrade() {
                pop.popdown();
            }
            let mut st = state.borrow_mut();
            let changed = st.create_compound_from_selection();
            drop(st);
            if changed {
                TimelineState::notify_project_changed(&state);
            }
            if let Some(a) = area_weak.upgrade() {
                a.queue_draw();
            }
        });
    }

    // ── Break Apart Compound Clip button ──
    {
        let state = state.clone();
        let pop_weak = clip_context_pop.downgrade();
        let area_weak = area.downgrade();
        btn_break_apart_compound.connect_clicked(move |_| {
            if let Some(pop) = pop_weak.upgrade() {
                pop.popdown();
            }
            let mut st = state.borrow_mut();
            let changed = st.break_apart_compound();
            drop(st);
            if changed {
                TimelineState::notify_project_changed(&state);
            }
            if let Some(a) = area_weak.upgrade() {
                a.queue_draw();
            }
        });
    }

    // ── Create Multicam Clip button ──
    {
        let state = state.clone();
        let pop_weak = clip_context_pop.downgrade();
        let area_weak = area.downgrade();
        btn_create_multicam.connect_clicked(move |_| {
            if let Some(pop) = pop_weak.upgrade() {
                pop.popdown();
            }
            let mut st = state.borrow_mut();
            let _ = st.request_create_multicam();
            drop(st);
            if let Some(a) = area_weak.upgrade() {
                a.queue_draw();
            }
        });
    }

    // ── Create Audition button ──
    {
        let state = state.clone();
        let pop_weak = clip_context_pop.downgrade();
        let area_weak = area.downgrade();
        btn_create_audition.connect_clicked(move |_| {
            if let Some(pop) = pop_weak.upgrade() {
                pop.popdown();
            }
            let changed = {
                let mut st = state.borrow_mut();
                st.create_audition_from_selection()
            };
            if changed {
                TimelineState::notify_project_changed(&state);
            }
            if let Some(a) = area_weak.upgrade() {
                a.queue_draw();
            }
        });
    }

    // ── Finalize Audition button ──
    {
        let state = state.clone();
        let pop_weak = clip_context_pop.downgrade();
        let area_weak = area.downgrade();
        btn_finalize_audition.connect_clicked(move |_| {
            if let Some(pop) = pop_weak.upgrade() {
                pop.popdown();
            }
            let changed = {
                let mut st = state.borrow_mut();
                st.finalize_selected_audition()
            };
            if changed {
                TimelineState::notify_project_changed(&state);
            }
            if let Some(a) = area_weak.upgrade() {
                a.queue_draw();
            }
        });
    }

    // Script-to-Timeline: Re-order by Script handler
    {
        let state = state.clone();
        let area_weak = area.downgrade();
        let pop_weak = clip_context_pop.downgrade();
        btn_reorder_by_script.connect_clicked(move |_| {
            if let Some(pop) = pop_weak.upgrade() {
                pop.popdown();
            }
            let changed = reorder_track_by_script(&state);
            if changed {
                TimelineState::notify_project_changed(&state);
            }
            if let Some(a) = area_weak.upgrade() {
                a.queue_draw();
            }
        });
    }

    for (btn, preset) in [
        (
            btn_track_height_small.clone(),
            crate::model::track::TrackHeightPreset::Small,
        ),
        (
            btn_track_height_medium.clone(),
            crate::model::track::TrackHeightPreset::Medium,
        ),
        (
            btn_track_height_large.clone(),
            crate::model::track::TrackHeightPreset::Large,
        ),
    ] {
        let state = state.clone();
        let area_weak = area.downgrade();
        let pop_weak = track_context_pop.downgrade();
        let track_context_track_idx = track_context_track_idx.clone();
        btn.connect_clicked(move |_| {
            let track_idx = *track_context_track_idx.borrow();
            let mut st = state.borrow_mut();
            let changed = track_idx
                .map(|idx| st.set_track_height_preset_by_index(idx, preset))
                .unwrap_or(false);
            drop(st);
            if let Some(pop) = pop_weak.upgrade() {
                pop.popdown();
            }
            if changed {
                TimelineState::notify_project_changed(&state);
                if let Some(a) = area_weak.upgrade() {
                    a.queue_draw();
                }
            }
        });
    }

    // Track color button handlers
    for (btn, color) in track_color_buttons {
        let state = state.clone();
        let area_weak = area.downgrade();
        let pop_weak = track_context_pop.downgrade();
        let track_context_track_idx = track_context_track_idx.clone();
        btn.connect_clicked(move |_| {
            let track_idx = *track_context_track_idx.borrow();
            let mut st = state.borrow_mut();
            let changed = track_idx
                .map(|idx| st.set_track_color_label_by_index(idx, color))
                .unwrap_or(false);
            drop(st);
            if let Some(pop) = pop_weak.upgrade() {
                pop.popdown();
            }
            if changed {
                TimelineState::notify_project_changed(&state);
                if let Some(a) = area_weak.upgrade() {
                    a.queue_draw();
                }
            }
        });
    }

    // Rename Track button handler — closes the context menu and opens the
    // rename popover, anchored at the track's left-side label area.
    {
        let state = state.clone();
        let pop_weak = track_context_pop.downgrade();
        let track_context_track_idx = track_context_track_idx.clone();
        let open_rename_popover = open_rename_popover.clone();
        btn_rename_track.connect_clicked(move |_| {
            let track_idx = *track_context_track_idx.borrow();
            if let Some(pop) = pop_weak.upgrade() {
                pop.popdown();
            }
            let Some(idx) = track_idx else {
                return;
            };
            let anchor = {
                let st = state.borrow();
                let proj = st.project.borrow();
                let editing_tracks = st.resolve_editing_tracks(&proj);
                let Some(track) = editing_tracks.get(idx) else {
                    return;
                };
                let track_id = track.id.clone();
                let row_top = track_row_top_in_tracks(editing_tracks, idx)
                    + st.breadcrumb_bar_height()
                    - st.vertical_scroll_offset;
                let row_height = track_row_height(track);
                let rect = gtk::gdk::Rectangle::new(
                    0,
                    row_top as i32,
                    TRACK_LABEL_WIDTH as i32,
                    row_height as i32,
                );
                Some((track_id, rect))
            };
            if let Some((track_id, rect)) = anchor {
                open_rename_popover(track_id, rect);
            }
        });
    }

    // Add Adjustment Layer button handler
    {
        let state = state.clone();
        let area_weak = area.downgrade();
        let pop_weak = track_context_pop.downgrade();
        let track_context_track_idx = track_context_track_idx.clone();
        btn_add_adjustment_layer.connect_clicked(move |_| {
            let track_idx = *track_context_track_idx.borrow();
            if let Some(idx) = track_idx {
                let (playhead, track_id, is_video, proj_rc) = {
                    let st = state.borrow();
                    let proj = st.project.borrow();
                    let valid = idx < proj.tracks.len() && proj.tracks[idx].is_video();
                    let tid = proj
                        .tracks
                        .get(idx)
                        .map(|t| t.id.clone())
                        .unwrap_or_default();
                    (st.playhead_ns, tid, valid, st.project.clone())
                };
                if is_video {
                    let clip = crate::model::clip::Clip::new_adjustment(playhead, 5_000_000_000);
                    let cmd = crate::undo::AddAdjustmentLayerCommand { clip, track_id };
                    let mut st = state.borrow_mut();
                    st.history.execute(Box::new(cmd), &mut proj_rc.borrow_mut());
                    drop(st);
                    TimelineState::notify_project_changed(&state);
                    if let Some(a) = area_weak.upgrade() {
                        a.queue_draw();
                    }
                }
            }
            if let Some(pop) = pop_weak.upgrade() {
                pop.popdown();
            }
        });
    }

    {
        let state = state.clone();
        let pop_weak = track_context_pop.downgrade();
        let track_context_track_idx = track_context_track_idx.clone();
        btn_generate_music.connect_clicked(move |_| {
            let track_idx = *track_context_track_idx.borrow();
            if let Some(idx) = track_idx {
                let (playhead, track_id) = {
                    let st = state.borrow();
                    let proj = st.project.borrow();
                    let tid = st
                        .resolve_editing_tracks(&proj)
                        .get(idx)
                        .map(|t| t.id.clone())
                        .unwrap_or_default();
                    (st.playhead_ns, tid)
                };
                let st = state.borrow();
                let cb = st.on_generate_music.clone();
                drop(st);
                if let Some(cb) = cb {
                    cb(MusicGenerationTarget {
                        track_id,
                        timeline_start_ns: playhead,
                        timeline_end_ns: None,
                    });
                }
            }
            if let Some(pop) = pop_weak.upgrade() {
                pop.popdown();
            }
        });
    }

    {
        let state = state.clone();
        let pop_weak = track_context_pop.downgrade();
        let track_context_track_idx = track_context_track_idx.clone();
        let area_weak = area.downgrade();
        btn_generate_music_region.connect_clicked(move |_| {
            let track_idx = *track_context_track_idx.borrow();
            let selected_track = track_idx.and_then(|idx| {
                let st = state.borrow();
                let proj = st.project.borrow();
                st.resolve_editing_tracks(&proj)
                    .get(idx)
                    .filter(|track| track.is_audio())
                    .map(|track| (track.id.clone(), track.label.clone()))
            });
            let mut status_cb = None;
            if let Some((track_id, _)) = selected_track.as_ref() {
                let mut st = state.borrow_mut();
                st.arm_music_generation_region(track_id.clone());
                status_cb = st.on_music_generation_status.clone();
            }
            if let Some(pop) = pop_weak.upgrade() {
                pop.popdown();
            }
            if let Some(a) = area_weak.upgrade() {
                a.grab_focus();
                a.queue_draw();
            }
            if let (Some((_, track_label)), Some(cb)) = (selected_track, status_cb) {
                cb(format!(
                    "Drag on {track_label} to define a MusicGen region (1–30s)."
                ));
            }
        });
    }

    // Drawing
    {
        let state = state.clone();
        let thumb_cache = thumb_cache.clone();
        let wave_cache = wave_cache.clone();
        let ruler_area = ruler_area.clone();
        area.set_draw_func(move |area, cr, width, height| {
            let mut tcache = thumb_cache.borrow_mut();
            tcache.poll();
            let mut wcache = wave_cache.borrow_mut();
            wcache.poll();
            let st = state.borrow();
            draw_timeline(cr, width, height, &st, &mut tcache, &mut wcache, false);
            if let Some(ruler) = ruler_area.borrow().as_ref() {
                ruler.queue_draw();
            }
            if let Some(ref weak) = st.minimap_widget {
                if let Some(m) = weak.upgrade() {
                    m.queue_draw();
                }
            }
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
        let clip_context_pop = clip_context_pop.clone();
        let track_context_pop = track_context_pop.clone();
        let track_context_track_idx = track_context_track_idx.clone();
        let btn_join_through_edit = btn_join_through_edit.clone();
        let btn_freeze_frame = btn_freeze_frame.clone();
        let btn_link_selected = btn_link_selected.clone();
        let btn_unlink_selected = btn_unlink_selected.clone();
        let btn_align_grouped = btn_align_grouped.clone();
        let btn_track_height_small = btn_track_height_small.clone();
        let btn_track_height_medium = btn_track_height_medium.clone();
        let btn_track_height_large = btn_track_height_large.clone();
        let btn_add_adjustment_layer = btn_add_adjustment_layer.clone();
        let btn_generate_music = btn_generate_music.clone();
        let open_rename_popover = open_rename_popover.clone();
        click.connect_pressed(move |gesture, n_press, x, y| {
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
            clip_context_pop.popdown();
            track_context_pop.popdown();

            if ruler_hit_test(&st, y) {
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
                        drop(st);
                        TimelineState::notify_project_changed(&state);
                    }
                } else {
                    // Left-click in ruler → seek
                    let ns = st.x_to_ns(x);
                    st.set_playhead_visual(ns);
                    let seek_cb = st.on_seek.clone();
                    drop(st);
                    if let Some(cb) = seek_cb {
                        cb(ns);
                    }
                }
            } else if button == 1 {
                // Double-click on the track label area → open rename popover.
                // Must run before any other left-click handling so it doesn't
                // also seek/scrub or trigger compound drill-down.
                if n_press == 2
                    && x < TRACK_LABEL_WIDTH
                    && st.solo_badge_hit_track_index(x, y).is_none()
                    && st.duck_badge_hit_track_index(x, y).is_none()
                    && st.mute_badge_hit_track_index(x, y).is_none()
                    && st.lock_badge_hit_track_index(x, y).is_none()
                {
                    let resolved = st.track_index_at_y(y).and_then(|track_idx| {
                        let proj = st.project.borrow();
                        let editing_tracks = st.resolve_editing_tracks(&proj);
                        editing_tracks.get(track_idx).map(|track| {
                            let row_top = track_row_top_in_tracks(editing_tracks, track_idx)
                                + st.breadcrumb_bar_height()
                                - st.vertical_scroll_offset;
                            let row_height = track_row_height(track);
                            (
                                track.id.clone(),
                                gtk::gdk::Rectangle::new(
                                    0,
                                    row_top as i32,
                                    TRACK_LABEL_WIDTH as i32,
                                    row_height as i32,
                                ),
                            )
                        })
                    });
                    if let Some((track_id, anchor)) = resolved {
                        drop(st);
                        open_rename_popover(track_id, anchor);
                        return;
                    }
                }
                match st.active_tool {
                    ActiveTool::Draw => {
                        // Drawing happens on the program monitor overlay,
                        // not the timeline — ignore timeline clicks.
                    }
                    ActiveTool::Razor => {
                        // Razor cut at click position on the clicked track
                        let ns = st.x_to_ns(x);
                        st.set_playhead_visual(ns);
                        let track_idx = st.track_index_at_y(y);
                        let seek_cb = st.on_seek.clone();
                        st.razor_cut_at_playhead_on_track(track_idx);
                        drop(st);
                        if let Some(cb) = seek_cb {
                            cb(ns);
                        }
                        TimelineState::notify_project_changed(&state);
                    }
                    ActiveTool::Select
                    | ActiveTool::Ripple
                    | ActiveTool::Roll
                    | ActiveTool::Slip
                    | ActiveTool::Slide => {
                        if st.solo_badge_hit_track_index(x, y).is_some() {
                            let changed = st
                                .solo_badge_hit_track_index(x, y)
                                .map(|track_idx| st.toggle_track_solo_by_index(track_idx))
                                .unwrap_or(false);
                            drop(st);
                            if changed {
                                TimelineState::notify_project_changed(&state);
                            }
                            if let Some(a) = area_weak.upgrade() {
                                a.queue_draw();
                            }
                            return;
                        }
                        if st.duck_badge_hit_track_index(x, y).is_some() {
                            let changed = st
                                .duck_badge_hit_track_index(x, y)
                                .map(|track_idx| st.toggle_track_duck_by_index(track_idx))
                                .unwrap_or(false);
                            drop(st);
                            if changed {
                                TimelineState::notify_project_changed(&state);
                            }
                            if let Some(a) = area_weak.upgrade() {
                                a.queue_draw();
                            }
                            return;
                        }
                        if st.mute_badge_hit_track_index(x, y).is_some() {
                            let changed = st
                                .mute_badge_hit_track_index(x, y)
                                .map(|track_idx| st.toggle_track_mute_by_index(track_idx))
                                .unwrap_or(false);
                            drop(st);
                            if changed {
                                TimelineState::notify_project_changed(&state);
                            }
                            if let Some(a) = area_weak.upgrade() {
                                a.queue_draw();
                            }
                            return;
                        }
                        if st.lock_badge_hit_track_index(x, y).is_some() {
                            let changed = st
                                .lock_badge_hit_track_index(x, y)
                                .map(|track_idx| st.toggle_track_lock_by_index(track_idx))
                                .unwrap_or(false);
                            drop(st);
                            if changed {
                                TimelineState::notify_project_changed(&state);
                            }
                            if let Some(a) = area_weak.upgrade() {
                                a.queue_draw();
                            }
                            return;
                        }
                        let mods = gesture.current_event_state();
                        let shift = mods.contains(gtk::gdk::ModifierType::SHIFT_MASK);
                        let ctrl_or_meta = mods.contains(gtk::gdk::ModifierType::CONTROL_MASK)
                            || mods.contains(gtk::gdk::ModifierType::META_MASK);
                        // Select clip
                        // First, check if a keyframe marker was clicked.
                        if let Some(kf_hit) = st.keyframe_marker_hit(x, y) {
                            st.set_single_clip_selection(
                                kf_hit.clip_id.clone(),
                                kf_hit.track_id.clone(),
                            );
                            if ctrl_or_meta {
                                st.toggle_keyframe_selection(&kf_hit.clip_id, kf_hit.local_time_ns);
                            } else {
                                st.set_single_keyframe_selection(
                                    kf_hit.clip_id.clone(),
                                    kf_hit.local_time_ns,
                                );
                            }
                            st.set_playhead_visual(kf_hit.timeline_ns);
                            let seek_cb = st.on_seek.clone();
                            let sel_cb = st.on_clip_selected.clone();
                            let new_sel = st.selected_clip_id.clone();
                            drop(st);
                            if let Some(cb) = seek_cb {
                                cb(kf_hit.timeline_ns);
                            }
                            if let Some(cb) = sel_cb {
                                cb(new_sel);
                            }
                            return;
                        }
                        let hit = st.hit_test(x, y);
                        st.clear_keyframe_selection();
                        match hit {
                            Some(h) => {
                                st.select_clip_with_modifiers(
                                    &h.clip_id,
                                    &h.track_id,
                                    shift,
                                    ctrl_or_meta,
                                );
                            }
                            None => {
                                if !shift && !ctrl_or_meta {
                                    st.clear_clip_selection();
                                    // Click on empty track area still selects the track
                                    let tid = {
                                        let proj = st.project.borrow();
                                        let editing_tracks = st.resolve_editing_tracks(&proj);
                                        st.track_index_at_y(y)
                                            .and_then(|track_idx| editing_tracks.get(track_idx))
                                            .map(|t| t.id.clone())
                                    };
                                    st.selected_track_id = tid;
                                } else {
                                }
                            }
                        }
                        // Double-click on compound clip → drill in
                        if n_press == 2 {
                            if let Some(ref clip_id) = st.selected_clip_id.clone() {
                                let is_compound = {
                                    let proj = st.project.borrow();
                                    let editing_tracks = st.resolve_editing_tracks(&proj);
                                    editing_tracks
                                        .iter()
                                        .flat_map(|t| t.clips.iter())
                                        .any(|c| c.id == *clip_id && c.is_compound())
                                };
                                if is_compound {
                                    st.enter_compound(clip_id.clone());
                                    drop(st);
                                    TimelineState::notify_project_changed(&state);
                                    if let Some(a) = area_weak.upgrade() {
                                        a.queue_draw();
                                    }
                                    return;
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
                let track_idx = st.track_index_at_y(y).unwrap_or(usize::MAX);
                let ns = st.x_to_ns(x);
                let threshold_ns = ((12.0 / st.pixels_per_second) * NS_PER_SECOND as f64) as u64;
                let transition_hit = {
                    let proj = st.project.borrow();
                    let editing_tracks = st.resolve_editing_tracks(&proj);
                    editing_tracks.get(track_idx).and_then(|track| {
                        track
                            .clips
                            .iter()
                            .filter(|c| c.outgoing_transition.is_active())
                            .filter_map(|c| {
                                let diff = c.timeline_end().abs_diff(ns);
                                if diff <= threshold_ns {
                                    Some((
                                        c.id.clone(),
                                        track.id.clone(),
                                        c.outgoing_transition.clone(),
                                        diff,
                                    ))
                                } else {
                                    None
                                }
                            })
                            .min_by_key(|(_, _, _, diff)| *diff)
                            .map(|(clip_id, track_id, old_transition, _)| {
                                (clip_id, track_id, old_transition)
                            })
                    })
                };

                if let Some((clip_id, track_id, old_transition)) = transition_hit {
                    let cmd = crate::undo::SetClipTransitionCommand {
                        clip_id,
                        track_id,
                        old_transition,
                        new_transition: OutgoingTransition::default(),
                    };
                    let project_rc = st.project.clone();
                    let mut proj = project_rc.borrow_mut();
                    st.history.execute(Box::new(cmd), &mut proj);
                    drop(proj);
                    drop(st);
                    TimelineState::notify_project_changed(&state);
                } else {
                    let mut show_context_menu = false;
                    let mut show_track_context_menu = false;
                    let mut sel_cb: Option<Rc<dyn Fn(Option<String>)>> = None;
                    let mut new_sel: Option<String> = None;

                    if x < TRACK_LABEL_WIDTH
                        && st.solo_badge_hit_track_index(x, y).is_none()
                        && st.duck_badge_hit_track_index(x, y).is_none()
                        && st.mute_badge_hit_track_index(x, y).is_none()
                        && st.lock_badge_hit_track_index(x, y).is_none()
                    {
                        if let Some(track_idx) = st.track_index_at_y(y) {
                            let selected = {
                                let proj = st.project.borrow();
                                let editing_tracks = st.resolve_editing_tracks(&proj);
                                editing_tracks.get(track_idx).map(|track| {
                                    (track.id.clone(), track.height_preset, track.kind)
                                })
                            };
                            if let Some((track_id, preset, track_kind)) = selected {
                                st.selected_track_id = Some(track_id);
                                *track_context_track_idx.borrow_mut() = Some(track_idx);
                                btn_track_height_small.set_sensitive(
                                    preset != crate::model::track::TrackHeightPreset::Small,
                                );
                                btn_track_height_medium.set_sensitive(
                                    preset != crate::model::track::TrackHeightPreset::Medium,
                                );
                                btn_track_height_large.set_sensitive(
                                    preset != crate::model::track::TrackHeightPreset::Large,
                                );
                                btn_add_adjustment_layer
                                    .set_visible(track_kind == TrackKind::Video);
                                btn_generate_music.set_visible(track_kind == TrackKind::Audio);
                                btn_generate_music_region
                                    .set_visible(track_kind == TrackKind::Audio);
                                track_context_pop.set_pointing_to(Some(&gtk::gdk::Rectangle::new(
                                    x as i32, y as i32, 1, 1,
                                )));
                                show_track_context_menu = true;
                            }
                        }
                    } else {
                        // Right-click clip → keep current selection if already selected, otherwise
                        // select just the clicked clip before showing clip actions.
                        let hit = st.hit_test(x, y);
                        if let Some(h) = hit {
                            st.prepare_clip_context_menu_selection(&h.clip_id, &h.track_id);
                            let actionability = st.clip_context_menu_actionability();
                            if apply_clip_context_menu_actionability(
                                &btn_join_through_edit,
                                &btn_freeze_frame,
                                &btn_link_selected,
                                &btn_unlink_selected,
                                &btn_align_grouped,
                                &btn_sync_audio,
                                &btn_sync_replace_audio,
                                &btn_remove_silent_parts,
                                &btn_detect_scene_cuts,
                                &btn_convert_ltc,
                                &btn_split_stereo,
                                &btn_create_compound,
                                &btn_break_apart_compound,
                                &btn_create_multicam,
                                actionability,
                            ) {
                                clip_context_pop.set_pointing_to(Some(&gtk::gdk::Rectangle::new(
                                    x as i32, y as i32, 1, 1,
                                )));
                                show_context_menu = true;
                            }
                        }
                        sel_cb = st.on_clip_selected.clone();
                        new_sel = st.selected_clip_id.clone();
                    }

                    drop(st);
                    if let Some(cb) = sel_cb {
                        cb(new_sel);
                    }
                    if show_track_context_menu {
                        track_context_pop.popup();
                    } else if show_context_menu {
                        clip_context_pop.popup();
                    }
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
                if ruler_hit_test(&st, y) {
                    // On drag-begin in ruler: record start offset for panning;
                    // also seek playhead to clicked position.
                    let ns = st.x_to_ns(x);
                    st.set_playhead_visual(ns);
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
                if st.music_generation_armed_track_id.is_some() {
                    let started = st.begin_music_generation_region_drag(x, y);
                    drop(st);
                    if started {
                        if let Some(a) = area_weak.upgrade() {
                            a.queue_draw();
                        }
                    }
                    return;
                }
                if !matches!(
                    st.active_tool,
                    ActiveTool::Select | ActiveTool::Ripple | ActiveTool::Slip | ActiveTool::Slide
                ) {
                    return;
                }

                let mods = gesture.current_event_state();
                let shift = mods.contains(gtk::gdk::ModifierType::SHIFT_MASK);
                let ctrl_or_meta = mods.contains(gtk::gdk::ModifierType::CONTROL_MASK)
                    || mods.contains(gtk::gdk::ModifierType::META_MASK);
                let alt = mods.contains(gtk::gdk::ModifierType::ALT_MASK);

                if st.active_tool == ActiveTool::Select {
                    if let Some(kf_hit) = st.keyframe_marker_hit(x, y) {
                        let mut selected_times =
                            st.selected_keyframe_local_times_for_clip(&kf_hit.clip_id);
                        if selected_times.is_empty()
                            || !(ctrl_or_meta || shift)
                            || !st.is_keyframe_local_time_selected(
                                &kf_hit.clip_id,
                                kf_hit.local_time_ns,
                            )
                        {
                            selected_times.clear();
                            selected_times.insert(kf_hit.local_time_ns);
                        }
                        let track_snapshot = {
                            let proj = st.project.borrow();
                            proj.track_ref(&kf_hit.track_id)
                                .map(|t| t.clips.clone())
                                .unwrap_or_default()
                        };
                        st.set_single_clip_selection(
                            kf_hit.clip_id.clone(),
                            kf_hit.track_id.clone(),
                        );
                        st.selected_keyframe_local_times
                            .insert(kf_hit.clip_id.clone(), selected_times.clone());
                        let mut ordered_times = selected_times.into_iter().collect::<Vec<_>>();
                        ordered_times.sort_unstable();
                        st.drag_op = DragOp::MoveKeyframeColumns {
                            clip_id: kf_hit.clip_id,
                            track_id: kf_hit.track_id,
                            original_track_clips: track_snapshot,
                            original_selected_local_times: ordered_times,
                            anchor_local_ns: kf_hit.local_time_ns,
                        };
                        return;
                    }

                    if alt {
                        let hit = st.hit_test(x, y);
                        if let Some(h) = hit {
                            if h.zone == HitZone::Body {
                                let local_ns = {
                                    let timeline_ns = st.x_to_ns(x);
                                    let proj = st.project.borrow();
                                    proj.clip_ref(&h.clip_id)
                                        .map(|clip| clip.local_timeline_position_ns(timeline_ns))
                                        .unwrap_or(0)
                                };
                                st.set_single_clip_selection(h.clip_id.clone(), h.track_id.clone());
                                st.begin_keyframe_marquee_selection(
                                    h.clip_id,
                                    h.track_id,
                                    local_ns,
                                    ctrl_or_meta,
                                );
                                return;
                            }
                        }
                    }
                }

                let hit = st.hit_test(x, y);
                if let Some(h) = hit {
                    // Extract clip data before mutating st (avoids borrow conflict)
                    let (clip_data, track_snapshot) = {
                        let proj = st.project.borrow();
                        let clip_data = proj
                            .clip_ref(&h.clip_id)
                            .map(|c| (c.timeline_start, c.source_in, c.source_out));
                        let track_snapshot = proj
                            .track_ref(&h.track_id)
                            .map(|t| t.clips.clone())
                            .unwrap_or_default();
                        (clip_data, track_snapshot)
                    };
                    if let Some((tl_start, src_in, src_out)) = clip_data {
                        let offset_ns = st.x_to_ns(x).saturating_sub(tl_start);
                        let click_ns = st.x_to_ns(x);
                        let move_clip_ids = st.move_clip_ids_for_drag(&h.clip_id);
                        let move_clip_set: HashSet<String> =
                            move_clip_ids.iter().cloned().collect();
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
                        // Preserve modifier-driven multi-select from click handling;
                        // drag-begin should not collapse Ctrl/Shift selections.
                        if !shift && !ctrl_or_meta {
                            if move_clip_ids.len() > 1 {
                                st.set_selection_with_primary(h.clip_id, h.track_id, move_clip_set);
                            } else {
                                st.set_single_clip_selection(h.clip_id, h.track_id);
                            }
                        }
                    }
                } else if x < TRACK_LABEL_WIDTH
                    && !ruler_hit_test(&st, y)
                    && st.solo_badge_hit_track_index(x, y).is_none()
                    && st.duck_badge_hit_track_index(x, y).is_none()
                    && st.mute_badge_hit_track_index(x, y).is_none()
                    && st.lock_badge_hit_track_index(x, y).is_none()
                {
                    // Drag started in track label area → track reorder
                    if let Some(track_idx) = st.track_index_at_y(y) {
                        st.drag_op = DragOp::ReorderTrack {
                            track_idx,
                            target_idx: track_idx,
                        };
                    }
                } else if st.active_tool == ActiveTool::Select && !ruler_hit_test(&st, y) {
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

                let ruler_drag = {
                    let st = state.borrow();
                    ruler_hit_test(&st, start_y)
                };
                if ruler_drag {
                    if button == 2 || button == 3 {
                        // Middle/right drag on ruler = pan timeline.
                        let mut st = state.borrow_mut();
                        st.scroll_offset = (st.ruler_pan_start_offset - offset_x).max(0.0);
                        st.user_scroll_cooldown_until =
                            Some(std::time::Instant::now() + std::time::Duration::from_millis(600));
                        if let Some(a) = area_weak.upgrade() {
                            a.queue_draw();
                        }
                    } else {
                        // Left drag on ruler = continuous scrubbing.
                        let mut st = state.borrow_mut();
                        let ns = st.x_to_ns(current_x);
                        st.set_playhead_visual(ns);
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
                if st.update_music_generation_region_drag(current_x) {
                    drop(st);
                    if let Some(a) = area_weak.upgrade() {
                        a.queue_draw();
                    }
                    return;
                }
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
                            let snap_ns =
                                (SNAP_TOLERANCE_PX / st.pixels_per_second * NS_PER_SECOND) as i64;
                            let eph = st.editing_playhead_ns();
                            let at_root = st.compound_nav_stack.is_empty();
                            let (snap_start, hit) = {
                                let proj = st.project.borrow();
                                let editing_tracks = st.resolve_editing_tracks(&proj);
                                let this_dur = editing_tracks
                                    .iter()
                                    .flat_map(|t| t.clips.iter())
                                    .find(|c| &c.id == clip_id)
                                    .map(|c| c.duration())
                                    .unwrap_or(0);
                                let mut cands: Vec<(u64, &'static str)> = Vec::new();
                                cands.push((0, "start"));
                                cands.push((eph, "playhead"));
                                if at_root {
                                    for m in &proj.markers {
                                        cands.push((m.position_ns, "marker"));
                                    }
                                }
                                for t in editing_tracks.iter() {
                                    for c in &t.clips {
                                        if move_set.contains(&c.id) {
                                            continue;
                                        }
                                        cands.push((c.timeline_start, "clip start"));
                                        cands.push((c.timeline_end(), "clip end"));
                                    }
                                }
                                let (s_start, h_start) =
                                    snap_to_candidates(raw_start as i64, snap_ns, &cands);
                                let desired_end = raw_start as i64 + this_dur as i64;
                                let (s_end_target, h_end) =
                                    snap_to_candidates(desired_end, snap_ns, &cands);
                                let s_end = s_end_target - this_dur as i64;
                                let d_start = (s_start - raw_start as i64).abs();
                                let d_end = (s_end - raw_start as i64).abs();
                                match (h_start, h_end) {
                                    (Some(hs), Some(he)) => {
                                        if d_start <= d_end {
                                            (s_start.max(0) as u64, Some(hs))
                                        } else {
                                            (s_end.max(0) as u64, Some(he))
                                        }
                                    }
                                    (Some(hs), None) => (s_start.max(0) as u64, Some(hs)),
                                    (None, Some(he)) => (s_end.max(0) as u64, Some(he)),
                                    (None, None) => (raw_start, None),
                                }
                            };
                            st.active_snap_hit = hit;
                            let delta = snap_start as i64 - original_start as i64;
                            let mut proj = st.project.borrow_mut();
                            for (member_id, member_start) in original_member_starts {
                                if let Some(clip) = proj.clip_mut(member_id) {
                                    clip.timeline_start =
                                        (i128::from(*member_start) + i128::from(delta)).max(0)
                                            as u64;
                                }
                            }
                            let editing_tracks = st.resolve_editing_tracks_mut(&mut proj);
                            for track in editing_tracks.iter_mut() {
                                if track.clips.iter().any(|c| move_set.contains(&c.id)) {
                                    track.sort_clips();
                                }
                            }
                        } else {
                            // ── Determine target track from y position ──────────
                            let target_track_idx = st.track_index_at_y(current_y).unwrap_or(0);
                            let (target_track_id, same_kind) = {
                                let proj = st.project.borrow();
                                let editing_tracks = st.resolve_editing_tracks(&proj);
                                let cur_kind = editing_tracks
                                    .iter()
                                    .find(|t| &t.id == current_track_id)
                                    .map(|t| t.kind);
                                match editing_tracks.get(target_track_idx) {
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
                                        let from = proj.track_mut(current_track_id);
                                        from.and_then(|t| {
                                            let pos = t.clips.iter().position(|c| &c.id == clip_id);
                                            pos.map(|i| t.clips.remove(i))
                                        })
                                    };
                                    if let Some(mut clip) = extracted {
                                        clip.timeline_start = raw_start;
                                        if let Some(to_track) = proj.track_mut(new_tid) {
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

                            let snap_ns =
                                (SNAP_TOLERANCE_PX / st.pixels_per_second * NS_PER_SECOND) as i64;
                            let eph = st.editing_playhead_ns();
                            let at_root = st.compound_nav_stack.is_empty();
                            let (snap_start, hit) = {
                                let proj = st.project.borrow();
                                let editing_tracks = st.resolve_editing_tracks(&proj);
                                let this_dur = editing_tracks
                                    .iter()
                                    .flat_map(|t| t.clips.iter())
                                    .find(|c| &c.id == clip_id)
                                    .map(|c| c.duration())
                                    .unwrap_or(0);
                                let mut cands: Vec<(u64, &'static str)> = Vec::new();
                                cands.push((0, "start"));
                                cands.push((eph, "playhead"));
                                if at_root {
                                    for m in &proj.markers {
                                        cands.push((m.position_ns, "marker"));
                                    }
                                }
                                for t in editing_tracks.iter() {
                                    for c in &t.clips {
                                        if &c.id == clip_id {
                                            continue;
                                        }
                                        cands.push((c.timeline_start, "clip start"));
                                        cands.push((c.timeline_end(), "clip end"));
                                    }
                                }
                                let (s_start, h_start) =
                                    snap_to_candidates(raw_start as i64, snap_ns, &cands);
                                let desired_end = raw_start as i64 + this_dur as i64;
                                let (s_end_target, h_end) =
                                    snap_to_candidates(desired_end, snap_ns, &cands);
                                let s_end = s_end_target - this_dur as i64;
                                let d_start = (s_start - raw_start as i64).abs();
                                let d_end = (s_end - raw_start as i64).abs();
                                match (h_start, h_end) {
                                    (Some(hs), Some(he)) => {
                                        if d_start <= d_end {
                                            (s_start.max(0) as u64, Some(hs))
                                        } else {
                                            (s_end.max(0) as u64, Some(he))
                                        }
                                    }
                                    (Some(hs), None) => (s_start.max(0) as u64, Some(hs)),
                                    (None, Some(he)) => (s_end.max(0) as u64, Some(he)),
                                    (None, None) => (raw_start, None),
                                }
                            };
                            st.active_snap_hit = hit;
                            let mut proj = st.project.borrow_mut();
                            if let Some(track) = proj.track_mut(&active_track_id) {
                                if let Some(clip) =
                                    track.clips.iter_mut().find(|c| &c.id == clip_id)
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
                        let snap_ns =
                            (SNAP_TOLERANCE_PX / st.pixels_per_second * NS_PER_SECOND) as i64;
                        let new_start_raw =
                            (original_timeline_start as i64 + drag_ns).max(0) as u64;

                        let eph = st.editing_playhead_ns();
                        let at_root = st.compound_nav_stack.is_empty();
                        let (snapped_start, hit) = {
                            let proj = st.project.borrow();
                            let editing_tracks = st.resolve_editing_tracks(&proj);
                            let mut cands: Vec<(u64, &'static str)> = Vec::new();
                            cands.push((0, "start"));
                            cands.push((eph, "playhead"));
                            if at_root {
                                for m in &proj.markers {
                                    cands.push((m.position_ns, "marker"));
                                }
                            }
                            for t in editing_tracks.iter() {
                                for c in &t.clips {
                                    if &c.id == clip_id {
                                        continue;
                                    }
                                    cands.push((c.timeline_start, "clip start"));
                                    cands.push((c.timeline_end(), "clip end"));
                                }
                            }
                            let (s, h) = snap_to_candidates(new_start_raw as i64, snap_ns, &cands);
                            (s.max(0) as u64, h)
                        };
                        st.active_snap_hit = hit;

                        let snapped_drag = snapped_start as i64 - original_timeline_start as i64;

                        let mut proj = st.project.borrow_mut();
                        if let Some(track) = proj.track_mut(track_id) {
                            // 1. Update the trimmed clip
                            let mut new_ts = original_timeline_start;
                            if let Some(clip) = track.clips.iter_mut().find(|c| &c.id == clip_id) {
                                let source_drag = clip.timeline_to_source_delta(snapped_drag);
                                let new_source_in =
                                    (original_source_in as i64 + source_drag).max(0) as u64;
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
                        // Snap the out-point to nearby clip edges, playhead, markers, or 0.
                        let snap_ns =
                            (SNAP_TOLERANCE_PX / st.pixels_per_second * NS_PER_SECOND) as i64;
                        let eph = st.editing_playhead_ns();
                        let at_root = st.compound_nav_stack.is_empty();
                        let (snapped_ns, hit) = {
                            let proj = st.project.borrow();
                            let editing_tracks = st.resolve_editing_tracks(&proj);
                            let mut cands: Vec<(u64, &'static str)> = Vec::new();
                            cands.push((eph, "playhead"));
                            if at_root {
                                for m in &proj.markers {
                                    cands.push((m.position_ns, "marker"));
                                }
                            }
                            for t in editing_tracks.iter() {
                                for c in &t.clips {
                                    if &c.id == clip_id {
                                        continue;
                                    }
                                    cands.push((c.timeline_start, "clip start"));
                                    cands.push((c.timeline_end(), "clip end"));
                                }
                            }
                            let (s, h) = snap_to_candidates(current_ns as i64, snap_ns, &cands);
                            (s.max(0) as u64, h)
                        };
                        st.active_snap_hit = hit;
                        let mut proj = st.project.borrow_mut();
                        if let Some(track) = proj.track_mut(track_id) {
                            // Find original clip data to compute stable delta
                            if let Some(orig_clip) =
                                original_track_clips.iter().find(|c| &c.id == clip_id)
                            {
                                // Calculate new source_out based on original start
                                let new_timeline_end = snapped_ns;
                                let tl_start = orig_clip.timeline_start;

                                if new_timeline_end > tl_start + 1_000_000 {
                                    let new_dur = new_timeline_end - tl_start;
                                    let new_source_dur = orig_clip.timeline_to_source_dur(new_dur);
                                    let mut new_source_out = orig_clip.source_in + new_source_dur;
                                    if let Some(max) = orig_clip.max_source_out() {
                                        new_source_out = new_source_out.min(max);
                                    }

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
                                        let tl_offset =
                                            snapped_ns.saturating_sub(clip.timeline_start);
                                        let source_offset = clip.timeline_to_source_dur(tl_offset);
                                        clip.source_out = clip.source_in + source_offset;
                                        clip.clamp_source_out();
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
                        if let Some(track) = proj.track_mut(&track_id) {
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
                                    let new_tl_dur = new_cut_pos - left.timeline_start;
                                    let new_source_dur = left.timeline_to_source_dur(new_tl_dur);
                                    left.source_out = left.source_in + new_source_dur;
                                    left.clamp_source_out();
                                }
                                // Update Right
                                if let Some(right) =
                                    track.clips.iter_mut().find(|c| &c.id == &right_clip_id)
                                {
                                    let source_drag = right.timeline_to_source_delta(drag_ns);
                                    let new_right_in =
                                        (original_right_in as i64 + source_drag).max(0) as u64;
                                    right.source_in = new_right_in;
                                    right.timeline_start = new_cut_pos;
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
                        let tl_delta = current_ns as i64 - drag_start_ns as i64;
                        let mut proj = st.project.borrow_mut();
                        if let Some(track) = proj.track_mut(track_id) {
                            if let Some(clip) = track.clips.iter_mut().find(|c| &c.id == clip_id) {
                                let source_delta = clip.timeline_to_source_delta(tl_delta);
                                let mut new_source_in =
                                    (original_source_in as i64 + source_delta).max(0) as u64;
                                let mut new_source_out = (original_source_out as i64 + source_delta)
                                    .max(new_source_in as i64 + 1_000_000)
                                    as u64;
                                // Clamp out to media duration
                                if let Some(max) = clip.max_source_out() {
                                    if new_source_out > max {
                                        let over = new_source_out - max;
                                        new_source_out = max;
                                        new_source_in = new_source_in.saturating_sub(over);
                                    }
                                }
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
                        let requested_delta = i128::from(current_ns) - i128::from(drag_start_ns);
                        let mut proj = st.project.borrow_mut();
                        if let Some(track) = proj.track_mut(track_id) {
                            let left_bounds = if let (Some(ref lid), Some(orig_out)) =
                                (left_clip_id, original_left_out)
                            {
                                track
                                    .clips
                                    .iter()
                                    .find(|c| &c.id == lid)
                                    .map(|c| (orig_out, c.source_in))
                            } else {
                                None
                            };
                            let right_bounds =
                                if let (Some(ref rid), Some(orig_in), Some(_orig_rs)) =
                                    (right_clip_id, original_right_in, original_right_start)
                                {
                                    track
                                        .clips
                                        .iter()
                                        .find(|c| &c.id == rid)
                                        .map(|c| (orig_in, c.source_out))
                                } else {
                                    None
                                };

                            let clamped_delta =
                                clamp_slide_delta(requested_delta, left_bounds, right_bounds);
                            let new_start =
                                (i128::from(original_start) + clamped_delta).max(0) as u64;

                            if let Some(clip) = track.clips.iter_mut().find(|c| &c.id == clip_id) {
                                clip.timeline_start = new_start;
                            }

                            if let (Some(ref lid), Some((orig_out, left_in))) =
                                (left_clip_id, left_bounds)
                            {
                                if let Some(left) = track.clips.iter_mut().find(|c| &c.id == lid) {
                                    left.source_out = (i128::from(orig_out) + clamped_delta)
                                        .max(i128::from(left_in) + 1_000_000)
                                        as u64;
                                    left.clamp_source_out();
                                }
                            }

                            if let (Some(ref rid), Some((orig_in, right_out)), Some(orig_rs)) =
                                (right_clip_id, right_bounds, original_right_start)
                            {
                                if let Some(right) = track.clips.iter_mut().find(|c| &c.id == rid) {
                                    let max_in = i128::from(right_out).saturating_sub(1_000_000);
                                    right.source_in = (i128::from(orig_in) + clamped_delta)
                                        .clamp(0, max_in)
                                        as u64;
                                    right.timeline_start =
                                        (i128::from(orig_rs) + clamped_delta).max(0) as u64;
                                }
                            }
                        }
                    }
                    DragOp::ReorderTrack { track_idx: _, .. } => {
                        let new_target = st.track_index_at_y(current_y).unwrap_or(0);
                        if let DragOp::ReorderTrack {
                            ref mut target_idx, ..
                        } = st.drag_op
                        {
                            *target_idx = new_target;
                        }
                    }
                    DragOp::MoveKeyframeColumns {
                        ref clip_id,
                        ref track_id,
                        ref original_track_clips,
                        ref original_selected_local_times,
                        anchor_local_ns,
                    } => {
                        let Some(original_clip) = original_track_clips
                            .iter()
                            .find(|c| &c.id == clip_id)
                            .cloned()
                        else {
                            return;
                        };
                        let clip_duration = original_clip.duration();
                        if clip_duration == 0 {
                            return;
                        }
                        let raw_local_ns = current_ns
                            .saturating_sub(original_clip.timeline_start)
                            .min(clip_duration);
                        let delta = i128::from(raw_local_ns) - i128::from(anchor_local_ns);
                        let mut source_times = original_selected_local_times.clone();
                        source_times.sort_unstable();
                        source_times.dedup();
                        let move_map = source_times
                            .into_iter()
                            .map(|from| {
                                let to = (i128::from(from) + delta)
                                    .clamp(0, i128::from(clip_duration))
                                    as u64;
                                (from, to)
                            })
                            .collect::<Vec<_>>();
                        let moved_times = move_map.iter().map(|(_, to)| *to).collect::<Vec<_>>();
                        let mut proj = st.project.borrow_mut();
                        if let Some(track) = proj.track_mut(track_id) {
                            if let Some(pos) = track.clips.iter().position(|c| &c.id == clip_id) {
                                let mut moved = original_clip;
                                moved.move_all_phase1_keyframes_local_ns(&move_map);
                                track.clips[pos] = moved;
                            }
                        }
                        drop(proj);
                        let selected_times = st
                            .selected_keyframe_local_times
                            .entry(clip_id.clone())
                            .or_default();
                        selected_times.clear();
                        selected_times.extend(moved_times);
                    }
                    DragOp::None => {
                        if st.marquee_selection.is_some() {
                            st.update_marquee_selection(current_x, current_y);
                        }
                        if let Some(marquee) = st.keyframe_marquee_selection.clone() {
                            let timeline_ns = st.x_to_ns(current_x);
                            let current_local_ns = {
                                let proj = st.project.borrow();
                                let editing_tracks = st.resolve_editing_tracks(&proj);
                                editing_tracks
                                    .iter()
                                    .find(|t| t.id == marquee.track_id)
                                    .and_then(|track| {
                                        track.clips.iter().find(|c| c.id == marquee.clip_id)
                                    })
                                    .map(|clip| clip.local_timeline_position_ns(timeline_ns))
                                    .unwrap_or(marquee.start_local_ns)
                            };
                            st.update_keyframe_marquee_selection(current_local_ns);
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
            let area_weak = area_weak.clone();
            move |_, _, _| {
                let mut st = state.borrow_mut();
                let music_generation_outcome = st.finish_music_generation_region_drag();
                let drag_op = std::mem::replace(&mut st.drag_op, DragOp::None);
                st.active_snap_hit = None;
                let should_notify_project = !matches!(&drag_op, DragOp::None);
                let had_marquee = st.marquee_selection.is_some();
                if had_marquee {
                    st.end_marquee_selection();
                }
                let had_keyframe_marquee = st.keyframe_marquee_selection.is_some();
                if had_keyframe_marquee {
                    st.end_keyframe_marquee_selection();
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
                                    proj.track_ref(current_track_id)
                                        .map(|t| t.clips.clone())
                                        .unwrap_or_default()
                                };
                                compact_gap_free_clips(&mut new_clips);
                                if cross_track {
                                    // Also compact the original (source) track
                                    let mut orig_clips_now = {
                                        let proj = st.project.borrow();
                                        proj.track_ref(original_track_id)
                                            .map(|t| t.clips.clone())
                                            .unwrap_or_default()
                                    };
                                    compact_gap_free_clips(&mut orig_clips_now);
                                    // Apply both compacted states
                                    {
                                        let mut proj = st.project.borrow_mut();
                                        if let Some(t) = proj
                                            .tracks
                                            .iter_mut()
                                            .find(|t| &t.id == current_track_id)
                                        {
                                            t.clips = new_clips;
                                        }
                                        if let Some(t) = proj
                                            .tracks
                                            .iter_mut()
                                            .find(|t| &t.id == original_track_id)
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
                                    proj.track_ref(current_track_id)
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
                                proj.track_ref(track_id)
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
                                proj.track_ref(track_id)
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
                                proj.track_ref(track_id)
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
                                proj.track_ref(track_id)
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
                                proj.track_ref(track_id)
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
                            if let Some(track) = proj.track_ref(track_id) {
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
                            proj.track_ref(track_id)
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
                        let track = proj.track_ref(track_id);
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
                    DragOp::MoveKeyframeColumns {
                        ref track_id,
                        ref original_track_clips,
                        ..
                    } => {
                        let new_track_clips = {
                            let proj = st.project.borrow();
                            proj.track_ref(track_id)
                                .map(|t| t.clips.clone())
                                .unwrap_or_default()
                        };
                        if &new_track_clips != original_track_clips {
                            let cmd = SetTrackClipsCommand {
                                track_id: track_id.clone(),
                                old_clips: original_track_clips.clone(),
                                new_clips: new_track_clips,
                                label: "Move keyframe columns".to_string(),
                            };
                            let project = st.project.clone();
                            let mut proj = project.borrow_mut();
                            st.history.execute(Box::new(cmd), &mut proj);
                        }
                    }
                    DragOp::None => {}
                }

                let music_cb = matches!(music_generation_outcome, Some(Ok(_)))
                    .then(|| st.on_generate_music.clone())
                    .flatten();
                let music_status_cb = matches!(music_generation_outcome, Some(Err(_)))
                    .then(|| st.on_music_generation_status.clone())
                    .flatten();
                let sel_cb = if had_marquee || had_keyframe_marquee {
                    st.on_clip_selected.clone()
                } else {
                    None
                };
                let new_sel = st.selected_clip_id.clone();
                drop(st);
                if let Some(a) = area_weak.upgrade() {
                    a.queue_draw();
                }
                if should_notify_project {
                    TimelineState::notify_project_changed(&state);
                }
                if let Some(cb) = sel_cb {
                    cb(new_sel);
                }
                if let Some(outcome) = music_generation_outcome {
                    match outcome {
                        Ok(target) => {
                            if let Some(cb) = music_cb {
                                cb(target);
                            }
                        }
                        Err(message) => {
                            if let Some(cb) = music_status_cb {
                                cb(message);
                            }
                        }
                    }
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
        key_ctrl.connect_key_pressed(move |ctrl_ev, key, _, modifiers| {
            use gtk::gdk::Key;
            let ctrl = modifiers.contains(gtk::gdk::ModifierType::CONTROL_MASK);
            let shift = modifiers.contains(gtk::gdk::ModifierType::SHIFT_MASK);
            let alt = modifiers.contains(gtk::gdk::ModifierType::ALT_MASK);

            // Don't intercept when a text entry has focus — prevents Space
            // from triggering play/pause while editing title text, and
            // single-letter shortcuts (B, T, R, S, …) from firing while
            // the user is typing in an Entry or SearchEntry.
            if !ctrl {
                if let Some(widget) = ctrl_ev.widget() {
                    if let Some(focused) = widget.root().and_then(|r| r.focus()) {
                        if focused.is::<gtk4::Text>()
                            || focused.is::<gtk4::Entry>()
                            || focused.is::<gtk4::SearchEntry>()
                            || focused.is::<gtk4::TextView>()
                            || focused.is::<gtk4::SpinButton>()
                        {
                            return glib::Propagation::Proceed;
                        }
                    }
                }
            }

            let mut st = state.borrow_mut();

            // Track whether we need to fire on_project_changed after releasing the borrow
            let mut notify_project = false;
            let mut notify_selection = false;
            let mut notify_tool: Option<ActiveTool> = None;

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
                Key::c | Key::C if ctrl && alt => st.copy_color_grade(),
                Key::m | Key::M if ctrl && alt => {
                    if st.selected_clip_id.is_some() {
                        let cb = st.on_match_color.clone();
                        drop(st);
                        if let Some(cb) = cb {
                            cb();
                        }
                        return glib::Propagation::Stop;
                    }
                    false
                }
                Key::v | Key::V if ctrl && alt => {
                    let changed = st.paste_color_grade();
                    if changed {
                        notify_project = true;
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
                Key::l | Key::L if ctrl && shift => {
                    let changed = st.unlink_selected_clips();
                    if changed {
                        notify_project = true;
                    }
                    changed
                }
                Key::g | Key::G if alt => {
                    let changed = st.create_compound_from_selection();
                    if changed {
                        notify_project = true;
                    }
                    changed
                }
                Key::m | Key::M if alt && !ctrl => {
                    let _ = st.request_create_multicam();
                    false // async — project change fires when sync completes
                }
                Key::l | Key::L if ctrl => {
                    let changed = st.link_selected_clips();
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
                    if st.has_selected_keyframes() {
                        let changed = st.delete_selected_keyframes();
                        if changed {
                            st.clear_keyframe_selection();
                            notify_project = true;
                        }
                        changed
                    } else {
                        st.delete_selected();
                        notify_project = true;
                        true
                    }
                }
                Key::_1 | Key::KP_1 if !ctrl => {
                    if st.selected_multicam_context().is_some() {
                        let changed = st.insert_multicam_angle_switch(0);
                        if changed {
                            notify_project = true;
                        }
                        changed
                    } else {
                        let changed =
                            st.set_selected_keyframe_interpolation(KeyframeInterpolation::Linear);
                        if changed {
                            notify_project = true;
                        }
                        changed
                    }
                }
                Key::_2 | Key::KP_2 if !ctrl => {
                    if st.selected_multicam_context().is_some() {
                        let changed = st.insert_multicam_angle_switch(1);
                        if changed {
                            notify_project = true;
                        }
                        changed
                    } else {
                        let changed =
                            st.set_selected_keyframe_interpolation(KeyframeInterpolation::EaseIn);
                        if changed {
                            notify_project = true;
                        }
                        changed
                    }
                }
                Key::_3 | Key::KP_3 if !ctrl => {
                    if st.selected_multicam_context().is_some() {
                        let changed = st.insert_multicam_angle_switch(2);
                        if changed {
                            notify_project = true;
                        }
                        changed
                    } else {
                        let changed =
                            st.set_selected_keyframe_interpolation(KeyframeInterpolation::EaseOut);
                        if changed {
                            notify_project = true;
                        }
                        changed
                    }
                }
                Key::_4 | Key::KP_4 if !ctrl => {
                    if st.selected_multicam_context().is_some() {
                        let changed = st.insert_multicam_angle_switch(3);
                        if changed {
                            notify_project = true;
                        }
                        changed
                    } else {
                        let changed = st
                            .set_selected_keyframe_interpolation(KeyframeInterpolation::EaseInOut);
                        if changed {
                            notify_project = true;
                        }
                        changed
                    }
                }
                Key::_5 | Key::KP_5 if !ctrl && st.selected_multicam_context().is_some() => {
                    let changed = st.insert_multicam_angle_switch(4);
                    if changed {
                        notify_project = true;
                    }
                    changed
                }
                Key::_6 | Key::KP_6 if !ctrl && st.selected_multicam_context().is_some() => {
                    let changed = st.insert_multicam_angle_switch(5);
                    if changed {
                        notify_project = true;
                    }
                    changed
                }
                Key::_7 | Key::KP_7 if !ctrl && st.selected_multicam_context().is_some() => {
                    let changed = st.insert_multicam_angle_switch(6);
                    if changed {
                        notify_project = true;
                    }
                    changed
                }
                Key::_8 | Key::KP_8 if !ctrl && st.selected_multicam_context().is_some() => {
                    let changed = st.insert_multicam_angle_switch(7);
                    if changed {
                        notify_project = true;
                    }
                    changed
                }
                Key::_9 | Key::KP_9 if !ctrl && st.selected_multicam_context().is_some() => {
                    let changed = st.insert_multicam_angle_switch(8);
                    if changed {
                        notify_project = true;
                    }
                    changed
                }
                Key::Right if ctrl && shift => {
                    let changed = st.select_clips_forward_from_playhead();
                    if changed {
                        notify_selection = true;
                    }
                    changed
                }
                Key::Left if ctrl && shift => {
                    let changed = st.select_clips_backward_from_playhead();
                    if changed {
                        notify_selection = true;
                    }
                    changed
                }
                Key::s | Key::S if !ctrl => {
                    let changed = st.toggle_selected_track_solo();
                    if changed {
                        notify_project = true;
                    }
                    changed
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
                Key::f | Key::F if shift && !ctrl => {
                    drop(st);
                    if let Some(a) = area_weak.upgrade() {
                        open_freeze_frame_dialog(state.clone(), a);
                    }
                    return glib::Propagation::Stop;
                }
                Key::f | Key::F if !ctrl && !shift && !alt => {
                    let cb = st.on_match_frame.clone();
                    drop(st);
                    if let Some(cb) = cb {
                        cb();
                    }
                    return glib::Propagation::Stop;
                }
                Key::b | Key::B if ctrl && shift => {
                    let changed = st.join_selected_through_edit();
                    if changed {
                        notify_project = true;
                        notify_selection = true;
                    }
                    changed
                }
                Key::m | Key::M if !ctrl && !alt => {
                    // M = toggle mute on selected track
                    let changed = st.toggle_selected_track_mute();
                    if changed {
                        notify_project = true;
                    }
                    changed
                }
                Key::l | Key::L if shift && !ctrl && !alt => {
                    // Shift+L = toggle lock on selected track
                    let changed = st.toggle_selected_track_lock();
                    if changed {
                        notify_project = true;
                    }
                    changed
                }
                Key::b | Key::B => {
                    // B = Blade/Razor
                    st.active_tool = if st.active_tool == ActiveTool::Razor {
                        ActiveTool::Select
                    } else {
                        ActiveTool::Razor
                    };
                    notify_tool = Some(st.active_tool.clone());
                    true
                }
                Key::r | Key::R => {
                    // R = Ripple Edit
                    st.active_tool = if st.active_tool == ActiveTool::Ripple {
                        ActiveTool::Select
                    } else {
                        ActiveTool::Ripple
                    };
                    notify_tool = Some(st.active_tool.clone());
                    true
                }
                Key::e | Key::E => {
                    // E = Roll Edit
                    st.active_tool = if st.active_tool == ActiveTool::Roll {
                        ActiveTool::Select
                    } else {
                        ActiveTool::Roll
                    };
                    notify_tool = Some(st.active_tool.clone());
                    true
                }
                Key::y | Key::Y if !ctrl => {
                    // Y = Slip Edit
                    st.active_tool = if st.active_tool == ActiveTool::Slip {
                        ActiveTool::Select
                    } else {
                        ActiveTool::Slip
                    };
                    notify_tool = Some(st.active_tool.clone());
                    true
                }
                Key::u | Key::U if !ctrl => {
                    // U = Slide Edit
                    st.active_tool = if st.active_tool == ActiveTool::Slide {
                        ActiveTool::Select
                    } else {
                        ActiveTool::Slide
                    };
                    notify_tool = Some(st.active_tool.clone());
                    true
                }
                Key::d | Key::D if !ctrl => {
                    // D = Draw
                    st.active_tool = if st.active_tool == ActiveTool::Draw {
                        ActiveTool::Select
                    } else {
                        ActiveTool::Draw
                    };
                    notify_tool = Some(st.active_tool.clone());
                    true
                }
                Key::Escape => {
                    if st.cancel_music_generation_region() {
                        true
                    } else if st.is_editing_compound() {
                        st.exit_compound();
                        notify_project = true;
                        true
                    } else {
                        st.active_tool = ActiveTool::Select;
                        notify_tool = Some(ActiveTool::Select);
                        true
                    }
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

            let sel_cb = if notify_selection {
                st.on_clip_selected.clone()
            } else {
                None
            };
            let tool_cb = if notify_tool.is_some() {
                st.on_tool_changed.clone()
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
            if notify_project {
                TimelineState::notify_project_changed(&state);
            }
            if let Some(cb) = sel_cb {
                cb(new_sel);
            }
            if let (Some(cb), Some(tool)) = (tool_cb, notify_tool) {
                cb(tool);
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
            if ctrl_held {
                // Ctrl+scroll = zoom
                let factor = if dy < 0.0 { 1.1 } else { 0.9 };
                st.pixels_per_second = (st.pixels_per_second * factor).clamp(10.0, 2000.0);
            } else if dx.abs() > dy.abs() {
                // Horizontal pan (Shift+scroll or trackpad horizontal swipe)
                st.scroll_offset = (st.scroll_offset + dx * 20.0).max(0.0);
                st.user_scroll_cooldown_until =
                    Some(std::time::Instant::now() + std::time::Duration::from_millis(600));
            } else if dy.abs() > 0.0 {
                // Vertical track scrolling
                let viewport_h = area_weak
                    .upgrade()
                    .map(|a| a.height() as f64)
                    .unwrap_or(0.0);
                let max_scroll = {
                    let proj = st.project.borrow();
                    let editing_tracks = st.resolve_editing_tracks(&proj);
                    let content_h = timeline_content_height_for_tracks(editing_tracks)
                        + st.breadcrumb_bar_height();
                    (content_h - viewport_h).max(0.0)
                };
                st.vertical_scroll_offset =
                    (st.vertical_scroll_offset + dy * 20.0).clamp(0.0, max_scroll);
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
                let track_idx = st.track_index_at_y(y).unwrap_or(usize::MAX);
                let tns = st.x_to_ns(x);
                let threshold_ns = ((12.0 / st.pixels_per_second) * NS_PER_SECOND as f64) as u64;
                let pair = {
                    let proj = st.project.borrow();
                    let editing_tracks = st.resolve_editing_tracks(&proj);
                    editing_tracks.get(track_idx).and_then(|track| {
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
                let track_idx = st.track_index_at_y(y).unwrap_or(0);
                let tns = st.x_to_ns(x);
                let threshold_ns = ((12.0 / st.pixels_per_second) * NS_PER_SECOND as f64) as u64;
                let candidate = {
                    let proj = st.project.borrow();
                    let editing_tracks = st.resolve_editing_tracks(&proj);
                    let track = match editing_tracks.get(track_idx) {
                        Some(t) => t,
                        None => return false,
                    };
                    let mut best: Option<(String, usize, String, OutgoingTransition)> = None;
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
                                i,
                                clip.id.clone(),
                                clip.outgoing_transition.clone(),
                            ));
                            best_diff = diff;
                        }
                    }
                    best
                };
                if let Some((track_id, clip_index, clip_id, old_transition)) = candidate {
                    let validated_transition = {
                        let proj = st.project.borrow();
                        proj.tracks
                            .iter()
                            .find(|track| track.id == track_id)
                            .ok_or(())
                            .and_then(|track| {
                                validate_track_transition_request(
                                    track,
                                    clip_index,
                                    &transition_kind,
                                    DEFAULT_TRANSITION_DURATION_NS,
                                    TransitionAlignment::EndOnCut,
                                )
                                .map(|validated| validated.transition)
                                .map_err(|_| ())
                            })
                    };
                    let Ok(new_transition) = validated_transition else {
                        st.hover_transition_pair = None;
                        return false;
                    };
                    let cmd = crate::undo::SetClipTransitionCommand {
                        clip_id,
                        track_id,
                        old_transition,
                        new_transition,
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
                // Check for external file manager drop (file:// URIs) before
                // attempting to parse as an internal "{source_path}|{duration_ns}" payload.
                let external_paths = crate::ui::media_browser::parse_external_drop_paths(&payload);
                if !external_paths.is_empty() {
                    let (track_idx, timeline_start_ns) = {
                        let st = state.borrow();
                        let track_row_idx = st.track_index_at_y(y).unwrap_or(0);
                        let tns = st.x_to_ns(x);
                        (track_row_idx, tns)
                    };
                    let cb = state.borrow().on_drop_external_files.clone();
                    if let Some(cb) = cb {
                        cb(external_paths, track_idx, timeline_start_ns);
                    }
                    state.borrow_mut().hover_transition_pair = None;
                } else {
                    // Internal payload format: "{source_path}|{duration_ns}"
                    let mut parts = payload.splitn(2, '|');
                    let source_path = match parts.next() {
                        Some(p) => p.to_string(),
                        None => return false,
                    };
                    let duration_ns: u64 = parts.next().and_then(|s| s.parse().ok()).unwrap_or(0);

                    let (track_idx, timeline_start_ns) = {
                        let st = state.borrow();
                        let track_row_idx = st.track_index_at_y(y).unwrap_or(0);
                        let tns = st.x_to_ns(x);
                        (track_row_idx, tns)
                    };

                    let cb = state.borrow().on_drop_clip.clone();
                    if let Some(cb) = cb {
                        cb(source_path, duration_ns, track_idx, timeline_start_ns);
                    }
                    state.borrow_mut().hover_transition_pair = None;
                }
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

pub fn build_timeline_ruler(
    state: Rc<RefCell<TimelineState>>,
    timeline_area: DrawingArea,
) -> DrawingArea {
    let ruler = DrawingArea::new();
    ruler.set_content_height(RULER_HEIGHT.ceil() as i32);
    ruler.set_vexpand(false);
    ruler.set_hexpand(true);
    ruler.set_focusable(true);

    {
        let state = state.clone();
        ruler.set_draw_func(move |_area, cr, width, height| {
            let st = state.borrow();
            let w = width as f64;
            let h = height as f64;
            let (bg_r, bg_g, bg_b) = crate::ui::colors::COLOR_TIMELINE_BG;
            cr.set_source_rgb(bg_r, bg_g, bg_b);
            cr.paint().ok();
            draw_ruler(cr, w, &st, 0.0);
            let ph_x = st.ns_to_x(st.editing_playhead_ns());
            if h > 0.0 {
                draw_ruler_playhead_marker(cr, ph_x, 0.0);
            }
        });
    }

    {
        let state = state.clone();
        let timeline_area = timeline_area.clone();
        let ruler_weak = ruler.downgrade();
        let click = GestureClick::new();
        click.connect_pressed(move |gesture, _n_press, x, y| {
            timeline_area.grab_focus();
            if !ruler_hit_test(&state.borrow(), y) || state.borrow().loading {
                return;
            }
            let button = gesture.current_button();
            let mut st = state.borrow_mut();
            if button == 3 {
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
                    drop(st);
                    TimelineState::notify_project_changed(&state);
                } else {
                    drop(st);
                }
            } else {
                let ns = st.x_to_ns(x);
                st.set_playhead_visual(ns);
                let seek_cb = st.on_seek.clone();
                drop(st);
                if let Some(cb) = seek_cb {
                    cb(ns);
                }
            }
            timeline_area.queue_draw();
            if let Some(r) = ruler_weak.upgrade() {
                r.queue_draw();
            }
        });
        ruler.add_controller(click);
    }

    {
        let state = state.clone();
        let timeline_area = timeline_area.clone();
        let ruler_weak = ruler.downgrade();
        let drag = GestureDrag::new();
        drag.connect_drag_begin({
            let state = state.clone();
            let timeline_area = timeline_area.clone();
            let ruler_weak = ruler_weak.clone();
            move |_gesture, x, y| {
                if state.borrow().loading || !ruler_hit_test(&state.borrow(), y) {
                    return;
                }
                timeline_area.grab_focus();
                let mut st = state.borrow_mut();
                let ns = st.x_to_ns(x);
                st.set_playhead_visual(ns);
                st.ruler_pan_start_offset = st.scroll_offset;
                let seek_cb = st.on_seek.clone();
                drop(st);
                if let Some(cb) = seek_cb {
                    cb(ns);
                }
                timeline_area.queue_draw();
                if let Some(r) = ruler_weak.upgrade() {
                    r.queue_draw();
                }
            }
        });
        drag.connect_drag_update(move |gesture, offset_x, _offset_y| {
            let (start_x, _start_y) = gesture.start_point().unwrap_or((0.0, 0.0));
            let current_x = start_x + offset_x;
            let button = gesture.current_button();
            if button == 2 || button == 3 {
                let mut st = state.borrow_mut();
                st.scroll_offset = (st.ruler_pan_start_offset - offset_x).max(0.0);
                st.user_scroll_cooldown_until =
                    Some(std::time::Instant::now() + std::time::Duration::from_millis(600));
                drop(st);
            } else {
                let mut st = state.borrow_mut();
                let ns = st.x_to_ns(current_x);
                st.set_playhead_visual(ns);
                let seek_cb = st.on_seek.clone();
                drop(st);
                if let Some(cb) = seek_cb {
                    cb(ns);
                }
            }
            timeline_area.queue_draw();
            if let Some(r) = ruler_weak.upgrade() {
                r.queue_draw();
            }
        });
        ruler.add_controller(drag);
    }

    ruler
}

// ── Mini-map / overview strip ──────────────────────────────────────────

/// Height of the mini-map strip in logical pixels.
const MINIMAP_HEIGHT: f64 = 28.0;
/// Horizontal left gutter in the mini-map (mirrors track label width at
/// a reduced size for visual alignment).
const MINIMAP_GUTTER: f64 = 24.0;

/// Compute the total duration in nanoseconds across the currently-editing
/// tracks (respecting compound drill-down).
fn total_duration_ns(st: &TimelineState) -> u64 {
    let proj = st.project.borrow();
    let tracks = st.resolve_editing_tracks(&proj);
    tracks
        .iter()
        .flat_map(|t| t.clips.iter())
        .map(|c| c.timeline_start + c.duration())
        .max()
        .unwrap_or(0)
}

/// Cairo paint for the mini-map overview strip.
fn draw_minimap(cr: &gtk::cairo::Context, width: f64, height: f64, st: &TimelineState) {
    // Background
    cr.set_source_rgb(0.10, 0.10, 0.12);
    cr.paint().ok();

    let total_ns = total_duration_ns(st);
    if total_ns == 0 {
        return;
    }

    let usable_w = (width - MINIMAP_GUTTER).max(1.0);
    let minimap_pps = usable_w / (total_ns as f64 / NS_PER_SECOND);

    let proj = st.project.borrow();
    let tracks = st.resolve_editing_tracks(&proj);
    let order = visual_order(tracks);
    let track_count = order.len().max(1);
    let lane_h = ((height - 2.0) / track_count as f64).clamp(1.0, 6.0);
    let total_lanes_h = lane_h * track_count as f64;
    let lanes_top = (height - total_lanes_h) / 2.0;

    // Draw clip rectangles
    for (vis_idx, &logical_idx) in order.iter().enumerate() {
        let track = &tracks[logical_idx];
        let lane_y = lanes_top + vis_idx as f64 * lane_h;
        for clip in &track.clips {
            let cx = MINIMAP_GUTTER + (clip.timeline_start as f64 / NS_PER_SECOND) * minimap_pps;
            let cw = ((clip.duration() as f64 / NS_PER_SECOND) * minimap_pps).max(1.0);
            let (r, g, b) = clip_fill_color(clip, track.kind.clone());
            cr.set_source_rgba(r, g, b, 0.85);
            cr.rectangle(cx, lane_y, cw, (lane_h - 0.5).max(1.0));
            cr.fill().ok();
        }
    }

    // Track lane separator lines
    if track_count > 1 {
        cr.set_source_rgba(1.0, 1.0, 1.0, 0.08);
        cr.set_line_width(0.5);
        for vis_idx in 1..track_count {
            let y = (lanes_top + vis_idx as f64 * lane_h).round() + 0.25;
            cr.move_to(MINIMAP_GUTTER, y);
            cr.line_to(width, y);
        }
        cr.stroke().ok();
    }

    // Viewport rectangle
    let visible_width = width; // the main timeline viewport width
    let view_left_ns = (st.scroll_offset / st.pixels_per_second) * NS_PER_SECOND;
    let view_right_ns = ((st.scroll_offset + visible_width) / st.pixels_per_second) * NS_PER_SECOND;
    let vp_x = MINIMAP_GUTTER + (view_left_ns / NS_PER_SECOND) * minimap_pps;
    let vp_w = ((view_right_ns - view_left_ns) / NS_PER_SECOND) * minimap_pps;
    let vp_x = vp_x.max(MINIMAP_GUTTER);
    let vp_w = vp_w.min(usable_w);
    cr.set_source_rgba(1.0, 1.0, 1.0, 0.12);
    cr.rectangle(vp_x, 0.0, vp_w, height);
    cr.fill().ok();
    cr.set_source_rgba(1.0, 1.0, 1.0, 0.45);
    cr.set_line_width(1.0);
    cr.rectangle(vp_x + 0.5, 0.5, vp_w - 1.0, height - 1.0);
    cr.stroke().ok();

    // Playhead
    let ph_ns = st.editing_playhead_ns();
    let ph_x = MINIMAP_GUTTER + (ph_ns as f64 / NS_PER_SECOND) * minimap_pps;
    if ph_x >= MINIMAP_GUTTER && ph_x <= width {
        cr.set_source_rgba(1.0, 0.3, 0.3, 0.9);
        cr.set_line_width(1.5);
        cr.move_to(ph_x, 0.0);
        cr.line_to(ph_x, height);
        cr.stroke().ok();
    }

    // Subtle top/bottom border
    cr.set_source_rgba(1.0, 1.0, 1.0, 0.06);
    cr.set_line_width(1.0);
    cr.move_to(0.0, height - 0.5);
    cr.line_to(width, height - 0.5);
    cr.stroke().ok();
}

/// Build the mini-map DrawingArea with click-to-pan and drag-to-scroll
/// gesture controllers. Returns the DrawingArea (initially hidden).
pub fn build_timeline_minimap(
    state: Rc<RefCell<TimelineState>>,
    timeline_area: DrawingArea,
    ruler: DrawingArea,
) -> DrawingArea {
    let minimap = DrawingArea::new();
    minimap.set_content_height(MINIMAP_HEIGHT.ceil() as i32);
    minimap.set_vexpand(false);
    minimap.set_hexpand(true);
    minimap.set_visible(false);

    // Draw function
    {
        let state = state.clone();
        minimap.set_draw_func(move |_area, cr, width, height| {
            let st = state.borrow();
            draw_minimap(cr, width as f64, height as f64, &st);
        });
    }

    // Helper: convert an x-coordinate in the minimap to a timeline ns position.
    let minimap_x_to_ns = {
        let state = state.clone();
        move |x: f64, minimap_width: f64| -> u64 {
            let st = state.borrow();
            let total_ns = total_duration_ns(&st);
            if total_ns == 0 {
                return 0;
            }
            let usable_w = (minimap_width - MINIMAP_GUTTER).max(1.0);
            let frac = ((x - MINIMAP_GUTTER) / usable_w).clamp(0.0, 1.0);
            (frac * total_ns as f64) as u64
        }
    };

    // Click: center viewport on clicked position. Ctrl+Click: seek playhead.
    // Double-click: zoom to fit entire project.
    {
        let state = state.clone();
        let timeline_area = timeline_area.clone();
        let ruler = ruler.clone();
        let minimap_weak = minimap.downgrade();
        let x_to_ns = minimap_x_to_ns.clone();
        let click = GestureClick::new();
        click.connect_pressed(move |gesture, n_press, x, _y| {
            let minimap_width = minimap_weak
                .upgrade()
                .map(|m| m.width() as f64)
                .unwrap_or(800.0);

            if n_press == 2 {
                // Double-click: zoom to fit entire project
                let st_ref = state.borrow();
                let total_ns = total_duration_ns(&st_ref);
                drop(st_ref);
                if total_ns == 0 {
                    return;
                }
                let total_secs = total_ns as f64 / NS_PER_SECOND;
                let viewport_w = timeline_area.width() as f64;
                let usable_viewport = (viewport_w - TRACK_LABEL_WIDTH).max(100.0);
                // Add 5% padding so clips don't touch the right edge
                let pps = (usable_viewport / (total_secs * 1.05)).clamp(10.0, 2000.0);
                let mut st = state.borrow_mut();
                st.pixels_per_second = pps;
                st.scroll_offset = 0.0;
                st.user_scroll_cooldown_until =
                    Some(std::time::Instant::now() + std::time::Duration::from_millis(600));
                drop(st);
            } else {
                let ns = x_to_ns(x, minimap_width);
                let modifiers = gesture.current_event_state();
                let ctrl = modifiers.contains(gdk4::ModifierType::CONTROL_MASK);

                let mut st = state.borrow_mut();
                if ctrl {
                    // Seek playhead
                    st.set_playhead_visual(ns);
                    let seek_cb = st.on_seek.clone();
                    drop(st);
                    if let Some(cb) = seek_cb {
                        cb(ns);
                    }
                } else {
                    // Center viewport on clicked position
                    let visible_secs = minimap_width / st.pixels_per_second;
                    let target_secs = ns as f64 / NS_PER_SECOND;
                    let new_offset = (target_secs - visible_secs / 2.0) * st.pixels_per_second;
                    st.scroll_offset = new_offset.max(0.0);
                    st.user_scroll_cooldown_until =
                        Some(std::time::Instant::now() + std::time::Duration::from_millis(600));
                    drop(st);
                }
            }
            timeline_area.queue_draw();
            ruler.queue_draw();
            if let Some(m) = minimap_weak.upgrade() {
                m.queue_draw();
            }
        });
        minimap.add_controller(click);
    }

    // Drag: pan viewport by dragging on the minimap.
    {
        let state = state.clone();
        let timeline_area = timeline_area.clone();
        let ruler = ruler.clone();
        let minimap_weak = minimap.downgrade();
        let drag = GestureDrag::new();
        let drag_start_offset: Rc<std::cell::Cell<f64>> = Rc::new(std::cell::Cell::new(0.0));
        {
            let state = state.clone();
            let drag_start_offset = drag_start_offset.clone();
            drag.connect_drag_begin(move |_gesture, _x, _y| {
                let st = state.borrow();
                drag_start_offset.set(st.scroll_offset);
            });
        }
        {
            let state = state.clone();
            let drag_start_offset = drag_start_offset.clone();
            let timeline_area = timeline_area.clone();
            let ruler = ruler.clone();
            let minimap_weak = minimap_weak.clone();
            drag.connect_drag_update(move |_gesture, offset_x, _offset_y| {
                let minimap_width = minimap_weak
                    .upgrade()
                    .map(|m| m.width() as f64)
                    .unwrap_or(800.0);
                let st_ref = state.borrow();
                let total_ns = total_duration_ns(&st_ref);
                drop(st_ref);
                if total_ns == 0 {
                    return;
                }
                let usable_w = (minimap_width - MINIMAP_GUTTER).max(1.0);
                let minimap_pps = usable_w / (total_ns as f64 / NS_PER_SECOND);
                let mut st = state.borrow_mut();
                // Convert minimap pixel offset to main-timeline pixel offset
                let main_offset_px = offset_x * (st.pixels_per_second / minimap_pps);
                st.scroll_offset = (drag_start_offset.get() + main_offset_px).max(0.0);
                st.user_scroll_cooldown_until =
                    Some(std::time::Instant::now() + std::time::Duration::from_millis(600));
                drop(st);
                timeline_area.queue_draw();
                ruler.queue_draw();
                if let Some(m) = minimap_weak.upgrade() {
                    m.queue_draw();
                }
            });
        }
        minimap.add_controller(drag);
    }

    minimap
}

pub fn export_timeline_snapshot_png(
    state: &TimelineState,
    width: i32,
    height: i32,
    path: &str,
) -> Result<(), String> {
    let width = width.max(1);
    let height = height.max(1);
    let mut surface = gtk::cairo::ImageSurface::create(gtk::cairo::Format::ARgb32, width, height)
        .map_err(|e| e.to_string())?;
    let cr = gtk::cairo::Context::new(&surface).map_err(|e| e.to_string())?;
    let mut cache = crate::media::thumb_cache::ThumbnailCache::new();
    let mut wcache = crate::media::waveform_cache::WaveformCache::new();
    draw_timeline(&cr, width, height, state, &mut cache, &mut wcache, true);
    drop(cr);
    surface.flush();
    let stride = surface.stride() as usize;
    let data = surface.data().map_err(|e| e.to_string())?;
    let bytes = glib::Bytes::from_owned(data.to_vec());
    let texture =
        gdk4::MemoryTexture::new(width, height, gdk4::MemoryFormat::B8g8r8a8, &bytes, stride);
    texture.save_to_png(path).map_err(|e| e.to_string())?;
    Ok(())
}

/// Cairo drawing of the entire timeline
fn draw_timeline(
    cr: &gtk::cairo::Context,
    width: i32,
    height: i32,
    st: &TimelineState,
    cache: &mut crate::media::thumb_cache::ThumbnailCache,
    wcache: &mut crate::media::waveform_cache::WaveformCache,
    draw_ruler_overlay: bool,
) {
    let w = width as f64;
    let h = height as f64;

    // Background
    let (bg_r, bg_g, bg_b) = crate::ui::colors::COLOR_TIMELINE_BG;
    cr.set_source_rgb(bg_r, bg_g, bg_b);
    cr.paint().ok();

    // Compound breadcrumb bar (when drilled into a compound clip)
    let overlay_top = if draw_ruler_overlay {
        RULER_HEIGHT
    } else {
        0.0
    };
    let breadcrumb_height = if st.is_editing_compound() {
        let bar_h = 22.0;
        cr.save().ok();
        cr.rectangle(0.0, overlay_top, w, bar_h);
        cr.set_source_rgb(0.18, 0.50, 0.48);
        let _ = cr.fill();
        cr.set_source_rgba(1.0, 1.0, 1.0, 0.95);
        cr.set_font_size(11.0);
        let labels = st.compound_breadcrumb_labels();
        let breadcrumb_text = labels.join(" > ");
        let _ = cr.move_to(8.0, overlay_top + 15.0);
        let _ = cr.show_text(&format!("{breadcrumb_text}  (Esc to go back)"));
        cr.restore().ok();
        bar_h
    } else {
        0.0
    };

    // Tracks
    let grouped_peer_highlight_ids = st.grouped_peer_highlight_ids();
    let linked_peer_highlight_ids = st.linked_peer_highlight_ids();
    let proj = st.project.borrow();
    let editing_tracks = st.resolve_editing_tracks(&proj);
    let ruler_top = 0.0;
    let track_content_top = overlay_top + breadcrumb_height;
    // For screen drawing, apply vertical scroll; for PNG export, don't scroll.
    let effective_scroll = if draw_ruler_overlay {
        0.0
    } else {
        let total_content = timeline_content_height_for_tracks(editing_tracks);
        let visible = (h - track_content_top).max(0.0);
        st.vertical_scroll_offset
            .clamp(0.0, (total_content - visible).max(0.0))
    };
    // Clip to the track area so scrolled tracks don't overdraw the breadcrumb/ruler.
    cr.save().ok();
    cr.rectangle(0.0, track_content_top, w, (h - track_content_top).max(0.0));
    cr.clip();
    let mut y = track_content_top - effective_scroll;
    for &logical_idx in &visual_order(editing_tracks) {
        let track = &editing_tracks[logical_idx];
        let track_height = track_row_height(track);
        // Skip tracks entirely outside the visible area
        if y + track_height > track_content_top && y < h {
            draw_track_row(
                cr,
                w,
                y,
                track_height,
                logical_idx,
                track,
                st,
                &grouped_peer_highlight_ids,
                &linked_peer_highlight_ids,
                cache,
                wcache,
            );
        }
        y += track_height;
    }
    cr.restore().ok();

    // Playhead (clipped to content area so it doesn't overdraw track labels)
    // When inside a compound, translate the main-timeline playhead to the
    // compound's internal time so it aligns with the internal clips.
    let ph_x = st.ns_to_x(st.editing_playhead_ns());
    if track_content_top < h {
        cr.save().ok();
        cr.rectangle(
            TRACK_LABEL_WIDTH,
            track_content_top,
            w - TRACK_LABEL_WIDTH,
            h - track_content_top,
        );
        cr.clip();
        cr.set_source_rgb(1.0, 0.3, 0.3);
        cr.set_line_width(2.0);
        cr.move_to(ph_x, track_content_top);
        cr.line_to(ph_x, h);
        cr.stroke().ok();
        cr.restore().ok();
    }

    // Snap indicator — dashed vertical guideline + badge at the active snap target.
    if !matches!(st.drag_op, DragOp::None) {
        if let Some(hit) = st.active_snap_hit.clone() {
            let sx = st.ns_to_x(hit.position_ns);
            if sx >= TRACK_LABEL_WIDTH - 1.0 {
                cr.save().ok();
                cr.rectangle(TRACK_LABEL_WIDTH, 0.0, w - TRACK_LABEL_WIDTH, h);
                cr.clip();
                cr.set_source_rgba(1.0, 0.82, 0.2, 0.9);
                cr.set_line_width(1.5);
                cr.set_dash(&[4.0, 3.0], 0.0);
                cr.move_to(sx, track_content_top);
                cr.line_to(sx, h);
                cr.stroke().ok();
                cr.set_dash(&[], 0.0);

                cr.select_font_face(
                    "sans",
                    gtk::cairo::FontSlant::Normal,
                    gtk::cairo::FontWeight::Bold,
                );
                cr.set_font_size(10.0);
                let te = cr
                    .text_extents(hit.label)
                    .unwrap_or_else(|_| cr.text_extents("X").unwrap());
                let pad = 5.0;
                let bh = 16.0;
                let bw = te.width() + pad * 2.0;
                let bx = sx + 4.0;
                let by = track_content_top + 4.0;
                rounded_rect(cr, bx, by, bw, bh, 3.0);
                cr.set_source_rgba(1.0, 0.82, 0.2, 0.95);
                cr.fill().ok();
                cr.set_source_rgb(0.0, 0.0, 0.0);
                cr.move_to(bx + pad, by + bh - 5.0);
                let _ = cr.show_text(hit.label);
                cr.restore().ok();
            }
        }
    }

    // Tool indicator
    let tool_label = match st.active_tool {
        ActiveTool::Razor => Some("✂ Razor (B to toggle)"),
        ActiveTool::Slip => Some("↔ Slip (Y to toggle)"),
        ActiveTool::Slide => Some("⇔ Slide (U to toggle)"),
        _ => None,
    };
    let mut indicator_y = track_content_top + 16.0;
    if let Some(label) = tool_label {
        cr.set_source_rgb(1.0, 0.8, 0.0);
        cr.set_font_size(12.0);
        let _ = cr.move_to(TRACK_LABEL_WIDTH + 8.0, indicator_y);
        let _ = cr.show_text(label);
        indicator_y += 16.0;
    }
    if st.magnetic_mode {
        cr.set_source_rgb(0.55, 0.95, 0.65);
        cr.set_font_size(12.0);
        let _ = cr.move_to(TRACK_LABEL_WIDTH + 8.0, indicator_y);
        let _ = cr.show_text("[Magnetic]");
        indicator_y += 16.0;
    }
    if st.music_generation_armed_track_id.is_some() {
        cr.set_source_rgb(0.78, 0.88, 1.0);
        cr.set_font_size(12.0);
        let _ = cr.move_to(TRACK_LABEL_WIDTH + 8.0, indicator_y);
        let _ = cr.show_text(
            "♫ Drag on the armed audio track to define a MusicGen region (Esc to cancel)",
        );
    }

    let track_row_y = |track_idx: usize| {
        track_row_top_in_tracks(editing_tracks, track_idx) + track_content_top - effective_scroll
    };
    if let Some(armed_track_id) = &st.music_generation_armed_track_id {
        if let Some((track_idx, track)) = editing_tracks
            .iter()
            .enumerate()
            .find(|(_, track)| &track.id == armed_track_id)
        {
            let top = track_row_y(track_idx) + 2.0;
            let height = track_row_height(track) - 4.0;
            if height > 0.0 {
                let (sr, sg, sb, sa) = crate::ui::colors::COLOR_SELECTION_FILL;
                cr.set_source_rgba(sr, sg, sb, sa);
                cr.rectangle(
                    TRACK_LABEL_WIDTH + 1.0,
                    top,
                    (w - TRACK_LABEL_WIDTH - 2.0).max(0.0),
                    height,
                );
                cr.fill().ok();
                let (br, bg, bb, ba) = crate::ui::colors::COLOR_SELECTION_BORDER;
                cr.set_source_rgba(br, bg, bb, ba);
                cr.set_line_width(1.2);
                cr.rectangle(
                    TRACK_LABEL_WIDTH + 1.0,
                    top,
                    (w - TRACK_LABEL_WIDTH - 2.0).max(0.0),
                    height,
                );
                cr.stroke().ok();
            }
        }
    }
    if let Some(draft) = &st.music_generation_region_draft {
        if let Some((track_idx, track)) = editing_tracks
            .iter()
            .enumerate()
            .find(|(_, track)| track.id == draft.track_id)
        {
            let start_ns = draft.start_ns.min(draft.current_ns);
            let end_ns = draft.start_ns.max(draft.current_ns);
            let left = st.ns_to_x(start_ns).max(TRACK_LABEL_WIDTH);
            let right = st.ns_to_x(end_ns);
            let top = track_row_y(track_idx) + 2.0;
            let bottom = top + track_row_height(track) - 4.0;
            if right > left && bottom > top {
                cr.set_source_rgba(0.36, 0.68, 1.0, 0.22);
                cr.rectangle(left, top, right - left, bottom - top);
                cr.fill().ok();
                cr.set_source_rgba(0.56, 0.82, 1.0, 0.95);
                cr.set_line_width(1.4);
                cr.rectangle(left, top, right - left, bottom - top);
                cr.stroke().ok();
                if right - left > 90.0 {
                    cr.set_source_rgba(0.96, 0.98, 1.0, 0.95);
                    cr.set_font_size(11.0);
                    let _ = cr.move_to(left + 8.0, top + 16.0);
                    let _ = cr.show_text("Music region");
                }
            }
        }
    }
    for overlay in &st.music_generation_overlays {
        if let Some((track_idx, track)) = editing_tracks
            .iter()
            .enumerate()
            .find(|(_, track)| track.id == overlay.track_id)
        {
            let left = st.ns_to_x(overlay.start_ns).max(TRACK_LABEL_WIDTH);
            let right = st.ns_to_x(overlay.end_ns);
            let top = track_row_y(track_idx) + 2.0;
            let bottom = top + track_row_height(track) - 4.0;
            if right <= left || bottom <= top {
                continue;
            }
            let (fill_rgba, stroke_rgba, label) = match overlay.status {
                MusicGenerationOverlayStatus::Pending => (
                    (0.62, 0.36, 0.95, 0.20),
                    (0.82, 0.60, 1.0, 0.95),
                    "Generating music…",
                ),
                MusicGenerationOverlayStatus::Failed => (
                    (0.92, 0.22, 0.28, 0.20),
                    (1.0, 0.52, 0.55, 0.95),
                    "Music generation failed",
                ),
            };
            cr.set_source_rgba(fill_rgba.0, fill_rgba.1, fill_rgba.2, fill_rgba.3);
            cr.rectangle(left, top, right - left, bottom - top);
            cr.fill().ok();
            cr.set_source_rgba(stroke_rgba.0, stroke_rgba.1, stroke_rgba.2, stroke_rgba.3);
            cr.set_line_width(1.4);
            cr.rectangle(left, top, right - left, bottom - top);
            cr.stroke().ok();
            if right - left > 120.0 {
                cr.set_source_rgba(0.98, 0.98, 1.0, 0.95);
                cr.set_font_size(11.0);
                let _ = cr.move_to(left + 8.0, top + 16.0);
                let _ = cr.show_text(label);
            }
        }
    }

    // Track reorder drop indicator
    if let DragOp::ReorderTrack {
        track_idx,
        target_idx,
    } = &st.drag_op
    {
        if track_idx != target_idx {
            let target_top = track_row_top_in_tracks(editing_tracks, *target_idx)
                + track_content_top
                - effective_scroll;
            let target_height = editing_tracks
                .get(*target_idx)
                .map(track_row_height)
                .unwrap_or(0.0);
            // Decide above/below based on *visual* position, not logical index,
            // since video tracks are visually reversed.
            let source_visual = logical_to_visual(editing_tracks, *track_idx);
            let target_visual = logical_to_visual(editing_tracks, *target_idx);
            let target_below_source = match (source_visual, target_visual) {
                (Some(s), Some(t)) => t > s,
                _ => target_idx > track_idx,
            };
            let indicator_y = target_top
                + if target_below_source {
                    target_height
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
        let top = m.start_y.min(m.current_y).max(overlay_top);
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

    if let Some(km) = &st.keyframe_marquee_selection {
        if let Some((track_idx, track)) = editing_tracks
            .iter()
            .enumerate()
            .find(|(_, t)| t.id == km.track_id)
        {
            if let Some(clip) = track.clips.iter().find(|c| c.id == km.clip_id) {
                let duration_ns = clip.duration();
                if duration_ns > 0 {
                    let cx = st.ns_to_x(clip.timeline_start);
                    let cw = (duration_ns as f64 / NS_PER_SECOND) * st.pixels_per_second;
                    let x0 = cx + (km.start_local_ns as f64 / duration_ns as f64) * cw;
                    let x1 = cx + (km.current_local_ns as f64 / duration_ns as f64) * cw;
                    let left = x0.min(x1).max(TRACK_LABEL_WIDTH);
                    let right = x0.max(x1);
                    let top = track_row_top_in_tracks(editing_tracks, track_idx)
                        + track_content_top
                        - effective_scroll
                        + 2.0;
                    let bottom = top + track_row_height(track) - 4.0;
                    if right > left && bottom > top {
                        cr.set_source_rgba(1.0, 0.75, 0.22, 0.20);
                        cr.rectangle(left, top, right - left, bottom - top);
                        cr.fill().ok();
                        cr.set_source_rgba(1.0, 0.85, 0.40, 0.95);
                        cr.set_line_width(1.3);
                        cr.rectangle(left, top, right - left, bottom - top);
                        cr.stroke().ok();
                    }
                }
            }
        }
    }

    if draw_ruler_overlay {
        draw_ruler(cr, w, st, ruler_top);
        draw_ruler_playhead_marker(cr, ph_x, ruler_top);
    }
}

fn clamp_slide_delta(
    requested_delta: i128,
    left_bounds: Option<(u64, u64)>,
    right_bounds: Option<(u64, u64)>,
) -> i128 {
    const MIN_CLIP_NS: i128 = 1_000_000;
    if left_bounds.is_none() && right_bounds.is_none() {
        return 0;
    }

    let mut min_delta = i128::MIN;
    let mut max_delta = i128::MAX;

    if let Some((orig_out, left_in)) = left_bounds {
        min_delta = min_delta.max(i128::from(left_in) + MIN_CLIP_NS - i128::from(orig_out));
    }
    if let Some((orig_in, right_out)) = right_bounds {
        max_delta = max_delta.min(i128::from(right_out) - MIN_CLIP_NS - i128::from(orig_in));
    }

    // Edge-clip slide fallback: keep slide active, but don't extend the only
    // available neighbor beyond its drag-start trimmed window.
    if left_bounds.is_some() && right_bounds.is_none() {
        max_delta = max_delta.min(0);
    } else if left_bounds.is_none() && right_bounds.is_some() {
        min_delta = min_delta.max(0);
    }

    if min_delta > max_delta {
        0
    } else {
        requested_delta.clamp(min_delta, max_delta)
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

fn draw_ruler(cr: &gtk::cairo::Context, width: f64, st: &TimelineState, top_y: f64) {
    cr.save().ok();
    cr.translate(0.0, top_y);
    cr.set_source_rgb(0.2, 0.2, 0.22);
    cr.rectangle(0.0, 0.0, width, RULER_HEIGHT);
    cr.fill().ok();

    cr.set_source_rgb(0.6, 0.6, 0.6);
    cr.set_line_width(1.0);
    cr.set_font_size(RULER_FONT_SIZE);

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
        cr.set_font_size(MARKER_FONT_SIZE);
        for marker in &proj.markers {
            let mx = st.ns_to_x(marker.position_ns);
            if mx < TRACK_LABEL_WIDTH || mx > width {
                continue;
            }
            let (r, g, b, _a) = crate::ui::colors::rgba_u32_to_f64(marker.color);
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

    let (lbl_r, lbl_g, lbl_b) = crate::ui::colors::COLOR_TRACK_LABEL_BG;
    cr.set_source_rgb(lbl_r, lbl_g, lbl_b);
    cr.rectangle(0.0, 0.0, TRACK_LABEL_WIDTH, RULER_HEIGHT);
    cr.fill().ok();
    cr.restore().ok();
}

fn draw_ruler_playhead_marker(cr: &gtk::cairo::Context, playhead_x: f64, top_y: f64) {
    if playhead_x < TRACK_LABEL_WIDTH {
        return;
    }
    cr.set_source_rgb(1.0, 0.3, 0.3);
    cr.set_line_width(2.0);
    cr.move_to(playhead_x, top_y);
    cr.line_to(playhead_x, top_y + RULER_HEIGHT);
    cr.stroke().ok();
    cr.move_to(playhead_x - 6.0, top_y);
    cr.line_to(playhead_x + 6.0, top_y);
    cr.line_to(playhead_x, top_y + 12.0);
    cr.fill().ok();
}

fn draw_track_row(
    cr: &gtk::cairo::Context,
    width: f64,
    y: f64,
    track_height: f64,
    track_idx: usize,
    track: &crate::model::track::Track,
    st: &TimelineState,
    grouped_peer_highlight_ids: &HashSet<String>,
    linked_peer_highlight_ids: &HashSet<String>,
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
        track_height,
    );
    cr.fill().ok();

    // Draw clips first (they may overlap into label column before clipping)
    for clip in &track.clips {
        draw_clip(
            cr,
            width,
            y,
            track_height,
            clip,
            track,
            st,
            grouped_peer_highlight_ids,
            linked_peer_highlight_ids,
            cache,
            wcache,
        );
    }

    draw_through_edit_boundary_indicators(
        cr,
        &through_edit_indicator_geometry_for_track(track, st, y, track_height, width),
    );

    // Draw transition markers (clip -> next clip) after clip bodies.
    for clip in &track.clips {
        if clip.outgoing_transition.is_active() {
            let ex = st.ns_to_x(clip.timeline_end());
            let marker_w = 10.0;
            cr.set_source_rgba(0.85, 0.85, 1.0, 0.75);
            cr.rectangle(ex - marker_w / 2.0, y + 4.0, marker_w, track_height - 8.0);
            cr.fill().ok();
        }
    }

    // Muted-track dimming overlay on clip content area
    if track.muted {
        cr.set_source_rgba(0.0, 0.0, 0.0, 0.35);
        cr.rectangle(
            TRACK_LABEL_WIDTH,
            y,
            width - TRACK_LABEL_WIDTH,
            track_height,
        );
        cr.fill().ok();
    }

    // Locked-track hatch overlay on clip content area
    if track.locked {
        cr.save().ok();
        cr.rectangle(
            TRACK_LABEL_WIDTH,
            y,
            width - TRACK_LABEL_WIDTH,
            track_height,
        );
        cr.clip();
        cr.set_source_rgba(0.6, 0.4, 0.1, 0.12);
        cr.set_line_width(1.0);
        let step = 8.0;
        let x0 = TRACK_LABEL_WIDTH;
        let x1 = width;
        let y0 = y;
        let y1 = y + track_height;
        let mut d = x0 - track_height;
        while d < x1 {
            cr.move_to(d.max(x0), y0.max(y0 + (d - x0).min(0.0)));
            cr.line_to(
                (d + track_height).min(x1),
                y1.min(y0 + (d + track_height - x0).max(0.0)),
            );
            d += step;
        }
        cr.stroke().ok();
        cr.restore().ok();
    }

    // Draw label column on top so it stays visible when timeline is scrolled
    let is_active = st.selected_track_id.as_deref() == Some(&track.id);
    if is_active {
        cr.set_source_rgb(0.28, 0.28, 0.32);
    } else {
        cr.set_source_rgb(0.22, 0.22, 0.25);
    }
    cr.rectangle(0.0, y, TRACK_LABEL_WIDTH, track_height);
    cr.fill().ok();

    // Active track accent bar (uses track color when set)
    if is_active {
        let (ar, ag, ab) = track.color_label.rgb().unwrap_or((0.3, 0.55, 0.95));
        cr.set_source_rgb(ar, ag, ab);
        cr.rectangle(0.0, y, 3.0, track_height);
        cr.fill().ok();
    }

    // Drag handle glyph
    cr.set_source_rgb(0.45, 0.45, 0.5);
    cr.set_font_size(10.0);
    let _ = cr.move_to(5.0, y + track_height / 2.0 + 3.0);
    let _ = cr.show_text("⠿");

    // Per-track color swatch (10×10 dot left of track name)
    let name_x;
    if let Some((cr_r, cr_g, cr_b)) = track.color_label.rgb() {
        let dot_x = 18.0;
        let dot_y = y + track_height / 2.0 - 1.0;
        let dot_r = 5.0;
        cr.set_source_rgb(cr_r, cr_g, cr_b);
        cr.arc(dot_x, dot_y, dot_r, 0.0, 2.0 * std::f64::consts::PI);
        cr.fill().ok();
        name_x = 28.0;
    } else {
        name_x = 18.0;
    }

    cr.set_source_rgb(0.8, 0.8, 0.8);
    cr.set_font_size(11.0);
    let _ = cr.move_to(name_x, y + track_height / 2.0 + 4.0);
    let _ = cr.show_text(&track.label);

    // Audio role label (below track name, dimmed).
    if track.is_audio() && track.audio_role != crate::model::track::AudioRole::None {
        let role_label = track.audio_role.short_label();
        let (role_r, role_g, role_b) = match track.audio_role {
            crate::model::track::AudioRole::Dialogue => crate::ui::colors::COLOR_AUDIO_DIALOGUE,
            crate::model::track::AudioRole::Effects => crate::ui::colors::COLOR_AUDIO_EFFECTS,
            crate::model::track::AudioRole::Music => crate::ui::colors::COLOR_AUDIO_MUSIC,
            _ => crate::ui::colors::COLOR_AUDIO_ROLE_NONE,
        };
        cr.set_source_rgb(role_r, role_g, role_b);
        cr.set_font_size(9.0);
        let _ = cr.move_to(name_x, y + track_height / 2.0 + 15.0);
        let _ = cr.show_text(role_label);
    }

    // --- Badge row: [D] [M] [S] [L] from left to right ---
    let badge_y = y + 6.0;

    // Lock badge (rightmost)
    let lock_x = track_label_lock_badge_x(st.show_track_audio_levels);
    if track.locked {
        cr.set_source_rgb(0.85, 0.5, 0.15);
    } else {
        cr.set_source_rgb(0.35, 0.35, 0.4);
    }
    cr.rectangle(
        lock_x,
        badge_y,
        TRACK_LABEL_BADGE_WIDTH,
        TRACK_LABEL_BADGE_HEIGHT,
    );
    cr.fill().ok();
    cr.set_source_rgb(0.1, 0.1, 0.12);
    cr.set_font_size(10.0);
    let _ = cr.move_to(lock_x + 4.5, badge_y + TRACK_LABEL_BADGE_HEIGHT - 3.0);
    let _ = cr.show_text("L");

    // Solo badge (second from right)
    let solo_x = track_label_solo_badge_x(st.show_track_audio_levels);
    if track.soloed {
        cr.set_source_rgb(0.9, 0.75, 0.2);
    } else {
        cr.set_source_rgb(0.35, 0.35, 0.4);
    }
    cr.rectangle(
        solo_x,
        badge_y,
        TRACK_LABEL_BADGE_WIDTH,
        TRACK_LABEL_BADGE_HEIGHT,
    );
    cr.fill().ok();
    cr.set_source_rgb(0.1, 0.1, 0.12);
    cr.set_font_size(10.0);
    let _ = cr.move_to(solo_x + 4.5, badge_y + TRACK_LABEL_BADGE_HEIGHT - 3.0);
    let _ = cr.show_text("S");

    // Mute badge
    let mute_x = track_label_mute_badge_x(st.show_track_audio_levels);
    if track.muted {
        cr.set_source_rgb(0.85, 0.25, 0.25);
    } else {
        cr.set_source_rgb(0.35, 0.35, 0.4);
    }
    cr.rectangle(
        mute_x,
        badge_y,
        TRACK_LABEL_BADGE_WIDTH,
        TRACK_LABEL_BADGE_HEIGHT,
    );
    cr.fill().ok();
    cr.set_source_rgb(0.1, 0.1, 0.12);
    cr.set_font_size(10.0);
    let _ = cr.move_to(mute_x + 4.0, badge_y + TRACK_LABEL_BADGE_HEIGHT - 3.0);
    let _ = cr.show_text("M");

    // Duck badge (only for audio tracks).
    if track.is_audio() {
        let duck_x = track_label_duck_badge_x(st.show_track_audio_levels);
        if track.duck {
            cr.set_source_rgb(0.2, 0.7, 0.9);
        } else {
            cr.set_source_rgb(0.35, 0.35, 0.4);
        }
        cr.rectangle(
            duck_x,
            badge_y,
            TRACK_LABEL_BADGE_WIDTH,
            TRACK_LABEL_BADGE_HEIGHT,
        );
        cr.fill().ok();
        cr.set_source_rgb(0.1, 0.1, 0.12);
        cr.set_font_size(10.0);
        let _ = cr.move_to(duck_x + 4.5, badge_y + TRACK_LABEL_BADGE_HEIGHT - 3.0);
        let _ = cr.show_text("D");
    }

    if st.show_track_audio_levels {
        let meter_x = TRACK_LABEL_WIDTH - TRACK_LABEL_METER_WIDTH - 6.0;
        let meter_y = y + 8.0;
        let meter_h = track_height - 16.0;
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
    cr.move_to(0.0, y + track_height);
    cr.line_to(width, y + track_height);
    cr.stroke().ok();
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct ThroughEditIndicatorGeometry {
    x: f64,
    y_top: f64,
    y_bottom: f64,
}

fn through_edit_indicator_geometry_for_track(
    track: &crate::model::track::Track,
    st: &TimelineState,
    track_y: f64,
    track_height: f64,
    view_width: f64,
) -> Vec<ThroughEditIndicatorGeometry> {
    let y_top = track_y + 5.0;
    let y_bottom = track_y + track_height - 5.0;
    detect_track_through_edit_boundaries(track)
        .into_iter()
        .filter(|boundary| {
            !st.is_clip_selected(&boundary.left_clip_id)
                && !st.is_clip_selected(&boundary.right_clip_id)
        })
        .filter_map(|boundary| {
            let left = track.clips.get(boundary.left_clip_index)?;
            let right = track.clips.get(boundary.right_clip_index)?;
            if left.id != boundary.left_clip_id || right.id != boundary.right_clip_id {
                return None;
            }
            if !through_edit_metadata_compatible(left, right) {
                return None;
            }
            let x = st.ns_to_x(boundary.boundary_ns);
            (x >= TRACK_LABEL_WIDTH && x <= view_width).then_some(ThroughEditIndicatorGeometry {
                x,
                y_top,
                y_bottom,
            })
        })
        .collect()
}

fn draw_through_edit_boundary_indicators(
    cr: &gtk::cairo::Context,
    indicators: &[ThroughEditIndicatorGeometry],
) {
    if indicators.is_empty() {
        return;
    }

    cr.save().ok();
    cr.set_source_rgba(0.86, 0.88, 0.95, 0.55);
    cr.set_line_width(1.0);
    cr.set_dash(&[2.5, 2.5], 0.0);
    for indicator in indicators {
        let x = indicator.x.round() + 0.5;
        cr.move_to(x, indicator.y_top);
        cr.line_to(x, indicator.y_bottom);
    }
    cr.stroke().ok();
    cr.restore().ok();
}

fn track_label_solo_badge_x(show_track_audio_levels: bool) -> f64 {
    // Badge layout right-to-left: [D] [M] [S] [L]
    // Lock is rightmost, solo is second from right
    track_label_lock_badge_x(show_track_audio_levels) - TRACK_LABEL_BADGE_WIDTH - 2.0
}

fn track_label_lock_badge_x(show_track_audio_levels: bool) -> f64 {
    if show_track_audio_levels {
        TRACK_LABEL_WIDTH - TRACK_LABEL_METER_WIDTH - TRACK_LABEL_BADGE_WIDTH - 12.0
    } else {
        TRACK_LABEL_WIDTH - TRACK_LABEL_BADGE_WIDTH - 8.0
    }
}

fn track_label_mute_badge_x(show_track_audio_levels: bool) -> f64 {
    track_label_solo_badge_x(show_track_audio_levels) - TRACK_LABEL_BADGE_WIDTH - 2.0
}

fn track_label_duck_badge_x(show_track_audio_levels: bool) -> f64 {
    track_label_mute_badge_x(show_track_audio_levels) - TRACK_LABEL_BADGE_WIDTH - 2.0
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
            let (r, g, b) = crate::ui::colors::COLOR_LEVEL_GOOD;
            cr.set_source_rgb(r, g, b);
            cr.rectangle(bx, y + height - green_h, bar_w, green_h);
            cr.fill().ok();
        }

        let yellow_frac = db_to_frac(-6.0);
        let yellow_h = ((yellow_frac - green_frac) * height).min((bar_h - green_h).max(0.0));
        if yellow_h > 0.0 {
            let (r, g, b) = crate::ui::colors::COLOR_LEVEL_WARN;
            cr.set_source_rgb(r, g, b);
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
            let (r, g, b) = crate::ui::colors::COLOR_LEVEL_CLIP;
            cr.set_source_rgb(r, g, b);
            cr.rectangle(bx, top, bar_w, red_h);
            cr.fill().ok();
        }
    }
}

fn draw_clip(
    cr: &gtk::cairo::Context,
    view_width: f64,
    track_y: f64,
    track_height: f64,
    clip: &crate::model::clip::Clip,
    track: &crate::model::track::Track,
    st: &TimelineState,
    grouped_peer_highlight_ids: &HashSet<String>,
    linked_peer_highlight_ids: &HashSet<String>,
    cache: &mut crate::media::thumb_cache::ThumbnailCache,
    wcache: &mut crate::media::waveform_cache::WaveformCache,
) {
    let cx = st.ns_to_x(clip.timeline_start);
    let cw = (clip.duration() as f64 / NS_PER_SECOND) * st.pixels_per_second;
    let cy = track_y + 2.0;
    let ch = track_height - 4.0;

    if cx + cw < TRACK_LABEL_WIDTH || cx > view_width {
        return;
    }

    let is_selected = st.is_clip_selected(&clip.id);
    let is_group_peer_highlighted = grouped_peer_highlight_ids.contains(&clip.id);
    let is_link_peer_highlighted = linked_peer_highlight_ids.contains(&clip.id);
    let is_transition_hover = st
        .hover_transition_pair
        .as_ref()
        .map(|(l, r)| clip.id == *l || clip.id == *r)
        .unwrap_or(false);

    let (r, g, b) = clip_fill_color(clip, track.kind);
    cr.set_source_rgb(r, g, b);
    rounded_rect(cr, cx, cy, cw.max(4.0), ch, 4.0);
    cr.fill().ok();

    // ── Title clip text label ───────────────────────────────────────────
    if clip.kind == crate::model::clip::ClipKind::Title && cw > 20.0 {
        cr.save().ok();
        rounded_rect(
            cr,
            cx + 1.0,
            cy + 1.0,
            (cw - 2.0).max(0.0),
            (ch - 2.0).max(0.0),
            3.0,
        );
        cr.clip();
        // Draw "T" badge
        cr.set_source_rgba(1.0, 1.0, 1.0, 0.5);
        cr.set_font_size(10.0);
        let _ = cr.move_to(cx + 4.0, cy + 12.0);
        let _ = cr.show_text("T");
        // Draw title text centered
        let display = if clip.title_text.is_empty() {
            &clip.label
        } else {
            &clip.title_text
        };
        let max_chars = ((cw - 10.0) / 7.0).max(1.0) as usize;
        let truncated = if display.len() > max_chars {
            format!("{}…", &display[..max_chars.saturating_sub(1)])
        } else {
            display.to_string()
        };
        cr.set_source_rgba(1.0, 1.0, 1.0, 0.9);
        cr.set_font_size((ch * 0.4).clamp(TRACK_LABEL_FONT_SIZE_MIN, TRACK_LABEL_FONT_SIZE_MAX));
        let te = cr
            .text_extents(&truncated)
            .unwrap_or_else(|_| cr.text_extents("X").unwrap());
        let tx = cx + (cw - te.width()) / 2.0;
        let ty = cy + (ch + te.height()) / 2.0;
        let _ = cr.move_to(tx, ty);
        let _ = cr.show_text(&truncated);
        cr.restore().ok();
    }

    // ── Adjustment layer hatch pattern + badge ─────────────────────────
    if clip.kind == crate::model::clip::ClipKind::Adjustment && cw > 20.0 {
        cr.save().ok();
        rounded_rect(
            cr,
            cx + 1.0,
            cy + 1.0,
            (cw - 2.0).max(0.0),
            (ch - 2.0).max(0.0),
            3.0,
        );
        cr.clip();
        // Draw diagonal hatch lines
        cr.set_source_rgba(1.0, 1.0, 1.0, 0.12);
        cr.set_line_width(1.0);
        let spacing = 8.0;
        let diag = cw + ch;
        let steps = (diag / spacing).ceil() as i32;
        for i in 0..steps {
            let offset = i as f64 * spacing;
            let _ = cr.move_to(cx + offset, cy);
            let _ = cr.line_to(cx + offset - ch, cy + ch);
        }
        cr.stroke().ok();
        // Draw "ADJ" badge
        cr.set_source_rgba(1.0, 1.0, 1.0, 0.5);
        cr.set_font_size(9.0);
        let _ = cr.move_to(cx + 4.0, cy + 11.0);
        let _ = cr.show_text("ADJ");
        // Draw label centered
        let max_chars = ((cw - 10.0) / 7.0).max(1.0) as usize;
        let truncated = if clip.label.len() > max_chars {
            format!("{}…", &clip.label[..max_chars.saturating_sub(1)])
        } else {
            clip.label.clone()
        };
        cr.set_source_rgba(1.0, 1.0, 1.0, 0.9);
        cr.set_font_size((ch * 0.4).clamp(TRACK_LABEL_FONT_SIZE_MIN, TRACK_LABEL_FONT_SIZE_MAX));
        let te = cr
            .text_extents(&truncated)
            .unwrap_or_else(|_| cr.text_extents("X").unwrap());
        let tx = cx + (cw - te.width()) / 2.0;
        let ty = cy + (ch + te.height()) / 2.0;
        let _ = cr.move_to(tx, ty);
        let _ = cr.show_text(&truncated);
        cr.restore().ok();
    }

    // ── Compound clip visual ──────────────────────────────────────────────
    if clip.kind == crate::model::clip::ClipKind::Compound && cw > 20.0 {
        // Badge
        cr.save().ok();
        let badge_size = 14.0_f64.min(ch * 0.4);
        let bx = cx + 4.0;
        let by = cy + 4.0;
        cr.rectangle(bx, by, badge_size, badge_size);
        cr.set_source_rgba(0.12, 0.53, 0.50, 0.85);
        let _ = cr.fill();
        cr.set_source_rgba(1.0, 1.0, 1.0, 0.9);
        let fs = badge_size * 0.6;
        cr.set_font_size(fs);
        let te = cr
            .text_extents("⊞")
            .unwrap_or_else(|_| cr.text_extents("C").unwrap());
        let _ = cr.move_to(
            bx + (badge_size - te.width()) / 2.0 - te.x_bearing(),
            by + (badge_size + te.height()) / 2.0,
        );
        let _ = cr.show_text("⊞");
        cr.restore().ok();

        // Centered label
        cr.save().ok();
        let font_sz = (ch * 0.35).clamp(TRACK_LABEL_FONT_SIZE_MIN, TRACK_LABEL_FONT_SIZE_MAX);
        cr.set_font_size(font_sz);
        cr.set_source_rgba(1.0, 1.0, 1.0, 0.85);
        let label_text = &clip.label;
        let max_label_w = (cw - 24.0).max(0.0);
        let mut truncated = label_text.clone();
        loop {
            let te = cr
                .text_extents(&truncated)
                .unwrap_or_else(|_| cr.text_extents("…").unwrap());
            if te.width() <= max_label_w || truncated.len() <= 1 {
                break;
            }
            truncated.pop();
            truncated.push('…');
        }
        let te = cr
            .text_extents(&truncated)
            .unwrap_or_else(|_| cr.text_extents("…").unwrap());
        let tx = cx + (cw - te.width()) / 2.0;
        let ty = cy + (ch + te.height()) / 2.0;
        let _ = cr.move_to(tx, ty);
        let _ = cr.show_text(&truncated);
        cr.restore().ok();
    }

    // ── Multicam clip visual ──────────────────────────────────────────────
    if clip.kind == crate::model::clip::ClipKind::Multicam && cw > 20.0 {
        // Badge
        cr.save().ok();
        let badge_size = 14.0_f64.min(ch * 0.4);
        let bx = cx + 4.0;
        let by = cy + 4.0;
        cr.rectangle(bx, by, badge_size, badge_size);
        cr.set_source_rgba(0.85, 0.45, 0.12, 0.85);
        let _ = cr.fill();
        cr.set_source_rgba(1.0, 1.0, 1.0, 0.9);
        let fs = badge_size * 0.55;
        cr.set_font_size(fs);
        let _ = cr.move_to(bx + 2.0, by + badge_size - 3.0);
        let _ = cr.show_text("MC");
        cr.restore().ok();

        // Draw angle switch markers and segment labels
        if let Some(ref switches) = clip.multicam_switches {
            let pps = st.pixels_per_second;
            for sw in switches.iter().skip(1) {
                // Vertical marker for each switch (except the first at 0)
                let sw_x = cx + (sw.position_ns as f64 / NS_PER_SECOND) * pps;
                if sw_x > cx && sw_x < cx + cw {
                    cr.save().ok();
                    cr.set_source_rgba(1.0, 1.0, 1.0, 0.6);
                    cr.set_line_width(1.0);
                    let _ = cr.move_to(sw_x, cy);
                    let _ = cr.line_to(sw_x, cy + ch);
                    let _ = cr.stroke();
                    cr.restore().ok();
                }
            }
            // Segment labels
            let segments = clip.multicam_segments();
            let font_sz = (ch * 0.28).clamp(7.0, 12.0);
            for (seg_start, seg_end, angle_idx) in &segments {
                let seg_x = cx + (*seg_start as f64 / NS_PER_SECOND) * pps;
                let seg_w = ((*seg_end - seg_start) as f64 / NS_PER_SECOND) * pps;
                if seg_w > 20.0 {
                    let label = clip
                        .multicam_angles
                        .as_ref()
                        .and_then(|a| a.get(*angle_idx))
                        .map(|a| a.label.as_str())
                        .unwrap_or("?");
                    cr.save().ok();
                    cr.set_font_size(font_sz);
                    cr.set_source_rgba(1.0, 1.0, 1.0, 0.7);
                    let _ = cr.move_to(seg_x + 4.0, cy + ch - 4.0);
                    let _ = cr.show_text(label);
                    cr.restore().ok();
                }
            }
        }

        // Centered label
        cr.save().ok();
        let font_sz = (ch * 0.35).clamp(TRACK_LABEL_FONT_SIZE_MIN, TRACK_LABEL_FONT_SIZE_MAX);
        cr.set_font_size(font_sz);
        cr.set_source_rgba(1.0, 1.0, 1.0, 0.85);
        let label_text = &clip.label;
        let max_label_w = (cw - 24.0).max(0.0);
        let mut truncated = label_text.clone();
        loop {
            let te = cr
                .text_extents(&truncated)
                .unwrap_or_else(|_| cr.text_extents("…").unwrap());
            if te.width() <= max_label_w || truncated.len() <= 1 {
                break;
            }
            truncated.pop();
            truncated.push('…');
        }
        let te = cr
            .text_extents(&truncated)
            .unwrap_or_else(|_| cr.text_extents("…").unwrap());
        let tx = cx + (cw - te.width()) / 2.0;
        let ty = cy + (ch + te.height()) / 2.0;
        let _ = cr.move_to(tx, ty);
        let _ = cr.show_text(&truncated);
        cr.restore().ok();
    }

    // ── Audition clip visual ──────────────────────────────────────────────
    if clip.kind == crate::model::clip::ClipKind::Audition && cw > 20.0 {
        // "AUD" badge in the top-left.
        cr.save().ok();
        let badge_w = 22.0_f64.min(cw * 0.3);
        let badge_h = 14.0_f64.min(ch * 0.4);
        let bx = cx + 4.0;
        let by = cy + 4.0;
        cr.rectangle(bx, by, badge_w, badge_h);
        cr.set_source_rgba(0.55, 0.40, 0.10, 0.9);
        let _ = cr.fill();
        cr.set_source_rgba(1.0, 1.0, 1.0, 0.95);
        let fs = badge_h * 0.65;
        cr.set_font_size(fs);
        let _ = cr.move_to(bx + 2.0, by + badge_h - 3.0);
        let _ = cr.show_text("AUD");
        cr.restore().ok();

        // "n / m" indicator on the right edge of the clip showing
        // active-take index out of total takes.
        if let Some(takes) = clip.audition_takes.as_ref() {
            if !takes.is_empty() && cw > 60.0 {
                cr.save().ok();
                let indicator = format!("{}/{}", clip.audition_active_take_index + 1, takes.len());
                let fs2 = (ch * 0.32).clamp(8.0, 12.0);
                cr.set_font_size(fs2);
                cr.set_source_rgba(1.0, 1.0, 1.0, 0.85);
                let te = cr
                    .text_extents(&indicator)
                    .unwrap_or_else(|_| cr.text_extents("?").unwrap());
                let tx = cx + cw - te.width() - 6.0;
                let ty = cy + 4.0 + te.height();
                let _ = cr.move_to(tx, ty);
                let _ = cr.show_text(&indicator);
                cr.restore().ok();
            }
        }

        // Centered label using the host clip's label (which mirrors the
        // active take's title).
        cr.save().ok();
        let font_sz = (ch * 0.35).clamp(TRACK_LABEL_FONT_SIZE_MIN, TRACK_LABEL_FONT_SIZE_MAX);
        cr.set_font_size(font_sz);
        cr.set_source_rgba(1.0, 1.0, 1.0, 0.92);
        let label_text = &clip.label;
        let max_label_w = (cw - 56.0).max(0.0);
        let mut truncated = label_text.clone();
        loop {
            let te = cr
                .text_extents(&truncated)
                .unwrap_or_else(|_| cr.text_extents("…").unwrap());
            if te.width() <= max_label_w || truncated.len() <= 1 {
                break;
            }
            truncated.pop();
            truncated.push('…');
        }
        let te = cr
            .text_extents(&truncated)
            .unwrap_or_else(|_| cr.text_extents("…").unwrap());
        let tx = cx + (cw - te.width()) / 2.0;
        let ty = cy + (ch + te.height()) / 2.0;
        let _ = cr.move_to(tx, ty);
        let _ = cr.show_text(&truncated);
        cr.restore().ok();
    }

    // ── Thumbnail strip for video clips ──────────────────────────────────
    if track.is_video()
        && clip.kind != crate::model::clip::ClipKind::Title
        && clip.kind != crate::model::clip::ClipKind::Adjustment
        && clip.kind != crate::model::clip::ClipKind::Compound
        && clip.kind != crate::model::clip::ClipKind::Multicam
        && clip.kind != crate::model::clip::ClipKind::Audition
        && cw > 20.0
    {
        const THUMB_ASPECT: f64 = 160.0 / 90.0;
        const MAX_THUMB_TILES_PER_CLIP: usize = 6;
        const MAX_NEW_THUMB_REQUESTS_PER_CLIP_PER_DRAW: usize = 2;

        let inner_x = cx + 1.0;
        let inner_y = cy + 1.0;
        let inner_w = (cw - 2.0).max(0.0);
        let inner_h = (ch - 2.0).max(0.0);

        if inner_w > 1.0 && inner_h > 1.0 {
            let src_span = clip.source_duration();
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
                    let sample_time =
                        if clip.kind == crate::model::clip::ClipKind::Image && !clip.animated_svg {
                            0
                        } else {
                            clip.source_in + src_offset
                        };

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
                let is_img = clip.kind == crate::model::clip::ClipKind::Image && !clip.animated_svg;
                let start_time = if is_img { 0 } else { clip.source_in };
                let end_time = if is_img {
                    0
                } else {
                    clip.source_out.saturating_sub(1).max(clip.source_in)
                };
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
    if track.is_audio() && cw > 8.0 {
        wcache.request(&clip.source_path);
        // Only compute peaks for the visible portion of the clip to avoid
        // allocating/iterating over tens of thousands of off-screen pixels.
        let vis_x0 = cx.max(TRACK_LABEL_WIDTH);
        let vis_x1 = (cx + cw).min(view_width);
        let vis_px = (vis_x1 - vis_x0).ceil().max(0.0) as usize;
        if vis_px > 0 {
            let src_span_ns = clip.source_duration() as f64;
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
                let volumes = compute_per_pixel_volumes(clip, frac0, frac1, vis_px);
                draw_waveform_batched(cr, &peaks, vis_x0, mid, ch / 2.0 - 2.0, &volumes, 0.85);
                draw_volume_envelope(cr, &volumes, vis_x0, mid, ch / 2.0 - 2.0);
                cr.restore().ok();
            }
        }
    }

    // ── Waveform overlay for video clips (when preference enabled) ────────
    if track.is_video() && st.show_waveform_on_video && cw > 8.0 {
        wcache.request(&clip.source_path);
        let vis_x0 = cx.max(TRACK_LABEL_WIDTH);
        let vis_x1 = (cx + cw).min(view_width);
        let vis_px = (vis_x1 - vis_x0).ceil().max(0.0) as usize;
        if vis_px > 0 {
            let src_span_ns = clip.source_duration() as f64;
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
                let volumes = compute_per_pixel_volumes(clip, frac0, frac1, vis_px);
                draw_waveform_batched(
                    cr,
                    &peaks,
                    vis_x0,
                    wave_mid,
                    wave_h / 2.0 - 1.0,
                    &volumes,
                    0.9,
                );
                draw_volume_envelope(cr, &volumes, vis_x0, wave_mid, wave_h / 2.0 - 1.0);
                cr.restore().ok();
            }
        }
    }

    if is_selected {
        cr.set_source_rgb(1.0, 0.85, 0.0);
        cr.set_line_width(2.0);
        rounded_rect(cr, cx, cy, cw.max(4.0), ch, 4.0);
        cr.stroke().ok();

        if is_link_peer_highlighted {
            cr.set_source_rgba(0.45, 0.95, 1.0, 0.95);
            cr.set_line_width(1.5);
            rounded_rect(
                cr,
                cx + 3.0,
                cy + 3.0,
                (cw - 6.0).max(2.0),
                (ch - 6.0).max(2.0),
                3.0,
            );
            cr.stroke().ok();
        }

        // Draw trim handles (lighter shaded edges)
        cr.set_source_rgba(1.0, 1.0, 1.0, 0.3);
        cr.rectangle(cx, cy, TRIM_HANDLE_PX, ch);
        cr.fill().ok();
        cr.rectangle(cx + cw - TRIM_HANDLE_PX, cy, TRIM_HANDLE_PX, ch);
        cr.fill().ok();
    } else if is_group_peer_highlighted {
        cr.set_source_rgba(0.95, 0.95, 0.55, 0.95);
        cr.set_line_width(1.5);
        rounded_rect(cr, cx, cy, cw.max(4.0), ch, 4.0);
        cr.stroke().ok();
    } else if is_transition_hover {
        cr.set_source_rgba(0.55, 0.85, 1.0, 0.95);
        cr.set_line_width(2.0);
        rounded_rect(cr, cx, cy, cw.max(4.0), ch, 4.0);
        cr.stroke().ok();
    }

    let selected_keyframe_times = st.selected_keyframe_local_times_for_clip(&clip.id);
    draw_clip_keyframe_markers(
        cr,
        clip,
        cx,
        cy,
        cw,
        ch,
        view_width,
        is_selected,
        &selected_keyframe_times,
    );

    if cw > 30.0 {
        let keyframe_count = clip_phase1_keyframe_count(clip);
        let has_keyframe_badge = keyframe_count > 0;
        let display_label = if has_keyframe_badge {
            format!("◆ {}", clip.label)
        } else {
            clip.label.clone()
        };

        cr.set_source_rgb(1.0, 1.0, 1.0);
        cr.set_font_size(11.0);
        let _ = cr.move_to(cx + 6.0, cy + ch / 2.0 + 4.0);
        let _ = cr.show_text(&display_label);

        // Speed badge: show e.g. "2×" or "0.5×" when speed ≠ 1.0, and "◀" when reversed
        let has_speed_badge =
            (clip.speed - 1.0).abs() > 0.01 || clip.reverse || !clip.speed_keyframes.is_empty();
        let has_lut_badge = !clip.lut_paths.is_empty();
        let has_missing_badge = clip.kind != crate::model::clip::ClipKind::Title
            && clip.kind != crate::model::clip::ClipKind::Adjustment
            && clip.kind != crate::model::clip::ClipKind::Compound
            && clip.kind != crate::model::clip::ClipKind::Multicam
            && clip.kind != crate::model::clip::ClipKind::Audition
            && st.source_is_missing(&clip.source_path);
        let has_link_badge = clip
            .link_group_id
            .as_ref()
            .map(|gid| !gid.is_empty())
            .unwrap_or(false);
        let mut badge_right = cx + cw - 6.0;
        if has_keyframe_badge && cw > 78.0 {
            let badge = format!("KF {keyframe_count}");
            cr.set_font_size(10.0);
            if let Ok(ext) = cr.text_extents(&badge) {
                let bx = badge_right - ext.width();
                let by = cy + 14.0;
                cr.set_source_rgba(0.0, 0.0, 0.0, 0.55);
                rounded_rect(cr, bx - 2.0, by - 11.0, ext.width() + 4.0, 14.0, 2.0);
                cr.fill().ok();
                cr.set_source_rgb(1.0, 0.86, 0.24);
                let _ = cr.move_to(bx, by);
                let _ = cr.show_text(&badge);
                badge_right = bx - 8.0;
            }
        }
        if has_speed_badge && cw > 60.0 {
            let badge = if !clip.speed_keyframes.is_empty() {
                if clip.reverse {
                    "\u{23F2} \u{25C0} Ramp".to_string()
                } else {
                    "\u{23F2} Ramp".to_string()
                }
            } else if clip.reverse {
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
                let bx = badge_right - ext.width();
                let by = cy + 14.0;
                // Badge background
                cr.set_source_rgba(0.0, 0.0, 0.0, 0.55);
                rounded_rect(cr, bx - 2.0, by - 11.0, ext.width() + 4.0, 14.0, 2.0);
                cr.fill().ok();
                cr.set_source_rgb(1.0, 0.9, 0.2);
                let _ = cr.move_to(bx, by);
                let _ = cr.show_text(&badge);
                badge_right = bx - 8.0;
            }
        }

        // LUT badge: small "LUT" indicator when a LUT file is assigned
        if has_lut_badge && cw > 80.0 {
            let badge = "LUT";
            cr.set_font_size(10.0);
            if let Ok(ext) = cr.text_extents(badge) {
                let bx = badge_right - ext.width();
                let by = cy + 14.0;
                cr.set_source_rgba(0.0, 0.0, 0.0, 0.55);
                rounded_rect(cr, bx - 2.0, by - 11.0, ext.width() + 4.0, 14.0, 2.0);
                cr.fill().ok();
                cr.set_source_rgb(0.4, 0.8, 1.0);
                let _ = cr.move_to(bx, by);
                let _ = cr.show_text(badge);
                badge_right = bx - 8.0;
            }
        }

        if has_missing_badge && cw > 95.0 {
            let badge = "OFFLINE";
            cr.set_font_size(10.0);
            if let Ok(ext) = cr.text_extents(badge) {
                let bx = badge_right - ext.width();
                let by = cy + 14.0;
                cr.set_source_rgba(0.0, 0.0, 0.0, 0.55);
                rounded_rect(cr, bx - 2.0, by - 11.0, ext.width() + 4.0, 14.0, 2.0);
                cr.fill().ok();
                cr.set_source_rgb(1.0, 0.45, 0.45);
                let _ = cr.move_to(bx, by);
                let _ = cr.show_text(badge);
                badge_right = bx - 8.0;
            }
        }

        if has_link_badge && cw > 120.0 {
            let badge = "LINK";
            cr.set_font_size(10.0);
            if let Ok(ext) = cr.text_extents(badge) {
                let bx = badge_right - ext.width();
                let by = cy + 14.0;
                cr.set_source_rgba(0.0, 0.0, 0.0, 0.55);
                rounded_rect(cr, bx - 2.0, by - 11.0, ext.width() + 4.0, 14.0, 2.0);
                cr.fill().ok();
                cr.set_source_rgb(0.55, 0.95, 1.0);
                let _ = cr.move_to(bx, by);
                let _ = cr.show_text(badge);
                badge_right = bx - 8.0;
            }
        }

        // CC badge: show when clip has subtitle segments
        if !clip.subtitle_segments.is_empty() && cw > 50.0 {
            let badge = "CC";
            cr.set_font_size(10.0);
            if let Ok(ext) = cr.text_extents(badge) {
                let bx = badge_right - ext.width();
                let by = cy + 14.0;
                cr.set_source_rgba(0.0, 0.0, 0.0, 0.55);
                rounded_rect(cr, bx - 2.0, by - 11.0, ext.width() + 4.0, 14.0, 2.0);
                cr.fill().ok();
                cr.set_source_rgb(0.9, 0.75, 1.0); // light purple
                let _ = cr.move_to(bx, by);
                let _ = cr.show_text(badge);
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct ClipKeyframeMarkerGeometry {
    x: f64,
    local_time_ns: u64,
    row: usize,
    color: (f64, f64, f64),
}

#[derive(Debug, Clone)]
struct KeyframeMarkerHit {
    clip_id: String,
    track_id: String,
    clip_label: String,
    local_time_ns: u64,
    timeline_ns: u64,
    impacted_properties: Vec<&'static str>,
}

fn clip_keyframe_lanes(
    clip: &Clip,
) -> [(
    &[crate::model::clip::NumericKeyframe],
    &'static str,
    (f64, f64, f64),
); 12] {
    [
        (&clip.scale_keyframes, "Scale", (1.0, 0.83, 0.30)),
        (&clip.opacity_keyframes, "Opacity", (0.94, 0.94, 0.94)),
        (&clip.position_x_keyframes, "Position X", (0.45, 0.90, 1.0)),
        (&clip.position_y_keyframes, "Position Y", (0.82, 0.62, 1.0)),
        (&clip.volume_keyframes, "Volume", (0.55, 1.0, 0.62)),
        (&clip.pan_keyframes, "Pan", (1.0, 0.66, 0.36)),
        (&clip.speed_keyframes, "Speed", (0.98, 0.82, 0.35)),
        (&clip.rotate_keyframes, "Rotate", (1.0, 0.55, 0.85)),
        (&clip.crop_left_keyframes, "Crop Left", (1.0, 0.76, 0.36)),
        (&clip.crop_right_keyframes, "Crop Right", (1.0, 0.72, 0.33)),
        (&clip.crop_top_keyframes, "Crop Top", (1.0, 0.69, 0.30)),
        (
            &clip.crop_bottom_keyframes,
            "Crop Bottom",
            (1.0, 0.65, 0.28),
        ),
    ]
}

fn collect_keyframe_property_labels_at_local_time(
    clip: &Clip,
    local_time_ns: u64,
) -> Vec<&'static str> {
    let duration_ns = clip.duration();
    if duration_ns == 0 {
        return Vec::new();
    }
    let local = local_time_ns.min(duration_ns);
    clip_keyframe_lanes(clip)
        .iter()
        .filter_map(|(keyframes, label, _)| {
            keyframes
                .iter()
                .any(|kf| kf.time_ns.min(duration_ns) == local)
                .then_some(*label)
        })
        .collect()
}

fn collect_keyframe_local_times_in_range(
    clip: &Clip,
    low_local_ns: u64,
    high_local_ns: u64,
) -> HashSet<u64> {
    let duration_ns = clip.duration();
    if duration_ns == 0 {
        return HashSet::new();
    }
    let low = low_local_ns.min(duration_ns);
    let high = high_local_ns.min(duration_ns);
    let (min_t, max_t) = if low <= high {
        (low, high)
    } else {
        (high, low)
    };
    let mut times = HashSet::new();
    for (keyframes, _label, _color) in clip_keyframe_lanes(clip) {
        for kf in keyframes {
            let t = kf.time_ns.min(duration_ns);
            if t >= min_t && t <= max_t {
                times.insert(t);
            }
        }
    }
    times
}

fn clip_keyframe_marker_geometry(
    clip: &Clip,
    cx: f64,
    cw: f64,
    view_left: f64,
    view_right: f64,
) -> Vec<ClipKeyframeMarkerGeometry> {
    let duration_ns = clip.duration();
    if duration_ns == 0 || cw <= 2.0 {
        return Vec::new();
    }

    let mut markers = Vec::new();
    for (row, (keyframes, _label, color)) in clip_keyframe_lanes(clip).iter().enumerate() {
        if keyframes.is_empty() {
            continue;
        }
        let mut seen_times = HashSet::new();
        for kf in *keyframes {
            let local_time_ns = kf.time_ns.min(duration_ns);
            if !seen_times.insert(local_time_ns) {
                continue;
            }
            let frac = local_time_ns as f64 / duration_ns as f64;
            let x = cx + frac * cw;
            if x >= view_left && x <= view_right {
                markers.push(ClipKeyframeMarkerGeometry {
                    x,
                    local_time_ns,
                    row,
                    color: *color,
                });
            }
        }
    }
    markers
}

fn draw_clip_keyframe_markers(
    cr: &gtk::cairo::Context,
    clip: &Clip,
    cx: f64,
    cy: f64,
    cw: f64,
    ch: f64,
    view_width: f64,
    is_selected: bool,
    selected_local_times: &HashSet<u64>,
) {
    if cw <= 10.0 || ch <= 10.0 {
        return;
    }
    let markers = clip_keyframe_marker_geometry(clip, cx, cw, TRACK_LABEL_WIDTH, view_width);
    if markers.is_empty() {
        return;
    }

    let marker_h = (ch * 0.22).clamp(4.0, 8.0);
    let row_pitch = 3.4;
    let base_y = cy + 3.0;
    let alpha = if is_selected { 1.0 } else { 0.9 };

    cr.save().ok();
    rounded_rect(
        cr,
        cx + 1.0,
        cy + 1.0,
        (cw - 2.0).max(1.0),
        (ch - 2.0).max(1.0),
        3.0,
    );
    cr.clip();
    for marker in markers {
        let marker_y = base_y + marker.row as f64 * row_pitch;
        if marker_y + marker_h > cy + ch - 1.0 {
            continue;
        }
        let marker_selected = selected_local_times.contains(&marker.local_time_ns);
        let diamond_mid_y = cy + 4.5;
        cr.set_source_rgba(marker.color.0, marker.color.1, marker.color.2, alpha);
        let diamond_half = if marker_selected { 4.2 } else { 3.0 };
        cr.move_to(marker.x, diamond_mid_y - diamond_half);
        cr.line_to(marker.x + diamond_half, diamond_mid_y);
        cr.line_to(marker.x, diamond_mid_y + diamond_half);
        cr.line_to(marker.x - diamond_half, diamond_mid_y);
        cr.close_path();
        cr.fill().ok();
        cr.set_source_rgba(
            if marker_selected { 1.0 } else { 0.05 },
            if marker_selected { 0.92 } else { 0.05 },
            if marker_selected { 0.28 } else { 0.05 },
            if marker_selected { 0.95 } else { 0.65 },
        );
        cr.set_line_width(if marker_selected { 1.2 } else { 0.9 });
        cr.move_to(marker.x, diamond_mid_y - diamond_half);
        cr.line_to(marker.x + diamond_half, diamond_mid_y);
        cr.line_to(marker.x, diamond_mid_y + diamond_half);
        cr.line_to(marker.x - diamond_half, diamond_mid_y);
        cr.close_path();
        cr.stroke().ok();
        cr.set_source_rgba(
            1.0,
            1.0,
            1.0,
            if marker_selected {
                0.52
            } else if is_selected {
                0.28
            } else {
                0.22
            },
        );
        cr.rectangle(marker.x - 0.5, cy + 1.0, 1.0, (ch - 2.0).max(1.0));
        cr.fill().ok();
        cr.set_source_rgba(marker.color.0, marker.color.1, marker.color.2, alpha);
        let lane_w = if marker_selected { 4.0 } else { 3.0 };
        cr.rectangle(marker.x - lane_w * 0.5, marker_y, lane_w, marker_h);
        cr.fill().ok();
        cr.set_source_rgba(0.05, 0.05, 0.05, 0.45);
        cr.set_line_width(0.8);
        cr.rectangle(marker.x - lane_w * 0.5, marker_y, lane_w, marker_h);
        cr.stroke().ok();
    }
    cr.restore().ok();
}

fn clip_phase1_keyframe_count(clip: &Clip) -> usize {
    clip.scale_keyframes.len()
        + clip.opacity_keyframes.len()
        + clip.position_x_keyframes.len()
        + clip.position_y_keyframes.len()
        + clip.volume_keyframes.len()
        + clip.pan_keyframes.len()
        + clip.speed_keyframes.len()
        + clip.rotate_keyframes.len()
        + clip.crop_left_keyframes.len()
        + clip.crop_right_keyframes.len()
        + clip.crop_top_keyframes.len()
        + clip.crop_bottom_keyframes.len()
}

fn clip_fill_color(clip: &Clip, track_kind: TrackKind) -> (f64, f64, f64) {
    match clip.color_label {
        crate::model::clip::ClipColorLabel::None => {
            if clip.kind == crate::model::clip::ClipKind::Title {
                return (0.75, 0.62, 0.22); // warm gold for title clips
            }
            if clip.kind == crate::model::clip::ClipKind::Adjustment {
                return (0.55, 0.35, 0.75); // purple for adjustment layers
            }
            if clip.kind == crate::model::clip::ClipKind::Compound {
                return (0.20, 0.63, 0.60); // teal for compound clips
            }
            if clip.kind == crate::model::clip::ClipKind::Multicam {
                return (0.85, 0.50, 0.17); // orange for multicam clips
            }
            if clip.kind == crate::model::clip::ClipKind::Audition {
                return (0.78, 0.62, 0.22); // gold for audition clips
            }
            match track_kind {
                TrackKind::Video => (0.17, 0.47, 0.85),
                TrackKind::Audio => (0.18, 0.65, 0.45),
            }
        }
        crate::model::clip::ClipColorLabel::Red => (0.78, 0.27, 0.27),
        crate::model::clip::ClipColorLabel::Orange => (0.83, 0.49, 0.20),
        crate::model::clip::ClipColorLabel::Yellow => (0.78, 0.68, 0.20),
        crate::model::clip::ClipColorLabel::Green => (0.28, 0.66, 0.33),
        crate::model::clip::ClipColorLabel::Teal => (0.20, 0.63, 0.60),
        crate::model::clip::ClipColorLabel::Blue => (0.22, 0.48, 0.85),
        crate::model::clip::ClipColorLabel::Purple => (0.53, 0.38, 0.80),
        crate::model::clip::ClipColorLabel::Magenta => (0.78, 0.35, 0.68),
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
/// Build a per-pixel volume vector by evaluating volume keyframes across the
/// visible fraction of a clip.  Falls back to the static clip volume when no
/// keyframes are present (single multiplication, same as before).
fn compute_per_pixel_volumes(clip: &Clip, frac0: f64, frac1: f64, vis_px: usize) -> Vec<f64> {
    let vol = (clip.volume as f64).max(0.0);
    if clip.volume_keyframes.is_empty() || vis_px == 0 {
        return vec![vol; vis_px];
    }
    let dur_ns = clip.duration() as f64;
    (0..vis_px)
        .map(|i| {
            let frac = frac0 + (i as f64 / vis_px as f64) * (frac1 - frac0);
            let local_ns = (frac * dur_ns) as u64;
            Clip::evaluate_keyframed_value(&clip.volume_keyframes, local_ns, vol).max(0.0)
        })
        .collect()
}

/// Draw a thin line tracing the volume envelope on top of the waveform.
/// Only draws when the volume varies (i.e. keyframes exist and produce
/// different values across the visible span).
fn draw_volume_envelope(
    cr: &gtk::cairo::Context,
    volumes: &[f64],
    cx: f64,
    mid_y: f64,
    half_range: f64,
) {
    if volumes.len() < 2 {
        return;
    }
    // Skip the envelope when volume is constant (no keyframes or flat curve).
    let first = volumes[0];
    if volumes.iter().all(|&v| (v - first).abs() < 1e-6) {
        return;
    }
    cr.set_source_rgba(1.0, 1.0, 1.0, 0.55);
    cr.set_line_width(1.0);
    for (i, &vol) in volumes.iter().enumerate() {
        let x = cx + i as f64 + 0.5;
        let y = mid_y - (vol.clamp(0.0, 1.0) * half_range);
        if i == 0 {
            cr.move_to(x, y);
        } else {
            cr.line_to(x, y);
        }
    }
    cr.stroke().ok();
}

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
    volumes: &[f64],
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
        let vol = volumes.get(i).copied().unwrap_or(1.0);
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

#[allow(dead_code)]
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

#[allow(deprecated)]
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
        ("S", "Toggle solo for selected track"),
        ("F", "Match Frame (load source in Source Monitor)"),
        ("Shift+F", "Create freeze-frame clip from selected clip"),
        (
            "Ctrl+Shift+B",
            "Join selected through-edit boundary into one clip",
        ),
        (
            "Escape",
            "Cancel armed MusicGen region draw, exit compound edit, or switch to Select tool",
        ),
        (
            "Delete / Bksp",
            "Delete selected clip(s), or selected keyframe column(s)",
        ),
        (
            "Shift+Delete / Shift+Bksp",
            "Ripple delete selected clip(s)",
        ),
        (
            "Drag keyframe marker",
            "Move selected keyframe column(s) on the clip",
        ),
        (
            "Alt+Drag clip body",
            "Box-select keyframe columns within that clip",
        ),
        (
            "1 / 2 / 3 / 4",
            "Set selected keyframe interpolation (L/In/Out/InOut)",
        ),
        (
            "Shift+Click (Timeline)",
            "Range-select clips (same-track span or cross-track time range)",
        ),
        (
            "Ctrl/Cmd+Click (Timeline)",
            "Toggle clip in current selection",
        ),
        ("Ctrl+A", "Select all timeline clips"),
        (
            "Drag empty timeline body",
            "Marquee-select intersecting clips",
        ),
        ("M", "Add marker at playhead"),
        ("Right-click ruler", "Remove nearest marker"),
        ("Right-click transition", "Remove transition at boundary"),
        ("Click track 'S' badge", "Toggle solo on that track"),
        ("Ctrl+,", "Open Preferences"),
        ("Ctrl+Z", "Undo"),
        ("Ctrl+Y / Ctrl+Shift+Z", "Redo"),
        ("Ctrl+C", "Copy selected timeline clip"),
        ("Ctrl+V", "Paste insert clip at playhead"),
        ("Ctrl+Shift+V", "Paste copied clip attributes"),
        ("Ctrl+G", "Group selected clips"),
        ("Ctrl+Shift+G", "Ungroup selected clips"),
        ("Ctrl+L", "Link selected clips"),
        ("Ctrl+Shift+L", "Unlink selected clips"),
        ("Ctrl+Shift+→", "Select clips forward from playhead"),
        ("Ctrl+Shift+←", "Select clips backward from playhead"),
        ("Ctrl+J", "Go to timecode (jump playhead)"),
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

/// Re-order clips on the selected track by their `scene_id`, matching the
/// screenplay scene order stored in the project's `parsed_script_path`.
///
/// Only affects clips that have a `scene_id` set (from script-to-timeline
/// assembly). Clips without a `scene_id` are left at the end.
fn reorder_track_by_script(state: &std::rc::Rc<std::cell::RefCell<TimelineState>>) -> bool {
    // First, gather the info we need without holding borrows.
    let (track_id, old_clips, script_path) = {
        let st = state.borrow();
        let proj = st.project.borrow();
        let track_id = match st.selected_track_id.as_ref() {
            Some(id) => id.clone(),
            None => return false,
        };
        let track = match proj.tracks.iter().find(|t| t.id == track_id) {
            Some(t) => t,
            None => return false,
        };
        if !track.clips.iter().any(|c| c.scene_id.is_some()) {
            return false;
        }
        (
            track_id,
            track.clips.clone(),
            proj.parsed_script_path.clone(),
        )
    };

    // Try to load the script for scene ordering.
    // Since scene IDs are UUIDs generated at parse time, we use the
    // clip scene_id values directly for relative ordering (alphabetical
    // as a fallback when the script can't be re-parsed).
    let scene_order: std::collections::HashMap<String, usize> =
        if let Some(ref script_path) = script_path {
            match crate::media::script::parse_script(script_path) {
                Ok(script) => script
                    .scenes
                    .iter()
                    .enumerate()
                    .map(|(i, s)| (s.id.clone(), i))
                    .collect(),
                Err(_) => std::collections::HashMap::new(),
            }
        } else {
            std::collections::HashMap::new()
        };

    // Sort clips: by scene order for those with scene_id, rest at the end.
    let mut new_clips = old_clips.clone();
    new_clips.sort_by(|a, b| {
        let a_order = a
            .scene_id
            .as_ref()
            .and_then(|id| scene_order.get(id))
            .copied()
            .unwrap_or(usize::MAX);
        let b_order = b
            .scene_id
            .as_ref()
            .and_then(|id| scene_order.get(id))
            .copied()
            .unwrap_or(usize::MAX);
        a_order.cmp(&b_order)
    });

    // Reassign timeline_start values gap-free.
    let mut cursor: u64 = 0;
    for clip in &mut new_clips {
        clip.timeline_start = cursor;
        cursor += clip.duration() as u64;
    }

    // Check if anything actually changed.
    let changed = new_clips
        .iter()
        .zip(old_clips.iter())
        .any(|(a, b)| a.id != b.id || a.timeline_start != b.timeline_start);

    if changed {
        let cmd = crate::undo::SetTrackClipsCommand {
            track_id,
            old_clips,
            new_clips,
            label: "Re-order by Script".into(),
        };
        let project_rc = state.borrow().project.clone();
        let mut proj = project_rc.borrow_mut();
        state.borrow_mut().history.execute(Box::new(cmd), &mut proj);
    }

    changed
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::clip::{Clip, ClipKind, KeyframeInterpolation, NumericKeyframe};
    use crate::model::project::Project;
    use std::cell::RefCell;
    use std::collections::{HashMap, HashSet};
    use std::rc::Rc;

    fn timeline_state_with_video_clips(
        spec: &[(&str, u64)],
    ) -> (TimelineState, String, Vec<String>) {
        let mut project = Project::new("Selection tests");
        let video_idx = project
            .tracks
            .iter()
            .position(|t| t.is_video())
            .expect("default project should include a video track");
        let track_id = project.tracks[video_idx].id.clone();
        let mut ids = Vec::new();
        for (id, start) in spec {
            let mut clip = Clip::new(format!("{id}.mp4"), 1_000_000_000, *start, ClipKind::Video);
            clip.id = (*id).to_string();
            project.tracks[video_idx].add_clip(clip);
            ids.push((*id).to_string());
        }
        (
            TimelineState::new(Rc::new(RefCell::new(project))),
            track_id,
            ids,
        )
    }

    fn timeline_state_with_two_video_tracks(
        first_spec: &[(&str, u64)],
        second_spec: &[(&str, u64)],
    ) -> (TimelineState, String, String, Vec<String>, Vec<String>) {
        let mut project = Project::new("Selection tests");
        project.add_video_track();
        project.dirty = false;
        let video_indices: Vec<usize> = project
            .tracks
            .iter()
            .enumerate()
            .filter_map(|(idx, t)| t.is_video().then_some(idx))
            .collect();
        let first_idx = video_indices[0];
        let second_idx = video_indices[1];
        let first_track_id = project.tracks[first_idx].id.clone();
        let second_track_id = project.tracks[second_idx].id.clone();
        let mut first_ids = Vec::new();
        let mut second_ids = Vec::new();
        for (id, start) in first_spec {
            let mut clip = Clip::new(format!("{id}.mp4"), 1_000_000_000, *start, ClipKind::Video);
            clip.id = (*id).to_string();
            project.tracks[first_idx].add_clip(clip);
            first_ids.push((*id).to_string());
        }
        for (id, start) in second_spec {
            let mut clip = Clip::new(format!("{id}.mp4"), 1_000_000_000, *start, ClipKind::Video);
            clip.id = (*id).to_string();
            project.tracks[second_idx].add_clip(clip);
            second_ids.push((*id).to_string());
        }
        (
            TimelineState::new(Rc::new(RefCell::new(project))),
            first_track_id,
            second_track_id,
            first_ids,
            second_ids,
        )
    }

    fn timeline_state_with_audio_clips(
        spec: &[(&str, u64, u64)],
    ) -> (TimelineState, String, Vec<String>) {
        let mut project = Project::new("Music generation tests");
        let audio_idx = project
            .tracks
            .iter()
            .position(|t| t.is_audio())
            .expect("default project should include an audio track");
        let track_id = project.tracks[audio_idx].id.clone();
        let mut ids = Vec::new();
        for (id, start, duration) in spec {
            let mut clip = Clip::new(format!("{id}.wav"), *duration, *start, ClipKind::Audio);
            clip.id = (*id).to_string();
            project.tracks[audio_idx].add_clip(clip);
            ids.push((*id).to_string());
        }
        (
            TimelineState::new(Rc::new(RefCell::new(project))),
            track_id,
            ids,
        )
    }

    #[test]
    fn ruler_hit_test_stays_in_fixed_header_band() {
        let mut state = TimelineState::new(Rc::new(RefCell::new(Project::new("Ruler tests"))));
        assert!(ruler_hit_test(&state, 0.0));
        assert!(ruler_hit_test(&state, RULER_HEIGHT - 0.5));
        assert!(!ruler_hit_test(&state, RULER_HEIGHT));

        state.vertical_scroll_offset = 180.0;
        assert!(ruler_hit_test(&state, 0.0));
        assert!(ruler_hit_test(&state, RULER_HEIGHT - 0.5));
        assert!(!ruler_hit_test(&state, RULER_HEIGHT));
    }

    fn timeline_state_with_through_edit_track() -> (TimelineState, String) {
        let mut project = Project::new("Through-edit indicator tests");
        let video_idx = project
            .tracks
            .iter()
            .position(|t| t.is_video())
            .expect("default project should include a video track");

        let track = &mut project.tracks[video_idx];
        track.id = "through-edit-track".to_string();
        track.clips.clear();

        let mut left = Clip::new("camera-a.mov", 1_000_000_000, 0, ClipKind::Video);
        left.id = "left".to_string();
        left.source_in = 0;
        left.source_out = 1_000_000_000;
        track.add_clip(left);

        let mut right = Clip::new(
            "camera-a.mov",
            2_000_000_000,
            1_000_000_000,
            ClipKind::Video,
        );
        right.id = "right".to_string();
        right.source_in = 1_000_000_000;
        right.source_out = 2_000_000_000;
        track.add_clip(right);

        (
            TimelineState::new(Rc::new(RefCell::new(project))),
            "through-edit-track".to_string(),
        )
    }

    fn timeline_state_with_windowed_compound(
        compound_timeline_start: u64,
        compound_source_in: u64,
    ) -> (TimelineState, String) {
        let mut project = Project::new("Compound playhead tests");
        let video_idx = project
            .tracks
            .iter()
            .position(|t| t.is_video())
            .expect("default project should include a video track");
        let track = &mut project.tracks[video_idx];
        track.clips.clear();

        let mut inner_track = crate::model::track::Track::new_video("Inner Video");
        let mut inner_clip = Clip::new("inner.mov", 30_000_000_000, 0, ClipKind::Video);
        inner_clip.id = "inner".to_string();
        inner_track.add_clip(inner_clip);

        let mut compound = Clip::new_compound(compound_timeline_start, vec![inner_track]);
        compound.id = "compound".to_string();
        compound.source_in = compound_source_in;
        compound.source_out = 30_000_000_000;
        track.add_clip(compound);

        let mut state = TimelineState::new(Rc::new(RefCell::new(project)));
        state.enter_compound("compound".to_string());
        (state, "compound".to_string())
    }

    #[test]
    fn editing_playhead_uses_full_internal_time_for_windowed_compounds() {
        let compound_timeline_start = 10_000_000_000;
        let compound_source_in = 10_000_000_000;
        let (mut st, compound_id) =
            timeline_state_with_windowed_compound(compound_timeline_start, compound_source_in);

        assert_eq!(
            st.root_playhead_from_internal_ns(0),
            compound_timeline_start
        );

        st.playhead_ns = compound_timeline_start;
        assert_eq!(st.editing_playhead_ns(), 0);
        assert_eq!(st.internal_playhead_ns(), 0);

        st.set_playhead_visual(2_000_000_000);
        assert_eq!(st.playhead_ns, compound_timeline_start + 2_000_000_000);
        assert_eq!(st.editing_playhead_ns(), 2_000_000_000);
        assert_eq!(st.internal_playhead_ns(), 2_000_000_000);

        assert_eq!(st.compound_nav_stack, vec![compound_id]);
    }

    #[test]
    fn finish_music_generation_region_drag_accepts_empty_audio_region() {
        let (mut st, track_id, _ids) = timeline_state_with_audio_clips(&[]);
        st.arm_music_generation_region(track_id.clone());
        st.music_generation_region_draft = Some(MusicGenerationRegionDraft {
            track_id: track_id.clone(),
            start_ns: 2_000_000_000,
            current_ns: 7_000_000_000,
        });

        let target = st
            .finish_music_generation_region_drag()
            .expect("draft should exist")
            .expect("empty five-second region should be valid");

        assert_eq!(target.track_id, track_id);
        assert_eq!(target.timeline_start_ns, 2_000_000_000);
        assert_eq!(target.timeline_end_ns, Some(7_000_000_000));
        assert!(st.music_generation_armed_track_id.is_none());
    }

    #[test]
    fn finish_music_generation_region_drag_rejects_overlap_without_disarming() {
        let (mut st, track_id, _ids) =
            timeline_state_with_audio_clips(&[("bed", 3_000_000_000, 2_000_000_000)]);
        st.arm_music_generation_region(track_id.clone());
        st.music_generation_region_draft = Some(MusicGenerationRegionDraft {
            track_id: track_id.clone(),
            start_ns: 4_000_000_000,
            current_ns: 6_000_000_000,
        });

        let error = st
            .finish_music_generation_region_drag()
            .expect("draft should exist")
            .expect_err("overlapping region should be rejected");

        assert_eq!(error, "Music region must stay in empty audio-track space.");
        assert_eq!(
            st.music_generation_armed_track_id.as_deref(),
            Some(track_id.as_str())
        );
    }

    #[test]
    fn music_generation_overlay_lifecycle_tracks_pending_failed_and_cleared_states() {
        let (mut st, track_id, _ids) = timeline_state_with_audio_clips(&[]);
        st.add_pending_music_generation_overlay(
            "job-1".to_string(),
            track_id.clone(),
            1_000_000_000,
            4_000_000_000,
        );
        assert_eq!(st.music_generation_overlays.len(), 1);
        assert_eq!(
            st.music_generation_overlays[0].status,
            MusicGenerationOverlayStatus::Pending
        );

        assert!(st.mark_music_generation_overlay_failed("job-1", "boom".to_string()));
        assert_eq!(
            st.music_generation_overlays[0].status,
            MusicGenerationOverlayStatus::Failed
        );
        assert_eq!(
            st.music_generation_overlays[0].error.as_deref(),
            Some("boom")
        );

        st.add_pending_music_generation_overlay(
            "job-2".to_string(),
            track_id,
            5_000_000_000,
            8_000_000_000,
        );
        assert_eq!(st.music_generation_overlays.len(), 1);
        assert_eq!(st.music_generation_overlays[0].job_id, "job-2");
        assert_eq!(
            st.music_generation_overlays[0].status,
            MusicGenerationOverlayStatus::Pending
        );

        assert!(st.resolve_music_generation_overlay_success("job-2"));
        assert!(st.music_generation_overlays.is_empty());
    }

    #[test]
    fn ctrl_toggle_adds_to_selection_set() {
        let (mut st, track_id, ids) =
            timeline_state_with_video_clips(&[("A", 0), ("B", 1_000_000_000)]);
        st.set_single_clip_selection(ids[0].clone(), track_id.clone());
        st.toggle_clip_selection(&ids[1], &track_id);

        assert!(st.is_clip_selected(&ids[0]));
        assert!(st.is_clip_selected(&ids[1]));
        assert_eq!(st.selected_ids_or_primary().len(), 2);
    }

    #[test]
    fn shift_range_selects_same_track_span() {
        let (mut st, track_id, ids) = timeline_state_with_video_clips(&[
            ("A", 0),
            ("B", 1_000_000_000),
            ("C", 2_000_000_000),
        ]);
        st.set_single_clip_selection(ids[0].clone(), track_id.clone());

        let changed = st.shift_select_range_to(&track_id, &ids[2]);
        let selected = st.selected_ids_or_primary();

        assert!(changed);
        assert_eq!(selected.len(), 3);
        assert!(selected.contains(&ids[0]));
        assert!(selected.contains(&ids[1]));
        assert!(selected.contains(&ids[2]));
        assert_eq!(st.selected_clip_id.as_deref(), Some(ids[2].as_str()));
    }

    #[test]
    fn shift_range_selects_cross_track_time_span() {
        let (mut st, track_a, track_b, ids_a, ids_b) = timeline_state_with_two_video_tracks(
            &[("A", 0), ("B", 2_000_000_000)],
            &[("X", 1_000_000_000), ("Y", 3_000_000_000)],
        );
        st.set_single_clip_selection(ids_a[0].clone(), track_a.clone());

        let changed = st.shift_select_range_to(&track_b, &ids_b[1]);
        let selected = st.selected_ids_or_primary();

        assert!(changed);
        assert_eq!(selected.len(), 4);
        assert!(selected.contains(&ids_a[0]));
        assert!(selected.contains(&ids_a[1]));
        assert!(selected.contains(&ids_b[0]));
        assert!(selected.contains(&ids_b[1]));
        assert_eq!(st.selected_clip_id.as_deref(), Some(ids_b[1].as_str()));
        assert_eq!(st.selected_track_id.as_deref(), Some(track_b.as_str()));
    }

    #[test]
    fn ctrl_shift_click_toggles_clicked_clip_instead_of_range_selecting() {
        let (mut st, track_a, track_b, ids_a, ids_b) = timeline_state_with_two_video_tracks(
            &[("A", 0), ("B", 2_000_000_000)],
            &[("X", 1_000_000_000), ("Y", 3_000_000_000)],
        );
        st.set_single_clip_selection(ids_a[0].clone(), track_a);

        st.select_clip_with_modifiers(&ids_b[1], &track_b, true, true);
        let selected = st.selected_ids_or_primary();

        assert_eq!(selected.len(), 2);
        assert!(selected.contains(&ids_a[0]));
        assert!(selected.contains(&ids_b[1]));
        assert!(!selected.contains(&ids_a[1]));
        assert!(!selected.contains(&ids_b[0]));
        assert_eq!(st.selected_clip_id.as_deref(), Some(ids_b[1].as_str()));
        assert_eq!(st.selected_track_id.as_deref(), Some(track_b.as_str()));
    }

    #[test]
    fn drag_move_ids_follow_current_multi_selection_when_clicked_member() {
        let (mut st, track_a, _track_b, ids_a, ids_b) = timeline_state_with_two_video_tracks(
            &[("A", 0), ("B", 2_000_000_000)],
            &[("X", 1_000_000_000)],
        );
        let mut selected = HashSet::new();
        selected.insert(ids_a[0].clone());
        selected.insert(ids_b[0].clone());
        st.set_selection_with_primary(ids_a[0].clone(), track_a, selected);

        let move_ids: HashSet<String> = st.move_clip_ids_for_drag(&ids_b[0]).into_iter().collect();
        assert_eq!(move_ids.len(), 2);
        assert!(move_ids.contains(&ids_a[0]));
        assert!(move_ids.contains(&ids_b[0]));
    }

    #[test]
    fn drag_move_ids_fallback_to_clicked_clip_when_not_in_selection() {
        let (mut st, track_a, _track_b, ids_a, ids_b) = timeline_state_with_two_video_tracks(
            &[("A", 0), ("B", 2_000_000_000)],
            &[("X", 1_000_000_000)],
        );
        let mut selected = HashSet::new();
        selected.insert(ids_a[0].clone());
        selected.insert(ids_b[0].clone());
        st.set_selection_with_primary(ids_a[0].clone(), track_a, selected);

        let move_ids: HashSet<String> = st.move_clip_ids_for_drag(&ids_a[1]).into_iter().collect();
        assert_eq!(move_ids.len(), 1);
        assert!(move_ids.contains(&ids_a[1]));
    }

    #[test]
    fn drag_move_ids_expand_group_members_when_selection_drags_grouped_clip() {
        let (mut st, track_a, track_b, ids_a, ids_b) = timeline_state_with_two_video_tracks(
            &[("A", 0)],
            &[("X", 1_000_000_000), ("Y", 2_000_000_000)],
        );
        {
            let mut proj = st.project.borrow_mut();
            let group_id = "g1".to_string();
            if let Some(track) = proj.tracks.iter_mut().find(|t| t.id == track_b) {
                for clip in &mut track.clips {
                    if clip.id == ids_b[0] || clip.id == ids_b[1] {
                        clip.group_id = Some(group_id.clone());
                    }
                }
            }
        }
        let mut selected = HashSet::new();
        selected.insert(ids_a[0].clone());
        selected.insert(ids_b[0].clone());
        st.set_selection_with_primary(ids_a[0].clone(), track_a, selected);

        let move_ids: HashSet<String> = st.move_clip_ids_for_drag(&ids_b[0]).into_iter().collect();
        assert_eq!(move_ids.len(), 3);
        assert!(move_ids.contains(&ids_a[0]));
        assert!(move_ids.contains(&ids_b[0]));
        assert!(move_ids.contains(&ids_b[1]));
    }

    #[test]
    fn set_single_clip_selection_expands_linked_members() {
        let (mut st, _track_a, track_b, _ids_a, ids_b) = timeline_state_with_two_video_tracks(
            &[("A", 0)],
            &[("X", 1_000_000_000), ("Y", 2_000_000_000)],
        );
        {
            let mut proj = st.project.borrow_mut();
            let link_group_id = "l1".to_string();
            if let Some(track) = proj.tracks.iter_mut().find(|t| t.id == track_b) {
                for clip in &mut track.clips {
                    if clip.id == ids_b[0] || clip.id == ids_b[1] {
                        clip.link_group_id = Some(link_group_id.clone());
                    }
                }
            }
        }

        st.set_single_clip_selection(ids_b[0].clone(), track_b);

        let selected = st.selected_ids_or_primary();
        assert_eq!(selected.len(), 2);
        assert!(selected.contains(&ids_b[0]));
        assert!(selected.contains(&ids_b[1]));
    }

    #[test]
    fn drag_move_ids_expand_link_members_when_selection_drags_linked_clip() {
        let (mut st, track_a, track_b, ids_a, ids_b) = timeline_state_with_two_video_tracks(
            &[("A", 0)],
            &[("X", 1_000_000_000), ("Y", 2_000_000_000)],
        );
        {
            let mut proj = st.project.borrow_mut();
            let link_group_id = "l1".to_string();
            if let Some(track) = proj.tracks.iter_mut().find(|t| t.id == track_b) {
                for clip in &mut track.clips {
                    if clip.id == ids_b[0] || clip.id == ids_b[1] {
                        clip.link_group_id = Some(link_group_id.clone());
                    }
                }
            }
        }
        let mut selected = HashSet::new();
        selected.insert(ids_a[0].clone());
        selected.insert(ids_b[0].clone());
        st.set_selection_with_primary(ids_a[0].clone(), track_a, selected);

        let move_ids: HashSet<String> = st.move_clip_ids_for_drag(&ids_b[0]).into_iter().collect();
        assert_eq!(move_ids.len(), 3);
        assert!(move_ids.contains(&ids_a[0]));
        assert!(move_ids.contains(&ids_b[0]));
        assert!(move_ids.contains(&ids_b[1]));
    }

    #[test]
    fn link_selected_clips_assigns_link_group_id() {
        let (mut st, track_a, _track_b, ids_a, ids_b) =
            timeline_state_with_two_video_tracks(&[("A", 0)], &[("X", 1_000_000_000)]);
        let mut selected = HashSet::new();
        selected.insert(ids_a[0].clone());
        selected.insert(ids_b[0].clone());
        st.set_selection_with_primary(ids_a[0].clone(), track_a, selected);

        assert!(st.link_selected_clips());

        let proj = st.project.borrow();
        let link_ids: HashSet<String> = proj
            .tracks
            .iter()
            .flat_map(|t| t.clips.iter())
            .filter(|c| c.id == ids_a[0] || c.id == ids_b[0])
            .filter_map(|c| c.link_group_id.clone())
            .collect();
        assert_eq!(link_ids.len(), 1);
    }

    #[test]
    fn unlink_selected_clips_clears_entire_link_group() {
        let (mut st, _track_a, track_b, ids_a, ids_b) = timeline_state_with_two_video_tracks(
            &[("A", 0)],
            &[("X", 1_000_000_000), ("Y", 2_000_000_000)],
        );
        {
            let mut proj = st.project.borrow_mut();
            let link_group_id = "l1".to_string();
            for track in &mut proj.tracks {
                for clip in &mut track.clips {
                    if clip.id == ids_a[0] || clip.id == ids_b[0] || clip.id == ids_b[1] {
                        clip.link_group_id = Some(link_group_id.clone());
                    }
                }
            }
        }
        st.set_single_clip_selection(ids_b[0].clone(), track_b);

        assert!(st.unlink_selected_clips());

        let proj = st.project.borrow();
        assert!(proj
            .tracks
            .iter()
            .flat_map(|t| t.clips.iter())
            .all(|c| c.link_group_id.is_none()));
    }

    #[test]
    fn clip_context_menu_selection_preserves_existing_selected_clip() {
        let (mut st, track_a, track_b, ids_a, ids_b) =
            timeline_state_with_two_video_tracks(&[("A", 0)], &[("X", 1_000_000_000)]);
        let mut selected = HashSet::new();
        selected.insert(ids_a[0].clone());
        selected.insert(ids_b[0].clone());
        st.set_selection_with_primary(ids_a[0].clone(), track_a, selected);

        assert!(!st.prepare_clip_context_menu_selection(&ids_b[0], &track_b));

        let selected = st.selected_ids_or_primary();
        assert_eq!(selected.len(), 2);
        assert!(selected.contains(&ids_a[0]));
        assert!(selected.contains(&ids_b[0]));
    }

    #[test]
    fn clip_context_menu_selection_replaces_unselected_clip() {
        let (mut st, track_a, track_b, ids_a, ids_b) =
            timeline_state_with_two_video_tracks(&[("A", 0)], &[("X", 1_000_000_000)]);
        let mut selected = HashSet::new();
        selected.insert(ids_a[0].clone());
        st.set_selection_with_primary(ids_a[0].clone(), track_a, selected);

        assert!(st.prepare_clip_context_menu_selection(&ids_b[0], &track_b));

        let selected = st.selected_ids_or_primary();
        assert_eq!(selected.len(), 1);
        assert!(selected.contains(&ids_b[0]));
    }

    #[test]
    fn clip_context_menu_actionability_is_empty_without_selection() {
        let (st, _track_id, _ids) = timeline_state_with_video_clips(&[("A", 0)]);
        let actionability = st.clip_context_menu_actionability();
        assert_eq!(actionability, ClipContextMenuActionability::default());
        assert!(!actionability.any());
    }

    #[test]
    fn clip_context_menu_actionability_enables_freeze_for_single_selected_video_clip() {
        let (mut st, track_id, ids) = timeline_state_with_video_clips(&[("A", 0)]);
        st.set_single_clip_selection(ids[0].clone(), track_id);
        st.playhead_ns = 500_000_000;

        let actionability = st.clip_context_menu_actionability();
        assert!(!actionability.join_through_edit);
        assert!(actionability.freeze_frame);
        assert!(!actionability.link_selected);
        assert!(!actionability.unlink_selected);
        assert!(!actionability.align_grouped);
        assert!(!actionability.sync_audio);
        assert!(actionability.remove_silent_parts);
        assert!(actionability.any());
    }

    #[test]
    fn clip_context_menu_actionability_enables_link_and_sync_for_multi_selection() {
        let (mut st, track_a, _track_b, ids_a, ids_b) =
            timeline_state_with_two_video_tracks(&[("A", 0)], &[("X", 1_000_000_000)]);
        let mut selected = HashSet::new();
        selected.insert(ids_a[0].clone());
        selected.insert(ids_b[0].clone());
        st.set_selection_with_primary(ids_a[0].clone(), track_a, selected);
        st.playhead_ns = 5_000_000_000;

        let actionability = st.clip_context_menu_actionability();
        assert!(!actionability.join_through_edit);
        assert!(!actionability.freeze_frame);
        assert!(actionability.link_selected);
        assert!(!actionability.unlink_selected);
        assert!(!actionability.align_grouped);
        assert!(actionability.sync_audio);
        assert!(!actionability.remove_silent_parts);
        assert!(actionability.any());
    }

    #[test]
    fn can_align_selected_groups_by_timecode_requires_group_time_metadata() {
        let (mut st, _track_a, track_b, _ids_a, ids_b) = timeline_state_with_two_video_tracks(
            &[("A", 0)],
            &[("X", 1_000_000_000), ("Y", 2_000_000_000)],
        );
        {
            let mut proj = st.project.borrow_mut();
            let group_id = "g1".to_string();
            if let Some(track) = proj.tracks.iter_mut().find(|t| t.id == track_b) {
                for clip in &mut track.clips {
                    if clip.id == ids_b[0] || clip.id == ids_b[1] {
                        clip.group_id = Some(group_id.clone());
                    }
                }
            }
        }

        st.set_single_clip_selection(ids_b[0].clone(), track_b);
        assert!(!st.can_align_selected_groups_by_timecode());
    }

    #[test]
    fn align_selected_groups_by_timecode_moves_entire_group() {
        let (mut st, _track_a, track_b, _ids_a, ids_b) = timeline_state_with_two_video_tracks(
            &[("A", 0)],
            &[("X", 4_000_000_000), ("Y", 9_000_000_000)],
        );
        {
            let mut proj = st.project.borrow_mut();
            let group_id = "g1".to_string();
            if let Some(track) = proj.tracks.iter_mut().find(|t| t.id == track_b) {
                for clip in &mut track.clips {
                    if clip.id == ids_b[0] {
                        clip.group_id = Some(group_id.clone());
                        clip.source_timecode_base_ns = Some(10_000_000_000);
                    } else if clip.id == ids_b[1] {
                        clip.group_id = Some(group_id.clone());
                        clip.source_timecode_base_ns = Some(12_000_000_000);
                    }
                }
            }
        }

        st.set_single_clip_selection(ids_b[0].clone(), track_b);
        assert!(st.can_align_selected_groups_by_timecode());
        assert!(st.align_selected_groups_by_timecode());

        let proj = st.project.borrow();
        let starts: HashMap<String, u64> = proj
            .tracks
            .iter()
            .flat_map(|track| track.clips.iter())
            .filter(|clip| clip.id == ids_b[0] || clip.id == ids_b[1])
            .map(|clip| (clip.id.clone(), clip.timeline_start))
            .collect();
        assert_eq!(starts.get(&ids_b[0]), Some(&4_000_000_000));
        assert_eq!(starts.get(&ids_b[1]), Some(&6_000_000_000));
    }

    #[test]
    fn create_freeze_frame_splits_selected_clip_and_inserts_silent_hold() {
        let (mut st, track_id, ids) =
            timeline_state_with_video_clips(&[("A", 0), ("B", 4_000_000_000)]);
        {
            let mut proj = st.project.borrow_mut();
            let track = proj
                .tracks
                .iter_mut()
                .find(|t| t.id == track_id)
                .expect("video track exists");
            let clip_a = track
                .clips
                .iter_mut()
                .find(|clip| clip.id == ids[0])
                .expect("clip A exists");
            clip_a.source_out = 4_000_000_000;
        }
        st.set_single_clip_selection(ids[0].clone(), track_id.clone());
        st.playhead_ns = 1_000_000_000;

        assert!(st.create_freeze_frame_from_selected_at_playhead(2_000_000_000));

        let proj = st.project.borrow();
        let track = proj
            .tracks
            .iter()
            .find(|t| t.id == track_id)
            .expect("video track exists");
        let freeze_clip = track
            .clips
            .iter()
            .find(|clip| clip.freeze_frame)
            .expect("freeze clip should be created");
        let freeze_clip_id = freeze_clip.id.clone();
        assert_eq!(freeze_clip.kind, ClipKind::Video);
        assert_eq!(freeze_clip.timeline_start, 1_000_000_000);
        assert_eq!(freeze_clip.duration(), 2_000_000_000);
        assert_eq!(
            freeze_clip.freeze_frame_hold_duration_ns,
            Some(2_000_000_000)
        );
        assert_eq!(freeze_clip.source_duration(), 1);
        assert_eq!(freeze_clip.volume, 0.0);
        assert!(freeze_clip.link_group_id.is_none());

        let clip_a_left = track
            .clips
            .iter()
            .find(|clip| clip.id == ids[0])
            .expect("left split clip should exist");
        assert_eq!(clip_a_left.timeline_start, 0);
        assert_eq!(clip_a_left.source_duration(), 1_000_000_000);

        let clip_b = track
            .clips
            .iter()
            .find(|clip| clip.id == ids[1])
            .expect("clip B should exist");
        assert_eq!(clip_b.timeline_start, 6_000_000_000);
        drop(proj);

        assert_eq!(st.selected_track_id.as_deref(), Some(track_id.as_str()));
        assert_eq!(
            st.selected_clip_id.as_deref(),
            Some(freeze_clip_id.as_str())
        );
    }

    #[test]
    fn create_freeze_frame_is_undoable() {
        let (mut st, track_id, ids) =
            timeline_state_with_video_clips(&[("A", 0), ("B", 4_000_000_000)]);
        {
            let mut proj = st.project.borrow_mut();
            let track = proj
                .tracks
                .iter_mut()
                .find(|t| t.id == track_id)
                .expect("video track exists");
            let clip_a = track
                .clips
                .iter_mut()
                .find(|clip| clip.id == ids[0])
                .expect("clip A exists");
            clip_a.source_out = 4_000_000_000;
        }
        st.set_single_clip_selection(ids[0].clone(), track_id.clone());
        st.playhead_ns = 1_000_000_000;

        let before = {
            let proj = st.project.borrow();
            proj.tracks
                .iter()
                .find(|t| t.id == track_id)
                .expect("video track exists")
                .clips
                .clone()
        };
        assert!(st.create_freeze_frame_from_selected_at_playhead(2_000_000_000));
        st.undo();
        let after = {
            let proj = st.project.borrow();
            proj.tracks
                .iter()
                .find(|t| t.id == track_id)
                .expect("video track exists")
                .clips
                .clone()
        };

        assert_eq!(after, before);
    }

    #[test]
    fn create_freeze_frame_ripples_other_tracks_and_splits_overlaps() {
        let (mut st, track_a, track_b, ids_a, ids_b) = timeline_state_with_two_video_tracks(
            &[("A", 0), ("B", 4_000_000_000)],
            &[("X", 500_000_000), ("Y", 2_000_000_000)],
        );
        {
            let mut proj = st.project.borrow_mut();
            let selected_track = proj
                .tracks
                .iter_mut()
                .find(|t| t.id == track_a)
                .expect("selected track exists");
            let clip_a = selected_track
                .clips
                .iter_mut()
                .find(|clip| clip.id == ids_a[0])
                .expect("clip A exists");
            clip_a.source_out = 4_000_000_000;
        }
        st.set_single_clip_selection(ids_a[0].clone(), track_a.clone());
        st.playhead_ns = 1_000_000_000;
        assert!(st.create_freeze_frame_from_selected_at_playhead(2_000_000_000));

        let proj = st.project.borrow();
        let selected_track = proj
            .tracks
            .iter()
            .find(|t| t.id == track_a)
            .expect("selected track exists");
        let clip_b = selected_track
            .clips
            .iter()
            .find(|clip| clip.id == ids_a[1])
            .expect("clip B should exist");
        assert_eq!(clip_b.timeline_start, 6_000_000_000);

        let other_track = proj
            .tracks
            .iter()
            .find(|t| t.id == track_b)
            .expect("other track exists");
        let x_left = other_track
            .clips
            .iter()
            .find(|clip| clip.id == ids_b[0])
            .expect("left split on other track should keep original id");
        assert_eq!(x_left.timeline_start, 500_000_000);
        assert_eq!(x_left.source_duration(), 500_000_000);

        let x_right = other_track
            .clips
            .iter()
            .find(|clip| clip.source_path.ends_with("X.mp4") && clip.id != ids_b[0])
            .expect("right split on other track should exist");
        assert_eq!(x_right.timeline_start, 3_000_000_000);
        assert_eq!(x_right.source_duration(), 500_000_000);

        let y = other_track
            .clips
            .iter()
            .find(|clip| clip.id == ids_b[1])
            .expect("clip Y should exist");
        assert_eq!(y.timeline_start, 4_000_000_000);
    }

    #[test]
    fn create_freeze_frame_multitrack_ripple_is_undoable() {
        let (mut st, track_a, _track_b, ids_a, _ids_b) = timeline_state_with_two_video_tracks(
            &[("A", 0), ("B", 4_000_000_000)],
            &[("X", 500_000_000), ("Y", 2_000_000_000)],
        );
        {
            let mut proj = st.project.borrow_mut();
            let selected_track = proj
                .tracks
                .iter_mut()
                .find(|t| t.id == track_a)
                .expect("selected track exists");
            let clip_a = selected_track
                .clips
                .iter_mut()
                .find(|clip| clip.id == ids_a[0])
                .expect("clip A exists");
            clip_a.source_out = 4_000_000_000;
        }
        st.set_single_clip_selection(ids_a[0].clone(), track_a);
        st.playhead_ns = 1_000_000_000;
        let before: Vec<(String, Vec<Clip>)> = {
            let proj = st.project.borrow();
            proj.tracks
                .iter()
                .map(|t| (t.id.clone(), t.clips.clone()))
                .collect()
        };

        assert!(st.create_freeze_frame_from_selected_at_playhead(2_000_000_000));
        st.undo();

        let after: Vec<(String, Vec<Clip>)> = {
            let proj = st.project.borrow();
            proj.tracks
                .iter()
                .map(|t| (t.id.clone(), t.clips.clone()))
                .collect()
        };
        assert_eq!(after, before);
    }

    #[test]
    fn create_freeze_frame_clears_copied_transition_metadata() {
        let (mut st, track_id, ids) =
            timeline_state_with_video_clips(&[("A", 0), ("B", 4_000_000_000)]);
        {
            let mut proj = st.project.borrow_mut();
            let track = proj
                .tracks
                .iter_mut()
                .find(|t| t.id == track_id)
                .expect("video track exists");
            let clip_a = track
                .clips
                .iter_mut()
                .find(|clip| clip.id == ids[0])
                .expect("clip A exists");
            clip_a.source_out = 4_000_000_000;
            clip_a.outgoing_transition = OutgoingTransition::new(
                "cross_dissolve",
                500_000_000,
                TransitionAlignment::EndOnCut,
            );
        }
        st.set_single_clip_selection(ids[0].clone(), track_id.clone());
        st.playhead_ns = 1_000_000_000;

        assert!(st.create_freeze_frame_from_selected_at_playhead(2_000_000_000));

        let proj = st.project.borrow();
        let track = proj
            .tracks
            .iter()
            .find(|t| t.id == track_id)
            .expect("video track exists");
        let left_clip = track
            .clips
            .iter()
            .find(|clip| clip.id == ids[0])
            .expect("left split clip should exist");
        assert!(!left_clip.outgoing_transition.is_active());

        let freeze_clip = track
            .clips
            .iter()
            .find(|clip| clip.freeze_frame)
            .expect("freeze clip should exist");
        assert!(!freeze_clip.outgoing_transition.is_active());
    }

    #[test]
    fn toggle_clip_selection_clears_removed_link_anchor() {
        let (mut st, track_a, track_b, ids_a, ids_b) = timeline_state_with_two_video_tracks(
            &[("A", 0)],
            &[("X", 1_000_000_000), ("Y", 2_000_000_000)],
        );
        {
            let mut proj = st.project.borrow_mut();
            let link_group_id = "l1".to_string();
            if let Some(track) = proj.tracks.iter_mut().find(|t| t.id == track_b) {
                for clip in &mut track.clips {
                    if clip.id == ids_b[0] || clip.id == ids_b[1] {
                        clip.link_group_id = Some(link_group_id.clone());
                    }
                }
            }
        }

        st.set_single_clip_selection(ids_b[0].clone(), track_b.clone());
        assert_eq!(
            st.selection_anchor_clip_id.as_deref(),
            Some(ids_b[0].as_str())
        );

        assert!(st.toggle_clip_selection(&ids_a[0], &track_a));
        assert!(st.toggle_clip_selection(&ids_b[0], &track_b));

        assert_eq!(st.selected_ids_or_primary().len(), 1);
        assert_eq!(st.selected_clip_id.as_deref(), Some(ids_a[0].as_str()));
        assert_eq!(
            st.selection_anchor_clip_id.as_deref(),
            Some(ids_a[0].as_str())
        );
    }

    #[test]
    fn grouped_peer_highlight_ids_marks_other_group_members() {
        let (mut st, _track_a, track_b, _ids_a, ids_b) = timeline_state_with_two_video_tracks(
            &[("A", 0)],
            &[
                ("X", 1_000_000_000),
                ("Y", 2_000_000_000),
                ("Z", 3_000_000_000),
            ],
        );
        {
            let mut proj = st.project.borrow_mut();
            let group_id = "g1".to_string();
            if let Some(track) = proj.tracks.iter_mut().find(|t| t.id == track_b) {
                for clip in &mut track.clips {
                    if clip.id == ids_b[0] || clip.id == ids_b[1] {
                        clip.group_id = Some(group_id.clone());
                    }
                }
            }
        }
        st.set_single_clip_selection(ids_b[0].clone(), track_b.clone());

        let peers = st.grouped_peer_highlight_ids();
        assert_eq!(peers.len(), 1);
        assert!(peers.contains(&ids_b[1]));
        assert!(!peers.contains(&ids_b[0]));
        assert!(!peers.contains(&ids_b[2]));
    }

    #[test]
    fn linked_peer_highlight_ids_marks_non_primary_linked_selection() {
        let (mut st, _track_a, track_b, _ids_a, ids_b) = timeline_state_with_two_video_tracks(
            &[("A", 0)],
            &[
                ("X", 1_000_000_000),
                ("Y", 2_000_000_000),
                ("Z", 3_000_000_000),
            ],
        );
        {
            let mut proj = st.project.borrow_mut();
            let link_group_id = "l1".to_string();
            if let Some(track) = proj.tracks.iter_mut().find(|t| t.id == track_b) {
                for clip in &mut track.clips {
                    if clip.id == ids_b[0] || clip.id == ids_b[1] {
                        clip.link_group_id = Some(link_group_id.clone());
                    }
                }
            }
        }
        st.set_single_clip_selection(ids_b[0].clone(), track_b.clone());

        let peers = st.linked_peer_highlight_ids();
        assert_eq!(peers.len(), 1);
        assert!(peers.contains(&ids_b[1]));
        assert!(!peers.contains(&ids_b[0]));
        assert!(!peers.contains(&ids_b[2]));
    }

    #[test]
    fn linked_peer_highlight_ids_ignores_unlinked_selected_clips() {
        let (mut st, track_a, track_b, ids_a, ids_b) = timeline_state_with_two_video_tracks(
            &[("A", 0)],
            &[("X", 1_000_000_000), ("Y", 2_000_000_000)],
        );
        {
            let mut proj = st.project.borrow_mut();
            let link_group_id = "l1".to_string();
            if let Some(track) = proj.tracks.iter_mut().find(|t| t.id == track_b) {
                for clip in &mut track.clips {
                    if clip.id == ids_b[0] || clip.id == ids_b[1] {
                        clip.link_group_id = Some(link_group_id.clone());
                    }
                }
            }
        }
        let mut selected = HashSet::new();
        selected.insert(ids_a[0].clone());
        selected.insert(ids_b[0].clone());
        st.set_selection_with_primary(ids_a[0].clone(), track_a, selected);

        let peers = st.linked_peer_highlight_ids();
        assert_eq!(peers.len(), 2);
        assert!(peers.contains(&ids_b[0]));
        assert!(peers.contains(&ids_b[1]));
        assert!(!peers.contains(&ids_a[0]));
    }

    #[test]
    fn grouped_peer_highlight_ids_excludes_selected_group_members() {
        let (mut st, track_a, track_b, ids_a, ids_b) = timeline_state_with_two_video_tracks(
            &[("A", 0)],
            &[
                ("X", 1_000_000_000),
                ("Y", 2_000_000_000),
                ("Z", 3_000_000_000),
            ],
        );
        {
            let mut proj = st.project.borrow_mut();
            let group_id = "g1".to_string();
            if let Some(track) = proj.tracks.iter_mut().find(|t| t.id == track_b) {
                for clip in &mut track.clips {
                    if clip.id == ids_b[0] || clip.id == ids_b[1] || clip.id == ids_b[2] {
                        clip.group_id = Some(group_id.clone());
                    }
                }
            }
        }
        let mut selected = HashSet::new();
        selected.insert(ids_a[0].clone());
        selected.insert(ids_b[0].clone());
        selected.insert(ids_b[1].clone());
        st.set_selection_with_primary(ids_a[0].clone(), track_a, selected);

        let peers = st.grouped_peer_highlight_ids();
        assert_eq!(peers.len(), 1);
        assert!(peers.contains(&ids_b[2]));
        assert!(!peers.contains(&ids_b[0]));
        assert!(!peers.contains(&ids_b[1]));
    }

    #[test]
    fn grouped_peer_highlight_ids_supports_multiple_selected_groups() {
        let (mut st, track_a, track_b, ids_a, ids_b) = timeline_state_with_two_video_tracks(
            &[("A", 0), ("B", 1_000_000_000)],
            &[
                ("X", 2_000_000_000),
                ("Y", 3_000_000_000),
                ("Z", 4_000_000_000),
                ("W", 5_000_000_000),
            ],
        );
        {
            let mut proj = st.project.borrow_mut();
            let g1 = "g1".to_string();
            let g2 = "g2".to_string();
            if let Some(track) = proj.tracks.iter_mut().find(|t| t.id == track_a) {
                for clip in &mut track.clips {
                    if clip.id == ids_a[0] || clip.id == ids_a[1] {
                        clip.group_id = Some(g1.clone());
                    }
                }
            }
            if let Some(track) = proj.tracks.iter_mut().find(|t| t.id == track_b) {
                for clip in &mut track.clips {
                    if clip.id == ids_b[0] || clip.id == ids_b[1] {
                        clip.group_id = Some(g2.clone());
                    }
                }
            }
        }
        let mut selected = HashSet::new();
        selected.insert(ids_a[0].clone());
        selected.insert(ids_b[0].clone());
        st.set_selection_with_primary(ids_a[0].clone(), track_a, selected);

        let peers = st.grouped_peer_highlight_ids();
        assert_eq!(peers.len(), 2);
        assert!(peers.contains(&ids_a[1]));
        assert!(peers.contains(&ids_b[1]));
        assert!(!peers.contains(&ids_a[0]));
        assert!(!peers.contains(&ids_b[0]));
        assert!(!peers.contains(&ids_b[2]));
        assert!(!peers.contains(&ids_b[3]));
    }

    #[test]
    fn select_all_clips_populates_multi_selection() {
        let (mut st, _track_id, _ids) = timeline_state_with_video_clips(&[
            ("A", 0),
            ("B", 1_000_000_000),
            ("C", 2_000_000_000),
        ]);

        let changed = st.select_all_clips();

        assert!(changed);
        assert_eq!(st.selected_ids_or_primary().len(), 3);
    }

    #[test]
    fn clamp_slide_delta_returns_zero_when_no_neighbors() {
        assert_eq!(clamp_slide_delta(5_000_000, None, None), 0);
        assert_eq!(clamp_slide_delta(-5_000_000, None, None), 0);
    }

    #[test]
    fn clamp_slide_delta_left_only_blocks_positive_extension() {
        // left bounds: source_in=0, original source_out=4s => min_delta=-3s.
        let left_bounds = Some((4_000_000, 0));
        assert_eq!(clamp_slide_delta(2_000_000, left_bounds, None), 0);
        assert_eq!(clamp_slide_delta(-9_000_000, left_bounds, None), -3_000_000);
    }

    #[test]
    fn clamp_slide_delta_right_only_blocks_negative_extension() {
        // right bounds: original source_in=1s, source_out=6s => max_delta=4s.
        let right_bounds = Some((1_000_000, 6_000_000));
        assert_eq!(clamp_slide_delta(-2_000_000, None, right_bounds), 0);
        assert_eq!(clamp_slide_delta(9_000_000, None, right_bounds), 4_000_000);
    }

    #[test]
    fn clamp_slide_delta_with_both_neighbors_honors_min_and_max() {
        // left -> min_delta = (2s + 1s - 10s) = -7s
        // right -> max_delta = (8s - 1s - 1s) = 6s
        let left_bounds = Some((10_000_000, 2_000_000));
        let right_bounds = Some((1_000_000, 8_000_000));
        assert_eq!(
            clamp_slide_delta(-12_000_000, left_bounds, right_bounds),
            -7_000_000
        );
        assert_eq!(
            clamp_slide_delta(12_000_000, left_bounds, right_bounds),
            6_000_000
        );
    }

    #[test]
    fn join_selected_through_edit_merges_with_metadata_and_undo_redo() {
        let (mut st, track_id) = timeline_state_with_through_edit_track();
        {
            let mut proj = st.project.borrow_mut();
            let track = proj
                .tracks
                .iter_mut()
                .find(|t| t.id == track_id)
                .expect("through-edit track should exist");
            for clip in &mut track.clips {
                if clip.id == "left" || clip.id == "right" {
                    clip.group_id = Some("group-1".to_string());
                    clip.link_group_id = Some("link-1".to_string());
                    clip.brightness = 0.25;
                    clip.lut_paths = vec!["/tmp/look.cube".to_string()];
                }
                if clip.id == "right" {
                    clip.outgoing_transition = OutgoingTransition::new(
                        "cross_dissolve",
                        250_000_000,
                        TransitionAlignment::EndOnCut,
                    );
                }
            }
        }
        st.set_single_clip_selection("right".to_string(), track_id.clone());
        let before = {
            let proj = st.project.borrow();
            proj.tracks
                .iter()
                .find(|t| t.id == track_id)
                .expect("through-edit track should exist")
                .clips
                .clone()
        };

        assert!(st.can_join_selected_through_edit());
        assert!(st.join_selected_through_edit());
        assert_eq!(st.history.undo_description(), Some("Join through edit"));

        {
            let proj = st.project.borrow();
            let track = proj
                .tracks
                .iter()
                .find(|t| t.id == track_id)
                .expect("through-edit track should exist");
            assert_eq!(track.clips.len(), 1);
            let merged = &track.clips[0];
            assert_eq!(merged.id, "left");
            assert_eq!(merged.source_in, 0);
            assert_eq!(merged.source_out, 2_000_000_000);
            assert_eq!(merged.timeline_start, 0);
            assert_eq!(merged.group_id.as_deref(), Some("group-1"));
            assert_eq!(merged.link_group_id.as_deref(), Some("link-1"));
            assert_eq!(merged.brightness, 0.25);
            assert_eq!(merged.lut_paths, vec!["/tmp/look.cube"]);
            assert_eq!(
                merged.outgoing_transition,
                OutgoingTransition::new(
                    "cross_dissolve",
                    250_000_000,
                    TransitionAlignment::EndOnCut,
                )
            );
        }
        assert_eq!(st.selected_clip_id.as_deref(), Some("left"));

        st.undo();
        let after_undo = {
            let proj = st.project.borrow();
            proj.tracks
                .iter()
                .find(|t| t.id == track_id)
                .expect("through-edit track should exist")
                .clips
                .clone()
        };
        assert_eq!(after_undo, before);

        st.redo();
        let after_redo = {
            let proj = st.project.borrow();
            proj.tracks
                .iter()
                .find(|t| t.id == track_id)
                .expect("through-edit track should exist")
                .clips
                .clone()
        };
        assert_eq!(after_redo.len(), 1);
        assert_eq!(after_redo[0].source_out, 2_000_000_000);
    }

    #[test]
    fn join_selected_through_edit_requires_matching_metadata() {
        let (mut st, track_id) = timeline_state_with_through_edit_track();
        {
            let mut proj = st.project.borrow_mut();
            let track = proj
                .tracks
                .iter_mut()
                .find(|t| t.id == track_id)
                .expect("through-edit track should exist");
            let right = track
                .clips
                .iter_mut()
                .find(|clip| clip.id == "right")
                .expect("right clip should exist");
            right.brightness = 0.5;
        }

        st.set_single_clip_selection("right".to_string(), track_id.clone());
        assert!(!st.can_join_selected_through_edit());
        assert!(!st.join_selected_through_edit());
        assert!(st.history.undo_description().is_none());
    }

    #[test]
    fn join_selected_through_edit_rejects_boundary_with_left_transition_metadata() {
        let (mut st, track_id) = timeline_state_with_through_edit_track();
        {
            let mut proj = st.project.borrow_mut();
            let track = proj
                .tracks
                .iter_mut()
                .find(|t| t.id == track_id)
                .expect("through-edit track should exist");
            let left = track
                .clips
                .iter_mut()
                .find(|clip| clip.id == "left")
                .expect("left clip should exist");
            left.outgoing_transition = OutgoingTransition::new(
                "cross_dissolve",
                125_000_000,
                TransitionAlignment::EndOnCut,
            );
        }

        st.set_single_clip_selection("right".to_string(), track_id);
        assert!(!st.can_join_selected_through_edit());
        assert!(!st.join_selected_through_edit());
    }

    #[test]
    fn through_edit_metadata_compatible_ignores_segment_timing_and_transition_fields() {
        let mut left = Clip::new("camera-a.mov", 1_000_000_000, 0, ClipKind::Video);
        left.id = "left".to_string();
        left.source_in = 0;
        left.source_out = 1_000_000_000;
        left.group_id = Some("group-1".to_string());
        left.link_group_id = Some("link-1".to_string());
        left.brightness = 0.25;

        let mut right = left.clone();
        right.id = "right".to_string();
        right.source_in = 1_000_000_000;
        right.source_out = 2_000_000_000;
        right.timeline_start = 1_000_000_000;
        right.outgoing_transition =
            OutgoingTransition::new("cross_dissolve", 250_000_000, TransitionAlignment::EndOnCut);

        assert!(through_edit_metadata_compatible(&left, &right));
    }

    #[test]
    fn through_edit_indicator_geometry_updates_with_zoom_scroll_and_row_position() {
        let (mut st, track_id) = timeline_state_with_through_edit_track();
        let track = {
            let proj = st.project.borrow();
            proj.tracks
                .iter()
                .find(|t| t.id == track_id)
                .expect("through-edit track should exist")
                .clone()
        };
        let track_height = track_row_height(&track);

        st.pixels_per_second = 100.0;
        st.scroll_offset = 0.0;
        let base = through_edit_indicator_geometry_for_track(
            &track,
            &st,
            RULER_HEIGHT,
            track_height,
            TRACK_LABEL_WIDTH + 400.0,
        );
        assert_eq!(base.len(), 1);
        assert!((base[0].x - (TRACK_LABEL_WIDTH + 100.0)).abs() < 0.001);
        assert_eq!(base[0].y_top, RULER_HEIGHT + 5.0);
        assert_eq!(base[0].y_bottom, RULER_HEIGHT + track_height - 5.0);

        st.pixels_per_second = 200.0;
        let zoomed = through_edit_indicator_geometry_for_track(
            &track,
            &st,
            RULER_HEIGHT,
            track_height,
            TRACK_LABEL_WIDTH + 400.0,
        );
        assert!((zoomed[0].x - (TRACK_LABEL_WIDTH + 200.0)).abs() < 0.001);

        st.scroll_offset = 75.0;
        let panned = through_edit_indicator_geometry_for_track(
            &track,
            &st,
            RULER_HEIGHT,
            track_height,
            TRACK_LABEL_WIDTH + 400.0,
        );
        assert!((panned[0].x - (TRACK_LABEL_WIDTH + 125.0)).abs() < 0.001);

        let reordered_row = through_edit_indicator_geometry_for_track(
            &track,
            &st,
            RULER_HEIGHT + track_height,
            track_height,
            TRACK_LABEL_WIDTH + 400.0,
        );
        assert_eq!(reordered_row[0].y_top, RULER_HEIGHT + track_height + 5.0);
        assert_eq!(
            reordered_row[0].y_bottom,
            RULER_HEIGHT + 2.0 * track_height - 5.0
        );
    }

    #[test]
    fn through_edit_indicator_geometry_filters_out_offscreen_boundaries() {
        let (st, track_id) = timeline_state_with_through_edit_track();
        let track = {
            let proj = st.project.borrow();
            proj.tracks
                .iter()
                .find(|t| t.id == track_id)
                .expect("through-edit track should exist")
                .clone()
        };

        let hidden_by_width = through_edit_indicator_geometry_for_track(
            &track,
            &st,
            RULER_HEIGHT,
            track_row_height(&track),
            TRACK_LABEL_WIDTH + 90.0,
        );
        assert!(hidden_by_width.is_empty());

        let mut st_scrolled = st;
        st_scrolled.scroll_offset = 250.0;
        let hidden_by_scroll = through_edit_indicator_geometry_for_track(
            &track,
            &st_scrolled,
            RULER_HEIGHT,
            track_row_height(&track),
            TRACK_LABEL_WIDTH + 500.0,
        );
        assert!(hidden_by_scroll.is_empty());
    }

    #[test]
    fn through_edit_indicator_geometry_hides_selected_boundaries() {
        let (mut st, track_id) = timeline_state_with_through_edit_track();
        st.set_single_clip_selection("left".to_string(), track_id.clone());
        let track = {
            let proj = st.project.borrow();
            proj.tracks
                .iter()
                .find(|t| t.id == track_id)
                .expect("through-edit track should exist")
                .clone()
        };

        let indicators = through_edit_indicator_geometry_for_track(
            &track,
            &st,
            RULER_HEIGHT,
            track_row_height(&track),
            TRACK_LABEL_WIDTH + 400.0,
        );
        assert!(indicators.is_empty());
    }

    #[test]
    fn through_edit_indicator_geometry_hides_metadata_incompatible_boundaries() {
        let (st, track_id) = timeline_state_with_through_edit_track();
        {
            let mut proj = st.project.borrow_mut();
            let track = proj
                .tracks
                .iter_mut()
                .find(|t| t.id == track_id)
                .expect("through-edit track should exist");
            let right = track
                .clips
                .iter_mut()
                .find(|clip| clip.id == "right")
                .expect("right clip should exist");
            right.brightness = 0.5;
        }
        let track = {
            let proj = st.project.borrow();
            proj.tracks
                .iter()
                .find(|t| t.id == track_id)
                .expect("through-edit track should exist")
                .clone()
        };

        let indicators = through_edit_indicator_geometry_for_track(
            &track,
            &st,
            RULER_HEIGHT,
            track_row_height(&track),
            TRACK_LABEL_WIDTH + 400.0,
        );
        assert!(indicators.is_empty());
    }

    #[test]
    fn clip_keyframe_marker_geometry_maps_keyframe_times_to_clip_pixels() {
        let mut clip = Clip::new("clip.mov", 2_000_000_000, 0, ClipKind::Video);
        clip.scale_keyframes = vec![
            NumericKeyframe {
                time_ns: 0,
                value: 1.0,
                interpolation: KeyframeInterpolation::Linear,
                bezier_controls: None,
            },
            NumericKeyframe {
                time_ns: 1_000_000_000,
                value: 1.5,
                interpolation: KeyframeInterpolation::Linear,
                bezier_controls: None,
            },
            NumericKeyframe {
                time_ns: 2_000_000_000,
                value: 2.0,
                interpolation: KeyframeInterpolation::Linear,
                bezier_controls: None,
            },
        ];
        clip.volume_keyframes = vec![NumericKeyframe {
            time_ns: 500_000_000,
            value: 0.8,
            interpolation: KeyframeInterpolation::Linear,
            bezier_controls: None,
        }];

        let markers = clip_keyframe_marker_geometry(&clip, 200.0, 300.0, TRACK_LABEL_WIDTH, 800.0);
        let scale_row_markers: Vec<_> =
            markers.iter().filter(|m| m.row == 0).map(|m| m.x).collect();
        let volume_row_markers: Vec<_> =
            markers.iter().filter(|m| m.row == 4).map(|m| m.x).collect();

        assert_eq!(scale_row_markers.len(), 3);
        assert!((scale_row_markers[0] - 200.0).abs() < 0.001);
        assert!((scale_row_markers[1] - 350.0).abs() < 0.001);
        assert!((scale_row_markers[2] - 500.0).abs() < 0.001);
        assert_eq!(volume_row_markers.len(), 1);
        assert!((volume_row_markers[0] - 275.0).abs() < 0.001);
    }

    #[test]
    fn clip_keyframe_marker_geometry_clamps_and_filters_offscreen_markers() {
        let mut clip = Clip::new("clip.mov", 1_000_000_000, 0, ClipKind::Video);
        clip.opacity_keyframes = vec![
            NumericKeyframe {
                time_ns: 200_000_000,
                value: 1.0,
                interpolation: KeyframeInterpolation::Linear,
                bezier_controls: None,
            },
            NumericKeyframe {
                time_ns: 2_000_000_000,
                value: 0.5,
                interpolation: KeyframeInterpolation::Linear,
                bezier_controls: None,
            },
        ];

        let markers = clip_keyframe_marker_geometry(
            &clip,
            TRACK_LABEL_WIDTH + 10.0,
            100.0,
            TRACK_LABEL_WIDTH + 50.0,
            TRACK_LABEL_WIDTH + 110.0,
        );
        assert_eq!(markers.len(), 1);
        assert_eq!(markers[0].row, 1);
        assert!((markers[0].x - (TRACK_LABEL_WIDTH + 110.0)).abs() < 0.001);
    }

    #[test]
    fn clip_phase1_keyframe_count_sums_all_supported_lanes() {
        let mut clip = Clip::new("clip.mov", 1_000_000_000, 0, ClipKind::Video);
        let mk = |time_ns| NumericKeyframe {
            time_ns,
            value: 1.0,
            interpolation: KeyframeInterpolation::Linear,
            bezier_controls: None,
        };
        clip.scale_keyframes = vec![mk(0)];
        clip.opacity_keyframes = vec![mk(0)];
        clip.position_x_keyframes = vec![mk(0)];
        clip.position_y_keyframes = vec![mk(0)];
        clip.volume_keyframes = vec![mk(0)];
        clip.pan_keyframes = vec![mk(0)];
        clip.speed_keyframes = vec![mk(0)];
        clip.rotate_keyframes = vec![mk(0)];
        clip.crop_left_keyframes = vec![mk(0)];
        clip.crop_right_keyframes = vec![mk(0)];
        clip.crop_top_keyframes = vec![mk(0)];
        clip.crop_bottom_keyframes = vec![mk(0)];

        assert_eq!(clip_phase1_keyframe_count(&clip), 12);
    }

    #[test]
    fn collect_keyframe_property_labels_at_local_time_reports_impacted_lanes() {
        let mut clip = Clip::new("clip.mov", 2_000_000_000, 0, ClipKind::Video);
        let mk = |time_ns| NumericKeyframe {
            time_ns,
            value: 1.0,
            interpolation: KeyframeInterpolation::Linear,
            bezier_controls: None,
        };
        clip.scale_keyframes = vec![mk(1_000_000_000)];
        clip.position_x_keyframes = vec![mk(1_000_000_000)];
        clip.rotate_keyframes = vec![mk(1_000_000_000)];
        clip.crop_top_keyframes = vec![mk(1_000_000_000)];
        clip.volume_keyframes = vec![mk(250_000_000)];

        let labels = collect_keyframe_property_labels_at_local_time(&clip, 1_000_000_000);
        assert_eq!(labels, vec!["Scale", "Position X", "Rotate", "Crop Top"]);
    }

    #[test]
    fn collect_keyframe_property_labels_clamps_to_clip_duration() {
        let mut clip = Clip::new("clip.mov", 1_000_000_000, 0, ClipKind::Video);
        let mk = |time_ns| NumericKeyframe {
            time_ns,
            value: 1.0,
            interpolation: KeyframeInterpolation::Linear,
            bezier_controls: None,
        };
        clip.opacity_keyframes = vec![mk(2_000_000_000)];
        clip.crop_bottom_keyframes = vec![mk(5_000_000_000)];

        let labels = collect_keyframe_property_labels_at_local_time(&clip, 1_000_000_000);
        assert_eq!(labels, vec!["Opacity", "Crop Bottom"]);
    }

    #[test]
    fn collect_keyframe_local_times_in_range_merges_and_dedups_lanes() {
        let mut clip = Clip::new("clip.mov", 2_000_000_000, 0, ClipKind::Video);
        let mk = |time_ns| NumericKeyframe {
            time_ns,
            value: 1.0,
            interpolation: KeyframeInterpolation::Linear,
            bezier_controls: None,
        };
        clip.scale_keyframes = vec![mk(100_000_000), mk(500_000_000), mk(1_200_000_000)];
        clip.opacity_keyframes = vec![mk(500_000_000), mk(900_000_000)];
        clip.pan_keyframes = vec![mk(1_900_000_000)];

        let times = collect_keyframe_local_times_in_range(&clip, 400_000_000, 1_000_000_000);
        assert_eq!(times.len(), 2);
        assert!(times.contains(&500_000_000));
        assert!(times.contains(&900_000_000));
    }

    #[test]
    fn collect_keyframe_local_times_in_range_clamps_to_duration() {
        let mut clip = Clip::new("clip.mov", 1_000_000_000, 0, ClipKind::Video);
        let mk = |time_ns| NumericKeyframe {
            time_ns,
            value: 1.0,
            interpolation: KeyframeInterpolation::Linear,
            bezier_controls: None,
        };
        clip.crop_left_keyframes = vec![mk(1_500_000_000)];
        clip.crop_right_keyframes = vec![mk(2_500_000_000)];

        let times = collect_keyframe_local_times_in_range(&clip, 900_000_000, 3_000_000_000);
        assert_eq!(times.len(), 1);
        assert!(times.contains(&1_000_000_000));
    }

    // ── delete_transcript_word_range tests ─────────────────────────────────
    //
    // These cover the load-bearing helper that the Transcript-Based Editing
    // panel and the `delete_transcript_range` MCP tool both call. The helper
    // splits a clip at clip-local 1× word boundaries, ripple-shifts any
    // downstream clips on the same track, and commits via SetTrackClipsCommand
    // so the whole edit is one undo entry.

    use crate::model::clip::{SubtitleSegment, SubtitleWord};

    /// Build a Project containing a single video track with one or more
    /// clips. Each clip starts at the given timeline position with the given
    /// 1× source duration.
    fn project_with_track_clips(specs: &[(&str, u64, u64)]) -> (Project, String, Vec<String>) {
        let mut project = Project::new("Transcript edit tests");
        // The default Project::new ships with one Video and one Audio track —
        // we use the Video track for these tests.
        let video_idx = project
            .tracks
            .iter()
            .position(|t| t.is_video())
            .expect("default project should include a video track");
        let track_id = project.tracks[video_idx].id.clone();
        // Clear any pre-existing default clips so the spec is authoritative.
        project.tracks[video_idx].clips.clear();
        let mut ids = Vec::new();
        for (id, timeline_start, source_duration) in specs {
            let mut clip = Clip::new(
                format!("{id}.mp4"),
                *source_duration,
                *timeline_start,
                ClipKind::Video,
            );
            clip.id = (*id).to_string();
            clip.source_in = 0;
            clip.source_out = *source_duration;
            project.tracks[video_idx].add_clip(clip);
            ids.push((*id).to_string());
        }
        (project, track_id, ids)
    }

    fn make_segment_with_words(words: &[(u64, u64, &str)]) -> SubtitleSegment {
        let words_vec: Vec<SubtitleWord> = words
            .iter()
            .map(|(s, e, t)| SubtitleWord {
                start_ns: *s,
                end_ns: *e,
                text: (*t).to_string(),
            })
            .collect();
        let start = words_vec.first().map(|w| w.start_ns).unwrap_or(0);
        let end = words_vec.last().map(|w| w.end_ns).unwrap_or(0);
        SubtitleSegment {
            id: format!("seg-{start}-{end}"),
            start_ns: start,
            end_ns: end,
            text: words.iter().map(|w| w.2).collect::<Vec<_>>().join(" "),
            words: words_vec,
        }
    }

    #[test]
    fn delete_transcript_middle_word_splits_clip_and_ripples_downstream() {
        // Two clips on a video track:
        //   A: timeline 0..10s, three words at 2..3, 4..5, 6..7 seconds
        //   B: timeline 10s..20s, no transcript
        // Delete the middle word (4..5s) from A. Expected:
        //   - A becomes two clips: left = 0..4s, right = 5..6s (timeline)
        //     because the right half is shifted left by the deleted span (1s)
        //   - B shifts from timeline_start=10s to 9s
        //   - One undo entry — Ctrl+Z restores both clips exactly.
        let s = 1_000_000_000_u64;
        let (project, track_id, ids) =
            project_with_track_clips(&[("A", 0, 10 * s), ("B", 10 * s, 10 * s)]);
        let project = Rc::new(RefCell::new(project));

        // Attach a transcript to clip A.
        {
            let mut proj = project.borrow_mut();
            let clip_a = proj.clip_mut(&ids[0]).expect("clip A should exist");
            clip_a.subtitle_segments = vec![make_segment_with_words(&[
                (2 * s, 3 * s, "alpha"),
                (4 * s, 5 * s, "beta"),
                (6 * s, 7 * s, "gamma"),
            ])];
        }

        let mut state = TimelineState::new(project.clone());
        let changed = state.delete_transcript_word_range(&ids[0], 4 * s, 5 * s);
        assert!(changed, "delete_transcript_word_range should report change");

        // After the edit:
        let proj = project.borrow();
        let track = proj
            .tracks
            .iter()
            .find(|t| t.id == track_id)
            .expect("track");
        assert_eq!(track.clips.len(), 3, "A split into 2, plus B");

        // Left half of A:
        let left = track
            .clips
            .iter()
            .find(|c| c.id == ids[0])
            .expect("left half retains original id");
        assert_eq!(left.timeline_start, 0);
        assert_eq!(left.source_in, 0);
        assert_eq!(left.source_out, 4 * s);
        // Left half subtitles must contain only the first word (rebased to 0
        // is a no-op since left starts at 0 already).
        assert_eq!(left.subtitle_segments.len(), 1);
        assert_eq!(left.subtitle_segments[0].words.len(), 1);
        assert_eq!(left.subtitle_segments[0].words[0].text, "alpha");

        // Right half of A: the only other video clip whose source_in == 5s.
        let right = track
            .clips
            .iter()
            .find(|c| c.id != ids[0] && c.id != ids[1])
            .expect("right half clip");
        assert_eq!(right.source_in, 5 * s);
        assert_eq!(right.source_out, 10 * s);
        // Right half should sit immediately after left (timeline_start = 4s).
        assert_eq!(right.timeline_start, 4 * s);
        // Right half subtitles must contain only the third word, REBASED to
        // local time 0 = source_in. Word "gamma" was at 6..7s in the original
        // clip-local 1× space, so after a 5s left rebase it should be at
        // 1s..2s.
        assert_eq!(right.subtitle_segments.len(), 1);
        assert_eq!(right.subtitle_segments[0].words.len(), 1);
        assert_eq!(right.subtitle_segments[0].words[0].text, "gamma");
        assert_eq!(right.subtitle_segments[0].words[0].start_ns, 1 * s);
        assert_eq!(right.subtitle_segments[0].words[0].end_ns, 2 * s);

        // Downstream clip B: shifted left by the deleted span (1s).
        let b = track.clips.iter().find(|c| c.id == ids[1]).expect("clip B");
        assert_eq!(b.timeline_start, 9 * s);
        assert_eq!(b.duration(), 10 * s);
    }

    #[test]
    fn delete_transcript_word_undo_restores_state() {
        let s = 1_000_000_000_u64;
        let (project, _track_id, ids) =
            project_with_track_clips(&[("A", 0, 10 * s), ("B", 10 * s, 10 * s)]);
        let project = Rc::new(RefCell::new(project));
        {
            let mut proj = project.borrow_mut();
            proj.clip_mut(&ids[0]).unwrap().subtitle_segments = vec![make_segment_with_words(&[
                (2 * s, 3 * s, "one"),
                (4 * s, 5 * s, "two"),
                (6 * s, 7 * s, "three"),
            ])];
        }
        // Snapshot expected post-undo state.
        let snapshot = project.borrow().clone();

        let mut state = TimelineState::new(project.clone());
        assert!(state.delete_transcript_word_range(&ids[0], 4 * s, 5 * s));
        // Undo should restore the original 2-clip layout.
        state.history.undo(&mut project.borrow_mut());
        let restored = project.borrow();
        assert_eq!(restored.tracks.len(), snapshot.tracks.len());
        for (got, want) in restored.tracks.iter().zip(snapshot.tracks.iter()) {
            assert_eq!(got.clips.len(), want.clips.len(), "track clip count");
            for (gc, wc) in got.clips.iter().zip(want.clips.iter()) {
                assert_eq!(gc.id, wc.id);
                assert_eq!(gc.timeline_start, wc.timeline_start);
                assert_eq!(gc.source_in, wc.source_in);
                assert_eq!(gc.source_out, wc.source_out);
                assert_eq!(gc.subtitle_segments.len(), wc.subtitle_segments.len());
            }
        }
    }

    #[test]
    fn delete_transcript_word_respects_clip_speed() {
        // Clip with speed=2.0: 10s of source content plays back in 5s of
        // timeline. Deleting word at clip-local 4..6s removes 2s of source,
        // which is 1s of timeline. Downstream clip should shift by 1s.
        let s = 1_000_000_000_u64;
        let (project, track_id, ids) =
            project_with_track_clips(&[("A", 0, 10 * s), ("B", 5 * s, 4 * s)]);
        let project = Rc::new(RefCell::new(project));
        {
            let mut proj = project.borrow_mut();
            let clip_a = proj.clip_mut(&ids[0]).unwrap();
            clip_a.speed = 2.0;
            clip_a.subtitle_segments = vec![make_segment_with_words(&[
                (1 * s, 2 * s, "first"),
                (4 * s, 6 * s, "middle"),
                (8 * s, 9 * s, "last"),
            ])];
        }
        let mut state = TimelineState::new(project.clone());
        assert!(state.delete_transcript_word_range(&ids[0], 4 * s, 6 * s));

        let proj = project.borrow();
        let track = proj.tracks.iter().find(|t| t.id == track_id).unwrap();
        // Downstream clip B should have shifted left by 1s (the deleted
        // span in timeline time = 2s clip-local / 2.0 speed).
        let b = track.clips.iter().find(|c| c.id == ids[1]).unwrap();
        assert_eq!(b.timeline_start, 4 * s);
    }

    #[test]
    fn delete_transcript_word_inside_compound_clip() {
        // A compound clip on a root video track contains an inner video
        // track with a single clip. We delete a word from the inner clip
        // and verify the inner track was mutated and the outer compound
        // remained intact.
        use crate::model::track::Track as TrackModel;
        let s = 1_000_000_000_u64;
        let mut project = Project::new("Compound transcript test");
        let outer_track_idx = project
            .tracks
            .iter()
            .position(|t| t.is_video())
            .expect("default video track");
        let outer_track_id = project.tracks[outer_track_idx].id.clone();
        // Inner clip lives inside the compound's compound_tracks.
        let mut inner_clip = Clip::new("inner.mp4", 10 * s, 0, ClipKind::Video);
        inner_clip.id = "INNER".to_string();
        inner_clip.source_in = 0;
        inner_clip.source_out = 10 * s;
        inner_clip.subtitle_segments = vec![make_segment_with_words(&[
            (2 * s, 3 * s, "left"),
            (4 * s, 5 * s, "mid"),
            (6 * s, 7 * s, "right"),
        ])];
        let mut inner_track = TrackModel::new_video("Inner V1");
        let inner_track_id = inner_track.id.clone();
        inner_track.clips.push(inner_clip);

        let mut compound = Clip::new("compound", 10 * s, 0, ClipKind::Compound);
        compound.id = "COMPOUND".to_string();
        compound.source_in = 0;
        compound.source_out = 10 * s;
        compound.compound_tracks = Some(vec![inner_track]);
        project.tracks[outer_track_idx].clips.clear();
        project.tracks[outer_track_idx].add_clip(compound);
        let project = Rc::new(RefCell::new(project));

        let mut state = TimelineState::new(project.clone());
        assert!(state.delete_transcript_word_range("INNER", 4 * s, 5 * s));

        let proj = project.borrow();
        // Outer track unchanged: still one compound clip.
        let outer = proj.tracks.iter().find(|t| t.id == outer_track_id).unwrap();
        assert_eq!(outer.clips.len(), 1);
        assert_eq!(outer.clips[0].id, "COMPOUND");
        // Inner track was mutated: now two clips (left + right halves).
        let compound = &outer.clips[0];
        let inner_tracks = compound.compound_tracks.as_ref().unwrap();
        let inner_track_after = inner_tracks
            .iter()
            .find(|t| t.id == inner_track_id)
            .unwrap();
        assert_eq!(
            inner_track_after.clips.len(),
            2,
            "inner clip should have been split"
        );
    }
}
