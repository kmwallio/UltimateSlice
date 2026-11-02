//! Background rendering: automatic Render-and-Replace for clips whose
//! effect stack the live preview cannot keep up with.
//!
//! Manual Render-and-Replace (`Clip::render_replace_enabled`) is the
//! user's explicit choice and is honoured by preview *and* export. This
//! module adds a session-level, preview-only layer on top of it:
//!
//! - [`BackgroundRenderMonitor`] watches the Program Monitor's frame-drop
//!   counter while playing and attributes sustained pressure to the
//!   bakeable clips under the playhead. When a clip keeps dropping frames
//!   it is *promoted* into the auto set.
//! - [`BackgroundRenderState`] holds the auto set (clip ids whose effect
//!   stack is baked in the background and swapped into the preview once
//!   the sidecar is ready), the ids the user opted out of this session,
//!   and the monitor.
//! - `apply_auto_flags` turns the auto set into the `render_replace_enabled`
//!   flag on a *clone* of the editing tracks right before they are
//!   flattened into `ProgramClip`s, so the player's existing sidecar swap +
//!   effect-neutralise path does all the work and the project model is
//!   never dirtied. Export reads the real project and therefore keeps
//!   running the live filter chain for auto-baked clips.
//!
//! The policy comes from `PreferencesState::background_render_mode`
//! (`BackgroundRenderMode::{Off, WhenDropping, Always}`).

use std::collections::{HashMap, HashSet};

use crate::media::render_replace_cache::{
    clip_effect_stack_is_complex, clip_has_bakeable_effects, is_bakeable_kind,
};
use crate::model::clip::Clip;
use crate::model::track::Track;
use crate::ui_state::BackgroundRenderMode;

/// Ticks (500 ms each) ignored after play starts or the clip set under the
/// playhead changes. Boundary staging and the first second of playback drop
/// frames for reasons that have nothing to do with the effect stack.
pub const SETTLE_TICKS: u8 = 2;
/// Minimum consecutive pressure ticks before a promotion can happen.
pub const MIN_PRESSURE_TICKS: u32 = 2;
/// Promote when at least this many frames were dropped while the clip was
/// under pressure (and `MIN_PRESSURE_TICKS` is met)…
pub const PROMOTE_DROPPED_FRAMES: u64 = 12;
/// …or when pressure lasted this many consecutive ticks regardless of the
/// QoS count (the compositor may be starved without any element reporting
/// a drop, which shows up as a low effective frame rate instead).
pub const PROMOTE_PRESSURE_TICKS: u32 = 4;
/// Effective fps below this fraction of the project rate counts as
/// pressure even with no QoS drops reported.
pub const PRESSURE_FPS_RATIO: f64 = 0.8;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PressureScore {
    /// Dropped frames attributed to the clip while it was under pressure.
    pub dropped: u64,
    /// Consecutive pressure ticks.
    pub ticks: u32,
}

/// One 500 ms observation from the window poll loop.
#[derive(Clone, Debug)]
pub struct MonitorSample<'a> {
    pub playing: bool,
    /// Cumulative QoS dropped-frame count from the Program Monitor.
    pub dropped_total: u64,
    /// Rolling compositor output fps (0 when not playing).
    pub effective_fps: f64,
    pub project_fps: f64,
    /// Ids of the eligible clips under the playhead right now.
    pub candidates: &'a [String],
}

/// Drop-pressure state machine. Pure logic, no GTK, unit-tested.
#[derive(Debug, Default)]
pub struct BackgroundRenderMonitor {
    last_dropped_total: Option<u64>,
    was_playing: bool,
    settle_ticks: u8,
    prev_candidates: Vec<String>,
    scores: HashMap<String, PressureScore>,
}

impl BackgroundRenderMonitor {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn reset(&mut self) {
        *self = Self::default();
    }

    pub fn scores(&self) -> &HashMap<String, PressureScore> {
        &self.scores
    }

    /// Feed one observation. Returns the clip ids promoted on this tick.
    pub fn tick(&mut self, sample: MonitorSample<'_>) -> Vec<String> {
        if !sample.playing {
            self.was_playing = false;
            self.last_dropped_total = None;
            self.prev_candidates.clear();
            self.decay_all();
            return Vec::new();
        }
        if !self.was_playing {
            self.was_playing = true;
            self.settle_ticks = SETTLE_TICKS;
            self.last_dropped_total = Some(sample.dropped_total);
            self.prev_candidates = sample.candidates.to_vec();
            return Vec::new();
        }
        if self.prev_candidates != sample.candidates {
            // A cut: the entering clip's first frames are staged/seeked and
            // any drops here belong to the boundary, not the stack.
            self.prev_candidates = sample.candidates.to_vec();
            self.settle_ticks = SETTLE_TICKS;
            self.last_dropped_total = Some(sample.dropped_total);
            return Vec::new();
        }
        if self.settle_ticks > 0 {
            self.settle_ticks -= 1;
            self.last_dropped_total = Some(sample.dropped_total);
            return Vec::new();
        }
        let delta = sample
            .dropped_total
            .saturating_sub(self.last_dropped_total.unwrap_or(sample.dropped_total));
        self.last_dropped_total = Some(sample.dropped_total);
        let fps_pressure = sample.project_fps > 0.0
            && sample.effective_fps > 0.5
            && sample.effective_fps < PRESSURE_FPS_RATIO * sample.project_fps;
        let pressure = delta > 0 || fps_pressure;

        let candidate_set: HashSet<&String> = sample.candidates.iter().collect();
        // Scores of clips no longer under the playhead fade out.
        for (id, score) in self.scores.iter_mut() {
            if !candidate_set.contains(id) {
                score.ticks = 0;
                score.dropped /= 2;
            }
        }
        self.scores.retain(|_, s| s.dropped > 0 || s.ticks > 0);

        let mut promoted = Vec::new();
        for id in sample.candidates {
            let score = self.scores.entry(id.clone()).or_default();
            if pressure {
                score.ticks += 1;
                score.dropped += delta;
            } else {
                score.ticks = 0;
                score.dropped /= 2;
            }
            let promote = score.ticks >= MIN_PRESSURE_TICKS
                && (score.dropped >= PROMOTE_DROPPED_FRAMES
                    || score.ticks >= PROMOTE_PRESSURE_TICKS);
            if promote {
                promoted.push(id.clone());
            }
        }
        for id in &promoted {
            self.scores.remove(id);
        }
        promoted
    }

    fn decay_all(&mut self) {
        for score in self.scores.values_mut() {
            score.ticks = 0;
            score.dropped /= 2;
        }
        self.scores.retain(|_, s| s.dropped > 0);
    }
}

/// Session-level background-rendering state shared between the window
/// poll loop, the reload path, the inspector and the MCP handler.
#[derive(Debug, Default)]
pub struct BackgroundRenderState {
    /// Clips whose effect stack is baked automatically (preview only).
    pub auto_ids: HashSet<String>,
    /// Clips the user opted out of this session (Cancel / "use live
    /// effects" in the inspector, or MCP). Never re-promoted until the
    /// app restarts or the mode is switched Off and back on.
    pub suppressed: HashSet<String>,
    pub monitor: BackgroundRenderMonitor,
    /// Promotions since the app started (telemetry).
    pub promoted_total: usize,
}

impl BackgroundRenderState {
    pub fn is_auto(&self, clip_id: &str) -> bool {
        self.auto_ids.contains(clip_id)
    }

    /// Promote `clip_id` into the auto set. Returns false when it was
    /// already there or is suppressed.
    pub fn promote(&mut self, clip_id: &str) -> bool {
        if self.suppressed.contains(clip_id) {
            return false;
        }
        if self.auto_ids.insert(clip_id.to_string()) {
            self.promoted_total += 1;
            true
        } else {
            false
        }
    }

    /// Opt a clip out for the rest of the session. Returns true when it
    /// was in the auto set.
    pub fn suppress(&mut self, clip_id: &str) -> bool {
        self.suppressed.insert(clip_id.to_string());
        self.auto_ids.remove(clip_id)
    }

    pub fn clear(&mut self) {
        self.auto_ids.clear();
        self.monitor.reset();
    }
}

/// Whether `clip` may be auto-baked at all: a bakeable kind with a
/// non-trivial baked scope that the user has not already toggled manually.
pub fn clip_is_auto_eligible(clip: &Clip) -> bool {
    !clip.render_replace_enabled && is_bakeable_kind(&clip.kind) && clip_has_bakeable_effects(clip)
}

/// Ids of the eligible clips covering `pos_ns` on `tracks` (the editing
/// tracks: root, or a compound's internals when drilled in). Compounds
/// are candidates as a whole; their internals are not inspected here.
pub fn candidates_at(tracks: &[Track], pos_ns: u64, state: &BackgroundRenderState) -> Vec<String> {
    let mut out = Vec::new();
    for track in tracks {
        for clip in &track.clips {
            if pos_ns < clip.timeline_start || pos_ns >= clip.timeline_end() {
                continue;
            }
            if state.auto_ids.contains(&clip.id) || state.suppressed.contains(&clip.id) {
                continue;
            }
            if clip_is_auto_eligible(clip) {
                out.push(clip.id.clone());
            }
        }
    }
    out.sort();
    out
}

/// Clone `tracks` with `render_replace_enabled` set on every clip (at any
/// nesting depth) whose id is in `ids`. Returns the clone; the project
/// model is untouched.
pub fn apply_auto_flags(tracks: &[Track], ids: &HashSet<String>) -> Vec<Track> {
    fn walk(tracks: &mut [Track], ids: &HashSet<String>, depth: usize) {
        if depth > 16 {
            return;
        }
        for track in tracks {
            for clip in &mut track.clips {
                if ids.contains(&clip.id) {
                    clip.render_replace_enabled = true;
                }
                if let Some(inner) = clip.compound_tracks.as_mut() {
                    walk(inner, ids, depth + 1);
                }
            }
        }
    }
    let mut cloned = tracks.to_vec();
    walk(&mut cloned, ids, 0);
    cloned
}

fn find_clip<'a>(tracks: &'a [Track], id: &str, depth: usize) -> Option<&'a Clip> {
    if depth > 16 {
        return None;
    }
    for track in tracks {
        for clip in &track.clips {
            if clip.id == id {
                return Some(clip);
            }
            if let Some(inner) = clip.compound_tracks.as_ref() {
                if let Some(found) = find_clip(inner, id, depth + 1) {
                    return Some(found);
                }
            }
        }
    }
    None
}

/// Bring the auto set in line with the current project and policy. Runs
/// on every reload (cheap: one lookup per auto id, one pass over the root
/// clips in `Always` mode).
///
/// - `Off`: everything is dropped and the monitor reset.
/// - Ids whose clip is gone, was toggled manually, or no longer has a
///   baked scope are dropped.
/// - `Always`: every root clip with a complex stack joins the set unless
///   the user opted it out.
///
/// Returns the ids added by this call (`Always` promotions).
pub fn reconcile(
    state: &mut BackgroundRenderState,
    root_tracks: &[Track],
    mode: BackgroundRenderMode,
) -> Vec<String> {
    if mode == BackgroundRenderMode::Off {
        state.clear();
        return Vec::new();
    }
    let stale: Vec<String> = state
        .auto_ids
        .iter()
        .filter(|id| {
            find_clip(root_tracks, id, 0).map_or(true, |clip| !clip_is_auto_eligible(clip))
        })
        .cloned()
        .collect();
    for id in &stale {
        state.auto_ids.remove(id);
    }
    let mut added = Vec::new();
    if mode == BackgroundRenderMode::Always {
        for track in root_tracks {
            for clip in &track.clips {
                if state.auto_ids.contains(&clip.id) || state.suppressed.contains(&clip.id) {
                    continue;
                }
                if clip_is_auto_eligible(clip) && clip_effect_stack_is_complex(clip) {
                    if state.promote(&clip.id) {
                        added.push(clip.id.clone());
                    }
                }
            }
        }
    }
    added
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::clip::ClipKind;

    fn video(label: &str, start: u64, dur: u64) -> Clip {
        let mut c = Clip::new(format!("/tmp/{label}.mp4"), dur, start, ClipKind::Video);
        c.label = label.to_string();
        c
    }

    fn sample<'a>(
        playing: bool,
        dropped: u64,
        fps: f64,
        candidates: &'a [String],
    ) -> MonitorSample<'a> {
        MonitorSample {
            playing,
            dropped_total: dropped,
            effective_fps: fps,
            project_fps: 30.0,
            candidates,
        }
    }

    #[test]
    fn monitor_ignores_drops_during_settle_and_boundaries() {
        let mut m = BackgroundRenderMonitor::new();
        let a = vec!["a".to_string()];
        assert!(m.tick(sample(true, 0, 30.0, &a)).is_empty()); // play start
        assert!(m.tick(sample(true, 50, 30.0, &a)).is_empty()); // settle 1
        assert!(m.tick(sample(true, 100, 30.0, &a)).is_empty()); // settle 2
                                                                 // Boundary: new candidate set resets settle.
        let b = vec!["b".to_string()];
        assert!(m.tick(sample(true, 200, 30.0, &b)).is_empty());
        assert!(m.tick(sample(true, 300, 30.0, &b)).is_empty());
        assert!(m.tick(sample(true, 400, 30.0, &b)).is_empty());
        assert!(m.scores().get("b").is_none() || m.scores()["b"].dropped == 0);
    }

    #[test]
    fn monitor_promotes_after_sustained_drops() {
        let mut m = BackgroundRenderMonitor::new();
        let a = vec!["a".to_string()];
        for _ in 0..3 {
            assert!(m.tick(sample(true, 0, 30.0, &a)).is_empty());
        }
        // Two pressure ticks with 8 drops each: 16 ≥ 12 and ticks ≥ 2.
        assert!(m.tick(sample(true, 8, 30.0, &a)).is_empty());
        let promoted = m.tick(sample(true, 16, 30.0, &a));
        assert_eq!(promoted, vec!["a".to_string()]);
        assert!(m.scores().get("a").is_none());
    }

    #[test]
    fn monitor_single_hiccup_does_not_promote() {
        let mut m = BackgroundRenderMonitor::new();
        let a = vec!["a".to_string()];
        for _ in 0..3 {
            m.tick(sample(true, 0, 30.0, &a));
        }
        assert!(m.tick(sample(true, 40, 30.0, &a)).is_empty()); // one big hiccup
        assert!(m.tick(sample(true, 40, 30.0, &a)).is_empty()); // clean tick halves it
        assert_eq!(m.scores()["a"].ticks, 0);
        assert_eq!(m.scores()["a"].dropped, 20);
    }

    #[test]
    fn monitor_low_fps_without_qos_drops_promotes_after_four_ticks() {
        let mut m = BackgroundRenderMonitor::new();
        let a = vec!["a".to_string()];
        for _ in 0..3 {
            m.tick(sample(true, 0, 30.0, &a));
        }
        for _ in 0..3 {
            assert!(m.tick(sample(true, 0, 15.0, &a)).is_empty());
        }
        assert_eq!(m.tick(sample(true, 0, 15.0, &a)), vec!["a".to_string()]);
    }

    #[test]
    fn monitor_stop_resets_play_state() {
        let mut m = BackgroundRenderMonitor::new();
        let a = vec!["a".to_string()];
        for _ in 0..3 {
            m.tick(sample(true, 0, 30.0, &a));
        }
        m.tick(sample(true, 8, 30.0, &a));
        assert!(m.tick(sample(false, 8, 0.0, &a)).is_empty());
        // Playing again: settle applies before drops count.
        assert!(m.tick(sample(true, 8, 30.0, &a)).is_empty());
        assert!(m.tick(sample(true, 100, 30.0, &a)).is_empty());
    }

    #[test]
    fn candidates_require_effects_and_cover_playhead() {
        let mut graded = video("graded", 0, 2_000_000_000);
        graded.denoise = 0.5;
        let plain = video("plain", 0, 2_000_000_000);
        let mut later = video("later", 5_000_000_000, 2_000_000_000);
        later.blur = 0.5;
        let mut manual = video("manual", 0, 2_000_000_000);
        manual.blur = 0.5;
        manual.render_replace_enabled = true;
        let mut track = Track::new_video("V1");
        track.clips = vec![graded.clone(), plain, later, manual];
        let state = BackgroundRenderState::default();
        let ids = candidates_at(&[track.clone()], 1_000_000_000, &state);
        assert_eq!(ids, vec![graded.id.clone()]);

        let mut suppressed = BackgroundRenderState::default();
        suppressed.suppress(&graded.id);
        assert!(candidates_at(&[track], 1_000_000_000, &suppressed).is_empty());
    }

    #[test]
    fn apply_auto_flags_sets_flag_on_clone_only() {
        let mut inner = video("inner", 0, 1_000_000_000);
        inner.blur = 0.3;
        let mut itrack = Track::new_video("inner");
        itrack.clips.push(inner.clone());
        let mut compound = Clip::new("", 1_000_000_000, 0, ClipKind::Compound);
        compound.compound_tracks = Some(vec![itrack]);
        let mut track = Track::new_video("V1");
        track.clips.push(compound);
        let ids: HashSet<String> = [inner.id.clone()].into_iter().collect();
        let cloned = apply_auto_flags(&[track.clone()], &ids);
        let flagged =
            cloned[0].clips[0].compound_tracks.as_ref().unwrap()[0].clips[0].render_replace_enabled;
        assert!(flagged);
        let original =
            track.clips[0].compound_tracks.as_ref().unwrap()[0].clips[0].render_replace_enabled;
        assert!(!original);
    }

    #[test]
    fn reconcile_off_clears_and_always_promotes_complex_only() {
        let mut complex = video("complex", 0, 1_000_000_000);
        complex.blur = 0.5;
        let mut cheap = video("cheap", 0, 1_000_000_000);
        cheap.brightness = 0.2;
        let mut track = Track::new_video("V1");
        track.clips = vec![complex.clone(), cheap.clone()];
        let tracks = vec![track];

        let mut state = BackgroundRenderState::default();
        let added = reconcile(&mut state, &tracks, BackgroundRenderMode::Always);
        assert_eq!(added, vec![complex.id.clone()]);
        assert!(state.is_auto(&complex.id));
        assert!(!state.is_auto(&cheap.id));
        assert_eq!(state.promoted_total, 1);
        // Idempotent.
        assert!(reconcile(&mut state, &tracks, BackgroundRenderMode::Always).is_empty());

        // Suppressed clips are never re-added.
        state.suppress(&complex.id);
        assert!(reconcile(&mut state, &tracks, BackgroundRenderMode::Always).is_empty());
        assert!(!state.is_auto(&complex.id));

        state.promote("ghost");
        assert!(state.is_auto("ghost"));
        reconcile(&mut state, &tracks, BackgroundRenderMode::WhenDropping);
        assert!(!state.is_auto("ghost"), "ids with no clip are pruned");

        state.promote(&cheap.id);
        reconcile(&mut state, &tracks, BackgroundRenderMode::Off);
        assert!(state.auto_ids.is_empty());
    }

    #[test]
    fn reconcile_drops_clips_toggled_manually() {
        let mut c = video("c", 0, 1_000_000_000);
        c.blur = 0.4;
        let mut track = Track::new_video("V1");
        track.clips.push(c.clone());
        let mut state = BackgroundRenderState::default();
        state.promote(&c.id);
        track.clips[0].render_replace_enabled = true;
        reconcile(&mut state, &[track], BackgroundRenderMode::WhenDropping);
        assert!(!state.is_auto(&c.id));
    }
}
