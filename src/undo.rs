use crate::model::clip::{AuditionTake, Clip, VoiceIsolationSource};
use crate::model::project::Project;
use crate::model::track::{AudioRole, Track};
use crate::model::transition::OutgoingTransition;

/// A reversible edit operation on the project.
pub trait EditCommand {
    fn execute(&self, project: &mut Project);
    fn undo(&self, project: &mut Project);
    #[allow(dead_code)]
    fn description(&self) -> &str;
}

// -----------------------------------------------------------------------
// Generic clip-property and track-property mutation wrappers (P1.4)
// -----------------------------------------------------------------------
//
// Many EditCommand impls in this file share the same skeleton:
//
//   fn execute(&self, project: &mut Project) {
//       if let Some(clip) = project.clip_mut(&self.clip_id) {
//           clip.field = self.new_value.clone();
//       }
//       project.dirty = true;
//   }
//
// The two helpers below collapse that pattern into a single generic struct.
// New property setters can use `ClipMutateCommand` instead of adding a
// 30-line per-property struct + impl. The `apply` function pointer is
// called for both execute (with `new_state`) and undo (with `old_state`),
// so the setter logic only has to be written once.
//
// For track-level properties (mute, solo, duck) use `TrackMutateCommand`.

/// Generic "set one clip property" undo command. Replace individual
/// per-property command structs with an instantiation of this. The `apply`
/// function receives a `&mut Clip` and the state value to apply.
pub struct ClipMutateCommand<T: Clone> {
    pub clip_id: String,
    pub old_state: T,
    pub new_state: T,
    pub apply: fn(&mut Clip, T),
    pub label: &'static str,
}

impl<T: Clone + 'static> EditCommand for ClipMutateCommand<T> {
    fn execute(&self, project: &mut Project) {
        if let Some(clip) = project.clip_mut(&self.clip_id) {
            (self.apply)(clip, self.new_state.clone());
        }
        project.dirty = true;
    }
    fn undo(&self, project: &mut Project) {
        if let Some(clip) = project.clip_mut(&self.clip_id) {
            (self.apply)(clip, self.old_state.clone());
        }
        project.dirty = true;
    }
    fn description(&self) -> &str {
        self.label
    }
}

/// Generic "set one track property" undo command.
pub struct TrackMutateCommand<T: Clone> {
    pub track_id: String,
    pub old_state: T,
    pub new_state: T,
    pub apply: fn(&mut crate::model::track::Track, T),
    pub label: &'static str,
}

impl<T: Clone + 'static> EditCommand for TrackMutateCommand<T> {
    fn execute(&self, project: &mut Project) {
        if let Some(track) = project.track_mut(&self.track_id) {
            (self.apply)(track, self.new_state.clone());
        }
        project.dirty = true;
    }
    fn undo(&self, project: &mut Project) {
        if let Some(track) = project.track_mut(&self.track_id) {
            (self.apply)(track, self.old_state.clone());
        }
        project.dirty = true;
    }
    fn description(&self) -> &str {
        self.label
    }
}

/// Move a clip to a new track / timeline position
pub struct MoveClipCommand {
    pub clip_id: String,
    pub from_track_id: String,
    pub to_track_id: String,
    pub old_timeline_start: u64,
    pub new_timeline_start: u64,
}

impl EditCommand for MoveClipCommand {
    fn execute(&self, project: &mut Project) {
        move_clip(
            project,
            &self.clip_id,
            &self.from_track_id,
            &self.to_track_id,
            self.new_timeline_start,
        );
    }
    fn undo(&self, project: &mut Project) {
        move_clip(
            project,
            &self.clip_id,
            &self.to_track_id,
            &self.from_track_id,
            self.old_timeline_start,
        );
    }
    fn description(&self) -> &str {
        "Move clip"
    }
}

/// Trim the in-point of a clip
pub struct TrimClipCommand {
    pub clip_id: String,
    pub track_id: String,
    pub old_source_in: u64,
    pub new_source_in: u64,
    pub old_timeline_start: u64,
    pub new_timeline_start: u64,
}

impl EditCommand for TrimClipCommand {
    fn execute(&self, project: &mut Project) {
        if let Some(track) = project.track_mut(&self.track_id) {
            if let Some(clip) = track.clips.iter_mut().find(|c| c.id == self.clip_id) {
                clip.source_in = self.new_source_in;
                clip.timeline_start = self.new_timeline_start;
            }
        }
        project.dirty = true;
    }
    fn undo(&self, project: &mut Project) {
        if let Some(track) = project.track_mut(&self.track_id) {
            if let Some(clip) = track.clips.iter_mut().find(|c| c.id == self.clip_id) {
                clip.source_in = self.old_source_in;
                clip.timeline_start = self.old_timeline_start;
            }
        }
        project.dirty = true;
    }
    fn description(&self) -> &str {
        "Trim clip"
    }
}

/// Trim the out-point of a clip
pub struct TrimOutCommand {
    pub clip_id: String,
    pub track_id: String,
    pub old_source_out: u64,
    pub new_source_out: u64,
}

impl EditCommand for TrimOutCommand {
    fn execute(&self, project: &mut Project) {
        if let Some(track) = project.track_mut(&self.track_id) {
            if let Some(clip) = track.clips.iter_mut().find(|c| c.id == self.clip_id) {
                clip.source_out = self.new_source_out;
            }
        }
        project.dirty = true;
    }
    fn undo(&self, project: &mut Project) {
        if let Some(track) = project.track_mut(&self.track_id) {
            if let Some(clip) = track.clips.iter_mut().find(|c| c.id == self.clip_id) {
                clip.source_out = self.old_source_out;
            }
        }
        project.dirty = true;
    }
    fn description(&self) -> &str {
        "Trim clip out-point"
    }
}

/// Ripple trim the out-point of a clip (shifting subsequent clips)
pub struct RippleTrimOutCommand {
    pub clip_id: String,
    pub track_id: String,
    pub old_source_out: u64,
    pub new_source_out: u64,
    /// The delta applied to subsequent clips (can be positive or negative)
    pub delta: i64,
}

impl EditCommand for RippleTrimOutCommand {
    fn execute(&self, project: &mut Project) {
        if let Some(track) = project.track_mut(&self.track_id) {
            let mut original_end = None;
            // 1. Find the clip, get its ORIGINAL end (before modification), then apply change
            if let Some(clip) = track.clips.iter_mut().find(|c| c.id == self.clip_id) {
                original_end = Some(clip.timeline_end());
                clip.source_out = self.new_source_out;
            }

            // 2. Shift subsequent clips based on ORIGINAL end threshold
            if let Some(threshold) = original_end {
                for clip in &mut track.clips {
                    // Skip the clip itself (by ID) if needed, but here we filter by position.
                    // The clip itself starts BEFORE the threshold (obviously).
                    // Subsequent clips start >= threshold.
                    if clip.timeline_start >= threshold {
                        let new_start = (clip.timeline_start as i64 + self.delta).max(0) as u64;
                        clip.timeline_start = new_start;
                    }
                }
            }
        }
        project.dirty = true;
    }
    fn undo(&self, project: &mut Project) {
        if let Some(track) = project.track_mut(&self.track_id) {
            let mut current_end = None;
            // 1. Find clip, get CURRENT end (which is the 'new' state we are undoing), then restore
            if let Some(clip) = track.clips.iter_mut().find(|c| c.id == self.clip_id) {
                current_end = Some(clip.timeline_end());
                clip.source_out = self.old_source_out;
            }

            // 2. Shift clips back using the CURRENT end as threshold
            if let Some(threshold) = current_end {
                for clip in &mut track.clips {
                    if clip.timeline_start >= threshold {
                        let new_start = (clip.timeline_start as i64 - self.delta).max(0) as u64;
                        clip.timeline_start = new_start;
                    }
                }
            }
        }
        project.dirty = true;
    }
    fn description(&self) -> &str {
        "Ripple trim"
    }
}

/// Ripple trim the in-point of a clip (shifting subsequent clips)
pub struct RippleTrimInCommand {
    pub clip_id: String,
    pub track_id: String,
    pub old_source_in: u64,
    pub new_source_in: u64,
    pub old_timeline_start: u64,
    pub new_timeline_start: u64,
    /// The delta applied to subsequent clips (can be positive or negative)
    pub delta: i64,
}

impl EditCommand for RippleTrimInCommand {
    fn execute(&self, project: &mut Project) {
        if let Some(track) = project.track_mut(&self.track_id) {
            let mut original_start = None;
            // 1. Find the clip, get its ORIGINAL start, then apply change
            if let Some(clip) = track.clips.iter_mut().find(|c| c.id == self.clip_id) {
                original_start = Some(clip.timeline_start);
                clip.source_in = self.new_source_in;
                clip.timeline_start = self.new_timeline_start;
            }

            // 2. Shift subsequent clips based on ORIGINAL start threshold
            // Note: Since we are trimming the IN point, the clip itself moves (timeline_start changes).
            // We use the ORIGINAL timeline_start as the threshold for subsequent clips.
            // Any clip starting AFTER the original start of this clip should be shifted.
            if let Some(threshold) = original_start {
                for clip in &mut track.clips {
                    // Skip the clip itself (by ID)
                    if clip.id == self.clip_id {
                        continue;
                    }

                    if clip.timeline_start >= threshold {
                        let new_start = (clip.timeline_start as i64 + self.delta).max(0) as u64;
                        clip.timeline_start = new_start;
                    }
                }
            }
        }
        project.dirty = true;
    }
    fn undo(&self, project: &mut Project) {
        if let Some(track) = project.track_mut(&self.track_id) {
            let mut current_start = None;
            // 1. Restore clip
            if let Some(clip) = track.clips.iter_mut().find(|c| c.id == self.clip_id) {
                current_start = Some(clip.timeline_start); // This is the 'new' start
                clip.source_in = self.old_source_in;
                clip.timeline_start = self.old_timeline_start;
            }

            // 2. Shift clips back
            // We use the CURRENT (new) start as threshold, because that's where clips are relative to.
            // Wait, if we moved start from 10 to 12 (delta +2). Subsequent clips moved +2.
            // To undo, we want to move them -2.
            // Threshold should be 12 (new_timeline_start).
            if let Some(threshold) = current_start {
                for clip in &mut track.clips {
                    if clip.id == self.clip_id {
                        continue;
                    }

                    if clip.timeline_start >= threshold {
                        let new_start = (clip.timeline_start as i64 - self.delta).max(0) as u64;
                        clip.timeline_start = new_start;
                    }
                }
            }
        }
        project.dirty = true;
    }
    fn description(&self) -> &str {
        "Ripple trim in-point"
    }
}

/// Slip edit: shift source_in and source_out equally, keeping timeline position and duration fixed.
pub struct SlipClipCommand {
    pub clip_id: String,
    pub track_id: String,
    pub old_source_in: u64,
    pub old_source_out: u64,
    pub new_source_in: u64,
    pub new_source_out: u64,
}

impl EditCommand for SlipClipCommand {
    fn execute(&self, project: &mut Project) {
        if let Some(track) = project.track_mut(&self.track_id) {
            if let Some(clip) = track.clips.iter_mut().find(|c| c.id == self.clip_id) {
                clip.source_in = self.new_source_in;
                clip.source_out = self.new_source_out;
            }
        }
        project.dirty = true;
    }
    fn undo(&self, project: &mut Project) {
        if let Some(track) = project.track_mut(&self.track_id) {
            if let Some(clip) = track.clips.iter_mut().find(|c| c.id == self.clip_id) {
                clip.source_in = self.old_source_in;
                clip.source_out = self.old_source_out;
            }
        }
        project.dirty = true;
    }
    fn description(&self) -> &str {
        "Slip clip"
    }
}

/// Slide edit: move a clip on the timeline while adjusting neighboring clips to compensate.
pub struct SlideClipCommand {
    pub clip_id: String,
    pub track_id: String,
    pub old_start: u64,
    pub new_start: u64,
    pub left_clip_id: Option<String>,
    pub old_left_out: Option<u64>,
    pub new_left_out: Option<u64>,
    pub right_clip_id: Option<String>,
    pub old_right_in: Option<u64>,
    pub new_right_in: Option<u64>,
    pub old_right_start: Option<u64>,
    pub new_right_start: Option<u64>,
}

impl EditCommand for SlideClipCommand {
    fn execute(&self, project: &mut Project) {
        if let Some(track) = project.track_mut(&self.track_id) {
            if let Some(clip) = track.clips.iter_mut().find(|c| c.id == self.clip_id) {
                clip.timeline_start = self.new_start;
            }
            if let (Some(ref lid), Some(new_out)) = (&self.left_clip_id, self.new_left_out) {
                if let Some(left) = track.clips.iter_mut().find(|c| &c.id == lid) {
                    left.source_out = new_out;
                }
            }
            if let (Some(ref rid), Some(new_in), Some(new_rs)) =
                (&self.right_clip_id, self.new_right_in, self.new_right_start)
            {
                if let Some(right) = track.clips.iter_mut().find(|c| &c.id == rid) {
                    right.source_in = new_in;
                    right.timeline_start = new_rs;
                }
            }
        }
        project.dirty = true;
    }
    fn undo(&self, project: &mut Project) {
        if let Some(track) = project.track_mut(&self.track_id) {
            if let Some(clip) = track.clips.iter_mut().find(|c| c.id == self.clip_id) {
                clip.timeline_start = self.old_start;
            }
            if let (Some(ref lid), Some(old_out)) = (&self.left_clip_id, self.old_left_out) {
                if let Some(left) = track.clips.iter_mut().find(|c| &c.id == lid) {
                    left.source_out = old_out;
                }
            }
            if let (Some(ref rid), Some(old_in), Some(old_rs)) =
                (&self.right_clip_id, self.old_right_in, self.old_right_start)
            {
                if let Some(right) = track.clips.iter_mut().find(|c| &c.id == rid) {
                    right.source_in = old_in;
                    right.timeline_start = old_rs;
                }
            }
        }
        project.dirty = true;
    }
    fn description(&self) -> &str {
        "Slide clip"
    }
}

/// Roll edit: adjust the cut point between two clips (left out-point, right in-point/start)
/// Total duration remains constant.
pub struct RollEditCommand {
    pub left_clip_id: String,
    pub right_clip_id: String,
    pub track_id: String,

    // Left clip changes
    pub old_left_out: u64,
    pub new_left_out: u64,

    // Right clip changes
    pub old_right_in: u64,
    pub new_right_in: u64,
    pub old_right_start: u64,
    pub new_right_start: u64,
}

impl EditCommand for RollEditCommand {
    fn execute(&self, project: &mut Project) {
        if let Some(track) = project.track_mut(&self.track_id) {
            // Update left clip
            if let Some(clip) = track.clips.iter_mut().find(|c| c.id == self.left_clip_id) {
                clip.source_out = self.new_left_out;
            }
            // Update right clip
            if let Some(clip) = track.clips.iter_mut().find(|c| c.id == self.right_clip_id) {
                clip.source_in = self.new_right_in;
                clip.timeline_start = self.new_right_start;
            }
        }
        project.dirty = true;
    }
    fn undo(&self, project: &mut Project) {
        if let Some(track) = project.track_mut(&self.track_id) {
            // Restore left clip
            if let Some(clip) = track.clips.iter_mut().find(|c| c.id == self.left_clip_id) {
                clip.source_out = self.old_left_out;
            }
            // Restore right clip
            if let Some(clip) = track.clips.iter_mut().find(|c| c.id == self.right_clip_id) {
                clip.source_in = self.old_right_in;
                clip.timeline_start = self.old_right_start;
            }
        }
        project.dirty = true;
    }
    fn description(&self) -> &str {
        "Roll edit"
    }
}

/// Delete a clip from a track
#[allow(dead_code)]
pub struct DeleteClipCommand {
    pub clip: Clip,
    pub track_id: String,
}

impl EditCommand for DeleteClipCommand {
    fn execute(&self, project: &mut Project) {
        if let Some(track) = project.track_mut(&self.track_id) {
            track.remove_clip(&self.clip.id);
        }
        project.dirty = true;
    }
    fn undo(&self, project: &mut Project) {
        if let Some(track) = project.track_mut(&self.track_id) {
            track.add_clip(self.clip.clone());
        }
        project.dirty = true;
    }
    fn description(&self) -> &str {
        "Delete clip"
    }
}

/// Replace a track's full clip list (used for grouped magnetic timeline edits).
pub struct SetTrackClipsCommand {
    pub track_id: String,
    pub old_clips: Vec<Clip>,
    pub new_clips: Vec<Clip>,
    #[allow(dead_code)]
    pub label: String,
}

impl EditCommand for SetTrackClipsCommand {
    fn execute(&self, project: &mut Project) {
        if let Some(track) = project.track_mut(&self.track_id) {
            track.clips = self.new_clips.clone();
        }
        project.dirty = true;
    }
    fn undo(&self, project: &mut Project) {
        if let Some(track) = project.track_mut(&self.track_id) {
            track.clips = self.old_clips.clone();
        }
        project.dirty = true;
    }
    fn description(&self) -> &str {
        &self.label
    }
}

#[derive(Clone)]
pub struct TrackClipsChange {
    pub track_id: String,
    pub old_clips: Vec<Clip>,
    pub new_clips: Vec<Clip>,
}

/// Replace multiple tracks' full clip lists as a single undo step.
pub struct SetMultipleTracksClipsCommand {
    pub changes: Vec<TrackClipsChange>,
    #[allow(dead_code)]
    pub label: String,
}

impl EditCommand for SetMultipleTracksClipsCommand {
    fn execute(&self, project: &mut Project) {
        for change in &self.changes {
            if let Some(track) = project.track_mut(&change.track_id) {
                track.clips = change.new_clips.clone();
            }
        }
        project.dirty = true;
    }

    fn undo(&self, project: &mut Project) {
        for change in &self.changes {
            if let Some(track) = project.track_mut(&change.track_id) {
                track.clips = change.old_clips.clone();
            }
        }
        project.dirty = true;
    }

    fn description(&self) -> &str {
        &self.label
    }
}

/// Split a clip at a given position (razor cut)
pub struct SplitClipCommand {
    pub original_clip: Clip,
    pub track_id: String,
    pub split_ns: u64,    // absolute timeline position of cut
    pub right_clip: Clip, // new clip for the right half
}

impl EditCommand for SplitClipCommand {
    fn execute(&self, project: &mut Project) {
        if let Some(track) = project.track_mut(&self.track_id) {
            // Shorten the original clip to end at the split point
            if let Some(clip) = track
                .clips
                .iter_mut()
                .find(|c| c.id == self.original_clip.id)
            {
                let cut_offset = self.split_ns - clip.timeline_start;
                let new_source_out = clip.source_in + cut_offset;
                clip.source_out = new_source_out;
                // Filter left clip subtitles: keep only segments that end before the cut point.
                clip.subtitle_segments
                    .retain(|s| s.end_ns <= new_source_out);
            }
            // Insert the right half (already has filtered subtitles from creation).
            track.add_clip(self.right_clip.clone());
        }
        project.dirty = true;
    }
    fn undo(&self, project: &mut Project) {
        if let Some(track) = project.track_mut(&self.track_id) {
            track.remove_clip(&self.right_clip.id);
            if let Some(clip) = track
                .clips
                .iter_mut()
                .find(|c| c.id == self.original_clip.id)
            {
                clip.source_out = self.original_clip.source_out;
            }
        }
        project.dirty = true;
    }
    fn description(&self) -> &str {
        "Razor cut"
    }
}

/// Join two through-edit clip segments back into a single clip.
pub struct JoinThroughEditCommand {
    pub track_id: String,
    pub old_clips: Vec<Clip>,
    pub new_clips: Vec<Clip>,
}

impl EditCommand for JoinThroughEditCommand {
    fn execute(&self, project: &mut Project) {
        if let Some(track) = project.track_mut(&self.track_id) {
            track.clips = self.new_clips.clone();
        }
        project.dirty = true;
    }

    fn undo(&self, project: &mut Project) {
        if let Some(track) = project.track_mut(&self.track_id) {
            track.clips = self.old_clips.clone();
        }
        project.dirty = true;
    }

    fn description(&self) -> &str {
        "Join through edit"
    }
}

/// Snapshot of all color-correction properties on a clip.
/// Used by SetClipColorCommand to fully restore state on undo/redo.
#[derive(Clone, Debug, Default)]
pub struct ClipColorSnapshot {
    pub brightness: f32,
    pub contrast: f32,
    pub saturation: f32,
    pub temperature: f32,
    pub tint: f32,
    pub denoise: f32,
    pub sharpness: f32,
    pub blur: f32,
    pub shadows: f32,
    pub midtones: f32,
    pub highlights: f32,
    pub exposure: f32,
    pub black_point: f32,
    pub highlights_warmth: f32,
    pub highlights_tint: f32,
    pub midtones_warmth: f32,
    pub midtones_tint: f32,
    pub shadows_warmth: f32,
    pub shadows_tint: f32,
}

impl ClipColorSnapshot {
    pub fn from_clip(clip: &crate::model::clip::Clip) -> Self {
        Self {
            brightness: clip.brightness,
            contrast: clip.contrast,
            saturation: clip.saturation,
            temperature: clip.temperature,
            tint: clip.tint,
            denoise: clip.denoise,
            sharpness: clip.sharpness,
            blur: clip.blur,
            shadows: clip.shadows,
            midtones: clip.midtones,
            highlights: clip.highlights,
            exposure: clip.exposure,
            black_point: clip.black_point,
            highlights_warmth: clip.highlights_warmth,
            highlights_tint: clip.highlights_tint,
            midtones_warmth: clip.midtones_warmth,
            midtones_tint: clip.midtones_tint,
            shadows_warmth: clip.shadows_warmth,
            shadows_tint: clip.shadows_tint,
        }
    }

    fn apply_to(&self, clip: &mut crate::model::clip::Clip) {
        clip.brightness = self.brightness;
        clip.contrast = self.contrast;
        clip.saturation = self.saturation;
        clip.temperature = self.temperature;
        clip.tint = self.tint;
        clip.denoise = self.denoise;
        clip.sharpness = self.sharpness;
        clip.blur = self.blur;
        clip.shadows = self.shadows;
        clip.midtones = self.midtones;
        clip.highlights = self.highlights;
        clip.exposure = self.exposure;
        clip.black_point = self.black_point;
        clip.highlights_warmth = self.highlights_warmth;
        clip.highlights_tint = self.highlights_tint;
        clip.midtones_warmth = self.midtones_warmth;
        clip.midtones_tint = self.midtones_tint;
        clip.shadows_warmth = self.shadows_warmth;
        clip.shadows_tint = self.shadows_tint;
    }
}

/// Set all color-correction properties on a clip (full snapshot approach).
pub struct SetClipColorCommand {
    pub clip_id: String,
    pub track_id: String,
    pub old_color: ClipColorSnapshot,
    pub new_color: ClipColorSnapshot,
}

impl EditCommand for SetClipColorCommand {
    fn execute(&self, project: &mut Project) {
        if let Some(track) = project.track_mut(&self.track_id) {
            if let Some(clip) = track.clips.iter_mut().find(|c| c.id == self.clip_id) {
                self.new_color.apply_to(clip);
            }
        }
        project.dirty = true;
    }
    fn undo(&self, project: &mut Project) {
        if let Some(track) = project.track_mut(&self.track_id) {
            if let Some(clip) = track.clips.iter_mut().find(|c| c.id == self.clip_id) {
                self.old_color.apply_to(clip);
            }
        }
        project.dirty = true;
    }
    fn description(&self) -> &str {
        "Set clip color"
    }
}

/// Normalize clip audio volume (stores old/new volume + measured loudness).
#[allow(dead_code)]
pub struct NormalizeClipAudioCommand {
    pub clip_id: String,
    pub old_volume: f32,
    pub new_volume: f32,
    pub old_measured_loudness: Option<f64>,
    pub new_measured_loudness: Option<f64>,
}

impl EditCommand for NormalizeClipAudioCommand {
    fn execute(&self, project: &mut Project) {
        if let Some(clip) = project.clip_mut(&self.clip_id) {
            clip.volume = self.new_volume;
            clip.measured_loudness_lufs = self.new_measured_loudness;
        }
        project.dirty = true;
    }
    fn undo(&self, project: &mut Project) {
        if let Some(clip) = project.clip_mut(&self.clip_id) {
            clip.volume = self.old_volume;
            clip.measured_loudness_lufs = self.old_measured_loudness;
        }
        project.dirty = true;
    }
    fn description(&self) -> &str {
        "Normalize clip audio"
    }
}

/// Set 3-band parametric EQ on a clip.
#[allow(dead_code)]
pub struct SetClipEqCommand {
    pub clip_id: String,
    pub track_id: String,
    pub old_eq_bands: [crate::model::clip::EqBand; 3],
    pub new_eq_bands: [crate::model::clip::EqBand; 3],
}

impl EditCommand for SetClipEqCommand {
    fn execute(&self, project: &mut Project) {
        if let Some(track) = project.track_mut(&self.track_id) {
            if let Some(clip) = track.clips.iter_mut().find(|c| c.id == self.clip_id) {
                clip.eq_bands = self.new_eq_bands;
            }
        }
        project.dirty = true;
    }
    fn undo(&self, project: &mut Project) {
        if let Some(track) = project.track_mut(&self.track_id) {
            if let Some(clip) = track.clips.iter_mut().find(|c| c.id == self.clip_id) {
                clip.eq_bands = self.old_eq_bands;
            }
        }
        project.dirty = true;
    }
    fn description(&self) -> &str {
        "Set clip EQ"
    }
}

/// Match clip audio tone using measured loudness plus 3-band EQ and 7-band match EQ.
pub struct MatchClipAudioCommand {
    pub clip_id: String,
    pub old_volume: f32,
    pub new_volume: f32,
    pub old_measured_loudness: Option<f64>,
    pub new_measured_loudness: Option<f64>,
    pub old_eq_bands: [crate::model::clip::EqBand; 3],
    pub new_eq_bands: [crate::model::clip::EqBand; 3],
    pub old_match_eq_bands: Vec<crate::model::clip::EqBand>,
    pub new_match_eq_bands: Vec<crate::model::clip::EqBand>,
}

impl MatchClipAudioCommand {
    fn apply_values(&self, project: &mut Project, use_new: bool) {
        if let Some(clip) = project.clip_mut(&self.clip_id) {
            if use_new {
                clip.volume = self.new_volume;
                clip.measured_loudness_lufs = self.new_measured_loudness;
                clip.eq_bands = self.new_eq_bands;
                clip.match_eq_bands = self.new_match_eq_bands.clone();
            } else {
                clip.volume = self.old_volume;
                clip.measured_loudness_lufs = self.old_measured_loudness;
                clip.eq_bands = self.old_eq_bands;
                clip.match_eq_bands = self.old_match_eq_bands.clone();
            }
        }
        project.dirty = true;
    }
}

impl EditCommand for MatchClipAudioCommand {
    fn execute(&self, project: &mut Project) {
        self.apply_values(project, true);
    }
    fn undo(&self, project: &mut Project) {
        self.apply_values(project, false);
    }
    fn description(&self) -> &str {
        "Match clip audio"
    }
}

/// Clear 7-band match EQ from a clip.
pub struct ClearMatchEqCommand {
    pub clip_id: String,
    pub old_match_eq_bands: Vec<crate::model::clip::EqBand>,
}

impl EditCommand for ClearMatchEqCommand {
    fn execute(&self, project: &mut Project) {
        if let Some(clip) = project.clip_mut(&self.clip_id) {
            clip.match_eq_bands.clear();
        }
        project.dirty = true;
    }
    fn undo(&self, project: &mut Project) {
        if let Some(clip) = project.clip_mut(&self.clip_id) {
            clip.match_eq_bands = self.old_match_eq_bands.clone();
        }
        project.dirty = true;
    }
    fn description(&self) -> &str {
        "Clear match EQ"
    }
}

/// Set clip volume and/or pan.
pub struct SetClipVolumeCommand {
    pub clip_id: String,
    pub track_id: String,
    pub old_volume: f32,
    pub new_volume: f32,
    pub old_pan: f32,
    pub new_pan: f32,
}

impl EditCommand for SetClipVolumeCommand {
    fn execute(&self, project: &mut Project) {
        if let Some(track) = project.track_mut(&self.track_id) {
            if let Some(clip) = track.clips.iter_mut().find(|c| c.id == self.clip_id) {
                clip.volume = self.new_volume;
                clip.pan = self.new_pan;
            }
        }
        project.dirty = true;
    }
    fn undo(&self, project: &mut Project) {
        if let Some(track) = project.track_mut(&self.track_id) {
            if let Some(clip) = track.clips.iter_mut().find(|c| c.id == self.clip_id) {
                clip.volume = self.old_volume;
                clip.pan = self.old_pan;
            }
        }
        project.dirty = true;
    }
    fn description(&self) -> &str {
        "Set clip volume/pan"
    }
}

/// Set clip voice isolation amount.
/// Set per-clip motion-blur enable + shutter angle as a single undoable
/// operation. The fields are coupled (the slider only matters when the
/// toggle is on) so changing either records both old/new pairs and the
/// undo restores both.
pub struct SetClipMotionBlurCommand {
    pub clip_id: String,
    pub track_id: String,
    pub old_enabled: bool,
    pub old_shutter_angle: f64,
    pub new_enabled: bool,
    pub new_shutter_angle: f64,
}

impl EditCommand for SetClipMotionBlurCommand {
    fn execute(&self, project: &mut Project) {
        if let Some(clip) = find_clip_mut(project, &self.clip_id, &self.track_id) {
            clip.motion_blur_enabled = self.new_enabled;
            clip.motion_blur_shutter_angle = self.new_shutter_angle;
        }
        project.dirty = true;
    }
    fn undo(&self, project: &mut Project) {
        if let Some(clip) = find_clip_mut(project, &self.clip_id, &self.track_id) {
            clip.motion_blur_enabled = self.old_enabled;
            clip.motion_blur_shutter_angle = self.old_shutter_angle;
        }
        project.dirty = true;
    }
    fn description(&self) -> &str {
        "Set clip motion blur"
    }
}

/// Undoable mutation for the Auto-Crop & Track feature.
///
/// Auto-crop touches three pieces of clip state at once:
/// 1. `clip.tracking_binding` — the transform binding that pans/zooms
///    the clip to center the tracked region.
/// 2. `clip.motion_trackers` — may be updated in place (region changed)
///    or gain a new tracker when the caller came in via MCP without
///    first creating one.
/// 3. `clip.masks[0].tracking_binding` — cleared so the clip transform
///    owns the binding alone.
///
/// Snapshot them all so undo restores the exact state the user had
/// before clicking the button.
pub struct SetClipAutoCropCommand {
    pub clip_id: String,
    pub old_tracking_binding: Option<crate::model::clip::TrackingBinding>,
    pub old_motion_trackers: Vec<crate::model::clip::MotionTracker>,
    pub old_first_mask_binding: Option<Option<crate::model::clip::TrackingBinding>>,
    pub new_tracking_binding: Option<crate::model::clip::TrackingBinding>,
    pub new_motion_trackers: Vec<crate::model::clip::MotionTracker>,
    pub new_first_mask_binding: Option<Option<crate::model::clip::TrackingBinding>>,
}

impl EditCommand for SetClipAutoCropCommand {
    fn execute(&self, project: &mut Project) {
        if let Some(clip) = project.clip_mut(&self.clip_id) {
            clip.tracking_binding = self.new_tracking_binding.clone();
            clip.motion_trackers = self.new_motion_trackers.clone();
            if let Some(first_mask_binding) = &self.new_first_mask_binding {
                if let Some(mask) = clip.masks.first_mut() {
                    mask.tracking_binding = first_mask_binding.clone();
                }
            }
        }
        project.dirty = true;
    }
    fn undo(&self, project: &mut Project) {
        if let Some(clip) = project.clip_mut(&self.clip_id) {
            clip.tracking_binding = self.old_tracking_binding.clone();
            clip.motion_trackers = self.old_motion_trackers.clone();
            if let Some(first_mask_binding) = &self.old_first_mask_binding {
                if let Some(mask) = clip.masks.first_mut() {
                    mask.tracking_binding = first_mask_binding.clone();
                }
            }
        }
        project.dirty = true;
    }
    fn description(&self) -> &str {
        "Auto-crop & track"
    }
}

pub struct SetClipVoiceIsolationCommand {
    pub clip_id: String,
    pub track_id: String,
    pub old_amount: f32,
    pub new_amount: f32,
}

impl EditCommand for SetClipVoiceIsolationCommand {
    fn execute(&self, project: &mut Project) {
        if let Some(clip) = find_clip_mut(project, &self.clip_id, &self.track_id) {
            clip.voice_isolation = self.new_amount;
        }
        project.dirty = true;
    }
    fn undo(&self, project: &mut Project) {
        if let Some(clip) = find_clip_mut(project, &self.clip_id, &self.track_id) {
            clip.voice_isolation = self.old_amount;
        }
        project.dirty = true;
    }
    fn description(&self) -> &str {
        "Set clip voice isolation"
    }
}

/// Switch the source of voice-isolation gate intervals between subtitles and
/// silence-detect analysis.
pub struct SetClipVoiceIsolationSourceCommand {
    pub clip_id: String,
    pub track_id: String,
    pub old_source: VoiceIsolationSource,
    pub new_source: VoiceIsolationSource,
}

impl EditCommand for SetClipVoiceIsolationSourceCommand {
    fn execute(&self, project: &mut Project) {
        if let Some(clip) = find_clip_mut(project, &self.clip_id, &self.track_id) {
            clip.voice_isolation_source = self.new_source;
        }
        project.dirty = true;
    }
    fn undo(&self, project: &mut Project) {
        if let Some(clip) = find_clip_mut(project, &self.clip_id, &self.track_id) {
            clip.voice_isolation_source = self.old_source;
        }
        project.dirty = true;
    }
    fn description(&self) -> &str {
        "Set voice isolation source"
    }
}

/// Update the silence-detect threshold and/or minimum gap parameters used to
/// produce voice-isolation speech intervals. Also captures the cached intervals
/// before/after so undo restores prior analysis without forcing a re-Analyze.
pub struct SetClipVoiceIsolationSilenceParamsCommand {
    pub clip_id: String,
    pub track_id: String,
    pub old_threshold_db: f32,
    pub new_threshold_db: f32,
    pub old_min_ms: u32,
    pub new_min_ms: u32,
    pub old_intervals: Vec<(u64, u64)>,
}

impl EditCommand for SetClipVoiceIsolationSilenceParamsCommand {
    fn execute(&self, project: &mut Project) {
        if let Some(clip) = find_clip_mut(project, &self.clip_id, &self.track_id) {
            clip.voice_isolation_silence_threshold_db = self.new_threshold_db;
            clip.voice_isolation_silence_min_ms = self.new_min_ms;
            // Parameter change invalidates the cached analysis.
            clip.voice_isolation_speech_intervals.clear();
        }
        project.dirty = true;
    }
    fn undo(&self, project: &mut Project) {
        if let Some(clip) = find_clip_mut(project, &self.clip_id, &self.track_id) {
            clip.voice_isolation_silence_threshold_db = self.old_threshold_db;
            clip.voice_isolation_silence_min_ms = self.old_min_ms;
            clip.voice_isolation_speech_intervals = self.old_intervals.clone();
        }
        project.dirty = true;
    }
    fn description(&self) -> &str {
        "Set voice isolation silence params"
    }
}

/// Capture the result of running silencedetect analysis on a clip so the
/// resulting cached intervals can be undone.
pub struct AnalyzeVoiceIsolationSilenceCommand {
    pub clip_id: String,
    pub track_id: String,
    pub old_intervals: Vec<(u64, u64)>,
    pub new_intervals: Vec<(u64, u64)>,
}

impl EditCommand for AnalyzeVoiceIsolationSilenceCommand {
    fn execute(&self, project: &mut Project) {
        if let Some(clip) = find_clip_mut(project, &self.clip_id, &self.track_id) {
            clip.voice_isolation_speech_intervals = self.new_intervals.clone();
        }
        project.dirty = true;
    }
    fn undo(&self, project: &mut Project) {
        if let Some(clip) = find_clip_mut(project, &self.clip_id, &self.track_id) {
            clip.voice_isolation_speech_intervals = self.old_intervals.clone();
        }
        project.dirty = true;
    }
    fn description(&self) -> &str {
        "Analyze voice isolation"
    }
}

/// Set clip playback speed.
pub struct SetClipSpeedCommand {
    pub clip_id: String,
    pub track_id: String,
    pub old_speed: f64,
    pub new_speed: f64,
}

impl EditCommand for SetClipSpeedCommand {
    fn execute(&self, project: &mut Project) {
        if let Some(track) = project.track_mut(&self.track_id) {
            if let Some(clip) = track.clips.iter_mut().find(|c| c.id == self.clip_id) {
                clip.speed = self.new_speed;
            }
        }
        project.dirty = true;
    }
    fn undo(&self, project: &mut Project) {
        if let Some(track) = project.track_mut(&self.track_id) {
            if let Some(clip) = track.clips.iter_mut().find(|c| c.id == self.clip_id) {
                clip.speed = self.old_speed;
            }
        }
        project.dirty = true;
    }
    fn description(&self) -> &str {
        "Set clip speed"
    }
}

/// Rename a clip's display label.
pub struct SetClipLabelCommand {
    pub clip_id: String,
    pub track_id: String,
    pub old_label: String,
    pub new_label: String,
}

impl EditCommand for SetClipLabelCommand {
    fn execute(&self, project: &mut Project) {
        if let Some(track) = project.track_mut(&self.track_id) {
            if let Some(clip) = track.clips.iter_mut().find(|c| c.id == self.clip_id) {
                clip.label = self.new_label.clone();
            }
        }
        project.dirty = true;
    }
    fn undo(&self, project: &mut Project) {
        if let Some(track) = project.track_mut(&self.track_id) {
            if let Some(clip) = track.clips.iter_mut().find(|c| c.id == self.clip_id) {
                clip.label = self.old_label.clone();
            }
        }
        project.dirty = true;
    }
    fn description(&self) -> &str {
        "Rename clip"
    }
}

/// Toggle a track's mute state. Construct via `set_track_mute_cmd()`.
pub fn set_track_mute_cmd(
    track_id: String,
    old_muted: bool,
    new_muted: bool,
) -> TrackMutateCommand<bool> {
    TrackMutateCommand {
        track_id,
        old_state: old_muted,
        new_state: new_muted,
        apply: |track, v| {
            track.muted = v;
        },
        label: "Toggle track mute",
    }
}

/// Toggle a track's solo state. Construct via `set_track_solo_cmd()`.
pub fn set_track_solo_cmd(
    track_id: String,
    old_solo: bool,
    new_solo: bool,
) -> TrackMutateCommand<bool> {
    TrackMutateCommand {
        track_id,
        old_state: old_solo,
        new_state: new_solo,
        apply: |track, v| {
            track.soloed = v;
        },
        label: "Toggle track solo",
    }
}

/// Toggle a track's duck (sidechain) state. Construct via `set_track_duck_cmd()`.
pub fn set_track_duck_cmd(
    track_id: String,
    old_duck: bool,
    new_duck: bool,
) -> TrackMutateCommand<bool> {
    TrackMutateCommand {
        track_id,
        old_state: old_duck,
        new_state: new_duck,
        apply: |track, v| {
            track.duck = v;
        },
        label: "Toggle track duck",
    }
}

/// Toggle a track's muted state. Construct via `set_track_muted_cmd()`.
pub fn set_track_muted_cmd(
    track_id: String,
    old_muted: bool,
    new_muted: bool,
) -> TrackMutateCommand<bool> {
    TrackMutateCommand {
        track_id,
        old_state: old_muted,
        new_state: new_muted,
        apply: |track, v| {
            track.muted = v;
        },
        label: "Toggle track mute",
    }
}

/// Toggle a track's locked state. Construct via `set_track_locked_cmd()`.
pub fn set_track_locked_cmd(
    track_id: String,
    old_locked: bool,
    new_locked: bool,
) -> TrackMutateCommand<bool> {
    TrackMutateCommand {
        track_id,
        old_state: old_locked,
        new_state: new_locked,
        apply: |track, v| {
            track.locked = v;
        },
        label: "Toggle track lock",
    }
}

/// Change a track's color label. Construct via `set_track_color_label_cmd()`.
pub fn set_track_color_label_cmd(
    track_id: String,
    old_color: crate::model::track::TrackColorLabel,
    new_color: crate::model::track::TrackColorLabel,
) -> TrackMutateCommand<crate::model::track::TrackColorLabel> {
    TrackMutateCommand {
        track_id,
        old_state: old_color,
        new_state: new_color,
        apply: |track, v| {
            track.color_label = v;
        },
        label: "Set track color",
    }
}

/// Rename a track. Construct via `set_track_label_cmd()`.
pub fn set_track_label_cmd(
    track_id: String,
    old_label: String,
    new_label: String,
) -> TrackMutateCommand<String> {
    TrackMutateCommand {
        track_id,
        old_state: old_label,
        new_state: new_label,
        apply: |track, v| {
            track.label = v;
        },
        label: "Rename track",
    }
}

/// Set a track's gain in dB. Construct via `set_track_gain_cmd()`.
pub fn set_track_gain_cmd(
    track_id: String,
    old_gain_db: f64,
    new_gain_db: f64,
) -> TrackMutateCommand<f64> {
    TrackMutateCommand {
        track_id,
        old_state: old_gain_db,
        new_state: new_gain_db,
        apply: |track, v| {
            track.gain_db = v;
        },
        label: "Set track gain",
    }
}

/// Set a track's stereo pan (−1.0 to +1.0). Construct via `set_track_pan_cmd()`.
pub fn set_track_pan_cmd(track_id: String, old_pan: f64, new_pan: f64) -> TrackMutateCommand<f64> {
    TrackMutateCommand {
        track_id,
        old_state: old_pan,
        new_state: new_pan,
        apply: |track, v| {
            track.pan = v;
        },
        label: "Set track pan",
    }
}

/// Match one clip's color to another — stores all color parameters before/after.
pub struct MatchColorCommand {
    pub clip_id: String,
    pub track_id: String,
    pub old_brightness: f32,
    pub old_contrast: f32,
    pub old_saturation: f32,
    pub old_temperature: f32,
    pub old_tint: f32,
    pub old_exposure: f32,
    pub old_black_point: f32,
    pub old_shadows: f32,
    pub old_midtones: f32,
    pub old_highlights: f32,
    pub old_highlights_warmth: f32,
    pub old_highlights_tint: f32,
    pub old_midtones_warmth: f32,
    pub old_midtones_tint: f32,
    pub old_shadows_warmth: f32,
    pub old_shadows_tint: f32,
    pub old_lut_paths: Vec<String>,
    pub new_brightness: f32,
    pub new_contrast: f32,
    pub new_saturation: f32,
    pub new_temperature: f32,
    pub new_tint: f32,
    pub new_exposure: f32,
    pub new_black_point: f32,
    pub new_shadows: f32,
    pub new_midtones: f32,
    pub new_highlights: f32,
    pub new_highlights_warmth: f32,
    pub new_highlights_tint: f32,
    pub new_midtones_warmth: f32,
    pub new_midtones_tint: f32,
    pub new_shadows_warmth: f32,
    pub new_shadows_tint: f32,
    pub new_lut_paths: Vec<String>,
}

impl MatchColorCommand {
    fn apply_values(&self, project: &mut Project, use_new: bool) {
        if let Some(track) = project.track_mut(&self.track_id) {
            if let Some(clip) = track.clips.iter_mut().find(|c| c.id == self.clip_id) {
                if use_new {
                    clip.brightness = self.new_brightness;
                    clip.contrast = self.new_contrast;
                    clip.saturation = self.new_saturation;
                    clip.temperature = self.new_temperature;
                    clip.tint = self.new_tint;
                    clip.exposure = self.new_exposure;
                    clip.black_point = self.new_black_point;
                    clip.shadows = self.new_shadows;
                    clip.midtones = self.new_midtones;
                    clip.highlights = self.new_highlights;
                    clip.highlights_warmth = self.new_highlights_warmth;
                    clip.highlights_tint = self.new_highlights_tint;
                    clip.midtones_warmth = self.new_midtones_warmth;
                    clip.midtones_tint = self.new_midtones_tint;
                    clip.shadows_warmth = self.new_shadows_warmth;
                    clip.shadows_tint = self.new_shadows_tint;
                    clip.lut_paths = self.new_lut_paths.clone();
                } else {
                    clip.brightness = self.old_brightness;
                    clip.contrast = self.old_contrast;
                    clip.saturation = self.old_saturation;
                    clip.temperature = self.old_temperature;
                    clip.tint = self.old_tint;
                    clip.exposure = self.old_exposure;
                    clip.black_point = self.old_black_point;
                    clip.shadows = self.old_shadows;
                    clip.midtones = self.old_midtones;
                    clip.highlights = self.old_highlights;
                    clip.highlights_warmth = self.old_highlights_warmth;
                    clip.highlights_tint = self.old_highlights_tint;
                    clip.midtones_warmth = self.old_midtones_warmth;
                    clip.midtones_tint = self.old_midtones_tint;
                    clip.shadows_warmth = self.old_shadows_warmth;
                    clip.shadows_tint = self.old_shadows_tint;
                    clip.lut_paths = self.old_lut_paths.clone();
                }
            }
        }
        project.dirty = true;
    }
}

impl EditCommand for MatchColorCommand {
    fn execute(&self, project: &mut Project) {
        self.apply_values(project, true);
    }
    fn undo(&self, project: &mut Project) {
        self.apply_values(project, false);
    }
    fn description(&self) -> &str {
        "Match clip color"
    }
}

/// Set transition metadata on a clip boundary (clip -> next clip).
pub struct SetClipTransitionCommand {
    pub clip_id: String,
    pub track_id: String,
    pub old_transition: OutgoingTransition,
    pub new_transition: OutgoingTransition,
}

impl EditCommand for SetClipTransitionCommand {
    fn execute(&self, project: &mut Project) {
        if let Some(track) = project.track_mut(&self.track_id) {
            if let Some(clip) = track.clips.iter_mut().find(|c| c.id == self.clip_id) {
                clip.outgoing_transition = self.new_transition.clone();
            }
        }
        project.dirty = true;
    }
    fn undo(&self, project: &mut Project) {
        if let Some(track) = project.track_mut(&self.track_id) {
            if let Some(clip) = track.clips.iter_mut().find(|c| c.id == self.clip_id) {
                clip.outgoing_transition = self.old_transition.clone();
            }
        }
        project.dirty = true;
    }
    fn description(&self) -> &str {
        "Set clip transition"
    }
}

/// Delete a track (stores full track + index for undo).
pub struct DeleteTrackCommand {
    pub track: crate::model::track::Track,
    pub index: usize,
}

impl EditCommand for DeleteTrackCommand {
    fn execute(&self, project: &mut Project) {
        if self.index < project.tracks.len() {
            project.tracks.remove(self.index);
        }
        project.dirty = true;
    }
    fn undo(&self, project: &mut Project) {
        let idx = self.index.min(project.tracks.len());
        project.tracks.insert(idx, self.track.clone());
        project.dirty = true;
    }
    fn description(&self) -> &str {
        "Delete track"
    }
}

/// Add a track (stores track + insertion index for undo).
pub struct AddTrackCommand {
    pub track: crate::model::track::Track,
    pub index: usize,
}

impl EditCommand for AddTrackCommand {
    fn execute(&self, project: &mut Project) {
        let idx = self.index.min(project.tracks.len());
        project.tracks.insert(idx, self.track.clone());
        project.dirty = true;
    }
    fn undo(&self, project: &mut Project) {
        if self.index < project.tracks.len() {
            project.tracks.remove(self.index);
        }
        project.dirty = true;
    }
    fn description(&self) -> &str {
        "Add track"
    }
}

/// Reorder a track from one index to another.
pub struct ReorderTrackCommand {
    pub from_index: usize,
    pub to_index: usize,
}

impl EditCommand for ReorderTrackCommand {
    fn execute(&self, project: &mut Project) {
        reorder_track(&mut project.tracks, self.from_index, self.to_index);
        project.dirty = true;
    }
    fn undo(&self, project: &mut Project) {
        reorder_track(&mut project.tracks, self.to_index, self.from_index);
        project.dirty = true;
    }
    fn description(&self) -> &str {
        "Reorder track"
    }
}

fn reorder_track<T>(vec: &mut Vec<T>, from: usize, to: usize) {
    if from >= vec.len() || to >= vec.len() || from == to {
        return;
    }
    let item = vec.remove(from);
    vec.insert(to, item);
}

pub struct EditHistory {
    pub undo_stack: Vec<Box<dyn EditCommand>>,
    pub redo_stack: Vec<Box<dyn EditCommand>>,
}

impl EditHistory {
    pub fn new() -> Self {
        Self {
            undo_stack: Vec::new(),
            redo_stack: Vec::new(),
        }
    }

    pub fn execute(&mut self, cmd: Box<dyn EditCommand>, project: &mut Project) {
        cmd.execute(project);
        self.undo_stack.push(cmd);
        self.redo_stack.clear(); // new action clears redo stack
    }

    pub fn undo_with_description(&mut self, project: &mut Project) -> Option<String> {
        let description = self
            .undo_stack
            .last()
            .map(|c| c.description().to_string())?;
        let cmd = self.undo_stack.pop().expect("undo stack just had an item");
        cmd.undo(project);
        self.redo_stack.push(cmd);
        Some(description)
    }

    pub fn undo(&mut self, project: &mut Project) -> bool {
        self.undo_with_description(project).is_some()
    }

    pub fn redo_with_description(&mut self, project: &mut Project) -> Option<String> {
        let description = self
            .redo_stack
            .last()
            .map(|c| c.description().to_string())?;
        let cmd = self.redo_stack.pop().expect("redo stack just had an item");
        cmd.execute(project);
        self.undo_stack.push(cmd);
        Some(description)
    }

    pub fn redo(&mut self, project: &mut Project) -> bool {
        self.redo_with_description(project).is_some()
    }

    #[allow(dead_code)]
    pub fn can_undo(&self) -> bool {
        !self.undo_stack.is_empty()
    }
    #[allow(dead_code)]
    pub fn can_redo(&self) -> bool {
        !self.redo_stack.is_empty()
    }

    #[allow(dead_code)]
    pub fn undo_description(&self) -> Option<&str> {
        self.undo_stack.last().map(|c| c.description())
    }

    #[allow(dead_code)]
    pub fn redo_description(&self) -> Option<&str> {
        self.redo_stack.last().map(|c| c.description())
    }
}

fn move_clip(
    project: &mut Project,
    clip_id: &str,
    from_track_id: &str,
    to_track_id: &str,
    new_start: u64,
) {
    // Extract clip from source track
    let clip = {
        let from_track = match project.track_mut(from_track_id) {
            Some(t) => t,
            None => return,
        };
        let pos = from_track.clips.iter().position(|c| c.id == clip_id);
        pos.map(|i| from_track.clips.remove(i))
    };

    if let Some(mut clip) = clip {
        clip.timeline_start = new_start;
        if let Some(to_track) = project.track_mut(to_track_id) {
            to_track.add_clip(clip);
        }
        project.dirty = true;
    }
}

// ── Frei0r Effect Commands ──────────────────────────────────────────────────

/// Add a frei0r effect to a clip.
pub struct AddFrei0rEffectCommand {
    pub clip_id: String,
    pub track_id: String,
    pub effect: crate::model::clip::Frei0rEffect,
    pub index: usize,
}

impl EditCommand for AddFrei0rEffectCommand {
    fn execute(&self, project: &mut Project) {
        if let Some(track) = project.track_mut(&self.track_id) {
            if let Some(clip) = track.clips.iter_mut().find(|c| c.id == self.clip_id) {
                let idx = self.index.min(clip.frei0r_effects.len());
                clip.frei0r_effects.insert(idx, self.effect.clone());
            }
        }
        project.dirty = true;
    }
    fn undo(&self, project: &mut Project) {
        if let Some(track) = project.track_mut(&self.track_id) {
            if let Some(clip) = track.clips.iter_mut().find(|c| c.id == self.clip_id) {
                clip.frei0r_effects.retain(|e| e.id != self.effect.id);
            }
        }
        project.dirty = true;
    }
    fn description(&self) -> &str {
        "Add frei0r effect"
    }
}

/// Remove a frei0r effect from a clip.
pub struct RemoveFrei0rEffectCommand {
    pub clip_id: String,
    pub track_id: String,
    pub effect: crate::model::clip::Frei0rEffect,
    pub index: usize,
}

impl EditCommand for RemoveFrei0rEffectCommand {
    fn execute(&self, project: &mut Project) {
        if let Some(track) = project.track_mut(&self.track_id) {
            if let Some(clip) = track.clips.iter_mut().find(|c| c.id == self.clip_id) {
                clip.frei0r_effects.retain(|e| e.id != self.effect.id);
            }
        }
        project.dirty = true;
    }
    fn undo(&self, project: &mut Project) {
        if let Some(track) = project.track_mut(&self.track_id) {
            if let Some(clip) = track.clips.iter_mut().find(|c| c.id == self.clip_id) {
                let idx = self.index.min(clip.frei0r_effects.len());
                clip.frei0r_effects.insert(idx, self.effect.clone());
            }
        }
        project.dirty = true;
    }
    fn description(&self) -> &str {
        "Remove frei0r effect"
    }
}

/// Reorder frei0r effects on a clip (swap two adjacent entries).
pub struct ReorderFrei0rEffectsCommand {
    pub clip_id: String,
    pub track_id: String,
    pub index_a: usize,
    pub index_b: usize,
}

impl EditCommand for ReorderFrei0rEffectsCommand {
    fn execute(&self, project: &mut Project) {
        if let Some(track) = project.track_mut(&self.track_id) {
            if let Some(clip) = track.clips.iter_mut().find(|c| c.id == self.clip_id) {
                let len = clip.frei0r_effects.len();
                if self.index_a < len && self.index_b < len {
                    clip.frei0r_effects.swap(self.index_a, self.index_b);
                }
            }
        }
        project.dirty = true;
    }
    fn undo(&self, project: &mut Project) {
        // Swapping the same pair reverses the operation.
        self.execute(project);
    }
    fn description(&self) -> &str {
        "Reorder frei0r effects"
    }
}

/// Change parameters of a frei0r effect on a clip.
pub struct SetFrei0rEffectParamsCommand {
    pub clip_id: String,
    pub track_id: String,
    pub effect_id: String,
    pub old_params: std::collections::HashMap<String, f64>,
    pub new_params: std::collections::HashMap<String, f64>,
}

impl EditCommand for SetFrei0rEffectParamsCommand {
    fn execute(&self, project: &mut Project) {
        if let Some(track) = project.track_mut(&self.track_id) {
            if let Some(clip) = track.clips.iter_mut().find(|c| c.id == self.clip_id) {
                if let Some(effect) = clip
                    .frei0r_effects
                    .iter_mut()
                    .find(|e| e.id == self.effect_id)
                {
                    effect.params = self.new_params.clone();
                }
            }
        }
        project.dirty = true;
    }
    fn undo(&self, project: &mut Project) {
        if let Some(track) = project.track_mut(&self.track_id) {
            if let Some(clip) = track.clips.iter_mut().find(|c| c.id == self.clip_id) {
                if let Some(effect) = clip
                    .frei0r_effects
                    .iter_mut()
                    .find(|e| e.id == self.effect_id)
                {
                    effect.params = self.old_params.clone();
                }
            }
        }
        project.dirty = true;
    }
    fn description(&self) -> &str {
        "Set frei0r effect parameters"
    }
}

/// Toggle the enabled state of a frei0r effect on a clip.
pub struct ToggleFrei0rEffectCommand {
    pub clip_id: String,
    pub track_id: String,
    pub effect_id: String,
}

impl EditCommand for ToggleFrei0rEffectCommand {
    fn execute(&self, project: &mut Project) {
        if let Some(track) = project.track_mut(&self.track_id) {
            if let Some(clip) = track.clips.iter_mut().find(|c| c.id == self.clip_id) {
                if let Some(effect) = clip
                    .frei0r_effects
                    .iter_mut()
                    .find(|e| e.id == self.effect_id)
                {
                    effect.enabled = !effect.enabled;
                }
            }
        }
        project.dirty = true;
    }
    fn undo(&self, project: &mut Project) {
        // Toggling again reverses the operation.
        self.execute(project);
    }
    fn description(&self) -> &str {
        "Toggle frei0r effect"
    }
}

/// Snapshot of all title-related properties for undo/redo.
#[derive(Clone, Debug)]
pub struct TitlePropertySnapshot {
    pub title_text: String,
    pub title_font: String,
    pub title_color: u32,
    pub title_x: f64,
    pub title_y: f64,
    pub title_template: String,
    pub title_outline_color: u32,
    pub title_outline_width: f64,
    pub title_shadow: bool,
    pub title_shadow_color: u32,
    pub title_shadow_offset_x: f64,
    pub title_shadow_offset_y: f64,
    pub title_bg_box: bool,
    pub title_bg_box_color: u32,
    pub title_bg_box_padding: f64,
    pub title_clip_bg_color: u32,
    pub title_secondary_text: String,
}

impl TitlePropertySnapshot {
    pub fn from_clip(clip: &Clip) -> Self {
        Self {
            title_text: clip.title_text.clone(),
            title_font: clip.title_font.clone(),
            title_color: clip.title_color,
            title_x: clip.title_x,
            title_y: clip.title_y,
            title_template: clip.title_template.clone(),
            title_outline_color: clip.title_outline_color,
            title_outline_width: clip.title_outline_width,
            title_shadow: clip.title_shadow,
            title_shadow_color: clip.title_shadow_color,
            title_shadow_offset_x: clip.title_shadow_offset_x,
            title_shadow_offset_y: clip.title_shadow_offset_y,
            title_bg_box: clip.title_bg_box,
            title_bg_box_color: clip.title_bg_box_color,
            title_bg_box_padding: clip.title_bg_box_padding,
            title_clip_bg_color: clip.title_clip_bg_color,
            title_secondary_text: clip.title_secondary_text.clone(),
        }
    }

    fn apply_to_clip(&self, clip: &mut Clip) {
        clip.title_text = self.title_text.clone();
        clip.title_font = self.title_font.clone();
        clip.title_color = self.title_color;
        clip.title_x = self.title_x;
        clip.title_y = self.title_y;
        clip.title_template = self.title_template.clone();
        clip.title_outline_color = self.title_outline_color;
        clip.title_outline_width = self.title_outline_width;
        clip.title_shadow = self.title_shadow;
        clip.title_shadow_color = self.title_shadow_color;
        clip.title_shadow_offset_x = self.title_shadow_offset_x;
        clip.title_shadow_offset_y = self.title_shadow_offset_y;
        clip.title_bg_box = self.title_bg_box;
        clip.title_bg_box_color = self.title_bg_box_color;
        clip.title_bg_box_padding = self.title_bg_box_padding;
        clip.title_clip_bg_color = self.title_clip_bg_color;
        clip.title_secondary_text = self.title_secondary_text.clone();
    }
}

pub struct SetTitlePropertiesCommand {
    pub clip_id: String,
    pub before: TitlePropertySnapshot,
    pub after: TitlePropertySnapshot,
}

impl EditCommand for SetTitlePropertiesCommand {
    fn execute(&self, project: &mut Project) {
        if let Some(clip) = project
            .tracks
            .iter_mut()
            .flat_map(|t| t.clips.iter_mut())
            .find(|c| c.id == self.clip_id)
        {
            self.after.apply_to_clip(clip);
        }
        project.dirty = true;
    }
    fn undo(&self, project: &mut Project) {
        if let Some(clip) = project
            .tracks
            .iter_mut()
            .flat_map(|t| t.clips.iter_mut())
            .find(|c| c.id == self.clip_id)
        {
            self.before.apply_to_clip(clip);
        }
        project.dirty = true;
    }
    fn description(&self) -> &str {
        "Set title properties"
    }
}

/// Add an adjustment layer clip to a track (undo removes it).
pub struct AddAdjustmentLayerCommand {
    pub clip: Clip,
    pub track_id: String,
}

impl EditCommand for AddAdjustmentLayerCommand {
    fn execute(&self, project: &mut Project) {
        if let Some(track) = project.track_mut(&self.track_id) {
            track.add_clip(self.clip.clone());
        }
        project.dirty = true;
    }
    fn undo(&self, project: &mut Project) {
        if let Some(track) = project.track_mut(&self.track_id) {
            track.remove_clip(&self.clip.id);
        }
        project.dirty = true;
    }
    fn description(&self) -> &str {
        "Add adjustment layer"
    }
}

/// Add a clip to a track (undo removes it).
pub struct AddClipCommand {
    pub clip: Clip,
    pub track_id: String,
}

impl EditCommand for AddClipCommand {
    fn execute(&self, project: &mut Project) {
        if let Some(track) = project.track_mut(&self.track_id) {
            track.add_clip(self.clip.clone());
        }
        project.dirty = true;
    }
    fn undo(&self, project: &mut Project) {
        if let Some(track) = project.track_mut(&self.track_id) {
            track.remove_clip(&self.clip.id);
        }
        project.dirty = true;
    }
    fn description(&self) -> &str {
        "Add clip"
    }
}

/// Set drawing items on a clip.
pub struct SetDrawingItemsCommand {
    pub clip_id: String,
    pub old_items: Vec<crate::model::clip::DrawingItem>,
    pub new_items: Vec<crate::model::clip::DrawingItem>,
}

impl EditCommand for SetDrawingItemsCommand {
    fn execute(&self, project: &mut Project) {
        if let Some(clip) = project.clip_mut(&self.clip_id) {
            clip.drawing_items = self.new_items.clone();
        }
        project.dirty = true;
    }
    fn undo(&self, project: &mut Project) {
        if let Some(clip) = project.clip_mut(&self.clip_id) {
            clip.drawing_items = self.old_items.clone();
        }
        project.dirty = true;
    }
    fn description(&self) -> &str {
        "Draw item"
    }
}

/// Set title animation on a clip.
pub struct SetTitleAnimationCommand {
    pub clip_id: String,
    pub old_animation: crate::model::clip::TitleAnimation,
    pub new_animation: crate::model::clip::TitleAnimation,
}

impl EditCommand for SetTitleAnimationCommand {
    fn execute(&self, project: &mut Project) {
        if let Some(clip) = project.clip_mut(&self.clip_id) {
            clip.title_animation = self.new_animation;
        }
        project.dirty = true;
    }
    fn undo(&self, project: &mut Project) {
        if let Some(clip) = project.clip_mut(&self.clip_id) {
            clip.title_animation = self.old_animation;
        }
        project.dirty = true;
    }
    fn description(&self) -> &str {
        "Set title animation"
    }
}

/// Set title animation duration on a clip.
pub struct SetTitleAnimationDurationCommand {
    pub clip_id: String,
    pub old_duration_ns: u64,
    pub new_duration_ns: u64,
}

impl EditCommand for SetTitleAnimationDurationCommand {
    fn execute(&self, project: &mut Project) {
        if let Some(clip) = project.clip_mut(&self.clip_id) {
            clip.title_animation_duration_ns = self.new_duration_ns;
        }
        project.dirty = true;
    }
    fn undo(&self, project: &mut Project) {
        if let Some(clip) = project.clip_mut(&self.clip_id) {
            clip.title_animation_duration_ns = self.old_duration_ns;
        }
        project.dirty = true;
    }
    fn description(&self) -> &str {
        "Set animation duration"
    }
}

/// Snapshot of a clip's mask state for undo/redo.
#[derive(Clone, Debug)]
pub struct ClipMaskSnapshot {
    pub masks: Vec<crate::model::clip::ClipMask>,
}

impl ClipMaskSnapshot {
    pub fn from_clip(clip: &crate::model::clip::Clip) -> Self {
        Self {
            masks: clip.masks.clone(),
        }
    }
    fn apply_to(&self, clip: &mut crate::model::clip::Clip) {
        clip.masks = self.masks.clone();
    }
}

/// Set clip mask properties (full snapshot replace).
pub struct SetClipMaskCommand {
    pub clip_id: String,
    pub track_id: String,
    pub old_mask: ClipMaskSnapshot,
    pub new_mask: ClipMaskSnapshot,
}

impl EditCommand for SetClipMaskCommand {
    fn execute(&self, project: &mut Project) {
        if let Some(track) = project.track_mut(&self.track_id) {
            if let Some(clip) = track.clips.iter_mut().find(|c| c.id == self.clip_id) {
                self.new_mask.apply_to(clip);
            }
        }
        project.dirty = true;
    }
    fn undo(&self, project: &mut Project) {
        if let Some(track) = project.track_mut(&self.track_id) {
            if let Some(clip) = track.clips.iter_mut().find(|c| c.id == self.clip_id) {
                self.old_mask.apply_to(clip);
            }
        }
        project.dirty = true;
    }
    fn description(&self) -> &str {
        "Set clip mask"
    }
}

/// Replace a clip's HSL qualifier (secondary color correction) with a full
/// snapshot. Undo restores the previous qualifier. `None` means the clip has
/// no qualifier at all.
pub struct SetClipHslQualifierCommand {
    pub clip_id: String,
    pub track_id: String,
    pub old: Option<crate::model::clip::HslQualifier>,
    pub new: Option<crate::model::clip::HslQualifier>,
}

impl EditCommand for SetClipHslQualifierCommand {
    fn execute(&self, project: &mut Project) {
        if let Some(clip) = find_clip_mut(project, &self.clip_id, &self.track_id) {
            clip.hsl_qualifier = self.new.clone();
        }
        project.dirty = true;
    }
    fn undo(&self, project: &mut Project) {
        if let Some(clip) = find_clip_mut(project, &self.clip_id, &self.track_id) {
            clip.hsl_qualifier = self.old.clone();
        }
        project.dirty = true;
    }
    fn description(&self) -> &str {
        "Set HSL qualifier"
    }
}

/// Set the project's master audio gain (dB). Applied post-mixdown in both
/// preview and export. Clamped to ±24 dB on execute. Used by the Loudness
/// Radar "Normalize to Target" and "Reset Gain" actions.
pub struct SetProjectMasterGainCommand {
    pub old_db: f64,
    pub new_db: f64,
}

impl EditCommand for SetProjectMasterGainCommand {
    fn execute(&self, project: &mut Project) {
        project.master_gain_db = self.new_db.clamp(-24.0, 24.0);
        project.dirty = true;
    }
    fn undo(&self, project: &mut Project) {
        project.master_gain_db = self.old_db.clamp(-24.0, 24.0);
        project.dirty = true;
    }
    fn description(&self) -> &str {
        "Set project master gain"
    }
}

// ── Audio bus commands ────────────────────────────────────────────────────

/// Set the gain (dB) of a role-based audio bus.
pub struct SetBusGainCommand {
    pub role: AudioRole,
    pub old_db: f64,
    pub new_db: f64,
}

impl EditCommand for SetBusGainCommand {
    fn execute(&self, project: &mut Project) {
        if let Some(bus) = project.bus_for_role_mut(&self.role) {
            bus.gain_db = self.new_db.clamp(-96.0, 24.0);
        }
        project.dirty = true;
    }
    fn undo(&self, project: &mut Project) {
        if let Some(bus) = project.bus_for_role_mut(&self.role) {
            bus.gain_db = self.old_db.clamp(-96.0, 24.0);
        }
        project.dirty = true;
    }
    fn description(&self) -> &str {
        "Set bus gain"
    }
}

/// Toggle mute on a role-based audio bus.
pub struct SetBusMuteCommand {
    pub role: AudioRole,
    pub old_muted: bool,
    pub new_muted: bool,
}

impl EditCommand for SetBusMuteCommand {
    fn execute(&self, project: &mut Project) {
        if let Some(bus) = project.bus_for_role_mut(&self.role) {
            bus.muted = self.new_muted;
        }
        project.dirty = true;
    }
    fn undo(&self, project: &mut Project) {
        if let Some(bus) = project.bus_for_role_mut(&self.role) {
            bus.muted = self.old_muted;
        }
        project.dirty = true;
    }
    fn description(&self) -> &str {
        "Set bus mute"
    }
}

/// Toggle solo on a role-based audio bus.
pub struct SetBusSoloCommand {
    pub role: AudioRole,
    pub old_soloed: bool,
    pub new_soloed: bool,
}

impl EditCommand for SetBusSoloCommand {
    fn execute(&self, project: &mut Project) {
        if let Some(bus) = project.bus_for_role_mut(&self.role) {
            bus.soloed = self.new_soloed;
        }
        project.dirty = true;
    }
    fn undo(&self, project: &mut Project) {
        if let Some(bus) = project.bus_for_role_mut(&self.role) {
            bus.soloed = self.old_soloed;
        }
        project.dirty = true;
    }
    fn description(&self) -> &str {
        "Set bus solo"
    }
}

/// Helper to create a `SetBusGainCommand`.
pub fn set_bus_gain_cmd(role: AudioRole, old_db: f64, new_db: f64) -> SetBusGainCommand {
    SetBusGainCommand {
        role,
        old_db,
        new_db,
    }
}

/// Helper to create a `SetBusMuteCommand`.
pub fn set_bus_mute_cmd(role: AudioRole, old_muted: bool, new_muted: bool) -> SetBusMuteCommand {
    SetBusMuteCommand {
        role,
        old_muted,
        new_muted,
    }
}

/// Helper to create a `SetBusSoloCommand`.
pub fn set_bus_solo_cmd(role: AudioRole, old_soloed: bool, new_soloed: bool) -> SetBusSoloCommand {
    SetBusSoloCommand {
        role,
        old_soloed,
        new_soloed,
    }
}

// ── Subtitle commands ─────────────────────────────────────────────────────

/// Set (or replace) all subtitle segments on a clip (used after STT generation).
pub struct GenerateSubtitlesCommand {
    pub clip_id: String,
    pub track_id: String,
    pub old_segments: Vec<crate::model::clip::SubtitleSegment>,
    pub new_segments: Vec<crate::model::clip::SubtitleSegment>,
}

impl EditCommand for GenerateSubtitlesCommand {
    fn execute(&self, project: &mut Project) {
        if let Some(clip) = find_clip_mut(project, &self.clip_id, &self.track_id) {
            clip.subtitle_segments = self.new_segments.clone();
        }
        project.dirty = true;
    }
    fn undo(&self, project: &mut Project) {
        if let Some(clip) = find_clip_mut(project, &self.clip_id, &self.track_id) {
            clip.subtitle_segments = self.old_segments.clone();
        }
        project.dirty = true;
    }
    fn description(&self) -> &str {
        "Generate subtitles"
    }
}

/// Edit the text of a single subtitle segment.
pub struct EditSubtitleTextCommand {
    pub clip_id: String,
    pub track_id: String,
    pub segment_id: String,
    pub old_text: String,
    pub new_text: String,
}

impl EditCommand for EditSubtitleTextCommand {
    fn execute(&self, project: &mut Project) {
        if let Some(clip) = find_clip_mut(project, &self.clip_id, &self.track_id) {
            if let Some(seg) = clip
                .subtitle_segments
                .iter_mut()
                .find(|s| s.id == self.segment_id)
            {
                // Re-sync per-word entries so karaoke / word highlight
                // rendering uses the edited text instead of the original
                // Whisper tokens.
                seg.set_text_and_resync_words(self.new_text.clone());
            }
        }
        project.dirty = true;
    }
    fn undo(&self, project: &mut Project) {
        if let Some(clip) = find_clip_mut(project, &self.clip_id, &self.track_id) {
            if let Some(seg) = clip
                .subtitle_segments
                .iter_mut()
                .find(|s| s.id == self.segment_id)
            {
                seg.set_text_and_resync_words(self.old_text.clone());
            }
        }
        project.dirty = true;
    }
    fn description(&self) -> &str {
        "Edit subtitle text"
    }
}

/// Edit the timing of a single subtitle segment.
pub struct EditSubtitleTimingCommand {
    pub clip_id: String,
    pub track_id: String,
    pub segment_id: String,
    pub old_start_ns: u64,
    pub old_end_ns: u64,
    pub new_start_ns: u64,
    pub new_end_ns: u64,
}

impl EditCommand for EditSubtitleTimingCommand {
    fn execute(&self, project: &mut Project) {
        if let Some(clip) = find_clip_mut(project, &self.clip_id, &self.track_id) {
            if let Some(seg) = clip
                .subtitle_segments
                .iter_mut()
                .find(|s| s.id == self.segment_id)
            {
                seg.start_ns = self.new_start_ns;
                seg.end_ns = self.new_end_ns;
            }
        }
        project.dirty = true;
    }
    fn undo(&self, project: &mut Project) {
        if let Some(clip) = find_clip_mut(project, &self.clip_id, &self.track_id) {
            if let Some(seg) = clip
                .subtitle_segments
                .iter_mut()
                .find(|s| s.id == self.segment_id)
            {
                seg.start_ns = self.old_start_ns;
                seg.end_ns = self.old_end_ns;
            }
        }
        project.dirty = true;
    }
    fn description(&self) -> &str {
        "Edit subtitle timing"
    }
}

/// Clear all subtitle segments from a clip.
pub struct ClearSubtitlesCommand {
    pub clip_id: String,
    pub track_id: String,
    pub old_segments: Vec<crate::model::clip::SubtitleSegment>,
}

impl EditCommand for ClearSubtitlesCommand {
    fn execute(&self, project: &mut Project) {
        if let Some(clip) = find_clip_mut(project, &self.clip_id, &self.track_id) {
            clip.subtitle_segments.clear();
        }
        project.dirty = true;
    }
    fn undo(&self, project: &mut Project) {
        if let Some(clip) = find_clip_mut(project, &self.clip_id, &self.track_id) {
            clip.subtitle_segments = self.old_segments.clone();
        }
        project.dirty = true;
    }
    fn description(&self) -> &str {
        "Clear subtitles"
    }
}

/// Delete a single subtitle segment from a clip.
pub struct DeleteSubtitleSegmentCommand {
    pub clip_id: String,
    pub track_id: String,
    pub segment_id: String,
    pub deleted_segment: crate::model::clip::SubtitleSegment,
    pub index: usize,
}

impl EditCommand for DeleteSubtitleSegmentCommand {
    fn execute(&self, project: &mut Project) {
        if let Some(clip) = find_clip_mut(project, &self.clip_id, &self.track_id) {
            clip.subtitle_segments.retain(|s| s.id != self.segment_id);
        }
        project.dirty = true;
    }
    fn undo(&self, project: &mut Project) {
        if let Some(clip) = find_clip_mut(project, &self.clip_id, &self.track_id) {
            let idx = self.index.min(clip.subtitle_segments.len());
            clip.subtitle_segments
                .insert(idx, self.deleted_segment.clone());
        }
        project.dirty = true;
    }
    fn description(&self) -> &str {
        "Delete subtitle segment"
    }
}

/// Set a subtitle style property on a clip.
pub struct SetSubtitleStyleCommand {
    pub clip_id: String,
    pub track_id: String,
    pub old_font: String,
    pub new_font: String,
    pub old_color: u32,
    pub new_color: u32,
    pub old_outline_color: u32,
    pub new_outline_color: u32,
    pub old_outline_width: f64,
    pub new_outline_width: f64,
    pub old_bg_box: bool,
    pub new_bg_box: bool,
    pub old_bg_box_color: u32,
    pub new_bg_box_color: u32,
    pub old_highlight_mode: crate::model::clip::SubtitleHighlightMode,
    pub new_highlight_mode: crate::model::clip::SubtitleHighlightMode,
    pub old_highlight_color: u32,
    pub new_highlight_color: u32,
}

impl EditCommand for SetSubtitleStyleCommand {
    fn execute(&self, project: &mut Project) {
        if let Some(clip) = find_clip_mut(project, &self.clip_id, &self.track_id) {
            clip.subtitle_font = self.new_font.clone();
            clip.subtitle_color = self.new_color;
            clip.subtitle_outline_color = self.new_outline_color;
            clip.subtitle_outline_width = self.new_outline_width;
            clip.subtitle_bg_box = self.new_bg_box;
            clip.subtitle_bg_box_color = self.new_bg_box_color;
            clip.subtitle_highlight_mode = self.new_highlight_mode;
            clip.subtitle_highlight_color = self.new_highlight_color;
        }
        project.dirty = true;
    }
    fn undo(&self, project: &mut Project) {
        if let Some(clip) = find_clip_mut(project, &self.clip_id, &self.track_id) {
            clip.subtitle_font = self.old_font.clone();
            clip.subtitle_color = self.old_color;
            clip.subtitle_outline_color = self.old_outline_color;
            clip.subtitle_outline_width = self.old_outline_width;
            clip.subtitle_bg_box = self.old_bg_box;
            clip.subtitle_bg_box_color = self.old_bg_box_color;
            clip.subtitle_highlight_mode = self.old_highlight_mode;
            clip.subtitle_highlight_color = self.old_highlight_color;
        }
        project.dirty = true;
    }
    fn description(&self) -> &str {
        "Set subtitle style"
    }
}

//// Helper: find a mutable clip reference by clip_id and track_id.
/// Uses recursive `clip_mut` so clips inside compound tracks are found
/// regardless of whether `track_id` is valid.
fn find_clip_mut<'a>(
    project: &'a mut Project,
    clip_id: &str,
    _track_id: &str,
) -> Option<&'a mut Clip> {
    project.clip_mut(clip_id)
}

// ─── Audition / clip-versions commands ────────────────────────────────────
//
// Create/Finalize audition use the existing `SetMultipleTracksClipsCommand`
// (whole-track snapshot) at the call site, since they replace clips in
// place. The three commands below cover *in-place* mutations of an existing
// audition clip — switching the active take, adding a take, removing a
// take — using full-clip snapshots so undo restores any field tweaks made
// while a different take was active.

/// Switch the currently active audition take. Snapshots the entire clip
/// before mutation so undo restores both the index and any tweaks the user
/// made to the host fields while the previous take was active.
pub struct SetActiveAuditionTakeCommand {
    pub clip_id: String,
    pub new_index: usize,
    pub before_snapshot: Option<Clip>,
}

impl EditCommand for SetActiveAuditionTakeCommand {
    fn execute(&self, project: &mut Project) {
        if let Some(clip) = project.clip_mut(&self.clip_id) {
            clip.set_active_audition_take(self.new_index);
        }
        project.dirty = true;
    }
    fn undo(&self, project: &mut Project) {
        if let (Some(clip), Some(snap)) = (
            project.clip_mut(&self.clip_id),
            self.before_snapshot.as_ref(),
        ) {
            *clip = snap.clone();
        }
        project.dirty = true;
    }
    fn description(&self) -> &str {
        "Switch audition take"
    }
}

/// Append a new take to an audition clip.
pub struct AddAuditionTakeCommand {
    pub clip_id: String,
    pub take: AuditionTake,
}

impl EditCommand for AddAuditionTakeCommand {
    fn execute(&self, project: &mut Project) {
        if let Some(clip) = project.clip_mut(&self.clip_id) {
            clip.add_audition_take(self.take.clone());
        }
        project.dirty = true;
    }
    fn undo(&self, project: &mut Project) {
        if let Some(clip) = project.clip_mut(&self.clip_id) {
            if let Some(takes) = clip.audition_takes.as_mut() {
                takes.pop();
            }
        }
        project.dirty = true;
    }
    fn description(&self) -> &str {
        "Add audition take"
    }
}

/// Remove a take from an audition clip. Refuses to remove the active take.
/// Stores the removed take in the command so undo can reinsert it.
pub struct RemoveAuditionTakeCommand {
    pub clip_id: String,
    pub take_index: usize,
    pub removed: std::cell::RefCell<Option<AuditionTake>>,
}

impl EditCommand for RemoveAuditionTakeCommand {
    fn execute(&self, project: &mut Project) {
        if let Some(clip) = project.clip_mut(&self.clip_id) {
            *self.removed.borrow_mut() = clip.remove_audition_take(self.take_index);
        }
        project.dirty = true;
    }
    fn undo(&self, project: &mut Project) {
        if let Some(clip) = project.clip_mut(&self.clip_id) {
            if let Some(take) = self.removed.borrow_mut().take() {
                if let Some(takes) = clip.audition_takes.as_mut() {
                    let insert_at = self.take_index.min(takes.len());
                    takes.insert(insert_at, take);
                    if insert_at <= clip.audition_active_take_index {
                        clip.audition_active_take_index += 1;
                    }
                }
            }
        }
        project.dirty = true;
    }
    fn description(&self) -> &str {
        "Remove audition take"
    }
}

/// Collapse an audition clip back to a normal clip referencing only the
/// currently active take. Snapshots the full clip so undo can restore the
/// audition wrapper and all alternate takes.
pub struct FinalizeAuditionCommand {
    pub clip_id: String,
    pub before_snapshot: Option<Clip>,
}

impl EditCommand for FinalizeAuditionCommand {
    fn execute(&self, project: &mut Project) {
        if let Some(clip) = project.clip_mut(&self.clip_id) {
            clip.finalize_audition();
        }
        project.dirty = true;
    }
    fn undo(&self, project: &mut Project) {
        if let (Some(clip), Some(snap)) = (
            project.clip_mut(&self.clip_id),
            self.before_snapshot.as_ref(),
        ) {
            *clip = snap.clone();
        }
        project.dirty = true;
    }
    fn description(&self) -> &str {
        "Finalize audition"
    }
}

/// Undo command for script-to-timeline assembly.
///
/// Captures the full track state before and after assembly so the
/// entire operation can be reverted atomically.
pub struct ScriptAssemblyCommand {
    /// All tracks before assembly.
    pub old_tracks: Vec<Track>,
    /// All tracks after assembly.
    pub new_tracks: Vec<Track>,
    pub label: String,
}

impl EditCommand for ScriptAssemblyCommand {
    fn execute(&self, project: &mut Project) {
        project.tracks = self.new_tracks.clone();
        project.dirty = true;
    }
    fn undo(&self, project: &mut Project) {
        project.tracks = self.old_tracks.clone();
        project.dirty = true;
    }
    fn description(&self) -> &str {
        &self.label
    }
}

// -----------------------------------------------------------------------
// Marker commands
// -----------------------------------------------------------------------

pub struct AddMarkerCommand {
    pub marker: crate::model::project::Marker,
}

impl EditCommand for AddMarkerCommand {
    fn execute(&self, project: &mut Project) {
        project.markers.push(self.marker.clone());
        project.markers.sort_by_key(|m| m.position_ns);
        project.dirty = true;
    }
    fn undo(&self, project: &mut Project) {
        project.markers.retain(|m| m.id != self.marker.id);
        project.dirty = true;
    }
    fn description(&self) -> &str {
        "Add marker"
    }
}

pub struct RemoveMarkerCommand {
    pub marker: crate::model::project::Marker,
}

impl EditCommand for RemoveMarkerCommand {
    fn execute(&self, project: &mut Project) {
        project.markers.retain(|m| m.id != self.marker.id);
        project.dirty = true;
    }
    fn undo(&self, project: &mut Project) {
        project.markers.push(self.marker.clone());
        project.markers.sort_by_key(|m| m.position_ns);
        project.dirty = true;
    }
    fn description(&self) -> &str {
        "Remove marker"
    }
}

pub struct EditMarkerCommand {
    pub marker_id: String,
    pub old_state: crate::model::project::Marker,
    pub new_state: crate::model::project::Marker,
}

impl EditCommand for EditMarkerCommand {
    fn execute(&self, project: &mut Project) {
        if let Some(m) = project.markers.iter_mut().find(|m| m.id == self.marker_id) {
            m.label = self.new_state.label.clone();
            m.color = self.new_state.color;
            m.notes = self.new_state.notes.clone();
            m.position_ns = self.new_state.position_ns;
        }
        project.markers.sort_by_key(|m| m.position_ns);
        project.dirty = true;
    }
    fn undo(&self, project: &mut Project) {
        if let Some(m) = project.markers.iter_mut().find(|m| m.id == self.marker_id) {
            m.label = self.old_state.label.clone();
            m.color = self.old_state.color;
            m.notes = self.old_state.notes.clone();
            m.position_ns = self.old_state.position_ns;
        }
        project.markers.sort_by_key(|m| m.position_ns);
        project.dirty = true;
    }
    fn description(&self) -> &str {
        "Edit marker"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::clip::{Clip, ClipKind};
    use crate::model::project::Project;
    use crate::model::track::Track;

    #[test]
    fn auto_crop_command_round_trip() {
        use crate::model::clip::{MotionTracker, TrackingBinding, TrackingRegion};

        let mut project = Project::new("Auto-Crop Test");
        let mut track = Track::new_video("Video");
        let mut clip = Clip::new("/tmp/a.mp4", 5_000_000_000, 0, ClipKind::Video);
        clip.id = "clip-1".to_string();
        // Start with one existing tracker and no binding.
        let mut tracker = MotionTracker::new("Subject");
        tracker.id = "tracker-1".to_string();
        tracker.analysis_region = TrackingRegion {
            center_x: 0.5,
            center_y: 0.5,
            width: 0.25,
            height: 0.25,
            rotation_deg: 0.0,
        };
        clip.motion_trackers.push(tracker.clone());
        track.add_clip(clip);
        project.tracks.push(track);

        let old_tracking_binding = None;
        let old_motion_trackers = vec![tracker.clone()];
        let old_first_mask_binding = None; // no mask on the clip

        // New state: auto-crop binding installed + updated tracker (could
        // have samples from a cache hit, but for test purposes just reuse).
        let new_binding = TrackingBinding {
            source_clip_id: "clip-1".to_string(),
            tracker_id: "tracker-1".to_string(),
            apply_translation: true,
            apply_scale: true,
            apply_rotation: false,
            offset_x: 0.0,
            offset_y: 0.0,
            scale_multiplier: 1.818,
            rotation_offset_deg: 0.0,
            strength: 1.0,
            smoothing: 0.0,
        };

        let cmd = SetClipAutoCropCommand {
            clip_id: "clip-1".to_string(),
            old_tracking_binding,
            old_motion_trackers,
            old_first_mask_binding,
            new_tracking_binding: Some(new_binding.clone()),
            new_motion_trackers: vec![tracker.clone()],
            new_first_mask_binding: None,
        };

        cmd.execute(&mut project);
        let applied = project.clip_ref("clip-1").unwrap();
        assert!(applied.tracking_binding.is_some());
        assert!((applied.tracking_binding.as_ref().unwrap().scale_multiplier - 1.818).abs() < 1e-9);
        assert!(project.dirty);

        project.dirty = false;
        cmd.undo(&mut project);
        let undone = project.clip_ref("clip-1").unwrap();
        assert!(undone.tracking_binding.is_none());
        assert_eq!(undone.motion_trackers.len(), 1);
        assert!(project.dirty);
    }

    #[test]
    fn master_gain_command_round_trip() {
        let mut project = Project::new("Loudness Test");
        assert_eq!(project.master_gain_db, 0.0);

        let cmd = SetProjectMasterGainCommand {
            old_db: 0.0,
            new_db: -3.5,
        };
        cmd.execute(&mut project);
        assert!((project.master_gain_db + 3.5).abs() < 1e-9);
        assert!(project.dirty);

        project.dirty = false;
        cmd.undo(&mut project);
        assert_eq!(project.master_gain_db, 0.0);
        assert!(project.dirty);
    }

    #[test]
    fn master_gain_command_clamps_to_plus_minus_24() {
        let mut project = Project::new("Loudness Test");
        let cmd = SetProjectMasterGainCommand {
            old_db: 0.0,
            new_db: 99.0,
        };
        cmd.execute(&mut project);
        assert_eq!(project.master_gain_db, 24.0, "upper clamp");

        let cmd2 = SetProjectMasterGainCommand {
            old_db: 0.0,
            new_db: -99.0,
        };
        cmd2.execute(&mut project);
        assert_eq!(project.master_gain_db, -24.0, "lower clamp");
    }

    #[test]
    fn master_gain_linear_matches_decibel_formula() {
        let mut project = Project::new("Gain Test");
        project.master_gain_db = 0.0;
        assert!((project.master_gain_linear() - 1.0).abs() < 1e-9);
        project.master_gain_db = 6.0;
        // 10^(6/20) ≈ 1.9953
        assert!((project.master_gain_linear() - 10.0_f64.powf(0.3)).abs() < 1e-9);
        project.master_gain_db = -20.0;
        assert!((project.master_gain_linear() - 0.1).abs() < 1e-9);
    }

    #[test]
    fn test_ripple_trim_out() {
        let mut project = Project::new("Test Project");
        let mut track = Track::new_video("Video Track");
        let track_id = track.id.clone();

        // Clip A: 0..10
        let mut clip_a = Clip::new("file1", 10, 0, ClipKind::Video);
        clip_a.id = "A".to_string();
        track.add_clip(clip_a);

        // Clip B: 15..25
        let mut clip_b = Clip::new("file2", 10, 15, ClipKind::Video);
        clip_b.id = "B".to_string();
        track.add_clip(clip_b);

        project.tracks.push(track);

        // Ripple Trim Out: Shorten A by 2 (10 -> 8). Delta = -2.
        // B should shift by -2 (15 -> 13).
        let cmd = RippleTrimOutCommand {
            clip_id: "A".to_string(),
            track_id: track_id.clone(),
            old_source_out: 10,
            new_source_out: 8,
            delta: -2,
        };

        cmd.execute(&mut project);

        let track = project.tracks.iter().find(|t| t.id == track_id).unwrap();
        let a = track.clips.iter().find(|c| c.id == "A").unwrap();
        let b = track.clips.iter().find(|c| c.id == "B").unwrap();

        assert_eq!(a.source_out, 8);
        assert_eq!(a.timeline_end(), 8);
        assert_eq!(b.timeline_start, 13);

        // Undo
        cmd.undo(&mut project);

        let track = project.tracks.iter().find(|t| t.id == track_id).unwrap();
        let a = track.clips.iter().find(|c| c.id == "A").unwrap();
        let b = track.clips.iter().find(|c| c.id == "B").unwrap();

        assert_eq!(a.source_out, 10);
        assert_eq!(a.timeline_end(), 10);
        assert_eq!(b.timeline_start, 15);
    }

    #[test]
    fn test_ripple_trim_out_extend() {
        let mut project = Project::new("Test Project");
        let mut track = Track::new_video("Video Track");
        let track_id = track.id.clone();

        // Clip A: 0..10
        let mut clip_a = Clip::new("file1", 10, 0, ClipKind::Video);
        clip_a.id = "A".to_string();
        track.add_clip(clip_a);

        // Clip B: 11..21 (gap of 1)
        let mut clip_b = Clip::new("file2", 10, 11, ClipKind::Video);
        clip_b.id = "B".to_string();
        track.add_clip(clip_b);

        project.tracks.push(track);

        // Ripple Trim Out: Extend A by 2 (10 -> 12). Delta = +2.
        // B should shift by +2 (11 -> 13).
        let cmd = RippleTrimOutCommand {
            clip_id: "A".to_string(),
            track_id: track_id.clone(),
            old_source_out: 10,
            new_source_out: 12,
            delta: 2,
        };

        cmd.execute(&mut project);

        let track = project.tracks.iter().find(|t| t.id == track_id).unwrap();
        let a = track.clips.iter().find(|c| c.id == "A").unwrap();
        let b = track.clips.iter().find(|c| c.id == "B").unwrap();

        assert_eq!(a.source_out, 12);
        assert_eq!(a.timeline_end(), 12);
        assert_eq!(b.timeline_start, 13);

        // Undo
        cmd.undo(&mut project);

        let track = project.tracks.iter().find(|t| t.id == track_id).unwrap();
        let a = track.clips.iter().find(|c| c.id == "A").unwrap();
        let b = track.clips.iter().find(|c| c.id == "B").unwrap();

        assert_eq!(a.source_out, 10);
        assert_eq!(a.timeline_end(), 10);
        assert_eq!(b.timeline_start, 11);
    }

    fn make_project_with_clip(
        clip_id: &str,
        source_out: u64,
        timeline_start: u64,
    ) -> (Project, String, String) {
        let mut project = Project::new("Test");
        let mut track = Track::new_video("V1");
        let track_id = track.id.clone();
        let mut clip = Clip::new("file.mp4", source_out, timeline_start, ClipKind::Video);
        clip.id = clip_id.to_string();
        track.add_clip(clip);
        project.tracks.push(track);
        (project, track_id, clip_id.to_string())
    }

    #[test]
    fn test_move_clip_command() {
        let mut project = Project::new("Test");
        let mut track_a = Track::new_video("A");
        let track_a_id = track_a.id.clone();
        let track_b = Track::new_video("B");
        let track_b_id = track_b.id.clone();

        let mut clip = Clip::new("file.mp4", 10, 0, ClipKind::Video);
        clip.id = "C".to_string();
        track_a.add_clip(clip);
        project.tracks.push(track_a);
        project.tracks.push(track_b);

        let cmd = MoveClipCommand {
            clip_id: "C".to_string(),
            from_track_id: track_a_id.clone(),
            to_track_id: track_b_id.clone(),
            old_timeline_start: 0,
            new_timeline_start: 5,
        };

        cmd.execute(&mut project);
        let ta = project.tracks.iter().find(|t| t.id == track_a_id).unwrap();
        let tb = project.tracks.iter().find(|t| t.id == track_b_id).unwrap();
        assert!(ta.clips.is_empty());
        assert_eq!(tb.clips[0].timeline_start, 5);

        cmd.undo(&mut project);
        let ta = project.tracks.iter().find(|t| t.id == track_a_id).unwrap();
        let tb = project.tracks.iter().find(|t| t.id == track_b_id).unwrap();
        assert_eq!(ta.clips[0].timeline_start, 0);
        assert!(tb.clips.is_empty());
    }

    #[test]
    fn match_clip_audio_command_updates_and_restores_volume_loudness_and_eq() {
        let mut project = Project::new("Test");
        let mut track = Track::new_audio("A1");
        let track_id = track.id.clone();
        let mut clip = Clip::new("voice.wav", 10, 0, ClipKind::Audio);
        clip.id = "A".to_string();
        clip.volume = 0.8;
        clip.measured_loudness_lufs = Some(-19.5);
        clip.eq_bands = crate::model::clip::default_eq_bands();
        track.add_clip(clip);
        project.tracks.push(track);

        let mut matched_eq = crate::model::clip::default_eq_bands();
        matched_eq[0].gain = -2.5;
        matched_eq[1].gain = 1.0;
        matched_eq[2].gain = 3.5;
        let cmd = MatchClipAudioCommand {
            clip_id: "A".to_string(),
            old_volume: 0.8,
            new_volume: 1.1,
            old_measured_loudness: Some(-19.5),
            new_measured_loudness: Some(-19.5),
            old_eq_bands: crate::model::clip::default_eq_bands(),
            new_eq_bands: matched_eq,
            old_match_eq_bands: Vec::new(),
            new_match_eq_bands: vec![
                crate::model::clip::EqBand {
                    freq: 100.0,
                    gain: -3.0,
                    q: 1.5,
                },
                crate::model::clip::EqBand {
                    freq: 200.0,
                    gain: -2.0,
                    q: 1.0,
                },
                crate::model::clip::EqBand {
                    freq: 400.0,
                    gain: 0.0,
                    q: 1.5,
                },
                crate::model::clip::EqBand {
                    freq: 800.0,
                    gain: 1.0,
                    q: 1.0,
                },
                crate::model::clip::EqBand {
                    freq: 2000.0,
                    gain: 3.0,
                    q: 1.0,
                },
                crate::model::clip::EqBand {
                    freq: 5000.0,
                    gain: 2.5,
                    q: 1.0,
                },
                crate::model::clip::EqBand {
                    freq: 9000.0,
                    gain: 0.0,
                    q: 1.5,
                },
            ],
        };

        cmd.execute(&mut project);
        let track = project.tracks.iter().find(|t| t.id == track_id).unwrap();
        let clip = track.clips.iter().find(|c| c.id == "A").unwrap();
        assert_eq!(clip.volume, 1.1);
        assert_eq!(clip.measured_loudness_lufs, Some(-19.5));
        assert_eq!(clip.eq_bands[0].gain, -2.5);
        assert_eq!(clip.eq_bands[1].gain, 1.0);
        assert_eq!(clip.eq_bands[2].gain, 3.5);
        assert_eq!(clip.match_eq_bands.len(), 7);
        assert_eq!(clip.match_eq_bands[4].gain, 3.0);

        cmd.undo(&mut project);
        let track = project.tracks.iter().find(|t| t.id == track_id).unwrap();
        let clip = track.clips.iter().find(|c| c.id == "A").unwrap();
        assert_eq!(clip.volume, 0.8);
        assert_eq!(clip.measured_loudness_lufs, Some(-19.5));
        assert_eq!(clip.eq_bands, crate::model::clip::default_eq_bands());
        assert!(clip.match_eq_bands.is_empty());
    }

    #[test]
    fn test_trim_clip_command() {
        let (mut project, track_id, clip_id) = make_project_with_clip("C", 10, 5);

        let cmd = TrimClipCommand {
            clip_id: clip_id.clone(),
            track_id: track_id.clone(),
            old_source_in: 0,
            new_source_in: 2,
            old_timeline_start: 5,
            new_timeline_start: 7,
        };

        cmd.execute(&mut project);
        let track = project.tracks.iter().find(|t| t.id == track_id).unwrap();
        let clip = track.clips.iter().find(|c| c.id == clip_id).unwrap();
        assert_eq!(clip.source_in, 2);
        assert_eq!(clip.timeline_start, 7);

        cmd.undo(&mut project);
        let track = project.tracks.iter().find(|t| t.id == track_id).unwrap();
        let clip = track.clips.iter().find(|c| c.id == clip_id).unwrap();
        assert_eq!(clip.source_in, 0);
        assert_eq!(clip.timeline_start, 5);
    }

    #[test]
    fn test_trim_out_command() {
        let (mut project, track_id, clip_id) = make_project_with_clip("C", 10, 0);

        let cmd = TrimOutCommand {
            clip_id: clip_id.clone(),
            track_id: track_id.clone(),
            old_source_out: 10,
            new_source_out: 8,
        };

        cmd.execute(&mut project);
        let track = project.tracks.iter().find(|t| t.id == track_id).unwrap();
        let clip = track.clips.iter().find(|c| c.id == clip_id).unwrap();
        assert_eq!(clip.source_out, 8);

        cmd.undo(&mut project);
        let track = project.tracks.iter().find(|t| t.id == track_id).unwrap();
        let clip = track.clips.iter().find(|c| c.id == clip_id).unwrap();
        assert_eq!(clip.source_out, 10);
    }

    #[test]
    fn test_delete_clip_command() {
        let (mut project, track_id, _) = make_project_with_clip("C", 10, 0);
        let clip_snapshot = {
            let track = project.tracks.iter().find(|t| t.id == track_id).unwrap();
            track.clips[0].clone()
        };

        let cmd = DeleteClipCommand {
            clip: clip_snapshot,
            track_id: track_id.clone(),
        };

        cmd.execute(&mut project);
        let track = project.tracks.iter().find(|t| t.id == track_id).unwrap();
        assert!(track.clips.is_empty());

        cmd.undo(&mut project);
        let track = project.tracks.iter().find(|t| t.id == track_id).unwrap();
        assert_eq!(track.clips.len(), 1);
        assert_eq!(track.clips[0].id, "C");
    }

    #[test]
    fn test_slip_clip_command() {
        let (mut project, track_id, clip_id) = make_project_with_clip("C", 10, 0);

        let cmd = SlipClipCommand {
            clip_id: clip_id.clone(),
            track_id: track_id.clone(),
            old_source_in: 0,
            old_source_out: 10,
            new_source_in: 2,
            new_source_out: 12,
        };

        cmd.execute(&mut project);
        let track = project.tracks.iter().find(|t| t.id == track_id).unwrap();
        let clip = track.clips.iter().find(|c| c.id == clip_id).unwrap();
        assert_eq!(clip.source_in, 2);
        assert_eq!(clip.source_out, 12);
        assert_eq!(clip.timeline_start, 0); // timeline position unchanged

        cmd.undo(&mut project);
        let track = project.tracks.iter().find(|t| t.id == track_id).unwrap();
        let clip = track.clips.iter().find(|c| c.id == clip_id).unwrap();
        assert_eq!(clip.source_in, 0);
        assert_eq!(clip.source_out, 10);
    }

    #[test]
    fn test_split_clip_command() {
        let (mut project, track_id, _) = make_project_with_clip("ORIG", 20, 0);
        let original_clip = {
            let track = project.tracks.iter().find(|t| t.id == track_id).unwrap();
            track.clips[0].clone()
        };

        let mut right_clip = Clip::new("file.mp4", 20, 10, ClipKind::Video);
        right_clip.id = "RIGHT".to_string();
        right_clip.source_in = 10;
        right_clip.source_out = 20;

        let cmd = SplitClipCommand {
            original_clip: original_clip.clone(),
            track_id: track_id.clone(),
            split_ns: 10,
            right_clip: right_clip.clone(),
        };

        cmd.execute(&mut project);
        let track = project.tracks.iter().find(|t| t.id == track_id).unwrap();
        assert_eq!(track.clips.len(), 2);
        let orig = track.clips.iter().find(|c| c.id == "ORIG").unwrap();
        assert_eq!(orig.source_out, 10); // trimmed to split point

        cmd.undo(&mut project);
        let track = project.tracks.iter().find(|t| t.id == track_id).unwrap();
        assert_eq!(track.clips.len(), 1);
        assert_eq!(track.clips[0].id, "ORIG");
        assert_eq!(track.clips[0].source_out, original_clip.source_out);
    }

    #[test]
    fn test_join_through_edit_command() {
        let mut project = Project::new("Test");
        let mut track = Track::new_video("V1");
        let track_id = track.id.clone();

        let mut left = Clip::new("file.mp4", 10, 0, ClipKind::Video);
        left.id = "L".to_string();
        left.source_in = 0;
        left.source_out = 10;
        left.group_id = Some("g1".to_string());
        left.link_group_id = Some("l1".to_string());
        left.brightness = 0.2;
        let mut right = left.clone();
        right.id = "R".to_string();
        right.source_in = 10;
        right.source_out = 20;
        right.timeline_start = 10;
        right.outgoing_transition = OutgoingTransition::new(
            "cross_dissolve",
            500,
            crate::model::transition::TransitionAlignment::EndOnCut,
        );
        track.add_clip(left.clone());
        track.add_clip(right.clone());
        project.tracks.push(track);

        let old_clips = vec![left.clone(), right.clone()];
        let mut merged = left.clone();
        merged.source_out = right.source_out;
        merged.outgoing_transition = right.outgoing_transition.clone();
        let new_clips = vec![merged.clone()];

        let cmd = JoinThroughEditCommand {
            track_id: track_id.clone(),
            old_clips,
            new_clips,
        };

        cmd.execute(&mut project);
        let track = project.tracks.iter().find(|t| t.id == track_id).unwrap();
        assert_eq!(track.clips.len(), 1);
        assert_eq!(track.clips[0], merged);

        cmd.undo(&mut project);
        let track = project.tracks.iter().find(|t| t.id == track_id).unwrap();
        assert_eq!(track.clips.len(), 2);
        assert_eq!(track.clips[0], left);
        assert_eq!(track.clips[1], right);
    }

    #[test]
    fn test_roll_edit_command() {
        let mut project = Project::new("Test");
        let mut track = Track::new_video("V1");
        let track_id = track.id.clone();

        let mut left = Clip::new("file.mp4", 10, 0, ClipKind::Video);
        left.id = "L".to_string();
        let mut right = Clip::new("file.mp4", 20, 10, ClipKind::Video);
        right.id = "R".to_string();
        right.source_in = 10;
        track.add_clip(left);
        track.add_clip(right);
        project.tracks.push(track);

        let cmd = RollEditCommand {
            left_clip_id: "L".to_string(),
            right_clip_id: "R".to_string(),
            track_id: track_id.clone(),
            old_left_out: 10,
            new_left_out: 12,
            old_right_in: 10,
            new_right_in: 12,
            old_right_start: 10,
            new_right_start: 12,
        };

        cmd.execute(&mut project);
        let track = project.tracks.iter().find(|t| t.id == track_id).unwrap();
        let l = track.clips.iter().find(|c| c.id == "L").unwrap();
        let r = track.clips.iter().find(|c| c.id == "R").unwrap();
        assert_eq!(l.source_out, 12);
        assert_eq!(r.source_in, 12);
        assert_eq!(r.timeline_start, 12);

        cmd.undo(&mut project);
        let track = project.tracks.iter().find(|t| t.id == track_id).unwrap();
        let l = track.clips.iter().find(|c| c.id == "L").unwrap();
        let r = track.clips.iter().find(|c| c.id == "R").unwrap();
        assert_eq!(l.source_out, 10);
        assert_eq!(r.source_in, 10);
        assert_eq!(r.timeline_start, 10);
    }

    #[test]
    fn test_reorder_track_command() {
        let mut project = Project::new("Test");
        // project already has tracks[0]=Video1, tracks[1]=Audio1
        // Add a third track
        project.add_video_track(); // tracks[2] = Video 2

        let id0 = project.tracks[0].id.clone();
        let cmd = ReorderTrackCommand {
            from_index: 0,
            to_index: 2,
        };
        cmd.execute(&mut project);
        assert_eq!(project.tracks[2].id, id0);

        cmd.undo(&mut project);
        assert_eq!(project.tracks[0].id, id0);
    }

    #[test]
    fn test_set_multiple_tracks_clips_command_updates_and_undoes_together() {
        let mut project = Project::new("Test");
        let mut video_track = Track::new_video("V1");
        let video_track_id = video_track.id.clone();
        let mut audio_track = Track::new_audio("A1");
        let audio_track_id = audio_track.id.clone();

        let mut video_clip = Clip::new("file.mp4", 10, 0, ClipKind::Video);
        video_clip.id = "video-a".to_string();
        let mut audio_clip = Clip::new("file.mp4", 10, 0, ClipKind::Audio);
        audio_clip.id = "audio-a".to_string();
        video_track.add_clip(video_clip.clone());
        audio_track.add_clip(audio_clip.clone());
        project.tracks.push(video_track);
        project.tracks.push(audio_track);

        let mut new_video_clip = video_clip.clone();
        new_video_clip.timeline_start = 20;
        let mut new_audio_clip = audio_clip.clone();
        new_audio_clip.timeline_start = 20;

        let cmd = SetMultipleTracksClipsCommand {
            changes: vec![
                TrackClipsChange {
                    track_id: video_track_id.clone(),
                    old_clips: vec![video_clip.clone()],
                    new_clips: vec![new_video_clip],
                },
                TrackClipsChange {
                    track_id: audio_track_id.clone(),
                    old_clips: vec![audio_clip.clone()],
                    new_clips: vec![new_audio_clip],
                },
            ],
            label: "Move linked pair".to_string(),
        };

        cmd.execute(&mut project);
        assert_eq!(
            project.track_mut(&video_track_id).unwrap().clips[0].timeline_start,
            20
        );
        assert_eq!(
            project.track_mut(&audio_track_id).unwrap().clips[0].timeline_start,
            20
        );

        cmd.undo(&mut project);
        assert_eq!(
            project.track_mut(&video_track_id).unwrap().clips[0].timeline_start,
            0
        );
        assert_eq!(
            project.track_mut(&audio_track_id).unwrap().clips[0].timeline_start,
            0
        );
    }

    #[test]
    fn test_edit_history_undo_redo() {
        let (mut project, track_id, clip_id) = make_project_with_clip("C", 10, 0);
        let mut history = EditHistory::new();

        assert!(!history.can_undo());
        assert!(!history.can_redo());

        let cmd = Box::new(TrimOutCommand {
            clip_id: clip_id.clone(),
            track_id: track_id.clone(),
            old_source_out: 10,
            new_source_out: 7,
        });
        history.execute(cmd, &mut project);
        assert!(history.can_undo());
        assert!(!history.can_redo());

        let did_undo = history.undo(&mut project);
        assert!(did_undo);
        assert!(!history.can_undo());
        assert!(history.can_redo());

        let track = project.tracks.iter().find(|t| t.id == track_id).unwrap();
        let clip = track.clips.iter().find(|c| c.id == clip_id).unwrap();
        assert_eq!(clip.source_out, 10); // restored

        let did_redo = history.redo(&mut project);
        assert!(did_redo);
        assert!(history.can_undo());
        assert!(!history.can_redo());

        let track = project.tracks.iter().find(|t| t.id == track_id).unwrap();
        let clip = track.clips.iter().find(|c| c.id == clip_id).unwrap();
        assert_eq!(clip.source_out, 7); // reapplied
    }

    #[test]
    fn test_edit_history_new_action_clears_redo() {
        let (mut project, track_id, clip_id) = make_project_with_clip("C", 10, 0);
        let mut history = EditHistory::new();

        history.execute(
            Box::new(TrimOutCommand {
                clip_id: clip_id.clone(),
                track_id: track_id.clone(),
                old_source_out: 10,
                new_source_out: 8,
            }),
            &mut project,
        );

        history.undo(&mut project);
        assert!(history.can_redo());

        // New action should clear redo stack
        history.execute(
            Box::new(TrimOutCommand {
                clip_id: clip_id.clone(),
                track_id: track_id.clone(),
                old_source_out: 10,
                new_source_out: 6,
            }),
            &mut project,
        );

        assert!(!history.can_redo());
    }

    #[test]
    fn test_edit_history_undo_description() {
        let (mut project, track_id, clip_id) = make_project_with_clip("C", 10, 0);
        let mut history = EditHistory::new();

        assert!(history.undo_description().is_none());

        history.execute(
            Box::new(TrimOutCommand {
                clip_id,
                track_id,
                old_source_out: 10,
                new_source_out: 8,
            }),
            &mut project,
        );

        assert_eq!(history.undo_description(), Some("Trim clip out-point"));
    }

    #[test]
    fn test_edit_history_undo_redo_descriptions_follow_operations() {
        let (mut project, track_id, clip_id) = make_project_with_clip("C", 10, 0);
        let mut history = EditHistory::new();
        let cmd = TrimOutCommand {
            clip_id: clip_id.clone(),
            track_id: track_id.clone(),
            old_source_out: 10,
            new_source_out: 6,
        };
        history.execute(Box::new(cmd), &mut project);

        assert_eq!(history.undo_description(), Some("Trim clip out-point"));
        assert!(history.redo_description().is_none());

        let undo_label = history.undo_with_description(&mut project);
        assert_eq!(undo_label.as_deref(), Some("Trim clip out-point"));
        assert!(history.undo_description().is_none());
        assert_eq!(history.redo_description(), Some("Trim clip out-point"));

        let redo_label = history.redo_with_description(&mut project);
        assert_eq!(redo_label.as_deref(), Some("Trim clip out-point"));
        assert_eq!(history.undo_description(), Some("Trim clip out-point"));
        assert!(history.redo_description().is_none());
    }

    #[test]
    fn test_edit_history_empty_undo_redo() {
        let mut project = Project::new("Test");
        let mut history = EditHistory::new();
        assert!(!history.undo(&mut project));
        assert!(!history.redo(&mut project));
        assert!(history.undo_with_description(&mut project).is_none());
        assert!(history.redo_with_description(&mut project).is_none());
    }

    // ── Compound clip undo tests ──────────────────────────────────────

    #[test]
    fn test_set_track_clips_command_on_nested_track() {
        let mut project = Project::new("Test");
        project.tracks.clear();

        // Create a compound clip with an internal video track
        let mut inner_track = Track::new_video("Inner V1");
        let inner_track_id = inner_track.id.clone();
        let mut clip_a = Clip::new("a.mp4", 5_000, 0, ClipKind::Video);
        clip_a.id = "A".into();
        inner_track.add_clip(clip_a.clone());

        let mut compound = Clip::new_compound(0, vec![inner_track]);
        compound.id = "compound".into();
        let mut root_track = Track::new_video("Root V1");
        root_track.add_clip(compound);
        project.tracks.push(root_track);

        // Verify inner clip exists
        assert!(project.clip_ref("A").is_some());

        // Delete the inner clip via SetTrackClipsCommand targeting the nested track
        let old_clips = vec![clip_a];
        let new_clips: Vec<Clip> = vec![];
        let cmd = SetTrackClipsCommand {
            track_id: inner_track_id.clone(),
            old_clips: old_clips.clone(),
            new_clips: new_clips.clone(),
            label: "Delete inner clip".into(),
        };

        cmd.execute(&mut project);
        // Inner clip should be gone
        assert!(project.clip_ref("A").is_none());
        let inner = project.track_ref(&inner_track_id).unwrap();
        assert!(inner.clips.is_empty());

        // Undo should restore it
        cmd.undo(&mut project);
        assert!(project.clip_ref("A").is_some());
        let inner = project.track_ref(&inner_track_id).unwrap();
        assert_eq!(inner.clips.len(), 1);
    }

    #[test]
    fn clip_mutate_command_sets_and_reverts_property() {
        let mut project = Project::new("Test");
        let track = &project.tracks[0];
        let track_id = track.id.clone();
        let mut clip = Clip::new("test.mp4", 5_000_000_000, 0, ClipKind::Video);
        clip.id = "c1".into();
        clip.volume = 1.0;
        project.track_mut(&track_id).unwrap().add_clip(clip);

        let cmd = ClipMutateCommand {
            clip_id: "c1".into(),
            old_state: 1.0_f32,
            new_state: 0.5_f32,
            apply: |clip, v| {
                clip.volume = v;
            },
            label: "Set volume",
        };

        cmd.execute(&mut project);
        assert!((project.clip_ref("c1").unwrap().volume - 0.5).abs() < 1e-6);
        assert!(project.dirty);

        project.dirty = false;
        cmd.undo(&mut project);
        assert!((project.clip_ref("c1").unwrap().volume - 1.0).abs() < 1e-6);
        assert!(project.dirty);
    }

    #[test]
    fn clip_mutate_command_finds_compound_internal_clips() {
        let mut project = Project::new("Test");
        project.tracks.clear();

        let mut inner_track = Track::new_video("Inner");
        let mut inner_clip = Clip::new("inner.mp4", 5_000, 0, ClipKind::Video);
        inner_clip.id = "inner-c1".into();
        inner_clip.volume = 1.0;
        inner_track.add_clip(inner_clip);

        let mut compound = Clip::new_compound(0, vec![inner_track]);
        compound.id = "compound".into();
        let mut root = Track::new_video("Root");
        root.add_clip(compound);
        project.tracks.push(root);

        let cmd = ClipMutateCommand {
            clip_id: "inner-c1".into(),
            old_state: 1.0_f32,
            new_state: 0.3_f32,
            apply: |clip, v| {
                clip.volume = v;
            },
            label: "Set inner volume",
        };

        cmd.execute(&mut project);
        assert!((project.clip_ref("inner-c1").unwrap().volume - 0.3).abs() < 1e-6);

        cmd.undo(&mut project);
        assert!((project.clip_ref("inner-c1").unwrap().volume - 1.0).abs() < 1e-6);
    }

    #[test]
    fn track_mutate_command_sets_and_reverts_property() {
        let mut project = Project::new("Test");
        let track_id = project.tracks[0].id.clone();
        assert!(!project.tracks[0].muted);

        let cmd = TrackMutateCommand {
            track_id: track_id.clone(),
            old_state: false,
            new_state: true,
            apply: |track, v| {
                track.muted = v;
            },
            label: "Toggle mute",
        };

        cmd.execute(&mut project);
        assert!(project.track_ref(&track_id).unwrap().muted);

        cmd.undo(&mut project);
        assert!(!project.track_ref(&track_id).unwrap().muted);
    }

    #[test]
    fn test_set_multiple_tracks_clips_command_on_nested_tracks() {
        let mut project = Project::new("Test");
        project.tracks.clear();

        let mut inner_v = Track::new_video("Inner V");
        let inner_v_id = inner_v.id.clone();
        let mut clip_v = Clip::new("v.mp4", 5_000, 0, ClipKind::Video);
        clip_v.id = "V1".into();
        inner_v.add_clip(clip_v.clone());

        let mut inner_a = Track::new_audio("Inner A");
        let inner_a_id = inner_a.id.clone();
        let mut clip_a = Clip::new("a.wav", 5_000, 0, ClipKind::Audio);
        clip_a.id = "A1".into();
        inner_a.add_clip(clip_a.clone());

        let mut compound = Clip::new_compound(0, vec![inner_v, inner_a]);
        compound.id = "compound".into();
        let mut root = Track::new_video("Root");
        root.add_clip(compound);
        project.tracks.push(root);

        // Use SetMultipleTracksClipsCommand to clear both nested tracks
        let cmd = SetMultipleTracksClipsCommand {
            changes: vec![
                TrackClipsChange {
                    track_id: inner_v_id.clone(),
                    old_clips: vec![clip_v],
                    new_clips: vec![],
                },
                TrackClipsChange {
                    track_id: inner_a_id.clone(),
                    old_clips: vec![clip_a],
                    new_clips: vec![],
                },
            ],
            label: "Clear compound internals".into(),
        };

        cmd.execute(&mut project);
        assert!(project.track_ref(&inner_v_id).unwrap().clips.is_empty());
        assert!(project.track_ref(&inner_a_id).unwrap().clips.is_empty());

        cmd.undo(&mut project);
        assert_eq!(project.track_ref(&inner_v_id).unwrap().clips.len(), 1);
        assert_eq!(project.track_ref(&inner_a_id).unwrap().clips.len(), 1);
    }

    #[test]
    fn add_marker_command_execute_and_undo() {
        use crate::model::project::Marker;
        let mut project = Project::new("Marker Undo");
        assert!(project.markers.is_empty());

        let marker = Marker::new(1_000_000_000, "M1");
        let cmd = AddMarkerCommand {
            marker: marker.clone(),
        };
        cmd.execute(&mut project);
        assert_eq!(project.markers.len(), 1);
        assert_eq!(project.markers[0].label, "M1");

        cmd.undo(&mut project);
        assert!(project.markers.is_empty());
    }

    #[test]
    fn remove_marker_command_execute_and_undo() {
        use crate::model::project::Marker;
        let mut project = Project::new("Marker Undo");
        let m = Marker::new(2_000_000_000, "M2");
        project.markers.push(m.clone());
        assert_eq!(project.markers.len(), 1);

        let cmd = RemoveMarkerCommand { marker: m };
        cmd.execute(&mut project);
        assert!(project.markers.is_empty());

        cmd.undo(&mut project);
        assert_eq!(project.markers.len(), 1);
        assert_eq!(project.markers[0].label, "M2");
    }

    #[test]
    fn edit_marker_command_execute_and_undo() {
        use crate::model::project::Marker;
        let mut project = Project::new("Marker Edit");
        let mut m = Marker::new(0, "Before");
        m.notes = "old notes".to_string();
        let marker_id = m.id.clone();
        project.markers.push(m.clone());

        let mut new_marker = m.clone();
        new_marker.label = "After".to_string();
        new_marker.notes = "new notes".to_string();
        let cmd = EditMarkerCommand {
            marker_id: marker_id.clone(),
            old_state: m,
            new_state: new_marker,
        };
        cmd.execute(&mut project);
        assert_eq!(project.markers[0].label, "After");
        assert_eq!(project.markers[0].notes, "new notes");

        cmd.undo(&mut project);
        assert_eq!(project.markers[0].label, "Before");
        assert_eq!(project.markers[0].notes, "old notes");
    }

    #[test]
    fn test_set_track_gain_cmd() {
        let mut project = Project::new("Gain Test");
        let mut audio_track = Track::new_audio("A1");
        let track_id = audio_track.id.clone();
        project.tracks.push(audio_track);
        assert_eq!(project.track_ref(&track_id).unwrap().gain_db, 0.0);

        let cmd = set_track_gain_cmd(track_id.clone(), 0.0, -6.0);
        cmd.execute(&mut project);
        assert_eq!(project.track_ref(&track_id).unwrap().gain_db, -6.0);
        cmd.undo(&mut project);
        assert_eq!(project.track_ref(&track_id).unwrap().gain_db, 0.0);
        cmd.execute(&mut project);
        assert_eq!(project.track_ref(&track_id).unwrap().gain_db, -6.0);
    }

    #[test]
    fn test_set_track_pan_cmd() {
        let mut project = Project::new("Pan Test");
        let mut audio_track = Track::new_audio("A1");
        let track_id = audio_track.id.clone();
        project.tracks.push(audio_track);
        assert_eq!(project.track_ref(&track_id).unwrap().pan, 0.0);

        let cmd = set_track_pan_cmd(track_id.clone(), 0.0, -0.5);
        cmd.execute(&mut project);
        assert_eq!(project.track_ref(&track_id).unwrap().pan, -0.5);
        cmd.undo(&mut project);
        assert_eq!(project.track_ref(&track_id).unwrap().pan, 0.0);
        cmd.execute(&mut project);
        assert_eq!(project.track_ref(&track_id).unwrap().pan, -0.5);
    }

    #[test]
    fn test_set_bus_gain_undo() {
        let mut project = Project::new("Test");
        let cmd = set_bus_gain_cmd(AudioRole::Dialogue, 0.0, -6.0);
        cmd.execute(&mut project);
        assert!((project.dialogue_bus.gain_db - (-6.0)).abs() < f64::EPSILON);
        cmd.undo(&mut project);
        assert!((project.dialogue_bus.gain_db - 0.0).abs() < f64::EPSILON);
    }

    #[test]
    fn test_set_bus_mute_undo() {
        let mut project = Project::new("Test");
        let cmd = set_bus_mute_cmd(AudioRole::Effects, false, true);
        cmd.execute(&mut project);
        assert!(project.effects_bus.muted);
        cmd.undo(&mut project);
        assert!(!project.effects_bus.muted);
    }

    #[test]
    fn test_set_bus_solo_undo() {
        let mut project = Project::new("Test");
        let cmd = set_bus_solo_cmd(AudioRole::Music, false, true);
        cmd.execute(&mut project);
        assert!(project.music_bus.soloed);
        cmd.undo(&mut project);
        assert!(!project.music_bus.soloed);
    }
}
