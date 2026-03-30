use crate::model::clip::Clip;
use crate::model::project::Project;

/// A reversible edit operation on the project.
pub trait EditCommand {
    fn execute(&self, project: &mut Project);
    fn undo(&self, project: &mut Project);
    #[allow(dead_code)]
    fn description(&self) -> &str;
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
                clip.source_out = clip.source_in + cut_offset;
            }
            // Insert the right half
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
    pub track_id: String,
    pub old_volume: f32,
    pub new_volume: f32,
    pub old_measured_loudness: Option<f64>,
    pub new_measured_loudness: Option<f64>,
}

impl EditCommand for NormalizeClipAudioCommand {
    fn execute(&self, project: &mut Project) {
        if let Some(track) = project.track_mut(&self.track_id) {
            if let Some(clip) = track.clips.iter_mut().find(|c| c.id == self.clip_id) {
                clip.volume = self.new_volume;
                clip.measured_loudness_lufs = self.new_measured_loudness;
            }
        }
        project.dirty = true;
    }
    fn undo(&self, project: &mut Project) {
        if let Some(track) = project.track_mut(&self.track_id) {
            if let Some(clip) = track.clips.iter_mut().find(|c| c.id == self.clip_id) {
                clip.volume = self.old_volume;
                clip.measured_loudness_lufs = self.old_measured_loudness;
            }
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

/// Toggle a track's mute state.
pub struct SetTrackMuteCommand {
    pub track_id: String,
    pub old_muted: bool,
    pub new_muted: bool,
}

impl EditCommand for SetTrackMuteCommand {
    fn execute(&self, project: &mut Project) {
        if let Some(track) = project.track_mut(&self.track_id) {
            track.muted = self.new_muted;
        }
        project.dirty = true;
    }
    fn undo(&self, project: &mut Project) {
        if let Some(track) = project.track_mut(&self.track_id) {
            track.muted = self.old_muted;
        }
        project.dirty = true;
    }
    fn description(&self) -> &str {
        "Toggle track mute"
    }
}

/// Toggle a track's solo state.
pub struct SetTrackSoloCommand {
    pub track_id: String,
    pub old_solo: bool,
    pub new_solo: bool,
}

impl EditCommand for SetTrackSoloCommand {
    fn execute(&self, project: &mut Project) {
        if let Some(track) = project.track_mut(&self.track_id) {
            track.soloed = self.new_solo;
        }
        project.dirty = true;
    }
    fn undo(&self, project: &mut Project) {
        if let Some(track) = project.track_mut(&self.track_id) {
            track.soloed = self.old_solo;
        }
        project.dirty = true;
    }
    fn description(&self) -> &str {
        "Toggle track solo"
    }
}

/// Toggle a track's duck (sidechain) state.
pub struct SetTrackDuckCommand {
    pub track_id: String,
    pub old_duck: bool,
    pub new_duck: bool,
}

impl EditCommand for SetTrackDuckCommand {
    fn execute(&self, project: &mut Project) {
        if let Some(track) = project.track_mut(&self.track_id) {
            track.duck = self.new_duck;
        }
        project.dirty = true;
    }
    fn undo(&self, project: &mut Project) {
        if let Some(track) = project.track_mut(&self.track_id) {
            track.duck = self.old_duck;
        }
        project.dirty = true;
    }
    fn description(&self) -> &str {
        "Toggle track duck"
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
    pub old_transition: String,
    pub old_transition_ns: u64,
    pub new_transition: String,
    pub new_transition_ns: u64,
}

impl EditCommand for SetClipTransitionCommand {
    fn execute(&self, project: &mut Project) {
        if let Some(track) = project.track_mut(&self.track_id) {
            if let Some(clip) = track.clips.iter_mut().find(|c| c.id == self.clip_id) {
                clip.transition_after = self.new_transition.clone();
                clip.transition_after_ns = self.new_transition_ns;
            }
        }
        project.dirty = true;
    }
    fn undo(&self, project: &mut Project) {
        if let Some(track) = project.track_mut(&self.track_id) {
            if let Some(clip) = track.clips.iter_mut().find(|c| c.id == self.clip_id) {
                clip.transition_after = self.old_transition.clone();
                clip.transition_after_ns = self.old_transition_ns;
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

    pub fn undo(&mut self, project: &mut Project) -> bool {
        if let Some(cmd) = self.undo_stack.pop() {
            cmd.undo(project);
            self.redo_stack.push(cmd);
            true
        } else {
            false
        }
    }

    pub fn redo(&mut self, project: &mut Project) -> bool {
        if let Some(cmd) = self.redo_stack.pop() {
            cmd.execute(project);
            self.undo_stack.push(cmd);
            true
        } else {
            false
        }
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
                if let Some(effect) = clip.frei0r_effects.iter_mut().find(|e| e.id == self.effect_id) {
                    effect.params = self.new_params.clone();
                }
            }
        }
        project.dirty = true;
    }
    fn undo(&self, project: &mut Project) {
        if let Some(track) = project.track_mut(&self.track_id) {
            if let Some(clip) = track.clips.iter_mut().find(|c| c.id == self.clip_id) {
                if let Some(effect) = clip.frei0r_effects.iter_mut().find(|e| e.id == self.effect_id) {
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
                if let Some(effect) = clip.frei0r_effects.iter_mut().find(|e| e.id == self.effect_id) {
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
        if let Some(clip) = project.tracks.iter_mut()
            .flat_map(|t| t.clips.iter_mut())
            .find(|c| c.id == self.clip_id)
        {
            self.after.apply_to_clip(clip);
        }
        project.dirty = true;
    }
    fn undo(&self, project: &mut Project) {
        if let Some(clip) = project.tracks.iter_mut()
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::clip::{Clip, ClipKind};
    use crate::model::project::Project;
    use crate::model::track::Track;

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
        right.transition_after = "cross_dissolve".to_string();
        right.transition_after_ns = 500;
        track.add_clip(left.clone());
        track.add_clip(right.clone());
        project.tracks.push(track);

        let old_clips = vec![left.clone(), right.clone()];
        let mut merged = left.clone();
        merged.source_out = right.source_out;
        merged.transition_after = right.transition_after.clone();
        merged.transition_after_ns = right.transition_after_ns;
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
    fn test_edit_history_empty_undo_redo() {
        let mut project = Project::new("Test");
        let mut history = EditHistory::new();
        assert!(!history.undo(&mut project));
        assert!(!history.redo(&mut project));
    }
}
