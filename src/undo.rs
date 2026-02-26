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
