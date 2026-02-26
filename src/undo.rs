use crate::model::project::Project;
use crate::model::clip::Clip;

/// A reversible edit operation on the project.
pub trait EditCommand {
    fn execute(&self, project: &mut Project);
    fn undo(&self, project: &mut Project);
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
        move_clip(project, &self.clip_id, &self.from_track_id, &self.to_track_id, self.new_timeline_start);
    }
    fn undo(&self, project: &mut Project) {
        move_clip(project, &self.clip_id, &self.to_track_id, &self.from_track_id, self.old_timeline_start);
    }
    fn description(&self) -> &str { "Move clip" }
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
    fn description(&self) -> &str { "Trim clip" }
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
    fn description(&self) -> &str { "Trim clip out-point" }
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
    fn description(&self) -> &str { "Ripple trim" }
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
                    if clip.id == self.clip_id { continue; }
                    
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
                    if clip.id == self.clip_id { continue; }

                    if clip.timeline_start >= threshold {
                        let new_start = (clip.timeline_start as i64 - self.delta).max(0) as u64;
                        clip.timeline_start = new_start;
                    }
                }
            }
        }
        project.dirty = true;
    }
    fn description(&self) -> &str { "Ripple trim in-point" }
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
    fn description(&self) -> &str { "Roll edit" }
}

/// Delete a clip from a track
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
    fn description(&self) -> &str { "Delete clip" }
}

/// Replace a track's full clip list (used for grouped magnetic timeline edits).
pub struct SetTrackClipsCommand {
    pub track_id: String,
    pub old_clips: Vec<Clip>,
    pub new_clips: Vec<Clip>,
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
    fn description(&self) -> &str { &self.label }
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
            if let Some(clip) = track.clips.iter_mut().find(|c| c.id == self.original_clip.id) {
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
            if let Some(clip) = track.clips.iter_mut().find(|c| c.id == self.original_clip.id) {
                clip.source_out = self.original_clip.source_out;
            }
        }
        project.dirty = true;
    }
    fn description(&self) -> &str { "Razor cut" }
}

/// Set color correction on a clip (brightness/contrast/saturation)
pub struct SetClipColorCommand {
    pub clip_id: String,
    pub track_id: String,
    pub old_brightness: f32,
    pub old_contrast: f32,
    pub old_saturation: f32,
    pub new_brightness: f32,
    pub new_contrast: f32,
    pub new_saturation: f32,
}

impl EditCommand for SetClipColorCommand {
    fn execute(&self, project: &mut Project) {
        if let Some(track) = project.track_mut(&self.track_id) {
            if let Some(clip) = track.clips.iter_mut().find(|c| c.id == self.clip_id) {
                clip.brightness = self.new_brightness;
                clip.contrast = self.new_contrast;
                clip.saturation = self.new_saturation;
            }
        }
        project.dirty = true;
    }
    fn undo(&self, project: &mut Project) {
        if let Some(track) = project.track_mut(&self.track_id) {
            if let Some(clip) = track.clips.iter_mut().find(|c| c.id == self.clip_id) {
                clip.brightness = self.old_brightness;
                clip.contrast = self.old_contrast;
                clip.saturation = self.old_saturation;
            }
        }
        project.dirty = true;
    }
    fn description(&self) -> &str { "Set clip color" }
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
    fn description(&self) -> &str { "Set clip transition" }
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
    fn description(&self) -> &str { "Delete track" }
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
    fn description(&self) -> &str { "Add track" }
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
    fn description(&self) -> &str { "Reorder track" }
}

fn reorder_track<T>(vec: &mut Vec<T>, from: usize, to: usize) {
    if from >= vec.len() || to >= vec.len() || from == to { return; }
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

    pub fn can_undo(&self) -> bool { !self.undo_stack.is_empty() }
    pub fn can_redo(&self) -> bool { !self.redo_stack.is_empty() }

    pub fn undo_description(&self) -> Option<&str> {
        self.undo_stack.last().map(|c| c.description())
    }
}

fn move_clip(project: &mut Project, clip_id: &str, from_track_id: &str, to_track_id: &str, new_start: u64) {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::project::Project;
    use crate::model::track::{Track, TrackKind};
    use crate::model::clip::{Clip, ClipKind};

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
}
