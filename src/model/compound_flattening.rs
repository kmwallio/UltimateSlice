use super::clip::{Clip, ClipKind};

/// One direct child clip from a compound clip, rebased into the parent timeline.
#[derive(Debug, Clone, PartialEq)]
pub struct FlattenedCompoundChild {
    pub clip: Clip,
    pub relative_track_index: usize,
    pub is_audio_track: bool,
    pub duck: bool,
    pub duck_amount_db: f64,
}

/// Trim a direct child of a compound clip to the compound's visible window and
/// rebase it into the parent timeline.
pub fn rebase_clip_from_compound_window(
    clip: &Clip,
    compound_timeline_start: u64,
    compound_source_in: u64,
    compound_source_out: u64,
) -> Option<Clip> {
    let mut clip = clip.rebase_to_window(compound_source_in, compound_source_out)?;
    clip.timeline_start = clip
        .timeline_start
        .saturating_sub(compound_source_in)
        .saturating_add(compound_timeline_start);
    Some(clip)
}

/// Enumerate the direct children of a compound clip after trimming them to the
/// compound window and rebasing them into the parent timeline.
pub fn flatten_compound_children(
    compound: &Clip,
    timeline_offset: u64,
) -> Vec<FlattenedCompoundChild> {
    if compound.kind != ClipKind::Compound {
        return Vec::new();
    }
    let Some(internal_tracks) = compound.compound_tracks.as_ref() else {
        return Vec::new();
    };
    let compound_timeline_start = timeline_offset.saturating_add(compound.timeline_start);
    let mut children = Vec::new();
    for (relative_track_index, track) in internal_tracks.iter().enumerate() {
        for inner_clip in &track.clips {
            let Some(clip) = rebase_clip_from_compound_window(
                inner_clip,
                compound_timeline_start,
                compound.source_in,
                compound.source_out,
            ) else {
                continue;
            };
            children.push(FlattenedCompoundChild {
                clip,
                relative_track_index,
                is_audio_track: track.is_audio(),
                duck: track.duck,
                duck_amount_db: track.duck_amount_db,
            });
        }
    }
    children
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::track::Track;

    #[test]
    fn rebase_clip_from_compound_window_trims_and_rebases_to_parent_timeline() {
        let mut clip = Clip::new("inner.mov", 10_000_000_000, 2_000_000_000, ClipKind::Video);
        clip.id = "inner".to_string();

        let rebased =
            rebase_clip_from_compound_window(&clip, 10_000_000_000, 4_000_000_000, 9_000_000_000)
                .expect("clip should overlap compound window");

        assert_eq!(rebased.source_in, 2_000_000_000);
        assert_eq!(rebased.source_out, 7_000_000_000);
        assert_eq!(rebased.timeline_start, 10_000_000_000);
    }

    #[test]
    fn flatten_compound_children_preserves_track_metadata_and_absolute_timing() {
        let mut video_track = Track::new_video("V1");
        let mut video_clip = Clip::new("video.mov", 8_000_000_000, 5_000_000_000, ClipKind::Video);
        video_clip.id = "video".to_string();
        video_track.add_clip(video_clip);

        let mut audio_track = Track::new_audio("A1");
        audio_track.duck = true;
        audio_track.duck_amount_db = -9.0;
        let mut audio_clip = Clip::new("audio.wav", 7_000_000_000, 6_000_000_000, ClipKind::Audio);
        audio_clip.id = "audio".to_string();
        audio_track.add_clip(audio_clip);

        let mut compound = Clip::new_compound(10_000_000_000, vec![video_track, audio_track]);
        compound.source_in = 4_000_000_000;
        compound.source_out = 12_000_000_000;

        let children = flatten_compound_children(&compound, 1_000_000_000);
        assert_eq!(children.len(), 2);

        assert_eq!(children[0].clip.id, "video");
        assert_eq!(children[0].clip.timeline_start, 12_000_000_000);
        assert_eq!(children[0].relative_track_index, 0);
        assert!(!children[0].is_audio_track);
        assert!(!children[0].duck);

        assert_eq!(children[1].clip.id, "audio");
        assert_eq!(children[1].clip.timeline_start, 13_000_000_000);
        assert_eq!(children[1].relative_track_index, 1);
        assert!(children[1].is_audio_track);
        assert!(children[1].duck);
        assert_eq!(children[1].duck_amount_db, -9.0);
    }

    #[test]
    fn flatten_compound_children_skips_non_overlapping_clips() {
        let mut track = Track::new_video("V1");
        let mut before = Clip::new("before.mov", 2_000_000_000, 0, ClipKind::Video);
        before.id = "before".to_string();
        track.add_clip(before);
        let mut inside = Clip::new("inside.mov", 8_000_000_000, 5_000_000_000, ClipKind::Video);
        inside.id = "inside".to_string();
        track.add_clip(inside);

        let mut compound = Clip::new_compound(20_000_000_000, vec![track]);
        compound.source_in = 4_000_000_000;
        compound.source_out = 9_000_000_000;

        let children = flatten_compound_children(&compound, 0);
        assert_eq!(children.len(), 1);
        assert_eq!(children[0].clip.id, "inside");
        assert_eq!(children[0].clip.timeline_start, 21_000_000_000);
    }
}
