use crate::media::player::Player;
use crate::media::program_player::{ProgramClip, ProgramPlayer};
use crate::model::clip::{Clip, ClipKind, Phase1KeyframeProperty};
use crate::model::media_library::MediaItem;
use crate::model::project::Project;
use crate::model::track::TrackKind;
use crate::recent;
use crate::ui::timecode;
use crate::ui::timeline::{build_timeline_panel, TimelineState};
use crate::ui::{inspector, media_browser, preferences, preview, program_monitor, toolbar};
use crate::undo::TrackClipsChange;
use glib;
use gtk4::prelude::*;
use gtk4::{self as gtk, ApplicationWindow, Orientation, Paned, ScrolledWindow};
use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::rc::Rc;

thread_local! {
    static MCP_MAIN_DISPATCH: RefCell<Option<Box<dyn FnMut(crate::mcp::McpCommand)>>> =
        RefCell::new(None);
}

fn flash_window_status_title(
    window: &gtk::ApplicationWindow,
    project: &Rc<RefCell<Project>>,
    message: &str,
) {
    let (title, dirty) = {
        let proj = project.borrow();
        (proj.title.clone(), proj.dirty)
    };
    window.set_title(Some(&format!("UltimateSlice — {title} ({message})")));
    let window_weak = window.downgrade();
    glib::timeout_add_local_once(std::time::Duration::from_secs(3), move || {
        if let Some(win) = window_weak.upgrade() {
            if dirty {
                win.set_title(Some(&format!("UltimateSlice — {title} •")));
            } else {
                win.set_title(Some(&format!("UltimateSlice — {title}")));
            }
        }
    });
}

/// Evaluate a clip's keyframe-interpolated transform at a given playhead position.
/// Returns `(scale, position_x, position_y, rotate, crop_left, crop_right, crop_top, crop_bottom)`
/// accounting for keyframes on those properties.
fn evaluate_clip_transform_at(
    clip: &Clip,
    playhead_ns: u64,
) -> (f64, f64, f64, i32, i32, i32, i32, i32) {
    let scale =
        clip.value_for_phase1_property_at_timeline_ns(Phase1KeyframeProperty::Scale, playhead_ns);
    let pos_x = clip
        .value_for_phase1_property_at_timeline_ns(Phase1KeyframeProperty::PositionX, playhead_ns);
    let pos_y = clip
        .value_for_phase1_property_at_timeline_ns(Phase1KeyframeProperty::PositionY, playhead_ns);
    let rotate = clip
        .value_for_phase1_property_at_timeline_ns(Phase1KeyframeProperty::Rotate, playhead_ns)
        .round()
        .clamp(-180.0, 180.0) as i32;
    let crop_left = clip
        .value_for_phase1_property_at_timeline_ns(Phase1KeyframeProperty::CropLeft, playhead_ns)
        .round()
        .clamp(0.0, 500.0) as i32;
    let crop_right = clip
        .value_for_phase1_property_at_timeline_ns(Phase1KeyframeProperty::CropRight, playhead_ns)
        .round()
        .clamp(0.0, 500.0) as i32;
    let crop_top = clip
        .value_for_phase1_property_at_timeline_ns(Phase1KeyframeProperty::CropTop, playhead_ns)
        .round()
        .clamp(0.0, 500.0) as i32;
    let crop_bottom = clip
        .value_for_phase1_property_at_timeline_ns(Phase1KeyframeProperty::CropBottom, playhead_ns)
        .round()
        .clamp(0.0, 500.0) as i32;
    (
        scale,
        pos_x,
        pos_y,
        rotate,
        crop_left,
        crop_right,
        crop_top,
        crop_bottom,
    )
}

/// Update the transform overlay to reflect the keyframe-interpolated transform
/// of the selected clip at the given playhead position.
fn sync_transform_overlay_to_playhead(
    transform_overlay: &crate::ui::transform_overlay::TransformOverlay,
    project: &Project,
    selected_clip_id: Option<&str>,
    playhead_ns: u64,
) {
    match selected_clip_id {
        Some(cid) => {
            let clip_opt = project
                .tracks
                .iter()
                .flat_map(|t| t.clips.iter())
                .find(|c| c.id == cid);
            if let Some(c) = clip_opt {
                if c.kind != ClipKind::Audio {
                    let (scale, pos_x, pos_y, rotate, cl, cr, ct, cb) =
                        evaluate_clip_transform_at(c, playhead_ns);
                    transform_overlay.set_transform(scale, pos_x, pos_y);
                    transform_overlay.set_rotation(rotate);
                    transform_overlay.set_crop(cl, cr, ct, cb);
                    transform_overlay.set_clip_selected(true);
                } else {
                    transform_overlay.set_clip_selected(false);
                }
            } else {
                transform_overlay.set_clip_selected(false);
            }
        }
        None => {
            transform_overlay.set_clip_selected(false);
        }
    }
}

fn seek_playhead_and_notify(
    timeline_state: &Rc<RefCell<TimelineState>>,
    timeline_panel_cell: &Rc<RefCell<Option<gtk::Widget>>>,
    timeline_pos_ns: u64,
) {
    let seek_cb = {
        let mut st = timeline_state.borrow_mut();
        st.playhead_ns = timeline_pos_ns;
        st.on_seek.clone()
    };
    if let Some(cb) = seek_cb {
        cb(timeline_pos_ns);
    }
    if let Some(ref w) = *timeline_panel_cell.borrow() {
        w.queue_draw();
    }
}

#[allow(deprecated)]
fn present_go_to_timecode_dialog(
    window: &gtk::ApplicationWindow,
    project: &Rc<RefCell<Project>>,
    timeline_state: &Rc<RefCell<TimelineState>>,
    timeline_panel_cell: &Rc<RefCell<Option<gtk::Widget>>>,
) {
    let dialog = gtk::Dialog::builder()
        .title("Go to Timecode")
        .transient_for(window)
        .modal(true)
        .default_width(360)
        .build();
    dialog.add_button("Cancel", gtk::ResponseType::Cancel);
    dialog.add_button("Go", gtk::ResponseType::Accept);
    dialog.set_default_response(gtk::ResponseType::Accept);

    let content = dialog.content_area();
    let hint = gtk::Label::new(Some("Format: HH:MM:SS:FF (or MM:SS:FF)"));
    hint.set_halign(gtk::Align::Start);
    hint.add_css_class("dim-label");
    content.append(&hint);

    let entry = gtk::Entry::new();
    entry.set_placeholder_text(Some("00:00:00:00"));
    entry.set_activates_default(true);
    {
        let fr = project.borrow().frame_rate.clone();
        let current = timeline_state.borrow().playhead_ns;
        entry.set_text(&timecode::format_ns_as_timecode(current, &fr));
    }
    content.append(&entry);

    let error_label = gtk::Label::new(None);
    error_label.set_halign(gtk::Align::Start);
    error_label.set_wrap(true);
    error_label.add_css_class("error");
    error_label.set_visible(false);
    content.append(&error_label);

    entry.connect_changed({
        let error_label = error_label.clone();
        move |_| {
            error_label.set_visible(false);
        }
    });

    let entry_for_response = entry.clone();
    dialog.connect_response({
        let project = project.clone();
        let timeline_state = timeline_state.clone();
        let timeline_panel_cell = timeline_panel_cell.clone();
        let error_label = error_label.clone();
        let window = window.clone();
        move |d, resp| {
            if resp != gtk::ResponseType::Accept {
                d.close();
                return;
            }
            let input = entry_for_response.text().to_string();
            let (frame_rate, duration) = {
                let proj = project.borrow();
                (proj.frame_rate.clone(), proj.duration())
            };
            match timecode::parse_timecode_to_ns(&input, &frame_rate) {
                Ok(parsed_ns) => {
                    let target_ns = parsed_ns.min(duration);
                    seek_playhead_and_notify(&timeline_state, &timeline_panel_cell, target_ns);
                    if parsed_ns > duration {
                        flash_window_status_title(
                            &window,
                            &project,
                            "Timecode past project end; jumped to end",
                        );
                    } else {
                        let tc = timecode::format_ns_as_timecode(target_ns, &frame_rate);
                        flash_window_status_title(&window, &project, &format!("Jumped to {tc}"));
                    }
                    d.close();
                }
                Err(err) => {
                    error_label.set_text(&err);
                    error_label.set_visible(true);
                }
            }
        }
    });

    dialog.present();
    entry.grab_focus();
    entry.select_region(0, -1);
}

fn lookup_source_timecode_base_ns(
    library: &[MediaItem],
    project: &Project,
    source_path: &str,
) -> Option<u64> {
    library
        .iter()
        .find(|item| item.source_path == source_path)
        .and_then(|item| item.source_timecode_base_ns)
        .or_else(|| {
            project
                .tracks
                .iter()
                .flat_map(|track| track.clips.iter())
                .find(|clip| clip.source_path == source_path)
                .and_then(|clip| clip.source_timecode_base_ns)
        })
}

#[derive(Clone, Copy)]
struct SourcePlacementInfo {
    is_audio_only: bool,
    has_audio: bool,
    source_timecode_base_ns: Option<u64>,
}

fn lookup_source_placement_info(
    library: &[MediaItem],
    project: &Project,
    source_path: &str,
) -> SourcePlacementInfo {
    let item = library.iter().find(|item| item.source_path == source_path);
    let mut is_audio_only = item.map(|item| item.is_audio_only).unwrap_or(false);
    let mut has_audio = item.map(|item| item.has_audio).unwrap_or(false);

    if item.is_none() || (!has_audio && !is_audio_only) {
        let uri = format!("file://{source_path}");
        let metadata = media_browser::probe_media_metadata(&uri);
        is_audio_only = metadata.is_audio_only;
        has_audio = metadata.has_audio;
    }

    SourcePlacementInfo {
        is_audio_only,
        has_audio,
        source_timecode_base_ns: lookup_source_timecode_base_ns(library, project, source_path),
    }
}

fn find_preferred_track_index_by_id(
    project: &Project,
    preferred_track_id: Option<&str>,
    kind: TrackKind,
) -> Option<usize> {
    if let Some(track_id) = preferred_track_id {
        if let Some((idx, _)) = project
            .tracks
            .iter()
            .enumerate()
            .find(|(_, track)| track.id == track_id && track.kind == kind)
        {
            return Some(idx);
        }
    }

    project
        .tracks
        .iter()
        .enumerate()
        .find(|(_, track)| track.kind == kind)
        .map(|(idx, _)| idx)
}

fn find_preferred_track_index_by_index(
    project: &Project,
    preferred_index: Option<usize>,
    kind: TrackKind,
) -> Option<usize> {
    if let Some(idx) = preferred_index {
        if project
            .tracks
            .get(idx)
            .is_some_and(|track| track.kind == kind)
        {
            return Some(idx);
        }
    }

    project
        .tracks
        .iter()
        .enumerate()
        .find(|(_, track)| track.kind == kind)
        .map(|(idx, _)| idx)
}

#[derive(Clone, Debug)]
struct SourcePlacementTarget {
    track_index: usize,
    clip_kind: ClipKind,
    mute_embedded_audio: bool,
}

#[derive(Clone, Debug, Default)]
struct SourcePlacementPlan {
    targets: Vec<SourcePlacementTarget>,
    link_group_id: Option<String>,
}

impl SourcePlacementPlan {
    fn uses_linked_pair(&self) -> bool {
        self.link_group_id.is_some()
    }
}

fn build_source_placement_plan_by_track_id(
    project: &Project,
    preferred_track_id: Option<&str>,
    source_info: SourcePlacementInfo,
    source_monitor_auto_link_av: bool,
) -> SourcePlacementPlan {
    let auto_link_pair =
        source_monitor_auto_link_av && !source_info.is_audio_only && source_info.has_audio;
    let video_track_idx =
        find_preferred_track_index_by_id(project, preferred_track_id, TrackKind::Video);
    let audio_track_idx =
        find_preferred_track_index_by_id(project, preferred_track_id, TrackKind::Audio);

    if auto_link_pair {
        if let (Some(video_idx), Some(audio_idx)) = (video_track_idx, audio_track_idx) {
            return SourcePlacementPlan {
                targets: vec![
                    SourcePlacementTarget {
                        track_index: video_idx,
                        clip_kind: ClipKind::Video,
                        mute_embedded_audio: true,
                    },
                    SourcePlacementTarget {
                        track_index: audio_idx,
                        clip_kind: ClipKind::Audio,
                        mute_embedded_audio: false,
                    },
                ],
                link_group_id: Some(uuid::Uuid::new_v4().to_string()),
            };
        }

        if let Some(video_idx) = video_track_idx {
            return SourcePlacementPlan {
                targets: vec![SourcePlacementTarget {
                    track_index: video_idx,
                    clip_kind: ClipKind::Video,
                    mute_embedded_audio: false,
                }],
                link_group_id: None,
            };
        }

        if let Some(audio_idx) = audio_track_idx {
            return SourcePlacementPlan {
                targets: vec![SourcePlacementTarget {
                    track_index: audio_idx,
                    clip_kind: ClipKind::Audio,
                    mute_embedded_audio: false,
                }],
                link_group_id: None,
            };
        }

        return SourcePlacementPlan::default();
    }

    let target_kind = if source_info.is_audio_only {
        TrackKind::Audio
    } else {
        TrackKind::Video
    };
    let clip_kind = if target_kind == TrackKind::Audio {
        ClipKind::Audio
    } else {
        ClipKind::Video
    };
    if let Some(track_idx) =
        find_preferred_track_index_by_id(project, preferred_track_id, target_kind)
    {
        return SourcePlacementPlan {
            targets: vec![SourcePlacementTarget {
                track_index: track_idx,
                clip_kind,
                mute_embedded_audio: false,
            }],
            link_group_id: None,
        };
    }

    SourcePlacementPlan::default()
}

fn build_source_placement_plan_by_track_index(
    project: &Project,
    preferred_track_index: Option<usize>,
    source_info: SourcePlacementInfo,
    source_monitor_auto_link_av: bool,
) -> SourcePlacementPlan {
    let preferred_track_id = preferred_track_index
        .and_then(|idx| project.tracks.get(idx))
        .map(|track| track.id.as_str());
    build_source_placement_plan_by_track_id(
        project,
        preferred_track_id,
        source_info,
        source_monitor_auto_link_av,
    )
}

fn build_source_clips_for_plan(
    plan: &SourcePlacementPlan,
    source_path: &str,
    source_in_ns: u64,
    source_out_ns: u64,
    timeline_start_ns: u64,
    source_timecode_base_ns: Option<u64>,
) -> Vec<(usize, Clip)> {
    plan.targets
        .iter()
        .map(|target| {
            let mut clip = build_source_clip(
                source_path,
                source_in_ns,
                source_out_ns,
                timeline_start_ns,
                target.clip_kind.clone(),
                source_timecode_base_ns,
                plan.link_group_id.as_deref(),
            );
            if target.mute_embedded_audio {
                clip.volume = 0.0;
            }
            (target.track_index, clip)
        })
        .collect()
}

fn build_source_clip(
    source_path: &str,
    source_in_ns: u64,
    source_out_ns: u64,
    timeline_start_ns: u64,
    kind: ClipKind,
    source_timecode_base_ns: Option<u64>,
    link_group_id: Option<&str>,
) -> Clip {
    let mut clip = Clip::new(
        source_path.to_string(),
        source_out_ns,
        timeline_start_ns,
        kind,
    );
    clip.source_in = source_in_ns;
    clip.source_out = source_out_ns;
    clip.source_timecode_base_ns = source_timecode_base_ns;
    clip.link_group_id = link_group_id.map(str::to_string);
    clip
}

fn add_clip_to_track(
    track: &mut crate::model::track::Track,
    clip: Clip,
    magnetic_mode: bool,
) -> TrackClipsChange {
    let old_clips = track.clips.clone();
    let track_id = track.id.clone();
    track.add_clip(clip);
    if magnetic_mode {
        track.compact_gap_free();
    }
    TrackClipsChange {
        track_id,
        old_clips,
        new_clips: track.clips.clone(),
    }
}

fn insert_clip_at_playhead_on_track(
    track: &mut crate::model::track::Track,
    clip: Clip,
    playhead: u64,
    magnetic_mode: bool,
) -> TrackClipsChange {
    let old_clips = track.clips.clone();
    let track_id = track.id.clone();
    let clip_duration = clip.duration();
    for existing in &mut track.clips {
        if existing.timeline_start >= playhead {
            existing.timeline_start += clip_duration;
        }
    }
    track.add_clip(clip);
    if magnetic_mode {
        track.compact_gap_free();
    }
    TrackClipsChange {
        track_id,
        old_clips,
        new_clips: track.clips.clone(),
    }
}

fn overwrite_clip_range_on_track(
    track: &mut crate::model::track::Track,
    clip: Clip,
    range_start: u64,
    range_end: u64,
    magnetic_mode: bool,
) -> TrackClipsChange {
    let old_clips = track.clips.clone();
    let track_id = track.id.clone();
    let mut kept: Vec<Clip> = Vec::new();
    for existing in track.clips.drain(..) {
        let c_start = existing.timeline_start;
        let c_end = existing.timeline_end();
        if c_end <= range_start || c_start >= range_end {
            kept.push(existing);
        } else if c_start >= range_start && c_end <= range_end {
            // Fully contained — remove.
        } else if c_start < range_start && c_end > range_end {
            let mut left = existing.clone();
            left.source_out = left.source_in + (range_start - c_start);
            let mut right = existing;
            let trim_left = range_end - right.timeline_start;
            right.source_in += trim_left;
            right.timeline_start = range_end;
            kept.push(left);
            kept.push(right);
        } else if c_start < range_start {
            let mut trimmed = existing;
            trimmed.source_out = trimmed.source_in + (range_start - trimmed.timeline_start);
            kept.push(trimmed);
        } else {
            let mut trimmed = existing;
            let trim_amount = range_end - trimmed.timeline_start;
            trimmed.source_in += trim_amount;
            trimmed.timeline_start = range_end;
            kept.push(trimmed);
        }
    }
    track.clips = kept;
    track.add_clip(clip);
    if magnetic_mode {
        track.compact_gap_free();
    }
    TrackClipsChange {
        track_id,
        old_clips,
        new_clips: track.clips.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn source_monitor_plan_links_video_and_audio_when_both_tracks_exist() {
        let project = Project::new("Test");
        let preferred_audio_track_id = project
            .tracks
            .iter()
            .find(|track| track.kind == TrackKind::Audio)
            .map(|track| track.id.clone())
            .expect("audio track should exist");
        let source_info = SourcePlacementInfo {
            is_audio_only: false,
            has_audio: true,
            source_timecode_base_ns: Some(42),
        };

        let plan = build_source_placement_plan_by_track_id(
            &project,
            Some(preferred_audio_track_id.as_str()),
            source_info,
            true,
        );

        assert_eq!(plan.targets.len(), 2);
        assert!(plan.link_group_id.is_some());
        assert!(plan
            .targets
            .iter()
            .any(|target| target.clip_kind == ClipKind::Video));
        assert!(plan
            .targets
            .iter()
            .any(|target| target.clip_kind == ClipKind::Audio));

        let created = build_source_clips_for_plan(
            &plan,
            "/tmp/source.mp4",
            100,
            300,
            1_000,
            source_info.source_timecode_base_ns,
        );
        let link_group_id = plan.link_group_id.as_deref();
        assert_eq!(created.len(), 2);
        assert!(created
            .iter()
            .all(|(_, clip)| clip.link_group_id.as_deref() == link_group_id));
        assert!(created
            .iter()
            .all(|(_, clip)| clip.timeline_start == 1_000 && clip.source_in == 100));
        let linked_video = created
            .iter()
            .find(|(_, clip)| clip.kind == ClipKind::Video)
            .expect("linked video clip should exist");
        let linked_audio = created
            .iter()
            .find(|(_, clip)| clip.kind == ClipKind::Audio)
            .expect("linked audio clip should exist");
        assert_eq!(linked_video.1.volume, 0.0);
        assert_eq!(linked_audio.1.volume, 1.0);
    }

    #[test]
    fn source_monitor_plan_with_auto_link_disabled_uses_single_video_clip_for_av_sources() {
        let project = Project::new("Test");
        let source_info = SourcePlacementInfo {
            is_audio_only: false,
            has_audio: true,
            source_timecode_base_ns: None,
        };

        let plan = build_source_placement_plan_by_track_id(&project, None, source_info, false);

        assert_eq!(plan.targets.len(), 1);
        assert_eq!(plan.targets[0].clip_kind, ClipKind::Video);
        assert!(plan.link_group_id.is_none());
    }

    #[test]
    fn source_monitor_plan_falls_back_to_single_kind_when_pair_not_possible() {
        let mut project_video_only = Project::new("Test");
        project_video_only
            .tracks
            .retain(|track| track.kind == TrackKind::Video);
        let mut project_audio_only = Project::new("Test");
        project_audio_only
            .tracks
            .retain(|track| track.kind == TrackKind::Audio);
        let source_info = SourcePlacementInfo {
            is_audio_only: false,
            has_audio: true,
            source_timecode_base_ns: None,
        };

        let video_only_plan =
            build_source_placement_plan_by_track_id(&project_video_only, None, source_info, true);
        assert_eq!(video_only_plan.targets.len(), 1);
        assert_eq!(video_only_plan.targets[0].clip_kind, ClipKind::Video);
        assert!(video_only_plan.link_group_id.is_none());

        let audio_only_plan =
            build_source_placement_plan_by_track_id(&project_audio_only, None, source_info, true);
        assert_eq!(audio_only_plan.targets.len(), 1);
        assert_eq!(audio_only_plan.targets[0].clip_kind, ClipKind::Audio);
        assert!(audio_only_plan.link_group_id.is_none());
    }

    #[test]
    fn source_monitor_plan_handles_audio_only_and_silent_video_sources() {
        let project = Project::new("Test");
        let audio_only = SourcePlacementInfo {
            is_audio_only: true,
            has_audio: true,
            source_timecode_base_ns: None,
        };
        let silent_video = SourcePlacementInfo {
            is_audio_only: false,
            has_audio: false,
            source_timecode_base_ns: None,
        };

        let audio_plan = build_source_placement_plan_by_track_id(&project, None, audio_only, true);
        assert_eq!(audio_plan.targets.len(), 1);
        assert_eq!(audio_plan.targets[0].clip_kind, ClipKind::Audio);
        assert!(audio_plan.link_group_id.is_none());

        let silent_video_plan =
            build_source_placement_plan_by_track_id(&project, None, silent_video, true);
        assert_eq!(silent_video_plan.targets.len(), 1);
        assert_eq!(silent_video_plan.targets[0].clip_kind, ClipKind::Video);
        assert!(silent_video_plan.link_group_id.is_none());
    }

    #[test]
    fn source_monitor_plan_returns_empty_when_no_matching_track_exists() {
        let mut project = Project::new("Test");
        project.tracks.clear();
        let source_info = SourcePlacementInfo {
            is_audio_only: false,
            has_audio: true,
            source_timecode_base_ns: None,
        };

        let plan = build_source_placement_plan_by_track_id(&project, None, source_info, true);
        assert!(plan.targets.is_empty());
        assert!(plan.link_group_id.is_none());
    }

    #[test]
    fn mcp_track_index_plan_matches_track_id_for_silent_video_audio_target() {
        let project = Project::new("Test");
        let preferred_audio_track = project
            .tracks
            .iter()
            .enumerate()
            .find(|(_, track)| track.kind == TrackKind::Audio)
            .expect("audio track should exist");
        let source_info = SourcePlacementInfo {
            is_audio_only: false,
            has_audio: false,
            source_timecode_base_ns: None,
        };

        let by_track_id = build_source_placement_plan_by_track_id(
            &project,
            Some(preferred_audio_track.1.id.as_str()),
            source_info,
            true,
        );
        let by_track_index = build_source_placement_plan_by_track_index(
            &project,
            Some(preferred_audio_track.0),
            source_info,
            true,
        );

        assert_eq!(by_track_index.targets.len(), 1);
        assert_eq!(
            by_track_index.targets[0].track_index,
            by_track_id.targets[0].track_index
        );
        assert_eq!(by_track_index.targets[0].clip_kind, ClipKind::Video);
        assert_eq!(by_track_index.link_group_id, by_track_id.link_group_id);
    }

    #[test]
    fn mcp_track_index_plan_uses_audio_for_audio_only_sources() {
        let project = Project::new("Test");
        let preferred_video_track_idx = project
            .tracks
            .iter()
            .enumerate()
            .find(|(_, track)| track.kind == TrackKind::Video)
            .map(|(idx, _)| idx)
            .expect("video track should exist");
        let preferred_audio_track_idx = project
            .tracks
            .iter()
            .enumerate()
            .find(|(_, track)| track.kind == TrackKind::Audio)
            .map(|(idx, _)| idx)
            .expect("audio track should exist");
        let source_info = SourcePlacementInfo {
            is_audio_only: true,
            has_audio: true,
            source_timecode_base_ns: None,
        };

        let plan = build_source_placement_plan_by_track_index(
            &project,
            Some(preferred_video_track_idx),
            source_info,
            true,
        );
        assert_eq!(plan.targets.len(), 1);
        assert_eq!(plan.targets[0].track_index, preferred_audio_track_idx);
        assert_eq!(plan.targets[0].clip_kind, ClipKind::Audio);
        assert!(plan.link_group_id.is_none());
    }

    #[test]
    fn mcp_track_index_plan_returns_empty_without_matching_tracks() {
        let mut project = Project::new("Test");
        project
            .tracks
            .retain(|track| track.kind == TrackKind::Video);
        let source_info = SourcePlacementInfo {
            is_audio_only: true,
            has_audio: true,
            source_timecode_base_ns: None,
        };

        let plan = build_source_placement_plan_by_track_index(&project, Some(0), source_info, true);
        assert!(plan.targets.is_empty());
        assert!(plan.link_group_id.is_none());
    }

    #[test]
    fn linked_insert_and_overwrite_keep_pair_aligned_and_linked() {
        let mut project = Project::new("Test");
        let playhead = 1_000_000_000;
        let source_in = 0;
        let source_out = 500_000_000;
        let source_info = SourcePlacementInfo {
            is_audio_only: false,
            has_audio: true,
            source_timecode_base_ns: None,
        };

        project.tracks[0].add_clip(build_source_clip(
            "/tmp/existing-video.mp4",
            0,
            1_000_000_000,
            1_500_000_000,
            ClipKind::Video,
            None,
            None,
        ));
        project.tracks[1].add_clip(build_source_clip(
            "/tmp/existing-audio.wav",
            0,
            1_000_000_000,
            1_500_000_000,
            ClipKind::Audio,
            None,
            None,
        ));

        let insert_plan =
            build_source_placement_plan_by_track_id(&project, None, source_info, true);
        let insert_link_group_id = insert_plan
            .link_group_id
            .clone()
            .expect("linked insert plan");
        for (track_idx, clip) in build_source_clips_for_plan(
            &insert_plan,
            "/tmp/source.mp4",
            source_in,
            source_out,
            playhead,
            None,
        ) {
            let _ = insert_clip_at_playhead_on_track(
                &mut project.tracks[track_idx],
                clip,
                playhead,
                false,
            );
        }

        let inserted: Vec<_> = project
            .tracks
            .iter()
            .flat_map(|track| track.clips.iter())
            .filter(|clip| clip.link_group_id.as_deref() == Some(insert_link_group_id.as_str()))
            .collect();
        assert_eq!(inserted.len(), 2);
        assert!(inserted.iter().all(|clip| clip.timeline_start == playhead));
        assert_eq!(
            project.tracks[0]
                .clips
                .iter()
                .find(|clip| clip.source_path == "/tmp/existing-video.mp4")
                .map(|clip| clip.timeline_start),
            Some(2_000_000_000)
        );
        assert_eq!(
            project.tracks[1]
                .clips
                .iter()
                .find(|clip| clip.source_path == "/tmp/existing-audio.wav")
                .map(|clip| clip.timeline_start),
            Some(2_000_000_000)
        );

        let range_start = 250_000_000;
        let range_end = 750_000_000;
        project.tracks[0].clips.clear();
        project.tracks[1].clips.clear();
        project.tracks[0].add_clip(build_source_clip(
            "/tmp/existing-video-overwrite.mp4",
            0,
            2_000_000_000,
            0,
            ClipKind::Video,
            None,
            None,
        ));
        project.tracks[1].add_clip(build_source_clip(
            "/tmp/existing-audio-overwrite.wav",
            0,
            2_000_000_000,
            0,
            ClipKind::Audio,
            None,
            None,
        ));

        let overwrite_plan =
            build_source_placement_plan_by_track_id(&project, None, source_info, true);
        let overwrite_link_group_id = overwrite_plan
            .link_group_id
            .clone()
            .expect("linked overwrite plan");
        for (track_idx, clip) in build_source_clips_for_plan(
            &overwrite_plan,
            "/tmp/source.mp4",
            source_in,
            source_out,
            range_start,
            None,
        ) {
            let _ = overwrite_clip_range_on_track(
                &mut project.tracks[track_idx],
                clip,
                range_start,
                range_end,
                false,
            );
        }

        let overwritten: Vec<_> = project
            .tracks
            .iter()
            .flat_map(|track| track.clips.iter())
            .filter(|clip| clip.link_group_id.as_deref() == Some(overwrite_link_group_id.as_str()))
            .collect();
        assert_eq!(overwritten.len(), 2);
        assert!(overwritten
            .iter()
            .all(|clip| clip.timeline_start == range_start));
        assert!(project.tracks.iter().all(|track| track.clips.len() == 3));
    }
}

fn align_grouped_clips_by_timecode_in_project(
    project: &mut Project,
    clip_ids: &[String],
) -> Result<(usize, usize), String> {
    if clip_ids.is_empty() {
        return Err("clip_ids must contain at least one clip id".to_string());
    }

    let clip_id_set: HashSet<&str> = clip_ids.iter().map(|id| id.as_str()).collect();
    let target_groups: HashSet<String> = project
        .tracks
        .iter()
        .flat_map(|track| track.clips.iter())
        .filter(|clip| clip_id_set.contains(clip.id.as_str()))
        .filter_map(|clip| clip.group_id.clone())
        .collect();

    if target_groups.is_empty() {
        return Err("No grouped clips found for the provided clip_ids".to_string());
    }

    let mut assignments: HashMap<String, u64> = HashMap::new();
    let mut aligned_group_count = 0usize;

    for group_id in &target_groups {
        let members: Vec<_> = project
            .tracks
            .iter()
            .flat_map(|track| track.clips.iter())
            .filter(|clip| clip.group_id.as_deref() == Some(group_id.as_str()))
            .map(|clip| {
                (
                    clip.id.clone(),
                    clip.timeline_start,
                    clip.source_timecode_start_ns(),
                )
            })
            .collect();

        if members.len() < 2 {
            continue;
        }
        if members
            .iter()
            .any(|(_, _, source_timecode_start_ns)| source_timecode_start_ns.is_none())
        {
            return Err(format!(
                "Grouped clips in group {group_id} are missing source timecode metadata"
            ));
        }

        let anchor = clip_ids
            .iter()
            .find_map(|requested_id| {
                members
                    .iter()
                    .find(|(clip_id, _, _)| clip_id == requested_id)
                    .cloned()
            })
            .or_else(|| {
                members
                    .iter()
                    .min_by_key(|(_, timeline_start, source_timecode_start_ns)| {
                        (source_timecode_start_ns.unwrap_or(0), *timeline_start)
                    })
                    .cloned()
            })
            .ok_or_else(|| format!("No anchor clip found for group {group_id}"))?;

        let (_, anchor_timeline_start, anchor_source_timecode_start_ns) = anchor;
        let anchor_source_timecode_start_ns = anchor_source_timecode_start_ns.unwrap_or(0);

        let mut proposed: Vec<(String, i128)> = members
            .iter()
            .map(|(clip_id, _, source_timecode_start_ns)| {
                (
                    clip_id.clone(),
                    i128::from(anchor_timeline_start)
                        + i128::from(source_timecode_start_ns.unwrap_or(0))
                        - i128::from(anchor_source_timecode_start_ns),
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

        aligned_group_count += 1;
        for (clip_id, new_start) in proposed {
            assignments.insert(clip_id, new_start.max(0) as u64);
        }
    }

    if assignments.is_empty() {
        return Err(
            "No grouped clips with source timecode metadata were eligible for alignment"
                .to_string(),
        );
    }

    let mut aligned_clip_count = 0usize;
    for track in &mut project.tracks {
        for clip in &mut track.clips {
            if let Some(new_start) = assignments.get(&clip.id) {
                if clip.timeline_start != *new_start {
                    clip.timeline_start = *new_start;
                    aligned_clip_count += 1;
                }
            }
        }
    }

    if aligned_clip_count == 0 {
        return Err("Grouped clips were already aligned by timecode".to_string());
    }

    Ok((aligned_group_count, aligned_clip_count))
}

/// Apply audio sync results to the project: reposition non-anchor clips
/// relative to the anchor clip's timeline_start using the computed offsets.
fn apply_audio_sync_results(
    results: &[(String, i64, f32)],
    project: &Rc<RefCell<Project>>,
    timeline_state: &Rc<RefCell<crate::ui::timeline::TimelineState>>,
    on_project_changed: &Rc<dyn Fn()>,
    window: Option<&gtk::ApplicationWindow>,
) {
    use crate::undo::SetTrackClipsCommand;

    const MIN_CONFIDENCE: f32 = 3.0;

    // Detect "no change" when all offsets are 0
    if results.iter().all(|(_, offset, _)| *offset == 0) {
        if let Some(win) = window {
            flash_window_status_title(
                win,
                project,
                "Audio sync: clips appear already aligned (offset = 0)",
            );
        }
        return;
    }

    // Check all results for minimum confidence
    let low_confidence = results.iter().any(|(_, _, c)| *c < MIN_CONFIDENCE);
    if low_confidence {
        if let Some(win) = window {
            flash_window_status_title(
                win,
                project,
                &format!(
                    "Audio sync failed — confidence too low ({:.1})",
                    results
                        .iter()
                        .map(|(_, _, c)| *c)
                        .fold(f32::INFINITY, f32::min)
                ),
            );
        }
        return;
    }

    // Find the anchor clip's timeline_start (first selected clip that wasn't synced)
    let synced_ids: HashSet<&str> = results.iter().map(|(id, _, _)| id.as_str()).collect();
    let anchor_timeline_start = {
        let proj = project.borrow();
        let st = timeline_state.borrow();
        let all_ids = st.selected_ids_or_primary();
        proj.tracks
            .iter()
            .flat_map(|t| t.clips.iter())
            .find(|c| all_ids.contains(&c.id) && !synced_ids.contains(c.id.as_str()))
            .map(|c| c.timeline_start)
            .unwrap_or(0)
    };

    // Build new clip positions
    let mut assignments: HashMap<String, u64> = HashMap::new();
    for (clip_id, offset_ns, _) in results {
        let new_start = (anchor_timeline_start as i64 + offset_ns).max(0) as u64;
        assignments.insert(clip_id.clone(), new_start);
    }

    if assignments.is_empty() {
        return;
    }

    // Apply changes via undo-friendly SetTrackClipsCommand
    {
        let mut st = timeline_state.borrow_mut();
        let proj_rc = st.project.clone();

        // Collect track updates first (avoids borrowing proj as both immutable and mutable)
        let track_updates: Vec<(String, Vec<Clip>, Vec<Clip>)> = {
            let proj = proj_rc.borrow();
            proj.tracks
                .iter()
                .filter_map(|track| {
                    let old_clips = track.clips.clone();
                    let mut new_clips = old_clips.clone();
                    let mut changed = false;
                    for clip in &mut new_clips {
                        if let Some(&new_start) = assignments.get(&clip.id) {
                            if clip.timeline_start != new_start {
                                clip.timeline_start = new_start;
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
                .collect()
        };

        let mut proj = proj_rc.borrow_mut();
        for (track_id, old_clips, new_clips) in track_updates {
            let cmd = SetTrackClipsCommand {
                track_id,
                old_clips,
                new_clips,
                label: "Sync clips by audio".to_string(),
            };
            st.history.execute(Box::new(cmd), &mut proj);
        }
        proj.dirty = true;
    }

    on_project_changed();

    if let Some(win) = window {
        flash_window_status_title(win, project, "Audio sync complete");
    }
}

fn export_displayed_frame_to_image(
    prog_player: &Rc<RefCell<ProgramPlayer>>,
    out_path: &std::path::Path,
) -> Result<&'static str, String> {
    let ext = out_path
        .extension()
        .and_then(|e| e.to_str())
        .map(|s| s.to_ascii_lowercase())
        .ok_or_else(|| "Missing output extension (.png, .jpg, .jpeg, or .ppm)".to_string())?;
    let out_str = out_path
        .to_str()
        .ok_or_else(|| "Output path must be valid UTF-8".to_string())?;
    if ext == "ppm" {
        prog_player
            .borrow_mut()
            .export_displayed_frame_ppm(out_str)
            .map_err(|e| e.to_string())?;
        return Ok("ppm");
    }
    if ext != "png" && ext != "jpg" && ext != "jpeg" {
        return Err("Unsupported extension; use .png, .jpg, .jpeg, or .ppm".to_string());
    }

    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let tmp_ppm = std::env::temp_dir().join(format!(
        "ultimateslice-frame-{}-{stamp}.ppm",
        std::process::id()
    ));
    let tmp_str = tmp_ppm
        .to_str()
        .ok_or_else(|| "Temporary path is not valid UTF-8".to_string())?;

    prog_player
        .borrow_mut()
        .export_displayed_frame_ppm(tmp_str)
        .map_err(|e| e.to_string())?;

    let ffmpeg = crate::media::export::find_ffmpeg().map_err(|e| e.to_string())?;
    let status = std::process::Command::new(&ffmpeg)
        .arg("-y")
        .arg("-loglevel")
        .arg("error")
        .arg("-i")
        .arg(&tmp_ppm)
        .arg("-frames:v")
        .arg("1")
        .arg(out_path)
        .status()
        .map_err(|e| format!("Failed to start ffmpeg: {e}"))?;
    let _ = std::fs::remove_file(&tmp_ppm);
    if !status.success() {
        return Err("ffmpeg failed while encoding still frame".to_string());
    }
    Ok(if ext == "png" { "png" } else { "jpeg" })
}

fn auto_preview_divisor(
    project_width: u32,
    project_height: u32,
    canvas_width: i32,
    canvas_height: i32,
    current_divisor: u32,
) -> u32 {
    let cw = canvas_width.max(1) as f64;
    let ch = canvas_height.max(1) as f64;
    let pw = project_width.max(2) as f64;
    let ph = project_height.max(2) as f64;
    let ratio = (pw / cw).max(ph / ch);
    let cur = match current_divisor {
        1 | 2 | 4 => current_divisor,
        _ => 1,
    };
    match cur {
        1 => {
            if ratio >= 1.9 {
                2
            } else {
                1
            }
        }
        2 => {
            if ratio >= 3.6 {
                4
            } else if ratio <= 1.35 {
                1
            } else {
                2
            }
        }
        4 => {
            if ratio <= 2.4 {
                2
            } else {
                4
            }
        }
        _ => 1,
    }
}

fn proxy_scale_for_mode(
    mode: &crate::ui_state::ProxyMode,
) -> crate::media::proxy_cache::ProxyScale {
    match mode {
        crate::ui_state::ProxyMode::QuarterRes => crate::media::proxy_cache::ProxyScale::Quarter,
        _ => crate::media::proxy_cache::ProxyScale::Half,
    }
}

fn ready_proxy_path_for_source(
    cache: &crate::media::proxy_cache::ProxyCache,
    source_path: &str,
    lut_path: Option<&str>,
) -> Option<String> {
    cache.get(source_path, lut_path).and_then(|proxy_path| {
        std::fs::metadata(proxy_path)
            .ok()
            .filter(|m| m.len() > 0)
            .map(|_| proxy_path.clone())
    })
}

fn collect_unique_clip_sources(project: &Project) -> Vec<(String, Option<String>)> {
    let mut seen: HashSet<(String, Option<String>)> = HashSet::new();
    project
        .tracks
        .iter()
        .filter(|t| t.kind == TrackKind::Video)
        .flat_map(|t| t.clips.iter())
        .filter_map(|c| {
            let key = (c.source_path.clone(), c.lut_path.clone());
            if seen.insert(key.clone()) {
                Some(key)
            } else {
                None
            }
        })
        .collect()
}

fn collect_unique_lut_clip_sources(project: &Project) -> Vec<(String, Option<String>)> {
    let mut seen: HashSet<(String, Option<String>)> = HashSet::new();
    project
        .tracks
        .iter()
        .filter(|t| t.kind == TrackKind::Video)
        .flat_map(|t| t.clips.iter())
        .filter_map(|c| {
            let lut = c.lut_path.as_ref()?;
            if lut.is_empty() {
                return None;
            }
            let key = (c.source_path.clone(), Some(lut.clone()));
            if seen.insert(key.clone()) {
                Some(key)
            } else {
                None
            }
        })
        .collect()
}

fn collect_near_playhead_clip_sources(
    project: &Project,
    playhead_ns: u64,
    window_ns: u64,
    max_items: usize,
) -> Vec<(String, Option<String>)> {
    if max_items == 0 {
        return Vec::new();
    }
    let window_start = playhead_ns.saturating_sub(window_ns);
    let window_end = playhead_ns.saturating_add(window_ns);

    let mut candidates: Vec<(u64, u64, String, Option<String>)> = project
        .tracks
        .iter()
        .filter(|t| t.kind == TrackKind::Video)
        .flat_map(|t| t.clips.iter())
        .filter(|c| c.timeline_end() >= window_start && c.timeline_start <= window_end)
        .map(|c| {
            let clip_end = c.timeline_end();
            let distance = if playhead_ns < c.timeline_start {
                c.timeline_start.saturating_sub(playhead_ns)
            } else if playhead_ns > clip_end {
                playhead_ns.saturating_sub(clip_end)
            } else {
                0
            };
            (
                distance,
                c.timeline_start,
                c.source_path.clone(),
                c.lut_path.clone(),
            )
        })
        .collect();

    candidates.sort_by_key(|(distance, timeline_start, _, _)| (*distance, *timeline_start));

    let mut out = Vec::new();
    let mut seen: HashSet<(String, Option<String>)> = HashSet::new();
    for (_, _, path, lut) in candidates {
        let key = (path, lut);
        if seen.insert(key.clone()) {
            out.push(key);
            if out.len() >= max_items {
                break;
            }
        }
    }
    out
}

/// Build and show the main application window.
pub fn build_window(
    app: &gtk::Application,
    mcp_enabled: bool,
    startup_project_path: Option<String>,
) {
    let window = ApplicationWindow::builder()
        .application(app)
        .title("UltimateSlice")
        .default_width(1440)
        .default_height(900)
        .build();

    let project = Rc::new(RefCell::new(Project::new("Untitled")));

    // Shared media library (items visible in the browser, not yet on timeline)
    let library: Rc<RefCell<Vec<MediaItem>>> = Rc::new(RefCell::new(Vec::new()));
    let preferences_state = Rc::new(RefCell::new(crate::ui_state::load_preferences_state()));

    // MCP command channel — created unconditionally so the socket transport can
    // be toggled at runtime via Preferences without restarting.
    let (mcp_sender, mcp_receiver) = std::sync::mpsc::channel::<crate::mcp::McpCommand>();
    let mcp_sender = Rc::new(mcp_sender);
    let mcp_receiver = Rc::new(RefCell::new(Some(mcp_receiver))); // taken once in the MCP block
    let mcp_socket_stop: Rc<RefCell<Option<std::sync::Arc<std::sync::atomic::AtomicBool>>>> =
        Rc::new(RefCell::new(None));

    let initial_hw_accel = preferences_state.borrow().hardware_acceleration_enabled;
    let initial_playback_priority = preferences_state.borrow().playback_priority.clone();
    let initial_source_playback_priority =
        preferences_state.borrow().source_playback_priority.clone();
    let initial_proxy_mode = preferences_state.borrow().proxy_mode.clone();
    let initial_background_prerender = preferences_state.borrow().background_prerender;
    let initial_preview_luts = preferences_state.borrow().preview_luts;
    let initial_preview_quality = preferences_state.borrow().preview_quality.clone();
    let initial_show_waveform_on_video = preferences_state.borrow().show_waveform_on_video;
    let initial_show_timeline_preview = preferences_state.borrow().show_timeline_preview;
    let initial_show_track_audio_levels = preferences_state.borrow().show_track_audio_levels;
    let (player_obj, paintable) =
        Player::new(initial_hw_accel).expect("Failed to create GStreamer player");
    player_obj.set_source_playback_priority(initial_source_playback_priority);
    let player = Rc::new(RefCell::new(player_obj));
    let source_original_uri_for_proxy_fallback: std::sync::Arc<std::sync::Mutex<Option<String>>> =
        std::sync::Arc::new(std::sync::Mutex::new(None));
    log::info!(
        "Source preview decoder capabilities: vaapi_available={}, mode={}",
        player.borrow().vaapi_available(),
        player.borrow().decode_mode_name()
    );
    // Monitor the source-preview pipeline bus for errors; if the HW decode
    // path fails, downgrade to software mode and retry automatically.
    {
        use gstreamer::prelude::*;
        let pipeline = player
            .borrow()
            .pipeline()
            .clone()
            .downcast::<gstreamer::Pipeline>()
            .ok();
        if let Some(ref pipe) = pipeline {
            if let Some(bus) = pipe.bus() {
                let player_for_bus = player.clone();
                let source_original_uri_for_proxy_fallback =
                    source_original_uri_for_proxy_fallback.clone();
                let _watch = bus.add_watch_local(move |_bus, msg: &gstreamer::Message| {
                    use gstreamer::MessageView;
                    match msg.view() {
                        MessageView::Error(err) => {
                            log::error!(
                                "Source preview pipeline error: {} (debug: {:?})",
                                err.error(),
                                err.debug()
                            );
                            let mut should_fallback = false;
                            let err_text = err.error().to_string().to_lowercase();
                            let dbg_text = err
                                .debug()
                                .map(|d| d.to_string().to_lowercase())
                                .unwrap_or_default();
                            if err_text.contains("not-negotiated")
                                || dbg_text.contains("not-negotiated")
                                || dbg_text.contains("dmabuf")
                                || dbg_text.contains("va")
                            {
                                should_fallback = true;
                            }
                            let mut hw_fallback_applied = false;
                            if should_fallback {
                                match player_for_bus.borrow().fallback_to_software_after_error() {
                                    Ok(true) => {
                                        hw_fallback_applied = true;
                                        log::warn!(
                                            "Source preview fallback: switched to software decode mode after HW-path error"
                                        );
                                    }
                                    Ok(false) => {}
                                    Err(e) => log::error!(
                                        "Source preview fallback failed: {e:#}"
                                    ),
                                }
                            }
                            // If proxy playback fails at runtime, retry once with
                            // the original source URI so preview does not stay black
                            // while waiting for a valid/usable proxy.
                            if !hw_fallback_applied {
                                let original_uri = source_original_uri_for_proxy_fallback
                                    .lock()
                                    .ok()
                                    .and_then(|u| u.clone());
                                if let Some(original_uri) = original_uri {
                                    let current_uri = player_for_bus.borrow().current_uri();
                                    if current_uri.as_deref() != Some(original_uri.as_str()) {
                                        if let Err(e) = player_for_bus.borrow().load(&original_uri)
                                        {
                                            log::error!(
                                                "Source preview proxy fallback-to-original failed: {e:#}"
                                            );
                                        } else {
                                            log::warn!(
                                                "Source preview proxy fallback: reloaded original media after proxy-path error"
                                            );
                                        }
                                    }
                                }
                            }
                        }
                        MessageView::Warning(warn) => {
                            log::warn!(
                                "Source preview pipeline warning: {} (debug: {:?})",
                                warn.error(),
                                warn.debug()
                            );
                        }
                        _ => {}
                    }
                    glib::ControlFlow::Continue
                });
            }
        }
    }

    let (mut prog_player_raw, prog_paintable, prog_paintable2) =
        ProgramPlayer::new().expect("Failed to create program player");
    {
        let p = project.borrow();
        prog_player_raw.set_project_dimensions(p.width, p.height);
        prog_player_raw.set_frame_rate(p.frame_rate.numerator, p.frame_rate.denominator);
    }
    prog_player_raw.set_playback_priority(initial_playback_priority);
    prog_player_raw.set_proxy_enabled(initial_proxy_mode.is_enabled());
    prog_player_raw.set_proxy_scale_divisor(match initial_proxy_mode {
        crate::ui_state::ProxyMode::QuarterRes => 4,
        _ => 2,
    });
    prog_player_raw.set_preview_luts(initial_preview_luts);
    prog_player_raw.set_preview_quality(initial_preview_quality.divisor());
    prog_player_raw.set_experimental_preview_optimizations(
        preferences_state
            .borrow()
            .experimental_preview_optimizations,
    );
    prog_player_raw.set_realtime_preview(preferences_state.borrow().realtime_preview);
    prog_player_raw.set_background_prerender(initial_background_prerender);
    prog_player_raw.set_audio_crossfade_preview(
        preferences_state.borrow().crossfade_enabled,
        preferences_state.borrow().crossfade_curve.clone(),
        preferences_state.borrow().crossfade_duration_ns,
    );
    let prog_player = Rc::new(RefCell::new(prog_player_raw));

    let proxy_cache = Rc::new(RefCell::new(crate::media::proxy_cache::ProxyCache::new()));
    proxy_cache
        .borrow_mut()
        .set_sidecar_mirror_enabled(initial_proxy_mode.is_enabled());
    let bg_removal_cache = Rc::new(RefCell::new(
        crate::media::bg_removal_cache::BgRemovalCache::new(),
    ));
    let effective_proxy_enabled = Rc::new(Cell::new(initial_proxy_mode.is_enabled()));
    let effective_proxy_scale_divisor = Rc::new(Cell::new(match initial_proxy_mode {
        crate::ui_state::ProxyMode::QuarterRes => 4,
        _ => 2,
    }));

    let timeline_state = Rc::new(RefCell::new(TimelineState::new(project.clone())));
    timeline_state.borrow_mut().show_waveform_on_video = initial_show_waveform_on_video;
    timeline_state.borrow_mut().show_timeline_preview = initial_show_timeline_preview;
    timeline_state.borrow_mut().show_track_audio_levels = initial_show_track_audio_levels;
    let pending_program_seek_ticket = Rc::new(Cell::new(0u64));
    let pending_reload_ticket = Rc::new(Cell::new(0u64));
    let mcp_light_refresh_next = Rc::new(Cell::new(false));
    let suppress_resume_on_next_reload = Rc::new(Cell::new(false));
    let clear_media_browser_on_next_reload = Rc::new(Cell::new(false));

    // ── Build toolbar ─────────────────────────────────────────────────────
    let window_weak = window.downgrade();

    // Two-phase setup: create a stable Rc handle now, fill in the real
    // implementation after the timeline panel is built (so we can capture
    // a weak reference to it for explicit queue_draw).
    let on_project_changed_impl: Rc<RefCell<Option<Box<dyn Fn()>>>> = Rc::new(RefCell::new(None));
    let on_project_changed: Rc<dyn Fn()> = {
        let cb = on_project_changed_impl.clone();
        Rc::new(move || {
            if let Some(f) = cb.borrow().as_ref() {
                f();
            }
        })
    };
    let open_preferences_impl: Rc<RefCell<Option<Rc<dyn Fn()>>>> = Rc::new(RefCell::new(None));
    let open_preferences: Rc<dyn Fn()> = {
        let cb = open_preferences_impl.clone();
        Rc::new(move || {
            if let Some(f) = cb.borrow().as_ref() {
                f();
            }
        })
    };
    *open_preferences_impl.borrow_mut() = Some({
        let window_weak = window_weak.clone();
        let preferences_state = preferences_state.clone();
        let player = player.clone();
        let prog_player = prog_player.clone();
        let proxy_cache = proxy_cache.clone();
        let project = project.clone();
        let timeline_state = timeline_state.clone();
        let mcp_sender = mcp_sender.clone();
        let mcp_socket_stop = mcp_socket_stop.clone();
        let bg_removal_cache = bg_removal_cache.clone();
        Rc::new(move || {
            if let Some(win) = window_weak.upgrade() {
                let current = preferences_state.borrow().clone();
                let old_proxy_mode = current.proxy_mode.clone();
                let old_preview_luts = current.preview_luts;
                let preferences_state = preferences_state.clone();
                let player = player.clone();
                let prog_player = prog_player.clone();
                let proxy_cache = proxy_cache.clone();
                let project = project.clone();
                let timeline_state = timeline_state.clone();
                let mcp_sender = mcp_sender.clone();
                let mcp_socket_stop = mcp_socket_stop.clone();
                let bg_removal_cache = bg_removal_cache.clone();
                let on_save: Rc<dyn Fn(crate::ui_state::PreferencesState)> =
                    Rc::new(move |new_state| {
                        *preferences_state.borrow_mut() = new_state.clone();
                        crate::ui_state::save_preferences_state(&new_state);
                        if let Err(e) = player
                            .borrow()
                            .set_hardware_acceleration(new_state.hardware_acceleration_enabled)
                        {
                            eprintln!("Failed to apply hardware acceleration setting: {e}");
                        }
                        player.borrow().set_source_playback_priority(
                            new_state.source_playback_priority.clone(),
                        );
                        prog_player
                            .borrow_mut()
                            .set_playback_priority(new_state.playback_priority.clone());
                        prog_player
                            .borrow_mut()
                            .set_proxy_enabled(new_state.proxy_mode.is_enabled());
                        proxy_cache
                            .borrow_mut()
                            .set_sidecar_mirror_enabled(new_state.proxy_mode.is_enabled());
                        prog_player.borrow_mut().set_proxy_scale_divisor(
                            match new_state.proxy_mode {
                                crate::ui_state::ProxyMode::QuarterRes => 4,
                                _ => 2,
                            },
                        );
                        prog_player
                            .borrow_mut()
                            .set_preview_quality(new_state.preview_quality.divisor());
                        prog_player
                            .borrow_mut()
                            .set_experimental_preview_optimizations(
                                new_state.experimental_preview_optimizations,
                            );
                        prog_player
                            .borrow_mut()
                            .set_realtime_preview(new_state.realtime_preview);
                        prog_player
                            .borrow_mut()
                            .set_background_prerender(new_state.background_prerender);
                        prog_player
                            .borrow_mut()
                            .set_preview_luts(new_state.preview_luts);
                        prog_player.borrow_mut().set_audio_crossfade_preview(
                            new_state.crossfade_enabled,
                            new_state.crossfade_curve.clone(),
                            new_state.crossfade_duration_ns,
                        );
                        if new_state.proxy_mode.is_enabled() {
                            // If the proxy scale changed, invalidate old entries so clips are
                            // re-transcoded at the new resolution.
                            if new_state.proxy_mode != old_proxy_mode
                                || new_state.preview_luts != old_preview_luts
                            {
                                proxy_cache.borrow_mut().invalidate_all();
                            }
                            let scale = match new_state.proxy_mode {
                                crate::ui_state::ProxyMode::QuarterRes => {
                                    crate::media::proxy_cache::ProxyScale::Quarter
                                }
                                _ => crate::media::proxy_cache::ProxyScale::Half,
                            };
                            let clips: Vec<(String, Option<String>)> = {
                                let proj = project.borrow();
                                proj.tracks
                                    .iter()
                                    .flat_map(|t| t.clips.iter())
                                    .map(|c| (c.source_path.clone(), c.lut_path.clone()))
                                    .collect()
                            };
                            {
                                let mut cache = proxy_cache.borrow_mut();
                                for (path, lut) in &clips {
                                    cache.request(path, scale, lut.as_deref());
                                }
                            }
                            let paths = proxy_cache.borrow().proxies.clone();
                            prog_player.borrow_mut().update_proxy_paths(paths);
                        } else if new_state.preview_luts {
                            if new_state.proxy_mode != old_proxy_mode
                                || new_state.preview_luts != old_preview_luts
                            {
                                proxy_cache.borrow_mut().invalidate_all();
                            }
                            let (project_w, project_h, clips): (
                                u32,
                                u32,
                                Vec<(String, Option<String>)>,
                            ) = {
                                let proj = project.borrow();
                                (
                                    proj.width,
                                    proj.height,
                                    collect_unique_lut_clip_sources(&proj),
                                )
                            };
                            {
                                let mut cache = proxy_cache.borrow_mut();
                                for (path, lut) in &clips {
                                    cache.request(
                                        path,
                                        crate::media::proxy_cache::ProxyScale::Project {
                                            width: project_w,
                                            height: project_h,
                                        },
                                        lut.as_deref(),
                                    );
                                }
                            }
                            let paths = proxy_cache.borrow().proxies.clone();
                            prog_player.borrow_mut().update_proxy_paths(paths);
                        } else {
                            prog_player.borrow_mut().update_proxy_paths(HashMap::new());
                        }
                        timeline_state.borrow_mut().show_waveform_on_video =
                            new_state.show_waveform_on_video;
                        timeline_state.borrow_mut().show_timeline_preview =
                            new_state.show_timeline_preview;
                        timeline_state.borrow_mut().show_track_audio_levels =
                            new_state.show_track_audio_levels;
                        // Start/stop MCP socket server based on preference change.
                        if new_state.mcp_socket_enabled && mcp_socket_stop.borrow().is_none() {
                            let stop = crate::mcp::start_mcp_socket_server((*mcp_sender).clone());
                            *mcp_socket_stop.borrow_mut() = Some(stop);
                        } else if !new_state.mcp_socket_enabled {
                            if let Some(stop) = mcp_socket_stop.borrow_mut().take() {
                                stop.store(true, std::sync::atomic::Ordering::Relaxed);
                            }
                        }
                    });
                preferences::show_preferences_dialog(
                    win.upcast_ref(),
                    current,
                    on_save,
                    bg_removal_cache.clone(),
                );
            }
        })
    });

    // ── Build inspector (after on_project_changed is defined so we can pass it) ──
    // timeline_panel_cell is shared between the inspector's on_audio_changed callback
    // and the program monitor poll timer. Declare it early (filled in after timeline build).
    let timeline_panel_cell: Rc<RefCell<Option<gtk4::Widget>>> = Rc::new(RefCell::new(None));
    // transform_overlay_cell holds the TransformOverlay after the program monitor is built.
    let transform_overlay_cell: Rc<
        RefCell<Option<Rc<crate::ui::transform_overlay::TransformOverlay>>>,
    > = Rc::new(RefCell::new(None));
    let (inspector_box, inspector_view) = inspector::build_inspector(
        project.clone(),
        // on_clip_changed: name changes → full project-changed cycle
        {
            let cb = on_project_changed.clone();
            move || cb()
        },
        // on_color_changed: slider → direct filter update, no pipeline reload
        {
            let prog_player = prog_player.clone();
            let window_weak = window_weak.clone();
            let project = project.clone();
            move |b, c, s, temp, tnt, d, sh, shd, mid, hil,
                  exp, bp, hw, ht, mw, mt, sw, st| {
                prog_player.borrow_mut().update_current_effects(
                    b as f64,
                    c as f64,
                    s as f64,
                    temp as f64,
                    tnt as f64,
                    d as f64,
                    sh as f64,
                    shd as f64,
                    mid as f64,
                    hil as f64,
                    exp as f64,
                    bp as f64,
                    hw as f64,
                    ht as f64,
                    mw as f64,
                    mt as f64,
                    sw as f64,
                    st as f64,
                );
                // Update window title dirty marker without a full reload
                if let Some(win) = window_weak.upgrade() {
                    let proj = project.borrow();
                    let title = format!("UltimateSlice — {} •", proj.title);
                    win.set_title(Some(&title));
                }
            }
        },
        // on_audio_changed: volume/pan slider → direct update, no pipeline reload
        {
            let prog_player = prog_player.clone();
            let window_weak = window_weak.clone();
            let project = project.clone();
            let cell = timeline_panel_cell.clone();
            // clip_id comes directly from the inspector (authoritative selected clip),
            // avoiding any race with timeline_state.selected_clip_id.
            move |clip_id: &str, vol: f32, pan: f32| {
                // The inspector already persisted vol/pan to the project model.
                // Just mark dirty and update live GStreamer audio for the exact clip.
                {
                    let mut proj = project.borrow_mut();
                    proj.dirty = true;
                }
                {
                    let mut pp = prog_player.borrow_mut();
                    // Sync volume keyframes from project model to player so
                    // keyframe evaluation is current without a full pipeline reload.
                    {
                        let proj = project.borrow();
                        for track in &proj.tracks {
                            if let Some(model_clip) = track.clips.iter().find(|c| c.id == clip_id) {
                                // Sync to video clips (embedded audio)
                                if let Some(player_clip) =
                                    pp.clips.iter_mut().find(|c| c.id == clip_id)
                                {
                                    player_clip.volume_keyframes =
                                        model_clip.volume_keyframes.clone();
                                    player_clip.pan_keyframes = model_clip.pan_keyframes.clone();
                                }
                                // Sync to audio-only clips
                                if let Some(audio_clip) =
                                    pp.audio_clips.iter_mut().find(|c| c.id == clip_id)
                                {
                                    audio_clip.volume_keyframes =
                                        model_clip.volume_keyframes.clone();
                                    audio_clip.pan_keyframes = model_clip.pan_keyframes.clone();
                                }
                                break;
                            }
                        }
                    }
                    pp.update_audio_for_clip(clip_id, vol as f64, pan as f64);
                }
                if let Some(win) = window_weak.upgrade() {
                    let proj = project.borrow();
                    let title = format!("UltimateSlice — {} •", proj.title);
                    win.set_title(Some(&title));
                }
                // Redraw timeline so the waveform height/color reflects the new volume.
                if let Some(ref w) = *cell.borrow() {
                    w.queue_draw();
                }
            }
        },
        // on_transform_changed: crop/rotate/flip/scale/position → direct update, no pipeline reload
        {
            let player = player.clone();
            let prog_player = prog_player.clone();
            let window_weak = window_weak.clone();
            let project = project.clone();
            let timeline_state = timeline_state.clone();
            let transform_overlay_cell = transform_overlay_cell.clone();
            move |cl, cr, ct, cb, rot, fh, fv, sc, px, py| {
                player.borrow().set_transform(cl, cr, ct, cb, rot, fh, fv);
                let selected = timeline_state.borrow().selected_clip_id.clone();
                let mut pp = prog_player.borrow_mut();
                if let Some(ref clip_id) = selected {
                    pp.update_transform_for_clip(clip_id, cl, cr, ct, cb, rot, fh, fv, sc, px, py);
                } else {
                    pp.update_current_transform(cl, cr, ct, cb, rot, fh, fv, sc, px, py);
                }
                // Keep the transform overlay in sync so drag handles reflect slider changes.
                if let Some(ref to) = *transform_overlay_cell.borrow() {
                    to.set_transform(sc, px, py);
                    to.set_rotation(rot);
                    to.set_crop(cl, cr, ct, cb);
                }
                if let Some(win) = window_weak.upgrade() {
                    let proj = project.borrow();
                    let title = format!("UltimateSlice — {} •", proj.title);
                    win.set_title(Some(&title));
                }
            }
        },
        // on_title_changed: text/position → direct update on textoverlay element
        {
            let prog_player = prog_player.clone();
            let project = project.clone();
            let window_weak = window_weak.clone();
            move |text: String, x: f64, y: f64| {
                // Get font/color from the currently selected clip
                let (font, color) = {
                    let proj = project.borrow();
                    let pp = prog_player.borrow();
                    if let Some(idx) = pp.current_clip_idx() {
                        if let Some(clip) = proj.tracks.iter().flat_map(|t| t.clips.iter()).nth(idx)
                        {
                            (clip.title_font.clone(), clip.title_color)
                        } else {
                            ("Sans Bold 36".to_string(), 0xFFFFFFFF)
                        }
                    } else {
                        ("Sans Bold 36".to_string(), 0xFFFFFFFF)
                    }
                };
                prog_player
                    .borrow_mut()
                    .update_current_title(&text, &font, color, x, y);
                if let Some(win) = window_weak.upgrade() {
                    let proj = project.borrow();
                    let title = format!("UltimateSlice — {} •", proj.title);
                    win.set_title(Some(&title));
                }
            }
        },
        // on_speed_changed: speed slider → reload current clip at new rate
        {
            let on_project_changed = on_project_changed.clone();
            move |_speed: f64| {
                // Reload clips so the timeline width and player both reflect the new speed.
                on_project_changed();
            }
        },
        // on_lut_changed: LUT file assigned/cleared → full project-changed cycle + proxy re-request
        {
            let on_project_changed = on_project_changed.clone();
            let proxy_cache = proxy_cache.clone();
            let preferences_state = preferences_state.clone();
            let project = project.clone();
            let prog_player = prog_player.clone();
            move |_lut_path: Option<String>| {
                on_project_changed();
                // Re-generate proxies so the newly assigned/cleared LUT is baked in.
                let prefs = preferences_state.borrow();
                if prefs.proxy_mode.is_enabled() || prefs.preview_luts {
                    let (scale, clips): (
                        crate::media::proxy_cache::ProxyScale,
                        Vec<(String, Option<String>)>,
                    ) = {
                        let proj = project.borrow();
                        if prefs.proxy_mode.is_enabled() {
                            (
                                match prefs.proxy_mode {
                                    crate::ui_state::ProxyMode::QuarterRes => {
                                        crate::media::proxy_cache::ProxyScale::Quarter
                                    }
                                    _ => crate::media::proxy_cache::ProxyScale::Half,
                                },
                                proj.tracks
                                    .iter()
                                    .flat_map(|t| t.clips.iter())
                                    .map(|c| (c.source_path.clone(), c.lut_path.clone()))
                                    .collect(),
                            )
                        } else {
                            (
                                crate::media::proxy_cache::ProxyScale::Project {
                                    width: proj.width,
                                    height: proj.height,
                                },
                                collect_unique_lut_clip_sources(&proj),
                            )
                        }
                    };
                    {
                        let mut cache = proxy_cache.borrow_mut();
                        cache.invalidate_all();
                        for (path, lut) in &clips {
                            cache.request(path, scale, lut.as_deref());
                        }
                    }
                    let paths = proxy_cache.borrow().proxies.clone();
                    prog_player.borrow_mut().update_proxy_paths(paths);
                }
            }
        },
        // on_opacity_changed: clip opacity slider → update top layer alpha immediately
        {
            let prog_player = prog_player.clone();
            let window_weak = window_weak.clone();
            let project = project.clone();
            let timeline_state = timeline_state.clone();
            move |opacity: f64| {
                let selected = timeline_state.borrow().selected_clip_id.clone();
                let mut pp = prog_player.borrow_mut();
                if let Some(ref clip_id) = selected {
                    pp.update_opacity_for_clip(clip_id, opacity);
                } else {
                    pp.update_current_opacity(opacity);
                }
                if let Some(win) = window_weak.upgrade() {
                    let proj = project.borrow();
                    let title = format!("UltimateSlice — {} •", proj.title);
                    win.set_title(Some(&title));
                }
            }
        },
        // on_reverse_changed: reverse checkbox → reload timeline and project
        {
            let on_project_changed = on_project_changed.clone();
            move |_reversed: bool| {
                // Reload clips so the timeline badge reflects the new reverse state.
                on_project_changed();
            }
        },
        // on_chroma_key_changed: chroma key toggle/color → full project-changed cycle
        {
            let on_project_changed = on_project_changed.clone();
            move || {
                on_project_changed();
            }
        },
        // on_chroma_key_slider_changed: tolerance/softness → live property update, no rebuild
        {
            let prog_player = prog_player.clone();
            let project = project.clone();
            let window_weak = window_weak.clone();
            let timeline_state = timeline_state.clone();
            move |tolerance: f32, softness: f32| {
                let (enabled, color) = {
                    let proj = project.borrow();
                    let selected = timeline_state.borrow().selected_clip_id.clone();
                    selected
                        .and_then(|id| {
                            proj.tracks
                                .iter()
                                .flat_map(|t| t.clips.iter())
                                .find(|c| c.id == id)
                                .map(|c| (c.chroma_key_enabled, c.chroma_key_color))
                        })
                        .unwrap_or((false, 0x00FF00))
                };
                prog_player
                    .borrow_mut()
                    .update_current_chroma_key(enabled, color, tolerance, softness);
                if let Some(win) = window_weak.upgrade() {
                    let proj = project.borrow();
                    let title = format!("UltimateSlice — {} •", proj.title);
                    win.set_title(Some(&title));
                }
            }
        },
        // on_bg_removal_changed: toggle/threshold → full project-changed cycle
        {
            let on_project_changed = on_project_changed.clone();
            move || {
                on_project_changed();
            }
        },
        {
            let timeline_state = timeline_state.clone();
            move || timeline_state.borrow().playhead_ns
        },
        // on_seek_to: navigate the playhead from the inspector (keyframe navigation)
        {
            let timeline_state = timeline_state.clone();
            let timeline_panel_cell = timeline_panel_cell.clone();
            let prog_player = prog_player.clone();
            move |ns: u64| {
                {
                    let mut st = timeline_state.borrow_mut();
                    st.playhead_ns = ns;
                }
                prog_player.borrow_mut().seek(ns);
                if let Some(ref w) = *timeline_panel_cell.borrow() {
                    w.queue_draw();
                }
            }
        },
    );

    // Set initial model availability on the inspector so the bg-removal
    // section is hidden when no ONNX model is present.
    inspector_view
        .bg_removal_model_available
        .set(bg_removal_cache.borrow().is_available());

    // Wire timeline's on_project_changed + on_seek + on_play_pause
    {
        let cb = on_project_changed.clone();
        timeline_state.borrow_mut().on_project_changed = Some(Rc::new(move || cb()));
    }
    // Wire on_clip_selected: lightweight inspector sync without pipeline rebuild.
    {
        let inspector_view = inspector_view.clone();
        let project = project.clone();
        let transform_overlay_cell = transform_overlay_cell.clone();
        let timeline_state_for_sel = timeline_state.clone();
        timeline_state.borrow_mut().on_clip_selected =
            Some(Rc::new(move |clip_id: Option<String>| {
                let proj = project.borrow();
                let playhead_ns = timeline_state_for_sel.borrow().playhead_ns;
                inspector_view.update(&proj, clip_id.as_deref(), playhead_ns);
                inspector_view.update_keyframe_indicator(&proj, playhead_ns);
                // Sync transform overlay handles with selection state,
                // using keyframe-interpolated values at the current playhead.
                if let Some(ref to) = *transform_overlay_cell.borrow() {
                    sync_transform_overlay_to_playhead(to, &proj, clip_id.as_deref(), playhead_ns);
                }
            }));
    }
    {
        let prog_player = prog_player.clone();
        let pending_program_seek_ticket = pending_program_seek_ticket.clone();
        timeline_state.borrow_mut().on_seek = Some(Rc::new(move |ns| {
            let ticket = pending_program_seek_ticket.get().wrapping_add(1);
            pending_program_seek_ticket.set(ticket);
            let prog_player_seek = prog_player.clone();
            let pending_program_seek_ticket_check = pending_program_seek_ticket.clone();
            glib::timeout_add_local_once(std::time::Duration::from_millis(0), move || {
                if pending_program_seek_ticket_check.get() != ticket {
                    return;
                }
                let seek_started = std::time::Instant::now();
                let needs_async = prog_player_seek.borrow_mut().seek(ns);
                log::debug!(
                    "window:on_seek timeline_pos={} needs_async={} elapsed_ms={}",
                    ns,
                    needs_async,
                    seek_started.elapsed().as_millis()
                );
                if needs_async {
                    // The pipeline is in Playing; let the GTK main loop run so
                    // gtk4paintablesink can complete its preroll, then restore Paused.
                    let pp = prog_player_seek.clone();
                    glib::timeout_add_local_once(
                        std::time::Duration::from_millis(250),
                        move || {
                            pp.borrow().complete_playing_pulse();
                        },
                    );
                }
            });
        }));
    }
    {
        let prog_player = prog_player.clone();
        let timeline_state2 = timeline_state.clone();
        timeline_state.borrow_mut().on_play_pause = Some(Rc::new(move || {
            let is_playing = prog_player.borrow().is_playing();
            // Pause extraction when starting playback, resume when stopping.
            if let Some(cb) = timeline_state2.borrow().on_extraction_pause.clone() {
                cb(!is_playing); // !is_playing because toggle hasn't happened yet
            }
            prog_player.borrow_mut().toggle_play_pause();
        }));
    }
    let on_export_frame_gui: Rc<dyn Fn()> = {
        let window_weak = window_weak.clone();
        let project = project.clone();
        let prog_player = prog_player.clone();
        Rc::new(move || {
            let Some(win) = window_weak.upgrade() else {
                return;
            };
            let dialog = gtk::FileDialog::new();
            dialog.set_title("Export Frame");
            dialog.set_initial_name(Some("frame.png"));
            let filter = gtk::FileFilter::new();
            filter.add_pattern("*.png");
            filter.add_pattern("*.jpg");
            filter.add_pattern("*.jpeg");
            filter.add_pattern("*.ppm");
            filter.set_name(Some("Image Files"));
            let filters = gtk::gio::ListStore::new::<gtk::FileFilter>();
            filters.append(&filter);
            dialog.set_filters(Some(&filters));
            let project = project.clone();
            let prog_player = prog_player.clone();
            let win_for_save = win.clone();
            dialog.save(Some(&win), gtk::gio::Cancellable::NONE, move |result| {
                if let Ok(file) = result {
                    if let Some(mut path) = file.path() {
                        if path.extension().is_none() {
                            path.set_extension("png");
                        }
                        match export_displayed_frame_to_image(&prog_player, &path) {
                            Ok(fmt) => flash_window_status_title(
                                &win_for_save,
                                &project,
                                &format!("Frame exported ({fmt})"),
                            ),
                            Err(e) => {
                                eprintln!("[frame-export] {e}");
                                flash_window_status_title(
                                    &win_for_save,
                                    &project,
                                    "Frame export failed",
                                );
                            }
                        }
                    }
                }
            });
        })
    };
    let on_go_to_timecode: Rc<dyn Fn()> = {
        let window_weak = window_weak.clone();
        let project = project.clone();
        let timeline_state = timeline_state.clone();
        let timeline_panel_cell = timeline_panel_cell.clone();
        Rc::new(move || {
            let Some(win) = window_weak.upgrade() else {
                return;
            };
            present_go_to_timecode_dialog(&win, &project, &timeline_state, &timeline_panel_cell);
        })
    };
    let header = toolbar::build_toolbar(
        project.clone(),
        library.clone(),
        timeline_state.clone(),
        bg_removal_cache.clone(),
        {
            let cb = on_project_changed.clone();
            move || cb()
        },
        {
            let suppress_resume_on_next_reload = suppress_resume_on_next_reload.clone();
            let clear_media_browser_on_next_reload = clear_media_browser_on_next_reload.clone();
            move || {
                suppress_resume_on_next_reload.set(true);
                clear_media_browser_on_next_reload.set(true);
            }
        },
        {
            let cb = on_export_frame_gui.clone();
            move || cb()
        },
    );
    window.set_titlebar(Some(&header));

    // ── Root layout: horizontal paned (content | inspector) ──────────────
    let root_hpaned = Paned::new(Orientation::Horizontal);
    root_hpaned.set_hexpand(true);
    root_hpaned.set_vexpand(true);
    root_hpaned.set_position(1120);

    let root_vpaned = Paned::new(Orientation::Vertical);
    root_vpaned.set_vexpand(true);
    root_vpaned.set_hexpand(true);
    root_vpaned.set_position(520);

    let top_paned = Paned::new(Orientation::Horizontal);
    top_paned.set_hexpand(true);
    top_paned.set_vexpand(true);
    top_paned.set_position(320);

    // ── Build preview first so we have source_marks ───────────────────────
    // on_append stub: real impl filled in below after source_marks is available.
    let on_append_impl: Rc<RefCell<Option<Rc<dyn Fn()>>>> = Rc::new(RefCell::new(None));
    let on_append: Rc<dyn Fn()> = {
        let cb = on_append_impl.clone();
        Rc::new(move || {
            if let Some(f) = cb.borrow().as_ref() {
                f();
            }
        })
    };
    let on_insert_impl: Rc<RefCell<Option<Rc<dyn Fn()>>>> = Rc::new(RefCell::new(None));
    let on_insert: Rc<dyn Fn()> = {
        let cb = on_insert_impl.clone();
        Rc::new(move || {
            if let Some(f) = cb.borrow().as_ref() {
                f();
            }
        })
    };
    let on_overwrite_impl: Rc<RefCell<Option<Rc<dyn Fn()>>>> = Rc::new(RefCell::new(None));
    let on_overwrite: Rc<dyn Fn()> = {
        let cb = on_overwrite_impl.clone();
        Rc::new(move || {
            if let Some(f) = cb.borrow().as_ref() {
                f();
            }
        })
    };
    let on_close_preview_impl: Rc<RefCell<Option<Rc<dyn Fn()>>>> = Rc::new(RefCell::new(None));
    let on_close_preview: Rc<dyn Fn()> = {
        let cb = on_close_preview_impl.clone();
        Rc::new(move || {
            if let Some(f) = cb.borrow().as_ref() {
                f();
            }
        })
    };
    let (preview_widget, source_marks, clip_name_label) = preview::build_preview(
        player.clone(),
        paintable,
        on_append.clone(),
        on_insert.clone(),
        on_overwrite.clone(),
        on_close_preview.clone(),
    );

    // Wire on_drop_clip — placed here so it can read source_marks to honour
    // the in/out selection set in the source monitor.
    {
        let project = project.clone();
        let library = library.clone();
        let on_project_changed = on_project_changed.clone();
        let source_marks = source_marks.clone();
        let timeline_state_for_drop = timeline_state.clone();
        timeline_state.borrow_mut().on_drop_clip = Some(Rc::new(
            move |source_path, duration_ns, track_idx, timeline_start_ns| {
                let magnetic_mode = timeline_state_for_drop.borrow().magnetic_mode;
                let source_info = {
                    let marks = source_marks.borrow();
                    if marks.path == source_path {
                        SourcePlacementInfo {
                            is_audio_only: marks.is_audio_only,
                            has_audio: marks.has_audio,
                            source_timecode_base_ns: marks.source_timecode_base_ns,
                        }
                    } else {
                        let lib = library.borrow();
                        let proj = project.borrow();
                        lookup_source_placement_info(&lib, &proj, &source_path)
                    }
                };
                let mut proj = project.borrow_mut();
                // If the source monitor has in/out marks for this clip, use them;
                // otherwise fall back to the full source range.
                let (src_in, src_out) = {
                    let marks = source_marks.borrow();
                    if marks.path == source_path && marks.in_ns < marks.out_ns {
                        (marks.in_ns, marks.out_ns)
                    } else {
                        (0, duration_ns)
                    }
                };
                let auto_link_pair = !source_info.is_audio_only && source_info.has_audio;
                let video_track_idx =
                    find_preferred_track_index_by_index(&proj, Some(track_idx), TrackKind::Video);
                let audio_track_idx =
                    find_preferred_track_index_by_index(&proj, Some(track_idx), TrackKind::Audio);

                if auto_link_pair && video_track_idx.is_some() && audio_track_idx.is_some() {
                    let link_group_id = uuid::Uuid::new_v4().to_string();
                    if let Some(video_idx) = video_track_idx {
                        let video_clip = build_source_clip(
                            &source_path,
                            src_in,
                            src_out,
                            timeline_start_ns,
                            ClipKind::Video,
                            source_info.source_timecode_base_ns,
                            Some(link_group_id.as_str()),
                        );
                        proj.tracks[video_idx].add_clip(video_clip);
                    }
                    if let Some(audio_idx) = audio_track_idx {
                        let audio_clip = build_source_clip(
                            &source_path,
                            src_in,
                            src_out,
                            timeline_start_ns,
                            ClipKind::Audio,
                            source_info.source_timecode_base_ns,
                            Some(link_group_id.as_str()),
                        );
                        proj.tracks[audio_idx].add_clip(audio_clip);
                    }
                    proj.dirty = true;
                    drop(proj);
                    on_project_changed();
                    return;
                }

                if let Some(track) = proj.tracks.get_mut(track_idx) {
                    let kind = match track.kind {
                        TrackKind::Video => ClipKind::Video,
                        TrackKind::Audio => ClipKind::Audio,
                    };
                    let clip = build_source_clip(
                        &source_path,
                        src_in,
                        src_out,
                        timeline_start_ns,
                        kind,
                        source_info.source_timecode_base_ns,
                        None,
                    );
                    let _ = add_clip_to_track(track, clip, magnetic_mode);
                    proj.dirty = true;
                    drop(proj);
                    on_project_changed();
                }
            },
        ));
    }

    // Shared flag: true while audio sync is running (read by status bar timer).
    let audio_sync_in_progress: Rc<Cell<bool>> = Rc::new(Cell::new(false));

    // Wire on_sync_audio — spawns a background thread for FFT cross-correlation.
    {
        let project = project.clone();
        let on_project_changed = on_project_changed.clone();
        let window_weak = window.downgrade();
        let sync_rx: Rc<RefCell<Option<std::sync::mpsc::Receiver<Vec<(String, i64, f32)>>>>> =
            Rc::new(RefCell::new(None));
        let sync_rx_for_timer = sync_rx.clone();
        let audio_sync_in_progress_timer = audio_sync_in_progress.clone();
        // Poll timer for sync results
        {
            let project = project.clone();
            let on_project_changed = on_project_changed.clone();
            let timeline_state = timeline_state.clone();
            let window_weak = window_weak.clone();
            glib::timeout_add_local(std::time::Duration::from_millis(100), move || {
                let rx_opt = sync_rx_for_timer.borrow();
                if let Some(ref rx) = *rx_opt {
                    if let Ok(results) = rx.try_recv() {
                        drop(rx_opt);
                        sync_rx_for_timer.borrow_mut().take();
                        audio_sync_in_progress_timer.set(false);
                        apply_audio_sync_results(
                            &results,
                            &project,
                            &timeline_state,
                            &on_project_changed,
                            window_weak.upgrade().as_ref(),
                        );
                    }
                }
                glib::ControlFlow::Continue
            });
        }
        let audio_sync_in_progress_cb = audio_sync_in_progress.clone();
        timeline_state.borrow_mut().on_sync_audio = Some(Rc::new(
            move |clip_infos: Vec<(String, String, u64, u64, u64, String)>| {
                if sync_rx.borrow().is_some() {
                    // Sync already in progress
                    return;
                }
                audio_sync_in_progress_cb.set(true);
                if let Some(win) = window_weak.upgrade() {
                    let proj = project.borrow();
                    let title = proj.title.clone();
                    let dirty = proj.dirty;
                    drop(proj);
                    win.set_title(Some(&format!("UltimateSlice — {title} (Syncing audio...)")));
                    let _ = dirty; // title restored by apply function
                }
                let (tx, rx) = std::sync::mpsc::channel();
                *sync_rx.borrow_mut() = Some(rx);
                std::thread::spawn(move || {
                    let _ = gstreamer::init();
                    let clips: Vec<(String, String, u64, u64)> = clip_infos
                        .iter()
                        .map(|(id, path, src_in, src_out, _tl_start, _track_id)| {
                            (id.clone(), path.clone(), *src_in, *src_out)
                        })
                        .collect();
                    let sync_results = crate::media::audio_sync::sync_clips_by_audio(&clips);
                    let results: Vec<(String, i64, f32)> = sync_results
                        .into_iter()
                        .map(|r| (r.clip_id, r.offset_ns, r.confidence))
                        .collect();
                    let _ = tx.send(results);
                });
            },
        ));
    }

    // ── Build program monitor ──────────────────────────────────────────────
    let prog_monitor_host = gtk::Box::new(Orientation::Vertical, 0);
    prog_monitor_host.set_hexpand(true);
    prog_monitor_host.set_vexpand(true);
    let monitor_state = Rc::new(RefCell::new(crate::ui_state::load_program_monitor_state()));
    let popout_window_cell: Rc<RefCell<Option<ApplicationWindow>>> = Rc::new(RefCell::new(None));
    let monitor_popped = Rc::new(Cell::new(false));
    let on_toggle_popout_impl: Rc<RefCell<Option<Rc<dyn Fn()>>>> = Rc::new(RefCell::new(None));
    let on_toggle_popout: Rc<dyn Fn()> = {
        let cb = on_toggle_popout_impl.clone();
        Rc::new(move || {
            if let Some(f) = cb.borrow().as_ref() {
                f();
            }
        })
    };

    let (
        prog_monitor_widget,
        pos_label,
        speed_label,
        picture_a,
        picture_b,
        vu_meter,
        vu_peak_cell,
        prog_canvas_frame,
        _prog_safe_area_setter,
    ) = {
        // Build the interactive transform overlay and wire its drag callback.
        let transform_overlay = Rc::new(crate::ui::transform_overlay::TransformOverlay::new(
            {
                let inspector_view = inspector_view.clone();
                let prog_player = prog_player.clone();
                let project = project.clone();
                let timeline_state = timeline_state.clone();
                let window_weak = window_weak.clone();
                move |sc, px, py| {
                    // 1. Update selected clip in model
                    let selected = timeline_state.borrow().selected_clip_id.clone();
                    if let Some(ref clip_id) = selected {
                        let mut proj = project.borrow_mut();
                        for track in &mut proj.tracks {
                            if let Some(clip) = track.clips.iter_mut().find(|c| &c.id == clip_id) {
                                clip.scale = sc;
                                clip.position_x = px;
                                clip.position_y = py;
                                proj.dirty = true;
                                break;
                            }
                        }
                    }
                    // 2. Sync inspector sliders without re-triggering the transform callback
                    {
                        *inspector_view.updating.borrow_mut() = true;
                        inspector_view.scale_slider.set_value(sc);
                        inspector_view.position_x_slider.set_value(px);
                        inspector_view.position_y_slider.set_value(py);
                        *inspector_view.updating.borrow_mut() = false;
                    }
                    // 3. Push to GStreamer without blocking reseek (live mode handles preview)
                    let cl = inspector_view.crop_left_slider.value() as i32;
                    let crv = inspector_view.crop_right_slider.value() as i32;
                    let ct = inspector_view.crop_top_slider.value() as i32;
                    let cb = inspector_view.crop_bottom_slider.value() as i32;
                    let rot = inspector_view.rotate_spin.value().round() as i32;
                    let fh = inspector_view.flip_h_btn.is_active();
                    let fv = inspector_view.flip_v_btn.is_active();
                    let mut pp = prog_player.borrow_mut();
                    pp.enter_transform_live_mode();
                    pp.set_transform_properties_only(
                        selected.as_deref(),
                        cl,
                        crv,
                        ct,
                        cb,
                        rot,
                        fh,
                        fv,
                        sc,
                        px,
                        py,
                    );
                    // 4. Update window dirty marker
                    if let Some(win) = window_weak.upgrade() {
                        let proj = project.borrow();
                        win.set_title(Some(&format!("UltimateSlice — {} •", proj.title)));
                    }
                }
            },
            {
                let inspector_view = inspector_view.clone();
                let player = player.clone();
                let prog_player = prog_player.clone();
                let project = project.clone();
                let timeline_state = timeline_state.clone();
                let window_weak = window_weak.clone();
                move |rot: i32| {
                    let selected = timeline_state.borrow().selected_clip_id.clone();
                    if let Some(ref clip_id) = selected {
                        let mut proj = project.borrow_mut();
                        for track in &mut proj.tracks {
                            if let Some(clip) = track.clips.iter_mut().find(|c| &c.id == clip_id) {
                                clip.rotate = rot;
                                proj.dirty = true;
                                break;
                            }
                        }
                    }
                    {
                        *inspector_view.updating.borrow_mut() = true;
                        inspector_view.rotate_spin.set_value(rot as f64);
                        *inspector_view.updating.borrow_mut() = false;
                    }
                    let cl = inspector_view.crop_left_slider.value() as i32;
                    let cr = inspector_view.crop_right_slider.value() as i32;
                    let ct = inspector_view.crop_top_slider.value() as i32;
                    let cb = inspector_view.crop_bottom_slider.value() as i32;
                    let fh = inspector_view.flip_h_btn.is_active();
                    let fv = inspector_view.flip_v_btn.is_active();
                    let sc = inspector_view.scale_slider.value();
                    let px = inspector_view.position_x_slider.value();
                    let py = inspector_view.position_y_slider.value();
                    player.borrow().set_transform(cl, cr, ct, cb, rot, fh, fv);
                    let mut pp = prog_player.borrow_mut();
                    pp.enter_transform_live_mode();
                    pp.set_transform_properties_only(
                        selected.as_deref(),
                        cl,
                        cr,
                        ct,
                        cb,
                        rot,
                        fh,
                        fv,
                        sc,
                        px,
                        py,
                    );
                    if let Some(win) = window_weak.upgrade() {
                        let proj = project.borrow();
                        win.set_title(Some(&format!("UltimateSlice — {} •", proj.title)));
                    }
                }
            },
            {
                let inspector_view = inspector_view.clone();
                let player = player.clone();
                let prog_player = prog_player.clone();
                let project = project.clone();
                let timeline_state = timeline_state.clone();
                let window_weak = window_weak.clone();
                move |cl, cr, ct, cb| {
                    let selected = timeline_state.borrow().selected_clip_id.clone();
                    if let Some(ref clip_id) = selected {
                        let mut proj = project.borrow_mut();
                        for track in &mut proj.tracks {
                            if let Some(clip) = track.clips.iter_mut().find(|c| &c.id == clip_id) {
                                clip.crop_left = cl;
                                clip.crop_right = cr;
                                clip.crop_top = ct;
                                clip.crop_bottom = cb;
                                proj.dirty = true;
                                break;
                            }
                        }
                    }
                    {
                        *inspector_view.updating.borrow_mut() = true;
                        inspector_view.crop_left_slider.set_value(cl as f64);
                        inspector_view.crop_right_slider.set_value(cr as f64);
                        inspector_view.crop_top_slider.set_value(ct as f64);
                        inspector_view.crop_bottom_slider.set_value(cb as f64);
                        *inspector_view.updating.borrow_mut() = false;
                    }
                    let rot = inspector_view.rotate_spin.value().round() as i32;
                    let fh = inspector_view.flip_h_btn.is_active();
                    let fv = inspector_view.flip_v_btn.is_active();
                    let sc = inspector_view.scale_slider.value();
                    let px = inspector_view.position_x_slider.value();
                    let py = inspector_view.position_y_slider.value();
                    player.borrow().set_transform(cl, cr, ct, cb, rot, fh, fv);
                    let mut pp = prog_player.borrow_mut();
                    pp.enter_transform_live_mode();
                    pp.set_transform_properties_only(
                        selected.as_deref(),
                        cl,
                        cr,
                        ct,
                        cb,
                        rot,
                        fh,
                        fv,
                        sc,
                        px,
                        py,
                    );
                    if let Some(win) = window_weak.upgrade() {
                        let proj = project.borrow();
                        win.set_title(Some(&format!("UltimateSlice — {} •", proj.title)));
                    }
                }
            },
            {
                // on_drag_begin: force paused editing so timeline doesn't
                // continue advancing while transform handles are dragged.
                let prog_player = prog_player.clone();
                move || {
                    prog_player.borrow_mut().pause();
                }
            },
            {
                // on_drag_end: exit live transform mode and do a final reseek
                // so the composited frame accurately reflects the last state.
                // If animation mode is active, auto-upsert keyframes.
                let prog_player = prog_player.clone();
                let inspector_view = inspector_view.clone();
                let project = project.clone();
                let timeline_state = timeline_state.clone();
                let on_project_changed = on_project_changed.clone();
                move || {
                    prog_player.borrow_mut().exit_transform_live_mode();
                    if inspector_view.animation_mode.get() {
                        let playhead = timeline_state.borrow().playhead_ns;
                        let clip_id = timeline_state.borrow().selected_clip_id.clone();
                        if let Some(clip_id) = clip_id {
                            let sc = inspector_view.scale_slider.value();
                            let px = inspector_view.position_x_slider.value();
                            let py = inspector_view.position_y_slider.value();
                            let mut changed = false;
                            {
                                let mut proj = project.borrow_mut();
                                for track in &mut proj.tracks {
                                    if let Some(clip) =
                                        track.clips.iter_mut().find(|c| c.id == clip_id)
                                    {
                                        let interp = inspector_view.selected_interpolation();
                                        clip.upsert_phase1_keyframe_at_timeline_ns_with_interp(
                                            Phase1KeyframeProperty::Scale,
                                            playhead,
                                            sc,
                                            interp,
                                        );
                                        clip.upsert_phase1_keyframe_at_timeline_ns_with_interp(
                                            Phase1KeyframeProperty::PositionX,
                                            playhead,
                                            px,
                                            interp,
                                        );
                                        clip.upsert_phase1_keyframe_at_timeline_ns_with_interp(
                                            Phase1KeyframeProperty::PositionY,
                                            playhead,
                                            py,
                                            interp,
                                        );
                                        proj.dirty = true;
                                        changed = true;
                                        break;
                                    }
                                }
                            }
                            if changed {
                                on_project_changed();
                            }
                        }
                    }
                }
            },
        ));
        // Initialise project dimensions (default 1920×1080 until first on_project_changed)
        {
            let p = project.borrow();
            transform_overlay.set_project_dimensions(p.width, p.height);
        }

        // Store the overlay handle for use in on_project_changed_impl
        let to = transform_overlay.clone();
        *transform_overlay_cell.borrow_mut() = Some(transform_overlay);

        program_monitor::build_program_monitor(
            prog_player.clone(),
            prog_paintable,
            prog_paintable2,
            {
                let p = project.borrow();
                p.width
            },
            {
                let p = project.borrow();
                p.height
            },
            // on_stop
            {
                let pp = prog_player.clone();
                let ts = timeline_state.clone();
                let cell = timeline_panel_cell.clone();
                move || {
                    if let Some(cb) = ts.borrow().on_extraction_pause.clone() {
                        cb(false);
                    }
                    pp.borrow_mut().stop();
                    ts.borrow_mut().playhead_ns = 0;
                    if let Some(ref w) = *cell.borrow() {
                        w.queue_draw();
                    }
                }
            },
            // on_play_pause
            {
                let pp = prog_player.clone();
                let ts = timeline_state.clone();
                move || {
                    let is_playing = pp.borrow().is_playing();
                    if let Some(cb) = ts.borrow().on_extraction_pause.clone() {
                        cb(!is_playing);
                    }
                    pp.borrow_mut().toggle_play_pause();
                }
            },
            {
                let cb = on_toggle_popout.clone();
                move || cb()
            },
            {
                let cb = on_go_to_timecode.clone();
                move || cb()
            },
            Some(to.drawing_area.clone()),
            monitor_state.borrow().show_safe_areas,
            {
                let monitor_state = monitor_state.clone();
                move |show| {
                    let mut state = monitor_state.borrow_mut();
                    if state.show_safe_areas != show {
                        state.show_safe_areas = show;
                        crate::ui_state::save_program_monitor_state(&state);
                    }
                }
            },
        )
    };

    // Give the transform overlay access to picture_a so it can query the actual
    // paintable intrinsic dimensions for pixel-perfect frame rect alignment.
    // Also give it the canvas AspectFrame so canvas_video_rect() can use
    // compute_bounds() to find the true canvas rect at any zoom level.
    if let Some(ref to) = *transform_overlay_cell.borrow() {
        to.set_picture(picture_a.clone());
        to.set_canvas_widget(prog_canvas_frame.clone().upcast::<gtk4::Widget>());
    }

    // ── Build colour scopes panel (hidden by default) ──────────────────────
    let (scopes_widget, scopes_state) = crate::ui::color_scopes::build_color_scopes();
    let scopes_revealer = gtk::Revealer::new();
    scopes_revealer.set_transition_type(gtk::RevealerTransitionType::SlideDown);
    scopes_revealer.set_child(Some(&scopes_widget));
    scopes_revealer.set_reveal_child(false);
    let docked_scopes_paned = Paned::new(Orientation::Vertical);
    docked_scopes_paned.set_hexpand(true);
    docked_scopes_paned.set_vexpand(true);
    docked_scopes_paned.set_resize_start_child(true);
    docked_scopes_paned.set_resize_end_child(true);
    docked_scopes_paned.set_shrink_end_child(true);
    docked_scopes_paned.set_start_child(Some(&prog_monitor_widget));
    docked_scopes_paned.set_end_child(Option::<&gtk::Widget>::None);
    {
        let state = monitor_state.borrow().clone();
        docked_scopes_paned.set_position(state.docked_split_pos.max(160));
    }
    {
        let monitor_state = monitor_state.clone();
        let monitor_popped = monitor_popped.clone();
        docked_scopes_paned.connect_position_notify(move |p| {
            if monitor_popped.get() {
                return;
            }
            let pos = p.position().max(160);
            let mut state = monitor_state.borrow_mut();
            if state.docked_split_pos != pos {
                state.docked_split_pos = pos;
                crate::ui_state::save_program_monitor_state(&state);
            }
        });
    }

    // 33 ms poll timer (~30 FPS): smoother playhead/timeline updates and
    // tighter clip-boundary handoff timing.
    {
        let pp = prog_player.clone();
        let ts = timeline_state.clone();
        let cell = timeline_panel_cell.clone();
        let last_pos_ns = Rc::new(Cell::new(u64::MAX));
        let last_pos_ns_c = last_pos_ns.clone();
        let last_draw_ns = Rc::new(Cell::new(u64::MAX));
        let last_draw_ns_c = last_draw_ns.clone();
        let vu = vu_meter.clone();
        let vu_pc = vu_peak_cell.clone();
        let scopes_rev = scopes_revealer.clone();
        let scopes_st = scopes_state.clone();
        let speed_lbl = speed_label.clone();
        let preferences_state = preferences_state.clone();
        let project = project.clone();
        let prog_canvas_frame = prog_canvas_frame.clone();
        let proxy_cache = proxy_cache.clone();
        let effective_proxy_enabled = effective_proxy_enabled.clone();
        let effective_proxy_scale_divisor = effective_proxy_scale_divisor.clone();
        let last_auto_check_us: Rc<Cell<i64>> = Rc::new(Cell::new(0));
        let last_auto_check_us_c = last_auto_check_us.clone();
        let last_auto_quality_switch_us: Rc<Cell<i64>> = Rc::new(Cell::new(0));
        let last_auto_quality_switch_us_c = last_auto_quality_switch_us.clone();
        let last_auto_proxy_switch_us: Rc<Cell<i64>> = Rc::new(Cell::new(0));
        let last_auto_proxy_switch_us_c = last_auto_proxy_switch_us.clone();
        let last_proxy_refresh_us: Rc<Cell<i64>> = Rc::new(Cell::new(0));
        let last_proxy_refresh_us_c = last_proxy_refresh_us.clone();
        let picture_a_poll = picture_a.clone();
        let picture_b_poll = picture_b.clone();
        let transform_overlay_poll = transform_overlay_cell.clone();
        let timeline_state_poll = timeline_state.clone();
        let inspector_view_poll = inspector_view.clone();
        glib::timeout_add_local(std::time::Duration::from_millis(33), move || {
            let (pos_ns, playing, opacity_a, opacity_b, peaks, track_peaks, scope_frame, jkl_rate) = {
                let mut player = pp.borrow_mut();
                let now_us = glib::monotonic_time();
                if now_us - last_auto_check_us_c.get() >= 250_000 {
                    last_auto_check_us_c.set(now_us);
                    let (preview_quality, proxy_mode, preview_luts) = {
                        let prefs = preferences_state.borrow();
                        (
                            prefs.preview_quality.clone(),
                            prefs.proxy_mode.clone(),
                            prefs.preview_luts,
                        )
                    };
                    let auto_preview_mode =
                        matches!(preview_quality, crate::ui_state::PreviewQuality::Auto);
                    let divisor = match preview_quality {
                        crate::ui_state::PreviewQuality::Auto => {
                            let (pw, ph) = {
                                let proj = project.borrow();
                                (proj.width, proj.height)
                            };
                            auto_preview_divisor(
                                pw,
                                ph,
                                prog_canvas_frame.width(),
                                prog_canvas_frame.height(),
                                player.preview_divisor(),
                            )
                        }
                        _ => preview_quality.divisor(),
                    };
                    let current_divisor = player.preview_divisor();
                    let can_switch_auto_quality = !player.is_playing()
                        || now_us - last_auto_quality_switch_us_c.get() >= 2_000_000;
                    if divisor == current_divisor || !auto_preview_mode || can_switch_auto_quality {
                        if auto_preview_mode && divisor != current_divisor {
                            last_auto_quality_switch_us_c.set(now_us);
                        }
                        player.set_preview_quality(divisor);
                    }
                    player.set_preview_luts(preview_luts);

                    let manual_proxy_mode = proxy_mode.is_enabled();
                    let current_proxy_enabled = effective_proxy_enabled.get();
                    let desired_proxy_enabled = manual_proxy_mode;
                    let desired_scale = proxy_scale_for_mode(&proxy_mode);
                    let desired_scale_divisor = match desired_scale {
                        crate::media::proxy_cache::ProxyScale::Quarter => 4,
                        _ => 2,
                    };
                    let wants_proxy_change = current_proxy_enabled != desired_proxy_enabled;
                    let wants_scale_change = desired_proxy_enabled
                        && effective_proxy_scale_divisor.get() != desired_scale_divisor;
                    if wants_proxy_change || wants_scale_change {
                        if desired_proxy_enabled && wants_scale_change {
                            proxy_cache.borrow_mut().invalidate_all();
                        }
                        player.set_proxy_enabled(desired_proxy_enabled);
                        player.set_proxy_scale_divisor(desired_scale_divisor);
                        effective_proxy_enabled.set(desired_proxy_enabled);
                        effective_proxy_scale_divisor.set(desired_scale_divisor);
                        last_auto_proxy_switch_us_c.set(now_us);
                    }
                    let refresh_proxy_paths = manual_proxy_mode;
                    if desired_proxy_enabled && refresh_proxy_paths {
                        last_proxy_refresh_us_c.set(now_us);
                        let clip_sources = {
                            let proj = project.borrow();
                            collect_unique_clip_sources(&proj)
                        };
                        {
                            let mut cache = proxy_cache.borrow_mut();
                            for (path, lut) in &clip_sources {
                                cache.request(path, desired_scale, lut.as_deref());
                            }
                        }
                        let paths = proxy_cache.borrow().proxies.clone();
                        player.update_proxy_paths(paths);
                    } else if !desired_proxy_enabled
                        && preview_luts
                        && now_us - last_proxy_refresh_us_c.get() >= 1_000_000
                    {
                        last_proxy_refresh_us_c.set(now_us);
                        let (project_w, project_h, lut_sources) = {
                            let proj = project.borrow();
                            (
                                proj.width,
                                proj.height,
                                collect_unique_lut_clip_sources(&proj),
                            )
                        };
                        {
                            let mut cache = proxy_cache.borrow_mut();
                            for (path, lut) in &lut_sources {
                                cache.request(
                                    path,
                                    crate::media::proxy_cache::ProxyScale::Project {
                                        width: project_w,
                                        height: project_h,
                                    },
                                    lut.as_deref(),
                                );
                            }
                        }
                        let paths = proxy_cache.borrow().proxies.clone();
                        player.update_proxy_paths(paths);
                    }
                }
                player.poll();
                let (oa, ob) = player.transition_opacities();
                let sf = if scopes_rev.reveals_child() {
                    player.try_pull_scope_frame()
                } else {
                    None
                };
                let rate = player.jkl_rate();
                (
                    player.timeline_pos_ns,
                    player.is_playing(),
                    oa,
                    ob,
                    player.audio_peak_db,
                    player.audio_track_peak_db.clone(),
                    sf,
                    rate,
                )
            };
            // Apply cross-dissolve opacities to the two program monitor pictures.
            picture_a_poll.set_opacity(opacity_a);
            picture_b_poll.set_opacity(opacity_b);
            // Force monitor repaint while paused so post-seek paintable updates
            // become visible even when timeline position is unchanged between ticks.
            if !playing {
                picture_a_poll.queue_draw();
                picture_b_poll.queue_draw();
            }
            // Update VU meter with current audio peak levels.
            vu_pc.set(peaks);
            vu.queue_draw();
            ts.borrow_mut().track_audio_peak_db = track_peaks;
            // Update colour scopes with the latest video frame.
            if let Some(frame) = scope_frame {
                crate::ui::color_scopes::update_scope_frame(&scopes_st, frame);
            }
            // Update J/K/L speed label.
            if jkl_rate == 0.0 || jkl_rate == 1.0 {
                speed_lbl.set_visible(false);
            } else {
                let abs = jkl_rate.abs() as u32;
                let arrow = if jkl_rate > 0.0 { "▶▶" } else { "◀◀" };
                speed_lbl.set_text(&format!("{arrow} {abs}×"));
                speed_lbl.set_visible(true);
            }
            if pos_ns != last_pos_ns_c.get() {
                let frame_rate = { project.borrow().frame_rate.clone() };
                pos_label.set_text(&program_monitor::format_timecode(pos_ns, &frame_rate));
                ts.borrow_mut().playhead_ns = pos_ns;
                let should_draw = if !playing {
                    true
                } else {
                    let last = last_draw_ns_c.get();
                    last == u64::MAX || pos_ns.saturating_sub(last) >= 50_000_000
                };
                if should_draw {
                    if let Some(ref w) = *cell.borrow() {
                        w.queue_draw();
                    }
                    last_draw_ns_c.set(pos_ns);
                }
                // Update transform overlay handles to reflect keyframe-interpolated
                // position at the new playhead time.
                if let Some(ref to) = *transform_overlay_poll.borrow() {
                    let selected = timeline_state_poll.borrow().selected_clip_id.clone();
                    if selected.is_some() {
                        let proj = project.borrow();
                        sync_transform_overlay_to_playhead(to, &proj, selected.as_deref(), pos_ns);
                    }
                }
                // Update inspector sliders to reflect keyframe-evaluated values
                // at the new playhead position.
                {
                    let proj = project.borrow();
                    inspector_view_poll.update_keyframed_sliders(&proj, pos_ns);
                }
                last_pos_ns_c.set(pos_ns);
            }
            glib::ControlFlow::Continue
        });
    }

    // Scopes toggle for the docked monitor/scopes split.
    {
        let scopes_btn = gtk::ToggleButton::with_label("▾ Scopes");
        scopes_btn.add_css_class("flat");
        scopes_btn.set_halign(gtk::Align::Start);
        scopes_btn.set_margin_start(4);
        let rev = scopes_revealer.clone();
        let docked_paned = docked_scopes_paned.clone();
        let monitor_state = monitor_state.clone();
        let prog_player_scope = prog_player.clone();
        scopes_btn.connect_toggled(move |b| {
            let visible = b.is_active();
            prog_player_scope.borrow().set_scope_enabled(visible);
            if visible {
                if docked_paned.end_child().is_none() {
                    docked_paned.set_end_child(Some(&rev));
                    let pos = monitor_state.borrow().docked_split_pos.max(160);
                    docked_paned.set_position(pos);
                }
                rev.set_reveal_child(true);
            } else {
                let pos = docked_paned.position().max(160);
                {
                    let mut state = monitor_state.borrow_mut();
                    if state.docked_split_pos != pos {
                        state.docked_split_pos = pos;
                        crate::ui_state::save_program_monitor_state(&state);
                    }
                }
                rev.set_reveal_child(false);
                docked_paned.set_end_child(Option::<&gtk::Widget>::None);
            }
        });
        prog_monitor_host.append(&scopes_btn);
    }
    let program_empty_hint = gtk::Label::new(Some(
        "Import media, then append or insert a clip to preview your timeline here.",
    ));
    program_empty_hint.set_halign(gtk::Align::Start);
    program_empty_hint.set_xalign(0.0);
    program_empty_hint.set_wrap(true);
    program_empty_hint.set_margin_start(8);
    program_empty_hint.set_margin_end(8);
    program_empty_hint.set_margin_bottom(6);
    program_empty_hint.add_css_class("panel-empty-state");
    program_empty_hint.set_visible(true);
    prog_monitor_host.append(&program_empty_hint);
    prog_monitor_host.append(&docked_scopes_paned);
    top_paned.set_end_child(Some(&prog_monitor_host));

    // Program monitor pop-out/dock toggle
    *on_toggle_popout_impl.borrow_mut() = Some({
        let app = app.clone();
        let docked_paned = docked_scopes_paned.clone();
        let monitor = prog_monitor_widget.clone();
        let pop_cell = popout_window_cell.clone();
        let popped = monitor_popped.clone();
        let monitor_state = monitor_state.clone();
        let scopes_rev = scopes_revealer.clone();
        Rc::new(move || {
            if !popped.get() {
                let state = monitor_state.borrow().clone();
                let pop_win = ApplicationWindow::builder()
                    .application(&app)
                    .title("UltimateSlice — Program Monitor")
                    .default_width(state.width.max(320))
                    .default_height(state.height.max(180))
                    .build();

                docked_paned.set_start_child(Option::<&gtk::Widget>::None);
                pop_win.set_child(Some(&monitor));
                scopes_rev.set_vexpand(true);

                let docked_paned_c = docked_paned.clone();
                let monitor_c = monitor.clone();
                let pop_cell_c = pop_cell.clone();
                let popped_c = popped.clone();
                let monitor_state_c = monitor_state.clone();
                let scopes_rev_c = scopes_rev.clone();
                pop_win.connect_close_request(move |w| {
                    let mut state = monitor_state_c.borrow_mut();
                    state.width = w.width().max(320);
                    state.height = w.height().max(180);
                    state.popped = false;
                    crate::ui_state::save_program_monitor_state(&state);
                    w.set_child(Option::<&gtk::Widget>::None);
                    if monitor_c.parent().is_none() {
                        docked_paned_c.set_start_child(Some(&monitor_c));
                    }
                    scopes_rev_c.set_vexpand(false);
                    popped_c.set(false);
                    *pop_cell_c.borrow_mut() = None;
                    glib::Propagation::Proceed
                });

                pop_win.present();
                popped.set(true);
                {
                    let mut state = monitor_state.borrow_mut();
                    state.popped = true;
                    crate::ui_state::save_program_monitor_state(&state);
                }
                *pop_cell.borrow_mut() = Some(pop_win);
            } else {
                let win = pop_cell.borrow().as_ref().cloned();
                if let Some(w) = win {
                    w.close();
                }
            }
        })
    });

    // ── on_append: reads source_marks, creates clip, adds to timeline ─────
    *on_append_impl.borrow_mut() = Some({
        let project = project.clone();
        let source_marks = source_marks.clone();
        let on_project_changed = on_project_changed.clone();
        let timeline_state = timeline_state.clone();
        let preferences_state = preferences_state.clone();
        Rc::new(move || {
            let marks = source_marks.borrow();
            if marks.path.is_empty() {
                return;
            }
            let path = marks.path.clone();
            let in_ns = marks.in_ns;
            let out_ns = marks.out_ns;
            let source_info = SourcePlacementInfo {
                is_audio_only: marks.is_audio_only,
                has_audio: marks.has_audio,
                source_timecode_base_ns: marks.source_timecode_base_ns,
            };
            drop(marks);

            let ts = timeline_state.borrow();
            let magnetic_mode = ts.magnetic_mode;
            let active_tid = ts.selected_track_id.clone();
            drop(ts);
            let source_monitor_auto_link_av =
                preferences_state.borrow().source_monitor_auto_link_av;

            {
                let mut proj = project.borrow_mut();
                let placement_plan = build_source_placement_plan_by_track_id(
                    &proj,
                    active_tid.as_deref(),
                    source_info,
                    source_monitor_auto_link_av,
                );
                if let Some(primary_target) = placement_plan.targets.first() {
                    let timeline_start = proj.tracks[primary_target.track_index].duration();
                    let magnetic_mode_for_placement =
                        magnetic_mode && !placement_plan.uses_linked_pair();
                    for (track_idx, clip) in build_source_clips_for_plan(
                        &placement_plan,
                        &path,
                        in_ns,
                        out_ns,
                        timeline_start,
                        source_info.source_timecode_base_ns,
                    ) {
                        let _ = add_clip_to_track(
                            &mut proj.tracks[track_idx],
                            clip,
                            magnetic_mode_for_placement,
                        );
                    }
                    proj.dirty = true;
                }
            }
            on_project_changed();
        })
    });

    // ── on_insert: reads source_marks, creates clip at playhead, shifts subsequent clips ──
    *on_insert_impl.borrow_mut() = Some({
        let project = project.clone();
        let source_marks = source_marks.clone();
        let on_project_changed = on_project_changed.clone();
        let timeline_state = timeline_state.clone();
        let preferences_state = preferences_state.clone();
        Rc::new(move || {
            let marks = source_marks.borrow();
            if marks.path.is_empty() {
                return;
            }
            let path = marks.path.clone();
            let in_ns = marks.in_ns;
            let out_ns = marks.out_ns;
            let source_info = SourcePlacementInfo {
                is_audio_only: marks.is_audio_only,
                has_audio: marks.has_audio,
                source_timecode_base_ns: marks.source_timecode_base_ns,
            };
            drop(marks);

            let ts = timeline_state.borrow();
            let magnetic_mode = ts.magnetic_mode;
            let playhead = ts.playhead_ns;
            let active_tid = ts.selected_track_id.clone();
            drop(ts);
            let source_monitor_auto_link_av =
                preferences_state.borrow().source_monitor_auto_link_av;

            let clip_duration = out_ns.saturating_sub(in_ns);
            if clip_duration == 0 {
                return;
            }

            {
                let mut proj = project.borrow_mut();
                let placement_plan = build_source_placement_plan_by_track_id(
                    &proj,
                    active_tid.as_deref(),
                    source_info,
                    source_monitor_auto_link_av,
                );
                let mut track_changes: Vec<TrackClipsChange> = Vec::new();
                let magnetic_mode_for_placement =
                    magnetic_mode && !placement_plan.uses_linked_pair();
                for (track_idx, clip) in build_source_clips_for_plan(
                    &placement_plan,
                    &path,
                    in_ns,
                    out_ns,
                    playhead,
                    source_info.source_timecode_base_ns,
                ) {
                    track_changes.push(insert_clip_at_playhead_on_track(
                        &mut proj.tracks[track_idx],
                        clip,
                        playhead,
                        magnetic_mode_for_placement,
                    ));
                }

                if !track_changes.is_empty() {
                    drop(proj);

                    let cmd: Box<dyn crate::undo::EditCommand> = if track_changes.len() == 1 {
                        let change = track_changes.pop().unwrap();
                        Box::new(crate::undo::SetTrackClipsCommand {
                            track_id: change.track_id,
                            old_clips: change.old_clips,
                            new_clips: change.new_clips,
                            label: "Insert at playhead".to_string(),
                        })
                    } else {
                        Box::new(crate::undo::SetMultipleTracksClipsCommand {
                            changes: track_changes,
                            label: "Insert at playhead".to_string(),
                        })
                    };
                    let st = timeline_state.borrow_mut();
                    let project_rc = st.project.clone();
                    drop(st);
                    let mut proj = project_rc.borrow_mut();
                    timeline_state.borrow_mut().history.undo_stack.push(cmd);
                    timeline_state.borrow_mut().history.redo_stack.clear();
                    proj.dirty = true;
                }
            }
            on_project_changed();
        })
    });

    // ── on_overwrite: reads source_marks, replaces timeline range at playhead ──
    *on_overwrite_impl.borrow_mut() = Some({
        let project = project.clone();
        let source_marks = source_marks.clone();
        let on_project_changed = on_project_changed.clone();
        let timeline_state = timeline_state.clone();
        let preferences_state = preferences_state.clone();
        Rc::new(move || {
            let marks = source_marks.borrow();
            if marks.path.is_empty() {
                return;
            }
            let path = marks.path.clone();
            let in_ns = marks.in_ns;
            let out_ns = marks.out_ns;
            let source_info = SourcePlacementInfo {
                is_audio_only: marks.is_audio_only,
                has_audio: marks.has_audio,
                source_timecode_base_ns: marks.source_timecode_base_ns,
            };
            drop(marks);

            let ts = timeline_state.borrow();
            let magnetic_mode = ts.magnetic_mode;
            let playhead = ts.playhead_ns;
            let active_tid = ts.selected_track_id.clone();
            drop(ts);
            let source_monitor_auto_link_av =
                preferences_state.borrow().source_monitor_auto_link_av;

            let clip_duration = out_ns.saturating_sub(in_ns);
            if clip_duration == 0 {
                return;
            }

            let range_start = playhead;
            let range_end = playhead + clip_duration;

            {
                let mut proj = project.borrow_mut();
                let placement_plan = build_source_placement_plan_by_track_id(
                    &proj,
                    active_tid.as_deref(),
                    source_info,
                    source_monitor_auto_link_av,
                );
                let mut track_changes: Vec<TrackClipsChange> = Vec::new();
                let magnetic_mode_for_placement =
                    magnetic_mode && !placement_plan.uses_linked_pair();
                for (track_idx, clip) in build_source_clips_for_plan(
                    &placement_plan,
                    &path,
                    in_ns,
                    out_ns,
                    playhead,
                    source_info.source_timecode_base_ns,
                ) {
                    track_changes.push(overwrite_clip_range_on_track(
                        &mut proj.tracks[track_idx],
                        clip,
                        range_start,
                        range_end,
                        magnetic_mode_for_placement,
                    ));
                }

                if !track_changes.is_empty() {
                    drop(proj);

                    let cmd: Box<dyn crate::undo::EditCommand> = if track_changes.len() == 1 {
                        let change = track_changes.pop().unwrap();
                        Box::new(crate::undo::SetTrackClipsCommand {
                            track_id: change.track_id,
                            old_clips: change.old_clips,
                            new_clips: change.new_clips,
                            label: "Overwrite at playhead".to_string(),
                        })
                    } else {
                        Box::new(crate::undo::SetMultipleTracksClipsCommand {
                            changes: track_changes,
                            label: "Overwrite at playhead".to_string(),
                        })
                    };
                    let st = timeline_state.borrow_mut();
                    let project_rc = st.project.clone();
                    drop(st);
                    let mut proj = project_rc.borrow_mut();
                    timeline_state.borrow_mut().history.undo_stack.push(cmd);
                    timeline_state.borrow_mut().history.redo_stack.clear();
                    proj.dirty = true;
                }
            }
            on_project_changed();
        })
    });

    // ── on_source_selected: loads clip into player + resets source_marks ──
    let on_source_selected: Rc<dyn Fn(String, u64)> = {
        let player = player.clone();
        let source_marks = source_marks.clone();
        let preview_widget = preview_widget.clone();
        let clip_name_label = clip_name_label.clone();
        let library = library.clone();
        let project = project.clone();
        let proxy_cache = proxy_cache.clone();
        let preferences_state = preferences_state.clone();
        let source_original_uri_for_proxy_fallback = source_original_uri_for_proxy_fallback.clone();
        Rc::new(move |path: String, duration_ns: u64| {
            // Show the source preview now that a clip is selected
            preview_widget.set_visible(true);
            // Update the clip name label
            let name = std::path::Path::new(&path)
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or(&path)
                .to_string();
            clip_name_label.set_text(&name);
            // Guard against duplicate selection-changed emissions for the same
            // item; avoid redundant playbin reconfiguration.
            let should_reload = {
                let m = source_marks.borrow();
                m.path != path
            };
            let source_info = {
                let lib = library.borrow();
                let proj = project.borrow();
                lookup_source_placement_info(&lib, &proj, &path)
            };
            if should_reload {
                let proxy_mode = preferences_state.borrow().proxy_mode.clone();
                let source_proxy_enabled = proxy_mode.is_enabled();
                let original_uri = format!("file://{path}");
                if let Ok(mut fallback_uri) = source_original_uri_for_proxy_fallback.lock() {
                    *fallback_uri = Some(original_uri.clone());
                }
                if source_proxy_enabled && !source_info.is_audio_only {
                    proxy_cache.borrow_mut().request(
                        &path,
                        proxy_scale_for_mode(&proxy_mode),
                        None,
                    );
                }
                let load_uri = {
                    let cache = proxy_cache.borrow();
                    if source_proxy_enabled {
                        if let Some(proxy_path) = ready_proxy_path_for_source(&cache, &path, None) {
                            format!("file://{proxy_path}")
                        } else {
                            original_uri.clone()
                        }
                    } else {
                        original_uri
                    }
                };
                let _ = player.borrow().load(&load_uri);
            }
            let mut m = source_marks.borrow_mut();
            m.path = path;
            m.duration_ns = duration_ns;
            m.in_ns = 0;
            m.out_ns = duration_ns;
            m.display_pos_ns = 0;
            m.is_audio_only = source_info.is_audio_only;
            m.has_audio = source_info.has_audio;
            m.source_timecode_base_ns = source_info.source_timecode_base_ns;
        })
    };

    // ── Media browser ─────────────────────────────────────────────────────
    let (browser, clear_media_selection) =
        media_browser::build_media_browser(library.clone(), on_source_selected.clone());
    // ── on_close_preview: deselect media + hide preview + reset source state ──
    *on_close_preview_impl.borrow_mut() = Some({
        let clear_media_selection = clear_media_selection.clone();
        let preview_widget = preview_widget.clone();
        let clip_name_label = clip_name_label.clone();
        let source_marks = source_marks.clone();
        let player = player.clone();
        let source_original_uri_for_proxy_fallback = source_original_uri_for_proxy_fallback.clone();
        Rc::new(move || {
            clear_media_selection();
            preview_widget.set_visible(false);
            clip_name_label.set_text("No source loaded");
            {
                let mut m = source_marks.borrow_mut();
                *m = crate::model::media_library::SourceMarks::default();
            }
            if let Ok(mut fallback_uri) = source_original_uri_for_proxy_fallback.lock() {
                *fallback_uri = None;
            }
            let _ = player.borrow().stop();
        })
    });
    // Left panel: vertical Paned — browser (top) + source preview (bottom, hidden until selection)
    // The Paned lets the user resize the split after a source is selected.
    preview_widget.set_visible(false);
    let left_vpaned = Paned::new(Orientation::Vertical);
    left_vpaned.set_vexpand(true);
    left_vpaned.set_position(320); // browser gets ~320 px by default
    left_vpaned.set_start_child(Some(&browser));
    left_vpaned.set_end_child(Some(&preview_widget));
    top_paned.set_start_child(Some(&left_vpaned));

    root_vpaned.set_start_child(Some(&top_paned));

    // ── Timeline ──────────────────────────────────────────────────────────
    let timeline_scroll = ScrolledWindow::new();
    timeline_scroll.set_policy(gtk::PolicyType::Automatic, gtk::PolicyType::Automatic);
    timeline_scroll.set_vexpand(true);
    timeline_scroll.set_hexpand(true);

    let (timeline_panel, timeline_area) =
        build_timeline_panel(timeline_state.clone(), on_project_changed.clone());
    timeline_scroll.set_child(Some(&timeline_panel));
    root_vpaned.set_end_child(Some(&timeline_scroll));

    // Fill in the timeline area cell so the poll timer + stop button can redraw it.
    *timeline_panel_cell.borrow_mut() = Some(timeline_area.clone().upcast::<gtk4::Widget>());

    // Now that timeline_panel exists, fill in the real on_project_changed implementation.
    // This runs after every edit: updates title, inspector, program player clip list,
    // and queues a redraw on the timeline.
    {
        let inspector_view = inspector_view.clone();
        let project = project.clone();
        let timeline_state = timeline_state.clone();
        let library = library.clone();
        let on_close_preview = on_close_preview.clone();
        let window_weak = window_weak.clone();
        let prog_player = prog_player.clone();
        let proxy_cache = proxy_cache.clone();
        let bg_removal_cache = bg_removal_cache.clone();
        let preferences_state = preferences_state.clone();
        let panel_weak = timeline_area.downgrade();
        let transform_overlay_cell = transform_overlay_cell.clone();
        let prog_canvas_frame = prog_canvas_frame.clone();
        let program_empty_hint = program_empty_hint.clone();
        let picture_a = picture_a.clone();
        let picture_b = picture_b.clone();
        let pending_reload_ticket = pending_reload_ticket.clone();
        let mcp_light_refresh_next = mcp_light_refresh_next.clone();
        let suppress_resume_on_next_reload = suppress_resume_on_next_reload.clone();
        let clear_media_browser_on_next_reload = clear_media_browser_on_next_reload.clone();

        *on_project_changed_impl.borrow_mut() = Some(Box::new(move || {
            let use_light_refresh = mcp_light_refresh_next.replace(false);
            if clear_media_browser_on_next_reload.replace(false) {
                on_close_preview();
                library.borrow_mut().clear();
                prog_player.borrow_mut().stop();
                let proxy_mode_enabled = preferences_state.borrow().proxy_mode.is_enabled();
                {
                    let mut cache = proxy_cache.borrow_mut();
                    cache.cleanup_for_unload(proxy_mode_enabled);
                    cache.invalidate_all();
                }
                prog_player.borrow_mut().update_proxy_paths(HashMap::new());
            }

            // Update window title
            if let Some(win) = window_weak.upgrade() {
                let proj = project.borrow();
                let dirty_marker = if proj.dirty { " •" } else { "" };
                win.set_title(Some(&format!(
                    "UltimateSlice — {}{dirty_marker}",
                    proj.title
                )));
            }

            // Update inspector and collect program clips — drop proj borrow before GStreamer call
            let (clips, media_from_project, project_dims, project_frame_rate): (
                Vec<ProgramClip>,
                Vec<(String, u64, Option<u64>)>,
                (u32, u32),
                (u32, u32),
            ) = {
                let proj = project.borrow();
                let selected = timeline_state.borrow().selected_clip_id.clone();
                let playhead_ns = timeline_state.borrow().playhead_ns;
                inspector_view.update(&proj, selected.as_deref(), playhead_ns);
                inspector_view.update_keyframe_indicator(&proj, playhead_ns);

                // Sync transform overlay: show handles when a clip is selected,
                // using keyframe-interpolated values at the current playhead.
                if let Some(ref to) = *transform_overlay_cell.borrow() {
                    to.set_project_dimensions(proj.width, proj.height);
                    // Keep canvas frame aspect ratio in sync with project dimensions.
                    if proj.height > 0 {
                        prog_canvas_frame.set_ratio(proj.width as f32 / proj.height as f32);
                    }
                    let playhead_ns = timeline_state.borrow().playhead_ns;
                    sync_transform_overlay_to_playhead(to, &proj, selected.as_deref(), playhead_ns);
                }

                let suppress_embedded_audio_ids: HashSet<String> = proj
                    .tracks
                    .iter()
                    .flat_map(|track| track.clips.iter())
                    .filter(|clip| {
                        clip.kind == ClipKind::Video
                            && proj
                                .tracks
                                .iter()
                                .flat_map(|peer_track| peer_track.clips.iter())
                                .any(|peer| clip.suppresses_embedded_audio_for_linked_peer(peer))
                    })
                    .map(|clip| clip.id.clone())
                    .collect();

                let clips = proj
                    .tracks
                    .iter()
                    .enumerate()
                    .filter(|(_, t)| proj.track_is_active_for_output(t))
                    .flat_map(|(t_idx, t)| {
                        let audio_only = t.kind == TrackKind::Audio;
                        let suppress_embedded_audio_ids = suppress_embedded_audio_ids.clone();
                        t.clips.iter().map(move |c| ProgramClip {
                            id: c.id.clone(),
                            source_path: c.source_path.clone(),
                            source_in_ns: c.source_in,
                            source_out_ns: c.source_out,
                            timeline_start_ns: c.timeline_start,
                            brightness: c.brightness as f64,
                            contrast: c.contrast as f64,
                            saturation: c.saturation as f64,
                            temperature: c.temperature as f64,
                            tint: c.tint as f64,
                            brightness_keyframes: c.brightness_keyframes.clone(),
                            contrast_keyframes: c.contrast_keyframes.clone(),
                            saturation_keyframes: c.saturation_keyframes.clone(),
                            temperature_keyframes: c.temperature_keyframes.clone(),
                            tint_keyframes: c.tint_keyframes.clone(),
                            denoise: c.denoise as f64,
                            sharpness: c.sharpness as f64,
                            volume: c.volume as f64,
                            volume_keyframes: c.volume_keyframes.clone(),
                            pan: c.pan as f64,
                            pan_keyframes: c.pan_keyframes.clone(),
                            crop_left: c.crop_left,
                            crop_left_keyframes: c.crop_left_keyframes.clone(),
                            crop_right: c.crop_right,
                            crop_right_keyframes: c.crop_right_keyframes.clone(),
                            crop_top: c.crop_top,
                            crop_top_keyframes: c.crop_top_keyframes.clone(),
                            crop_bottom: c.crop_bottom,
                            crop_bottom_keyframes: c.crop_bottom_keyframes.clone(),
                            rotate: c.rotate,
                            rotate_keyframes: c.rotate_keyframes.clone(),
                            flip_h: c.flip_h,
                            flip_v: c.flip_v,
                            title_text: c.title_text.clone(),
                            title_font: c.title_font.clone(),
                            title_color: c.title_color,
                            title_x: c.title_x,
                            title_y: c.title_y,
                            speed: c.speed,
                            speed_keyframes: c.speed_keyframes.clone(),
                            reverse: c.reverse,
                            freeze_frame: c.freeze_frame,
                            freeze_frame_source_ns: c.freeze_frame_source_ns,
                            freeze_frame_hold_duration_ns: c.freeze_frame_hold_duration_ns,
                            is_audio_only: audio_only,
                            track_index: t_idx,
                            transition_after: c.transition_after.clone(),
                            transition_after_ns: c.transition_after_ns,
                            lut_path: c.lut_path.clone(),
                            scale: c.scale,
                            scale_keyframes: c.scale_keyframes.clone(),
                            opacity: c.opacity,
                            opacity_keyframes: c.opacity_keyframes.clone(),
                            position_x: c.position_x,
                            position_x_keyframes: c.position_x_keyframes.clone(),
                            position_y: c.position_y,
                            position_y_keyframes: c.position_y_keyframes.clone(),
                            shadows: c.shadows as f64,
                            midtones: c.midtones as f64,
                            highlights: c.highlights as f64,
                            exposure: c.exposure as f64,
                            black_point: c.black_point as f64,
                            highlights_warmth: c.highlights_warmth as f64,
                            highlights_tint: c.highlights_tint as f64,
                            midtones_warmth: c.midtones_warmth as f64,
                            midtones_tint: c.midtones_tint as f64,
                            shadows_warmth: c.shadows_warmth as f64,
                            shadows_tint: c.shadows_tint as f64,
                            has_audio: !c.is_freeze_frame()
                                && !suppress_embedded_audio_ids.contains(&c.id),
                            chroma_key_enabled: c.chroma_key_enabled,
                            chroma_key_color: c.chroma_key_color,
                            chroma_key_tolerance: c.chroma_key_tolerance,
                            chroma_key_softness: c.chroma_key_softness,
                            bg_removal_enabled: c.bg_removal_enabled,
                            bg_removal_threshold: c.bg_removal_threshold,
                        })
                    })
                    .collect();
                // Keep media browser in sync with timeline clip sources after project open/load.
                // Collect only unique source paths to avoid redundant work.
                let mut media_seen: HashSet<&str> = HashSet::new();
                let media: Vec<(String, u64, Option<u64>)> = proj
                    .tracks
                    .iter()
                    .flat_map(|t| t.clips.iter())
                    .filter(|c| media_seen.insert(c.source_path.as_str()))
                    .map(|c| {
                        (
                            c.source_path.clone(),
                            c.source_out,
                            c.source_timecode_base_ns,
                        )
                    })
                    .collect();
                (
                    clips,
                    media,
                    (proj.width, proj.height),
                    (proj.frame_rate.numerator, proj.frame_rate.denominator),
                )
            }; // proj borrow dropped here — safe to call GStreamer below
            program_empty_hint.set_visible(clips.is_empty());
            let has_clips = !clips.is_empty();
            picture_a.set_visible(has_clips);
            picture_b.set_visible(has_clips);

            {
                let mut lib = library.borrow_mut();
                let seen: HashSet<String> = lib.iter().map(|i| i.source_path.clone()).collect();
                for (path, dur, source_timecode_base_ns) in &media_from_project {
                    if let Some(item) = lib.iter_mut().find(|i| i.source_path == *path) {
                        if item.duration_ns == 0 && *dur > 0 {
                            item.duration_ns = *dur;
                        }
                        if item.source_timecode_base_ns.is_none()
                            && source_timecode_base_ns.is_some()
                        {
                            item.source_timecode_base_ns = *source_timecode_base_ns;
                        }
                    }
                }
                let new_items: Vec<_> = media_from_project
                    .into_iter()
                    .filter(|(path, _, _)| !seen.contains(path))
                    .collect();
                for (path, dur, source_timecode_base_ns) in new_items {
                    let mut item = MediaItem::new(path, dur);
                    item.source_timecode_base_ns = source_timecode_base_ns;
                    lib.push(item);
                }
            }

            // Reload program player — preserve current position so the monitor
            // doesn't jump to 0 on every project change (e.g., clip name edit).
            let suppress_resume = suppress_resume_on_next_reload.replace(false);
            let (prev_pos, was_playing) = {
                let pp = prog_player.borrow();
                (
                    pp.timeline_pos_ns,
                    !suppress_resume
                        && matches!(pp.state(), crate::media::player::PlayerState::Playing),
                )
            };
            let (proj_w, proj_h) = project_dims;
            let (fr_num, fr_den) = project_frame_rate;
            let prog_player_reload = prog_player.clone();
            let preferences_state_reload = preferences_state.clone();
            let project_reload = project.clone();
            let proxy_cache_reload = proxy_cache.clone();
            let bg_removal_cache_reload = bg_removal_cache.clone();
            let reload_ticket = pending_reload_ticket.get().wrapping_add(1);
            pending_reload_ticket.set(reload_ticket);
            let pending_reload_ticket_phase1 = pending_reload_ticket.clone();
            glib::timeout_add_local_once(std::time::Duration::from_millis(0), move || {
                if pending_reload_ticket_phase1.get() != reload_ticket {
                    return;
                }
                let phase1_started = std::time::Instant::now();
                const NEAR_PLAYHEAD_PROXY_PRIME_WINDOW_NS: u64 = 8_000_000_000;
                const NEAR_PLAYHEAD_PROXY_PRIME_MAX_SOURCES: usize = 8;
                if !use_light_refresh {
                    // Resolve proxy paths BEFORE load_clips so the first
                    // rebuild_pipeline_at() uses proxies instead of originals.
                    {
                        let proxy_mode = preferences_state_reload.borrow().proxy_mode.clone();
                        let manual_proxy_mode = proxy_mode.is_enabled();
                        if manual_proxy_mode {
                            let manual_scale = proxy_scale_for_mode(&proxy_mode);
                            let near_playhead_sources: Vec<(String, Option<String>)> = {
                                let proj = project_reload.borrow();
                                collect_near_playhead_clip_sources(
                                    &proj,
                                    prev_pos,
                                    NEAR_PLAYHEAD_PROXY_PRIME_WINDOW_NS,
                                    NEAR_PLAYHEAD_PROXY_PRIME_MAX_SOURCES,
                                )
                            };
                            {
                                let mut cache = proxy_cache_reload.borrow_mut();
                                for (path, lut) in &near_playhead_sources {
                                    cache.request(path, manual_scale, lut.as_deref());
                                }
                            }
                            let clip_sources: Vec<(String, Option<String>)> = {
                                let proj = project_reload.borrow();
                                let mut seen: HashSet<(String, Option<String>)> = HashSet::new();
                                proj.tracks
                                    .iter()
                                    .flat_map(|t| t.clips.iter())
                                    .filter_map(|c| {
                                        let key = (c.source_path.clone(), c.lut_path.clone());
                                        if seen.insert(key.clone()) {
                                            Some(key)
                                        } else {
                                            None
                                        }
                                    })
                                    .collect()
                            };
                            {
                                let mut cache = proxy_cache_reload.borrow_mut();
                                for (path, lut) in &clip_sources {
                                    cache.request(path, manual_scale, lut.as_deref());
                                }
                            }
                            if !near_playhead_sources.is_empty() {
                                log::debug!(
                                    "window:on_project_changed primed {} near-playhead proxy source(s) around {}ns",
                                    near_playhead_sources.len(),
                                    prev_pos
                                );
                            }
                        }
                        let paths = proxy_cache_reload.borrow().proxies.clone();
                        prog_player_reload.borrow_mut().update_proxy_paths(paths);
                    }

                    // Request bg-removal for clips that have it enabled.
                    {
                        let proj = project_reload.borrow();
                        let mut cache = bg_removal_cache_reload.borrow_mut();
                        for track in &proj.tracks {
                            for clip in &track.clips {
                                if clip.bg_removal_enabled {
                                    cache.request(&clip.source_path, clip.bg_removal_threshold);
                                }
                            }
                        }
                        let paths = cache.paths.clone();
                        prog_player_reload
                            .borrow_mut()
                            .update_bg_removal_paths(paths);
                    }
                }

                {
                    let mut pp = prog_player_reload.borrow_mut();
                    pp.set_project_dimensions(proj_w, proj_h);
                    pp.set_frame_rate(fr_num, fr_den);
                    pp.load_clips(clips);
                }
                log::debug!(
                    "window:on_project_changed phase1_load ticket={} elapsed_ms={}",
                    reload_ticket,
                    phase1_started.elapsed().as_millis()
                );

                let prog_player_reload_phase2 = prog_player_reload.clone();
                let pending_reload_ticket_phase2 = pending_reload_ticket_phase1.clone();
                glib::timeout_add_local_once(std::time::Duration::from_millis(16), move || {
                    if pending_reload_ticket_phase2.get() != reload_ticket {
                        return;
                    }
                    let phase2_started = std::time::Instant::now();
                    let mut pp = prog_player_reload_phase2.borrow_mut();
                    if !pp.clips.is_empty() {
                        if was_playing {
                            // Preserve playback behavior after clip reloads.
                            let _ = pp.seek(prev_pos);
                            pp.play();
                        } else {
                            // Rebuild the pipeline at the previous position so the
                            // program monitor shows the correct composited frame.
                            // Without this, load_clips() leaves no decoder slots
                            // loaded and the monitor can stay on the previous frame.
                            let pos = prev_pos.min(pp.timeline_dur_ns);
                            let needs_async = pp.seek(pos);
                            if needs_async {
                                drop(pp);
                                let pp2 = prog_player_reload_phase2.clone();
                                glib::timeout_add_local_once(
                                    std::time::Duration::from_millis(250),
                                    move || {
                                        pp2.borrow().complete_playing_pulse();
                                    },
                                );
                            }
                        }
                    }
                    log::debug!(
                        "window:on_project_changed phase2_seek ticket={} elapsed_ms={}",
                        reload_ticket,
                        phase2_started.elapsed().as_millis()
                    );
                });
            });

            // Force immediate timeline redraw (don't wait for 100ms timer)
            if let Some(p) = panel_weak.upgrade() {
                if let Some(area_widget) = p.first_child() {
                    if let Ok(area) = area_widget.downcast::<gtk::DrawingArea>() {
                        let track_count = project.borrow().tracks.len().max(1);
                        area.set_content_height((24.0 + 60.0 * track_count as f64) as i32);
                        area.queue_draw();
                    } else {
                        p.queue_draw();
                    }
                } else {
                    p.queue_draw();
                }
            }
        }));
    }

    root_hpaned.set_start_child(Some(&root_vpaned));

    // Right sidebar: inspector + transitions pane
    let right_sidebar = gtk::Box::new(Orientation::Vertical, 6);
    right_sidebar.set_margin_start(6);
    right_sidebar.set_margin_end(6);
    right_sidebar.set_margin_top(6);
    right_sidebar.set_margin_bottom(6);

    let inspector_scroll = ScrolledWindow::new();
    inspector_scroll.set_vexpand(true);
    inspector_scroll.set_policy(gtk::PolicyType::Never, gtk::PolicyType::Automatic);
    inspector_scroll.set_child(Some(&inspector_box));
    right_sidebar.append(&inspector_scroll);

    let transitions_header = gtk::Box::new(Orientation::Horizontal, 6);
    let transitions_title = gtk::Label::new(Some("Transitions"));
    transitions_title.set_halign(gtk::Align::Start);
    transitions_title.set_hexpand(true);
    let transitions_toggle = gtk::Button::with_label("Hide Transitions");
    transitions_toggle.add_css_class("small-btn");
    transitions_header.append(&transitions_title);
    transitions_header.append(&transitions_toggle);
    right_sidebar.append(&transitions_header);

    let transitions_revealer = gtk::Revealer::new();
    transitions_revealer.set_reveal_child(true);
    let transitions_list = gtk::ListBox::new();
    transitions_list.add_css_class("boxed-list");
    transitions_list.set_selection_mode(gtk::SelectionMode::None);

    // Helper: add a transition row with drag-source to the list.
    let add_transition_row = |list: &gtk::ListBox, display: &str, kind: &str| {
        let row = gtk::ListBoxRow::new();
        let bx = gtk::Box::new(Orientation::Horizontal, 6);
        bx.set_margin_start(8);
        bx.set_margin_end(8);
        bx.set_margin_top(6);
        bx.set_margin_bottom(6);
        let name_lbl = gtk::Label::new(Some(display));
        name_lbl.set_halign(gtk::Align::Start);
        name_lbl.set_hexpand(true);
        let hint_lbl = gtk::Label::new(Some("Drag to clip boundary"));
        hint_lbl.add_css_class("dim-label");
        bx.append(&name_lbl);
        bx.append(&hint_lbl);
        row.set_child(Some(&bx));
        let drag_src = gtk::DragSource::new();
        drag_src.set_actions(gdk4::DragAction::COPY);
        drag_src.set_exclusive(false);
        let payload = format!("transition:{kind}");
        let val = glib::Value::from(&payload);
        drag_src.set_content(Some(&gdk4::ContentProvider::for_value(&val)));
        row.add_controller(drag_src);
        list.append(&row);
    };

    add_transition_row(&transitions_list, "Cross-dissolve", "cross_dissolve");
    add_transition_row(&transitions_list, "Fade to black", "fade_to_black");
    add_transition_row(&transitions_list, "Wipe right →", "wipe_right");
    add_transition_row(&transitions_list, "← Wipe left", "wipe_left");

    transitions_revealer.set_child(Some(&transitions_list));
    right_sidebar.append(&transitions_revealer);

    {
        let revealer = transitions_revealer.clone();
        transitions_toggle.connect_clicked(move |btn| {
            let show = !revealer.reveals_child();
            revealer.set_reveal_child(show);
            btn.set_label(if show {
                "Hide Transitions"
            } else {
                "Show Transitions"
            });
        });
    }

    root_hpaned.set_end_child(Some(&right_sidebar));

    // ── Status bar (proxy progress) ───────────────────────────────────────
    let status_bar = gtk::Box::new(Orientation::Horizontal, 8);
    status_bar.set_margin_start(8);
    status_bar.set_margin_end(8);
    status_bar.set_margin_top(4);
    status_bar.set_margin_bottom(4);
    status_bar.add_css_class("status-bar");
    status_bar.set_visible(true);
    let status_label = gtk::Label::new(Some("Proxy queue idle"));
    status_label.set_halign(gtk::Align::Start);
    status_label.add_css_class("status-bar-label");
    status_label.set_visible(false);
    let status_progress = gtk::ProgressBar::new();
    status_progress.set_hexpand(true);
    status_progress.set_show_text(true);
    status_progress.set_text(Some("Idle"));
    status_progress.add_css_class("proxy-progress");
    status_progress.set_visible(false);
    let track_levels_toggle = gtk::ToggleButton::new();
    track_levels_toggle.set_active(initial_show_track_audio_levels);
    let track_levels_row = gtk::Box::new(Orientation::Horizontal, 4);
    let track_levels_icon = gtk::Image::from_icon_name(if initial_show_track_audio_levels {
        "view-reveal-symbolic"
    } else {
        "view-conceal-symbolic"
    });
    let track_levels_text = gtk::Label::new(Some("Track Audio Levels"));
    track_levels_row.append(&track_levels_icon);
    track_levels_row.append(&track_levels_text);
    track_levels_toggle.set_child(Some(&track_levels_row));
    track_levels_toggle.add_css_class("round");
    track_levels_toggle.add_css_class("flat");
    let background_render_toggle = gtk::ToggleButton::new();
    background_render_toggle.set_active(initial_background_prerender);
    let background_render_row = gtk::Box::new(Orientation::Horizontal, 4);
    let background_render_icon = gtk::Image::from_icon_name(if initial_background_prerender {
        "system-run-symbolic"
    } else {
        "process-stop-symbolic"
    });
    let background_render_text = gtk::Label::new(Some("Background Render"));
    background_render_row.append(&background_render_icon);
    background_render_row.append(&background_render_text);
    background_render_toggle.set_child(Some(&background_render_row));
    background_render_toggle.add_css_class("round");
    background_render_toggle.add_css_class("flat");
    status_bar.append(&track_levels_toggle);
    status_bar.append(&background_render_toggle);
    status_bar.append(&status_label);
    status_bar.append(&status_progress);

    // Wrap main content + status bar in a vertical box
    let outer_vbox = gtk::Box::new(Orientation::Vertical, 0);
    outer_vbox.append(&root_hpaned);
    outer_vbox.append(&status_bar);
    window.set_child(Some(&outer_vbox));

    // Poll proxy cache every 500ms to drain completed transcodes and update status bar.
    {
        let timeline_state = timeline_state.clone();
        let preferences_state = preferences_state.clone();
        let timeline_area = timeline_area.clone();
        let track_levels_icon = track_levels_icon.clone();
        track_levels_toggle.connect_toggled(move |btn| {
            let show = btn.is_active();
            timeline_state.borrow_mut().show_track_audio_levels = show;
            track_levels_icon.set_icon_name(Some(if show {
                "view-visible-symbolic"
            } else {
                "view-conceal-symbolic"
            }));
            let new_state = {
                let mut prefs = preferences_state.borrow_mut();
                prefs.show_track_audio_levels = show;
                prefs.clone()
            };
            crate::ui_state::save_preferences_state(&new_state);
            timeline_area.queue_draw();
        });
    }
    {
        let preferences_state = preferences_state.clone();
        let prog_player = prog_player.clone();
        let background_render_icon = background_render_icon.clone();
        background_render_toggle.connect_toggled(move |btn| {
            let enabled = btn.is_active();
            prog_player.borrow_mut().set_background_prerender(enabled);
            background_render_icon.set_icon_name(Some(if enabled {
                "system-run-symbolic"
            } else {
                "process-stop-symbolic"
            }));
            let new_state = {
                let mut prefs = preferences_state.borrow_mut();
                prefs.background_prerender = enabled;
                prefs.clone()
            };
            crate::ui_state::save_preferences_state(&new_state);
        });
    }

    {
        let proxy_cache = proxy_cache.clone();
        let bg_removal_cache = bg_removal_cache.clone();
        let prog_player = prog_player.clone();
        let effective_proxy_enabled = effective_proxy_enabled.clone();
        let status_label = status_label.clone();
        let status_progress = status_progress.clone();
        let player = player.clone();
        let source_marks = source_marks.clone();
        let audio_sync_in_progress = audio_sync_in_progress.clone();
        let inspector_view = inspector_view.clone();
        let preferences_state = preferences_state.clone();
        glib::timeout_add_local(std::time::Duration::from_millis(500), move || {
            let resolved = proxy_cache.borrow_mut().poll();
            // Always sync proxy paths when proxies are effectively enabled — disk-cached proxies
            // are added synchronously by request() and never appear in `resolved`.
            if effective_proxy_enabled.get() {
                if !resolved.is_empty() || !proxy_cache.borrow().proxies.is_empty() {
                    let paths = proxy_cache.borrow().proxies.clone();
                    prog_player.borrow_mut().update_proxy_paths(paths);
                }
            }
            // Auto-reload source preview when its proxy completes.
            let source_proxy_enabled = preferences_state.borrow().proxy_mode.is_enabled();
            if source_proxy_enabled && !resolved.is_empty() {
                let current_source = source_marks.borrow().path.clone();
                if !current_source.is_empty() {
                    let cache = proxy_cache.borrow();
                    for key in &resolved {
                        if *key == current_source {
                            if let Some(proxy_path) =
                                ready_proxy_path_for_source(&cache, &current_source, None)
                            {
                                let uri = format!("file://{proxy_path}");
                                let _ = player.borrow().load(&uri);
                            }
                            break;
                        }
                    }
                }
            }
            // Poll bg-removal cache and sync paths to ProgramPlayer.
            {
                let bg_resolved = bg_removal_cache.borrow_mut().poll();
                if !bg_resolved.is_empty() || !bg_removal_cache.borrow().paths.is_empty() {
                    let paths = bg_removal_cache.borrow().paths.clone();
                    prog_player.borrow_mut().update_bg_removal_paths(paths);
                }
                // Keep inspector section visibility in sync with model availability.
                inspector_view
                    .bg_removal_model_available
                    .set(bg_removal_cache.borrow().is_available());
            }
            let proxy_progress = proxy_cache.borrow().progress();
            let prerender_progress = prog_player.borrow().background_prerender_progress();
            let bg_progress = bg_removal_cache.borrow().progress();
            let proxy_active = proxy_progress.in_flight;
            let prerender_active = prerender_progress.in_flight;
            let bg_active = bg_progress.in_flight;
            let syncing_audio = audio_sync_in_progress.get();
            if proxy_active || prerender_active || syncing_audio || bg_active {
                status_label.set_visible(true);
                let mut parts = Vec::new();
                if syncing_audio {
                    parts.push("Syncing audio…".to_string());
                }
                if proxy_active {
                    parts.push(format!(
                        "Generating proxies… {}/{}",
                        proxy_progress.completed, proxy_progress.total
                    ));
                }
                if prerender_active {
                    parts.push(format!(
                        "Prerendering… {}/{}",
                        prerender_progress.completed, prerender_progress.total
                    ));
                }
                if bg_active {
                    parts.push(format!(
                        "Removing backgrounds… {}/{}",
                        bg_progress.completed, bg_progress.total
                    ));
                }
                status_label.set_text(&parts.join(" | "));
                if proxy_active {
                    status_progress.set_visible(true);
                    let fraction = proxy_progress.byte_fraction.unwrap_or_else(|| {
                        if proxy_progress.total > 0 {
                            (proxy_progress.completed as f64 / proxy_progress.total as f64)
                                .clamp(0.0, 0.99)
                        } else {
                            0.0
                        }
                    });
                    status_progress.set_fraction(fraction);
                    status_progress.set_text(Some(&format!("{:.0}%", fraction * 100.0)));
                } else if prerender_active && prerender_progress.total > 0 {
                    status_progress.set_visible(true);
                    let fraction = (prerender_progress.completed as f64
                        / prerender_progress.total as f64)
                        .clamp(0.0, 0.99);
                    status_progress.set_fraction(fraction);
                    status_progress.set_text(Some(&format!("{:.0}%", fraction * 100.0)));
                } else if bg_active && bg_progress.total > 0 {
                    status_progress.set_visible(true);
                    let fraction =
                        (bg_progress.completed as f64 / bg_progress.total as f64).clamp(0.0, 0.99);
                    status_progress.set_fraction(fraction);
                    status_progress.set_text(Some(&format!("{:.0}%", fraction * 100.0)));
                } else {
                    status_progress.set_visible(false);
                }
            } else {
                status_label.set_visible(false);
                status_progress.set_visible(false);
                status_progress.set_fraction(0.0);
                status_progress.set_text(Some("Idle"));
            }
            glib::ControlFlow::Continue
        });
    }

    // ── MCP server (stdio + optional socket) ────────────────────────────
    {
        let mcp_receiver = mcp_receiver
            .borrow_mut()
            .take()
            .expect("MCP receiver already taken");

        // Stdio transport (--mcp flag)
        if mcp_enabled {
            let stdio_sender = (*mcp_sender).clone();
            std::thread::spawn(move || {
                crate::mcp::server::run_stdio_server(stdio_sender);
            });
            eprintln!("[MCP] Server listening on stdio (JSON-RPC 2.0 / MCP 2024-11-05)");
        }

        // Socket transport (Preferences toggle) — can start/stop at runtime.
        if preferences_state.borrow().mcp_socket_enabled {
            let stop = crate::mcp::start_mcp_socket_server((*mcp_sender).clone());
            *mcp_socket_stop.borrow_mut() = Some(stop);
        }

        let project = project.clone();
        let library = library.clone();
        let player = player.clone();
        let prog_player = prog_player.clone();
        let timeline_state = timeline_state.clone();
        let preferences_state = preferences_state.clone();
        let proxy_cache = proxy_cache.clone();
        let bg_removal_cache = bg_removal_cache.clone();
        let on_close_preview = on_close_preview.clone();
        let on_source_selected = on_source_selected.clone();
        let on_project_changed = on_project_changed.clone();
        let mcp_light_refresh_next = mcp_light_refresh_next.clone();
        let on_project_changed_mcp_debounced: Rc<dyn Fn()> = {
            let on_project_changed = on_project_changed.clone();
            let refresh_pending = Rc::new(Cell::new(false));
            Rc::new(move || {
                if refresh_pending.replace(true) {
                    return;
                }
                let refresh_pending = refresh_pending.clone();
                let on_project_changed = on_project_changed.clone();
                glib::timeout_add_local_once(std::time::Duration::from_millis(30), move || {
                    refresh_pending.set(false);
                    on_project_changed();
                });
            })
        };
        let on_project_changed_mcp_light: Rc<dyn Fn()> = {
            let on_project_changed_mcp_debounced = on_project_changed_mcp_debounced.clone();
            let mcp_light_refresh_next = mcp_light_refresh_next.clone();
            Rc::new(move || {
                mcp_light_refresh_next.set(true);
                on_project_changed_mcp_debounced();
            })
        };
        let on_project_changed_mcp_full: Rc<dyn Fn()> = {
            let on_project_changed_mcp_debounced = on_project_changed_mcp_debounced.clone();
            let mcp_light_refresh_next = mcp_light_refresh_next.clone();
            Rc::new(move || {
                mcp_light_refresh_next.set(false);
                on_project_changed_mcp_debounced();
            })
        };
        let suppress_resume_on_next_reload = suppress_resume_on_next_reload.clone();
        let clear_media_browser_on_next_reload = clear_media_browser_on_next_reload.clone();
        let window_weak = window.downgrade();
        MCP_MAIN_DISPATCH.with(|slot| {
            *slot.borrow_mut() = Some(Box::new(move |cmd| {
                if let Some(win) = window_weak.upgrade() {
                    handle_mcp_command(
                        cmd,
                        &win,
                        &project,
                        &library,
                        &player,
                        &prog_player,
                        &timeline_state,
                        &preferences_state,
                        &proxy_cache,
                        &bg_removal_cache,
                        &on_close_preview,
                        &on_source_selected,
                        &on_project_changed_mcp_light,
                        &on_project_changed_mcp_full,
                        &suppress_resume_on_next_reload,
                        &clear_media_browser_on_next_reload,
                    );
                }
            }));
        });

        let main_ctx = glib::MainContext::default();
        std::thread::spawn(move || {
            while let Ok(cmd) = mcp_receiver.recv() {
                main_ctx.invoke(move || {
                    MCP_MAIN_DISPATCH.with(|slot| {
                        if let Some(dispatch) = slot.borrow_mut().as_mut() {
                            dispatch(cmd);
                        }
                    });
                });
            }
        });
    }

    // Auto-save: every 60 seconds, write to a temp file if the project is dirty.
    {
        let project = project.clone();
        let window_weak = window.downgrade();
        glib::timeout_add_local(std::time::Duration::from_secs(60), move || {
            let is_dirty = project.borrow().dirty;
            if is_dirty {
                let xml_result = {
                    let proj = project.borrow();
                    crate::fcpxml::writer::write_fcpxml(&proj)
                };
                if let Ok(xml) = xml_result {
                    let path = "/tmp/ultimateslice-autosave.fcpxml";
                    if std::fs::write(path, xml).is_ok() {
                        if let Some(win) = window_weak.upgrade() {
                            let proj = project.borrow();
                            let title = format!("UltimateSlice — {} (Auto-saved)", proj.title);
                            win.set_title(Some(&title));
                            // Restore normal title after 3 seconds
                            let win_w2 = win.downgrade();
                            let proj_title = proj.title.clone();
                            glib::timeout_add_local_once(
                                std::time::Duration::from_secs(3),
                                move || {
                                    if let Some(w) = win_w2.upgrade() {
                                        w.set_title(Some(&format!(
                                            "UltimateSlice — {} •",
                                            proj_title
                                        )));
                                    }
                                },
                            );
                        }
                    }
                }
            }
            glib::ControlFlow::Continue
        });
    }

    // ── Window-level J/K/L: shuttle scrubbing in the program monitor ─────────
    // L — play forward, each press cycles speed: 1×→2×→4×→8×
    // K — pause / reset shuttle speed
    // J — play backward, each press cycles speed: −1×→−2×→−4×→−8×
    {
        use std::cell::Cell;
        let prog_player = prog_player.clone();
        let jkl_rate_cell: Rc<Cell<f64>> = Rc::new(Cell::new(0.0));
        let key_ctrl = gtk4::EventControllerKey::new();
        key_ctrl.set_propagation_phase(gtk4::PropagationPhase::Capture);
        key_ctrl.connect_key_pressed(move |ctrl, key, _, _mods| {
            use gtk4::gdk::Key;
            if key != Key::j
                && key != Key::J
                && key != Key::k
                && key != Key::K
                && key != Key::l
                && key != Key::L
            {
                return glib::Propagation::Proceed;
            }
            // Don't intercept when a text entry has focus.
            if let Some(widget) = ctrl.widget() {
                if let Some(focused) = widget.root().and_then(|r| r.focus()) {
                    if focused.is::<gtk4::Entry>() || focused.is::<gtk4::TextView>() {
                        return glib::Propagation::Proceed;
                    }
                }
            }
            let current = jkl_rate_cell.get();
            let new_rate = if key == Key::k || key == Key::K {
                0.0
            } else if key == Key::l || key == Key::L {
                // Cycle: stopped/reverse → 1×, then double up to 8×.
                match current as i64 {
                    r if r <= 0 => 1.0,
                    1 => 2.0,
                    2 => 4.0,
                    _ => 8.0,
                }
            } else {
                // J: cycle: stopped/forward → −1×, then double up to −8×.
                match current as i64 {
                    r if r >= 0 => -1.0,
                    -1 => -2.0,
                    -2 => -4.0,
                    _ => -8.0,
                }
            };
            jkl_rate_cell.set(new_rate);
            prog_player.borrow_mut().set_jkl_rate(new_rate);
            glib::Propagation::Stop
        });
        window.add_controller(key_ctrl);
    }
    // ── Window-level M key: add marker at current playhead (works regardless of focus) ──
    {
        let project = project.clone();
        let prog_player = prog_player.clone();
        let on_project_changed = on_project_changed.clone();
        let key_ctrl = gtk4::EventControllerKey::new();
        key_ctrl.set_propagation_phase(gtk4::PropagationPhase::Capture);
        key_ctrl.connect_key_pressed(move |ctrl, key, _, _mods| {
            use gtk4::gdk::Key;
            if key != Key::m && key != Key::M {
                return glib::Propagation::Proceed;
            }
            // Don't intercept M when a text entry or similar has focus
            if let Some(widget) = ctrl.widget() {
                if let Some(focused) = widget.root().and_then(|r| r.focus()) {
                    if focused.is::<gtk4::Entry>() || focused.is::<gtk4::TextView>() {
                        return glib::Propagation::Proceed;
                    }
                }
            }
            let pos = prog_player.borrow().timeline_pos_ns;
            project.borrow_mut().add_marker(pos, "Marker");
            on_project_changed();
            glib::Propagation::Stop
        });
        window.add_controller(key_ctrl);
    }
    // ── Window-level , and . keys: Insert / Overwrite at playhead ─────────
    {
        let on_insert = on_insert.clone();
        let on_overwrite = on_overwrite.clone();
        let key_ctrl = gtk4::EventControllerKey::new();
        key_ctrl.set_propagation_phase(gtk4::PropagationPhase::Capture);
        key_ctrl.connect_key_pressed(move |ctrl, key, _, mods| {
            use gtk4::gdk::{Key, ModifierType};
            // Skip if Ctrl is held (Ctrl+, = Preferences)
            if mods.contains(ModifierType::CONTROL_MASK) {
                return glib::Propagation::Proceed;
            }
            if key != Key::comma && key != Key::period {
                return glib::Propagation::Proceed;
            }
            // Don't intercept when a text entry has focus
            if let Some(widget) = ctrl.widget() {
                if let Some(focused) = widget.root().and_then(|r| r.focus()) {
                    if focused.is::<gtk4::Entry>() || focused.is::<gtk4::TextView>() {
                        return glib::Propagation::Proceed;
                    }
                }
            }
            if key == Key::comma {
                on_insert();
            } else {
                on_overwrite();
            }
            glib::Propagation::Stop
        });
        window.add_controller(key_ctrl);
    }
    // ── Window-level Ctrl+J: Go to timecode ────────────────────────────────
    {
        let on_go_to_timecode = on_go_to_timecode.clone();
        let key_ctrl = gtk4::EventControllerKey::new();
        key_ctrl.set_propagation_phase(gtk4::PropagationPhase::Capture);
        key_ctrl.connect_key_pressed(move |ctrl, key, _, mods| {
            use gtk4::gdk::{Key, ModifierType};
            if !mods.contains(ModifierType::CONTROL_MASK) || (key != Key::j && key != Key::J) {
                return glib::Propagation::Proceed;
            }
            if let Some(widget) = ctrl.widget() {
                if let Some(focused) = widget.root().and_then(|r| r.focus()) {
                    if focused.is::<gtk4::Entry>() || focused.is::<gtk4::TextView>() {
                        return glib::Propagation::Proceed;
                    }
                }
            }
            on_go_to_timecode();
            glib::Propagation::Stop
        });
        window.add_controller(key_ctrl);
    }
    // ── Window-level Ctrl+, key: open Preferences ───────────────────────────
    {
        let open_preferences = open_preferences.clone();
        let key_ctrl = gtk4::EventControllerKey::new();
        key_ctrl.set_propagation_phase(gtk4::PropagationPhase::Capture);
        key_ctrl.connect_key_pressed(move |_, key, _, mods| {
            use gtk4::gdk::{Key, ModifierType};
            if mods.contains(ModifierType::CONTROL_MASK) && key == Key::comma {
                open_preferences();
                return glib::Propagation::Stop;
            }
            glib::Propagation::Proceed
        });
        window.add_controller(key_ctrl);
    }
    // ── Window-level Alt+Left/Right: keyframe navigation ───────────────────
    {
        let project = project.clone();
        let timeline_state = timeline_state.clone();
        let inspector_view = inspector_view.clone();
        let prog_player = prog_player.clone();
        let timeline_panel_cell = timeline_panel_cell.clone();
        let key_ctrl = gtk4::EventControllerKey::new();
        key_ctrl.set_propagation_phase(gtk4::PropagationPhase::Capture);
        key_ctrl.connect_key_pressed(move |ctrl, key, _, mods| {
            use gtk4::gdk::{Key, ModifierType};
            if !mods.contains(ModifierType::ALT_MASK) {
                return glib::Propagation::Proceed;
            }
            if key != Key::Left && key != Key::Right {
                return glib::Propagation::Proceed;
            }
            if let Some(widget) = ctrl.widget() {
                if let Some(focused) = widget.root().and_then(|r| r.focus()) {
                    if focused.is::<gtk4::Entry>() || focused.is::<gtk4::TextView>() {
                        return glib::Propagation::Proceed;
                    }
                }
            }
            let (clip_id, playhead) = {
                let st = timeline_state.borrow();
                (st.selected_clip_id.clone(), st.playhead_ns)
            };
            let Some(clip_id) = clip_id else {
                return glib::Propagation::Proceed;
            };
            let proj = project.borrow();
            let target = proj
                .tracks
                .iter()
                .flat_map(|t| t.clips.iter())
                .find(|c| c.id == clip_id)
                .and_then(|clip| {
                    let local = clip.local_timeline_position_ns(playhead);
                    let local_target = if key == Key::Left {
                        clip.prev_keyframe_local_ns(local)
                    } else {
                        clip.next_keyframe_local_ns(local)
                    };
                    local_target.map(|lt| clip.timeline_start.saturating_add(lt))
                });
            drop(proj);
            if let Some(ns) = target {
                {
                    let mut st = timeline_state.borrow_mut();
                    st.playhead_ns = ns;
                }
                prog_player.borrow_mut().seek(ns);
                let proj = project.borrow();
                inspector_view.update_keyframe_indicator(&proj, ns);
                if let Some(ref w) = *timeline_panel_cell.borrow() {
                    w.queue_draw();
                }
            }
            glib::Propagation::Stop
        });
        window.add_controller(key_ctrl);
    }
    // ── Window-level Shift+K: toggle animation mode ────────────────────────
    {
        let inspector_view = inspector_view.clone();
        let key_ctrl = gtk4::EventControllerKey::new();
        key_ctrl.set_propagation_phase(gtk4::PropagationPhase::Capture);
        key_ctrl.connect_key_pressed(move |ctrl, key, _, mods| {
            use gtk4::gdk::{Key, ModifierType};
            if !mods.contains(ModifierType::SHIFT_MASK) {
                return glib::Propagation::Proceed;
            }
            if key != Key::K && key != Key::k {
                return glib::Propagation::Proceed;
            }
            if let Some(widget) = ctrl.widget() {
                if let Some(focused) = widget.root().and_then(|r| r.focus()) {
                    if focused.is::<gtk4::Entry>() || focused.is::<gtk4::TextView>() {
                        return glib::Propagation::Proceed;
                    }
                }
            }
            let new_state = !inspector_view.animation_mode.get();
            inspector_view.animation_mode.set(new_state);
            inspector_view.animation_mode_btn.set_active(new_state);
            glib::Propagation::Stop
        });
        window.add_controller(key_ctrl);
    }

    if monitor_state.borrow().popped {
        on_toggle_popout();
    }

    {
        let project = project.clone();
        let on_project_changed = on_project_changed.clone();
        let proxy_cache = proxy_cache.clone();
        let preferences_state = preferences_state.clone();
        let close_approved = Rc::new(Cell::new(false));
        let close_approved_for_signal = close_approved.clone();
        window.connect_close_request(move |w| {
            let proxy_mode_enabled = preferences_state.borrow().proxy_mode.is_enabled();
            if close_approved_for_signal.get() {
                proxy_cache
                    .borrow_mut()
                    .cleanup_for_unload(proxy_mode_enabled);
                return glib::Propagation::Proceed;
            }
            let close_approved_for_continue = close_approved.clone();
            let proxy_cache_for_continue = proxy_cache.clone();
            let preferences_state_for_continue = preferences_state.clone();
            let weak = w.downgrade();
            let on_continue: Rc<dyn Fn()> = Rc::new(move || {
                close_approved_for_continue.set(true);
                let proxy_mode_enabled = preferences_state_for_continue
                    .borrow()
                    .proxy_mode
                    .is_enabled();
                proxy_cache_for_continue
                    .borrow_mut()
                    .cleanup_for_unload(proxy_mode_enabled);
                if let Some(win) = weak.upgrade() {
                    win.close();
                }
            });
            crate::ui::toolbar::confirm_unsaved_then(
                Some(w.clone().upcast::<gtk::Window>()),
                project.clone(),
                on_project_changed.clone(),
                on_continue,
            );
            glib::Propagation::Stop
        });
    }

    if let Some(path) = startup_project_path {
        let (tx, rx) = std::sync::mpsc::sync_channel::<Result<Project, String>>(1);
        let path_bg = path.clone();
        std::thread::spawn(move || {
            let result = std::fs::read_to_string(&path_bg)
                .map_err(|e| format!("Failed to read startup project file: {e}"))
                .and_then(|xml| {
                    crate::fcpxml::parser::parse_fcpxml_with_path(
                        &xml,
                        Some(std::path::Path::new(&path_bg)),
                    )
                    .map_err(|e| format!("FCPXML parse error: {e}"))
                });
            let _ = tx.send(result);
        });
        timeline_state.borrow_mut().loading = true;
        let project = project.clone();
        let timeline_state = timeline_state.clone();
        let on_project_changed = on_project_changed.clone();
        let suppress_resume_on_next_reload = suppress_resume_on_next_reload.clone();
        let clear_media_browser_on_next_reload = clear_media_browser_on_next_reload.clone();
        glib::timeout_add_local(std::time::Duration::from_millis(10), move || {
            match rx.try_recv() {
                Ok(Ok(mut new_proj)) => {
                    new_proj.file_path = Some(path.clone());
                    recent::push(&path);
                    *project.borrow_mut() = new_proj;
                    timeline_state.borrow_mut().loading = false;
                    suppress_resume_on_next_reload.set(true);
                    clear_media_browser_on_next_reload.set(true);
                    on_project_changed();
                    glib::ControlFlow::Break
                }
                Ok(Err(e)) => {
                    timeline_state.borrow_mut().loading = false;
                    eprintln!("{e}");
                    glib::ControlFlow::Break
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => glib::ControlFlow::Continue,
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    timeline_state.borrow_mut().loading = false;
                    eprintln!("Startup project open worker disconnected");
                    glib::ControlFlow::Break
                }
            }
        });
    }

    window.present();
}

// ── MCP command handler (runs on GTK main thread) ────────────────────────────

fn handle_mcp_command(
    cmd: crate::mcp::McpCommand,
    window: &gtk::ApplicationWindow,
    project: &Rc<RefCell<Project>>,
    library: &Rc<RefCell<Vec<MediaItem>>>,
    player: &Rc<RefCell<Player>>,
    prog_player: &Rc<RefCell<ProgramPlayer>>,
    timeline_state: &Rc<RefCell<TimelineState>>,
    preferences_state: &Rc<RefCell<crate::ui_state::PreferencesState>>,
    proxy_cache: &Rc<RefCell<crate::media::proxy_cache::ProxyCache>>,
    bg_removal_cache: &Rc<RefCell<crate::media::bg_removal_cache::BgRemovalCache>>,
    on_close_preview: &Rc<dyn Fn()>,
    on_source_selected: &Rc<dyn Fn(String, u64)>,
    on_project_changed: &Rc<dyn Fn()>,
    on_project_changed_full: &Rc<dyn Fn()>,
    suppress_resume_on_next_reload: &Rc<Cell<bool>>,
    clear_media_browser_on_next_reload: &Rc<Cell<bool>>,
) {
    use crate::mcp::McpCommand;
    use serde_json::json;

    match cmd {
        McpCommand::GetProject { reply } => {
            let proj = project.borrow();
            let v = serde_json::to_value(&*proj).unwrap_or(json!(null));
            reply.send(v).ok();
        }

        McpCommand::ListTracks { compact, reply } => {
            let proj = project.borrow();
            let tracks: Vec<_> = proj
                .tracks
                .iter()
                .enumerate()
                .map(|(i, t)| {
                    if compact {
                        json!({
                            "index":      i,
                            "id":         t.id,
                            "kind":       format!("{:?}", t.kind),
                            "clip_count": t.clips.len(),
                        })
                    } else {
                        json!({
                            "index":      i,
                            "id":         t.id,
                            "label":      t.label,
                            "kind":       format!("{:?}", t.kind),
                            "clip_count": t.clips.len(),
                            "muted":      t.muted,
                            "locked":     t.locked,
                            "soloed":     t.soloed,
                            "height_preset": match t.height_preset {
                                crate::model::track::TrackHeightPreset::Small => "small",
                                crate::model::track::TrackHeightPreset::Medium => "medium",
                                crate::model::track::TrackHeightPreset::Large => "large",
                            },
                        })
                    }
                })
                .collect();
            reply.send(json!(tracks)).ok();
        }

        McpCommand::ListClips { compact, reply } => {
            let proj = project.borrow();
            let clips: Vec<_> = proj
                .tracks
                .iter()
                .enumerate()
                .flat_map(|(ti, track)| {
                    track.clips.iter().map(move |c| {
                        if compact {
                            json!({
                                "id":               c.id,
                                "source_path":      c.source_path,
                                "track_index":      ti,
                                "track_id":         track.id,
                                "timeline_start_ns": c.timeline_start,
                                "source_in_ns":     c.source_in,
                                "source_out_ns":    c.source_out,
                                "duration_ns":      c.duration(),
                            })
                        } else {
                            json!({
                                "id":               c.id,
                                "label":            c.label,
                                "source_path":      c.source_path,
                                "track_index":      ti,
                                "track_id":         track.id,
                                "group_id":         c.group_id,
                                "link_group_id":    c.link_group_id,
                                "source_timecode_base_ns": c.source_timecode_base_ns,
                                "source_timecode_start_ns": c.source_timecode_start_ns(),
                                "timeline_start_ns": c.timeline_start,
                                "source_in_ns":     c.source_in,
                                "source_out_ns":    c.source_out,
                                "duration_ns":      c.duration(),
                                "color_label":      match c.color_label {
                                    crate::model::clip::ClipColorLabel::None => "none",
                                    crate::model::clip::ClipColorLabel::Red => "red",
                                    crate::model::clip::ClipColorLabel::Orange => "orange",
                                    crate::model::clip::ClipColorLabel::Yellow => "yellow",
                                    crate::model::clip::ClipColorLabel::Green => "green",
                                    crate::model::clip::ClipColorLabel::Teal => "teal",
                                    crate::model::clip::ClipColorLabel::Blue => "blue",
                                    crate::model::clip::ClipColorLabel::Purple => "purple",
                                    crate::model::clip::ClipColorLabel::Magenta => "magenta",
                                },
                                "brightness":       c.brightness,
                                "contrast":         c.contrast,
                                "saturation":       c.saturation,
                                "temperature":      c.temperature,
                                "tint":             c.tint,
                                "denoise":          c.denoise,
                                "sharpness":        c.sharpness,
                                "shadows":          c.shadows,
                                "midtones":         c.midtones,
                                "highlights":       c.highlights,
                                "exposure":         c.exposure,
                                "black_point":      c.black_point,
                                "highlights_warmth": c.highlights_warmth,
                                "highlights_tint":  c.highlights_tint,
                                "midtones_warmth":  c.midtones_warmth,
                                "midtones_tint":    c.midtones_tint,
                                "shadows_warmth":   c.shadows_warmth,
                                "shadows_tint":     c.shadows_tint,
                                "volume":           c.volume,
                                "pan":              c.pan,
                                "scale":            c.scale,
                                "opacity":          c.opacity,
                                "position_x":       c.position_x,
                                "position_y":       c.position_y,
                                "speed":            c.speed,
                                "scale_keyframes":      c.scale_keyframes,
                                "opacity_keyframes":    c.opacity_keyframes,
                                "brightness_keyframes": c.brightness_keyframes,
                                "contrast_keyframes":   c.contrast_keyframes,
                                "saturation_keyframes": c.saturation_keyframes,
                                "temperature_keyframes": c.temperature_keyframes,
                                "tint_keyframes":       c.tint_keyframes,
                                "position_x_keyframes": c.position_x_keyframes,
                                "position_y_keyframes": c.position_y_keyframes,
                                "volume_keyframes":     c.volume_keyframes,
                                "pan_keyframes":        c.pan_keyframes,
                                "speed_keyframes":      c.speed_keyframes,
                                "rotate_keyframes":     c.rotate_keyframes,
                                "crop_left_keyframes":  c.crop_left_keyframes,
                                "crop_right_keyframes": c.crop_right_keyframes,
                                "crop_top_keyframes":   c.crop_top_keyframes,
                                "crop_bottom_keyframes": c.crop_bottom_keyframes,
                                "bg_removal_enabled":   c.bg_removal_enabled,
                                "bg_removal_threshold": c.bg_removal_threshold,
                            })
                        }
                    })
                })
                .collect();
            reply.send(json!(clips)).ok();
        }

        McpCommand::GetTimelineSettings { reply } => {
            let magnetic_mode = timeline_state.borrow().magnetic_mode;
            reply
                .send(json!({
                    "magnetic_mode": magnetic_mode
                }))
                .ok();
        }

        McpCommand::GetPlayheadPosition { reply } => {
            let timeline_pos_ns = prog_player.borrow().timeline_pos_ns;
            reply
                .send(json!({
                    "timeline_pos_ns": timeline_pos_ns
                }))
                .ok();
        }

        McpCommand::GetPerformanceSnapshot { reply } => {
            let snapshot = prog_player.borrow().performance_snapshot();
            let transition_metrics: Vec<_> = snapshot
                .transition_metrics
                .iter()
                .map(|metric| {
                    json!({
                        "kind": metric.kind.clone(),
                        "hit": metric.hit,
                        "miss": metric.miss,
                        "hit_rate_percent": metric.hit_rate_percent
                    })
                })
                .collect();
            reply
                .send(json!({
                    "player_state": snapshot.player_state,
                    "playback_priority": snapshot.playback_priority,
                    "timeline_pos_ns": snapshot.timeline_pos_ns,
                    "background_prerender_enabled": snapshot.background_prerender_enabled,
                    "prerender_total_requested": snapshot.prerender_total_requested,
                    "prerender_pending": snapshot.prerender_pending,
                    "prerender_ready": snapshot.prerender_ready,
                    "prerender_failed": snapshot.prerender_failed,
                    "prerender_cache_hits": snapshot.prerender_cache_hits,
                    "prerender_cache_misses": snapshot.prerender_cache_misses,
                    "prerender_cache_hit_rate_percent": snapshot.prerender_cache_hit_rate_percent,
                    "prewarmed_boundary_ns": snapshot.prewarmed_boundary_ns,
                    "active_prerender_segment_key": snapshot.active_prerender_segment_key,
                    "rebuild_history_samples": snapshot.rebuild_history_samples,
                    "rebuild_history_recent_ms": snapshot.rebuild_history_recent_ms,
                    "rebuild_latest_ms": snapshot.rebuild_latest_ms,
                    "rebuild_p50_ms": snapshot.rebuild_p50_ms,
                    "rebuild_p75_ms": snapshot.rebuild_p75_ms,
                    "transition_hits_total": snapshot.transition_hits_total,
                    "transition_misses_total": snapshot.transition_misses_total,
                    "transition_hit_rate_percent": snapshot.transition_hit_rate_percent,
                    "transition_low_hitrate_pressure": snapshot.transition_low_hitrate_pressure,
                    "prerender_queue_pressure": snapshot.prerender_queue_pressure,
                    "transition_metrics": transition_metrics
                }))
                .ok();
        }

        McpCommand::SetMagneticMode { enabled, reply } => {
            timeline_state.borrow_mut().magnetic_mode = enabled;
            reply
                .send(json!({"success": true, "magnetic_mode": enabled}))
                .ok();
            on_project_changed();
        }

        McpCommand::SetTrackSolo {
            track_id,
            solo,
            reply,
        } => {
            let result = {
                let mut proj = project.borrow_mut();
                if let Some(track) = proj.track_mut(&track_id) {
                    track.soloed = solo;
                    proj.dirty = true;
                    Ok(())
                } else {
                    Err(format!("Track id not found: {track_id}"))
                }
            };
            match result {
                Ok(()) => {
                    reply
                        .send(json!({"success": true, "track_id": track_id, "soloed": solo}))
                        .ok();
                    on_project_changed();
                }
                Err(message) => {
                    reply.send(json!({"success": false, "error": message})).ok();
                }
            }
        }

        McpCommand::SetTrackHeightPreset {
            track_id,
            height_preset,
            reply,
        } => {
            let parsed = match height_preset.as_str() {
                "small" => Some(crate::model::track::TrackHeightPreset::Small),
                "medium" => Some(crate::model::track::TrackHeightPreset::Medium),
                "large" => Some(crate::model::track::TrackHeightPreset::Large),
                _ => None,
            };
            let Some(parsed) = parsed else {
                reply
                    .send(json!({"success": false, "error": "height_preset must be one of: small, medium, large"}))
                    .ok();
                return;
            };
            let result = {
                let mut proj = project.borrow_mut();
                if let Some(track) = proj.track_mut(&track_id) {
                    track.height_preset = parsed;
                    proj.dirty = true;
                    Ok(())
                } else {
                    Err(format!("Track id not found: {track_id}"))
                }
            };
            match result {
                Ok(()) => {
                    reply
                        .send(json!({"success": true, "track_id": track_id, "height_preset": height_preset}))
                        .ok();
                    on_project_changed();
                }
                Err(message) => {
                    reply.send(json!({"success": false, "error": message})).ok();
                }
            }
        }

        McpCommand::CloseSourcePreview { reply } => {
            on_close_preview();
            reply.send(json!({"success": true})).ok();
        }

        McpCommand::GetPreferences { reply } => {
            let prefs = preferences_state.borrow().clone();
            reply
                .send(json!({
                    "hardware_acceleration_enabled": prefs.hardware_acceleration_enabled,
                    "playback_priority": prefs.playback_priority.as_str(),
                    "source_playback_priority": prefs.source_playback_priority.as_str(),
                    "proxy_mode": prefs.proxy_mode.as_str(),
                    "show_timeline_preview": prefs.show_timeline_preview,
                    "show_track_audio_levels": prefs.show_track_audio_levels,
                    "gsk_renderer": prefs.gsk_renderer.as_str(),
                    "preview_quality": prefs.preview_quality.as_str(),
                    "experimental_preview_optimizations": prefs.experimental_preview_optimizations,
                    "realtime_preview": prefs.realtime_preview,
                    "background_prerender": prefs.background_prerender,
                    "preview_luts": prefs.preview_luts,
                    "crossfade_enabled": prefs.crossfade_enabled,
                    "crossfade_curve": prefs.crossfade_curve.as_str(),
                    "crossfade_duration_ns": prefs.crossfade_duration_ns
                }))
                .ok();
        }

        McpCommand::SetHardwareAcceleration { enabled, reply } => {
            let new_state = {
                let mut prefs = preferences_state.borrow_mut();
                prefs.hardware_acceleration_enabled = enabled;
                prefs.clone()
            };
            crate::ui_state::save_preferences_state(&new_state);
            match player.borrow().set_hardware_acceleration(enabled) {
                Ok(()) => {
                    reply
                        .send(json!({
                            "success": true,
                            "hardware_acceleration_enabled": enabled,
                            "playback_priority": new_state.playback_priority.as_str(),
                            "source_playback_priority": new_state.source_playback_priority.as_str()
                        }))
                        .ok();
                }
                Err(e) => {
                    reply
                        .send(json!({
                            "success": false,
                            "hardware_acceleration_enabled": enabled,
                            "playback_priority": new_state.playback_priority.as_str(),
                            "source_playback_priority": new_state.source_playback_priority.as_str(),
                            "error": e.to_string()
                        }))
                        .ok();
                }
            }
        }

        McpCommand::SetPlaybackPriority { priority, reply } => {
            let parsed = crate::ui_state::PlaybackPriority::from_str(&priority);
            let new_state = {
                let mut prefs = preferences_state.borrow_mut();
                prefs.playback_priority = parsed.clone();
                prefs.clone()
            };
            crate::ui_state::save_preferences_state(&new_state);
            prog_player.borrow_mut().set_playback_priority(parsed);
            reply
                .send(json!({
                    "success": true,
                    "playback_priority": new_state.playback_priority.as_str()
                }))
                .ok();
        }

        McpCommand::SetSourcePlaybackPriority { priority, reply } => {
            let parsed = crate::ui_state::PlaybackPriority::from_str(&priority);
            let new_state = {
                let mut prefs = preferences_state.borrow_mut();
                prefs.source_playback_priority = parsed.clone();
                prefs.clone()
            };
            crate::ui_state::save_preferences_state(&new_state);
            player.borrow().set_source_playback_priority(parsed.clone());
            reply
                .send(json!({
                    "success": true,
                    "source_playback_priority": new_state.source_playback_priority.as_str()
                }))
                .ok();
        }

        McpCommand::SetCrossfadeSettings {
            enabled,
            curve,
            duration_ns,
            reply,
        } => {
            const MIN_CROSSFADE_DURATION_NS: u64 = 10_000_000;
            const MAX_CROSSFADE_DURATION_NS: u64 = 10_000_000_000;
            let parsed_curve = match curve.as_str() {
                "equal_power" => crate::ui_state::CrossfadeCurve::EqualPower,
                "linear" => crate::ui_state::CrossfadeCurve::Linear,
                _ => {
                    reply
                        .send(json!({"success": false, "error": "curve must be one of: equal_power, linear"}))
                        .ok();
                    return;
                }
            };
            if !(MIN_CROSSFADE_DURATION_NS..=MAX_CROSSFADE_DURATION_NS).contains(&duration_ns) {
                reply
                    .send(json!({
                        "success": false,
                        "error": "duration_ns must be between 10_000_000 and 10_000_000_000"
                    }))
                    .ok();
                return;
            }
            let new_state = {
                let mut prefs = preferences_state.borrow_mut();
                prefs.crossfade_enabled = enabled;
                prefs.crossfade_curve = parsed_curve;
                prefs.crossfade_duration_ns = duration_ns;
                prefs.clone()
            };
            crate::ui_state::save_preferences_state(&new_state);
            prog_player.borrow_mut().set_audio_crossfade_preview(
                new_state.crossfade_enabled,
                new_state.crossfade_curve.clone(),
                new_state.crossfade_duration_ns,
            );
            reply
                .send(json!({
                    "success": true,
                    "crossfade_enabled": new_state.crossfade_enabled,
                    "crossfade_curve": new_state.crossfade_curve.as_str(),
                    "crossfade_duration_ns": new_state.crossfade_duration_ns
                }))
                .ok();
        }

        McpCommand::SetProxyMode { mode, reply } => {
            let parsed = crate::ui_state::ProxyMode::from_str(&mode);
            let enabled = parsed.is_enabled();
            let new_state = {
                let mut prefs = preferences_state.borrow_mut();
                prefs.proxy_mode = parsed;
                prefs.clone()
            };
            crate::ui_state::save_preferences_state(&new_state);
            proxy_cache.borrow_mut().set_sidecar_mirror_enabled(enabled);
            prog_player.borrow_mut().set_proxy_enabled(enabled);
            prog_player
                .borrow_mut()
                .set_preview_luts(new_state.preview_luts);
            prog_player
                .borrow_mut()
                .set_proxy_scale_divisor(match new_state.proxy_mode {
                    crate::ui_state::ProxyMode::QuarterRes => 4,
                    _ => 2,
                });
            if enabled {
                let scale = match new_state.proxy_mode {
                    crate::ui_state::ProxyMode::QuarterRes => {
                        crate::media::proxy_cache::ProxyScale::Quarter
                    }
                    _ => crate::media::proxy_cache::ProxyScale::Half,
                };
                let clip_sources: Vec<(String, Option<String>)> = {
                    let proj = project.borrow();
                    let mut seen: HashSet<(String, Option<String>)> = HashSet::new();
                    proj.tracks
                        .iter()
                        .flat_map(|t| t.clips.iter())
                        .filter_map(|c| {
                            let key = (c.source_path.clone(), c.lut_path.clone());
                            if seen.insert(key.clone()) {
                                Some(key)
                            } else {
                                None
                            }
                        })
                        .collect()
                };
                {
                    let mut cache = proxy_cache.borrow_mut();
                    cache.invalidate_all();
                    for (path, lut) in &clip_sources {
                        cache.request(path, scale, lut.as_deref());
                    }
                }
                let paths = proxy_cache.borrow().proxies.clone();
                prog_player.borrow_mut().update_proxy_paths(paths);
            } else if new_state.preview_luts {
                let (project_w, project_h, lut_sources): (u32, u32, Vec<(String, Option<String>)>) = {
                    let proj = project.borrow();
                    (
                        proj.width,
                        proj.height,
                        collect_unique_lut_clip_sources(&proj),
                    )
                };
                {
                    let mut cache = proxy_cache.borrow_mut();
                    cache.invalidate_all();
                    for (path, lut) in &lut_sources {
                        cache.request(
                            path,
                            crate::media::proxy_cache::ProxyScale::Project {
                                width: project_w,
                                height: project_h,
                            },
                            lut.as_deref(),
                        );
                    }
                }
                let paths = proxy_cache.borrow().proxies.clone();
                prog_player.borrow_mut().update_proxy_paths(paths);
            }
            reply
                .send(json!({
                    "success": true,
                    "proxy_mode": new_state.proxy_mode.as_str()
                }))
                .ok();
        }

        McpCommand::SetGskRenderer { renderer, reply } => {
            let parsed = crate::ui_state::GskRenderer::from_str(&renderer);
            let new_state = {
                let mut prefs = preferences_state.borrow_mut();
                prefs.gsk_renderer = parsed;
                prefs.clone()
            };
            crate::ui_state::save_preferences_state(&new_state);
            reply
                .send(json!({
                    "success": true,
                    "gsk_renderer": new_state.gsk_renderer.as_str(),
                    "note": "Restart the application for the new renderer to take effect."
                }))
                .ok();
        }

        McpCommand::SetPreviewQuality { quality, reply } => {
            let parsed = crate::ui_state::PreviewQuality::from_str(&quality);
            prog_player
                .borrow_mut()
                .set_preview_quality(parsed.divisor());
            let new_state = {
                let mut prefs = preferences_state.borrow_mut();
                prefs.preview_quality = parsed;
                prefs.clone()
            };
            crate::ui_state::save_preferences_state(&new_state);
            reply
                .send(json!({
                    "success": true,
                    "preview_quality": new_state.preview_quality.as_str()
                }))
                .ok();
        }

        McpCommand::SetRealtimePreview { enabled, reply } => {
            prog_player.borrow_mut().set_realtime_preview(enabled);
            let new_state = {
                let mut prefs = preferences_state.borrow_mut();
                prefs.realtime_preview = enabled;
                prefs.clone()
            };
            crate::ui_state::save_preferences_state(&new_state);
            reply
                .send(json!({
                    "success": true,
                    "realtime_preview": enabled
                }))
                .ok();
        }

        McpCommand::SetExperimentalPreviewOptimizations { enabled, reply } => {
            prog_player
                .borrow_mut()
                .set_experimental_preview_optimizations(enabled);
            let new_state = {
                let mut prefs = preferences_state.borrow_mut();
                prefs.experimental_preview_optimizations = enabled;
                prefs.clone()
            };
            crate::ui_state::save_preferences_state(&new_state);
            reply
                .send(json!({
                    "success": true,
                    "experimental_preview_optimizations": enabled
                }))
                .ok();
        }

        McpCommand::SetBackgroundPrerender { enabled, reply } => {
            prog_player.borrow_mut().set_background_prerender(enabled);
            let new_state = {
                let mut prefs = preferences_state.borrow_mut();
                prefs.background_prerender = enabled;
                prefs.clone()
            };
            crate::ui_state::save_preferences_state(&new_state);
            reply
                .send(json!({
                    "success": true,
                    "background_prerender": enabled
                }))
                .ok();
        }

        McpCommand::SetPreviewLuts { enabled, reply } => {
            prog_player.borrow_mut().set_preview_luts(enabled);
            let new_state = {
                let mut prefs = preferences_state.borrow_mut();
                prefs.preview_luts = enabled;
                prefs.clone()
            };
            crate::ui_state::save_preferences_state(&new_state);
            if !new_state.proxy_mode.is_enabled() && enabled {
                let (project_w, project_h, lut_sources): (u32, u32, Vec<(String, Option<String>)>) = {
                    let proj = project.borrow();
                    (
                        proj.width,
                        proj.height,
                        collect_unique_lut_clip_sources(&proj),
                    )
                };
                {
                    let mut cache = proxy_cache.borrow_mut();
                    cache.invalidate_all();
                    for (path, lut) in &lut_sources {
                        cache.request(
                            path,
                            crate::media::proxy_cache::ProxyScale::Project {
                                width: project_w,
                                height: project_h,
                            },
                            lut.as_deref(),
                        );
                    }
                }
                let paths = proxy_cache.borrow().proxies.clone();
                prog_player.borrow_mut().update_proxy_paths(paths);
            } else if !enabled && !new_state.proxy_mode.is_enabled() {
                prog_player.borrow_mut().update_proxy_paths(HashMap::new());
            }
            reply
                .send(json!({
                    "success": true,
                    "preview_luts": enabled
                }))
                .ok();
        }

        McpCommand::AddClip {
            source_path,
            track_index,
            timeline_start_ns,
            source_in_ns,
            source_out_ns,
            reply,
        } => {
            let magnetic_mode = timeline_state.borrow().magnetic_mode;
            let source_monitor_auto_link_av =
                preferences_state.borrow().source_monitor_auto_link_av;
            let source_info = {
                let lib = library.borrow();
                let proj = project.borrow();
                lookup_source_placement_info(&lib, &proj, &source_path)
            };
            let created = {
                let mut proj = project.borrow_mut();
                let placement_plan = build_source_placement_plan_by_track_index(
                    &proj,
                    Some(track_index),
                    source_info,
                    source_monitor_auto_link_av,
                );
                if placement_plan.targets.is_empty() {
                    Err(format!("Track index {track_index} does not exist"))
                } else {
                    let magnetic_mode_for_placement =
                        magnetic_mode && !placement_plan.uses_linked_pair();
                    let mut created_clip_ids: Vec<String> = Vec::new();
                    for (target_track_idx, clip) in build_source_clips_for_plan(
                        &placement_plan,
                        &source_path,
                        source_in_ns,
                        source_out_ns,
                        timeline_start_ns,
                        source_info.source_timecode_base_ns,
                    ) {
                        created_clip_ids.push(clip.id.clone());
                        let _ = add_clip_to_track(
                            &mut proj.tracks[target_track_idx],
                            clip,
                            magnetic_mode_for_placement,
                        );
                    }
                    proj.dirty = true;
                    Ok((
                        created_clip_ids.first().cloned().unwrap_or_default(),
                        created_clip_ids.into_iter().skip(1).collect::<Vec<_>>(),
                        placement_plan.link_group_id.clone(),
                    ))
                }
            };
            match created {
                Ok((clip_id, linked_clip_ids, link_group_id)) => {
                    reply
                        .send(json!({
                            "success": true,
                            "clip_id": clip_id,
                            "linked_clip_ids": linked_clip_ids,
                            "link_group_id": link_group_id
                        }))
                        .ok();
                    on_project_changed_full();
                }
                Err(e) => {
                    reply.send(json!({"success": false, "error": e})).ok();
                }
            }
        }

        McpCommand::RemoveClip { clip_id, reply } => {
            let magnetic_mode = timeline_state.borrow().magnetic_mode;
            let mut proj = project.borrow_mut();
            let target_ids: HashSet<String> = proj
                .tracks
                .iter()
                .flat_map(|t| t.clips.iter())
                .find(|c| c.id == clip_id)
                .map(|clip| {
                    if let Some(link_group_id) = clip.link_group_id.clone() {
                        proj.tracks
                            .iter()
                            .flat_map(|t| t.clips.iter())
                            .filter(|c| c.link_group_id.as_deref() == Some(link_group_id.as_str()))
                            .map(|c| c.id.clone())
                            .collect()
                    } else {
                        std::iter::once(clip_id.clone()).collect()
                    }
                })
                .unwrap_or_default();
            let mut removed_count = 0usize;
            for track in proj.tracks.iter_mut() {
                let before = track.clips.len();
                track.clips.retain(|c| !target_ids.contains(&c.id));
                if before != track.clips.len() {
                    removed_count += before - track.clips.len();
                    if magnetic_mode {
                        track.compact_gap_free();
                    }
                }
            }
            let found = removed_count > 0;
            if found {
                proj.dirty = true;
            }
            drop(proj);
            reply
                .send(json!({"success": found, "removed_clip_count": removed_count}))
                .ok();
            if found {
                on_project_changed();
            }
        }

        McpCommand::MoveClip {
            clip_id,
            new_start_ns,
            reply,
        } => {
            let magnetic_mode = timeline_state.borrow().magnetic_mode;
            let mut proj = project.borrow_mut();
            let target = proj
                .tracks
                .iter()
                .flat_map(|t| t.clips.iter())
                .find(|c| c.id == clip_id)
                .map(|clip| (clip.timeline_start, clip.link_group_id.clone()));
            let mut moved_count = 0usize;
            if let Some((original_start_ns, link_group_id)) = target {
                if let Some(link_group_id) = link_group_id {
                    let delta = i128::from(new_start_ns) - i128::from(original_start_ns);
                    for track in proj.tracks.iter_mut() {
                        let mut changed = false;
                        for clip in &mut track.clips {
                            if clip.link_group_id.as_deref() == Some(link_group_id.as_str()) {
                                clip.timeline_start =
                                    (i128::from(clip.timeline_start) + delta).max(0) as u64;
                                moved_count += 1;
                                changed = true;
                            }
                        }
                        if changed {
                            track.sort_clips();
                            if magnetic_mode {
                                track.compact_gap_free();
                            }
                        }
                    }
                } else {
                    for track in proj.tracks.iter_mut() {
                        if let Some(idx) = track.clips.iter().position(|c| c.id == clip_id) {
                            track.clips[idx].timeline_start = new_start_ns;
                            if magnetic_mode {
                                track.compact_gap_free();
                            } else {
                                track.sort_clips();
                            }
                            moved_count = 1;
                            break;
                        }
                    }
                }
            }
            let found = moved_count > 0;
            if found {
                proj.dirty = true;
            }
            drop(proj);
            reply
                .send(json!({"success": found, "moved_clip_count": moved_count}))
                .ok();
            if found {
                on_project_changed();
            }
        }

        McpCommand::LinkClips { clip_ids, reply } => {
            if clip_ids.len() < 2 {
                reply
                    .send(json!({"success": false, "error": "clip_ids must contain at least two clip ids"}))
                    .ok();
                return;
            }
            let clip_id_set: HashSet<String> = clip_ids.into_iter().collect();
            let link_group_id = uuid::Uuid::new_v4().to_string();
            let mut proj = project.borrow_mut();
            let mut linked_count = 0usize;
            for track in proj.tracks.iter_mut() {
                for clip in &mut track.clips {
                    if clip_id_set.contains(&clip.id) {
                        clip.link_group_id = Some(link_group_id.clone());
                        linked_count += 1;
                    }
                }
            }
            let success = linked_count >= 2;
            if success {
                proj.dirty = true;
            }
            drop(proj);
            reply
                .send(json!({
                    "success": success,
                    "link_group_id": if success { Some(link_group_id) } else { None::<String> },
                    "linked_clip_count": linked_count
                }))
                .ok();
            if success {
                on_project_changed();
            }
        }

        McpCommand::UnlinkClips { clip_ids, reply } => {
            let clip_id_set: HashSet<String> = clip_ids.into_iter().collect();
            if clip_id_set.is_empty() {
                reply
                    .send(json!({"success": false, "error": "clip_ids must contain at least one clip id"}))
                    .ok();
                return;
            }
            let mut proj = project.borrow_mut();
            let target_link_groups: HashSet<String> = proj
                .tracks
                .iter()
                .flat_map(|t| t.clips.iter())
                .filter(|c| clip_id_set.contains(&c.id))
                .filter_map(|c| c.link_group_id.clone())
                .collect();
            if target_link_groups.is_empty() {
                drop(proj);
                reply
                    .send(json!({"success": false, "error": "No linked clips found for the provided clip_ids"}))
                    .ok();
                return;
            }
            let mut unlinked_count = 0usize;
            for track in proj.tracks.iter_mut() {
                for clip in &mut track.clips {
                    if clip
                        .link_group_id
                        .as_deref()
                        .is_some_and(|gid| target_link_groups.contains(gid))
                    {
                        clip.link_group_id = None;
                        unlinked_count += 1;
                    }
                }
            }
            let success = unlinked_count > 0;
            if success {
                proj.dirty = true;
            }
            drop(proj);
            reply
                .send(json!({"success": success, "unlinked_clip_count": unlinked_count}))
                .ok();
            if success {
                on_project_changed();
            }
        }

        McpCommand::AlignGroupedClipsByTimecode { clip_ids, reply } => {
            let result = {
                let mut proj = project.borrow_mut();
                let result = align_grouped_clips_by_timecode_in_project(&mut proj, &clip_ids);
                if result.is_ok() {
                    proj.dirty = true;
                }
                result
            };
            match result {
                Ok((aligned_group_count, aligned_clip_count)) => {
                    reply
                        .send(json!({
                            "success": true,
                            "aligned_group_count": aligned_group_count,
                            "aligned_clip_count": aligned_clip_count
                        }))
                        .ok();
                    on_project_changed();
                }
                Err(error) => {
                    reply.send(json!({"success": false, "error": error})).ok();
                }
            }
        }

        McpCommand::SyncClipsByAudio { clip_ids, reply } => {
            if clip_ids.len() < 2 {
                reply
                    .send(json!({"success": false, "error": "Need at least 2 clip ids"}))
                    .ok();
            } else {
                // Collect clip info from project
                let clips: Vec<(String, String, u64, u64)> = {
                    let proj = project.borrow();
                    clip_ids
                        .iter()
                        .filter_map(|id| {
                            proj.tracks
                                .iter()
                                .flat_map(|t| t.clips.iter())
                                .find(|c| &c.id == id)
                                .map(|c| {
                                    (
                                        c.id.clone(),
                                        c.source_path.clone(),
                                        c.source_in,
                                        c.source_out,
                                    )
                                })
                        })
                        .collect()
                };
                if clips.len() < 2 {
                    reply
                        .send(json!({"success": false, "error": "Could not find 2+ clips with the provided ids"}))
                        .ok();
                } else {
                    let anchor_timeline_start = {
                        let proj = project.borrow();
                        proj.tracks
                            .iter()
                            .flat_map(|t| t.clips.iter())
                            .find(|c| c.id == clips[0].0)
                            .map(|c| c.timeline_start)
                            .unwrap_or(0)
                    };
                    let sync_results = crate::media::audio_sync::sync_clips_by_audio(&clips);
                    let mut result_json = Vec::new();
                    let mut assignments: HashMap<String, u64> = HashMap::new();
                    let mut all_confident = true;
                    for r in &sync_results {
                        let new_start = (anchor_timeline_start as i64 + r.offset_ns).max(0) as u64;
                        result_json.push(json!({
                            "clip_id": r.clip_id,
                            "offset_ns": r.offset_ns,
                            "confidence": r.confidence,
                            "new_timeline_start_ns": new_start,
                        }));
                        if r.confidence < 3.0 {
                            all_confident = false;
                        } else {
                            assignments.insert(r.clip_id.clone(), new_start);
                        }
                    }
                    if all_confident && !assignments.is_empty() {
                        let mut proj = project.borrow_mut();
                        for track in &mut proj.tracks {
                            for clip in &mut track.clips {
                                if let Some(&new_start) = assignments.get(&clip.id) {
                                    clip.timeline_start = new_start;
                                }
                            }
                        }
                        proj.dirty = true;
                        drop(proj);
                        on_project_changed();
                    }
                    reply
                        .send(json!({
                            "success": all_confident,
                            "results": result_json,
                        }))
                        .ok();
                }
            }
        }

        McpCommand::TrimClip {
            clip_id,
            source_in_ns,
            source_out_ns,
            reply,
        } => {
            let magnetic_mode = timeline_state.borrow().magnetic_mode;
            let mut proj = project.borrow_mut();
            let mut found = false;
            for track in proj.tracks.iter_mut() {
                if let Some(idx) = track.clips.iter().position(|c| c.id == clip_id) {
                    track.clips[idx].source_in = source_in_ns;
                    track.clips[idx].source_out = source_out_ns;
                    if magnetic_mode {
                        track.compact_gap_free();
                    }
                    proj.dirty = true;
                    found = true;
                    break;
                }
            }
            drop(proj);
            reply.send(json!({"success": found})).ok();
            if found {
                on_project_changed();
            }
        }

        McpCommand::SlipClip {
            clip_id,
            delta_ns,
            reply,
        } => {
            let mut proj = project.borrow_mut();
            let mut found = false;
            for track in proj.tracks.iter_mut() {
                if let Some(clip) = track.clips.iter_mut().find(|c| c.id == clip_id) {
                    let new_in = (clip.source_in as i64 + delta_ns).max(0) as u64;
                    let new_out =
                        (clip.source_out as i64 + delta_ns).max(new_in as i64 + 1_000_000) as u64;
                    clip.source_in = new_in;
                    clip.source_out = new_out;
                    proj.dirty = true;
                    found = true;
                    break;
                }
            }
            drop(proj);
            reply.send(json!({"success": found})).ok();
            if found {
                on_project_changed();
            }
        }

        McpCommand::SlideClip {
            clip_id,
            delta_ns,
            reply,
        } => {
            let mut proj = project.borrow_mut();
            let mut found = false;
            for track in proj.tracks.iter_mut() {
                let clip_idx = track.clips.iter().position(|c| c.id == clip_id);
                if let Some(idx) = clip_idx {
                    let mut sorted_indices: Vec<usize> = (0..track.clips.len()).collect();
                    sorted_indices.sort_by_key(|&i| track.clips[i].timeline_start);
                    let sorted_pos = sorted_indices.iter().position(|&i| i == idx);
                    let left_idx = sorted_pos.and_then(|p| {
                        if p > 0 {
                            Some(sorted_indices[p - 1])
                        } else {
                            None
                        }
                    });
                    let right_idx = sorted_pos.and_then(|p| sorted_indices.get(p + 1).copied());
                    // Validate neighbors
                    let left_ok = left_idx
                        .map(|li| {
                            let new_out =
                                (track.clips[li].source_out as i64 + delta_ns).max(0) as u64;
                            new_out > track.clips[li].source_in + 1_000_000
                        })
                        .unwrap_or(true);
                    let right_ok = right_idx
                        .map(|ri| {
                            let new_in =
                                (track.clips[ri].source_in as i64 + delta_ns).max(0) as u64;
                            new_in + 1_000_000 < track.clips[ri].source_out
                        })
                        .unwrap_or(true);
                    if left_ok && right_ok {
                        track.clips[idx].timeline_start =
                            (track.clips[idx].timeline_start as i64 + delta_ns).max(0) as u64;
                        if let Some(li) = left_idx {
                            track.clips[li].source_out =
                                (track.clips[li].source_out as i64 + delta_ns).max(0) as u64;
                        }
                        if let Some(ri) = right_idx {
                            track.clips[ri].source_in =
                                (track.clips[ri].source_in as i64 + delta_ns).max(0) as u64;
                            track.clips[ri].timeline_start =
                                (track.clips[ri].timeline_start as i64 + delta_ns).max(0) as u64;
                        }
                        proj.dirty = true;
                        found = true;
                    }
                    break;
                }
            }
            drop(proj);
            reply.send(json!({"success": found})).ok();
            if found {
                on_project_changed_full();
            }
        }

        McpCommand::SetClipColor {
            clip_id,
            brightness,
            contrast,
            saturation,
            temperature,
            tint,
            denoise,
            sharpness,
            shadows,
            midtones,
            highlights,
            exposure,
            black_point,
            highlights_warmth,
            highlights_tint,
            midtones_warmth,
            midtones_tint,
            shadows_warmth,
            shadows_tint,
            reply,
        } => {
            let mut proj = project.borrow_mut();
            let mut found = false;
            'outer: for track in proj.tracks.iter_mut() {
                for clip in track.clips.iter_mut() {
                    if clip.id == clip_id {
                        clip.brightness = brightness as f32;
                        clip.contrast = contrast as f32;
                        clip.saturation = saturation as f32;
                        clip.temperature = temperature as f32;
                        clip.tint = tint as f32;
                        clip.denoise = denoise as f32;
                        clip.sharpness = sharpness as f32;
                        clip.shadows = shadows as f32;
                        clip.midtones = midtones as f32;
                        clip.highlights = highlights as f32;
                        clip.exposure = exposure as f32;
                        clip.black_point = black_point as f32;
                        clip.highlights_warmth = highlights_warmth as f32;
                        clip.highlights_tint = highlights_tint as f32;
                        clip.midtones_warmth = midtones_warmth as f32;
                        clip.midtones_tint = midtones_tint as f32;
                        clip.shadows_warmth = shadows_warmth as f32;
                        clip.shadows_tint = shadows_tint as f32;
                        proj.dirty = true;
                        found = true;
                        break 'outer;
                    }
                }
            }
            drop(proj);
            reply.send(json!({"success": found})).ok();
            if found {
                on_project_changed_full();
            }
        }

        McpCommand::SetClipColorLabel {
            clip_id,
            color_label,
            reply,
        } => {
            let parsed = match color_label.as_str() {
                "none" => Some(crate::model::clip::ClipColorLabel::None),
                "red" => Some(crate::model::clip::ClipColorLabel::Red),
                "orange" => Some(crate::model::clip::ClipColorLabel::Orange),
                "yellow" => Some(crate::model::clip::ClipColorLabel::Yellow),
                "green" => Some(crate::model::clip::ClipColorLabel::Green),
                "teal" => Some(crate::model::clip::ClipColorLabel::Teal),
                "blue" => Some(crate::model::clip::ClipColorLabel::Blue),
                "purple" => Some(crate::model::clip::ClipColorLabel::Purple),
                "magenta" => Some(crate::model::clip::ClipColorLabel::Magenta),
                _ => None,
            };
            let Some(parsed) = parsed else {
                reply
                    .send(json!({"success": false, "error": "color_label must be one of: none, red, orange, yellow, green, teal, blue, purple, magenta"}))
                    .ok();
                return;
            };
            let mut proj = project.borrow_mut();
            let mut found = false;
            'outer: for track in proj.tracks.iter_mut() {
                for clip in track.clips.iter_mut() {
                    if clip.id == clip_id {
                        clip.color_label = parsed;
                        proj.dirty = true;
                        found = true;
                        break 'outer;
                    }
                }
            }
            drop(proj);
            reply
                .send(json!({"success": found, "clip_id": clip_id, "color_label": color_label}))
                .ok();
            if found {
                on_project_changed();
            }
        }

        McpCommand::SetClipChromaKey {
            clip_id,
            enabled,
            color,
            tolerance,
            softness,
            reply,
        } => {
            let mut proj = project.borrow_mut();
            let mut found = false;
            'outer: for track in proj.tracks.iter_mut() {
                for clip in track.clips.iter_mut() {
                    if clip.id == clip_id {
                        if let Some(v) = enabled {
                            clip.chroma_key_enabled = v;
                        }
                        if let Some(v) = color {
                            clip.chroma_key_color = v;
                        }
                        if let Some(v) = tolerance {
                            clip.chroma_key_tolerance = v as f32;
                        }
                        if let Some(v) = softness {
                            clip.chroma_key_softness = v as f32;
                        }
                        proj.dirty = true;
                        found = true;
                        break 'outer;
                    }
                }
            }
            drop(proj);
            reply.send(json!({"success": found})).ok();
            if found {
                on_project_changed();
            }
        }

        McpCommand::SetClipBgRemoval {
            clip_id,
            enabled,
            threshold,
            reply,
        } => {
            let mut proj = project.borrow_mut();
            let mut found = false;
            'outer: for track in proj.tracks.iter_mut() {
                for clip in track.clips.iter_mut() {
                    if clip.id == clip_id {
                        if let Some(v) = enabled {
                            clip.bg_removal_enabled = v;
                        }
                        if let Some(v) = threshold {
                            clip.bg_removal_threshold = v;
                        }
                        proj.dirty = true;
                        found = true;
                        break 'outer;
                    }
                }
            }
            drop(proj);
            reply.send(json!({"success": found})).ok();
            if found {
                on_project_changed();
            }
        }

        McpCommand::SetClipLut {
            clip_id,
            lut_path,
            reply,
        } => {
            let mut proj = project.borrow_mut();
            let mut found = false;
            'outer: for track in proj.tracks.iter_mut() {
                for clip in track.clips.iter_mut() {
                    if clip.id == clip_id {
                        clip.lut_path = lut_path.clone();
                        proj.dirty = true;
                        found = true;
                        break 'outer;
                    }
                }
            }
            drop(proj);
            reply.send(json!({"success": found})).ok();
            if found {
                on_project_changed();
            }
        }

        McpCommand::SetClipTransform {
            clip_id,
            scale,
            position_x,
            position_y,
            rotate,
            reply,
        } => {
            let mut proj = project.borrow_mut();
            let mut found = false;
            'outer: for track in proj.tracks.iter_mut() {
                for clip in track.clips.iter_mut() {
                    if clip.id == clip_id {
                        clip.scale = scale.clamp(0.1, 4.0);
                        clip.position_x = position_x.clamp(-1.0, 1.0);
                        clip.position_y = position_y.clamp(-1.0, 1.0);
                        if let Some(rot) = rotate {
                            clip.rotate = rot.clamp(-180, 180);
                        }
                        proj.dirty = true;
                        found = true;
                        break 'outer;
                    }
                }
            }
            drop(proj);
            reply.send(json!({"success": found})).ok();
            if found {
                on_project_changed();
            }
        }

        McpCommand::SetClipOpacity {
            clip_id,
            opacity,
            reply,
        } => {
            let mut proj = project.borrow_mut();
            let mut found = false;
            'outer: for track in proj.tracks.iter_mut() {
                for clip in track.clips.iter_mut() {
                    if clip.id == clip_id {
                        clip.opacity = opacity.clamp(0.0, 1.0);
                        proj.dirty = true;
                        found = true;
                        break 'outer;
                    }
                }
            }
            drop(proj);
            reply.send(json!({"success": found})).ok();
            if found {
                on_project_changed();
            }
        }

        McpCommand::SetClipKeyframe {
            clip_id,
            property,
            timeline_pos_ns,
            value,
            interpolation,
            reply,
        } => {
            let Some(property) = Phase1KeyframeProperty::parse(&property) else {
                reply
                    .send(json!({"success": false, "error": "property must be one of: position_x, position_y, scale, opacity, brightness, contrast, saturation, temperature, tint, volume, pan, rotate, crop_left, crop_right, crop_top, crop_bottom"}))
                    .ok();
                return;
            };
            let interp = interpolation
                .as_deref()
                .map(|s| match s {
                    "ease_in" | "easeIn" => crate::model::clip::KeyframeInterpolation::EaseIn,
                    "ease_out" | "easeOut" => crate::model::clip::KeyframeInterpolation::EaseOut,
                    "ease_in_out" | "ease" | "easeInOut" => {
                        crate::model::clip::KeyframeInterpolation::EaseInOut
                    }
                    _ => crate::model::clip::KeyframeInterpolation::Linear,
                })
                .unwrap_or(crate::model::clip::KeyframeInterpolation::Linear);
            let timeline_pos_ns =
                timeline_pos_ns.unwrap_or_else(|| prog_player.borrow().timeline_pos_ns);
            let mut found = false;
            let mut keyframe_time_ns = None;
            {
                let mut proj = project.borrow_mut();
                'outer: for track in proj.tracks.iter_mut() {
                    for clip in track.clips.iter_mut() {
                        if clip.id == clip_id {
                            keyframe_time_ns =
                                Some(clip.upsert_phase1_keyframe_at_timeline_ns_with_interp(
                                    property,
                                    timeline_pos_ns,
                                    value,
                                    interp,
                                ));
                            proj.dirty = true;
                            found = true;
                            break 'outer;
                        }
                    }
                }
            }
            reply
                .send(json!({
                    "success": found,
                    "clip_id": clip_id,
                    "property": property.as_str(),
                    "timeline_pos_ns": timeline_pos_ns,
                    "clip_local_time_ns": keyframe_time_ns
                }))
                .ok();
            if found {
                on_project_changed();
            }
        }

        McpCommand::RemoveClipKeyframe {
            clip_id,
            property,
            timeline_pos_ns,
            reply,
        } => {
            let Some(property) = Phase1KeyframeProperty::parse(&property) else {
                reply
                    .send(json!({"success": false, "error": "property must be one of: position_x, position_y, scale, opacity, brightness, contrast, saturation, temperature, tint, volume, pan, rotate, crop_left, crop_right, crop_top, crop_bottom"}))
                    .ok();
                return;
            };
            let timeline_pos_ns =
                timeline_pos_ns.unwrap_or_else(|| prog_player.borrow().timeline_pos_ns);
            let mut found = false;
            let mut removed = false;
            {
                let mut proj = project.borrow_mut();
                'outer: for track in proj.tracks.iter_mut() {
                    for clip in track.clips.iter_mut() {
                        if clip.id == clip_id {
                            removed = clip
                                .remove_phase1_keyframe_at_timeline_ns(property, timeline_pos_ns);
                            if removed {
                                proj.dirty = true;
                            }
                            found = true;
                            break 'outer;
                        }
                    }
                }
            }
            reply
                .send(json!({
                    "success": found && removed,
                    "clip_id": clip_id,
                    "property": property.as_str(),
                    "timeline_pos_ns": timeline_pos_ns,
                    "removed": removed
                }))
                .ok();
            if found && removed {
                on_project_changed();
            }
        }

        McpCommand::SetTitle { title, reply } => {
            project.borrow_mut().title = title.clone();
            project.borrow_mut().dirty = true;
            reply.send(json!({"success": true})).ok();
            on_project_changed();
        }

        McpCommand::SaveFcpxml { path, reply } => {
            let result = {
                let proj = project.borrow();
                crate::fcpxml::writer::write_fcpxml_for_path(&proj, std::path::Path::new(&path))
                    .and_then(|xml| std::fs::write(&path, xml).map_err(|e| anyhow::anyhow!(e)))
            };
            match result {
                Ok(_) => {
                    {
                        let mut proj = project.borrow_mut();
                        proj.file_path = Some(path.clone());
                        proj.dirty = false;
                    }
                    on_project_changed();
                    reply.send(json!({"success": true, "path": path})).ok()
                }
                Err(e) => reply
                    .send(json!({"success": false, "error": e.to_string()}))
                    .ok(),
            };
        }

        McpCommand::SaveProjectWithMedia { path, reply } => {
            let result = {
                let proj = project.borrow();
                crate::fcpxml::writer::export_project_with_media(&proj, std::path::Path::new(&path))
            };
            match result {
                Ok(library_path) => {
                    {
                        let mut proj = project.borrow_mut();
                        proj.file_path = Some(path.clone());
                        proj.dirty = false;
                    }
                    on_project_changed();
                    reply
                        .send(json!({
                            "success": true,
                            "path": path,
                            "library_path": library_path.to_string_lossy()
                        }))
                        .ok()
                }
                Err(e) => reply
                    .send(json!({"success": false, "error": e.to_string()}))
                    .ok(),
            };
        }

        McpCommand::OpenFcpxml { path, reply } => {
            let (tx, rx) = std::sync::mpsc::sync_channel::<Result<Project, String>>(1);
            let path_bg = path.clone();
            std::thread::spawn(move || {
                let result = std::fs::read_to_string(&path_bg)
                    .map_err(|e| e.to_string())
                    .and_then(|xml| {
                        crate::fcpxml::parser::parse_fcpxml_with_path(
                            &xml,
                            Some(std::path::Path::new(&path_bg)),
                        )
                        .map_err(|e| e.to_string())
                    });
                let _ = tx.send(result);
            });
            timeline_state.borrow_mut().loading = true;
            let project = project.clone();
            let timeline_state = timeline_state.clone();
            let on_project_changed = on_project_changed_full.clone();
            let suppress_resume_on_next_reload = suppress_resume_on_next_reload.clone();
            let clear_media_browser_on_next_reload = clear_media_browser_on_next_reload.clone();
            glib::timeout_add_local(std::time::Duration::from_millis(10), move || {
                match rx.try_recv() {
                    Ok(Ok(mut new_proj)) => {
                        new_proj.file_path = Some(path.clone());
                        let track_count = new_proj.tracks.len();
                        let clip_count: usize = new_proj.tracks.iter().map(|t| t.clips.len()).sum();
                        *project.borrow_mut() = new_proj;
                        timeline_state.borrow_mut().loading = false;
                        reply.send(json!({"success": true, "path": path, "tracks": track_count, "clips": clip_count})).ok();
                        suppress_resume_on_next_reload.set(true);
                        clear_media_browser_on_next_reload.set(true);
                        let on_project_changed = on_project_changed.clone();
                        glib::timeout_add_local_once(
                            std::time::Duration::from_millis(0),
                            move || {
                                on_project_changed();
                            },
                        );
                        glib::ControlFlow::Break
                    }
                    Ok(Err(e)) => {
                        timeline_state.borrow_mut().loading = false;
                        reply.send(json!({"success": false, "error": e})).ok();
                        glib::ControlFlow::Break
                    }
                    Err(std::sync::mpsc::TryRecvError::Empty) => glib::ControlFlow::Continue,
                    Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                        timeline_state.borrow_mut().loading = false;
                        reply.send(json!({"success": false, "error": "open_fcpxml worker disconnected"})).ok();
                        glib::ControlFlow::Break
                    }
                }
            });
        }

        McpCommand::ExportMp4 { path, reply } => {
            let proj = project.borrow().clone();
            let bg_paths = bg_removal_cache.borrow().paths.clone();
            std::thread::spawn(move || {
                let (done_tx, done_rx) = std::sync::mpsc::sync_channel::<Result<(), String>>(1);
                let proj_worker = proj.clone();
                let path_worker = path.clone();
                std::thread::spawn(move || {
                    let (tx, _rx) = std::sync::mpsc::channel();
                    let result = crate::media::export::export_project(
                        &proj_worker,
                        &path_worker,
                        crate::media::export::ExportOptions::default(),
                        None,
                        &bg_paths,
                        tx,
                    )
                    .map_err(|e| e.to_string())
                    .map(|_| ());
                    let _ = done_tx.send(result);
                });

                match done_rx.recv_timeout(std::time::Duration::from_secs(660)) {
                    Ok(Ok(())) => {
                        let _ = reply.send(json!({"success": true, "path": path}));
                    }
                    Ok(Err(e)) => {
                        let _ = reply.send(json!({"success": false, "error": e}));
                    }
                    Err(_) => {
                        let _ = reply.send(json!({
                            "success": false,
                            "error": "MP4 export timed out after 11 minutes (export thread still running)"
                        }));
                    }
                }
            });
        }

        McpCommand::ListExportPresets { reply } => {
            let state = crate::ui_state::load_export_presets_state();
            let presets: Vec<_> = state
                .presets
                .iter()
                .map(|preset| {
                    let options = preset.to_export_options();
                    json!({
                        "name": preset.name,
                        "video_codec": match options.video_codec {
                            crate::media::export::VideoCodec::H264 => "h264",
                            crate::media::export::VideoCodec::H265 => "h265",
                            crate::media::export::VideoCodec::Vp9 => "vp9",
                            crate::media::export::VideoCodec::ProRes => "prores",
                            crate::media::export::VideoCodec::Av1 => "av1",
                        },
                        "container": match options.container {
                            crate::media::export::Container::Mp4 => "mp4",
                            crate::media::export::Container::Mov => "mov",
                            crate::media::export::Container::WebM => "webm",
                            crate::media::export::Container::Mkv => "mkv",
                        },
                        "output_width": options.output_width,
                        "output_height": options.output_height,
                        "crf": options.crf,
                        "audio_codec": match options.audio_codec {
                            crate::media::export::AudioCodec::Aac => "aac",
                            crate::media::export::AudioCodec::Opus => "opus",
                            crate::media::export::AudioCodec::Flac => "flac",
                            crate::media::export::AudioCodec::Pcm => "pcm",
                        },
                        "audio_bitrate_kbps": options.audio_bitrate_kbps
                    })
                })
                .collect();
            reply
                .send(json!({"presets": presets, "last_used_preset": state.last_used_preset}))
                .ok();
        }

        McpCommand::SaveExportPreset {
            name,
            video_codec,
            container,
            output_width,
            output_height,
            crf,
            audio_codec,
            audio_bitrate_kbps,
            reply,
        } => {
            let video_codec = match video_codec.as_str() {
                "h264" => crate::media::export::VideoCodec::H264,
                "h265" => crate::media::export::VideoCodec::H265,
                "vp9" => crate::media::export::VideoCodec::Vp9,
                "prores" => crate::media::export::VideoCodec::ProRes,
                "av1" => crate::media::export::VideoCodec::Av1,
                _ => {
                    reply
                        .send(json!({"success": false, "error": "video_codec must be one of: h264, h265, vp9, prores, av1"}))
                        .ok();
                    return;
                }
            };
            let container = match container.as_str() {
                "mp4" => crate::media::export::Container::Mp4,
                "mov" => crate::media::export::Container::Mov,
                "webm" => crate::media::export::Container::WebM,
                "mkv" => crate::media::export::Container::Mkv,
                _ => {
                    reply
                        .send(json!({"success": false, "error": "container must be one of: mp4, mov, webm, mkv"}))
                        .ok();
                    return;
                }
            };
            let audio_codec = match audio_codec.as_str() {
                "aac" => crate::media::export::AudioCodec::Aac,
                "opus" => crate::media::export::AudioCodec::Opus,
                "flac" => crate::media::export::AudioCodec::Flac,
                "pcm" => crate::media::export::AudioCodec::Pcm,
                _ => {
                    reply
                        .send(json!({"success": false, "error": "audio_codec must be one of: aac, opus, flac, pcm"}))
                        .ok();
                    return;
                }
            };
            if crf > 51 {
                reply
                    .send(json!({"success": false, "error": "crf must be between 0 and 51"}))
                    .ok();
                return;
            }
            let options = crate::media::export::ExportOptions {
                video_codec,
                container,
                output_width,
                output_height,
                crf,
                audio_codec,
                audio_bitrate_kbps,
            };
            let mut state = crate::ui_state::load_export_presets_state();
            match state.upsert_preset(crate::ui_state::ExportPreset::from_export_options(
                name, &options,
            )) {
                Ok(()) => {
                    let saved_name = state.last_used_preset.clone();
                    crate::ui_state::save_export_presets_state(&state);
                    reply
                        .send(json!({"success": true, "name": saved_name}))
                        .ok();
                }
                Err(e) => {
                    reply.send(json!({"success": false, "error": e})).ok();
                }
            }
        }

        McpCommand::DeleteExportPreset { name, reply } => {
            let mut state = crate::ui_state::load_export_presets_state();
            let removed = state.delete_preset(&name);
            if removed {
                crate::ui_state::save_export_presets_state(&state);
            }
            reply.send(json!({"success": removed, "name": name})).ok();
        }

        McpCommand::ExportWithPreset {
            path,
            preset_name,
            reply,
        } => {
            let state = crate::ui_state::load_export_presets_state();
            let Some(preset) = state.get_preset(&preset_name).cloned() else {
                reply
                    .send(json!({"success": false, "error": format!("Export preset not found: {preset_name}")}))
                    .ok();
                return;
            };
            let options = preset.to_export_options();
            let proj = project.borrow().clone();
            let bg_paths = bg_removal_cache.borrow().paths.clone();
            std::thread::spawn(move || {
                let (done_tx, done_rx) = std::sync::mpsc::sync_channel::<Result<(), String>>(1);
                let proj_worker = proj.clone();
                let path_worker = path.clone();
                std::thread::spawn(move || {
                    let (tx, _rx) = std::sync::mpsc::channel();
                    let result = crate::media::export::export_project(
                        &proj_worker,
                        &path_worker,
                        options,
                        None,
                        &bg_paths,
                        tx,
                    )
                    .map_err(|e| e.to_string())
                    .map(|_| ());
                    let _ = done_tx.send(result);
                });

                match done_rx.recv_timeout(std::time::Duration::from_secs(660)) {
                    Ok(Ok(())) => {
                        let _ = reply.send(
                            json!({"success": true, "path": path, "preset_name": preset_name}),
                        );
                    }
                    Ok(Err(e)) => {
                        let _ = reply.send(json!({"success": false, "error": e}));
                    }
                    Err(_) => {
                        let _ = reply.send(json!({
                            "success": false,
                            "error": "export_with_preset timed out after 11 minutes (export thread still running)"
                        }));
                    }
                }
            });
        }

        McpCommand::ListLibrary { reply } => {
            let lib = library.borrow();
            let items: Vec<_> = lib
                .iter()
                .map(|item| {
                    json!({
                        "label":       item.label,
                        "source_path": item.source_path,
                        "duration_ns": item.duration_ns,
                        "duration_s":  item.duration_ns as f64 / 1_000_000_000.0,
                        "is_audio_only": item.is_audio_only,
                        "has_audio": item.has_audio,
                    })
                })
                .collect();
            reply.send(json!(items)).ok();
        }

        McpCommand::ImportMedia { path, reply } => {
            let uri = format!("file://{path}");
            let metadata = crate::ui::media_browser::probe_media_metadata(&uri);
            let duration_ns = metadata.duration_ns.unwrap_or(10 * 1_000_000_000);
            let audio_only = metadata.is_audio_only;
            let has_audio = metadata.has_audio;
            let source_timecode_base_ns = {
                let lib = library.borrow();
                let proj = project.borrow();
                lookup_source_timecode_base_ns(&lib, &proj, &path)
            };
            let mut item = MediaItem::new(path.clone(), duration_ns);
            item.is_audio_only = audio_only;
            item.has_audio = has_audio;
            item.source_timecode_base_ns = source_timecode_base_ns;
            let label = item.label.clone();
            library.borrow_mut().push(item);
            reply
                .send(json!({
                    "success": true,
                    "label": label,
                    "duration_ns": duration_ns,
                    "is_audio_only": audio_only,
                    "has_audio": has_audio,
                    "source_timecode_base_ns": source_timecode_base_ns
                }))
                .ok();
        }

        McpCommand::ReorderTrack {
            from_index,
            to_index,
            reply,
        } => {
            let track_count = {
                let proj = project.borrow();
                proj.tracks.len()
            };
            if from_index >= track_count || to_index >= track_count {
                reply
                    .send(json!({"error": "Index out of range", "track_count": track_count}))
                    .ok();
            } else if from_index == to_index {
                reply
                    .send(json!({"success": true, "message": "No change needed"}))
                    .ok();
            } else {
                let cmd = crate::undo::ReorderTrackCommand {
                    from_index,
                    to_index,
                };
                {
                    let st = timeline_state.borrow_mut();
                    let project_rc = st.project.clone();
                    drop(st);
                    let mut proj = project_rc.borrow_mut();
                    timeline_state
                        .borrow_mut()
                        .history
                        .execute(Box::new(cmd), &mut proj);
                }
                reply
                    .send(json!({"success": true, "from_index": from_index, "to_index": to_index}))
                    .ok();
                on_project_changed();
            }
        }
        McpCommand::SetTransition {
            track_index,
            clip_index,
            kind,
            duration_ns,
            reply,
        } => {
            let candidate = {
                let proj = project.borrow();
                let Some(track) = proj.tracks.get(track_index) else {
                    reply.send(json!({"error":"Track index out of range","track_count":proj.tracks.len()})).ok();
                    return;
                };
                if clip_index + 1 >= track.clips.len() {
                    reply.send(json!({"error":"clip_index must reference a clip with a following clip","clip_count":track.clips.len()})).ok();
                    return;
                }
                let clip = &track.clips[clip_index];
                Some((
                    track.id.clone(),
                    clip.id.clone(),
                    clip.transition_after.clone(),
                    clip.transition_after_ns,
                    clip.duration(),
                ))
            };
            let Some((track_id, clip_id, old_kind, old_duration_ns, clip_dur_ns)) = candidate
            else {
                return;
            };
            let new_kind = kind.trim().to_string();
            let supported = ["cross_dissolve", "fade_to_black", "wipe_right", "wipe_left"];
            if !new_kind.is_empty() && !supported.contains(&new_kind.as_str()) {
                reply
                    .send(json!({"error":"Unsupported transition kind","supported":supported}))
                    .ok();
                return;
            }
            let new_duration_ns = if new_kind.is_empty() {
                0
            } else {
                duration_ns.min(clip_dur_ns.saturating_sub(1_000_000))
            };
            let cmd = crate::undo::SetClipTransitionCommand {
                clip_id,
                track_id,
                old_transition: old_kind,
                old_transition_ns: old_duration_ns,
                new_transition: new_kind.clone(),
                new_transition_ns: new_duration_ns,
            };
            {
                let st = timeline_state.borrow_mut();
                let project_rc = st.project.clone();
                drop(st);
                let mut proj = project_rc.borrow_mut();
                timeline_state
                    .borrow_mut()
                    .history
                    .execute(Box::new(cmd), &mut proj);
            }
            reply
                .send(json!({
                    "success": true,
                    "track_index": track_index,
                    "clip_index": clip_index,
                    "kind": new_kind,
                    "duration_ns": new_duration_ns
                }))
                .ok();
            on_project_changed_full();
        }

        McpCommand::CreateProject { title, reply } => {
            *project.borrow_mut() = crate::model::project::Project::new(title.clone());
            {
                let mut st = timeline_state.borrow_mut();
                st.playhead_ns = 0;
                st.scroll_offset = 0.0;
                st.pixels_per_second = 100.0;
                st.selected_clip_id = None;
                st.selected_track_id = None;
                st.history = crate::undo::EditHistory::new();
            }
            reply.send(json!({"success": true, "title": title})).ok();
            suppress_resume_on_next_reload.set(true);
            clear_media_browser_on_next_reload.set(true);
            on_project_changed_full();
        }

        McpCommand::InsertClip {
            source_path,
            source_in_ns,
            source_out_ns,
            track_index,
            timeline_pos_ns,
            reply,
        } => {
            let clip_duration = source_out_ns.saturating_sub(source_in_ns);
            if clip_duration == 0 {
                reply
                    .send(json!({"error": "source_in_ns must be less than source_out_ns"}))
                    .ok();
                return;
            }
            let playhead = timeline_pos_ns.unwrap_or_else(|| timeline_state.borrow().playhead_ns);
            let magnetic_mode = timeline_state.borrow().magnetic_mode;
            let source_monitor_auto_link_av =
                preferences_state.borrow().source_monitor_auto_link_av;
            let source_info = {
                let lib = library.borrow();
                let proj = project.borrow();
                lookup_source_placement_info(&lib, &proj, &source_path)
            };
            let result = {
                let mut proj = project.borrow_mut();
                let placement_plan = build_source_placement_plan_by_track_index(
                    &proj,
                    track_index,
                    source_info,
                    source_monitor_auto_link_av,
                );
                let mut track_changes: Vec<TrackClipsChange> = Vec::new();
                let mut created_clip_ids: Vec<String> = Vec::new();
                let magnetic_mode_for_placement =
                    magnetic_mode && !placement_plan.uses_linked_pair();
                for (target_track_idx, clip) in build_source_clips_for_plan(
                    &placement_plan,
                    &source_path,
                    source_in_ns,
                    source_out_ns,
                    playhead,
                    source_info.source_timecode_base_ns,
                ) {
                    created_clip_ids.push(clip.id.clone());
                    track_changes.push(insert_clip_at_playhead_on_track(
                        &mut proj.tracks[target_track_idx],
                        clip,
                        playhead,
                        magnetic_mode_for_placement,
                    ));
                }
                if track_changes.is_empty() {
                    Err("No matching track found")
                } else {
                    proj.dirty = true;
                    Ok((
                        track_changes,
                        created_clip_ids.first().cloned().unwrap_or_default(),
                        created_clip_ids.into_iter().skip(1).collect::<Vec<_>>(),
                        placement_plan.link_group_id.clone(),
                    ))
                }
            };
            match result {
                Ok((mut track_changes, clip_id, linked_clip_ids, link_group_id)) => {
                    let cmd: Box<dyn crate::undo::EditCommand> = if track_changes.len() == 1 {
                        let change = track_changes.pop().unwrap();
                        Box::new(crate::undo::SetTrackClipsCommand {
                            track_id: change.track_id,
                            old_clips: change.old_clips,
                            new_clips: change.new_clips,
                            label: "Insert at playhead (MCP)".to_string(),
                        })
                    } else {
                        Box::new(crate::undo::SetMultipleTracksClipsCommand {
                            changes: track_changes,
                            label: "Insert at playhead (MCP)".to_string(),
                        })
                    };
                    let st = timeline_state.borrow_mut();
                    let project_rc = st.project.clone();
                    drop(st);
                    let mut proj = project_rc.borrow_mut();
                    timeline_state.borrow_mut().history.undo_stack.push(cmd);
                    timeline_state.borrow_mut().history.redo_stack.clear();
                    proj.dirty = true;
                    drop(proj);
                    reply
                        .send(json!({
                            "success": true,
                            "clip_id": clip_id,
                            "linked_clip_ids": linked_clip_ids,
                            "link_group_id": link_group_id
                        }))
                        .ok();
                    on_project_changed_full();
                }
                Err(e) => {
                    reply.send(json!({"error": e})).ok();
                }
            }
        }

        McpCommand::OverwriteClip {
            source_path,
            source_in_ns,
            source_out_ns,
            track_index,
            timeline_pos_ns,
            reply,
        } => {
            let clip_duration = source_out_ns.saturating_sub(source_in_ns);
            if clip_duration == 0 {
                reply
                    .send(json!({"error": "source_in_ns must be less than source_out_ns"}))
                    .ok();
                return;
            }
            let playhead = timeline_pos_ns.unwrap_or_else(|| timeline_state.borrow().playhead_ns);
            let magnetic_mode = timeline_state.borrow().magnetic_mode;
            let source_monitor_auto_link_av =
                preferences_state.borrow().source_monitor_auto_link_av;
            let range_start = playhead;
            let range_end = playhead + clip_duration;
            let source_info = {
                let lib = library.borrow();
                let proj = project.borrow();
                lookup_source_placement_info(&lib, &proj, &source_path)
            };
            let result = {
                let mut proj = project.borrow_mut();
                let placement_plan = build_source_placement_plan_by_track_index(
                    &proj,
                    track_index,
                    source_info,
                    source_monitor_auto_link_av,
                );
                let mut track_changes: Vec<TrackClipsChange> = Vec::new();
                let mut created_clip_ids: Vec<String> = Vec::new();
                let magnetic_mode_for_placement =
                    magnetic_mode && !placement_plan.uses_linked_pair();
                for (target_track_idx, clip) in build_source_clips_for_plan(
                    &placement_plan,
                    &source_path,
                    source_in_ns,
                    source_out_ns,
                    playhead,
                    source_info.source_timecode_base_ns,
                ) {
                    created_clip_ids.push(clip.id.clone());
                    track_changes.push(overwrite_clip_range_on_track(
                        &mut proj.tracks[target_track_idx],
                        clip,
                        range_start,
                        range_end,
                        magnetic_mode_for_placement,
                    ));
                }
                if track_changes.is_empty() {
                    Err("No matching track found")
                } else {
                    proj.dirty = true;
                    Ok((
                        track_changes,
                        created_clip_ids.first().cloned().unwrap_or_default(),
                        created_clip_ids.into_iter().skip(1).collect::<Vec<_>>(),
                        placement_plan.link_group_id.clone(),
                    ))
                }
            };
            match result {
                Ok((mut track_changes, clip_id, linked_clip_ids, link_group_id)) => {
                    let cmd: Box<dyn crate::undo::EditCommand> = if track_changes.len() == 1 {
                        let change = track_changes.pop().unwrap();
                        Box::new(crate::undo::SetTrackClipsCommand {
                            track_id: change.track_id,
                            old_clips: change.old_clips,
                            new_clips: change.new_clips,
                            label: "Overwrite at playhead (MCP)".to_string(),
                        })
                    } else {
                        Box::new(crate::undo::SetMultipleTracksClipsCommand {
                            changes: track_changes,
                            label: "Overwrite at playhead (MCP)".to_string(),
                        })
                    };
                    let st = timeline_state.borrow_mut();
                    let project_rc = st.project.clone();
                    drop(st);
                    let mut proj = project_rc.borrow_mut();
                    timeline_state.borrow_mut().history.undo_stack.push(cmd);
                    timeline_state.borrow_mut().history.redo_stack.clear();
                    proj.dirty = true;
                    drop(proj);
                    reply
                        .send(json!({
                            "success": true,
                            "clip_id": clip_id,
                            "linked_clip_ids": linked_clip_ids,
                            "link_group_id": link_group_id
                        }))
                        .ok();
                    on_project_changed_full();
                }
                Err(e) => {
                    reply.send(json!({"error": e})).ok();
                }
            }
        }

        McpCommand::SeekPlayhead {
            timeline_pos_ns,
            reply,
        } => {
            timeline_state.borrow_mut().playhead_ns = timeline_pos_ns;
            let needs_async = prog_player.borrow_mut().seek(timeline_pos_ns);
            if needs_async {
                // 3+ tracks: the pipeline is in Playing.  Let the GTK main
                // loop run so gtk4paintablesink can complete its preroll, then
                // restore Paused and reply.
                let pp = prog_player.clone();
                glib::timeout_add_local_once(std::time::Duration::from_millis(500), move || {
                    pp.borrow().complete_playing_pulse();
                    reply
                        .send(json!({"ok": true, "timeline_pos_ns": timeline_pos_ns}))
                        .ok();
                });
            } else {
                reply
                    .send(json!({"ok": true, "timeline_pos_ns": timeline_pos_ns}))
                    .ok();
            }
        }

        McpCommand::ExportDisplayedFrame { path, reply } => {
            if path.is_empty() {
                reply
                    .send(json!({"ok": false, "error": "path is required"}))
                    .ok();
            } else {
                match prog_player.borrow_mut().export_displayed_frame_ppm(&path) {
                    Ok(()) => reply
                        .send(json!({"ok": true, "path": path, "format": "ppm"}))
                        .ok(),
                    Err(e) => reply
                        .send(json!({"ok": false, "error": e.to_string()}))
                        .ok(),
                };
            }
        }

        McpCommand::ExportTimelineSnapshot {
            path,
            width,
            height,
            reply,
        } => {
            if path.is_empty() {
                reply
                    .send(json!({"ok": false, "error": "path is required"}))
                    .ok();
            } else {
                let st = timeline_state.borrow();
                match crate::ui::timeline::widget::export_timeline_snapshot_png(
                    &st,
                    width as i32,
                    height as i32,
                    &path,
                ) {
                    Ok(()) => reply
                        .send(json!({"ok": true, "path": path, "width": width, "height": height}))
                        .ok(),
                    Err(e) => reply.send(json!({"ok": false, "error": e})).ok(),
                };
            }
        }

        McpCommand::Play { reply } => {
            if let Some(cb) = timeline_state.borrow().on_extraction_pause.clone() {
                cb(true);
            }
            prog_player.borrow_mut().play();
            reply.send(json!({"ok": true})).ok();
        }

        McpCommand::Pause { reply } => {
            if let Some(cb) = timeline_state.borrow().on_extraction_pause.clone() {
                cb(false);
            }
            prog_player.borrow_mut().pause();
            reply.send(json!({"ok": true})).ok();
        }

        McpCommand::Stop { reply } => {
            if let Some(cb) = timeline_state.borrow().on_extraction_pause.clone() {
                cb(false);
            }
            prog_player.borrow_mut().stop();
            reply.send(json!({"ok": true})).ok();
        }

        McpCommand::TakeScreenshot { reply } => {
            // Generate a timestamped filename in the current working directory.
            let timestamp = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs();
            let filename = format!("ultimateslice-screenshot-{timestamp}.png");
            let path = std::env::current_dir()
                .unwrap_or_else(|_| std::path::PathBuf::from("."))
                .join(&filename);

            // Snapshot the window widget using GTK snapshot + GSK CairoRenderer.
            let w = window.width().max(1);
            let h = window.height().max(1);
            let paintable = gtk::WidgetPaintable::new(Some(window));
            let snapshot = gtk::Snapshot::new();
            paintable.snapshot(&snapshot, w as f64, h as f64);

            match snapshot.to_node() {
                Some(node) => {
                    let renderer = gtk::gsk::CairoRenderer::new();
                    match renderer.realize(None::<&gdk4::Surface>) {
                        Ok(()) => {
                            let bounds = gtk::graphene::Rect::new(0.0, 0.0, w as f32, h as f32);
                            let texture = renderer.render_texture(&node, Some(&bounds));
                            renderer.unrealize();
                            match texture.save_to_png(&path) {
                                Ok(()) => reply
                                    .send(json!({"ok": true, "path": path.to_string_lossy()}))
                                    .ok(),
                                Err(e) => reply
                                    .send(json!({"ok": false, "error": e.to_string()}))
                                    .ok(),
                            };
                        }
                        Err(e) => {
                            reply
                                .send(json!({"ok": false, "error": format!("Renderer realize failed: {e}")}))
                                .ok();
                        }
                    }
                }
                None => {
                    reply
                        .send(json!({"ok": false, "error": "Window produced no render node"}))
                        .ok();
                }
            }
        }

        McpCommand::SelectLibraryItem { path, reply } => {
            let item = library
                .borrow()
                .iter()
                .find(|i| i.source_path == path)
                .cloned();
            match item {
                Some(media_item) => {
                    on_source_selected(media_item.source_path.clone(), media_item.duration_ns);
                    reply
                        .send(json!({
                            "ok": true,
                            "label": media_item.label,
                            "duration_ns": media_item.duration_ns,
                        }))
                        .ok();
                }
                None => {
                    reply
                        .send(json!({
                            "ok": false,
                            "error": format!("No library item with path: {path}"),
                        }))
                        .ok();
                }
            }
        }

        McpCommand::SourcePlay { reply } => {
            let _ = player.borrow().play();
            reply.send(json!({"ok": true})).ok();
        }

        McpCommand::SourcePause { reply } => {
            let _ = player.borrow().pause();
            reply.send(json!({"ok": true})).ok();
        }
    }
}
